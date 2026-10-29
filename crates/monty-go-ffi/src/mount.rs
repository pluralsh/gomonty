//! Host directory mounts served natively by `monty-fs`.
//!
//! A [`MontyGoMount`] pins an opened host directory. Each run or feed builds a
//! fresh [`MontyGoMountTable`] from its mounts, mirroring upstream's pool:
//! overlay writes and write budgets are scoped to that table, so one mount can
//! back any number of concurrent runs.

use std::{ptr, sync::Mutex};

use monty::{ReplProgress, RunProgress};
use monty_fs::{Mount, MountCallOutcome, MountMode, MountRoot, MountTable};
use monty_types::{ExtFunctionResult, OsFunctionCall};

use crate::{
    FfiError, MontyGoBytes, MontyGoError, MontyGoOpResult, MontyGoProgress, ProgressState,
    StoredProgress, catch_op_result, decode_wire, ffi_panic_error, new_error, progress_ref,
    resume_call_internal, slice_from_raw, take_progress, wire::WireMountOptions, write_out,
};

const DEFAULT_MODE: &str = "overlay";

/// Opaque mount handle for the Go bindings.
pub struct MontyGoMount {
    root: MountRoot,
    mode: String,
    write_bytes_limit: Option<u64>,
}

impl MontyGoMount {
    fn open(options: WireMountOptions) -> Result<Self, FfiError> {
        let mode = options.mode.unwrap_or_else(|| DEFAULT_MODE.to_owned());
        MountMode::from_mode_str(&mode).map_err(FfiError::Api)?;
        let root = MountRoot::open(&options.virtual_path, &options.host_path)
            .map_err(|error| FfiError::Api(error.to_string()))?;
        Ok(Self {
            root,
            mode,
            write_bytes_limit: options.write_bytes_limit,
        })
    }

    fn to_mount(&self) -> Result<Mount, FfiError> {
        let mode = MountMode::from_mode_str(&self.mode).map_err(FfiError::Api)?;
        Ok(Mount::with_root(
            self.root.clone(),
            mode,
            self.write_bytes_limit,
        ))
    }
}

/// Opaque per-run mount table handle for the Go bindings.
pub struct MontyGoMountTable {
    table: Mutex<MountTable>,
}

/// Result of mount construction.
#[repr(C)]
pub struct MontyGoMountResult {
    /// Created mount handle on success.
    pub mount: *mut MontyGoMount,
    /// Error handle on failure.
    pub error: *mut MontyGoError,
}

/// Result of mount table construction.
#[repr(C)]
pub struct MontyGoMountTableResult {
    /// Created mount table handle on success.
    pub table: *mut MontyGoMountTable,
    /// Error handle on failure.
    pub error: *mut MontyGoError,
}

