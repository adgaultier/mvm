# TODO

Prioritized backlog after the initial implementation (see `implementation.md`).

## 1. Re-run integration on real host (validates userns virtiofs mode)
`scripts/integration.sh` must run in a normal shell (the Claude Code sandbox
hides `/dev/kvm` and blocks `newuidmap`). The suite covers guest `chown`,
root-owned files, and rootfs persistence across stop/start — all exercising
the new rootless userns mode (virtiofs root + overlay driver), untested on
real KVM. Expect `mvm: userns mode active` and `storage driver: overlay` in
the daemon output.

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
- **Rootless chown / virtiofs UID translation** — fixed while keeping
  virtiofs as the root filesystem: `mvm serve` re-execs into a user
  namespace (podman-style, subuid ranges via newuidmap/newgidmap), so
  libkrun's in-process virtiofs server has CAP_CHOWN over mapped uids.
  Unpack applies real tar-header ownership as namespace-root. An `ext4`
  block-root driver also exists (`MVM_STORAGE_DRIVER=ext4`, agent
  pivot_root + ownership manifest) for hosts without subuid ranges.
- **Restart semantics** — overlay upper layer (default driver) and ext4
  disks persist across stop/start (docker-like); the `copy` fallback remains
  ephemeral and is documented as such. Overlay `create` no longer wipes the
  sandbox dir (which used to delete console.log on restart).
- **Exec stdin** — `mvm exec -i` forwards local stdin over
  `POST /exec/{session}/stdin`.
