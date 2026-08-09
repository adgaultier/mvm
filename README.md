# mvm — microVM sandboxes on libkrun
`mvm` aims to run OCI container images as hardware-isolated microVMs, giving each sandbox its own Linux kernel while retaining a Docker-style workflow. Each VM is booted through [libkrun](https://github.com/containers/libkrun), with the image rootfs shared over virtiofs, and managed through a CLI, HTTP API, or ratatui dashboard

> **Status:** work in progress, partially written with coding agents.
> Contributions welcome. See [`TODO.md`](TODO.md) for the backlog and
> [`AGENTS.md`](AGENTS.md) for architecture notes.

**Contents** — [Quick start](#quick-start) · [Architecture](#architecture) ·
[Requirements](#requirements) · [Build](#build) · [CLI](#cli) ·
[Networking](#networking) · [Storage](#storage) · [Guest security](#guest-security) ·
[Security model and hardening](#security-model-and-hardening) ·
[HTTP API](#http-api) · [Environment](#environment)

## Quick start

```console
$ mvm serve &                       # the daemon owns all state
$ mvm pull alpine
$ mvm run alpine echo hello-from-microvm
hello-from-microvm

$ mvm run --name dev alpine sleep infinity &
$ mvm exec dev uname -a
Linux localhost 6.12.91 #1 SMP PREEMPT_DYNAMIC ... x86_64 Linux
$ printf 'stdin works\n' | mvm exec -i dev cat
stdin works
$ mvm exec -it dev sh               # interactive shell on a pty
$ mvm stop dev && mvm rm dev
```

Interactive sandboxes, one-shot and long-lived:

```console
$ mvm run --rm -it alpine sh              # one-shot; removed on exit
$ mvm create -it --name box alpine sh     # long-lived
$ mvm start -a box                        # ctrl-p ctrl-q detaches
$ mvm attach box                          # …and comes back to it
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
                 KVM / HVF
```

- **Daemon + clients.** `mvm serve` owns all state behind a local HTTP API
  (default `127.0.0.1:24642`, override with `MVM_HOST`). CLI and TUI are
  stateless clients.
- **One shim process per VM.** libkrun's `krun_start_enter()` takes over the
  calling process, so the daemon spawns a detached `mvm __vm-shim` per sandbox.
  Its stdout/stderr *is* the guest console, pumped to `console.log` and to live
  log followers. libkrun's own diagnostics go to a separate `krun.log`, so
  hypervisor noise never shows up as guest output.
- **Guest agent as the guest's init.** A static musl binary is injected at
  `/.mvm/agent` (libkrun's `/init.krun` is PID 1 and execs it). It spawns the
  workload — on its own pty with `-t` — reaps zombies, and serves `exec` (stdin,
  stdout/stderr streaming, exit codes) over **vsock**, so no guest networking is
  required.
- **Pure-Rust image pulls.** Registry auth, manifest resolution, blob
  verification, layer unpack and OCI whiteouts are implemented in-tree — no
  skopeo/podman needed.

## Requirements

| | |
|---|---|
| Host | **Linux x86_64** with KVM (`/dev/kvm` read-write for your user), or **macOS on Apple Silicon** (macOS 14+, Hypervisor.framework) |
| Hypervisor | **libkrun** + **libkrunfw** (`libkrun.so.1` / `libkrun.dylib`, plus headers to build) |
| Toolchain | Rust, plus the musl target matching your host arch (`x86_64-` or `aarch64-unknown-linux-musl`) for the guest agent |
| Optional | [gvproxy](https://github.com/containers/gvisor-tap-vsock) ≥ v0.8.9 for userspace NAT / port forwarding |

Guests are the **same architecture as the host** — on Apple Silicon pull arm64
images (`mvm pull` resolves the matching platform manifest automatically).

Rootless operation is fully supported: state lives in `~/.local/share/mvm`
(as root: `/var/lib/mvm`; override with `MVM_DATA_DIR`).

On macOS, `scripts/install-darwin.sh` installs everything (rustup, zig, libkrun
and gvproxy from the `libkrun/krun` Homebrew tap).

## Build

```console
$ scripts/build.sh        # release binaries + static agent → dist/
$ cargo test --workspace  # unit tests, no KVM needed
$ scripts/integration.sh  # end-to-end: boots real VMs (needs KVM/libkrun + network)
```

On macOS run `scripts/install-darwin.sh` once first; `build.sh` then
cross-compiles the agent with `cargo zigbuild` and codesigns `dist/mvm` with the
Hypervisor.framework entitlement — without it macOS refuses to start VMs.

`mvm` looks for `mvm-agent` next to its own binary, or at `MVM_AGENT_PATH`. The
agent **must** be the static musl build; it runs inside guests whose libc you
don't control, and the daemon refuses a dynamically linked one.

## CLI

### Commands

| Command | Description |
|---|---|
| `mvm serve [--addr HOST:PORT]` | run the daemon |
| `mvm pull IMAGE` | pull an OCI image (docker references) |
| `mvm images` / `mvm rmi IMAGE` | list / remove local images |
| `mvm create IMAGE [CMD…]` | create a sandbox without starting it |
| `mvm run IMAGE [CMD…]` | create + start + attach; the sandbox survives the workload unless `--rm` |
| `mvm ps [-a]` | list sandboxes (`-a` includes stopped ones) |
| `mvm start [-a] SANDBOX` | start a created/stopped sandbox (`-a` also attaches) |
| `mvm attach [--no-stdin] SANDBOX` | attach the terminal to a running sandbox's console; **ctrl-p ctrl-q** detaches and leaves the workload running |
| `mvm exec [-i] [-t] [-u USER] SANDBOX CMD…` | run a command in a live sandbox |
| `mvm logs [-f] [-n N] SANDBOX` | guest console output (`-n` = last N lines) |
| `mvm stop SANDBOX` / `mvm rm [-f] SANDBOX` | lifecycle (`rm -f` force-removes a running sandbox) |
| `mvm resize SANDBOX [--cpus N] [-m MiB] [--restart]` | change the VM's allocation |
| `mvm inspect SANDBOX` | full sandbox JSON |
| `mvm clone SANDBOX [--fork] [FLAG…]` | new sandbox from the source's spec |
| `mvm-tui` | live dashboard |

Any command taking a `SANDBOX` accepts its **id, a unique id prefix, or its
name**. Names are unique — creating a second sandbox with a taken name is
refused; without `--name` the daemon generates one.

### `run` / `create` options

| Flag | Meaning |
|---|---|
| `--name NAME` | sandbox name |
| `-e KEY=VAL` | environment variable |
| `-v host:guest[:ro]` | bind mount |
| `-p host:guest` | port mapping (needs a network profile that supports it) |
| `--net PROFILE` | `none` (default) \| `tsi` \| `gvproxy[:<socket>]` \| `tap:<dev>` |
| `--cpus N` / `-m MiB` | vCPU count (default 1) / memory (default 512) |
| `-w DIR` | working directory in the guest |
| `-u USER` | run the workload as this user (`name/uid[:group/gid]`), overriding the image's `USER`; resolved against the *guest's* `/etc/passwd` |
| `-i` / `-t` | keep the console's stdin open / give the workload its own guest pty |
| `--rm` | (`run` only) remove the sandbox when the workload exits |

`-i` and `-t` are **properties of the sandbox, fixed at create time** (as in
docker). `start` has no `-i`/`-t` of its own — it reuses what the sandbox was
created with, so a client can never contradict the running VM:

```console
$ mvm create -it --name dev alpine sh    # or: mvm run -it --name dev …
$ mvm start dev                          # detached
$ mvm attach dev                         # ctrl-p ctrl-q to leave it running
```

`mvm run` keeps the sandbox after the workload exits (docker's default), so
`mvm run alpine true` still leaves a sandbox you can `start` again. Pass `--rm`
to remove it on exit.

### `resize`

A microVM's size is fixed at boot, so `resize` rewrites the spec and the new
allocation applies on the **next start**. `--restart` reboots the sandbox
immediately.

### `clone`

`mvm clone SRC` creates a new sandbox with a copy of the source's spec; any
`run`/`create` flag overrides it (`-e`, `-v`, `-p` *replace* the source's lists
rather than appending). The source's name is never inherited.

`--fork` also copies the source's current disk (reflink'd), so the clone boots
with its files intact. For forking a *running* source, stop it first — the
snapshot is point-in-time, not crash-consistent.

### `mvm-tui`

Live dashboard over the same HTTP API. Console output is `mvm logs`, not a TUI pane.

| Key | Action |
|---|---|
| `tab` / `1` / `2` | switch between the Sandboxes and Images tabs |
| `j` / `k` (or arrows), `g` | move the selection / jump to top |
| `s` / `x` / `d` | start / stop / delete (with confirmation) |
| `i` | inspect pane (`j`/`k`, PgUp/PgDn to scroll, `esc` closes) |
| `r` | resize form — `tab` switches field, `+`/`-` adjust, `enter` applies, `^r` applies and restarts |
| `q` / `esc` | quit / close the open modal |

## Networking

Select a profile with `--net`, placed before the image.

### `none` (default)

Fully isolated — loopback only. mvm attaches a dead NIC to switch off libkrun's
default TSI backend, which would otherwise give every guest transparent host
networking.

### `tsi`

libkrun's Transparent Socket Impersonation: guest sockets are serviced by the
host directly. Outbound internet and DNS with **zero setup** — no proxy, no NIC,
no root — plus `-p` port maps. The guest shares the host's network identity; use
`gvproxy` when you want NAT separation.

### `gvproxy`

Rootless userspace NAT (the same stack podman machine uses). Outbound internet
plus `-p host:guest` forwards, with no setup beyond having the `gvproxy` binary
on `PATH` (`MVM_GVPROXY_BIN` overrides):

```console
$ mvm run --net gvproxy -p 8080:80 alpine sh -c 'apk add curl && ...'
```

The daemon starts a **private gvproxy per sandbox** and stops it with the
sandbox. That is not a luxury: a gvproxy vfkit datagram endpoint learns its peer
from the first packet and never re-learns, so a shared socket serves the first VM
and silently leaves every later one with no route at all (and all guests boot on
the same static address anyway).

The guest is configured automatically (192.168.127.2/24, gateway and DNS at .1).
Throughput is modest (userspace TCP/IP) — fine for package installs and API
calls.

`gvproxy:<socket>` attaches to a gvproxy you run yourself instead — one sandbox
per instance, listening in **vfkit** mode (libkrun speaks the vfkit datagram
protocol, *not* `-listen-qemu`), with `MVM_GVPROXY_CONTROL` pointing at its
`-listen` socket if you want port forwards registered through gvproxy's HTTP
control API:

```console
$ gvproxy -listen unix:///run/gvproxy/control.sock \
    -listen-vfkit unixgram:///run/gvproxy/gvproxy.sock &
$ export MVM_GVPROXY_CONTROL=/run/gvproxy/control.sock
$ mvm run --net gvproxy:/run/gvproxy/gvproxy.sock alpine ...
```

On Linux, gvproxy < v0.8.9 does not implement vfkit unixgram sockets and exits
with `unsupported 'unixgram' scheme`; use a newer build.

### `tap:<dev>`

Attach to an existing TAP device for near-native performance (Linux-only). You
own the plumbing, and the guest needs its own IP configuration — mvm does no
addressing here:

```console
$ sudo ip tuntap add dev mvmtap0 mode tap && sudo ip link set mvmtap0 up
$ mvm run --net tap:mvmtap0 alpine ...
```

## Storage

The guest root is always served over **virtiofs**. Drivers are auto-selected;
`MVM_STORAGE_DRIVER` overrides.

| Driver | Behavior |
|---|---|
| `overlay` | default under root and userns mode: kernel OverlayFS with the image rootfs as lower layer and a per-sandbox upper. Changes **persist** across `stop`/`start`. Auto-probed, falls back to `copy` if unsupported. |
| `copy` | per-sandbox rootfs copy; universal fallback and the macOS default (APFS copies are `clonefile(2)`, so they are cheap). The rootfs is **rebuilt (wiped) on every start**. |

### Rootless userns mode (Linux)

`mvm serve` re-execs itself inside a user namespace mapping uid 0 to your user
and uids 1..65535 to your `/etc/subuid` range (podman-style). libkrun's
in-process virtiofs server then runs as namespace-root, so guest `chown` and
ownership work with full fidelity: image files appear root-owned, and
`apk`/`apt`/`useradd` behave.

This requires `/etc/subuid` + `/etc/subgid` entries and `newuidmap`/`newgidmap`,
and degrades gracefully (with a warning) when they are missing. Opt out with
`MVM_USERNS=0`. On macOS there is no userns; the daemon runs with host
credentials.

## Guest security

* **Hardware isolation.** Every sandbox runs in a separate VM with its own
  kernel. The host shares only the virtiofs rootfs, explicitly requested
  mounts, and the vsock control channel. See [`TODO.SEC.md`](security/TODO.SEC.md)
  for the security model and hardening backlog.

* **Raw sockets are banned guest-wide, always.** The agent installs a
  seccomp-bpf filter before starting any workload; a failed installation is
  fatal. The filter denies `socket(2)` for `AF_PACKET` (any type) and
  `AF_INET`/`AF_INET6` with `SOCK_RAW`. It is inherited by the workload and
  every exec session and cannot be weakened; there is no opt-out.
  `AF_NETLINK` with a raw type remains allowed because it is required by
  network bootstrap. This restriction breaks `tcpdump`, `arping`, old
  `ping`, and AF_PACKET-based DHCP clients such as `udhcpc`.

* **Guest-local user resolution.** `-u` and the image's `USER` are resolved
  against the guest rootfs's `/etc/passwd`, never the host's user database.

## Security model and hardening

`mvm` treats the guest as potentially hostile: guest root, arbitrary native
code execution, and even guest-kernel compromise are within the threat model.
The security boundary is the VM and its host-side interfaces, not the guest
process sandbox alone.

The security work therefore focuses on preventing a compromised guest from
crossing that boundary, accessing other sandboxes, abusing host capabilities,
exfiltrating credentials, or exhausting host resources. See:

* [`TODO.SEC.md`](security/TODO.SEC.md) — security invariants, hardening,
  resource governance, and security status.
* [`TODO.CREDENTIALS.md`](security/TODO.CREDENTIALS.md) — credential isolation
  and the planned credential proxy.
* [`TODO.ADVERSARIAL.md`](security/TODO.ADVERSARIAL.md) — adversarial testing,
  escape detection, cross-VM isolation, and host-side canaries.

### Current control-plane limitation

The control plane is currently unauthenticated and is intended to remain
reachable only through loopback. It is **not a remote or multi-tenant security
boundary** in its current form. Hardening and explicit security invariants for
the control plane are tracked in [`TODO.SEC.md`](security/TODO.SEC.md).


## HTTP API

```
GET    /health
GET    /api/v1/info                                    {"version","storage_driver"}

GET    /api/v1/sandboxes                 POST   /api/v1/sandboxes
GET    /api/v1/sandboxes/{id}            DELETE /api/v1/sandboxes/{id}
POST   /api/v1/sandboxes/{id}/start      POST   /api/v1/sandboxes/{id}/stop
POST   /api/v1/sandboxes/{id}/resize                   {"vcpus":N,"ram_mib":N}
POST   /api/v1/sandboxes/{id}/clone                    {"spec":{…},"fork":bool}
GET    /api/v1/sandboxes/{id}/logs?follow=bool&tail=N&raw=bool   (console)
POST   /api/v1/sandboxes/{id}/stdin[?eof=true]         (console stdin)
POST   /api/v1/sandboxes/{id}/console/resize           {"cols":N,"rows":N}
POST   /api/v1/sandboxes/{id}/exec                     (framed event stream)
POST   /api/v1/sandboxes/{id}/exec/{session}/stdin[?eof=true]
POST   /api/v1/sandboxes/{id}/exec/{session}/resize    {"cols":N,"rows":N}

GET    /api/v1/images                    DELETE /api/v1/images/{name}
POST   /api/v1/images/pull                             (JSON-lines progress)
```

Exec and log streams use length-prefixed JSON frames (`u32` BE length + JSON),
defined in `crates/common/src/protocol.rs`.

The console stream drops terminal *query* sequences (DSR `ESC[6n`, Device
Attributes `ESC[c`): replaying a question makes the reader's terminal answer into
its own input buffer. `raw=true` keeps them and is only for a client that owns a
terminal and answers them — i.e. `mvm attach` / `mvm run -it`.

## Environment

| Variable | Effect |
|---|---|
| `MVM_HOST` | daemon address for clients (default `http://127.0.0.1:24642`) |
| `MVM_DATA_DIR` | state root (default `~/.local/share/mvm`, `/var/lib/mvm` as root) |
| `MVM_AGENT_PATH` | guest agent binary |
| `MVM_STORAGE_DRIVER` | force `overlay` or `copy` |
| `MVM_USERNS=0` | disable rootless userns mode (Linux) |
| `MVM_GVPROXY_BIN` | gvproxy binary for managed `--net gvproxy` |
| `MVM_GVPROXY_CONTROL` | control socket of a gvproxy *you* run, for `--net gvproxy:<socket>` port maps |

State layout under the data dir:

```
<data>/
├── sandboxes.json          # registry
├── images/<store-key>/     # meta.json, rootfs/
└── sandboxes/<id>/
    ├── shim.json           # VM config written by the daemon
    ├── console.log         # guest console
    ├── krun.log            # libkrun's own diagnostics
    ├── rootfs|upper|work/  # per-driver filesystem state
    └── agent.sock          # agent control channel
```

