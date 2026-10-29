//go:build (darwin && arm64) || (linux && amd64) || (linux && arm64) || (windows && amd64)

package monty

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"
)

func TestDataclassMethodsRouteByInstanceID(t *testing.T) {
	counter := Dataclass{
		Name:       "Counter",
		FieldNames: []string{"start"},
		Attrs:      Dict{{Key: String("start"), Value: Int(40)}},
		Methods: map[string]MethodHandler{
			"add": func(_ context.Context, call Call) (Result, error) {
				self, _ := call.Args[0].Dataclass()
				start := self.Attrs[0].Value.Raw().(int64)
				return Return(Int(start + call.Args[1].Raw().(int64))), nil
			},
		},
	}

	runner, err := New("[counter.start, counter.add(2)]", CompileOptions{Inputs: []string{"counter"}})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	defer runner.Close()

	value, err := runner.Run(context.Background(), RunOptions{
		Inputs: map[string]Value{"counter": DataclassValue(counter)},
	})
	if err != nil {
		t.Fatalf("Run: %v", err)
	}
	assertRawEqual(t, value.Raw(), []any{int64(40), int64(42)})
}

func TestDataclassIdentityRoundTrips(t *testing.T) {
	runner, err := New(`
from dataclasses import dataclass

@dataclass
class Point:
    x: int

Point(3)
`, CompileOptions{})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	defer runner.Close()

	value, err := runner.Run(context.Background(), RunOptions{})
	if err != nil {
		t.Fatalf("Run: %v", err)
	}
	point, ok := value.Dataclass()
	if !ok {
		t.Fatalf("expected dataclass, got %s", value.Kind())
	}
	if point.Name != "Point" || !point.SandboxClass || point.PlainClass || point.ClassID == "" || point.InstanceID == "" {
		t.Fatalf("unexpected dataclass: %+v", point)
	}
	assertRawEqual(t, point.Attrs[0].Value.Raw(), int64(3))
}

func TestBytesRoundTrip(t *testing.T) {
	runner, err := New("data + b'!' * 20", CompileOptions{Inputs: []string{"data"}})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	defer runner.Close()

	value, err := runner.Run(context.Background(), RunOptions{
		Inputs: map[string]Value{"data": Bytes([]byte("payload"))},
	})
	if err != nil {
		t.Fatalf("Run: %v", err)
	}
	if got, want := string(value.Raw().([]byte)), "payload"+strings.Repeat("!", 20); got != want {
		t.Fatalf("got %q want %q", got, want)
	}
}

func TestSystemSleepHonorsContext(t *testing.T) {
	runner, err := New("import time\ntime.sleep(0.01)\n'done'", CompileOptions{})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	defer runner.Close()

	value, err := runner.Run(context.Background(), RunOptions{})
	if err != nil {
		t.Fatalf("Run: %v", err)
	}
	assertRawEqual(t, value.Raw(), "done")

	slow, err := New("import time\ntime.sleep(5)", CompileOptions{})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	defer slow.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()
	started := time.Now()
	if _, err := slow.Run(ctx, RunOptions{}); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("expected deadline exceeded, got %v", err)
	}
	if elapsed := time.Since(started); elapsed > 2*time.Second {
		t.Fatalf("sleep ignored cancellation: took %s", elapsed)
	}
}

func TestImportOfMissingModuleRaisesModuleNotFound(t *testing.T) {
	runner, err := New(`
try:
    import yaml
except ModuleNotFoundError as e:
    result = str(e)
result
`, CompileOptions{})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	defer runner.Close()

	value, err := runner.Run(context.Background(), RunOptions{})
	if err != nil {
		t.Fatalf("Run: %v", err)
	}
	assertRawEqual(t, value.Raw(), "No module named 'yaml'")
}

func TestReplClose(t *testing.T) {
	repl, err := NewRepl(ReplOptions{})
	if err != nil {
		t.Fatalf("NewRepl: %v", err)
	}
	if _, err := repl.FeedRun(context.Background(), "x = 1", FeedOptions{}); err != nil {
		t.Fatalf("FeedRun: %v", err)
	}
	if err := repl.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if err := repl.Close(); err != nil {
		t.Fatalf("second Close: %v", err)
	}
	if _, err := repl.FeedRun(context.Background(), "x", FeedOptions{}); !errors.Is(err, ErrClosed) {
		t.Fatalf("expected ErrClosed, got %v", err)
	}
}

