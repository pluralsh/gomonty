//! C ABI for Go bindings to Monty.
//!
//! The ABI deliberately keeps compiled runners, REPL sessions, mounts, and
//! in-flight snapshots opaque. Structured values cross the boundary as
//! versioned binary payloads defined in [`wire`], while suspend/resume state
//! stays in Rust-owned handles that the Go wrapper drives explicitly.

mod mount;
mod wire;

use std::{
    any::Any,
    borrow::Cow,
    collections::BTreeMap,
    ffi::{CStr, c_char},
    mem,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
    sync::Once,
};

use monty::{
    Dump, MontyRepl, MontyRun, ReplProgress, ReplStartError, RunProgress, Session, SessionRef,
    type_check_nesting_exception,
};
use monty_type_checking::{SourceFile, TypeChecker};
use monty_types::{
    CallArgs, CompileOptions, ExcType, ExtFunctionResult, MontyException, MontyObject, MontyUuid,
    NameLookupResult, PrintWriter, PrintWriterCallback, ResourceLimits, ResourceTracker,
    SOURCE_SCAN_THRESHOLD, TypeCheckingConfig, TypeCheckingFormat, validate_cwd,
};
use wire::{
    WIRE_CALL_RESULT_EXCEPTION, WIRE_CALL_RESULT_PENDING, WIRE_CALL_RESULT_RETURN,
    WIRE_LOOKUP_RESULT_UNDEFINED, WIRE_LOOKUP_RESULT_VALUE, WIRE_PROGRESS_COMPLETE,
    WIRE_PROGRESS_FUNCTION_CALL, WIRE_PROGRESS_FUTURE, WIRE_PROGRESS_NAME_LOOKUP, WireCallResult,
    WireCompileOptions, WireErrorSummary, WireFeedOptions, WireFutureResults, WireLookupResult,
    WirePair, WireProgressPayload, WireReplOptions, WireResourceLimits, WireStartOptions,
    WireValue,
};

/// Counts live allocations so `ResourceLimits::max_memory` can be enforced.
///
/// Monty measures memory as the library's live allocator bytes above a
/// baseline, so the limit is shared by every session in the process.
#[global_allocator]
static ALLOCATOR: monty_alloc::LimitedAllocator = monty_alloc::LimitedAllocator;

static MEMORY_BASELINE: Once = Once::new();

/// Records the memory baseline once, without arming a hard limit.
fn arm_memory_baseline() {
    MEMORY_BASELINE.call_once(|| {
        let _ = monty_alloc::set_hard_limit(None);
    });
}

/// Opaque runner handle for the Go bindings.
pub struct MontyGoRunner {
    runner: MontyRun,
    script_name: String,
    input_names: Vec<String>,
}

/// Opaque REPL handle for the Go bindings.
pub struct MontyGoRepl {
    script_name: String,
    inner: Option<MontyRepl>,
}

/// Opaque progress handle for the Go bindings.
pub struct MontyGoProgress {
    inner: Option<StoredProgress>,
}

/// Opaque error handle for the Go bindings.
pub struct MontyGoError {
    inner: FfiError,
}

#[derive(Debug)]
enum FfiError {
    Exception(MontyException),
    Typing(TypingFailure),
    Api(String),
}

/// A failed type check, kept as source so it can be re-rendered in any format.
///
/// Upstream diagnostics borrow the checker that produced them, so they cannot
/// outlive this call; re-running the checker on demand is cheap enough for the
/// error path.
#[derive(Debug)]
struct TypingFailure {
    rendered: String,
    code: String,
    script_name: String,
    stubs: Option<String>,
}

const TYPE_STUBS_PATH: &str = "type_stubs.py";

fn run_type_check(
    code: &str,
    script_name: &str,
    stubs: Option<&str>,
    config: TypeCheckingConfig,
) -> Result<Option<String>, FfiError> {
    type_check_nesting_exception(code, script_name, SOURCE_SCAN_THRESHOLD)
        .map_err(FfiError::Exception)?;
    let mut checker = TypeChecker::default();
    let source = SourceFile::new(code, script_name);
    let stubs = stubs.map(|stubs| SourceFile::new(stubs, TYPE_STUBS_PATH));
    checker
        .run(&source, stubs.as_ref(), config)
        .map(|failure| failure.map(|failure| failure.to_string()))
        .map_err(FfiError::Api)
}

fn type_check_source(code: &str, script_name: &str, stubs: Option<&str>) -> Result<(), FfiError> {
    match run_type_check(code, script_name, stubs, TypeCheckingConfig::default())? {
        None => Ok(()),
        Some(rendered) => Err(FfiError::Typing(TypingFailure {
            rendered,
            code: code.to_owned(),
            script_name: script_name.to_owned(),
            stubs: stubs.map(str::to_owned),
        })),
    }
}

fn ffi_panic_error(payload: Box<dyn Any + Send>) -> FfiError {
    let message = if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "panic payload was not a string".to_owned()
    };
    FfiError::Api(format!("monty-go ffi panicked: {message}"))
}

fn catch_runner_result<F>(f: F) -> MontyGoRunnerResult
where
    F: FnOnce() -> MontyGoRunnerResult,
{
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => MontyGoRunnerResult::err(ffi_panic_error(payload)),
    }
}

fn catch_repl_result<F>(f: F) -> MontyGoReplResult
where
    F: FnOnce() -> MontyGoReplResult,
{
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => MontyGoReplResult::err(ffi_panic_error(payload)),
    }
}

fn catch_op_result<F>(f: F) -> MontyGoOpResult
where
    F: FnOnce() -> MontyGoOpResult,
{
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => MontyGoOpResult::err(ffi_panic_error(payload), None, String::new()),
    }
}

