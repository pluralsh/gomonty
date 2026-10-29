//go:build (darwin && arm64) || (linux && amd64) || (linux && arm64) || (windows && amd64)

package monty

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sync"
	"testing"
)

func newTestMountDir(t *testing.T, virtualPath string, hostPath string, mode MountMode) *MountDir {
	t.Helper()
	mount, err := NewMountDir(virtualPath, hostPath, MountDirOptions{Mode: mode})
	if err != nil {
		t.Fatalf("NewMountDir: %v", err)
	}
	t.Cleanup(func() {
		_ = mount.Close()
	})
	return mount
}

func writeTestFile(t *testing.T, path string, content string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatalf("MkdirAll: %v", err)
	}
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("WriteFile: %v", err)
	}
}

func newTestRunner(t *testing.T, code string) *Runner {
	t.Helper()
	runner, err := New(code, CompileOptions{ScriptName: "mount.py"})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	t.Cleanup(func() {
		closeTestRunner(runner)
	})
	return runner
}

func runWithMounts(t *testing.T, code string, opts RunOptions) Value {
	t.Helper()
	value, err := newTestRunner(t, code).Run(context.Background(), opts)
	if err != nil {
		t.Fatalf("Run: %v", err)
	}
	return value
}

func newServiceTree(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	writeTestFile(t, filepath.Join(dir, "README.md"), "# service")
	writeTestFile(t, filepath.Join(dir, "helm", "values.yaml"), "replicas: 2")
	writeTestFile(t, filepath.Join(dir, "helm", "templates", "deployment.yaml"), "kind: Deployment")
	return dir
}

func TestMountDirReadOnly(t *testing.T) {
	dir := t.TempDir()
	writeTestFile(t, filepath.Join(dir, "a.txt"), "alpha")
	writeTestFile(t, filepath.Join(dir, "sub", "b.txt"), "beta")
	mount := newTestMountDir(t, "/data", dir, MountReadOnly)

	value := runWithMounts(t, `
from pathlib import Path

def attempt(fn):
    try:
        return fn()
    except PermissionError:
        return 'PermissionError'

[
    Path('/data/a.txt').read_text(),
    Path('/data/sub/b.txt').read_text(),
    Path('/data/sub').is_dir(),
    Path('/data/missing.txt').exists(),
    sorted([p.name for p in Path('/data').iterdir()]),
    attempt(lambda: Path('/data/new.txt').write_text('x')),
]
`, RunOptions{Mounts: []*MountDir{mount}})

	want := []any{"alpha", "beta", true, false, []any{"a.txt", "sub"}, "PermissionError"}
	assertRawEqual(t, value.Raw(), want)
	if _, err := os.Stat(filepath.Join(dir, "new.txt")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("read-only mount wrote to host: %v", err)
	}
}

// Lua's fs.read(path) maps onto native file reads.
func TestMountDirLuaReadParity(t *testing.T) {
	mount := newTestMountDir(t, "/service", newServiceTree(t), MountReadOnly)

	value := runWithMounts(t, `
from pathlib import Path

with open('/service/helm/values.yaml') as f:
    opened = f.read()

def missing():
    try:
        Path('/service/nope.yaml').read_text()
    except FileNotFoundError:
        return 'FileNotFoundError'

[Path('/service/helm/values.yaml').read_text(), opened, missing()]
`, RunOptions{Mounts: []*MountDir{mount}})

	assertRawEqual(t, value.Raw(), []any{"replicas: 2", "replicas: 2", "FileNotFoundError"})
}

// Lua's fs.walk(root) maps onto os.walk and Path.rglob.
func TestMountDirLuaWalkParity(t *testing.T) {
	mount := newTestMountDir(t, "/service", newServiceTree(t), MountReadOnly)

	value := runWithMounts(t, `
import os
from pathlib import Path

walked = []
for root, dirs, files in os.walk('/service'):
    for name in files:
        walked.append(os.path.join(root, name))

[sorted(walked), sorted(str(p) for p in Path('/service').rglob('*.yaml'))]
`, RunOptions{Mounts: []*MountDir{mount}})

	assertRawEqual(t, value.Raw(), []any{
		[]any{"/service/README.md", "/service/helm/templates/deployment.yaml", "/service/helm/values.yaml"},
		[]any{"/service/helm/templates/deployment.yaml", "/service/helm/values.yaml"},
	})
}

