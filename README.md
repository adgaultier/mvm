# mvm -  microVM sandbox runtime
## Contents 
[Presentation](#presentation) · [Quick start](#quick-start) · [Architecture](#architecture) ·
 [Installation](#installation) · [CLI](#cli) · [Networking](#networking) · [Storage](#storage) · [Security model](#security-model-and-hardening) · [HTTP API](#http-api) · [Agents API](#agent-api) · [Environment](#environment) · [Status](#status)

## Presentation
### What is mvm?
`mvm` runs OCI container images as isolated microVMs, each with its
own Linux kernel, booted through [libkrun](https://github.com/containers/libkrun).
It provides a familiar container workflow -  pull, run, exec, logs, stop, rm -  while using a VM as the isolation boundary.

Each sandbox is a separate VM, treated as a hostile guest. The host exposes only explicitly controlled interfaces: virtiofs storage, requested mounts, vsock, and the selected network backend.

A small static musl guestd acts as init and provides process control over vsock, so basic sandbox control does not depend on guest networking

mvm is designed for workloads that may be untrusted or generated dynamically:
from AI agents and the code they run to developer tools, automation, builds,
and experiments.




### What you get
- **OCI images** -  pull and run images directly from registries.
- **VM isolation** -  each sandbox has its own Linux kernel and VM boundary.
- **Container-style lifecycle** -  create, run, start, attach, exec, logs, stop, rm, clone.
- **Explicit networking** -  networking is disabled by default; TSI, gvproxy,
  or TAP can be enabled when needed.
- **Controlled interfaces** -   host/guest access is explicit: storage, mounts,
  process control, and networking are configured per sandbox.
- **Persistent or disposable storage** -  OverlayFS or copy-based rootfs,depending on platform and configuration.
- **Rootless Linux** -  run the daemon without host root privileges.
- **Interactive workloads** -  PTYs, stdin, attach, and exec are supported.
- **HTTP API + TUI** -  manage sandboxes through a local daemon, with the CLI
and TUI acting as thin clients.


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
           mvm CLI / mvm-tui
          (thin HTTP clients)
                    │
              HTTP API
                    │
             Sandbox Manager
                    │
    ┌───────────────┼───────────────┐
 OCI Image      Network         Storage
  Manager       Manager         Manager
    └───────────────┼───────────────┘
                    │
             VM Shim / libkrun
                    │
              ┌─────▼─────┐
              │  microVM  │
              │           │
              │  kernel   │
              │  guestd  │
              │ workload  │
              └───────────┘
                    │
                 KVM / HVF
```
- **Daemon + clients**. `mvm serve` owns all sandbox and image state behind a
  local HTTP API (default `127.0.0.1:24642`). The CLI and TUI are thin,
  stateless clients

- **Sandbox lifecycle**. The Sandbox Manager coordinates VM lifecycle,
networking, storage, and image-backed root filesystems. Each sandbox gets
its own VM shim and microVM

- **Guestd**.  A small static musl binary acts as the guest's init,
spawning the workload and providing process execution, stdin/stdout,
PTY, and lifecycle control over vsock. Guest networking is not required
for sandbox control.

- **OCI images**. Image pulls are implemented in Rust, including registry
  authentication, manifest resolution, blob verification, layer unpacking,
  and OCI whiteouts; no skopeo or podman is required.


## Installation
### Requirements
| | |
|---|---|
| Host | **Linux x86_64** with KVM (`/dev/kvm` read-write for your user), or **macOS on Apple Silicon** (macOS 14+, Hypervisor.framework) |
| Hypervisor | **libkrun** + **libkrunfw** (`libkrun.so.1` / `libkrun.dylib`, plus headers to build) |
| Toolchain | Rust, plus the musl target matching your host arch (`x86_64-` or `aarch64-unknown-linux-musl`) for the guestd |
| Optional | [gvproxy](https://github.com/containers/gvisor-tap-vsock) ≥ v0.8.9 for userspace NAT / port forwarding |



- On linux run `scripts/instal-linux.sh` to install likbrun dependencies
- On macOS run `scripts/install-darwin.sh` to install libkrun/libkrunfw from the libkrun/krun Homebrew tap plus Zig and cargo-zigbuild for the static guestd
- gvproxy is optional and can be installed or selected separately with
`MVM_GVPROXY_BIN`.

### Build

```console
$ scripts/build.sh        # release binaries + static guestd → dist/
$ cargo test --workspace  # unit tests, no KVM needed
$ just -f scripts/integration/Justfile all  # boots real VMs (needs KVM/libkrun + network)
```

`mvm` looks for `mvm-guestd` next to its own binary, or at `MVM_GUESTD_PATH`. The
guestd **must** be the static musl build; it runs inside guests whose libc you
don't control, and the daemon refuses a dynamically linked one.

## CLI

### Commands

| Command | Description |
|---|---|
| `mvm serve [--port PORT]` | run the daemon (always binds `127.0.0.1`) |
| `mvm pull IMAGE` | pull an OCI image (docker references) |
| `mvm load --name IMAGE FILE` | load an OCI image layout archive (`.tar`) into the local store |
| `mvm images` / `mvm rmi IMAGE` | list / remove local images |
| `mvm create IMAGE [CMD…]` | create a sandbox without starting it |
| `mvm run IMAGE [CMD…]` | create + start + attach; the sandbox survives the workload unless `--rm` |
| `mvm ps [-a]` | list sandboxes (`-a` includes stopped ones) |
| `mvm start [-a] SDBX` | start a created/stopped sandbox (`-a` also attaches) |
| `mvm attach [--no-stdin] SDBX` | attach the terminal to a running sandbox's console; **ctrl-p ctrl-q** detaches and leaves the workload running |
| `mvm exec [-i] [-t] [-u USER] SDBX CMD…` | run a command in a live sandbox |
| `mvm logs [-f] [-n N] SDBX` | guest console output (`-n` = last N lines) |
| `mvm stop SDBX` / `mvm rm [-f] SDBX` | lifecycle (`rm -f` force-removes a running sandbox) |
| `mvm resize SDBX [--cpus N] [-m MiB] [--restart]` | change the VM's allocation |
| `mvm inspect SDBX` | full sandbox JSON |
| `mvm clone SDBX [--fork] [FLAG…]` | new sandbox from the source's spec |
| `mvm-tui` | live dashboard |
| `mvm-flow SDBX` | live agent lineage graph (a sandbox and its delegated children) |


Any command taking a `SDBX` accepts its **id, a unique id prefix, or its
name**. Names are unique -  creating a second sandbox with a taken name is
refused; without `--name` the daemon generates one.

### `pull` / `load`
Guests are the **same architecture as the host** -  on Apple Silicon pull arm64
images .
`mvm load` ingests an **OCI image layout** archive -  the `.tar` produced by
`podman save --format oci-archive`, `buildah push oci:`, or `skopeo copy
oci:`.

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
| `--security PROFILE` | `default` \| `strict` -  strict installs an extra guest-side seccomp filter in the workload's spawn path for hostile workloads; see [SEC.TODO.md](doc/security/SEC.TODO.md) |
| `--rm` | (`run` only) remove the sandbox when the workload exits |



`-i` and `-t` are **properties of the sandbox, fixed at create time** (as in
docker). `start` has no `-i`/`-t` of its own -  it reuses what the sandbox was
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

A microVM's cpu & ram specs are fixed at boot, so `resize` rewrites the spec and the new
allocation applies on the **next start**. `--restart` reboots the sandbox
immediately.

### `clone`
`mvm clone SRC` creates a new sandbox with a copy of the source's spec; any
`run`/`create` flag overrides it (`-e`, `-v`, `-p` *replace* the source's lists
rather than appending). The source's name is never inherited.
`--fork` also copies the source's current disk (reflink'd), so the clone boots
with its files intact. For forking a *running* source, stop it first -  the
snapshot is point-in-time, not crash-consistent.


### `mvm-tui`

Live dashboard with sandbox and image tabs. Sandboxes can be started, stopped,
resized (`r`), deleted (`d` with confirmation), and inspected (`i`, including
lifecycle timing flamegraphs); images are listed only.

### `mvm-flow`

Live lineage graph of one agent and everything it delegated to:

```console
$ mvm-flow [--host URL] <sandbox-id|name>
```

Each node shows the agent's derived status (`ready` / `running` / `booting` /
`stopped` / `failed`), its cpu/ram allocation and, when the delegation carried
a timeout, a TTL countdown. Edges follow the parent link and are labelled with
the last notification the child received. The graph is polled from
`GET /api/v1/agents` and reconciled live; pan/zoom with mouse or `hjkl`,
`Tab`/arrows select a node (details in the bottom bar), `f` fits the view,
`q` quits.

## Networking

Select a profile with `--net`, placed before the image.

Networking is opt-in. Sandboxes have no external network access by default.

### `none` (default)

No external network access; the guest has loopback only. mvm attaches a dead NIC to switch off libkrun's
default TSI backend, which would otherwise give every guest transparent host
networking.

### `tsi`

libkrun's Transparent Socket Impersonation: guest sockets are serviced by the
host directly. Outbound internet and DNS with **zero setup** -  no proxy, no NIC,
no root -  plus `-p` port maps. The guest shares the host's network identity; use
`gvproxy` when you want NAT separation. Use it at your own risk.

### `gvproxy`

Rootless userspace NAT with outbound internet access and optional
`-p host:guest` port forwards. The daemon starts and manages one private
gvproxy instance per sandbox. Install gvproxy separately and put it on `PATH`
or set `MVM_GVPROXY_BIN`:

```console
$ mvm run --net gvproxy -p 8080:80 alpine sh -c 'apk add curl && ...'
```

The guest network is configured automatically. To use an externally managed
instance, pass its vfkit socket with `gvproxy:<socket>`; set
`MVM_GVPROXY_CONTROL` when using its port-forward control API:

```console
$ gvproxy -listen unix:///run/gvproxy/control.sock \
    -listen-vfkit unixgram:///run/gvproxy/gvproxy.sock &
$ export MVM_GVPROXY_CONTROL=/run/gvproxy/control.sock
$ mvm run --net gvproxy:/run/gvproxy/gvproxy.sock alpine ...
```

### `tap:<dev>`

Attach to an existing TAP device for near-native performance (Linux-only). You
own the plumbing, and the guest needs its own IP configuration -  mvm does no
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

State layout under the `MVM_DATA_DIR` data dir:

```
<data>/
├── sandboxes.json          # registry
├── images/<store-key>/     # meta.json, rootfs/
└── sandboxes/<id>/
    ├── shim.json           # VM config written by the daemon
    ├── console.log         # guest console
    ├── krun.log            # libkrun's own diagnostics
    ├── rootfs|upper|work/  # per-driver filesystem state
    └── guestd.sock        # guestd control channel
```


### Rootless userns mode (Linux)

`mvm serve` re-execs itself inside a user namespace mapping uid 0 to your user
and uids 1..65535 to your `/etc/subuid` range (podman-style). libkrun's
in-process virtiofs server then runs as namespace-root, so guest `chown` and
ownership work with full fidelity: image files appear root-owned, and
`apk`/`apt`/`useradd` behave.

This requires `/etc/subuid` + `/etc/subgid` entries and `newuidmap`/`newgidmap`,
and degrades gracefully (with a warning) when they are missing. Opt out with
`MVM_USERNS=0`. On macOS there is no userns; the daemon runs with host
credentials, so guest `chown` and image ownership are **not** preserved -  the
`copy` driver writes the rootfs as the host user. To compensate, the guestd
repairs the workload's home directory ownership before spawning (gated by
`MVM_HOST_OS=macos`), so a non-root workload can still write to its own home.


### Bind mounts (`-v`)

`-v host:guest[:ro]` mounts an existing **live host directory** into the guest as
an extra virtio-fs share. Like the rootfs, extra `-v` mounts use libkrun's
**LinuxComplete** permission semantics, so the guest's normal Unix DAC
(discretionary access control) checks run against the host's real ownership and
mode bits — the guest sees exactly what is on the host.

On Linux with rootless userns mode, the daemon prepares each `:rw` mount by
`chown -R`-ing the host directory to the subuid that the guest's agent user
(uid 1000) maps to, so a non-root workload can write without loosening the
mount to world-writable. Because guest DAC is real, a guest **root** workload is
trusted with host data and can `chown`/`chmod` the mounted files — this is the
accepted cost of faithful semantics, so treat `:rw` as a trust decision. Prefer
`:ro` unless the workload genuinely needs to write (e.g. an agent workspace whose
artifacts you want back on the host). Note the consequence: after that chown the
host invoking user no longer owns the dir's contents directly, so read them back
via the guest (`mvm run … cat /data/…` or exec) rather than expecting direct
host access. On macOS (no userns) the guest's uid 1000 maps back to the host
invocation user, so there is nothing to prepare.

## Security model and hardening

### Security model

`mvm` treats every sandbox guest as potentially hostile. The VM is the
security boundary: a compromised guest should not be able to access the host
kernel, other sandboxes, or host resources beyond the interfaces explicitly
assigned to that sandbox.

The host exposes only controlled interfaces to each VM:

- **virtiofs** for the guest root filesystem and explicitly requested mounts;
- **vsock** for sandbox control and process execution;
- the selected **network backend**;
- the VM's allocated CPU and memory resources.

The guest does not share the host kernel. Each sandbox has its own Linux
kernel and VM address space.


### Current hardening

Each sandbox runs in an isolated microVM with its own kernel. Host/guest
interaction is limited to explicitly configured interfaces such as virtiofs,
vsock, and the selected network backend.

The guestd installs an always-on seccomp-BPF filter before starting the
workload. It blocks `AF_PACKET` and raw `AF_INET`/`AF_INET6` sockets across
the workload and all exec sessions. `AF_NETLINK` raw-type sockets remain
allowed for network bootstrap, so tools such as `tcpdump`, `arping`,
old-style `ping`, and `udhcpc` are unavailable.

With `--security=strict`, workloads additionally receive a
workload-scoped seccomp profile and `PR_SET_NO_NEW_PRIVS`. This denies BPF,
`ptrace`, namespace and mount changes, kernel module loading, kexec, keyctl,
perf events, userfaultfd, and io_uring syscalls. The trusted guestd
retains the broader syscall surface required for VM and exec management.

Strict mode does not yet drop all Linux capabilities. The libkrunfw guest
kernel supports loading and attaching `cgroup_skb` programs but does not ship
BTF, and mvm does not yet install production egress policies. Planned
cgroup-BPF network enforcement applies only to NIC-backed networking; it does
not apply to `tsi`, whose socket operations are serviced by the host.

Workload users are resolved against the guest rootfs's `/etc/passwd`, never
the host's user database. On Linux, rootless operation can run the daemon
inside a user namespace while preserving guest file ownership through
virtiofs.





### Security limitations

mvm is currently intended for local, single-user use. It is **not a
multi-tenant or remotely exposed sandbox service**.

The host control API is for now unauthenticated and is intended to remain reachable
only on loopback. Do not expose it to an untrusted network.

Host bind mounts and network access are explicit sandbox configuration, but
they also expand the sandbox's capabilities. In particular, a workload with
access to a host mount should be considered trusted with respect to the data
made available through that mount.

The current hardening should therefore not be interpreted as a complete
security boundary against every possible VM, virtiofs, hypervisor, or host
resource attack.

### Hardening roadmap

The detailed threat model, security assumptions, and planned hardening work
are tracked in [`doc/security/SEC.TODO.md`](doc/security/SEC.TODO.md).



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

GET    /api/v1/agents                    (one AgentView per sandbox: derived
                                         status, parent/children, ttl deadline,
                                         last notification; agent-api feature)

GET    /api/v1/images                    DELETE /api/v1/images/{name}
POST   /api/v1/images/pull                             (JSON-lines progress)
POST   /api/v1/images/load?name=…                      (body = OCI-layout .tar; JSON-lines progress)
```


### Agent API
WIP, Gated behind `agent-api` cargo feature

The Agent API provides a VM-scoped control interface for workloads running
inside a sandbox. It does **not** ride HTTP: each running sandbox gets its
own vsock channel (guest connects out to CID 2, port 24643 —
`AGENT_API_VSOCK_PORT`), backed on the host by a per-sandbox unix socket, the
same mechanism used for the exec control channel

A per-sandbox bearer token (minted at boot, revoked when the sandbox stops
or is removed) is still required in the request body and cross-checked
against the socket that accepted the connection. Methods:

```text
inspect                              -> the caller's own agent info (redacted:
                                        id, state, vcpus, ram_mib, parent/children,
                                        capability placeholders; control-plane
                                        internals like host mounts/ports/pids are
                                        never exposed)
stop                                  -> stops the caller's own sandbox
delegate {timeout, message}           -> launch a child that is an interactive
                                        clone of the caller (same workload) to
                                        work on `message`; the message is queued
                                        on the child as a Daddy notification and
                                        delivered through the child's own
                                        notification command once it declares
                                        ready — the parent supplies data only,
                                        never the child's command
set_notification_command {command}    -> register the `<MSG>` shell template the
                                        control plane runs to deliver async
                                        notifications to this agent (`<MSG>` is
                                        the notification rendered as
                                        human-readable text)
test_notification                     -> fire one mock notification of every kind
                                        through the real delivery path
```

The notification template is normally registered at boot by the bridge from
the agent's `NOTIFICATION_CMD` environment variable.

See `scripts/agents/mvm-agent-mcp` for the guest-side MCP bridge that speaks
this protocol.


## Environment

| Variable | Effect |
|---|---|
| `MVM_HOST` | Daemon address for clients (default `http://127.0.0.1:24642`) |
| `MVM_DATA_DIR` | State root defaults to `~/.local/share/mvm` or  `/var/lib/mvm` for root users |
| `MVM_GUESTD_PATH` | Path to the guestd binary |
| `MVM_STORAGE_DRIVER` | Force `overlay` or `copy` |
| `MVM_USERNS=0` | Disable rootless user namespaces on Linux |
| `MVM_GVPROXY_BIN` | Path to `gvproxy` |
| `MVM_GVPROXY_CONTROL` | Control socket for an externally managed `gvproxy` |
| `RUST_LOG` | Configure daemon log verbosity |




## Status

mvm is work in progress and actively evolving.

The architecture, CLI, and APIs are not yet considered stable. The current
backlog and security work are tracked in [`TODO.md`](TODO.md) and
[`doc/security/SEC.TODO.md`](doc/security/SEC.TODO.md).

See [`AGENTS.md`](AGENTS.md)  for development and architectural context.
