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
- **Guest agent as PID 1** (`/.mvm/agent`, injected at start): spawns the
  workload, reaps zombies, serves exec (pipes or pty) over **vsock port
  1024** → `<sandbox>/agent.sock` host-side. Wire protocol: u32-BE length
  + JSON frames, byte payloads base64-encoded (`common::protocol`).
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
  vfkit-mode datagram socket + agent-side static IP bootstrap,
  `tap:<dev>` = pre-provisioned TAP.

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
| `tui` | ratatui dashboard |

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
- The agent runs as **PID 1 in the guest**: it must reap zombies and only
  use std + libc (keep dependencies out of `crates/agent`). Pty sessions
  share one fd for stdin/stdout — mind the close-once logic.
- **`unshare(CLONE_NEWUSER)` requires a single-threaded process** — userns
  entry must happen before the tokio runtime exists.
- Whiteout handling and layer unpack have unit tests in `crates/image` —
  extend those rather than testing via pulls.

## Runtime env vars

`MVM_HOST` (client → daemon addr), `MVM_DATA_DIR` (state root),
`MVM_AGENT_PATH` (guest agent binary), `MVM_STORAGE_DRIVER`
(`overlay`/`copy`/`ext4`), `MVM_USERNS=0` (disable userns mode),
`MVM_DISK_SLACK_MIB` (ext4 driver writable slack).

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