fn catch_error_result<F>(f: F) -> *mut MontyGoError
where
    F: FnOnce() -> *mut MontyGoError,
{
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => new_error(ffi_panic_error(payload)),
    }
}

fn new_error(error: FfiError) -> *mut MontyGoError {
    Box::into_raw(Box::new(MontyGoError { inner: error }))
}

/// Writes `error` to `error_out` when the caller asked for it.
fn set_error_out(error_out: *mut *mut MontyGoError, error: FfiError) {
    if !error_out.is_null() {
        // SAFETY: caller owns the out pointer
        unsafe { *error_out = new_error(error) };
    }
}

/// Runs a bytes-producing operation, reporting failures through `error_out`.
fn bytes_result<F>(out: *mut MontyGoBytes, error_out: *mut *mut MontyGoError, f: F)
where
    F: FnOnce() -> Result<Vec<u8>, FfiError>,
{
    if !error_out.is_null() {
        // SAFETY: caller owns the out pointer
        unsafe { *error_out = ptr::null_mut() };
    }
    let bytes = match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(bytes)) => MontyGoBytes::from_vec(bytes),
        Ok(Err(error)) => {
            set_error_out(error_out, error);
            MontyGoBytes::empty()
        }
        Err(payload) => {
            set_error_out(error_out, ffi_panic_error(payload));
            MontyGoBytes::empty()
        }
    };
    if !out.is_null() {
        // SAFETY: caller owns the out pointer
        unsafe { *out = bytes };
    }
}

/// Paused execution state plus the script name used for diagnostics.
#[derive(Debug)]
struct StoredProgress {
    script_name: String,
    state: ProgressState,
}

#[derive(Debug)]
enum ProgressState {
    Run(RunProgress),
    Repl(ReplProgress),
}

/// Heap-allocated bytes returned across the C ABI.
#[repr(C)]
pub struct MontyGoBytes {
    /// Pointer to the byte buffer.
    pub ptr: *mut u8,
    /// Buffer length in bytes.
    pub len: usize,
}

impl MontyGoBytes {
    fn empty() -> Self {
        Self {
            ptr: ptr::null_mut(),
            len: 0,
        }
    }

    fn from_vec(bytes: Vec<u8>) -> Self {
        if bytes.is_empty() {
            return Self::empty();
        }
        let mut bytes = bytes.into_boxed_slice();
        let result = Self {
            ptr: bytes.as_mut_ptr(),
            len: bytes.len(),
        };
        mem::forget(bytes);
        result
    }
}

/// Result of runner construction or loading.
#[repr(C)]
pub struct MontyGoRunnerResult {
    /// Created runner handle on success.
    pub runner: *mut MontyGoRunner,
    /// Error handle on failure.
    pub error: *mut MontyGoError,
}

/// Result of REPL construction or loading.
#[repr(C)]
pub struct MontyGoReplResult {
    /// Created REPL handle on success.
    pub repl: *mut MontyGoRepl,
    /// Error handle on failure.
    pub error: *mut MontyGoError,
}

/// Result of start/resume/feed operations.
#[repr(C)]
pub struct MontyGoOpResult {
    /// Progress handle on success.
    pub progress: *mut MontyGoProgress,
    /// Decoded payload for the current progress state.
    pub progress_payload: MontyGoBytes,
    /// Error handle on failure.
    pub error: *mut MontyGoError,
    /// Recovered REPL handle for REPL runtime errors.
    pub repl: *mut MontyGoRepl,
    /// Captured `print()` output from this step.
    pub prints: MontyGoBytes,
}

impl MontyGoRunnerResult {
    fn ok(runner: MontyGoRunner) -> Self {
        Self {
            runner: Box::into_raw(Box::new(runner)),
            error: ptr::null_mut(),
        }
    }

    fn err(error: FfiError) -> Self {
        Self {
            runner: ptr::null_mut(),
            error: new_error(error),
        }
    }
}

impl MontyGoReplResult {
    fn ok(repl: MontyGoRepl) -> Self {
        Self {
            repl: Box::into_raw(Box::new(repl)),
            error: ptr::null_mut(),
        }
    }

    fn err(error: FfiError) -> Self {
        Self {
            repl: ptr::null_mut(),
            error: new_error(error),
        }
    }
}

impl MontyGoOpResult {
    fn ok(progress: StoredProgress, prints: String) -> Self {
        match encode_wire(&progress.describe()) {
            Ok(progress_payload) => Self {
                progress: Box::into_raw(Box::new(MontyGoProgress {
                    inner: Some(progress),
                })),
                progress_payload,
                error: ptr::null_mut(),
                repl: ptr::null_mut(),
                prints: MontyGoBytes::from_vec(prints.into_bytes()),
            },
            Err(error) => Self::err(error, None, prints),
        }
    }

    fn err(error: FfiError, repl: Option<MontyGoRepl>, prints: String) -> Self {
        Self {
            progress: ptr::null_mut(),
            progress_payload: MontyGoBytes::empty(),
            error: new_error(error),
            repl: repl.map_or(ptr::null_mut(), |repl| Box::into_raw(Box::new(repl))),
            prints: MontyGoBytes::from_vec(prints.into_bytes()),
        }
    }

    fn from_step(result: StepResult) -> Self {
        match result {
            Ok((progress, prints)) => Self::ok(progress, prints),
            Err((error, repl, prints)) => Self::err(error, repl, prints),
        }
    }
}

/// Outcome of one execution step: the next progress, or an error plus any
/// recovered REPL session. Both carry the step's captured prints.
type StepResult = Result<(StoredProgress, String), (FfiError, Option<MontyGoRepl>, String)>;

struct PrintCollector {
    buffer: String,
}

impl PrintCollector {
    fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }
}

impl PrintWriterCallback for PrintCollector {
    fn stdout_write(&mut self, output: Cow<'_, str>) -> Result<(), MontyException> {
        self.buffer.push_str(&output);
        Ok(())
    }

