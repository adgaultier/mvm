# AGENTS.md — working on mvm

## What this is

A docker-style microVM sandbox platform: OCI images run as KVM microVMs via
libkrun. Workspace of 11 crates under `crates/`. `README.md` is the
user-facing documentation (CLI, API, storage/network modes); `TODO.md` is
the living backlog and done-log. This file is for people (and agents)
changing the code.

## Build & test

```sh
cargo build --workspace          # host binaries (mvm, mvm-tui, gnu mvm-guestd)
cargo test --workspace           # unit tests, no KVM needed
cargo build -p mvm-guestd --target x86_64-unknown-linux-musl --release
                                 # the REAL guestd (static). The gnu-linked
                                 # target/debug/mvm-guestd will NOT run in guests
                                 # (and the daemon refuses to inject it).
scripts/build.sh                 # all of the above, release, into dist/
just -f scripts/integration/Justfile all   # boots real VMs; needs /dev/kvm
                                 # (Linux) or libkrun (macOS), plus network
                                 # (or `just <section>` for one, `just` to list)
```

Hosts: Linux x86_64 (KVM) and macOS Apple Silicon (Hypervisor.framework),
using Homebrew libkrun. `scripts/install-darwin.sh` installs libkrun, Zig, and
cargo-zigbuild.
Guests are same-arch as the host, so the guestd's musl target follows the
host arch; on macOS the cross-link goes through `cargo zigbuild` and the
`mvm` binary must carry the hypervisor entitlement (build.sh codesigns
dist/; a bare `cargo build` binary needs it too — see Sharp edges).