fn catch<T>(f: impl FnOnce() -> Result<T, FfiError>) -> Result<T, FfiError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|payload| Err(ffi_panic_error(payload)))
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_mount_new(
    options_ptr: *const u8,
    options_len: usize,
    out: *mut MontyGoMountResult,
) {
    let result = catch(|| {
        // SAFETY: caller passes a valid buffer
        let options = unsafe { slice_from_raw(options_ptr, options_len) };
        decode_wire::<WireMountOptions>(options).and_then(MontyGoMount::open)
    });
    let result = match result {
        Ok(mount) => MontyGoMountResult {
            mount: Box::into_raw(Box::new(mount)),
            error: ptr::null_mut(),
        },
        Err(error) => MontyGoMountResult {
            mount: ptr::null_mut(),
            error: new_error(error),
        },
    };
    write_out(out, result);
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_mount_free(mount: *mut MontyGoMount) {
    if !mount.is_null() {
        // SAFETY: pointer was created by `Box::into_raw`
        unsafe { drop(Box::from_raw(mount)) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_mount_table_new(
    mounts: *const *const MontyGoMount,
    mounts_len: usize,
    out: *mut MontyGoMountTableResult,
) {
    let result = catch(|| {
        let mut table = MountTable::new();
        if !mounts.is_null() && mounts_len > 0 {
            // SAFETY: caller passes `mounts_len` valid handle pointers
            let handles = unsafe { std::slice::from_raw_parts(mounts, mounts_len) };
            for handle in handles.iter().filter(|handle| !handle.is_null()) {
                // SAFETY: non-null handles were created by `monty_go_mount_new`
                let mount = unsafe { &**handle }.to_mount()?;
                table
                    .push_mount(mount)
                    .map_err(|error| FfiError::Api(error.to_string()))?;
            }
        }
        Ok(table)
    });
    let result = match result {
        Ok(table) => MontyGoMountTableResult {
            table: Box::into_raw(Box::new(MontyGoMountTable {
                table: Mutex::new(table),
            })),
            error: ptr::null_mut(),
        },
        Err(error) => MontyGoMountTableResult {
            table: ptr::null_mut(),
            error: new_error(error),
        },
    };
    write_out(out, result);
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_mount_table_free(table: *mut MontyGoMountTable) {
    if !table.is_null() {
        // SAFETY: pointer was created by `Box::into_raw`
        unsafe { drop(Box::from_raw(table)) };
    }
}

fn os_call_of(progress: &StoredProgress) -> Option<&OsFunctionCall> {
    match &progress.state {
        ProgressState::Run(RunProgress::OsCall(call)) => Some(&call.function_call),
        ProgressState::Repl(ReplProgress::OsCall(call)) => Some(&call.function_call),
        _ => None,
    }
}

fn is_os_call(progress: &StoredProgress) -> bool {
    os_call_of(progress).is_some()
}

/// Serves the paused OS call from `table`, which may be null.
///
/// When no mount covers the call it is answered with the upstream no-handler
/// exception (`PermissionError` for filesystem calls) if `default_on_miss` is
/// set; otherwise `handled` stays false, `out` stays empty, and the progress
/// is left untouched for the host to answer.
#[unsafe(no_mangle)]
pub extern "C" fn monty_go_progress_resume_os(
    progress: *mut MontyGoProgress,
    table: *const MontyGoMountTable,
    default_on_miss: bool,
    handled: *mut bool,
    out: *mut MontyGoOpResult,
) {
    write_out(handled, false);
    let result = catch_op_result(|| {
        let call = match progress_ref(progress).map(os_call_of) {
            Ok(Some(call)) => call.clone(),
            Ok(None) => {
                return MontyGoOpResult::err(
                    FfiError::Api("progress is not an os-call snapshot".to_owned()),
                    None,
                    String::new(),
                );
            }
            Err(error) => return MontyGoOpResult::err(error, None, String::new()),
        };

        let outcome = if table.is_null() {
            MountCallOutcome::NotHandled(call)
        } else {
            // SAFETY: non-null tables were created by `monty_go_mount_table_new`
            let table = unsafe { &*table };
            table
                .table
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .handle_os_call(call)
        };

        let ext_result = match outcome {
            MountCallOutcome::Handled(Ok(value)) => ExtFunctionResult::Return(value),
            MountCallOutcome::Handled(Err(error)) => {
                ExtFunctionResult::Error(error.into_exception())
            }
            MountCallOutcome::NotHandled(call) if default_on_miss => {
                ExtFunctionResult::Error(call.on_no_handler())
            }
            MountCallOutcome::NotHandled(_) => return unhandled(),
        };

        write_out(handled, true);
        match take_progress(progress, is_os_call, "progress is not an os-call snapshot") {
            Ok(inner) => MontyGoOpResult::from_step(resume_call_internal(inner, ext_result)),
            Err(error) => MontyGoOpResult::err(error, None, String::new()),
        }
    });
    write_out(out, result);
}

fn unhandled() -> MontyGoOpResult {
    MontyGoOpResult {
        progress: ptr::null_mut(),
        progress_payload: MontyGoBytes::empty(),
        error: ptr::null_mut(),
        repl: ptr::null_mut(),
        prints: MontyGoBytes::empty(),
    }
}