    fn stdout_push(&mut self, end: char) -> Result<(), MontyException> {
        self.buffer.push(end);
        Ok(())
    }
}

fn finish_run(
    script_name: String,
    result: Result<RunProgress, MontyException>,
    prints: PrintCollector,
) -> StepResult {
    match result {
        Ok(progress) => Ok((
            StoredProgress {
                script_name,
                state: ProgressState::Run(progress),
            },
            prints.buffer,
        )),
        Err(error) => Err((FfiError::Exception(error), None, prints.buffer)),
    }
}

fn finish_repl(
    script_name: String,
    result: Result<ReplProgress, Box<ReplStartError>>,
    prints: PrintCollector,
) -> StepResult {
    match result {
        Ok(progress) => Ok((
            StoredProgress {
                script_name,
                state: ProgressState::Repl(progress),
            },
            prints.buffer,
        )),
        Err(error) => {
            let ReplStartError { repl, error } = *error;
            Err((
                FfiError::Exception(error),
                Some(MontyGoRepl {
                    script_name,
                    inner: Some(repl),
                }),
                prints.buffer,
            ))
        }
    }
}

const TYPE_MSG_FORMATS: &str = "expected 'traceback', 'type-msg', or 'msg'";

impl FfiError {
    fn summary(&self) -> WireErrorSummary {
        match self {
            Self::Exception(error) => WireErrorSummary::from_exception(error),
            Self::Typing(error) => WireErrorSummary {
                version: wire::WIRE_VERSION,
                kind: "typing".to_owned(),
                type_name: "TypeError".to_owned(),
                message: error.rendered.clone(),
                traceback: Vec::new(),
            },
            Self::Api(message) => WireErrorSummary {
                version: wire::WIRE_VERSION,
                kind: "api".to_owned(),
                type_name: "RuntimeError".to_owned(),
                message: message.clone(),
                traceback: Vec::new(),
            },
        }
    }

    fn display(&self, format: &str, color: bool) -> Result<String, String> {
        match self {
            Self::Exception(error) => match format {
                "traceback" => Ok(error.to_string()),
                "type-msg" => Ok(error.summary()),
                "msg" => Ok(error.message().unwrap_or_default().to_owned()),
                _ => Err(format!("invalid display format '{format}', {TYPE_MSG_FORMATS}")),
            },
            Self::Typing(error) if matches!(format, "traceback" | "type-msg" | "msg") => {
                Ok(error.rendered.clone())
            }
            Self::Typing(error) => {
                let config = TypeCheckingConfig {
                    format: TypeCheckingFormat::from_name(format)?,
                    color,
                };
                match run_type_check(
                    &error.code,
                    &error.script_name,
                    error.stubs.as_deref(),
                    config,
                ) {
                    Ok(Some(rendered)) => Ok(rendered),
                    Ok(None) => Ok(error.rendered.clone()),
                    Err(error) => Err(error.summary().message),
                }
            }
            Self::Api(message) => match format {
                "msg" | "type-msg" | "traceback" => Ok(message.clone()),
                _ => Err(format!("invalid display format '{format}', {TYPE_MSG_FORMATS}")),
            },
        }
    }
}

impl StoredProgress {
    fn describe(&self) -> WireProgressPayload {
        let is_repl = matches!(self.state, ProgressState::Repl(_));
        let base = WireProgressPayload {
            version: wire::WIRE_VERSION,
            script_name: self.script_name.clone(),
            is_repl,
            ..WireProgressPayload::default()
        };
        match &self.state {
            ProgressState::Run(RunProgress::FunctionCall(call)) => describe_call(
                base,
                &call.function_name,
                &call.args,
                call.call_id,
                call.object_id,
            ),
            ProgressState::Repl(ReplProgress::FunctionCall(call)) => describe_call(
                base,
                &call.function_name,
                &call.args,
                call.call_id,
                call.object_id,
            ),
            ProgressState::Run(RunProgress::OsCall(call)) => describe_call(
                WireProgressPayload {
                    is_os_function: true,
                    ..base
                },
                call.function_call.name(),
                &call.function_call.clone().to_args(),
                call.call_id,
                None,
            ),
            ProgressState::Repl(ReplProgress::OsCall(call)) => describe_call(
                WireProgressPayload {
                    is_os_function: true,
                    ..base
                },
                call.function_call.name(),
                &call.function_call.clone().to_args(),
                call.call_id,
                None,
            ),
            ProgressState::Run(RunProgress::NameLookup(lookup)) => {
                describe_lookup(base, &lookup.name)
            }
            ProgressState::Repl(ReplProgress::NameLookup(lookup)) => {
                describe_lookup(base, &lookup.name)
            }
            ProgressState::Run(RunProgress::ResolveFutures(state)) => {
                describe_futures(base, state.pending_call_ids())
            }
            ProgressState::Repl(ReplProgress::ResolveFutures(state)) => {
                describe_futures(base, state.pending_call_ids())
            }
            ProgressState::Run(RunProgress::Complete(value))
            | ProgressState::Repl(ReplProgress::Complete { value, .. }) => WireProgressPayload {
                variant: WIRE_PROGRESS_COMPLETE,
                output: Some(WireValue::from_monty(value)),
                ..base
            },
        }
    }

    fn into_repl(self) -> Result<MontyGoRepl, (FfiError, Self)> {
        match self.state {
            ProgressState::Repl(progress) => Ok(MontyGoRepl {
                script_name: self.script_name,
                inner: Some(progress.into_repl()),
            }),
            ProgressState::Run(_) => Err((
                FfiError::Api("progress handle does not own a REPL session".to_owned()),
                self,
            )),
        }
    }

