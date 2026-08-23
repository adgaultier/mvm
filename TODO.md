# TODO

Prioritized backlog. See `README.md` for user-facing behavior and
`AGENTS.md` for architecture notes and sharp edges.

## 1. Credentials injection (HIGH PRIORITY)
Modeled on Docker Sandboxes' credential handling
(https://docs.docker.com/ai/sandboxes/doc/security/credentials/), adapted to
mvm's daemon + vsock architecture. Guiding principle: **real secrets never
enter the guest** — not its env, not its filesystem; the guest sees
sentinels and the host injects at the network boundary.

-> SEE [TODO](doc/security/NETWORK.SEC.TODO.md)

## 2. SEC HARDENING
-> See  
- [`SEC entrypoint`](doc/security/SEC.TODO.md)
- [`NETWORK`](doc/security/NETWORK.SEC.TODO.md)
- [`KERNEL`](doc/security/KERNEL.SEC.TODO.md)
- [`ADVERSARIAL`](doc/security/AVERSARIAL.SEC.TODO.md)

## 3. MVM Snapshot → CoW VM Fork
see [TODO](doc/lifecycle/SNAPSHOT-COW.LIFECYCLE.TODO.md)

## 4. `--net passt` as new network mode 

First-class `passt` support, on par with `tsi` and `gvproxy` — the second
of libkrun's two documented virtio-net backends ("virtio-net + passt/gvproxy"
vs "virtio-vsock + TSI"). Unlike gvproxy's in-guest static-IP bootstrap,
passt runs its own DHCP/DNS on the guest side and gives the VM a real
interface with outbound connectivity and host port forwards with zero host
setup — the missing "easy outbound networking" option for corp networks where
TSI's socket impersonation is undesirable.


Plan (mirror the gvproxy flow):
1. `krun-sys`: add the `krun_add_net_unixstream` FFI binding (mirror the
   `krun_add_net_unixgram` binding at krun-sys/src/lib.rs:48; keep in sync
   with /usr/include/libkrun.h:448).
2. `network` crate: `NetworkMode::Passt` — `--net passt` runs a private passt
   per sandbox, `--net passt:<socket>` attaches to one you run (gvproxy
   parity). `passt` subprocess spawned by the daemon like gvproxy
   (socketpair + `passt -f --fd N` per crun's handler, or `--socket-path`);
   passt's stdout/stderr must go to /dev/null or a log (closing them makes
   passt exit). Wait for it to be ready before boot; kill *and* wait it at
   teardown (the gvproxy zombie lesson, AGENTS.md).
