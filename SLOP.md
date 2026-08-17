# Slop Findings

This file records only unjustified hacks, incorrect magic values, unnecessary
reinvention, or concrete correctness/security problems. Severity(aka SLOP INDEX) is from 1
(minor maintenance risk) to 5 (critical security or data-loss risk).

Intentional low-level FFI, handwritten seccomp BPF, static-agent constraints,
and deliberate module boundaries are documented elsewhere and are not slop.

## Resolved

### Resolved — OCI opaque whiteout followed symlinks

`crates/image/src/unpack.rs:45-53` uses `dir.is_dir()` and `read_dir(&dir)`
when applying `.wh..wh..opq`. If a prior layer makes the parent directory a
symlink, the opaque whiteout can traverse outside the rootfs and recursively
delete an external directory.

Fixed by rejecting symlink components during destructive traversal. Regression
test: `opaque_whiteout_rejects_symlink_parent`.

### Resolved — Unauthenticated control API bound beyond localhost

The unauthenticated control API previously allowed non-loopback binds. A
network-reachable caller could create, start, execute in, mount, and delete
sandboxes.

Fixed by enforcing loopback-only control binds in `api::serve`, with API unit
tests for IPv4/IPv6 loopback and non-loopback addresses.

## Active Findings

### Resolved — Concurrent sandbox starts could launch duplicate VMs

`Manager::start` now acquires a per-sandbox async lock before checking state or
preparing storage, so concurrent callers cannot launch duplicate shims.

### Resolved — Image replacement could destroy the last valid image

`crates/image/src/store.rs` now renames the existing image to a same-filesystem
backup, installs the staged replacement, and restores the backup if installation
fails.

### Severity 4 — Image and upload paths are unbounded in memory

`crates/image/src/registry.rs:288-314`, `crates/image/src/load.rs:172-189`,
and `crates/api/src/routes.rs:343-375` buffer complete blobs/layers/uploads.
A malicious registry or client can cause excessive memory use.

Stream into bounded staging files, enforce compressed/uncompressed limits, and
apply an HTTP request-body limit.

### Resolved — Exec output was silently discarded under backpressure

`attach_agent` now awaits bounded channel sends instead of using `try_send`, so
slow clients apply backpressure rather than losing stdout/stderr frames.

### Resolved — Corrupt registry data was treated as an empty registry

`Manager::new` now returns an explicit initialization error while preserving
the corrupt registry file for diagnosis.

### Severity 3 — Sandbox ID accepts path separators after deserialization

`crates/common/src/id.rs:32-40` accepts arbitrary strings through `From<String>`
and `From<&str>`, while IDs are used in data-directory paths. Corrupt or
hand-edited state can escape the intended sandbox directory structure.

Validate the fixed-length hexadecimal format during construction and
deserialization.

### Severity 3 — Port protocol suffix is silently ignored

`crates/network/src/lib.rs:86-98` strips `/udp` or any unknown suffix and
returns only `(host, guest)`. `80:80/udp` can therefore be treated as the
default protocol instead of being implemented or rejected.

Parse and preserve the protocol, or reject unsupported suffixes explicitly.

### Severity 3 — Frame encoder does not enforce the decoder limit

`crates/common/src/protocol.rs:116-123` casts payload length to `u32` without
checking `MAX_FRAME`. It can emit frames that the peer is guaranteed to
reject.

Reject oversized payloads before encoding and add a boundary test.

### Severity 3 — Requested virtiofs mounts fail silently

`crates/agent/src/linux.rs` mount handling ignores directory creation and
`mount()` results. A workload can start without a requested mount and receive
no structured diagnostic.

Return/report mount failures and fail startup when a requested mount is not
available.

### Severity 3 — Agent protocol sends discard encoding and write failures

`crates/agent/src/linux.rs:319-325` ignores both frame-encoding failures and
vsock flush errors. The agent can continue after failing to report readiness,
stdout, stderr, or exit events, leaving the host with an incomplete protocol
stream and no explicit failure.

Return the error from `send`/`flush_out` or transition the agent connection to
a failed state when an event cannot be delivered.

### Severity 3 — Exec PTY duplicates are passed to raw-FD ownership APIs unchecked

`crates/agent/src/linux.rs:757-771` calls `dup(fd)` inside
`Stdio::from_raw_fd` without checking for `-1`. A descriptor exhaustion error
can therefore create invalid child stdio setup and obscure the real failure.

Check each duplication, close already-created descriptors, and report a clear
exec error.

### Severity 3 — Exec PTY session setup ignores terminal errors

`crates/agent/src/linux.rs:779-785` ignores failures from `setsid()` and
`TIOCSCTTY`. An exec session may then run without a session leader or
controlling terminal despite requesting `-t`.

Return `last_os_error()` from the `pre_exec` closure when either operation
fails.

### Severity 2 — TSI DNS setup suppresses filesystem failures

`crates/agent/src/linux.rs:112-117` ignores failures creating `/etc` and
writing `resolv.conf`. TSI networking can then start without DNS configuration
and without a diagnostic.

Return or log the error consistently, while preserving the intentional
best-effort behavior for images with read-only `/etc` if compatibility requires
it.

### Severity 3 — Console and persistence I/O errors are discarded

`crates/manager/src/lib.rs` contains log-pump paths that panic or ignore
`write_all`, `flush`, and final persistence errors. Disk-full or permission
failures can silently truncate logs or lose final sandbox state.

Propagate the failure into an explicit logging/state error and add failure-path
tests.

### Severity 3 — Temporary image/upload staging cleanup is incomplete

`crates/api/src/routes.rs:359-375` and `crates/image/src/store.rs:83-120` can
leave temporary files/directories after streaming, metadata, delete, or rename
failures.

Use an RAII cleanup guard and add cleanup tests for every early-return path.

### Severity 2 — Frame decoder repeatedly shifts the buffer

`crates/common/src/protocol.rs:151` drains from the front of a `Vec`, shifting
remaining bytes for every frame. A stream containing many frames incurs
avoidable copying.

Maintain a read offset or use a buffer type designed for incremental parsing.

### Severity 2 — TUI terminal restoration is not fully RAII-protected

`crates/tui/src/main.rs:38-48` can leave raw mode or the alternate screen active
when setup, drawing, or event handling returns an error.

Use one terminal guard whose `Drop` always restores terminal state.

### Severity 2 — Storage invokes `cp` through `PATH`

`crates/storage/src/lib.rs:125-132` executes `cp` through the daemon's `PATH`.
A manipulated environment can select an unintended executable.

Use a trusted absolute path or implement the copy/reflink operation through
Rust/libc APIs.

## Resolved Finding

### Incorrect BPF UAPI constants in the guest probe — fixed

`scripts/integration/probes/bpfprobe.c` once used `28` for
`BPF_PROG_TYPE_CGROUP_SKB` and `2` for `BPF_CGROUP_INET_EGRESS`. The correct
values are `8` and `1`; the old values caused a false `progl=22` result. The
probe now reports `progl=0 attach=0` on the current libkrunfw guest.