The guestd is musl-only because `-C target-feature=+crt-static` on the
gnu target breaks proc-macro crates (`serde_derive`). `guestd_binary()`
resolution: `MVM_GUESTD_PATH` → next to the executable → the dev tree's
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
- **Guestd as the guest's init** (`/.mvm/guestd`, injected at start,
  exec'd by libkrun's `/init.krun` which is PID 1): spawns the workload,
  reaps zombies, serves exec (pipes or pty) over **vsock port 1024** →
  `<sandbox>/guestd.sock` host-side. Wire protocol: u32-BE length + JSON
  frames, byte payloads base64-encoded (`common::protocol`). With `-t` the
  workload gets its own guest pty and the guestd bridges it to the console.
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
  vfkit-mode datagram socket + guestd-side static IP bootstrap (the daemon
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
    └── guestd.sock        # guestd control channel (unix listener)
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
| `manager` | sandbox registry + lifecycle; owns guestd vsock channels + console stdin |
| `guestd` | guest PID 1; std+libc only, poll(2) event loop, must stay static-friendly |
| `api` | axum routes; streams = `Body::from_stream`; exec has a kill-on-drop guard |
| `cli` | `mvm` binary incl. hidden `__vm-shim` subcommand + userns re-exec |
| `tui` | ratatui dashboard; modal forms own the keyboard while open (`r` resize, `d` delete confirmation) |
| `flow` | `mvm-flow` binary; rataflow lineage graph of one agent + descendants, polled from `GET /api/v1/agents` |

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

- **gvproxy requires vfkit Unix datagrams** — Linux gvproxy versions before
  v0.8.9 do not support the `unixgram` vfkit endpoint and fail with
  `unsupported 'unixgram' scheme`. The external form uses
  `MVM_GVPROXY_CONTROL` for the HTTP control socket; the managed form creates
  the control and vfkit sockets under the sandbox data directory.
- The guestd runs as **the guest's init**: it must reap zombies and only use
  std + libc (keep dependencies out of `crates/guestd`). Pty sessions share
  one fd for stdin/stdout — mind the close-once logic. libkrun's own
  `/init.krun` is literally PID 1 and execs the guestd as its child.
- **`krun_set_exec` argv must NOT repeat the exec path** — `init.krun`
  prepends it as `argv[0]` itself. Passing it again made the guestd treat
  `/.mvm/guestd` as its own workload and run a *second* guestd: the outer
  instance consumed every `MVM_*` var (and `remove_var`'d it) before the
  inner one — the instance that actually spawns the workload and serves
  exec — ever saw it, so `MVM_CONSOLE_TTY` silently did nothing. The tell is
  two `/.mvm/guestd` entries in the guest's `ps`.
- **The `MVM_*` env channel rides the kernel cmdline.** libkrun serialises
  envp into the guest cmdline (visible in `/proc/cmdline`, quoted, before
  `--`); the kernel hands `KEY=VALUE` params to init, which passes them on.
  When a var "disappears", compare `/proc/1/environ` with the guestd's own
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
- **Termios restore alone does not undo guest terminal modes.** A guest TUI
  may DECSET 1003 (mouse-motion reporting) or push the kitty keyboard
  protocol; that state lives in the terminal *emulator*, not in the line
  discipline, so if the VM dies first the host terminal keeps translating
  mouse moves into shell input after `mvm` exits, or ctrl+letter into
  `ESC[97;5u`-style keycodes. The reset is issued only when the guest is
  gone: `restore_terminal(modes)` in `cli/src/run.rs` emits the DECRSTs for
  the mouse modes plus the keyboard-protocol resets (`RESET_TERMINAL_MODES`)
  from the guard's `Drop` (stream EOF = sandbox exit) and the signal handler
  (async-signal-safe `write`), while a *detach* (`^P^Q`) restores termios
  only — the modes must survive so the TUI's keyboard protocol still works
  when the next attach replays into the still-running guest. Resetting on
  detach broke resume: the backlog can't re-assert the modes (console_filter
  strips them from the recording) and the TUI doesn't re-push them.
- **One gvproxy serves one VM, for life.** Its vfkit datagram endpoint
  learns the peer from the first packet and never re-learns, so a shared
  socket leaves every later VM with no route and no error (and all guests
  boot on the same static 192.168.127.2 anyway). Bare `--net gvproxy` runs
  a private gvproxy per sandbox; kill *and* `wait()` it, or it lingers as a
  zombie holding host ports.
- **Sandbox starts are serialized per sandbox.** `Manager::start` acquires a
  per-id async lock before checking state or preparing storage, preventing two
  concurrent callers from launching duplicate shims. Guestd exec output uses
  awaited bounded-channel sends, so slow clients apply backpressure instead of
  silently losing stdout/stderr frames.
- **`unshare(CLONE_NEWUSER)` requires a single-threaded process** — userns
  entry must happen before the tokio runtime exists.
- **Terminal *queries* and *mode changes* are filtered per consumer, not per
  stream.** A tty session's output contains sequences that ask the terminal
  to answer — DSR (`ESC[6n`, "where is the cursor?"), Device Attributes
  (`ESC[c`), OSC colour/palette queries, XTGETTCAP/XTVERSION/DECRQM. A
  question that reaches someone who will not answer it makes *their*
  terminal answer into its own input buffer: stray `^[[1;5R` in their shell
  (the alpine prompt `~ # ` is 4 columns wide, hence column 5), or a dump of
  `;10;rgb:…` replies echoed after `mvm logs` exits. Mode changes are just
  as leaky: a replayed DECSET 1003 or kitty-keyboard push rewires the
  reader's terminal with nobody left to undo it. So `manager::console_filter`
  strips both from `console.log`, and the logs route runs the same filter
  over the backlog *and* the live broadcast unless the client asks for
  `?raw=true` (the backlog re-filter also cleans logs recorded before the
  filter existed). Only an interactive console session (`mvm attach`,
  `mvm run -it`) sets `raw` — it owns the terminal, reads the replies, and
  resets modes on exit (`RESET_TERMINAL_MODES`). Filtering only the
  recording is the trap: it leaves `mvm logs -f`'s live tail unprotected,
  which is the same bug with a longer path to it. Colours, cursor motion and
  erases are real output and stay everywhere.
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
  guestd, never against the host's user database — the same name means
  different uids in different images. The drop order in `apply_user` is
  setgroups → setgid → setuid (dropping the uid first forfeits the privilege
  needed for the other two), and it is registered *after* the pty
  `pre_exec`s, since `TIOCSCTTY` must run while still root. The pty slave is
  `fchown`ed to the target so the workload can reopen `/dev/tty`.
- Whiteout handling and layer unpack have unit tests in `crates/image` —
  extend those rather than testing via pulls. Later OCI layers may replace an
  existing path with a hard link; remove the destination before
  `tar::Entry::unpack_in` or the unpack fails with `EEXIST`.
- **Raw sockets are banned guest-wide, always.** The guestd installs a
  seccomp filter at the top of `real_main` (before mounts/network; a failed
  install is fatal) that denies `socket(2)` for `AF_PACKET` (any type) and
  `AF_INET`/`AF_INET6` with `(type & 0xf) == SOCK_RAW`. The check keys on the
  *domain first* precisely because `AF_NETLINK` sockets are routinely created
  with a raw type (`SOCK_RAW | SOCK_CLOEXEC` in the network bootstrap) — that
  is legitimate and stays allowed. The filter is inherited and additive, so a
  workload cannot weaken it, and it kills on an unexpected arch (a wrong
  syscall number would otherwise be interpreted against the wrong table).
   This breaks `tcpdump`, `arping`, old `ping`, and AF_PACKET DHCP clients
   (`udhcpc`); the planned `--net passt` guestd-side bootstrap must NOT rely on
   DHCP — it needs the gvproxy-style static-IP bootstrap instead.    Always-on
   by design: there is no opt-out. Filter shape is probed in-VM by
   `scripts/integration/probes/rawprobe.c` through `just seccomp` (as
   workload *and* exec session).
- **`--security=strict` is a workload-scoped *second* seccomp filter.** The
  raw-socket ban above is installed on the guestd (PID 1) at boot and inherited
  by everything. The strict filter is different: it is installed in the
  workload's `pre_exec` (`apply_strict_seccomp` in `linux.rs`, *before*
  `apply_user`'s privilege drop), so only the workload — and exec sessions —
  lose `bpf`/`keyctl`/`perf_event_open`/`userfaultfd`/`io_uring_*`, `ptrace`,
  namespace changes, mount/pivot-root, module loading, and kexec, while the
  guestd keeps the full syscall surface it needs for exec/pty plumbing. The
  strict set is exercised by `scripts/integration/probes/strictprobe.c` via
   `just seccomp`. The var rides the `MVM_*` channel as
  `MVM_SECURITY_STRICT` and is scrubbed like the other plumbing vars. This
   matters for the cgroup-BPF plan: `bpf(2)` is denied for every workload and
   exec session, while the guestd can still load trusted eBPF programs itself.
   Strict mode adds the remaining high-risk syscall restrictions. TSI is excluded
  from guest network policy because its sockets are host-serviced. Plumbed:
  `--security` on `create`/`run`/`clone` → `SandboxSpec` → `ShimConfig` → shim
  env → guestd. Probe the guest kernel's BPF capability with
   `scripts/integration/probes/bpfprobe.c` (BTF/cgroup2/prog-load verdict)
   before assuming CO-RE/BPF-LSM support; cgroup-BPF does not require BTF.
   The embedded Aya bootstrap lives in `crates/guest-ebpf` and is built by
   `crates/guestd/build.rs` for Linux guestd targets. It attaches `connect4`
   for the bootstrap and, for NIC-backed modes only, `dns_egress` with the
   `ALLOWED_DNS_IPV4` map. The map contains the NIC gateway from
   `MVM_NET_CONFIG` (normally `192.168.127.1` for gvproxy); TSI deliberately
    skips DNS eBPF enforcement. The eBPF programs attach to the guest cgroup
    root, so workloads and exec sessions cannot escape the policy by changing
    cgroup membership. The
   dedicated end-to-end check is `just -f scripts/integration/Justfile dns-ebpf`;
   `just bpfprobe` is retained as a historical raw kernel probe but skips
   because workload `bpf(2)` is intentionally denied; `dns-ebpf` validates the
   trusted guestd attachment path. The `seccomp`
   integration section probes BPF load, attach, detach, map create/update, and
   pin/get attempts as workload root and through exec sessions.
  Strict workloads also set `PR_SET_NO_NEW_PRIVS` before exec and drop
  `CAP_CHOWN` from the bounding set (`drop_cap_chown` in `apply_strict_seccomp`,
  `PR_CAPBSET_DROP` — dropping from the bounding set also clears it from the
  permitted/effective/ambient sets and it cannot return via exec, setuid, or
  file capabilities). The cap drop stops a *root* strict workload from re-owning
  live host data on `-v` (LinuxComplete) mounts — the mechanism that prevents
  ownership divergence between nested parent/child workspaces sharing a host
  dir. `apply_strict_seccomp` runs before `apply_user`'s privilege drop, so the
  drop happens while the process is still root with a full bounding set.
   Verified by `scripts/integration/probes/chownprobe.c` via `just seccomp`
  (chown succeeds by default, EPERM under strict). Dropping further caps
  (e.g. `CAP_FOWNER`, to also stop `chmod` on non-owned files) remains a
  separate task.
 - **VM token: never persisted, never exposed — not even its hash.** The Agent
  API token is minted in `Manager::start` and held as `guest_token_hash`
  (SHA-256) on the `Sandbox` record — but that field is `#[serde(skip)]`, so
  it is neither serialized into API responses nor written to `sandboxes.json`:
  it lives only in the manager's memory for the VM's lifetime and is cleared
  the moment the sandbox stops or exits (not merely gated on state). The
  plaintext token is passed to the shim as a *process env var*
  (`MVM_GUEST_TOKEN`) — never through `ShimConfig`, so `shim.json` stays
  token-free — and the shim forwards it into the guest via the `MVM_*` env
  channel, where it is deliberately **not scrubbed**: it must reach the
  workload's environment (and every exec session, via `baseline_env`) so the
  tools it spawns (the `mvm-agent-mcp` bridge) can authenticate over the
  Agent API's vsock channel. The plaintext therefore exists only transiently
  — in the shim's process environment, in the guest environment, and on
  `/proc/cmdline` (the `MVM_*` channel rides the kernel cmdline) — and is
  never written to host disk. The Agent API is not HTTP: it's a per-sandbox
  vsock channel (guest → CID 2, port 24643 = `AGENT_API_VSOCK_PORT`), backed
  host-side by a per-sandbox unix socket (`<sandbox_dir>/agent-api.sock`,
  same libkrun vsock-over-unix-socket bridge as the exec control channel) —
  there is no shared listener or `{id}` in any request; identity comes from
  *both* which socket accepted the connection and the bearer token in the
  request body, and `Manager::authorize` checks they agree. Token lookup uses
  a constant-time hash compare over the sandbox list (no `HashMap` keyed on
  the secret). The whole surface is gated behind the `agent-api` cargo
  feature (on `mvm-manager`, on by default via the `mvm` binary's own
  `agent-api` feature): `--no-default-features` drops the accept loop and the
  token-verification code, while token *minting* into the guest env stays (it
  is part of the boot plumbing).
- **Control-plane notification delivery runs `sh -c` through `mvm exec`.**
  A sandbox's `notification_command` (`async_cmd`, `<MSG>` = the
  notification's human-readable text, `Notification::to_text`) is a template
  the control plane executes as
  `sh -c <template>` via the manager's `exec` — so delivery only works while
  the guestd is up and the sandbox is `Running` (that's by design: the spec
  delivers asynchronously to *running* agents). `<MSG>` substitution is a
  literal string replace (`MSG_PLACEHOLDER`), not a shell expansion. The
  rendered text is prose — spaces, parentheses, semicolons — so templates
  must quote `<MSG>` (single quotes, e.g. `echo '<MSG>' >> log`); a bare
  `<MSG>` word-splits and breaks on the first metacharacter. The placeholder
  is `<MSG>`, not `$MSG`, on purpose: a `$`-form
  gets expanded by the shell that *creates* the sandbox when the template
  arrives via `-e` ("unbound variable" or silently empty), `<MSG>` survives
  any quoting. `test_notification` (Agent API method + `mvm-agent-mcp` tool)
  fires one mock notification of every kind through this same path and is
  the end-to-end wiring check in `agent-api.sh`.
- **Delegation boots an interactive clone — never a parent-supplied command.**
  `DelegateRequest` is `{timeout, message}`; the child inherits the caller's
  spec verbatim (same workload, image, env — including `attach_stdin`, so an
  interactive parent stays attachable in its children) and starts
  immediately, with the message queued on it as a Daddy `input`
  notification (`Sandbox.pending_notifications`, persisted). The queue is
  drained by `flush_pending` once the child is running, has declared
  `ready`, and has registered its `notification_command` — both `mark_ready`
  and `set_notification_command` trigger a flush, whichever comes last wins
  the race. An agent may therefore hand a child *data*, but only the
  operator-provided workload + notification template decide what a child
  executes. Delivery failures leave the remainder queued; a non-zero exit of
  the agent's own command counts as delivered (recorded in the history).
- **Agent state in `Sandbox` lives nested under `agent`**. `parent`,
  `ttl_deadline`, `notification_command`, `pending_notifications` and
  `recent_notifications` moved from five flat fields to a single
  `SandboxAgent` object kept as `Sandbox.agent` — so `mvm inspect` shows
  one tidy nested object instead of five flat top-level keys. Deliberate
  breaking change: legacy `sandboxes.json` files written before this
  nesting will fail to deserialize old flat keys; the manager does not
  migrate them — blow away the registry (or hand-edit) after upgrading.
- **Agent API responses need a graceful close.** Each Agent API connection
  carries one request + one response; dropping the unix socket right after
  `write_all` races libkrun's vsock bridge — the guest sees EOF *before* the
  response bytes still in flight and loses the whole frame (the tell:
  `vsockprobe: recv header: Success`, i.e. `read()` returned 0 with no
  errno). Long-lived channels never hit this because they don't close
  per-message. `handle_conn` therefore half-closes (`shutdown`) and drains
  until the client closes (capped at 5 s) before the stream is dropped.
- **macOS rootfs loses image ownership (host uid owns everything).** The copy
  driver writes the rootfs as the host user and macOS has no userns, so
  `/home/agent` ends up owned by the host uid and a non-root workload can't
  write its own home (`PermissionDenied` on opencode's log). The shim sets
  `MVM_HOST_OS=macos` (host-gated via `#[cfg(target_os = "macos")]`), and the
  guestd — before spawning a non-root workload whose home is owned by someone
  else — recursively `lchown`s the home to the workload's uid/gid using
  descriptor-relative traversal. macOS
  virtiofs `LinuxComplete` semantics turns that chown into a
  `user.containers.override_stat` xattr, so it sticks for the boot; it's a
  no-op on Linux (userns already owns the files, and the uid check skips it).
- **rataflow event loops must drain every pending event.** The terminal
  delivers mouse at 125–1000 Hz; `mvm-flow` polls with a timeout for the
  *first* event, then loops `poll(Duration::ZERO)` until empty before
  rendering — reading one event per frame makes pan/drag stutter. Also never
  rebuild the `Flow` on poll: reconcile by diffing (`node_content_mut` for
  status updates, add/remove only on structural change), or the viewport and
  selection reset every second. `apply_layout(Sugiyama)` rewrites node
  positions *and* handle sides, so it runs only on structural change, with
  `request_fit_view()` only on the first one. The delegation lineage behind
  the edges is data-path only (`Sandbox.parent`/`ttl_deadline`, display in
  the nodes); TTL/idle *enforcement* is still an open item.
- **Mount host paths must be absolute.** libkrun's virtiofs passthrough opens
  the host dir with `openat(AT_FDCWD, …, O_NOFOLLOW)` from the *daemon's* cwd,
  so a relative `-v` source fails with `ENOENT` at device activation — which
  surfaces as a guest `fc_vcpu` panic `Failed to activate device: BadActivate`
  (and `krun.log` records `virtio_fs: failed to create worker: No such file
  or directory`). The CLI canonicalizes `-v` host paths (`parse_volume`), and
  `Manager::validate_mounts` rejects any relative path that still slips in via
  the API/TUI.
- **`-v` mounts are LinuxComplete, and the manager chowns `:rw` mounts so the
  guest's uid 1000 can write.** `add_virtiofs` uses `krun_add_virtiofs4` with
  `KRUN_SEMANTICS_LINUX_COMPLETE` for every extra `-v` share (same as the
  rootfs), so the guest's normal Unix DAC runs against the host's real
  ownership/mode bits — the guest sees exactly what's on the host, and a guest
  **root** workload is therefore trusted with the live host dir (`chown`/`chmod`
  forwarding). To keep a non-root workload writable on `:rw` mounts, the manager
  `chown -R`s the host dir to the subuid/subgid that guest 1000 maps to
  (`prepare_mount_ownership`, Linux + userns only, gated on
  `MVM_SUBID_START`/`MVM_SUBGID_START` that `maybe_enter_userns` binds into the
  userns child; macOS has no userns, so guest 1000 maps back to the invocation
  user who already owns the dir). Treat `:rw` as a trust decision (e.g. an agent
  workspace whose artifacts you want back on the host); prefer `:ro`. Do NOT try
  to restore the non-root write by switching `-v` to
  `KRUN_SEMANTICS_LINUX_SIMPLIFIED` — that silently removes guest-root's DAC on
  host data *and* doesn't actually grant guest-1000 writes either; the chown
  model is the supported path. Cost of the chown: the host invoking user loses
  direct write (and for a `0700` dir, read) access to the mount's contents —
  retrieve artifacts from inside the guest (`mvm exec … cat /data/…`), or the
  shared `-v` dir is unreadable to host tools after first boot. Idmaps are
  unsupported on these mounts. The guest-1000 identity is *namespace* uid
  `1000`: `chown(2)` takes caller-namespace uids, so passing the host subuid
  (`sub_start + 999`) to `lchown` is an unmapped uid and fails with `EINVAL` —
  chown to `1000` and let the kernel map it to the subuid on disk (that's what
  virtiofs then reports to the guest as uid 1000). The cli userns step and the
  manager must agree on this mapping.
- **A `workspace:` mount is namespaced per delegated child.** The `-v`
  keyword `workspace:HOST:GUEST` tags one `:rw` mount as the sandbox's agent
  workspace (`Mount.workspace`); it must be `:rw`. `Manager::delegate` runs a
  child with the workspace host rewritten to `HOST/<child-id>` (dir created via
  `nest_workspace_for` on the child *record* — the child's id only exists after
  `create`), so parent and children never share a live workspace dir.
  Non-workspace mounts stay inherited verbatim and shared. Nesting recurses for
  free: a child whose workspace is already `HOST/<parent-id>` delegates a
  grandchild into `HOST/<parent-id>/<grandchild-id>` — the remap appends the
  current child's id to whatever the workspace host already is. The child's own
  boot then `prepare_mount_ownership`s its nested dir for its guest-1000, so
  ownership stays consistent (identical identity for every generation). The
  nested parent workspace gets re-owned by *both* parent and child boots (same
  subuid), which is why integration cleanup must relax the tree from the
  guest (`chmod -R a+rwx`) before the host can `rm -rf` it — see the
  agent-api.sh workspace checks.
- **`krun_set_rlimits` speaks numeric resource IDs.** libkrun joins the
  entries into `KRUN_RLIMITS="…"` and `/init.krun` parses every field with
  bare `strtoull`, so `7=1024:1024` works while the header-documented
  `RLIMIT_NOFILE=1024:1024` fails silently ("Invalid rlimit ID", nothing set)
  — and an *empty* var fails the same way, which is why `set_rlimits`
  refuses empty lists. The shim forwards only the daemon's NOFILE
  (`host_rlimits()` in shim.rs), pairing it with the **Linux** enum
  position — host constant values differ on macOS (NOFILE is 8 there, 7 on
  Linux) — so the host half must be resolved per-platform by the libc
  crate, never hardcoded from either side. Every other resource keeps the
  guest kernel defaults. `RLIM_INFINITY` travels as decimal `u64::MAX` —
  macOS's infinity is `0x7fff_ffff_ffff_ffff`, not `u64::MAX`. The
  forwarded value is the *daemon's* limit: start `mvm serve` with the
  desired `ulimit -n` (macOS defaults to a soft 256 in some launch
  contexts).




## Runtime env vars

`MVM_HOST` (client → daemon addr), `MVM_DATA_DIR` (state root),
`MVM_GUESTD_PATH` (guestd binary), `MVM_STORAGE_DRIVER`
(`overlay`/`copy`), `MVM_USERNS=0` (disable userns mode), `MVM_GVPROXY_BIN`
(gvproxy binary for managed `--net gvproxy`), `MVM_GVPROXY_CONTROL`
(control socket of a gvproxy *you* run, for `--net gvproxy:<socket>` port
maps).

Daemon → guestd (set by the shim): `MVM_MOUNTS`, `MVM_NET_CONFIG`,
`MVM_NET_TSI`, `MVM_CONSOLE_TTY`, `MVM_CONSOLE_SIZE`, `MVM_USER`,
`MVM_HOSTNAME` (the clean sandbox name, or the plain id when unnamed; applied
by the guestd via `sethostname(2)` + `/etc/hostname` + `/etc/hosts`),
`MVM_HOST_OS` — scrubbed by the guestd before it spawns the workload — plus
`MVM_GUEST_TOKEN`, which is deliberately *not* scrubbed so the workload's
tooling (the `mvm-agent-mcp` bridge) can authenticate over the Agent API's
vsock channel.

For managed gvproxy, `MVM_NET_CONFIG` carries `192.168.127.2/24,192.168.127.1`;
the guestd uses the gateway as the guest DNS endpoint and the DNS eBPF map
allows only that IPv4 address on TCP/UDP port 53. `MVM_NET_TSI` keeps its
existing public resolvers and intentionally skips this policy.

## Conventions

- Comments: minimal. State what the code can't say (IDs, invariants,
  gotchas); no design essays or implementation justifications slop — the
  "why" belongs in Sharp edges or commit messages.
- Errors: `mvm_common::Error` (`thiserror`) inside crates; CLI-facing code
  maps to `String`.
- Keep the daemon's HTTP surface documented in README when routes change.
- Integration test must stay green and self-contained: `just -f
  scripts/integration/Justfile all` runs against an isolated `MVM_DATA_DIR` +
  port, skips gracefully (KVM, gvproxy), and cleans up after itself.
- Record non-obvious findings in this file's Sharp edges; track work in
  `TODO.md` (move finished items to its Done section with a one-liner on
  the mechanism).