    fn dump(&self) -> Result<Vec<u8>, FfiError> {
        let session = match &self.state {
            ProgressState::Run(progress) => SessionRef::Running(progress),
            ProgressState::Repl(progress) => SessionRef::Suspended(progress),
        };
        monty::dump(&self.script_name, None, &[], session).map_err(|e| FfiError::Api(e.to_string()))
    }

    fn is_call(&self) -> bool {
        matches!(
            self.state,
            ProgressState::Run(RunProgress::FunctionCall(_) | RunProgress::OsCall(_))
                | ProgressState::Repl(ReplProgress::FunctionCall(_) | ReplProgress::OsCall(_))
        )
    }

    fn is_lookup(&self) -> bool {
        matches!(
            self.state,
            ProgressState::Run(RunProgress::NameLookup(_))
                | ProgressState::Repl(ReplProgress::NameLookup(_))
        )
    }

    fn is_futures(&self) -> bool {
        matches!(
            self.state,
            ProgressState::Run(RunProgress::ResolveFutures(_))
                | ProgressState::Repl(ReplProgress::ResolveFutures(_))
        )
    }
}

fn describe_call(
    base: WireProgressPayload,
    function_name: &str,
    args: &CallArgs,
    call_id: u32,
    object_id: Option<MontyUuid>,
) -> WireProgressPayload {
    WireProgressPayload {
        variant: WIRE_PROGRESS_FUNCTION_CALL,
        is_method_call: object_id.is_some(),
        function_name: function_name.to_owned(),
        args: args.args().map(WireValue::from_ref).collect(),
        kwargs: args
            .kwargs()
            .map(|(key, value)| WirePair {
                key: WireValue::from_ref(key),
                value: WireValue::from_ref(value),
            })
            .collect(),
        call_id,
        object_id: object_id.map(|id| id.as_bytes().to_vec()).unwrap_or_default(),
        ..base
    }
}

fn describe_lookup(base: WireProgressPayload, name: &str) -> WireProgressPayload {
    WireProgressPayload {
        variant: WIRE_PROGRESS_NAME_LOOKUP,
        variable_name: name.to_owned(),
        ..base
    }
}

fn describe_futures(base: WireProgressPayload, pending: &[u32]) -> WireProgressPayload {
    WireProgressPayload {
        variant: WIRE_PROGRESS_FUTURE,
        pending_call_ids: pending.to_vec(),
        ..base
    }
}

fn extract_inputs(
    input_names: &[String],
    mut inputs: BTreeMap<String, WireValue>,
) -> Result<Vec<MontyObject>, FfiError> {
    input_names
        .iter()
        .map(|name| {
            let value = inputs
                .remove(name)
                .ok_or_else(|| FfiError::Api(format!("missing required input '{name}'")))?;
            value.into_monty().map_err(FfiError::Api)
        })
        .collect()
}

fn extract_feed_inputs(
    inputs: BTreeMap<String, WireValue>,
) -> Result<Vec<(String, MontyObject)>, FfiError> {
    inputs
        .into_iter()
        .map(|(name, value)| {
            value
                .into_monty()
                .map(|value| (name, value))
                .map_err(FfiError::Api)
        })
        .collect()
}

fn resource_tracker(limits: Option<WireResourceLimits>) -> ResourceTracker {
    ResourceTracker::new(limits.map(ResourceLimits::from).unwrap_or_default())
}

fn decode_wire<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, FfiError> {
    rmp_serde::from_slice(bytes).map_err(|e| FfiError::Api(format!("invalid wire payload: {e}")))
}

fn encode_wire<T: serde::Serialize>(value: &T) -> Result<MontyGoBytes, FfiError> {
    rmp_serde::to_vec_named(value)
        .map(MontyGoBytes::from_vec)
        .map_err(|e| FfiError::Api(format!("failed to encode wire payload: {e}")))
}

unsafe fn slice_from_raw<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: caller guarantees ptr/len point to a valid byte buffer for the duration
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

unsafe fn string_from_cstr<'a>(ptr: *const c_char) -> Result<&'a str, FfiError> {
    if ptr.is_null() {
        return Err(FfiError::Api("expected non-null C string".to_owned()));
    }
    // SAFETY: caller guarantees ptr is NUL-terminated
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .map_err(|e| FfiError::Api(format!("invalid UTF-8 string: {e}")))
}

fn decode_utf8<'a>(bytes: &'a [u8], field_name: &str) -> Result<&'a str, FfiError> {
    std::str::from_utf8(bytes)
        .map_err(|error| FfiError::Api(format!("{field_name} is not valid UTF-8: {error}")))
}

fn decode_call_result(result: WireCallResult) -> Result<ExtFunctionResult, FfiError> {
    match result.kind {
        WIRE_CALL_RESULT_RETURN => result
            .value
            .ok_or_else(|| FfiError::Api("missing return value".to_owned()))?
            .into_monty()
            .map(ExtFunctionResult::Return)
            .map_err(FfiError::Api),
        WIRE_CALL_RESULT_EXCEPTION => Ok(ExtFunctionResult::Error(MontyException::new(
            parse_exc_type(&result.exc_type)?,
            result.arg,
        ))),
        WIRE_CALL_RESULT_PENDING => Ok(ExtFunctionResult::Future(0)),
        other => Err(FfiError::Api(format!("unknown call result kind: {other}"))),
    }
}

fn decode_lookup_result(result: WireLookupResult) -> Result<NameLookupResult, FfiError> {
    match result.kind {
        WIRE_LOOKUP_RESULT_VALUE => result
            .value
            .ok_or_else(|| FfiError::Api("missing lookup value".to_owned()))?
            .into_monty()
            .map(NameLookupResult::Value)
            .map_err(FfiError::Api),
        WIRE_LOOKUP_RESULT_UNDEFINED => Ok(NameLookupResult::Undefined),
        other => Err(FfiError::Api(format!(
            "unknown lookup result kind: {other}"
        ))),
    }
}

