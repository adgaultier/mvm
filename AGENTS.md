# AGENTS.md — working on mvm

## What this is

A docker-style microVM sandbox platform: OCI images run as KVM microVMs via
libkrun. Workspace of 11 crates under `crates/`. Read `implementation.md`
for the full architecture rationale; `README.md` for user-facing behavior.

## Build & test

```sh
cargo build --workspace          # host binaries (mvm, mvm-tui, gnu mvm-agent)
cargo test --workspace           # unit tests, no KVM needed
cargo build -p mvm-agent --target x86_64-unknown-linux-musl --release
                                 # the REAL guest agent (static). The gnu-linked
                                 # target/debug/mvm-agent will NOT run in guests.
scripts/build.sh                 # all of the above, release, into dist/
scripts/integration.sh           # boots real VMs; needs /dev/kvm, libkrun, network
```

The guest agent is musl-only because `-C target-feature=+crt-static` on the
gnu target breaks proc-macro crates (`serde_derive`).

## Crate map

| Crate | Notes |
|---|---|
| `common` | shared types, `DataDir` layout, vsock frame protocol — no heavy deps |
| `krun-sys` | hand-written libkrun FFI; keep in sync with `/usr/include/libkrun.h` |
| `runtime` | `KrunContext` RAII wrapper, shim entry (`run_shim`), shim spawner |
| `image` | registry client (blocking reqwest), layer unpack, whiteouts, `ImageStore` |
| `storage` | `copy` (rootless) / `overlay` (root) drivers |
| `network` | profile validation, port-map parsing |
| `manager` | sandbox registry + lifecycle; owns agent vsock channels |
| `agent` | guest PID 1; std+libc only, poll(2) event loop, must stay static-friendly |
| `api` | axum routes; streams = `Body::from_stream` |
| `cli` | `mvm` binary incl. hidden `__vm-shim` subcommand |
| `tui` | ratatui dashboard |

## Sharp edges (learned the hard way)

- **`krun_start_enter` never returns** — it exits the process with the
  workload's code. VM boot must happen in the re-executed shim process
  (`mvm __vm-shim`), never in the daemon.
- **`reqwest::blocking` clients must be constructed OUTSIDE tokio.**
  `Manager::new` builds one (registry client); `serve()` in `cli/src/main.rs`
  deliberately creates the Manager before building the runtime. Moving
  manager construction into async code panics at startup.
- **State `running` ≠ agent ready.** The agent's vsock connection lands
  shortly after boot; `exec` against a just-started sandbox can fail with
  "no agent connection". Tests should poll `mvm exec <sb> true` (see
  `scripts/integration.sh`).
- The agent runs as **PID 1 in the guest**: it must reap zombies and should
  only use std + libc (keep dependencies out of `crates/agent`).
- Whiteout handling and layer unpack have unit tests in `crates/image` —
  extend those rather than testing via pulls.

## Runtime env vars

`MVM_HOST` (client → daemon addr), `MVM_DATA_DIR` (state root),
`MVM_AGENT_PATH` (guest agent binary), `MVM_STORAGE_DRIVER` (`copy`/`overlay`).

## Conventions

- Errors: `mvm_common::Error` (`thiserror`) inside crates; CLI-facing code
  maps to `String`.
- Keep the daemon's HTTP surface documented in README when routes change.
- Integration test must stay green: `scripts/integration.sh` runs against
  an isolated `MVM_DATA_DIR` + port and cleans up after itself.
