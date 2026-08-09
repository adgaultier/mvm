# TODO

Prioritized backlog. See `README.md` for user-facing behavior and
`AGENTS.md` for architecture notes and sharp edges.

## 1. Credentials injection (design plan)
Modeled on Docker Sandboxes' credential handling
(https://docs.docker.com/ai/sandboxes/security/credentials/), adapted to
mvm's daemon + vsock architecture. Guiding principle: **real secrets never
enter the guest** — not its env, not its filesystem; the guest sees
sentinels and the host injects at the network boundary.

**Milestone A — secret store + env injection (foundation):**
- `mvm secret set|rm|ls` in the daemon. Backends: Linux Secret Service
  (GNOME Keyring / KDE Wallet) via D-Bus, falling back to an encrypted
  file under the data dir (0700 dir / 0600 file, same posture as
  `~/.docker/config.json`).
- Scopes, docker-style: global (all sandboxes, applied at create) vs
  per-sandbox (immediate). `mvm run --secret NAME[=ENV_VAR]` resolves at
  start. Secret *names* go in `SandboxSpec`; values are resolved at start
  time only — never persisted in `sandboxes.json`, redacted in
  `mvm inspect` output.
- Weakness (accepted for A): the value still lands in the guest's env,
  like `-e` today. A is about storage hygiene + UX, not isolation.

**Milestone B — credential proxy with sentinel values (the real thing):**
- Guest env gets `ANTHROPIC_API_KEY=mvm-proxy-managed` (sentinel), plus
  `HTTP(S)_PROXY=http://127.0.0.1:<port>`. The agent bridges that
  loopback port over **vsock** to a host-side proxy owned by the daemon —
  works in every network mode including `none`/`tsi`, since egress
  happens host-side.
- The host proxy matches requests against a service map (api.anthropic.com,
  api.openai.com, api.github.com, generativelanguage.googleapis.com, …)
  and swaps the sentinel for the real header value on the way out.
  Built-in service list + user-defined `(domain, header, secret)` triples.
- HTTPS: header injection requires TLS termination at the proxy. Generate
  a per-data-dir CA, terminate+re-originate TLS for *matched* domains
  only, and have the agent install the CA into the guest trust store at
  boot (append to `/etc/ssl/certs/ca-certificates.crt` + the common
  distro variants). Unmatched domains get plain CONNECT tunneling — no
  MITM outside the declared service list.
- Egress policy falls out for free: the proxy can enforce a domain
  allowlist per sandbox (deny-by-default option for agentic workloads).

**Milestone C — forwarding, not copying:**
- SSH agent forwarding over vsock (`SSH_AUTH_SOCK` bridged by the guest
  agent) for git push/commit signing — keys never enter the guest.
- OAuth-style flows (Claude Code, etc.): token lives host-side, proxy
  injects; sandbox never sees it.

Non-goals for now: console-log secret redaction; Windows/macOS keychains.


## 4. Live window resize for `mvm run -it` / `mvm attach`
The workload pty boots at the client's size and TERM is forwarded, but the
size is fixed for the session: resizing the terminal mid-run leaves the guest
on the old geometry (exec -it has full resize). `attach` is worse — it uses
`tty_size` as recorded at *create* time, so attaching from a differently sized
terminal starts out wrong. Needs a console-level resize path: a sandbox-keyed
Resize message (the protocol's is keyed to an exec session) plus TIOCSWINSZ on
the workload pty in the agent, with `run`/`attach` polling `term_size()` the
way `exec` does.

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