fn decode_future_results(
    results: WireFutureResults,
) -> Result<Vec<(u32, ExtFunctionResult)>, FfiError> {
    results
        .results
        .into_iter()
        .map(|(call_id, result)| {
            let result = match decode_call_result(result)? {
                ExtFunctionResult::Future(_) => ExtFunctionResult::Future(call_id),
                other => other,
            };
            Ok((call_id, result))
        })
        .collect()
}

fn parse_exc_type(exc_type: &str) -> Result<ExcType, FfiError> {
    exc_type
        .parse()
        .map_err(|_| FfiError::Api(format!("unknown exception type: {exc_type}")))
}

fn start_runner_internal(handle: &MontyGoRunner, options: WireStartOptions) -> StepResult {
    let inputs = extract_inputs(&handle.input_names, options.inputs)
        .map_err(|error| (error, None, String::new()))?;
    let mut runner = handle.runner.clone();
    if let Some(cwd) = options.cwd {
        let cwd = validate_cwd(&cwd).map_err(|error| (FfiError::Api(error), None, String::new()))?;
        runner.set_cwd(&cwd);
    }
    let mut prints = PrintCollector::new();
    let result = runner.start(
        inputs,
        resource_tracker(options.limits),
        PrintWriter::Callback(&mut prints),
    );
    finish_run(handle.script_name.clone(), result, prints)
}

fn feed_start_internal(
    repl_handle: &mut MontyGoRepl,
    code: &[u8],
    options: WireFeedOptions,
) -> StepResult {
    let script_name = repl_handle.script_name.clone();
    let Some(repl) = repl_handle.inner.take() else {
        return Err((
            FfiError::Api("repl handle is not available for execution".to_owned()),
            Some(MontyGoRepl {
                script_name,
                inner: None,
            }),
            String::new(),
        ));
    };
    let restore = |error: FfiError, repl: MontyRepl| {
        Err((
            error,
            Some(MontyGoRepl {
                script_name: script_name.clone(),
                inner: Some(repl),
            }),
            String::new(),
        ))
    };

    let code = match decode_utf8(code, "code") {
        Ok(code) => code,
        Err(error) => return restore(error, repl),
    };
    let inputs = match extract_feed_inputs(options.inputs) {
        Ok(inputs) => inputs,
        Err(error) => return restore(error, repl),
    };

    let mut prints = PrintCollector::new();
    let result = repl.feed_start(code, inputs, PrintWriter::Callback(&mut prints));
    finish_repl(script_name, result, prints)
}

fn adjust_pending_result(result: ExtFunctionResult, call_id: u32) -> ExtFunctionResult {
    match result {
        ExtFunctionResult::Future(_) => ExtFunctionResult::Future(call_id),
        other => other,
    }
}

fn resume_call_internal(progress: StoredProgress, ext_result: ExtFunctionResult) -> StepResult {
    let StoredProgress { script_name, state } = progress;
    let mut prints = PrintCollector::new();
    let print = PrintWriter::Callback(&mut prints);
    match state {
        ProgressState::Run(RunProgress::FunctionCall(call)) => {
            let result = adjust_pending_result(ext_result, call.call_id);
            let result = call.resume(result, print);
            finish_run(script_name, result, prints)
        }
        ProgressState::Run(RunProgress::OsCall(call)) => {
            let result = adjust_pending_result(ext_result, call.call_id);
            let result = call.resume(result, print);
            finish_run(script_name, result, prints)
        }
        ProgressState::Repl(ReplProgress::FunctionCall(call)) => {
            let result = adjust_pending_result(ext_result, call.call_id);
            let result = call.resume(result, print);
            finish_repl(script_name, result, prints)
        }
        ProgressState::Repl(ReplProgress::OsCall(call)) => {
            let result = adjust_pending_result(ext_result, call.call_id);
            let result = call.resume(result, print);
            finish_repl(script_name, result, prints)
        }
        _ => Err((
            FfiError::Api("progress is not a function or os-call snapshot".to_owned()),
            None,
            prints.buffer,
        )),
    }
}

fn resume_lookup_internal(progress: StoredProgress, lookup: NameLookupResult) -> StepResult {
    let StoredProgress { script_name, state } = progress;
    let mut prints = PrintCollector::new();
    let print = PrintWriter::Callback(&mut prints);
    match state {
        ProgressState::Run(RunProgress::NameLookup(state)) => {
            let result = state.resume(lookup, print);
            finish_run(script_name, result, prints)
        }
        ProgressState::Repl(ReplProgress::NameLookup(state)) => {
            let result = state.resume(lookup, print);
            finish_repl(script_name, result, prints)
        }
        _ => Err((
            FfiError::Api("progress is not a name-lookup snapshot".to_owned()),
            None,
            prints.buffer,
        )),
    }
}

fn resume_futures_internal(
    progress: StoredProgress,
    results: Vec<(u32, ExtFunctionResult)>,
) -> StepResult {
    let StoredProgress { script_name, state } = progress;
    let mut prints = PrintCollector::new();
    let print = PrintWriter::Callback(&mut prints);
    match state {
        ProgressState::Run(RunProgress::ResolveFutures(state)) => {
            let result = state.resume(results, print);
            finish_run(script_name, result, prints)
        }
        ProgressState::Repl(ReplProgress::ResolveFutures(state)) => {
            let result = state.resume(results, print);
            finish_repl(script_name, result, prints)
        }
        _ => Err((
            FfiError::Api("progress is not a future snapshot".to_owned()),
            None,
            prints.buffer,
        )),
    }
}

