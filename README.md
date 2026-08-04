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
stdin works
$ mvm exec -it dev sh          # interactive shell on a pty (vi/top work)
$ mvm run -it alpine sh        # one-shot interactive sandbox (console attach)
$ mvm create -it --name box alpine sh && mvm start -a box
                               # long-lived one; ctrl-p ctrl-q detaches,
                               # mvm attach box comes back to it
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
- **Guest agent as the guest's init.** A static musl binary is injected at
  `/.mvm/agent` (libkrun's own `/init.krun` is PID 1 and execs it); it spawns
  the workload — on its own pty with `-t` — reaps zombies, and serves `exec`
  (with stdin/stdout/stderr streaming and exit codes) over **vsock** — no
  networking required in the guest.
- **Pure-Rust image pulls.** Registry auth, manifest resolution, blob
  verification, layer unpack and OCI whiteouts are implemented in-tree; later
  layers can replace existing paths, including hard-link destinations; no
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
| `mvm run [-i] [-t] IMAGE [CMD…]` | create + start + attach; ephemeral unless `--keep` (`-i` attaches stdin to the console, `-t` gives the workload its own guest pty at your terminal's size, `-it` = interactive shell) |
| `mvm create IMAGE [CMD…]` | create without starting |
| `mvm ps [-a]` | list sandboxes |
| `mvm start [-a] SANDBOX` | start a created/stopped sandbox (`-a` also attaches) |
| `mvm attach [--no-stdin] SANDBOX` | attach the terminal to a running sandbox's console; detach with **ctrl-p ctrl-q** (the workload keeps running) |
| `mvm stop/rm SANDBOX` | lifecycle (`rm -f` force-removes running) |
| `mvm resize SANDBOX [--cpus N] [-m MiB]` | change the VM's allocation; a microVM's size is fixed at boot, so this applies on next start (`--restart` reboots it now) |
| `mvm exec [-i] [-t] SANDBOX CMD…` | run a command in a live sandbox (`-i` forwards stdin, `-t` allocates a pty; `-it` = interactive shell) |
| `mvm logs [-f] [-n N] SANDBOX` | guest console output (`-n` = last N lines) |
| `mvm inspect SANDBOX` | full sandbox JSON |
| `mvm-tui` | live dashboard (sandboxes, images, console); `s` start, `x` stop, `d` delete (asks `y`/`n` — it destroys the filesystem), `r` opens a resize form (`tab` switches field, `+`/`-` adjust, `enter` applies, `^r` applies and restarts). The console pane shows guest output as plain text — escape sequences are stripped, so colours don't render (and the guest can't drive your terminal) |

`run`/`create` options: `--name`, `-e KEY=VAL`, `-v host:guest[:ro]`,
`-p host:guest`, `--net none|tsi|gvproxy[:<socket>]|tap:<dev>`, `--cpus N`, `-m MiB`,
`-w workdir`, `--keep`.

Any command that takes a `SANDBOX` accepts its **id, a unique id prefix, or
its `--name`** (names are unique; creating a second sandbox with a taken name
is refused).

`-i` and `-t` are properties of the sandbox, fixed at create time (as in
docker) — `start` has no `-i`/`-t` of its own, it reuses what the sandbox was
created with. So a long-lived interactive VM is:

```console
$ mvm create -it --name dev alpine sh    # or: mvm run -it --keep --name dev …
$ mvm start dev                          # detached
$ mvm attach dev                         # ctrl-p ctrl-q to leave it running
```

Note that **`mvm run` removes the sandbox when the workload exits** unless you
pass `--keep` (the inverse of docker's `--rm` default), so `run --name foo`
without `--keep` leaves nothing to `start` afterwards.

## HTTP API

```
GET    /health
GET    /api/v1/sandboxes                 POST   /api/v1/sandboxes
GET    /api/v1/sandboxes/{id}            DELETE /api/v1/sandboxes/{id}
POST   /api/v1/sandboxes/{id}/start      POST   /api/v1/sandboxes/{id}/stop
POST   /api/v1/sandboxes/{id}/resize                  {"vcpus":N,"ram_mib":N}
GET    /api/v1/sandboxes/{id}/logs?follow=bool&tail=N (raw console stream)
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
- **Network profiles** (`--net`, placed before the image):
  - `none` (default): fully isolated — loopback only. (mvm attaches a
    dead NIC to switch off libkrun's default TSI backend, which would
    otherwise give every guest transparent host networking.)
  - `tsi`: libkrun's Transparent Socket Impersonation — guest sockets are
    serviced by the host directly. Outbound internet + DNS with **zero
    setup** (no proxy, no NIC, no root), plus `-p` port maps. The guest
    shares the host's network identity; use `gvproxy` when you want NAT
    separation.
  - `gvproxy`: rootless userspace NAT (the same stack podman machine uses).
    Outbound internet plus `-p host:guest` port forwards, with no setup
    beyond having the `gvproxy` binary on `PATH` (`MVM_GVPROXY_BIN`
    overrides): the daemon starts a **private gvproxy per sandbox** and
    stops it with the sandbox.

    ```console
    $ mvm run --net gvproxy -p 8080:80 alpine sh -c 'apk add curl && ...'
    ```

    One per sandbox is not a luxury — a gvproxy vfkit datagram endpoint
    learns its peer from the first packet and never re-learns, so a shared
    socket serves the first VM and silently leaves every later one with no
    route at all (and all guests boot on the same static address anyway).

    `gvproxy:<socket>` attaches to a gvproxy you run yourself instead — one
    sandbox per instance, listening in **vfkit** mode (libkrun speaks the
    vfkit datagram protocol — not `-listen-qemu`), with
    `MVM_GVPROXY_CONTROL` pointing at its `-listen` socket if you want port
    forwards:

    ```console
    $ gvproxy -listen unix:///run/gvproxy/control.sock \
        -listen-vfkit unixgram:///run/gvproxy/gvproxy.sock &
    $ export MVM_GVPROXY_CONTROL=/run/gvproxy/control.sock
    $ mvm run --net gvproxy:/run/gvproxy/gvproxy.sock alpine ...
    ```

    Port mappings are registered through gvproxy's HTTP control API. On Linux,
    gvproxy <v0.8.9 does not implement vfkit unixgram sockets and exits with
    `unsupported 'unixgram' scheme`; use a newer build that includes Unix vfkit
    transport support (or build the upstream project).

    The guest is configured automatically (192.168.127.2/24, gw/DNS .1).
    Throughput is modest (userspace TCP/IP) — fine for package installs
    and API calls.
  - `tap:<dev>`: attach to an existing TAP device for near-native
    performance; you own the plumbing (and the guest needs its own IP
    config — mvm does no addressing):

    ```console
    $ sudo ip tuntap add dev mvmtap0 mode tap && sudo ip link set mvmtap0 up
    $ mvm run --net tap:mvmtap0 alpine ...
    ```

## Known limitations

- Without userns mode (no subuid ranges / newuidmap), guest `chown` over
  virtiofs is limited to host credentials.
- Guest chowns land on subuids on the host; clean up sandbox state through
  `mvm rm` (the daemon), not by deleting the data dir by hand.
- x86_64 Linux only (matches the vendored libkrun FFI subset).
- No CPU/RAM hot-plug: `mvm resize` (and the TUI's `r` form) change the
  spec, and the VM picks the new size up when it next boots.
- `run -it` sizes the guest pty once, at start; resizing your terminal
  mid-session does not propagate (`exec -it` does track resizes).