func TestMountDirCwdResolvesRelativePaths(t *testing.T) {
	mount := newTestMountDir(t, "/service", newServiceTree(t), MountReadOnly)

	value := runWithMounts(t, `
import os
from pathlib import Path
[os.getcwd(), Path('helm/values.yaml').read_text()]
`, RunOptions{Mounts: []*MountDir{mount}, Cwd: "/service"})

	assertRawEqual(t, value.Raw(), []any{"/service", "replicas: 2"})
}

func TestMountDirRootResolvesRelativePaths(t *testing.T) {
	dir := t.TempDir()
	writeTestFile(t, filepath.Join(dir, "config", "values.yaml"), "replicas: 2")
	mount := newTestMountDir(t, "/", dir, MountReadOnly)

	value := runWithMounts(t, `
from pathlib import Path
Path('config/values.yaml').read_text()
`, RunOptions{Mounts: []*MountDir{mount}})

	assertRawEqual(t, value.Raw(), "replicas: 2")
}

func TestMountDirConfinesEscapes(t *testing.T) {
	root := t.TempDir()
	dir := filepath.Join(root, "service")
	writeTestFile(t, filepath.Join(dir, "a.txt"), "alpha")
	writeTestFile(t, filepath.Join(root, "secret.txt"), "secret")
	if err := os.Symlink(filepath.Join(root, "secret.txt"), filepath.Join(dir, "link.txt")); err != nil {
		t.Skipf("symlinks unavailable: %v", err)
	}
	mount := newTestMountDir(t, "/data", dir, MountReadOnly)

	value := runWithMounts(t, `
from pathlib import Path

def attempt(path):
    try:
        return Path(path).read_text()
    except PermissionError:
        return 'PermissionError'

[
    attempt('/data/../secret.txt'),
    attempt('/data/link.txt'),
    attempt('/etc/hosts'),
]
`, RunOptions{Mounts: []*MountDir{mount}})

	assertRawEqual(t, value.Raw(), []any{"PermissionError", "PermissionError", "PermissionError"})
}

func TestUnmountedFilesystemDeniedWithoutOSHandler(t *testing.T) {
	value := runWithMounts(t, `
from pathlib import Path

def attempt(fn):
    try:
        return fn()
    except PermissionError as e:
        return str(e)

attempt(lambda: Path('/etc/hosts').read_text())
`, RunOptions{})

	assertRawEqual(t, value.Raw(), "Permission denied: '/etc/hosts'")
}

func TestMountDirOverlayKeepsWritesInMemory(t *testing.T) {
	dir := t.TempDir()
	writeTestFile(t, filepath.Join(dir, "a.txt"), "alpha")
	mount := newTestMountDir(t, "/data", dir, MountOverlay)

	code := `
from pathlib import Path
before = Path('/data/a.txt').read_text()
Path('/data/a.txt').write_text('changed')
[before, Path('/data/a.txt').read_text()]
`
	first := runWithMounts(t, code, RunOptions{Mounts: []*MountDir{mount}})
	assertRawEqual(t, first.Raw(), []any{"alpha", "changed"})

	second := runWithMounts(t, code, RunOptions{Mounts: []*MountDir{mount}})
	assertRawEqual(t, second.Raw(), []any{"alpha", "changed"})

	content, err := os.ReadFile(filepath.Join(dir, "a.txt"))
	if err != nil {
		t.Fatalf("ReadFile: %v", err)
	}
	if string(content) != "alpha" {
		t.Fatalf("overlay wrote to host: got %q", content)
	}
}

func TestMountDirSharedByConcurrentRuns(t *testing.T) {
	dir := t.TempDir()
	for i := range 8 {
		writeTestFile(t, filepath.Join(dir, fmt.Sprintf("%d.txt", i)), fmt.Sprintf("file-%d", i))
	}
	mount := newTestMountDir(t, "/data", dir, MountReadOnly)

	var wg sync.WaitGroup
	errs := make(chan error, 8)
	for i := range 8 {
		wg.Go(func() {
			runner, err := New(fmt.Sprintf("from pathlib import Path\nPath('/data/%d.txt').read_text()", i), CompileOptions{})
			if err != nil {
				errs <- err
				return
			}
			defer runner.Close()
			value, err := runner.Run(context.Background(), RunOptions{Mounts: []*MountDir{mount}})
			if err != nil {
				errs <- err
				return
			}
			if got, want := value.Raw(), fmt.Sprintf("file-%d", i); got != want {
				errs <- fmt.Errorf("run %d: got %v want %v", i, got, want)
			}
		})
	}
	wg.Wait()
	close(errs)
	for err := range errs {
		t.Error(err)
	}
}

