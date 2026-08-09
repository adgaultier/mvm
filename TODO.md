# TODO

Prioritized backlog. See `README.md` for user-facing behavior and
`AGENTS.md` for architecture notes and sharp edges.

## 1. Credentials injection (design plan)
Modeled on Docker Sandboxes' credential handling
(https://docs.docker.com/ai/sandboxes/security/credentials/), adapted to
mvm's daemon + vsock architecture. Guiding principle: **real secrets never
enter the guest** — not its env, not its filesystem; the guest sees
sentinels and the host injects at the network boundary.

SEE CREDENTIALS.md


## 5. Guest ptys are not reachable by path (`ttyname` fails)
Inside a guest, `tty` reports "not a tty" and `ls /dev/pts` does not show the
workload's pty even though `[ -t 0 ]` is true and `/proc/self/fd/0` points at
`/dev/pts/0`: three devpts instances end up stacked on `/dev/pts` (libkrun's
init, then the agent's), and the pty is allocated in one that a later mount
shadows. Anything resolving a tty by name — `sudo`, `screen`, `script`,
`os.ttyname` — sees an inconsistent world. Fix is for `ensure_devpts` to reuse
a usable existing instance instead of stacking another; check what ptmxmode
that leaves for non-root workloads before changing it.

## 7. Pull memory usage
Layer blobs are buffered fully in RAM during pull (fetch + sha256 +
unpack from `Vec<u8>`); a 300 MB layer means a 300 MB spike. Stream to a
temp file with incremental hashing instead — matters for images like
`docker/sandbox-templates:opencode` (~750 MB compressed).

## 8. `clone --checkpoint`: carry a stopped VM's memory (blocked on libkrun)

Snapshot the source VM's *memory* so `mvm clone --fork --checkpoint SRC` creates a clone that resumes where it left off — effectively suspend-to-disk for the VM. This extends `--fork`: the source is frozen, its VM state is captured, then it is stopped and its overlayfs duplicated; the clone restores the captured state on boot.

**Firecracker comparison spike.** Firecracker's model is two files: a full guest-memory dump plus a versioned microVM state file. Device state is serialized through a `Persist` trait, with explicit restore invariants around compatible KVM/kernel semantics, CPU model/architecture, and externally-backed resources. libkrun today has only a small part of this: `krun_vm_pause`/`krun_vm_resume` exist on **main (2.0-dev)**, but only for macOS/Hypervisor.framework; they return `-ENOTSUP` on Linux/KVM and are absent from released 1.x branches. There is currently no snapshot/save/load API: nothing serializes guest RAM or vCPU/device state, and no restore-before-boot path.

**Conclusion:** libkrun could implement the Firecracker model because it owns the relevant VM state, but this would be new libkrun functionality rather than something mvm can build against today. Conceptually, the flow would be a new save API in shim A producing VM state + RAM files in the sandbox, followed by a corresponding load API in shim B before `krun_start_enter`. This avoids the in-guest CRIU dependency entirely, but requires libkrun work for Linux/KVM pause, RAM/state serialization, a versioned state format, and restore invariants.

The mvm-side plumbing remains valid and cheap to land independently: `common::protocol::Checkpoint`, a `checkpoint` spec flag alongside `fork`, and `Manager::clone_sandbox` doing **checkpoint → stop → duplicate**. `--fork` already stops before copying overlayfs, so `--checkpoint` simply adds VM execution state to the existing disk snapshot. Expose `clone --checkpoint` and, if useful, `mvm checkpoint SRC`.

## 9. Agent gateway — knative-style activation in front of mvm
Internal company tool (not client-facing): one long-lived agent per
user/project, reachable at a stable URL, woken on demand and stopped when
idle. Traefik does ingress/TLS/auth-forwarding only; the gateway owns compute
lifecycle. Separate crate, one host to start with.

```
client -> Traefik (*.agents.corp, TLS, forward-auth)
       -> agent gateway :8080  (activation + reverse proxy)
       -> 127.0.0.1:<agent port> -> mvm sandbox -> opencode :4096
```

**Decisions taken.** One host: gateway and `mvm serve` colocated, daemon on
loopback, ports allocated locally — a 64 GB box holds 30–60 agents at
512 MiB–1 GiB. Going multi-host turns the gateway into a scheduler (capacity,
workspace placement) and needs auth on mvm's API, so treat it as a separate
project, not a config flag. A host port per agent is accepted: allocate at
*create* (mvm's `ports` are create-time only), store it on the agent record,
own it for the agent's life, release on delete. `agent_id` **is** the mvm
sandbox name — names are unique and resolvable by name/id/prefix, so no
mapping table. mvm owns lifecycle state (`GET /sandboxes/{id}`); the gateway's
DB holds only what mvm cannot know: owner, viewers, host port, `last_used_at`,
quotas, image pin. Reconcile against mvm on startup.

**Where knative's model does not fit, and it matters.** Knative assumes
request-scoped work: no traffic means nothing is happening. An agent can spend
ten minutes editing files and running tests with no HTTP traffic at all, so
idle-by-traffic kills working agents and looks random. Idle must mean *no HTTP
activity **and** no active work*, with "active work" coming from the workload
(an opencode `/status`) or from `mvm exec <id>` inspecting the guest. Likewise
`last_used_at` must be bumped when a stream *completes*, or a 20-minute SSE
response gets its VM shut out from under it.

**Phases.**
1. Stable URL + forward-auth; proxy to an already-running agent; agent
   registry with port allocation. Streaming, WebSockets, long requests,
   cancellation, no response buffering.
2. Activation: per-agent startup lock so concurrent requests coalesce onto one
   start; wait for the workload's own health endpoint; explicit activation
   budget with 503 + `Retry-After` past it. Measure workload-ready time from
   day one — VM boot is ~250 ms, the workload is the real cost, and activation
   latency is the SLA.
3. `agentctl shell|logs|status` — mvm's `exec -it`/`attach` over vsock work
   even with `--net none`, and console/exit-code/uptime are already in the
   API. This is the feature that makes the platform supportable, and no
   competitor to it has anything comparable; do not leave it for last.
4. Admission control: per-user concurrent cap, global memory budget, queue
   with a visible "waiting for capacity" state, per-agent size via
   `mvm resize`. Then idle shutdown (per the work-detection rule above),
   crash-loop backoff, and metrics: activation count/latency histogram,
   per-agent memory.
5. `agentctl upgrade <id> --image X` as a first-class verb: create a new
   sandbox with the same workspace volume, verify healthy, delete the old.
   mvm specs are immutable apart from cpu/ram, so done ad hoc this loses work.

**Networking.** Start with `--net tsi`: zero setup, `-p` maps work, one less
process per VM. Switch to `--net gvproxy` (one per sandbox) when egress policy
matters — inside a corp network a hallucinating agent reaching internal
services is the real risk, and only gvproxy can hold an allowlist.

**Secrets.** Depends on credentials injection. Cheapest large win first:
per-agent GitHub App installation tokens (hour-scoped) instead of long-lived
PATs. LLM-generated code inside the VM can read its own environment, so its
Milestone B
(sentinel + host proxy over vsock) is the real answer for API keys — and it
shares the vsock bridge with everything else here.

**Deliberately not doing.** A multi-backend `VMManager` abstraction
(Firecracker/Cloud Hypervisor/Docker): one runtime exists, and a lowest-common
-denominator interface would forfeit exec-over-vsock and per-sandbox gvproxy.
Keep the four operations as a module boundary; extract a trait if a second
backend ever appears. Also no separate `status` column duplicating mvm's.

**Blocked on / related:** credentials injection, and pull memory usage (a
750 MB image spikes RAM by its layer size on a box also running 40 VMs). The
image-USER blocker is gone — the opencode image runs as `agent`, and mvm now
honors that. A watch/SSE endpoint on `/sandboxes` would beat polling once
agent counts grow.
## Done

- **Live console window resize for `mvm run -it` / `mvm attach`** (2026-08-09) —
  the console now tracks the terminal like `exec` does. The agent `dup`s the
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
  `mkfs.ext4 -d` image building, the agent's `pivot_root` onto /dev/vda
  (plus `apply_ownership` and the `mount` helper), the `root_disk`
  plumbing through `PreparedRootfs`/`ShimConfig`/the shim's virtio-blk
  attach, `MVM_ROOT_DISK`/`MVM_WORKDIR`, and the whole ownership-manifest
  pipeline (`OwnershipManifest`, `ownership.jsonl`, `OwnershipEntry`,
  `GUEST_OWNERSHIP_PATH`). Rationale: it was added to fix rootless guest
  chown, which the userns mode later solved properly for the default
  virtiofs path; it was opt-in, never exercised by `integration.sh`, and
  needed e2fsprogs (absent on macOS). Storage is now `overlay` + `copy`.
- **`run -t` lost the workload's last output** — the agent bridges the
  workload's guest pty to the console on a detached thread, but nothing
  waited for it: once the workload exited, `real_main` returned and
  `process::exit` tore the process down mid-drain, so a short-lived `-t`
  workload raced its own final bytes. Reproduced at ~8/15 (`run -t alpine
  printf 'A\n'` losing its CRLF); the agent now keeps the thread's
  JoinHandle and joins it before reporting `WorkloadExit` → 20/20.

- **Apple Silicon port** — mvm builds, boots and passes the integration
  suite 46/46 on aarch64-apple-darwin (2026-08-06). libkrun 1.19.4 from
  the `libkrun/krun` Homebrew tap (Hypervisor.framework, same-arch arm64
  guests); `scripts/install-darwin.sh` provisions the machine. Mechanism:
  Linux-only code cfg-gated (userns, agent body, `/proc`, tap); Homebrew
  lib dir baked into every binary as rpath via `.cargo/config.toml`
  (libkrun dlopens libkrunfw by bare name); `dist/mvm` codesigned with
  the hypervisor entitlement (without it `krun_start_enter` → EINVAL);
  agent cross-compiled with `cargo zigbuild` (host-arch musl target,
  `agent_binary()` now rejects non-ELF candidates); copy driver uses
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
  the agent path, so a second agent instance ate the `MVM_*` vars and no
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
  explicit zero-setup outbound mode (agent writes public resolvers);
  `gvproxy[:<socket>]` fixed to the vfkit datagram protocol with
  agent-side static IP/route/DNS bootstrap (ioctls, no guest binaries).
- **Exec**: binary-safe base64 streams; `-it` with real pty (openpty,
  devpts, Resize protocol + endpoint, raw-mode CLI); kill-on-disconnect
  guard on the API stream.
- **Interactive `mvm run -i/-t`** — console stdin attach over
  `POST /sandboxes/{id}/stdin`, VEOF on EOF, local raw mode with `-t`.
- **Deterministic lifecycle** — `start` waits for the agent channel
  (exec also tolerates a concurrent start); log broadcast closes on shim
  exit so `run` streams to EOF and `logs -f` terminates.
- **Image store** — up-to-date digest short-circuit, atomic staged
  pulls, per-key pull locking, `rmi` in-use refusal.
- **Layer replacement unpacking** — later OCI hard-link entries replace an
  existing destination before unpacking, matching Docker layer semantics.
- **Guardrails** — dynamically-linked agent rejected (PT_INTERP check);
  mvm flags after the image rejected with a hint.
