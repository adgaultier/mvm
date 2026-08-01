# mvm — microVM sandboxes on libkrun

`mvm` runs OCI container images as hardware-isolated **microVMs** with a
docker-style CLI, an HTTP API, and a ratatui dashboard. Each sandbox is a
real KVM virtual machine (via [libkrun](https://github.com/containers/libkrun))
booting its own kernel, with the image rootfs shared over virtiofs.

```console
$ mvm serve &
$ mvm pull alpine
$ mvm run alpine echo hello-from-microvm
hello-from-microvm
$ mvm run --name dev --keep alpine sleep infinity &
$ mvm exec dev uname -a
Linux localhost 6.12.91 #1 SMP PREEMPT_DYNAMIC ... x86_64 Linux
$ printf 'stdin works\n' | mvm exec -i dev cat
$ mvm exec -it dev sh          # interactive shell on a pty (vi/top work)
stdin works
$ mvm stop dev && mvm rm dev
```

## Architecture

```
      mvm CLI / mvm-tui (thin HTTP clients)
                    │
        HTTP API (mvm serve — axum)
                    │
             Sandbox Manager
                    │
    ┌───────────────┼───────────────┐
 OCI Image      Network         Storage
  Manager       Manager         Manager
    └───────────────┼───────────────┘
                    │
      libkrun shim (one process per VM)
                    │
                   KVM
```

- **Daemon + client.** `mvm serve` owns all state behind a local HTTP API
  (default `127.0.0.1:24642`, override with `MVM_HOST`). CLI and TUI are
  stateless clients.
- **One shim process per VM.** libkrun's `krun_start_enter()` takes over the
  calling process, so the daemon spawns a detached `mvm __vm-shim` per
  sandbox; its stdout/stderr is the guest console, pumped to
  `console.log` and live log followers.
- **Guest agent as PID 1.** A static musl binary is injected at
  `/.mvm/agent`; it spawns the workload, reaps zombies, and serves
  `exec` (with stdin/stdout/stderr streaming and exit codes) over
  **vsock** — no networking required in the guest.
- **Pure-Rust image pulls.** Registry auth, manifest resolution, blob
  verification, layer unpack and OCI whiteouts are implemented in-tree; no
  skopeo/podman needed.

## Requirements

- Linux x86_64 with **KVM** (`/dev/kvm` read-write for your user)
- **libkrun** and **libkrunfw** installed (`libkrun.so.1`, headers for building)
- Rust toolchain + `x86_64-unknown-linux-musl` target (for the guest agent)
- Optional: [gvproxy](https://github.com/containers/gvisor-tap-vsock) for
  outbound networking / port forwarding

Rootless operation is fully supported (state lives in
`~/.local/share/mvm`; as root: `/var/lib/mvm`; override: `MVM_DATA_DIR`).

## Build

```console
$ scripts/build.sh        # release binaries + static agent → dist/
$ scripts/integration.sh  # end-to-end test: boots real VMs (needs KVM + network)
```

`mvm` looks for `mvm-agent` next to its own binary, or at `MVM_AGENT_PATH`.
The agent **must** be the static musl build — it runs inside guests whose
libc you don't control.

## CLI

| Command | Description |
|---|---|
| `mvm serve [--addr HOST:PORT]` | run the daemon |
| `mvm pull IMAGE` | pull an OCI image (docker references) |
| `mvm images` / `mvm rmi IMAGE` | list / remove local images |
| `mvm run IMAGE [CMD…]` | create + start + attach; ephemeral unless `--keep` |
| `mvm create IMAGE [CMD…]` | create without starting |
| `mvm ps [-a]` | list sandboxes |
| `mvm start/stop/rm SANDBOX` | lifecycle (`rm -f` force-removes running) |
| `mvm exec [-i] [-t] SANDBOX CMD…` | run a command in a live sandbox (`-i` forwards stdin, `-t` allocates a pty; `-it` = interactive shell) |
| `mvm logs [-f] SANDBOX` | guest console output |
| `mvm inspect SANDBOX` | full sandbox JSON |
| `mvm-tui` | live dashboard (sandboxes, images, console) |

`run`/`create` options: `--name`, `-e KEY=VAL`, `-v host:guest[:ro]`,
`-p host:guest`, `--net none|gvproxy|tap:<dev>`, `--cpus N`, `-m MiB`,
`-w workdir`, `--keep`.

## HTTP API

```
GET    /health
GET    /api/v1/sandboxes                 POST   /api/v1/sandboxes
GET    /api/v1/sandboxes/{id}            DELETE /api/v1/sandboxes/{id}
POST   /api/v1/sandboxes/{id}/start      POST   /api/v1/sandboxes/{id}/stop
GET    /api/v1/sandboxes/{id}/logs?follow=bool        (raw console stream)
POST   /api/v1/sandboxes/{id}/exec                    (framed event stream)
POST   /api/v1/sandboxes/{id}/exec/{session}/stdin[?eof=true]
POST   /api/v1/sandboxes/{id}/exec/{session}/resize   {"cols":N,"rows":N}
GET    /api/v1/images                    DELETE /api/v1/images/{name}
POST   /api/v1/images/pull                            (JSON-lines progress)
```

Exec/log streams use length-prefixed JSON frames (`u32` BE length + JSON),
defined in `crates/common/src/protocol.rs`.

## Storage & networking

- **Rootless userns mode** (automatic): `mvm serve` re-execs itself inside a
  user namespace mapping uid 0 to your user and uids 1..65535 to your
  `/etc/subuid` range (podman-style). libkrun's in-process **virtiofs**
  server then runs as namespace-root, so guest `chown`/ownership works with
  full fidelity — image files appear root-owned, `apk`/`apt`/`useradd` work.
  Requires `/etc/subuid` + `/etc/subgid` entries and `newuidmap`/`newgidmap`;
  degrades gracefully (with a warning) when missing. Opt out: `MVM_USERNS=0`.
- **Storage drivers** (auto-selected, override `MVM_STORAGE_DRIVER`) — the
  guest root is always served over **virtiofs**:
  - `overlay` (default for root and userns mode): kernel OverlayFS, image
    rootfs as lower layer, per-sandbox upper. Changes persist across
    `stop`/`start`. Auto-probed; falls back to `copy` if unsupported.
  - `copy`: per-sandbox rootfs copy; universal fallback. Rootfs is rebuilt
    (wiped) on every start.
  - `ext4` (opt-in): per-sandbox ext4 image built with `mkfs.ext4 -d`,
    booted as a virtio-blk root instead of virtiofs.
- **Network profiles:** `none` (default — no NIC, strongest isolation),
  `gvproxy` (userspace NAT + `-p` port maps; needs a running gvproxy),
  `tap:<dev>` (pre-configured TAP device).

## Known limitations

- Without userns mode (no subuid ranges / newuidmap), guest `chown` over
  virtiofs is limited to host credentials.
- Guest chowns land on subuids on the host; clean up sandbox state through
  `mvm rm` (the daemon), not by deleting the data dir by hand.
- x86_64 Linux only (matches the vendored libkrun FFI subset).
