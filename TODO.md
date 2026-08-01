# TODO

Prioritized backlog after the initial implementation (see `implementation.md`).

## 1. Re-run integration on real host
`scripts/integration.sh` must run in a normal shell (the Claude Code sandbox
hides `/dev/kvm` and blocks `newuidmap`). Validates, since the last full run:
the overlay driver in userns mode (`userxattr` fix — expect
`storage driver: overlay` and the persistence check passing), binary-safe
exec streams, the agent-ready barrier in `start` (no more 500s in the log),
deterministic `mvm run` exit, and `logs -f` terminating on sandbox exit.

## 2. Image store audit
Review `registry.rs`/`store.rs`: per-layer blob caching vs re-download,
image GC / `rmi` refcounting against existing sandboxes, locking for
concurrent pulls of the same reference. `daemon.pid` is defined but unused.

---

## Done

- **E2E integration** — 12/12 passing on real KVM (2026-08-01); rerun with
  userns mode: 14/15 (overlay probe fixed afterwards with `userxattr`).
- **Rootless chown / virtiofs UID translation** — fixed while keeping
  virtiofs as the root filesystem: `mvm serve` re-execs into a user
  namespace (podman-style, subuid ranges via newuidmap/newgidmap), so
  libkrun's in-process virtiofs server has CAP_CHOWN over mapped uids.
  Unpack applies real tar-header ownership as namespace-root. Validated on
  real KVM (guest chown + root-owned files pass). An `ext4` block-root
  driver also exists (`MVM_STORAGE_DRIVER=ext4`, agent pivot_root +
  ownership manifest) for hosts without subuid ranges.
- **Restart semantics** — overlay upper layer (default driver) and ext4
  disks persist across stop/start (docker-like); the `copy` fallback remains
  ephemeral and is documented as such. Overlay `create` no longer wipes the
  sandbox dir (which used to delete console.log on restart).
- **Exec stdin** — `mvm exec -i` forwards local stdin over
  `POST /exec/{session}/stdin`.
- **Binary-safe exec streams** — Stdin/Stdout/Stderr frames carry base64
  bytes (hand-rolled codec in `common::protocol::b64`, no new agent deps);
  integration has a 64 KiB /dev/urandom sha256 roundtrip check.
- **Deterministic run/exec lifecycle** — `start` blocks until the guest
  agent's vsock channel is up (or the VM dies); the daemon closes a
  sandbox's log broadcast on shim exit, so `mvm run` just streams the
  follow log to EOF (no state polling, no flush sleeps, no cut tails) and
  `logs -f` terminates when the sandbox exits.
- **Agent linkage guard** — dynamically-linked agent candidates are
  rejected (ELF PT_INTERP check) so a glibc agent can never be injected
  into a musl guest again; dev trees auto-find the musl build.
- **CLI flag misuse guard** — mvm options after the image (which would
  silently join the guest command) are rejected with a hint.
- **Exec TTY support** — `mvm exec -it sb sh` gives a real interactive
  shell: the agent allocates a pty (openpty; mounts devpts at boot), the
  child becomes session leader on the slave, output merges onto Stdout;
  a `Resize` protocol message + `/exec/{session}/resize` endpoint carry
  winsize (initial size in the Exec request, then a 500 ms poll in the
  CLI); the CLI enters raw mode (termios, restored on drop) so ^C/arrows
  reach the guest.
- **Exec kill on client disconnect** — the API's exec stream holds a
  kill-on-drop guard: if the HTTP response is dropped before the Exit
  frame (client Ctrl-C/crash/network loss), the daemon SIGKILLs the guest
  session. Integration check: killed client → no orphaned `sleep` in VM.
- **Interactive `mvm run` (`-i`/`-t`)** — attach_stdin sandboxes get the
  shim's stdin as a pipe feeding the guest console (default stays
  /dev/null so stdin-readers see EOF); POST /sandboxes/{id}/stdin writes
  to it; EOF sends VEOF through the console line discipline (a tty has no
  pipe EOF). CLI pumps local stdin; `-t` adds local raw mode. Console
  resize is not propagated (known gap; exec -it has full resize).
