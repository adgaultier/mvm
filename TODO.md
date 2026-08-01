# TODO

Prioritized backlog after the initial implementation (see `implementation.md`).

## 1. Re-run integration on real host (validates ext4 root disk)
`scripts/integration.sh` must run in a normal shell (the Claude Code sandbox
hides `/dev/kvm`). The suite now covers guest `chown`, manifest-restored file
ownership, and rootfs persistence across stop/start — all exercising the new
`ext4` storage driver, which is untested on real KVM.

## 2. Binary-safe exec streams
`Stdin`/`Stdout`/`Stderr` protocol frames carry `String` after
`from_utf8_lossy`, so binary data through `exec` is corrupted. Switch the data
fields to base64 (or raw byte frames) across agent, manager, API, CLI.

## 3. TTY support for exec (`-t`)
Sessions are pipes; interactive shells get no prompt/line editing. Needs pty
allocation in the agent, a winsize/resize protocol message, raw-mode CLI.

## 4. Exec kill on client disconnect
`AgentRequest::Kill` is implemented in the agent but never sent — Ctrl-C on
`mvm exec` leaves the guest process running. Send Kill when the exec response
stream is dropped (or add a kill endpoint) and hook CLI Ctrl-C.

## 5. Deterministic `mvm run` exit
`run_attached` polls state every 250 ms and sleeps 200 ms hoping the log tail
flushed — last output lines can be cut. Wait on the `WorkloadExit` event (or
console EOF) and join the log thread. Consider interactive stdin for `run`.
Related: `exec` right after `start` races the agent's vsock connection (one
500 seen in integration) — add a "wait for agent ready" barrier in `start`.

## 6. Image store audit
Review `registry.rs`/`store.rs`: per-layer blob caching vs re-download,
image GC / `rmi` refcounting against existing sandboxes, locking for
concurrent pulls of the same reference. `daemon.pid` is defined but unused.

---

## Done

- **E2E integration** — 12/12 passing on real KVM (2026-08-01).
- **Rootless chown / virtiofs UID translation** — fixed by the `ext4` storage
  driver (rootless default): per-sandbox ext4 image built with
  `mkfs.ext4 -d`, booted as a virtio-blk root; the agent pivots onto it and
  restores file ownership recorded from the layer tar headers at unpack time.
- **Restart semantics** — `ext4` disks persist across stop/start
  (docker-like); the `copy` fallback remains ephemeral and is documented as
  such.
- **Exec stdin** — `mvm exec -i` forwards local stdin over
  `POST /exec/{session}/stdin`.
