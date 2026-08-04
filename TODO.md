# TODO

Prioritized backlog. See `README.md` for user-facing behavior and
`AGENTS.md` for architecture notes and sharp edges.

## 1. Honor the image USER directive
`ImageConfig.user` is parsed and ignored — every workload runs as root.
Faithful behavior (docker parity, needed by e.g.
`docker/sandbox-templates:opencode`, which runs as `agent`): the guest
agent resolves the user against the rootfs `/etc/passwd`/`/etc/group`, then
setgroups/setgid/setuid before spawning the workload, and sets
HOME/USER/LOGNAME. Also applies to exec sessions (docker exec runs as the
container user by default, `-u` overrides). The OpenCode image now pulls
successfully after hard-link replacement handling was added, but its VM
startup still stops before this identity behavior can be tested.

## 2. Credentials injection (design plan)
Modeled on Docker Sandboxes' credential handling
(https://docs.docker.com/ai/sandboxes/security/credentials/), adapted to
mvm's daemon + vsock architecture. Guiding principle: **real secrets never
enter the guest** — not its env, not its filesystem; the guest sees
sentinels and the host injects at the network boundary.

**Milestone A — secret store + env injection (foundation):**
- `mvm secret set|rm|ls` in the daemon. Backends: Linux Secret Service
  (GNOME Keyring / KDE Wallet) via D-Bus, falling back to an encrypted
  file under the data dir (0700 dir / 0600 file, same posture as
  `~/.docker/config.json`).
- Scopes, docker-style: global (all sandboxes, applied at create) vs
  per-sandbox (immediate). `mvm run --secret NAME[=ENV_VAR]` resolves at
  start. Secret *names* go in `SandboxSpec`; values are resolved at start
  time only — never persisted in `sandboxes.json`, redacted in
  `mvm inspect` output.
- Weakness (accepted for A): the value still lands in the guest's env,
  like `-e` today. A is about storage hygiene + UX, not isolation.

**Milestone B — credential proxy with sentinel values (the real thing):**
- Guest env gets `ANTHROPIC_API_KEY=mvm-proxy-managed` (sentinel), plus
  `HTTP(S)_PROXY=http://127.0.0.1:<port>`. The agent bridges that
  loopback port over **vsock** to a host-side proxy owned by the daemon —
  works in every network mode including `none`/`tsi`, since egress
  happens host-side.
- The host proxy matches requests against a service map (api.anthropic.com,
  api.openai.com, api.github.com, generativelanguage.googleapis.com, …)
  and swaps the sentinel for the real header value on the way out.
  Built-in service list + user-defined `(domain, header, secret)` triples.
- HTTPS: header injection requires TLS termination at the proxy. Generate
  a per-data-dir CA, terminate+re-originate TLS for *matched* domains
  only, and have the agent install the CA into the guest trust store at
  boot (append to `/etc/ssl/certs/ca-certificates.crt` + the common
  distro variants). Unmatched domains get plain CONNECT tunneling — no
  MITM outside the declared service list.
- Egress policy falls out for free: the proxy can enforce a domain
  allowlist per sandbox (deny-by-default option for agentic workloads).

**Milestone C — forwarding, not copying:**
- SSH agent forwarding over vsock (`SSH_AUTH_SOCK` bridged by the guest
  agent) for git push/commit signing — keys never enter the guest.
- OAuth-style flows (Claude Code, etc.): token lives host-side, proxy
  injects; sandbox never sees it.

Non-goals for now: console-log secret redaction; Windows/macOS keychains.

## 3. Live window resize for `mvm run -it` / `mvm attach`
The workload pty boots at the client's size and TERM is forwarded, but the
size is fixed for the session: resizing the terminal mid-run leaves the guest
on the old geometry (exec -it has full resize). `attach` is worse — it uses
`tty_size` as recorded at *create* time, so attaching from a differently sized
terminal starts out wrong. Needs a console-level resize path: a sandbox-keyed
Resize message (the protocol's is keyed to an exec session) plus TIOCSWINSZ on
the workload pty in the agent, with `run`/`attach` polling `term_size()` the
way `exec` does.

## 4. Guest ptys are not reachable by path (`ttyname` fails)
Inside a guest, `tty` reports "not a tty" and `ls /dev/pts` does not show the
workload's pty even though `[ -t 0 ]` is true and `/proc/self/fd/0` points at
`/dev/pts/0`: three devpts instances end up stacked on `/dev/pts` (libkrun's
init, then the agent's), and the pty is allocated in one that a later mount
shadows. Anything resolving a tty by name — `sudo`, `screen`, `script`,
`os.ttyname` — sees an inconsistent world. Fix is for `ensure_devpts` to reuse
a usable existing instance instead of stacking another; check what ptmxmode
that leaves for non-root workloads before changing it.

## 5. Decide `run`'s lifetime default
`mvm run` removes the sandbox when the workload exits unless `--keep`, the
inverse of docker (`run` keeps; `--rm` removes). It reads as "naming does not
work": `run --name foo …` then `mvm start foo` says not found, which is why
the CLI now prints a notice when it removes a named sandbox. Either keep the
current default and leave the notice, or flip to docker semantics with `--rm`
(`--keep` staying as a no-op alias) — a breaking change for scripts.

## 6. Pull memory usage
Layer blobs are buffered fully in RAM during pull (fetch + sha256 +
unpack from `Vec<u8>`); a 300 MB layer means a 300 MB spike. Stream to a
temp file with incremental hashing instead — matters for images like
`docker/sandbox-templates:opencode` (~750 MB compressed).

## 7. TUI: render guest colours in the console pane
The pane is plain text since escapes are stripped at the edge. Showing colours
means parsing SGR into ratatui spans (`ansi-to-tui`-style) — never by passing
escapes through, which is what let the guest drive the user's terminal.

---

## Done

- **E2E integration** — 49/49 on real KVM in rootless userns mode with the
  overlay driver, gvproxy v0.8.9 installed (2026-08-04); 21/21 at the
  original pass (2026-08-01).
- **TUI delete confirmation + selection fix** — `d` asks `y`/`n` before
  destroying a sandbox's filesystem. Naming the target in that prompt exposed
  a second bug: `clamp_selection` recovered a lost selection by jumping to the
  *last* row, so actions could silently hit a sandbox other than the one the
  user was looking at. Both covered by unit tests.
- **TUI console sanitizing** — the pane rendered guest bytes verbatim, so a
  guest's escape sequences drove the user's terminal; the ones that query it
  (shell prompts emit `ESC [ 6 n`) made the terminal reply on the TUI's stdin,
  where crossterm parses `ESC ]` + reply as Alt+`]` plus plain keys — `r` from
  `rgb:` opened the resize form by itself. Stripped at the poller edge, with
  the tail capped so a long console isn't refetched whole every poll.
- **Attach** — `mvm attach [--no-stdin] SANDBOX` and `mvm start -a`, so an
  interactive sandbox no longer has to be driven by the `run` that created
  it. `-i`/`-t` stay create-time properties (docker parity) and attach reads
  them off the spec; ctrl-p ctrl-q detaches without sending EOF; the replayed
  backlog is capped (`logs -n N` / `?tail=N` on the logs route).
- **Networking validated end to end** — the gvproxy host-to-guest
  forwarding question is closed: one gvproxy vfkit datagram endpoint serves
  exactly one VM (it learns its peer from the first packet and never
  re-learns), so bare `--net gvproxy` now spawns a *private* gvproxy per
  sandbox, registers `-p` maps on that instance's control socket, and
  SIGTERMs + reaps it with the VM (pid persisted for post-restart cleanup).
  `gvproxy:<socket>` still attaches to an external one. Sequential and
  concurrent sandboxes both have working forwards.
- **`mvm run -it`** — was frozen: `io::copy` into `stdout`'s LineWriter held
  prompts and raw-mode echo until a newline; `krun_set_exec` argv repeated
  the agent path, so a second agent instance ate the `MVM_*` vars and no
  workload pty was ever allocated; and three stacked line disciplines
  double-echoed and buffered keystrokes. Now: flush per chunk, argv without
  the exec path, raw shim pty + raw bridged console + explicit interactive
  termios on the workload pty, plus client winsize and TERM.
- **Resize CPU/memory** — `mvm resize SANDBOX [--cpus N] [-m MiB]
  [--restart]`, `POST /sandboxes/{id}/resize`, and the TUI's `r` form
  (tab/digits/+- , enter applies, ^r applies and restarts). No hot-plug in
  libkrun, so it rewrites the spec and the next boot picks it up; the form
  and CLI both say when a restart is pending.
- **Rootless chown / virtiofs UID translation** — `mvm serve` re-execs
  into a user namespace (subuid ranges via newuidmap/newgidmap, two-stage
  SIGSTOP handshake so the daemon keeps capabilities after exec); the
  in-process virtiofs server chowns to mapped uids. Unpack applies real
  tar-header ownership. `ext4` block-root driver kept as opt-in.
- **Overlay storage rootless** — `userxattr` mounts inside the userns;
  upper layer persists across stop/start; probe with labeled fallback.
- **Network isolation + modes** — `none` now really is isolated (dead
  unixgram NIC disables libkrun's default TSI); `tsi` exposed as an
  explicit zero-setup outbound mode (agent writes public resolvers);
  `gvproxy[:<socket>]` fixed to the vfkit datagram protocol with
  agent-side static IP/route/DNS bootstrap (ioctls, no guest binaries).
- **Exec**: binary-safe base64 streams; `-it` with real pty (openpty,
  devpts, Resize protocol + endpoint, raw-mode CLI); kill-on-disconnect
  guard on the API stream.
- **Interactive `mvm run -i/-t`** — console stdin attach over
  `POST /sandboxes/{id}/stdin`, VEOF on EOF, local raw mode with `-t`.
- **Deterministic lifecycle** — `start` waits for the agent channel
  (exec also tolerates a concurrent start); log broadcast closes on shim
  exit so `run` streams to EOF and `logs -f` terminates.
- **Image store** — up-to-date digest short-circuit, atomic staged
  pulls, per-key pull locking, `rmi` in-use refusal.
- **Layer replacement unpacking** — later OCI hard-link entries replace an
  existing destination before unpacking, matching Docker layer semantics.
- **Guardrails** — dynamically-linked agent rejected (PT_INTERP check);
  mvm flags after the image rejected with a hint.
