# TODO

Prioritized backlog after the initial implementation (see `implementation.md`).

## 1. End-to-end validation (gates everything else)
Run `scripts/integration.sh` on the real host — the Claude Code sandbox hides
`/dev/kvm`, so it must run in a normal shell. Use `--net none` (no gvproxy
installed). Everything past the libkrun FFI boundary (boot, vsock handshake,
exec, console stream) is untested against a real VM; expect a fix round.

## 2. Binary-safe exec streams
`Stdin`/`Stdout`/`Stderr` protocol frames carry `String` after
`from_utf8_lossy`, so binary data through `exec` is corrupted. Switch the data
fields to base64 (or raw byte frames) across agent, manager, API, CLI.

## 3. Rootless guest `chown` (virtiofs UID translation)
libkrun's in-process virtiofs server runs with the daemon's host credentials
(uid 1000), so guest-root `chown` gets `EPERM` rootless — breaks `apk`/`apt`
postinst, `useradd`, entrypoint `chown`s. There is no uid-map knob in the
libkrun 1.x C API.

Options:
- **ext4 root disk (preferred):** build a per-sandbox image with
  `mkfs.ext4 -d rootfs/` (rootless, encodes arbitrary uids), attach via
  `krun_add_disk`, agent mounts `/dev/vda` + `switch_root` before spawning the
  workload. Full POSIX semantics, no host mediation.
- Run the daemon as root (overlay driver) — works today, not rootless.
- userns + `newuidmap` shim (podman-style) — proper parity, most invasive
  (storage layer must unpack inside the mapping).

## 4. TTY support for exec (`-t`)
Sessions are pipes; interactive shells get no prompt/line editing. Needs pty
allocation in the agent, a winsize/resize protocol message, raw-mode CLI.

## 5. Exec kill on client disconnect
`AgentRequest::Kill` is implemented in the agent but never sent — Ctrl-C on
`mvm exec` leaves the guest process running. Send Kill when the exec response
stream is dropped (or add a kill endpoint) and hook CLI Ctrl-C.

## 6. Restart semantics
`CopyDriver::create` wipes + re-copies the rootfs on every start, so
stop+start discards filesystem changes (unlike docker). Preserve the rootfs on
restart, or document sandboxes as ephemeral-by-design.

## 7. Deterministic `mvm run` exit
`run_attached` polls state every 250 ms and sleeps 200 ms hoping the log tail
flushed — last output lines can be cut. Wait on the `WorkloadExit` event (or
console EOF) and join the log thread. Consider interactive stdin for `run`.

## 8. Image store audit
Review `registry.rs`/`store.rs`: per-layer blob caching vs re-download,
image GC / `rmi` refcounting against existing sandboxes, locking for
concurrent pulls of the same reference. `daemon.pid` is defined but unused.
