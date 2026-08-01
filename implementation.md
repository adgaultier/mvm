# mvm — Implementation Summary

A docker-sandbox-style microVM sandbox platform in Rust, built on **ratatui**
(TUI) + **libkrun** (KVM microVM runtime). Reproduces the architecture from the
design meeting:

```
            HTTP/gRPC API (mvm serve — axum)
                      │
               Sandbox Manager (mvm-manager)
                      │
        ┌──────────────┼──────────────┐
   OCI Image      Network        Storage
    Manager        Manager        Manager
        │              │              │
        └──────────────┼──────────────┘
                      │
               libkrun Runtime (shim)
                      │
                    KVM
                      │
                One MicroVM per sandbox
```

## Environment (verified during this session)

- Rust 1.96.0, host `x86_64`, Linux
- `/dev/kvm` accessible by the user (`crw-rw-rw-`)
- System libkrun installed: `/usr/lib/libkrun.so.1` (+ `libkrunfw`) and
  headers `/usr/include/libkrun.h` (libkrun 1.x API, includes `krun_add_vsock_port2`,
  `krun_add_virtiofs3`, `krun_set_gvproxy_path`, `krun_add_net_tap`, …)
- Internet reachable (docker registry auth endpoint returns 401 → pullable)
- Rootless user (uid 1000); no fuse-overlayfs; ext2/ext3 fs (no reflink)
- musl target added via `rustup target add x86_64-unknown-linux-musl` for the
  static guest agent

## Key architecture decisions

1. **Daemon + client model.** `mvm serve` runs the manager behind an HTTP API
   on `127.0.0.1:24642` (override: `MVM_HOST`). The CLI (`mvm`) and TUI
   (`mvm-tui`) are thin HTTP clients. Matches docker's client/daemon split.