/// Takes the progress out of its handle after `check` confirms its state.
fn take_progress(
    progress: *mut MontyGoProgress,
    check: fn(&StoredProgress) -> bool,
    wrong_state: &str,
) -> Result<StoredProgress, FfiError> {
    if progress.is_null() {
        return Err(FfiError::Api("progress handle is null".to_owned()));
    }
    // SAFETY: caller passes a valid handle pointer
    let progress = unsafe { &mut *progress };
    match progress.inner.as_ref() {
        None => Err(FfiError::Api(
            "progress handle is no longer available".to_owned(),
        )),
        Some(inner) if !check(inner) => Err(FfiError::Api(wrong_state.to_owned())),
        Some(_) => Ok(progress.inner.take().expect("checked above")),
    }
}

fn write_out<T>(out: *mut T, value: T) {
    if !out.is_null() {
        // SAFETY: caller owns the out pointer
        unsafe { *out = value };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_bytes_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() && len > 0 {
        // SAFETY: ptr/len were allocated by `MontyGoBytes::from_vec`
        unsafe {
            let _ = Vec::from_raw_parts(ptr, len, len);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_runner_free(runner: *mut MontyGoRunner) {
    if !runner.is_null() {
        // SAFETY: pointer was created by `Box::into_raw`
        unsafe { drop(Box::from_raw(runner)) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_repl_free(repl: *mut MontyGoRepl) {
    if !repl.is_null() {
        // SAFETY: pointer was created by `Box::into_raw`
        unsafe { drop(Box::from_raw(repl)) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_progress_free(progress: *mut MontyGoProgress) {
    if !progress.is_null() {
        // SAFETY: pointer was created by `Box::into_raw`
        unsafe { drop(Box::from_raw(progress)) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_error_free(error: *mut MontyGoError) {
    if !error.is_null() {
        // SAFETY: pointer was created by `Box::into_raw`
        unsafe { drop(Box::from_raw(error)) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_error_json(error: *const MontyGoError, out: *mut MontyGoBytes) {
    let bytes = if error.is_null() {
        MontyGoBytes::empty()
    } else {
        // SAFETY: caller passes a valid handle pointer
        let error = unsafe { &*error };
        serde_json::to_vec(&error.inner.summary())
            .map_or_else(|_| MontyGoBytes::empty(), MontyGoBytes::from_vec)
    };
    write_out(out, bytes);
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_error_display(
    error: *const MontyGoError,
    format: *const c_char,
    color: bool,
    out: *mut MontyGoBytes,
) {
    let bytes = if error.is_null() {
        MontyGoBytes::empty()
    } else {
        // SAFETY: caller passes a valid handle pointer
        let error = unsafe { &*error };
        // SAFETY: caller passes a NUL-terminated string or null
        let format = unsafe { string_from_cstr(format) }.unwrap_or("traceback");
        let rendered = catch_unwind(AssertUnwindSafe(|| error.inner.display(format, color)))
            .unwrap_or_else(|payload| Err(ffi_panic_error(payload).summary().message));
        MontyGoBytes::from_vec(rendered.unwrap_or_else(|error| error).into_bytes())
    };
    write_out(out, bytes);
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_runner_new(
    code_ptr: *const u8,
    code_len: usize,
    options_ptr: *const u8,
    options_len: usize,
    out: *mut MontyGoRunnerResult,
) {
    let result = catch_runner_result(|| {
        // SAFETY: caller passes valid buffers
        let code = unsafe { slice_from_raw(code_ptr, code_len) };
        // SAFETY: caller passes valid buffers
        let options = unsafe { slice_from_raw(options_ptr, options_len) };

        let code = match decode_utf8(code, "code") {
            Ok(code) => code.to_owned(),
            Err(error) => return MontyGoRunnerResult::err(error),
        };
        let options: WireCompileOptions = match decode_wire(options) {
            Ok(options) => options,
            Err(error) => return MontyGoRunnerResult::err(error),
        };

        let script_name = options.script_name.unwrap_or_else(|| "main.py".to_owned());
        let input_names = options.inputs.unwrap_or_default();

        if options.type_check
            && let Err(error) =
                type_check_source(&code, &script_name, options.type_check_stubs.as_deref())
        {
            return MontyGoRunnerResult::err(error);
        }

        match MontyRun::new(
            code,
            &script_name,
            input_names.clone(),
            CompileOptions::default(),
        ) {
            Ok(runner) => MontyGoRunnerResult::ok(MontyGoRunner {
                runner,
                script_name,
                input_names,
            }),
            Err(error) => MontyGoRunnerResult::err(FfiError::Exception(error)),
        }
    });
    write_out(out, result);
}

#[derive(serde::Serialize)]
struct StoredRunnerRef<'a> {
    runner: &'a MontyRun,
    script_name: &'a str,
    input_names: &'a [String],
}

#[derive(serde::Deserialize)]
struct StoredRunner {
    runner: MontyRun,
    script_name: String,
    input_names: Vec<String>,
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_runner_load(
    data_ptr: *const u8,
    data_len: usize,
    out: *mut MontyGoRunnerResult,
) {
    let result = catch_runner_result(|| {
        // SAFETY: caller passes a valid buffer
        let data = unsafe { slice_from_raw(data_ptr, data_len) };
        match minicbor_serde::from_slice::<StoredRunner>(data) {
            Ok(stored) => MontyGoRunnerResult::ok(MontyGoRunner {
                runner: stored.runner,
                script_name: stored.script_name,
                input_names: stored.input_names,
            }),
            Err(error) => MontyGoRunnerResult::err(FfiError::Api(format!(
                "invalid runner snapshot: {error}"
            ))),
        }
    });
    write_out(out, result);
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_runner_dump(
    runner: *const MontyGoRunner,
    out: *mut MontyGoBytes,
    error_out: *mut *mut MontyGoError,
) {
    bytes_result(out, error_out, || {
        if runner.is_null() {
            return Err(FfiError::Api("runner handle is null".to_owned()));
        }
        // SAFETY: caller passes a valid handle pointer
        let runner = unsafe { &*runner };
        minicbor_serde::to_vec(&StoredRunnerRef {
            runner: &runner.runner,
            script_name: &runner.script_name,
            input_names: &runner.input_names,
        })
        .map_err(|error| FfiError::Api(error.to_string()))
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_runner_type_check(
    runner: *const MontyGoRunner,
    prefix_ptr: *const u8,
    prefix_len: usize,
) -> *mut MontyGoError {
    catch_error_result(|| {
        if runner.is_null() {
            return new_error(FfiError::Api("runner handle is null".to_owned()));
        }
        // SAFETY: caller passes a valid handle pointer
        let runner = unsafe { &*runner };
        // SAFETY: caller passes a valid buffer
        let prefix = unsafe { slice_from_raw(prefix_ptr, prefix_len) };
        let prefix = if prefix.is_empty() {
            None
        } else {
            match decode_utf8(prefix, "prefix") {
                Ok(prefix) => Some(prefix),
                Err(error) => return new_error(error),
            }
        };
        match type_check_source(runner.runner.code(), &runner.script_name, prefix) {
            Ok(()) => ptr::null_mut(),
            Err(error) => new_error(error),
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_runner_start(
    runner: *const MontyGoRunner,
    options_ptr: *const u8,
    options_len: usize,
    out: *mut MontyGoOpResult,
) {
    let result = catch_op_result(|| {
        arm_memory_baseline();
        if runner.is_null() {
            return MontyGoOpResult::err(
                FfiError::Api("runner handle is null".to_owned()),
                None,
                String::new(),
            );
        }
        // SAFETY: caller passes a valid handle pointer
        let runner = unsafe { &*runner };
        // SAFETY: caller passes a valid buffer
        let options = unsafe { slice_from_raw(options_ptr, options_len) };
        let options: WireStartOptions = match decode_wire(options) {
            Ok(options) => options,
            Err(error) => return MontyGoOpResult::err(error, None, String::new()),
        };
        MontyGoOpResult::from_step(start_runner_internal(runner, options))
    });
    write_out(out, result);
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_repl_new(
    options_ptr: *const u8,
    options_len: usize,
    out: *mut MontyGoReplResult,
) {
    let result = catch_repl_result(|| {
        arm_memory_baseline();
        // SAFETY: caller passes a valid buffer
        let options = unsafe { slice_from_raw(options_ptr, options_len) };
        let options: WireReplOptions = match decode_wire(options) {
            Ok(options) => options,
            Err(error) => return MontyGoReplResult::err(error),
        };
        let script_name = options.script_name.unwrap_or_else(|| "main.py".to_owned());
        let mut repl = MontyRepl::new(
            &script_name,
            resource_tracker(options.limits),
            CompileOptions::default(),
        );
        if let Some(cwd) = options.cwd {
            match validate_cwd(&cwd) {
                Ok(cwd) => repl.set_cwd(&cwd),
                Err(error) => return MontyGoReplResult::err(FfiError::Api(error)),
            }
        }

        MontyGoReplResult::ok(MontyGoRepl {
            script_name,
            inner: Some(repl),
        })
    });
    write_out(out, result);
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_repl_load(
    data_ptr: *const u8,
    data_len: usize,
    out: *mut MontyGoReplResult,
) {
    let result = catch_repl_result(|| {
        arm_memory_baseline();
        // SAFETY: caller passes a valid buffer
        let data = unsafe { slice_from_raw(data_ptr, data_len) };
        match Dump::load(data) {
            Ok(Dump {
                script_name,
                state: Session::Idle(repl),
                ..
            }) => MontyGoReplResult::ok(MontyGoRepl {
                script_name,
                inner: Some(*repl),
            }),
            Ok(_) => MontyGoReplResult::err(FfiError::Api(
                "snapshot does not contain an idle REPL session".to_owned(),
            )),
            Err(error) => MontyGoReplResult::err(FfiError::Api(error.to_string())),
        }
    });
    write_out(out, result);
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_repl_dump(
    repl: *const MontyGoRepl,
    out: *mut MontyGoBytes,
    error_out: *mut *mut MontyGoError,
) {
    bytes_result(out, error_out, || {
        if repl.is_null() {
            return Err(FfiError::Api("repl handle is null".to_owned()));
        }
        // SAFETY: caller passes a valid handle pointer
        let repl = unsafe { &*repl };
        let Some(inner) = &repl.inner else {
            return Err(FfiError::Api(
                "repl handle is not currently available".to_owned(),
            ));
        };
        monty::dump(&repl.script_name, None, &[], SessionRef::Idle(inner))
            .map_err(|error| FfiError::Api(error.to_string()))
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_repl_feed_start(
    repl: *mut MontyGoRepl,
    code_ptr: *const u8,
    code_len: usize,
    options_ptr: *const u8,
    options_len: usize,
    out: *mut MontyGoOpResult,
) {
    let result = catch_op_result(|| {
        if repl.is_null() {
            return MontyGoOpResult::err(
                FfiError::Api("repl handle is null".to_owned()),
                None,
                String::new(),
            );
        }
        // SAFETY: caller passes a valid handle pointer
        let repl = unsafe { &mut *repl };
        // SAFETY: caller passes valid buffers
        let code = unsafe { slice_from_raw(code_ptr, code_len) };
        // SAFETY: caller passes valid buffers
        let options = unsafe { slice_from_raw(options_ptr, options_len) };
        let options: WireFeedOptions = match decode_wire(options) {
            Ok(options) => options,
            Err(error) => return MontyGoOpResult::err(error, None, String::new()),
        };
        MontyGoOpResult::from_step(feed_start_internal(repl, code, options))
    });
    write_out(out, result);
}

fn progress_ref<'a>(progress: *const MontyGoProgress) -> Result<&'a StoredProgress, FfiError> {
    if progress.is_null() {
        return Err(FfiError::Api("progress handle is null".to_owned()));
    }
    // SAFETY: caller passes a valid handle pointer
    unsafe { &*progress }
        .inner
        .as_ref()
        .ok_or_else(|| FfiError::Api("progress handle is no longer available".to_owned()))
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_progress_describe(
    progress: *const MontyGoProgress,
    out: *mut MontyGoBytes,
    error_out: *mut *mut MontyGoError,
) {
    bytes_result(out, error_out, || {
        let inner = progress_ref(progress)?;
        rmp_serde::to_vec_named(&inner.describe())
            .map_err(|e| FfiError::Api(format!("failed to encode wire payload: {e}")))
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_progress_dump(
    progress: *const MontyGoProgress,
    out: *mut MontyGoBytes,
    error_out: *mut *mut MontyGoError,
) {
    bytes_result(out, error_out, || progress_ref(progress)?.dump());
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_progress_load(
    data_ptr: *const u8,
    data_len: usize,
    out: *mut MontyGoOpResult,
) {
    let result = catch_op_result(|| {
        arm_memory_baseline();
        // SAFETY: caller passes a valid buffer
        let data = unsafe { slice_from_raw(data_ptr, data_len) };
        let dump = match Dump::load(data) {
            Ok(dump) => dump,
            Err(error) => {
                return MontyGoOpResult::err(FfiError::Api(error.to_string()), None, String::new());
            }
        };
        let state = match dump.state {
            Session::Running(progress) => ProgressState::Run(*progress),
            Session::Suspended(progress) => ProgressState::Repl(*progress),
            Session::Idle(_) => {
                return MontyGoOpResult::err(
                    FfiError::Api("snapshot contains an idle REPL, not a paused execution".to_owned()),
                    None,
                    String::new(),
                );
            }
        };
        MontyGoOpResult::ok(
            StoredProgress {
                script_name: dump.script_name,
                state,
            },
            String::new(),
        )
    });
    write_out(out, result);
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_progress_take_repl(
    progress: *mut MontyGoProgress,
    out: *mut MontyGoReplResult,
) {
    let result = catch_repl_result(|| {
        if progress.is_null() {
            return MontyGoReplResult::err(FfiError::Api("progress handle is null".to_owned()));
        }
        // SAFETY: caller passes a valid handle pointer
        let progress = unsafe { &mut *progress };
        let Some(inner) = progress.inner.take() else {
            return MontyGoReplResult::err(FfiError::Api(
                "progress handle is no longer available".to_owned(),
            ));
        };
        match inner.into_repl() {
            Ok(repl) => MontyGoReplResult::ok(repl),
            Err((error, inner)) => {
                progress.inner = Some(inner);
                MontyGoReplResult::err(error)
            }
        }
    });
    write_out(out, result);
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_progress_resume_call(
    progress: *mut MontyGoProgress,
    result_ptr: *const u8,
    result_len: usize,
    out: *mut MontyGoOpResult,
) {
    let result = catch_op_result(|| {
        // SAFETY: caller passes a valid buffer
        let result = unsafe { slice_from_raw(result_ptr, result_len) };
        let decoded = decode_wire::<WireCallResult>(result).and_then(decode_call_result);
        let result = match decoded {
            Ok(result) => result,
            Err(error) => return MontyGoOpResult::err(error, None, String::new()),
        };
        let inner = match take_progress(
            progress,
            StoredProgress::is_call,
            "progress is not a function or os-call snapshot",
        ) {
            Ok(inner) => inner,
            Err(error) => return MontyGoOpResult::err(error, None, String::new()),
        };
        MontyGoOpResult::from_step(resume_call_internal(inner, result))
    });
    write_out(out, result);
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_progress_resume_lookup(
    progress: *mut MontyGoProgress,
    result_ptr: *const u8,
    result_len: usize,
    out: *mut MontyGoOpResult,
) {
    let result = catch_op_result(|| {
        // SAFETY: caller passes a valid buffer
        let result = unsafe { slice_from_raw(result_ptr, result_len) };
        let decoded = decode_wire::<WireLookupResult>(result).and_then(decode_lookup_result);
        let result = match decoded {
            Ok(result) => result,
            Err(error) => return MontyGoOpResult::err(error, None, String::new()),
        };
        let inner = match take_progress(
            progress,
            StoredProgress::is_lookup,
            "progress is not a name-lookup snapshot",
        ) {
            Ok(inner) => inner,
            Err(error) => return MontyGoOpResult::err(error, None, String::new()),
        };
        MontyGoOpResult::from_step(resume_lookup_internal(inner, result))
    });
    write_out(out, result);
}

#[unsafe(no_mangle)]
pub extern "C" fn monty_go_progress_resume_futures(
    progress: *mut MontyGoProgress,
    results_ptr: *const u8,
    results_len: usize,
    out: *mut MontyGoOpResult,
) {
    let result = catch_op_result(|| {
        // SAFETY: caller passes a valid buffer
        let results = unsafe { slice_from_raw(results_ptr, results_len) };
        let decoded = decode_wire::<WireFutureResults>(results).and_then(decode_future_results);
        let results = match decoded {
            Ok(results) => results,
            Err(error) => return MontyGoOpResult::err(error, None, String::new()),
        };
        let inner = match take_progress(
            progress,
            StoredProgress::is_futures,
            "progress is not a future snapshot",
        ) {
            Ok(inner) => inner,
            Err(error) => return MontyGoOpResult::err(error, None, String::new()),
        };
        MontyGoOpResult::from_step(resume_futures_internal(inner, results))
    });
    write_out(out, result);
}
