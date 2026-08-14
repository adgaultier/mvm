# AGENTS.md — working on mvm

## What this is

A docker-style microVM sandbox platform: OCI images run as KVM microVMs via
libkrun. Workspace of 11 crates under `crates/`. `README.md` is the
user-facing documentation (CLI, API, storage/network modes); `TODO.md` is
the living backlog and done-log. This file is for people (and agents)
changing the code.

## Build & test

```sh
cargo build --workspace          # host binaries (mvm, mvm-tui, gnu mvm-agent)
cargo test --workspace           # unit tests, no KVM needed
cargo build -p mvm-agent --target x86_64-unknown-linux-musl --release
                                 # the REAL guest agent (static). The gnu-linked
                                 # target/debug/mvm-agent will NOT run in guests
                                 # (and the daemon refuses to inject it).
scripts/build.sh                 # all of the above, release, into dist/
just -f scripts/integration/Justfile all   # boots real VMs; needs /dev/kvm
                                 # (Linux) or libkrun (macOS), plus network
                                 # (or `just <section>` for one, `just` to list)
```

Hosts: Linux x86_64 (KVM) and macOS Apple Silicon (Hypervisor.framework,
via Homebrew libkrun — `scripts/install-darwin.sh` sets the machine up).
Guests are same-arch as the host, so the agent's musl target follows the
host arch; on macOS the cross-link goes through `cargo zigbuild` and the
`mvm` binary must carry the hypervisor entitlement (build.sh codesigns
dist/; a bare `cargo build` binary needs it too — see Sharp edges).

The guest agent is musl-only because `-C target-feature=+crt-static` on the
gnu target breaks proc-macro crates (`serde_derive`). `agent_binary()`
resolution: `MVM_AGENT_PATH` → next to the executable → the dev tree's
`target/<host-arch>-unknown-linux-musl/release/` → `PATH`, skipping any
candidate that is dynamically linked or not a Linux ELF at all.

## Architecture decisions (the "why")

- **Daemon + thin clients.** `mvm serve` owns all state behind HTTP on
  `127.0.0.1:24642`; CLI/TUI are stateless HTTP clients.
- **One shim process per VM.** `krun_start_enter()` takes over the calling
  process, so the daemon re-execs `mvm __vm-shim <shim.json>` per sandbox
  (detached session leader; its stdout/stderr *is* the guest console).
  With `attach_stdin` sandboxes the shim's stdin is a pipe feeding the
  console.
- **Guest agent as the guest's init** (`/.mvm/agent`, injected at start,
  exec'd by libkrun's `/init.krun` which is PID 1): spawns the workload,
  reaps zombies, serves exec (pipes or pty) over **vsock port 1024** →
  `<sandbox>/agent.sock` host-side. Wire protocol: u32-BE length + JSON
  frames, byte payloads base64-encoded (`common::protocol`). With `-t` the
  workload gets its own guest pty and the agent bridges it to the console.
- **Rootless userns mode (Linux-only).** `serve` re-execs into a user namespace
  (0 → user, 1.. → /etc/subuid) so the in-process virtiofs server can
  chown to mapped uids. Two-stage SIGSTOP handshake in
  `cli/src/userns.rs` — the child must re-exec *after* the maps are
  written or execve strips its capabilities (see Sharp edges). On macOS
  there is no userns; the daemon runs with host credentials.
- **Storage** (`storage` crate): `overlay` (default under root/userns;
  `userxattr` inside a userns; upper persists across restarts) and `copy`
  (fallback, wiped each start). Both are served to the guest over virtiofs.
- **Networking** (`--net`): `none` = dead unixgram NIC (disables libkrun's
  default TSI!), `tsi` = transparent host-serviced sockets, `gvproxy` =
  vfkit-mode datagram socket + agent-side static IP bootstrap (the daemon
  runs one gvproxy *per sandbox*; `gvproxy:<socket>` attaches to yours),
  `tap:<dev>` = pre-provisioned TAP.
- **No CPU/RAM hot-plug.** `mvm resize` / the TUI's `r` form rewrite the
  spec; the allocation changes at the next boot, not live.