3. `runtime/shim.rs`: add `krun_add_net_unixstream` in the NIC setup, passing
   the socket fd/path via `ShimConfig`; first NIC = eth0 (with TSI disabled
   when passt is added, same as gvproxy's dead-NIC trick).
4. Port maps: translate mvm's `-p host:guest` into passt `-t`/`-u` forwards
   (crun's handler uses `-t all -u all`; we want per-map), instead of
   `krun_set_port_map`.
5. guestd: confirm eth0 comes up via passt's DHCP (libkrun's init embeds a
   DHCP client, but the guestd IS init here — may need a DHCP/static bootstrap
   like gvproxy's; verify what the workload sees).
6. Env: `MVM_PASST_BIN` (default `passt`, macOS needs no Homebrew libkrun
   dependency here since passt is standalone). Docs: README network table +
   route surface if any. Integration: outbound + dns + port-forward checks
   mirroring the gvproxy section, gated on `command -v passt`.


## 5. Daemon logging & tracing
Mostly done: added `sandbox started/created/removed` and `image pulled/loaded/removed` INFO lines, WARNs for guestd-injection failure, unexpected shim exit, and auth rejections, plus DEBUG/TRACE step-level detail (start lifecycle, exec sessions, gvproxy, storage driver selection) — see the `tracing::` calls in `manager`/`api`/`image`/`storage`. Remaining: fold the pre-tracing `eprintln!` startup banner (data dir, userns, storage) into one structured `info` line. (The `start` `boot_ms`/`total_ms` now come from `Sandbox.lifecycle` — see item 6.)

## 6. Add startup latency instrumentation
Measure startup latency end-to-end and surface it. **Timing capture is done**
(2026-08-14): the manager records `total`, `vm_boot` (shim spawn -> guestd
ready) and per-phase wall times for create/start/stop with monotonic
`Instant`s, as `Sandbox.lifecycle` — see the Done entry; the TUI inspect modal
renders them as flamegraph bars. Remaining: a `--timings` CLI/API output
(Vm boot / guest ready / exec / total) exposing the same numbers to scripts,
then benchmark `mvm run --rm alpine true` over 100 runs and report
min/median/p95/max.

## 7. Pull memory usage
Layer blobs are buffered fully in RAM during pull (fetch + sha256 +
unpack from `Vec<u8>`); a 300 MB layer means a 300 MB spike. Stream to a
temp file with incremental hashing instead — matters for images like
`docker/sandbox-templates:opencode` (~750 MB compressed).

## 8. No `mvm build`
A dockerfile/build command is not planned; `mvm load` covers importing
prebuilt images.


## Done

- **Delegation redesign: interactive clone + Daddy notification**
  (2026-08-22) — the `DELEGATION_CMD` template mechanism is gone (supersedes
  the delegation entry below). `delegate {timeout, message}` now boots an
  interactive *clone* of the caller (same image/workload/env, ports dropped),
  and the message is queued on the child as a Daddy `input` notification
  (`Sandbox.pending_notifications`, persisted). `flush_pending` drains the
  queue through the child's own registered `NOTIFICATION_CMD` once the child
  is running, has declared `ready` and has registered the command — both
  `mark_ready` and `set_notification_command` trigger a flush, whichever comes
  last. The Agent API's `set_delegation_command` was removed; integration
  coverage: clone boot, queued task, lineage + TTL, and the full
  ready → register → deliver loop.
- **Template placeholder: `<MSG>` replaces `$MSG`** (2026-08-22) — hard switch
  of `MSG_PLACEHOLDER` (used by `NOTIFICATION_CMD` delivery) from `$MSG` to
  `<MSG>`: a `$`-form is expanded
  by the shell that *creates* the sandbox when the template is passed via
  `-e` ("MSG: unbound variable" or silently empty), `<MSG>` survives any
  quoting. Substitution stays a literal string replace; the old `$MSG` form is
  no longer recognized.
- **Delegation without command injection: DELEGATION_CMD template**
  (2026-08-22; superseded the same day by the interactive-clone redesign
  above) — a delegating parent can no longer choose its child's command.
  `DelegateRequest` is now `{timeout, message}` (data only); the child's
  workload comes from the caller's registered `delegation_command` template —
  the same `<MSG>` mechanism as `NOTIFICATION_CMD`: the MCP bridge registers
  the agent's `DELEGATION_CMD` env var at boot (`set_delegation_command`,
  mirroring `set_notification_command`), and `Manager::delegate` boots the
  child with `sh -c <template, <MSG> = JSON-serialized message>`. Delegation
  is refused when no template is registered. Integration coverage: template
  registration, `<MSG>` substitution visible in the child's command, lineage +
  TTL on `/agents`, and the no-template refusal.
- **Integration suite: per-section counters + real delegate/vsock coverage**
  (2026-08-21) — `just all` now resets `PASS/FAIL/SKIP` before each section
  (one section's failure no longer re-fails every later section) and tallies
  a grand total + failed-section list at the end. `agent-api.sh` replaces the
  "delegate not implemented" stub check with real assertions (child created,
  `parent` link + TTL deadline visible on `GET /api/v1/agents`).
  `lifecycle.sh` writes `/persist-marker` itself before the restart so the
  persistence check no longer depends on another section running first. Also
  fixed the Agent API response close race (half-close + drain before drop,
  see AGENTS.md) that made vsockprobe round-trips lose their reply.
- **mvm-flow: live agent lineage graph viewer + real delegation lineage**
  (2026-08-21) — new `crates/flow` binary (`mvm-flow <sandbox-id|name>`), a
  rataflow/ratatui 0.30 graph of one agent and its delegated descendants,
  polled live from the new `GET /api/v1/agents` (agent-api feature; one
  `AgentView` per sandbox: derived status stopped/booting/running/ready/
  failed, parent/children, TTL deadline, last notification). Lineage is now
  real: `Sandbox.parent` + `ttl_deadline` records, the Agent API `delegate`
  method creates the child (spec inherited, ports dropped, `delegate` label)
  and starts it; TTL is display-only — enforcement is still open. Edge labels
  come from a bounded per-sandbox notification history
  (`record_notification`, newest 16). Workspace upgraded ratatui 0.29→0.30 /
  crossterm 0.28→0.29. Plan: `doc/agentic/AGENT-FLOW.TODO.md`.
- **Async notifications: wire types, registration, control-plane delivery**
  (2026-08-18) — `Notification` + `NotificationFrom`/`NotificationKind`/
  `TerminationReason` (`from`/`type` kebab-case wire shape per
  `doc/agentic/notes.md`) live in the feature-gated `mvm_common::agent_api`
  module. Each sandbox records an `async_cmd` template
  (`Sandbox.notification_command`, `<MSG>` = the serialized notification);
  the agent registers it over the Agent API
  (`set_notification_command`), and the guest-side bridge does so at boot by
  reading `NOTIFICATION_CMD` (unset/empty = no-op; see `server.py`). The
  control plane delivers by running the template as `sh -c` through `mvm
  exec` (`Manager::deliver_notification` in the feature-gated
  `manager/src/notifications.rs`), waiting for the Exit frame. `test_notification`
  (Agent API method + `mvm-agent-mcp` tool) fires one mock notification of
  every kind through the real delivery path — `agent-api.sh` now exercises
  the full loop: agent calls `test_notification` over vsock, control plane
  runs the registered command, all six kinds land in the guest. Still open:
  a real dispatcher for actual lifecycle events (child TTL expiry, idle
  restarts, parent input), the notification queue, and TTL/idle timeouts.

- **Agent API returns a redacted `AgentInfo`, not the `Sandbox` record**
  (2026-08-18) — `inspect`, `stop` and `set_notification_command` used to
  echo the full `Sandbox` (host mount paths, network profile, port mappings,
  `pid`/`gvproxy_pid`, lifecycle telemetry) to the workload. They now return
  `AgentInfo` (`mvm_common::agent_api`, feature-gated): `id`, `name`, `state`,
  `vcpus`, `ram_mib`, plus delegation/lineage placeholders — `parent:
  Option<SandboxId>` (singular, None = root), `children: Vec`, `capabilities`
  and `budget` — all empty until delegation lands. Unit test asserts no
  sensitive field is serialized; `agent-api.sh` greps the live `inspect`
  response for the control-plane field names.

- **Agent API: vsock transport, no more HTTP** (2026-08-18) — replaced the
  shared `--agent-addr` HTTP listener (below) with a per-sandbox vsock
  channel: the guest dials out to CID 2, port 24643
  (`protocol::AGENT_API_VSOCK_PORT`), bridged host-side by a per-sandbox
  unix socket (`<sandbox_dir>/agent-api.sock`) — the same libkrun
  vsock-over-unix-socket mechanism the exec control channel already used,
  set up in `Manager::start` alongside it and gated by the same condition
  (only when the exec-guestd is injected). Motivation:
  `doc/agentic/notifications-delegation.md` — gvproxy cannot forward a guest
  connection to a host-loopback-only listener without exposing the agent
  surface on a real LAN-reachable address, so HTTP over the network stack was
  a dead end regardless of `passt` vs `gvproxy`. Wire format is
  length-prefixed JSON (`protocol::encode_frame`), one request per
  connection: `{"method","token","params"}` ->
  `{"ok","result"|"error"}` (`mvm_common::api::{AgentApiRequest,
  AgentApiResponse}`), dispatched in the new `mvm-manager::agent_api` module.
  Identity now comes from two independent checks that must agree: which
  per-sandbox socket accepted the connection, and the bearer token in the
  body (`Manager::authenticate_vm` + `authorize`) — closing a gap the old
  shared-listener design couldn't. `mvm-api`'s `/agent/v1` routes, the
  `agent-api` feature on that crate, and the CLI's `--agent-addr`/
  `MVM_AGENT_ADDR` flag are gone; `agent-api` is now purely a
  `mvm-manager`/`mvm-common` feature. Guest-side bridge:
  `scripts/agents/mvm-agent-mcp` (draft). Still open: delegate mechanics, and
  a real guest-side integration test client (the rewritten
  `scripts/integration/sections/agent-api.sh` only exercises the wire
  protocol when the guest happens to have `python3`).

- **Manager lifecycle and image-swap safety** (2026-08-17) — sandbox starts
  are serialized per sandbox, exec output uses awaited bounded-channel sends
  instead of dropping frames under backpressure, image pull/load replacement
  uses a backup-and-restore rename swap, and corrupt `sandboxes.json` now
  fails daemon initialization instead of being treated as empty. Covered by
  focused manager/image tests and the `run`/`exec` integration sections.

- **`--security=strict` guest syscall hardening** (2026-08-16) — `mvm
  create/run/clone --security=strict` plumbs `SandboxSpec.security` →
  `ShimConfig.security` → `MVM_SECURITY_STRICT`, and the guestd installs a
  workload-scoped second seccomp filter (`build_strict` in
  `crates/guestd/src/seccomp.rs`, applied via `apply_strict_seccomp` in the
  workload's `pre_exec` *before* the privilege drop, and in exec sessions)
  denying `bpf`, `keyctl`, `perf_event_open`, `userfaultfd`, the
  `io_uring_*` trio, ptrace, namespace/mount changes, module loading, and
  kexec with `EPERM`. The guestd keeps the full syscall surface
  (needed for exec/pty — and, later, for the guestd itself loading eBPF in
  Phase 2). `scripts/integration/probes/bpfprobe.c` (wired into the
  `bpfprobe` integration section) probes the libkrunfw guest kernel's
  BTF/cgroup2/prog-load+attach capabilities; `progl=0`+`attach=0` is the
  gate for the planned in-guest cgroup_skb/egress
  policy. `scripts/integration/probes/strictprobe.c` now verifies the added
  syscall denials through `just raw-seccomp`. This is the P2
  syscall-hardening baseline from `doc/security/SEC.TODO.md`; strict
  workloads also set `PR_SET_NO_NEW_PRIVS` before exec. Capability dropping
  remains a separate compatibility task.

- **Lifecycle latency flamegraph in the TUI** (2026-08-14) — the manager
  records each `create`/`start`/`stop` as a `LifecycleOp` (`op`, `at`,
  `total_ms`, ordered `phases`) using monotonic `Instant`s, stored bounded
  (16) on `Sandbox.lifecycle` and serialized into the API. Phase lists:
  `create` validate/register/persist,
  `start` rootfs/guestd/gvproxy/ports/shim/persist/boot (shim->guestd-ready),
  `stop` terminate/persist. The TUI inspect modal renders one segmented
  colored bar per op (`tui::flame_bar_line`/`flame_legend`, `█` runs
  proportional to ms + a `░` tail for the untimed slack), showing the full
  recorded history (`tui::visible_lifecycle`) with a timestamp per bar. The
  `start` INFO log sources `total_ms`/`boot_ms` from the same timings. This
  is the data half of TODO#6 (see item 6 for what's left).

- **`mvm load` — OCI image layout archives** (2026-08-13) — `mvm load --name
  IMAGE FILE` (docker `load` parity, OCI-layout only for now) imports a
  `.tar` produced by `podman save --format oci-archive` / `buildah push oci:`.
  The archive is streamed to the daemon (`POST /images/load`, spooled to a
  temp file, never buffered), which resolves the platform manifest out of
  `index.json`, verifies each `blobs/sha256/<digest>` against its filename,
  and unpacks via the same `unpack_layer` + config parser the pull path uses.
  `ImageStore::load` reuses the staging + atomic-rename swap; the shared
  config parsing was lifted into `registry::image_config_from_bytes`.
  docker-archive (`docker save`, with nested `manifest.json`/`layer.tar`)
  is deliberately not handled yet.
- **VM-scoped authentication for the Agent API (P0 sub-item)** (2026-08-13) —
  a second listener (`--agent-addr`/`MVM_AGENT_ADDR`, default
  `127.0.0.1:24643`) serves `/agent/v1` (`inspect`/`stop`/`delegate`-stub),
  authenticated per request by `Authorization: Bearer <token>` and resolved
  to `Principal::Vm(vm_id)` — routes carry no `{id}`, so a VM can only act
  on itself. 32-byte token minted in `Manager::start`; only a SHA-256 hash
  (`guest_token_hash`) is kept in the manager's memory (`#[serde(skip)]`:
  never in API responses, never persisted), cleared on stop/exit. Plaintext
  passed to the shim as a process env var (never `shim.json`) and forwarded
  into the guest over the `MVM_*` channel (readable via `/proc/cmdline`, and
  deliberately *not* scrubbed so workload tooling — the MCP bridge — can
  present it). Constant-time hash compare over the sandbox list. Regenerated
  on restart, revoked on stop/removal; never accepted on the control plane.
  Delegate mechanics + in-guest transport/MCP bridge + control-plane human
  auth remain deferred.

- **Devpts stacking investigation (TODO#5)** (2026-08-10) — the original item
  reported that `tty` fails and `ls /dev/pts` is empty. Neither reproduces:
  the guestd mounts devpts *before* `openpty()`, so ptys land in the topmost
  (visible) instance. Two devpts instances exist (libkrun init + guestd), but
  `tty`, `ls /dev/pts`, `ttyname`, and non-root access all work. Added 4
  integration tests to the `console-resize`/`exec` integration sections
  (devpts count, ls, ttyname, non-root); all pass. The item is downgraded to
  a low-priority cleanup:
  deduplicate the devpts mount.

- **Raw socket creation banned in the guest (seccomp-bpf)** (2026-08-09) —
  the guestd installs a hand-built `SECCOMP_MODE_FILTER` at the top of
  `real_main` (before mounts/network; install failure is fatal, always-on, no
  opt-out) that denies `socket(2)` for `AF_PACKET` (any type) and
  `AF_INET`/`AF_INET6` with `(type & 0xf) == SOCK_RAW` → `ERRNO|EPERM`,
  allows everything else (incl. `AF_NETLINK`, which the network bootstrap
  creates with a raw type), and kills on a mismatched audit arch.
  `crates/guestd/src/seccomp.rs` builds the BPF with `libc::sock_filter` /
  `sock_fprog` + `prctl` (no new deps, musl-safe); the filter is inherited by
  every descendant (workload, exec sessions) and cannot be weakened.
  `scripts/integration/probes/rawprobe.c` probes the full matrix in-VM as
  both a workload and an exec session via `just raw-seccomp` (80/80 green).

- **Live console window resize for `mvm run -it` / `mvm attach`** (2026-08-09) —
  the console now tracks the terminal like `exec` does. The guestd `dup`s the
  workload pty master for itself (`console_pty: Option<OwnedFd>`, bridge keeps
  the original) and answers `AgentRequest::ConsoleResize` with `TIOCSWINSZ`
  (logged, not fatal, on failure); `POST /sandboxes/{id}/console/resize`
  reaches it via `Manager::console_resize`; `console_session` sends the local
  `term_size()` *before* console I/O — so `attach`'s create-time geometry is
  gone — then a 500 ms poll thread mirrors exec's. Gated on the workload
  having a pty, not on stdin. Covered by the `console resize` section in
  `integration.sh` (65/65 green). Follow-up (leftover): extract a shared
  "poll local tty size and invoke callback" helper for exec and console.


- **`mvm run` keeps by default; `--rm` removes on exit** (2026-08-09) —
  flipped to docker semantics. `run` no longer removes the sandbox when the
  workload exits unless `--rm`; the "kept" notice (id + generated name +
  state) goes to *stderr* so `mvm run` stays pipeable. TODO section 6 was
  this decision; the removed-on-exit notice now only fires under `--rm`.


- **`mvm clone` / `mvm clone --fork`** (2026-08-09) — new sandbox from an
  existing one's spec with every create flag overridable; `--fork` carries the
  source's current disk. `StorageDriver::duplicate(from, to)` reflink-copies
  the overlay `upper/` (or the whole rootfs under `copy`); `Manager` gained
  `clone_sandbox` (not `clone` — it shadows `Clone`'s) + a
  `POST /sandboxes/{id}/clone` route; the CLI flattens `CloneArgs` so only
  flags you pass override the inherited spec. Forking a running source is a
  point-in-time snapshot — stop it first for crash consistency. Covered by
  the `clone` section in `integration.sh` (58/58 green).


- **Dropped the `ext4` storage driver** (2026-08-06) — removed the
  opt-in block-device root and everything that existed only to serve it:
  `mkfs.ext4 -d` image building, the guestd's `pivot_root` onto /dev/vda
  (plus `apply_ownership` and the `mount` helper), the `root_disk`
  plumbing through `PreparedRootfs`/`ShimConfig`/the shim's virtio-blk
  attach, `MVM_ROOT_DISK`/`MVM_WORKDIR`, and the whole ownership-manifest
  pipeline (`OwnershipManifest`, `ownership.jsonl`, `OwnershipEntry`,
  `GUEST_OWNERSHIP_PATH`). Rationale: it was added to fix rootless guest
  chown, which the userns mode later solved properly for the default
  virtiofs path; it was opt-in, never exercised by `integration.sh`, and
  needed e2fsprogs (absent on macOS). Storage is now `overlay` + `copy`.
- **`run -t` lost the workload's last output** — the guestd bridges the
  workload's guest pty to the console on a detached thread, but nothing
  waited for it: once the workload exited, `real_main` returned and
  `process::exit` tore the process down mid-drain, so a short-lived `-t`
  workload raced its own final bytes. Reproduced at ~8/15 (`run -t alpine
  printf 'A\n'` losing its CRLF); the guestd now keeps the thread's
  JoinHandle and joins it before reporting `WorkloadExit` → 20/20.

- **Apple Silicon port** — mvm builds, boots and passes the integration
  suite 46/46 on aarch64-apple-darwin (2026-08-06). libkrun 1.19.4 from
  the `libkrun/krun` Homebrew tap (Hypervisor.framework, same-arch arm64
  guests); `scripts/install-darwin.sh` installs libkrun/libkrunfw, Zig, and
  cargo-zigbuild. Mechanism:
  Linux-only code cfg-gated (userns, guestd body, `/proc`, tap); Homebrew
  lib dir baked into every binary as rpath via `.cargo/config.toml`
  (libkrun dlopens libkrunfw by bare name); `dist/mvm` codesigned with
  the hypervisor entitlement (without it `krun_start_enter` → EINVAL);
  guestd cross-compiled with `cargo zigbuild` (host-arch musl target,
  `guestd_binary()` now rejects non-ELF candidates); copy driver uses
  `clonefile(2)` on APFS. macOS limits: no userns/chown fidelity, storage
  = copy (wiped per start), no tap profile.

- **E2E integration** — 49/49 on real KVM in rootless userns mode with the
  overlay driver, gvproxy v0.8.9 installed (2026-08-04); 21/21 at the
  original pass (2026-08-01).
- **TUI delete confirmation + selection fix** — `d` asks `y`/`n` before
  destroying a sandbox's filesystem. Naming the target in that prompt exposed
  a second bug: `clamp_selection` recovered a lost selection by jumping to the
  *last* row, so actions could silently hit a sandbox other than the one the
  user was looking at. Both covered by unit tests.
- **TUI console sanitizing** — the pane rendered guest bytes verbatim, so a
  guest's escape sequences drove the user's terminal; the ones that query it
  (shell prompts emit `ESC [ 6 n`) made the terminal reply on the TUI's stdin,
  where crossterm parses `ESC ]` + reply as Alt+`]` plus plain keys — `r` from
  `rgb:` opened the resize form by itself. Stripped at the poller edge, with
  the tail capped so a long console isn't refetched whole every poll.
- **Attach** — `mvm attach [--no-stdin] SANDBOX` and `mvm start -a`, so an
  interactive sandbox no longer has to be driven by the `run` that created
  it. `-i`/`-t` stay create-time properties (docker parity) and attach reads
  them off the spec; ctrl-p ctrl-q detaches without sending EOF; the replayed
  backlog is capped (`logs -n N` / `?tail=N` on the logs route).
- **Networking validated end to end** — the gvproxy host-to-guest
  forwarding question is closed: one gvproxy vfkit datagram endpoint serves
  exactly one VM (it learns its peer from the first packet and never
  re-learns), so bare `--net gvproxy` now spawns a *private* gvproxy per
  sandbox, registers `-p` maps on that instance's control socket, and
  SIGTERMs + reaps it with the VM (pid persisted for post-restart cleanup).
  `gvproxy:<socket>` still attaches to an external one. Sequential and
  concurrent sandboxes both have working forwards.
- **`mvm run -it`** — was frozen: `io::copy` into `stdout`'s LineWriter held
  prompts and raw-mode echo until a newline; `krun_set_exec` argv repeated
  the guestd path, so a second guestd instance ate the `MVM_*` vars and no
  workload pty was ever allocated; and three stacked line disciplines
  double-echoed and buffered keystrokes. Now: flush per chunk, argv without
  the exec path, raw shim pty + raw bridged console + explicit interactive
  termios on the workload pty, plus client winsize and TERM.
- **Resize CPU/memory** — `mvm resize SANDBOX [--cpus N] [-m MiB]
  [--restart]`, `POST /sandboxes/{id}/resize`, and the TUI's `r` form
  (tab/digits/+- , enter applies, ^r applies and restarts). No hot-plug in
  libkrun, so it rewrites the spec and the next boot picks it up; the form
  and CLI both say when a restart is pending.
- **Rootless chown / virtiofs UID translation** — `mvm serve` re-execs
  into a user namespace (subuid ranges via newuidmap/newgidmap, two-stage
  SIGSTOP handshake so the daemon keeps capabilities after exec); the
  in-process virtiofs server chowns to mapped uids. Unpack applies real
  tar-header ownership.
- **Overlay storage rootless** — `userxattr` mounts inside the userns;
  upper layer persists across stop/start; probe with labeled fallback.
- **Network isolation + modes** — `none` now really is isolated (dead
  unixgram NIC disables libkrun's default TSI); `tsi` exposed as an
  explicit zero-setup outbound mode (guestd writes public resolvers);
  `gvproxy[:<socket>]` fixed to the vfkit datagram protocol with
  guestd-side static IP/route/DNS bootstrap (ioctls, no guest binaries).
- **Exec**: binary-safe base64 streams; `-it` with real pty (openpty,
  devpts, Resize protocol + endpoint, raw-mode CLI); kill-on-disconnect
  guard on the API stream.
- **Interactive `mvm run -i/-t`** — console stdin attach over
  `POST /sandboxes/{id}/stdin`, VEOF on EOF, local raw mode with `-t`.
- **Deterministic lifecycle** — `start` waits for the guestd channel
  (exec also tolerates a concurrent start); log broadcast closes on shim
  exit so `run` streams to EOF and `logs -f` terminates.
- **Image store** — up-to-date digest short-circuit, atomic staged
  pulls, per-key pull locking, `rmi` in-use refusal.
- **Layer replacement unpacking** — later OCI hard-link entries replace an
  existing destination before unpacking, matching Docker layer semantics.
- **Guardrails** — dynamically-linked guestd rejected (PT_INTERP check);
  mvm flags after the image rejected with a hint.