func TestMountDirFallsBackToOSHandler(t *testing.T) {
	dir := t.TempDir()
	writeTestFile(t, filepath.Join(dir, "a.txt"), "alpha")
	mount := newTestMountDir(t, "/data", dir, MountReadOnly)

	var calls []OSFunction
	value := runWithMounts(t, `
from pathlib import Path
[Path('/data/a.txt').read_text(), Path('/elsewhere.txt').read_text()]
`, RunOptions{
		Mounts: []*MountDir{mount},
		OS: func(_ context.Context, call OSCall) (Result, error) {
			calls = append(calls, call.Function)
			return Return(String("from handler")), nil
		},
	})

	assertRawEqual(t, value.Raw(), []any{"alpha", "from handler"})
	if len(calls) != 1 || calls[0] != OSPathReadText {
		t.Fatalf("unexpected OS handler calls: %v", calls)
	}
}

func TestMountDirWithReplFeedRun(t *testing.T) {
	dir := t.TempDir()
	writeTestFile(t, filepath.Join(dir, "a.txt"), "alpha")
	mount := newTestMountDir(t, "/data", dir, MountReadOnly)

	repl, err := NewRepl(ReplOptions{ScriptName: "repl.py"})
	if err != nil {
		t.Fatalf("NewRepl: %v", err)
	}
	t.Cleanup(func() {
		_ = repl.Close()
	})

	ctx := context.Background()
	opts := FeedOptions{Mounts: []*MountDir{mount}}
	if _, err := repl.FeedRun(ctx, "from pathlib import Path\ncontent = Path('/data/a.txt').read_text()", opts); err != nil {
		t.Fatalf("FeedRun: %v", err)
	}
	if _, err := repl.FeedRun(ctx, "Path('/nope.txt').read_text()", opts); err == nil {
		t.Fatal("expected unmounted read to fail")
	}
	value, err := repl.FeedRun(ctx, "content.upper()", opts)
	if err != nil {
		t.Fatalf("FeedRun after error: %v", err)
	}
	assertRawEqual(t, value.Raw(), "ALPHA")
}

func TestMountDirClosed(t *testing.T) {
	mount := newTestMountDir(t, "/data", t.TempDir(), MountReadOnly)
	if err := mount.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}

	runner := newTestRunner(t, "from pathlib import Path\nPath('/data').exists()")
	if _, err := runner.Run(context.Background(), RunOptions{Mounts: []*MountDir{mount}}); !errors.Is(err, ErrClosed) {
		t.Fatalf("expected ErrClosed, got %v", err)
	}
}

func TestNewMountDirErrors(t *testing.T) {
	dir := t.TempDir()
	for name, tc := range map[string]struct {
		virtualPath string
		hostPath    string
		mode        MountMode
	}{
		"relative virtual path": {virtualPath: "data", hostPath: dir, mode: MountReadOnly},
		"missing host path":     {virtualPath: "/data", hostPath: filepath.Join(dir, "missing"), mode: MountReadOnly},
		"unknown mode":          {virtualPath: "/data", hostPath: dir, mode: "bogus"},
	} {
		t.Run(name, func(t *testing.T) {
			mount, err := NewMountDir(tc.virtualPath, tc.hostPath, MountDirOptions{Mode: tc.mode})
			if err == nil {
				_ = mount.Close()
				t.Fatal("expected error")
			}
		})
	}
}

func TestOverlappingMountsRejected(t *testing.T) {
	dir := t.TempDir()
	writeTestFile(t, filepath.Join(dir, "sub", "a.txt"), "alpha")
	outer := newTestMountDir(t, "/data", dir, MountReadWrite)
	inner := newTestMountDir(t, "/data/sub", filepath.Join(dir, "sub"), MountReadOnly)

	runner := newTestRunner(t, "1")
	if _, err := runner.Run(context.Background(), RunOptions{Mounts: []*MountDir{outer, inner}}); err == nil {
		t.Fatal("expected overlapping mounts to be rejected")
	}
}

func assertRawEqual(t *testing.T, got any, want any) {
	t.Helper()
	if !rawEqual(got, want) {
		t.Fatalf("unexpected value: got %#v want %#v", got, want)
	}
}

func rawEqual(got any, want any) bool {
	switch want := want.(type) {
	case []any:
		items, ok := got.([]Value)
		if !ok || len(items) != len(want) {
			return false
		}
		for i := range want {
			if !rawEqual(items[i].Raw(), want[i]) {
				return false
			}
		}
		return true
	default:
		return got == want
	}
}