- **`-i`/`-t` are create-time properties** (docker parity): `attach_stdin`
  decides whether the daemon keeps the console's write end, `tty` whether the
  workload gets a guest pty. `start` therefore takes no `-i`/`-t` — attaching
  is the per-client choice (`mvm attach`, `mvm start -a`), and it reads both
  flags off the spec so a client can never contradict the running VM.

## Data layout (`~/.local/share/mvm`, root: `/var/lib/mvm`, `$MVM_DATA_DIR`)

```
<data>/
├── sandboxes.json          # registry (atomic tmp+rename persistence)
├── images/<store-key>/     # meta.json, rootfs/
│                           # (.pulling-* = staging dirs, skipped by list)
└── sandboxes/<id>/
    ├── shim.json           # ShimConfig written by the daemon
    ├── console.log         # guest console (appended by log pump)
    ├── krun.log            # libkrun's own diagnostics, kept off the console
    ├── rootfs|upper|work/  # per-driver filesystem state
    └── agent.sock          # agent control channel (unix listener)
```

## Crate map

| Crate | Notes |
|---|---|
| `common` | shared types, `DataDir` layout, vsock frame protocol + b64 — no heavy deps |
| `krun-sys` | hand-written libkrun FFI; keep in sync with `/usr/include/libkrun.h` |
| `runtime` | `KrunContext` RAII wrapper, shim entry (`run_shim`), shim spawner |
| `image` | registry client (blocking reqwest), layer unpack + ownership manifest, `ImageStore` |
| `storage` | `overlay` / `copy` drivers, overlay probe |
| `network` | profile validation, port-map parsing |
| `manager` | sandbox registry + lifecycle; owns agent vsock channels + console stdin |
| `agent` | guest PID 1; std+libc only, poll(2) event loop, must stay static-friendly |
| `api` | axum routes; streams = `Body::from_stream`; exec has a kill-on-drop guard |
| `cli` | `mvm` binary incl. hidden `__vm-shim` subcommand + userns re-exec |
| `tui` | ratatui dashboard; modal forms own the keyboard while open (`r` resize, `d` delete confirmation) |

## Sharp edges (learned the hard way)

- **`krun_start_enter` never returns** — it exits the process with the
  workload's code. VM boot must happen in the re-executed shim process,
  never in the daemon.
- **`reqwest::blocking` clients must be constructed OUTSIDE tokio.**
  `Manager::new` builds one (registry client); `serve()` deliberately
  creates the Manager before building the runtime.
- **execve before uid maps = empty capability set.** Never exec inside a
  fresh userns before `newuidmap` ran; the process looks like root but
  can't mount/unshare, and nothing restores the caps afterward. Hence the
  stage-1 self-SIGSTOP + stage-2 re-exec in `userns.rs`. (A pre-exec
  SIGSTOP deadlocks `Command::spawn`, which waits for the exec.)
- **libkrun defaults to TSI networking** when no NIC is configured — "no
  network device" does NOT mean isolated. `none` mode attaches a dead
  socketpair-backed NIC precisely to switch TSI off.
- **`krun_set_gvproxy_path` speaks the vfkit *datagram* protocol** —
  gvproxy must run with `-listen-vfkit unixgram://…`, not `-listen-qemu`.
- The agent runs as **the guest's init**: it must reap zombies and only use
  std + libc (keep dependencies out of `crates/agent`). Pty sessions share
  one fd for stdin/stdout — mind the close-once logic. libkrun's own
  `/init.krun` is literally PID 1 and execs the agent as its child.