func TestDumpLoadRoundTrips(t *testing.T) {
	ctx := context.Background()

	runner, err := New("fetch(x) + 1", CompileOptions{Inputs: []string{"x"}})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	defer runner.Close()
	runnerData, err := runner.Dump()
	if err != nil {
		t.Fatalf("Runner.Dump: %v", err)
	}
	loaded, err := LoadRunner(runnerData)
	if err != nil {
		t.Fatalf("LoadRunner: %v", err)
	}
	defer loaded.Close()

	progress, err := loaded.Start(ctx, StartOptions{Inputs: map[string]Value{"x": Int(1)}})
	if err != nil {
		t.Fatalf("Start: %v", err)
	}
	snapshot, ok := progress.(*Snapshot)
	if !ok || snapshot.FunctionName != "fetch" {
		t.Fatalf("expected fetch snapshot, got %#v", progress)
	}
	snapshotData, err := snapshot.Dump()
	if err != nil {
		t.Fatalf("Snapshot.Dump: %v", err)
	}
	closeTestProgress(snapshot)

	restored, err := LoadSnapshot(snapshotData)
	if err != nil {
		t.Fatalf("LoadSnapshot: %v", err)
	}
	complete, err := restored.(*Snapshot).ResumeReturn(ctx, Int(41))
	if err != nil {
		t.Fatalf("ResumeReturn: %v", err)
	}
	assertRawEqual(t, complete.(*Complete).Output.Raw(), int64(42))

	repl, err := NewRepl(ReplOptions{ScriptName: "session.py"})
	if err != nil {
		t.Fatalf("NewRepl: %v", err)
	}
	defer repl.Close()
	if _, err := repl.FeedRun(ctx, "total = 10", FeedOptions{}); err != nil {
		t.Fatalf("FeedRun: %v", err)
	}
	replData, err := repl.Dump()
	if err != nil {
		t.Fatalf("Repl.Dump: %v", err)
	}
	restoredRepl, err := LoadRepl(replData)
	if err != nil {
		t.Fatalf("LoadRepl: %v", err)
	}
	defer restoredRepl.Close()
	value, err := restoredRepl.FeedRun(ctx, "total * 2", FeedOptions{})
	if err != nil {
		t.Fatalf("FeedRun after load: %v", err)
	}
	assertRawEqual(t, value.Raw(), int64(20))
}

func TestSyntaxErrorLocation(t *testing.T) {
	_, err := New("x = (", CompileOptions{})
	var syntaxErr *SyntaxError
	if !errors.As(err, &syntaxErr) {
		t.Fatalf("expected SyntaxError, got %T %v", err, err)
	}
	if len(syntaxErr.Frames) == 0 || syntaxErr.Frames[len(syntaxErr.Frames)-1].Line != 1 {
		t.Fatalf("expected a line-1 frame, got %+v", syntaxErr.Frames)
	}
}

func TestMaxDurationLimit(t *testing.T) {
	runner, err := New("while True:\n    pass", CompileOptions{})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	defer runner.Close()

	_, err = runner.Run(context.Background(), RunOptions{Limits: &ResourceLimits{MaxDuration: 50 * time.Millisecond}})
	var runtimeErr *RuntimeError
	if !errors.As(err, &runtimeErr) || !strings.Contains(runtimeErr.Error(), "TimeoutError") {
		t.Fatalf("expected TimeoutError, got %v", err)
	}
}

func TestMaxMemoryLimit(t *testing.T) {
	runner, err := New("data = []\nwhile True:\n    data.append('x' * 100000)", CompileOptions{})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	defer runner.Close()

	_, err = runner.Run(context.Background(), RunOptions{Limits: &ResourceLimits{MaxMemory: 32 << 20}})
	var runtimeErr *RuntimeError
	if !errors.As(err, &runtimeErr) || !strings.Contains(runtimeErr.Error(), "MemoryError") {
		t.Fatalf("expected MemoryError, got %v", err)
	}
}
