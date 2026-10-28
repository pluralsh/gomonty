// Package monty exposes cgo-free Go bindings for the Monty sandboxed Python
// interpreter.
//
// The package provides compiled runner and REPL APIs, typed value conversion,
// host callback dispatch, and low-level pause/resume snapshots. It embeds the
// platform-specific shared library needed by the bindings and loads it with
// purego, so normal Go module consumers do not need a local C toolchain.
//
// To expose host directories to scripts, use NewMountDir and pass the mounts
// through RunOptions.Mounts or FeedOptions.Mounts; filesystem calls under a
// mount are served natively by upstream monty-fs. For Go-owned filesystem and
// environment callbacks, use the companion vfs package.
package monty