- **`krun_set_exec` argv must NOT repeat the exec path** — `init.krun`
  prepends it as `argv[0]` itself. Passing it again made the agent treat
  `/.mvm/agent` as its own workload and run a *second* agent: the outer
  instance consumed every `MVM_*` var (and `remove_var`'d it) before the
  inner one — the instance that actually spawns the workload and serves
  exec — ever saw it, so `MVM_CONSOLE_TTY` silently did nothing. The tell is
  two `/.mvm/agent` entries in the guest's `ps`.
- **The `MVM_*` env channel rides the kernel cmdline.** libkrun serialises
  envp into the guest cmdline (visible in `/proc/cmdline`, quoted, before
  `--`); the kernel hands `KEY=VALUE` params to init, which passes them on.
  When a var "disappears", compare `/proc/1/environ` with the agent's own
  env before suspecting the transport.
- **A guest pty needs its termios spelled out.** This kernel's fresh pty
  slaves come up with `ONLCR` clear, so a workload pty built with
  `openpty(…, NULL, NULL)` emits bare LFs and staircases any client in raw
  mode. `interactive_pty_termios` sets it explicitly.
- **Exactly one line discipline per stream.** Console plumbing stacks ptys
  (host shim pty → guest console → workload pty); every layer that stays
  canonical adds an echo, buffers input until Enter, and steals ^C. Only
  the innermost pty — the workload's — should be cooked; the shim pty and
  the bridged console are `cfmakeraw`'d.
- **Never render guest console bytes verbatim in a TUI.** The TUI's console
  pane passed them through, so the guest's escape sequences drove the user's
  terminal — and the ones that *ask* it something (every shell prompt emits
  `ESC [ 6 n`; TUIs query colours with `ESC ] 11 ; ?`) make the terminal answer
  on the TUI's stdin. crossterm parses `ESC ]` as Alt+`]` and then the rest of
  the reply as ordinary keys, so `ESC ] 11 ; rgb:2d2d/…` types `r`, `g`, `b`,
  `d`… into the app: the resize form opened by itself and `d` was one hex digit
  away from deleting a sandbox. Sanitizing at the poller edge fixed it; the
  pane was then dropped altogether (console output is `mvm logs`), which is
  why no `sanitize_console` exists today. Any future widget showing console
  text needs that filter back.
- **A detach cannot unwind through the log stream.** The blocking read on the
  follow stream can't be interrupted, so the stdin thread leaves the process
  itself — which skips `RawTermGuard`'s `Drop`. The saved termios therefore
  lives in a static that both paths restore from (`restore_terminal`);
  forgetting that leaves the user's shell in raw mode.
- **`std::io::stdout()` is a `LineWriter`.** `io::copy` into it holds
  anything without a trailing newline (shell prompts, raw-mode echo) until
  the next `\n` or process exit — which is exactly what a frozen terminal
  looks like. Flush per chunk on interactive paths.
- **One gvproxy serves one VM, for life.** Its vfkit datagram endpoint
  learns the peer from the first packet and never re-learns, so a shared
  socket leaves every later VM with no route and no error (and all guests
  boot on the same static 192.168.127.2 anyway). Bare `--net gvproxy` runs
  a private gvproxy per sandbox; kill *and* `wait()` it, or it lingers as a
  zombie holding host ports.
- **`unshare(CLONE_NEWUSER)` requires a single-threaded process** — userns
  entry must happen before the tokio runtime exists.
- **Terminal *queries* are filtered per consumer, not per stream.** A tty
  session's output contains sequences that ask the terminal to answer — DSR
  (`ESC[6n`, "where is the cursor?") and Device Attributes (`ESC[c`). A
  question that reaches someone who will not answer it makes *their*
  terminal answer into its own input buffer: stray `^[[1;5R` in their shell
  (the alpine prompt `~ # ` is 4 columns wide, hence column 5). So:
  `manager::console_filter` strips queries from `console.log`, and the logs
  route runs the same filter over the live broadcast unless the client asks
  for `?raw=true`. Only an interactive console session (`mvm attach`,
  `mvm run -it`) sets `raw` — it owns the terminal and reads the reply.
  Filtering only the recording is the trap: it leaves `mvm logs -f`'s live
  tail unprotected, which is the same bug with a longer path to it. Colours,
  cursor motion and erases are real output and stay everywhere.
- **macOS: no hypervisor entitlement, no VMs.** Hypervisor.framework
  refuses binaries that don't carry `com.apple.security.hypervisor`;
  `krun_start_enter` fails with `EINVAL` and no better hint. `build.sh`
  codesigns `dist/mvm` with `scripts/hypervisor.entitlements`, and
  `integration.sh` signs whatever binary it tests — a bare `cargo build`
  binary needs the same `codesign --force --sign - --entitlements …`.
- **macOS: libkrun dlopens `libkrunfw` by bare name** — dyld resolves that
  against the *calling binary's* rpath, so `.cargo/config.toml` bakes the
  Homebrew lib dirs into every binary. (A `-sys` crate's
  `cargo:rustc-link-arg` does not reach the final link — only its
  link-search does — which is why the rpath lives in the config, not in
  `krun-sys/build.rs`.)
- **macOS is not GNU userland.** BSD `cp` rejects `--reflink`, so the copy
  driver uses `clonefile(2)` there; `script(1)`, `sha256sum` and
  `timeout(1)` differ too — `integration.sh` wraps them (`run_pty`,
  `sha256_stream`, a PATH shim for `timeout`, which must be a real command
  because `script` execs it).
- **The shim's stderr is the guest console, so libkrun must log elsewhere.**
  libkrun maps the guest's fd 2 onto the shim's, which is why stderr joins
  the console deliberately — but that also files libkrun's *own* host-side
  diagnostics as guest output. `deferring proxy removal` from the vsock
  muxer thread ends up in `console.log` and in `mvm run`'s stdout, and it
  appears at VM teardown, so it lands mid-capture and breaks anything
  comparing console output exactly (it was failing 10 integration checks).
  `krun_init_log(fd, …)` points libkrun at `<sandbox>/krun.log` instead;
  `krun_set_log_level` alone cannot fix this, since it only sets verbosity
  on stderr. Style must be `NEVER` — the fd is a file, not a terminal.
- **The guest resolves the guest's users.** `USER` / `-u` is passed through
  as `MVM_USER` and looked up against the *rootfs's* `/etc/passwd` by the
  agent, never against the host's user database — the same name means
  different uids in different images. The drop order in `apply_user` is
  setgroups → setgid → setuid (dropping the uid first forfeits the privilege
  needed for the other two), and it is registered *after* the pty
  `pre_exec`s, since `TIOCSCTTY` must run while still root. The pty slave is
  `fchown`ed to the target so the workload can reopen `/dev/tty`.
- Whiteout handling and layer unpack have unit tests in `crates/image` —
  extend those rather than testing via pulls. Later OCI layers may replace an
  existing path with a hard link; remove the destination before
  `tar::Entry::unpack_in` or the unpack fails with `EEXIST`.
- **Raw sockets are banned guest-wide, always.** The agent installs a
  seccomp filter at the top of `real_main` (before mounts/network; a failed
  install is fatal) that denies `socket(2)` for `AF_PACKET` (any type) and
  `AF_INET`/`AF_INET6` with `(type & 0xf) == SOCK_RAW`. The check keys on the
  *domain first* precisely because `AF_NETLINK` sockets are routinely created
  with a raw type (`SOCK_RAW | SOCK_CLOEXEC` in the network bootstrap) — that
  is legitimate and stays allowed. The filter is inherited and additive, so a
  workload cannot weaken it, and it kills on an unexpected arch (a wrong
  syscall number would otherwise be interpreted against the wrong table).
   This breaks `tcpdump`, `arping`, old `ping`, and AF_PACKET DHCP clients
   (`udhcpc`); the planned `--net passt` agent-side bootstrap must NOT rely on
   DHCP — it needs the gvproxy-style static-IP bootstrap instead.    Always-on
   by design: there is no opt-out. Filter shape is probed in-VM by
   `scripts/integration/probes/rawprobe.c` through `just raw-seccomp` (as
   workload *and* exec session).
- **`--security=strict` is a workload-scoped *second* seccomp filter.** The
  raw-socket ban above is installed on the agent (PID 1) at boot and inherited
  by everything. The strict filter is different: it is installed in the
  workload's `pre_exec` (`apply_strict_seccomp` in `linux.rs`, *before*
  `apply_user`'s privilege drop), so only the workload — and exec sessions —
  lose `bpf`/`keyctl`/`perf_event_open`/`userfaultfd`/`io_uring_*`, while the
  agent keeps the full syscall surface it needs for exec/pty plumbing. The
  var rides the `MVM_*` channel as `MVM_SECURITY_STRICT` and is scrubbed like
  the other plumbing vars. This matters for the Aya plan: the agent can still
  load eBPF programs itself (Phase 2) even in strict mode — the workload just
  can't. Plumbed: `--security` on `create`/`run`/`clone` → `SandboxSpec` →
  `ShimConfig` → shim env → agent. Probe the guest kernel's BPF capability
  with `scripts/integration/probes/bpfprobe.c` (BTF/cgroup2/prog-load
  verdict) before assuming any in-guest eBPF enforcement is possible.
 - **VM token: never persisted, never exposed — not even its hash.** The Agent
  API token is minted in `Manager::start` and held as `agent_token_hash`
  (SHA-256) on the `Sandbox` record — but that field is `#[serde(skip)]`, so
  it is neither serialized into API responses nor written to `sandboxes.json`:
  it lives only in the manager's memory for the VM's lifetime and is cleared
  the moment the sandbox stops or exits (not merely gated on state). The
  plaintext token is passed to the shim as a *process env var*
  (`MVM_AGENT_TOKEN`) — never through `ShimConfig`, so `shim.json` stays
  token-free — and the shim forwards it into the guest via the `MVM_*` env
  channel, where it is deliberately **not scrubbed**: it must reach the
  workload's environment (and every exec session, via `baseline_env`) so the
  tools it spawns (the `mvm-agent-mcp` bridge) can authenticate to
  `/agent/v1`. The plaintext therefore exists only transiently — in the shim's
  process environment, in the guest environment, and on `/proc/cmdline` (the
  `MVM_*` channel rides the kernel cmdline) — and is never written to host
  disk. The Agent API routes carry no `{id}` — the sandbox is derived from the
  token, so a caller can only act on itself. Token lookup uses a constant-time
  hash compare over the sandbox list (no `HashMap` keyed on the secret).
- **macOS rootfs loses image ownership (host uid owns everything).** The copy
  driver writes the rootfs as the host user and macOS has no userns, so
  `/home/agent` ends up owned by the host uid and a non-root workload can't
  write its own home (`PermissionDenied` on opencode's log). The shim sets
  `MVM_HOST_OS=macos` (host-gated via `#[cfg(target_os = "macos")]`), and the
  agent — before spawning a non-root workload whose home is owned by someone
  else — recursively `lchown`s the home to the workload's uid/gid. macOS
  virtiofs `LinuxComplete` semantics turns that chown into a
  `user.containers.override_stat` xattr, so it sticks for the boot; it's a
  no-op on Linux (userns already owns the files, and the uid check skips it).
- **Mount host paths must be absolute.** libkrun's virtiofs passthrough opens
  the host dir with `openat(AT_FDCWD, …, O_NOFOLLOW)` from the *daemon's* cwd,
  so a relative `-v` source fails with `ENOENT` at device activation — which
  surfaces as a guest `fc_vcpu` panic `Failed to activate device: BadActivate`
  (and `krun.log` records `virtio_fs: failed to create worker: No such file
  or directory`). The CLI canonicalizes `-v` host paths (`parse_volume`), and
  `Manager::validate_mounts` rejects any relative path that still slips in via
  the API/TUI.




## Runtime env vars

`MVM_HOST` (client → daemon addr), `MVM_AGENT_ADDR` (Agent API listen addr,
default `127.0.0.1:24643`), `MVM_DATA_DIR` (state root),
`MVM_AGENT_PATH` (guest agent binary), `MVM_STORAGE_DRIVER`
(`overlay`/`copy`), `MVM_USERNS=0` (disable userns mode), `MVM_GVPROXY_BIN`
(gvproxy binary for managed `--net gvproxy`), `MVM_GVPROXY_CONTROL`
(control socket of a gvproxy *you* run, for `--net gvproxy:<socket>` port
maps).

Daemon → guest agent (set by the shim): `MVM_MOUNTS`, `MVM_NET_CONFIG`,
`MVM_NET_TSI`, `MVM_CONSOLE_TTY`, `MVM_CONSOLE_SIZE`, `MVM_USER`,
`MVM_HOST_OS` — scrubbed by the agent before it spawns the workload — plus
`MVM_AGENT_TOKEN`, which is deliberately *not* scrubbed so the workload's
tooling (the `mvm-agent-mcp` bridge) can authenticate to `/agent/v1`.

## Conventions

- Errors: `mvm_common::Error` (`thiserror`) inside crates; CLI-facing code
  maps to `String`.
- Keep the daemon's HTTP surface documented in README when routes change.
- Integration test must stay green and self-contained: `just -f
  scripts/integration/Justfile all` runs against an isolated `MVM_DATA_DIR` +
  port, skips gracefully (KVM, gvproxy), and cleans up after itself.
- Record non-obvious findings in this file's Sharp edges; track work in
  `TODO.md` (move finished items to its Done section with a one-liner on
  the mechanism).
