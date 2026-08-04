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
scripts/integration.sh           # boots real VMs; needs /dev/kvm, libkrun, network
```

The guest agent is musl-only because `-C target-feature=+crt-static` on the
gnu target breaks proc-macro crates (`serde_derive`). `agent_binary()`
resolution: `MVM_AGENT_PATH` → next to the executable → the dev tree's
`target/x86_64-unknown-linux-musl/release/` → `PATH`, skipping any
candidate with an ELF `PT_INTERP` (dynamically linked).

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
- **Rootless userns mode.** `serve` re-execs into a user namespace
  (0 → user, 1.. → /etc/subuid) so the in-process virtiofs server can
  chown to mapped uids. Two-stage SIGSTOP handshake in
  `cli/src/userns.rs` — the child must re-exec *after* the maps are
  written or execve strips its capabilities (see Sharp edges).
- **Storage** (`storage` crate): `overlay` (default under root/userns;
  `userxattr` inside a userns; upper persists across restarts), `copy`
  (fallback, wiped each start), `ext4` (opt-in block-device root; agent
  pivot_roots onto /dev/vda and applies the tar-header ownership manifest
  recorded at unpack).
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
├── images/<store-key>/     # meta.json, ownership.jsonl, rootfs/
│                           # (.pulling-* = staging dirs, skipped by list)
└── sandboxes/<id>/
    ├── shim.json           # ShimConfig written by the daemon
    ├── console.log         # guest console (appended by log pump)
    ├── rootfs|upper|work/  # per-driver filesystem state
    ├── disk.img            # ext4 driver only (sparse)
    ├── bootstrap/          # ext4 driver: agent + ownership manifest
    └── agent.sock          # agent control channel (unix listener)
```

## Crate map

| Crate | Notes |
|---|---|
| `common` | shared types, `DataDir` layout, vsock frame protocol + b64 — no heavy deps |
| `krun-sys` | hand-written libkrun FFI; keep in sync with `/usr/include/libkrun.h` |
| `runtime` | `KrunContext` RAII wrapper, shim entry (`run_shim`), shim spawner |
| `image` | registry client (blocking reqwest), layer unpack + ownership manifest, `ImageStore` |
| `storage` | `overlay` / `copy` / `ext4` drivers, overlay probe |
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
- **Never render guest console bytes verbatim in the TUI.** The console pane
  used to pass them through, so the guest's escape sequences drove the user's
  terminal — and the ones that *ask* it something (every shell prompt emits
  `ESC [ 6 n`; TUIs query colours with `ESC ] 11 ; ?`) make the terminal answer
  on the TUI's stdin. crossterm parses `ESC ]` as Alt+`]` and then the rest of
  the reply as ordinary keys, so `ESC ] 11 ; rgb:2d2d/…` types `r`, `g`, `b`,
  `d`… into the app: the resize form opened by itself and `d` was one hex digit
  away from deleting a sandbox. `app::sanitize_console` strips CSI/OSC/DCS/nF
  escapes and control bytes at the poller edge; keep it that way, and never
  feed raw console text to a widget.
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
- Whiteout handling and layer unpack have unit tests in `crates/image` —
  extend those rather than testing via pulls. Later OCI layers may replace an
  existing path with a hard link; remove the destination before
  `tar::Entry::unpack_in` or the unpack fails with `EEXIST`.

## Runtime env vars

`MVM_HOST` (client → daemon addr), `MVM_DATA_DIR` (state root),
`MVM_AGENT_PATH` (guest agent binary), `MVM_STORAGE_DRIVER`
(`overlay`/`copy`/`ext4`), `MVM_USERNS=0` (disable userns mode),
`MVM_DISK_SLACK_MIB` (ext4 driver writable slack), `MVM_GVPROXY_BIN`
(gvproxy binary for managed `--net gvproxy`), `MVM_GVPROXY_CONTROL`
(control socket of a gvproxy *you* run, for `--net gvproxy:<socket>` port
maps).

Daemon → guest agent (set by the shim, scrubbed by the agent before it
spawns the workload): `MVM_ROOT_DISK`, `MVM_WORKDIR`, `MVM_MOUNTS`,
`MVM_NET_CONFIG`, `MVM_NET_TSI`, `MVM_CONSOLE_TTY`, `MVM_CONSOLE_SIZE`.

## Conventions

- Errors: `mvm_common::Error` (`thiserror`) inside crates; CLI-facing code
  maps to `String`.
- Keep the daemon's HTTP surface documented in README when routes change.
- Integration test must stay green and self-contained:
  `scripts/integration.sh` runs against an isolated `MVM_DATA_DIR` + port,
  skips gracefully (KVM, gvproxy), and cleans up after itself.
- Record non-obvious findings in this file's Sharp edges; track work in
  `TODO.md` (move finished items to its Done section with a one-liner on
  the mechanism).