2. **libkrun runs the VM in-process** — `krun_start_enter()` never returns
   (it `exit()`s with the guest workload's code). So the VM is booted by a
   re-executed shim: `mvm __vm-shim <shim.json>`, spawned by the daemon as a
   detached session leader (`setsid`), with stdin from `/dev/null` and its
   stdout/stderr piped back to the daemon = the **guest console stream**.
3. **Guest agent over vsock for `exec`.** A statically-linked `mvm-agent`
   binary is injected at `/.mvm/agent` in each sandbox rootfs and booted as
   PID 1 (libkrun's implicit init mounts `/proc /sys /dev` first). It:
   - spawns the user workload as its child (stdio → `/dev/console`),
   - connects to the host over **vsock port 1024** (host side:
     `krun_add_vsock_port2(ctx, 1024, <sb>/agent.sock, listen=false)` +
     tokio `UnixListener`),
   - serves `exec` requests with a single-threaded `poll()` event loop,
   - reaps zombies (PID 1 duty), forwards signals, mounts extra virtiofs
     shares (`MVM_MOUNTS` env), and reports the workload exit code.
4. **Wire protocol** (`common::protocol`): length-prefixed JSON frames
   (`u32 BE len + JSON`), `AgentRequest` (Exec/Stdin/StdinEof/Kill/Ping) and
   `AgentEvent` (Ready/Stdout/Stderr/Exit/WorkloadExit/Pong/Error). The same
   framing rides over the HTTP body for exec/log streaming.
5. **Pure-Rust OCI puller** — no skopeo/podman dependency. Implements docker
   reference normalization, Bearer auth-challenge token flow, index/manifest
   resolution for the host platform, blob download with sha256 verification,
   gzip/zstd/tar layer decompression, and OCI whiteouts (`.wh.*` + opaque dirs).
   Device nodes are skipped when rootless (guest gets devtmpfs anyway).
6. **Rootless userns mode** (virtiofs-first chown fix). `mvm serve` re-execs
   itself under `unshare(CLONE_NEWUSER)` before tokio starts (must be
   single-threaded), the parent installs uid/gid maps via
   `newuidmap`/`newgidmap` (0 → user, 1.. → /etc/subuid range) and stays as
   a thin supervisor; the child gets a private mount namespace. Everything
   downstream — unpack, storage, libkrun's in-process virtiofs server — runs
   as namespace-root, so guest chown works through virtiofs (chowns land on
   subuids). Unpack applies real tar-header ownership when euid==0; the
   `copy` driver preserves it; device nodes are only unpacked as *init-ns*
   root (namespace-root can't mknod). Degrades with a warning when
   subuid/newuidmap are missing; `MVM_USERNS=0` opts out. See
   `crates/cli/src/userns.rs`.
7. **Storage drivers** (guest root always over virtiofs):
   - `overlay` (default for root *and* userns mode, auto-probed): OverlayFS,
     image = lower, per-sandbox upper/work/merged. Upper persists across
     stop/start. Unprivileged overlayfs in a userns needs kernel ≥ 5.11.
   - `copy`: full recursive copy per sandbox (`cp --reflink=auto`,
     `--preserve` incl. ownership as ns-root). Fallback; wiped each start.
   - `ext4` (opt-in via `MVM_STORAGE_DRIVER=ext4`): per-sandbox ext4 image
     (`mkfs.ext4 -d`) attached via `krun_add_disk`; guest boots a bootstrap
     virtiofs dir, agent restores ownership from the recorded manifest and
     `pivot_root`s onto `/dev/vda`. Kept as an alternative for hosts without
     subuid ranges.
8. **Networking profiles**: `none` (default, no virtio-net device = isolated),
   `gvproxy` (userspace NAT via `krun_set_gvproxy_path` + `krun_set_port_map`),
   `tap:<dev>` (existing TAP via `krun_add_net_tap`). Validated by the network
   crate before use.

## Workspace layout (11 crates)

| Crate | Role |
|---|---|
| `crates/common` | Shared types: `Error`, `SandboxId`, `DataDir`, `SandboxSpec`/`Sandbox`/`SandboxState`, `ImageConfig`/`ImageInfo`, `NetworkMode`/`Mount`, exec/pull API types, vsock frame protocol |
| `crates/krun-sys` | Hand-written raw FFI to the libkrun subset mvm uses |
| `crates/runtime` | Safe `KrunContext` RAII wrapper; `ShimConfig`; `run_shim()` VM entry; `spawn_shim()` supervisor (detached process, piped console) |
| `crates/image` | `ImageReference` parsing, `RegistryClient` (auth, index, blobs, digest verify), layer `unpack` (whiteouts, gz/zstd), `ImageStore` (unpacked rootfs + `meta.json`) |
| `crates/storage` | `StorageDriver` trait, `copy` (rootless) and `overlay` (root) drivers |
| `crates/network` | Profile validation + `-p` port-map parsing |
| `crates/manager` | Sandbox registry + lifecycle (create/start/exec/stop/rm), persistence (`sandboxes.json`), console log pump → file + broadcast, child watcher, agent channel accept/dispatch |
| `crates/agent` | Static in-guest PID 1 agent (std + libc only), vsock client, poll loop, sessions |
| `crates/api` | axum REST server: sandboxes, logs (streaming, follow), exec (streaming), images, pull (streaming progress) |
| `crates/cli` | `mvm` binary: `serve pull images rmi create run ps start stop rm inspect logs exec` + hidden `__vm-shim` |
| `crates/tui` | `mvm-tui` ratatui dashboard (sandboxes/images tables, console pane, s/x/d/j/k/tab/q) |

## Data layout (rootless: `~/.local/share/mvm`, root: `/var/lib/mvm`)

```
<data>/
├── sandboxes.json          # registry (atomic tmp+rename persistence)
├── images/<store-key>/     # per-image
│   ├── meta.json           # reference, digest, size, ImageConfig
│   └── rootfs/             # unpacked layer content
└── sandboxes/<id>/
    ├── shim.json           # ShimConfig written by the daemon
    ├── console.log         # guest console (appended by log pump)
    ├── rootfs/             # per-sandbox writable root (copy driver)
    ├── disk.img            # ext4 root image (ext4 driver, sparse)
    ├── bootstrap/          # virtiofs boot dir (ext4 driver: agent+manifest)
    └── agent.sock          # unix socket for the agent control channel
```

## API surface

- `GET  /health`
- `GET/POST /api/v1/sandboxes`, `GET/DELETE /api/v1/sandboxes/{id}`
- `POST /api/v1/sandboxes/{id}/start|stop`
- `GET  /api/v1/sandboxes/{id}/logs?follow=bool`  → raw console bytes stream
- `POST /api/v1/sandboxes/{id}/exec`               → framed AgentEvent stream
- `GET /api/v1/images`, `DELETE /api/v1/images/{*name}`
- `POST /api/v1/images/pull`                        → JSON-lines progress stream

## Build status

- `cargo build --workspace` and `cargo test --workspace` are clean; all 17 unit
  tests pass (reference parsing, protocol framing, whiteouts, unpack, port
  maps, storage copy).
- The guest agent is built separately for full static linking:
  `rustup target add x86_64-unknown-linux-musl` (required because
  `-C target-feature=+crt-static` on the gnu target breaks proc-macro crates
  like `serde_derive`).

## End-to-end status (verified 2026-08-01)

- `scripts/build.sh` produces `dist/` (release `mvm`, `mvm-tui`, static musl
  `mvm-agent`); `scripts/integration.sh` boots real VMs against an isolated
  `MVM_DATA_DIR` + port — **12/12 checks pass** with both debug and release
  binaries: health, pull, run stdout, run exit-code propagation, exec
  stdout/exit-code, `exec -i` stdin round-trip, volume mounts, stop/rm
  lifecycle, console logs.
- Exec stdin is fully wired end to end: agent `Stdin`/`StdinEof` frames,
  manager routing, `POST …/exec/{session}/stdin` API, and `mvm exec -i`
  (pumps local stdin; non-interactive exec closes guest stdin immediately so
  readers don't hang).
- Startup fix: `Manager::new` builds a `reqwest::blocking` registry client,
  which panics if constructed inside tokio — `serve()` now creates the
  manager before building the runtime (see AGENTS.md "sharp edges").
- README.md and AGENTS.md written.

## Known limitations

- Userns mode + overlay-in-userns not yet validated on real KVM — re-run
  `scripts/integration.sh` (checks: guest chown, root-owned files, restart
  persistence). Expect `mvm: userns mode active` in the daemon log and
  `storage driver: overlay`.
- Without userns (no subuid/newuidmap — e.g. inside the CC sandbox, where
  newuidmap fails with "Could not set caps"), guest chown over virtiofs is
  limited to host credentials.
- Guest chowns land on host subuids: remove sandbox state via `mvm rm`, not
  manual `rm -rf` of the data dir (subuid-owned files resist deletion).
- No pseudo-TTY for exec (`-t`); `-i` streams raw stdin without a terminal.
- Exec streams are UTF-8-lossy (binary stdin/stdout corrupted) — TODO.md #2.
- Sandbox state `running` precedes agent vsock connect by a moment; exec in
  that window fails with "no agent connection" (tests poll `exec <sb> true`).
- gvproxy/tap networking implemented but not e2e-tested here (no gvproxy
  binary on this host); `none` profile verified.
