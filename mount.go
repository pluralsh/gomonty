package monty

import (
	"sync"

	"github.com/ewhauser/gomonty/internal/ffi"
)

// MountMode controls how sandboxed code may access a mounted host directory.
type MountMode string

const (
	// MountReadOnly allows reads and rejects every write with PermissionError.
	MountReadOnly MountMode = "read-only"
	// MountReadWrite applies writes directly to the host directory.
	MountReadWrite MountMode = "read-write"
	// MountOverlay reads from the host directory but keeps writes in memory.
	MountOverlay MountMode = "overlay"
)

// MountDirOptions configures a host directory mount.
type MountDirOptions struct {
	// Mode defaults to MountOverlay.
	Mode MountMode
	// WriteBytesLimit caps the total bytes written through the mount during
	// one run or feed. Nil means unlimited.
	WriteBytesLimit *uint64
}

// MountDir exposes a host directory to sandboxed code at a virtual path.
//
// Filesystem OS calls under the virtual path (pathlib, open, os.walk,
// os.scandir, glob) are served natively by Monty, which confines them to the
// host directory: `..` traversal and symlinks that escape it are rejected.
//
// The host directory is opened once by NewMountDir. A MountDir may be shared
// by concurrent runs: each Run or FeedRun gets its own view, so overlay writes
// and WriteBytesLimit are scoped to that call.
type MountDir struct {
	mu          sync.RWMutex
	handle      *ffi.Mount
	virtualPath string
	hostPath    string
	mode        MountMode
}

// NewMountDir mounts hostPath at the absolute virtualPath.
func NewMountDir(virtualPath string, hostPath string, opts MountDirOptions) (*MountDir, error) {
	mode := opts.Mode
	if mode == "" {
		mode = MountOverlay
	}
	modeString := string(mode)
	payload, err := marshalWire(wireMountOptions{
		Version:         wireVersion,
		VirtualPath:     virtualPath,
		HostPath:        hostPath,
		Mode:            &modeString,
		WriteBytesLimit: opts.WriteBytesLimit,
	})
	if err != nil {
		return nil, err
	}

	result := ffi.NewMount(payload)
	if result.Error != nil {
		return nil, newError(result.Error)
	}
	return &MountDir{
		handle:      result.Mount,
		virtualPath: virtualPath,
		hostPath:    hostPath,
		mode:        mode,
	}, nil
}

// VirtualPath returns the path the directory is mounted at inside the sandbox.
func (m *MountDir) VirtualPath() string {
	return m.virtualPath
}

// HostPath returns the host directory as passed to NewMountDir.
func (m *MountDir) HostPath() string {
	return m.hostPath
}

// Mode returns the mount mode.
func (m *MountDir) Mode() MountMode {
	return m.mode
}

// Close releases the mount. Runs already in progress keep their view.
func (m *MountDir) Close() error {
	if m == nil {
		return nil
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.handle != nil {
		m.handle.Close()
		m.handle = nil
	}
	return nil
}

// newMountTable builds the mount table for one run or feed. It returns nil
// when there are no mounts.
func newMountTable(mounts []*MountDir) (*ffi.MountTable, error) {
	handles := make([]*ffi.Mount, 0, len(mounts))
	locked := make([]*MountDir, 0, len(mounts))
	defer func() {
		for _, mount := range locked {
			mount.mu.RUnlock()
		}
	}()
	for _, mount := range mounts {
		if mount == nil {
			continue
		}
		mount.mu.RLock()
		locked = append(locked, mount)
		if mount.handle == nil {
			return nil, ErrClosed
		}
		handles = append(handles, mount.handle)
	}
	if len(handles) == 0 {
		return nil, nil
	}

	result := ffi.NewMountTable(handles)
	if result.Error != nil {
		return nil, newError(result.Error)
	}
	return result.Table, nil
}
