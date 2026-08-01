# TODO

Prioritized backlog. See `README.md` for user-facing behavior and
`AGENTS.md` for architecture notes and sharp edges.

## 1. Validate networking modes on real host
`scripts/integration.sh` now exercises these on real KVM: `none` isolation
(TSI must be off), `tsi` outbound+DNS, and gvproxy outbound/DNS. The gvproxy
control API registers `-p` mappings, but host-to-guest forwarding still needs
investigation; the current compatible gvproxy run is 28 passed, 1 failed.
The guest-side static config (192.168.127.2/24 via agent ioctls) and vfkit
socket wiring otherwise work for outbound traffic.

## 2. Honor the image USER directive
`ImageConfig.user` is parsed and ignored — every workload runs as root.
Faithful behavior (docker parity, needed by e.g.
`docker/sandbox-templates:opencode`, which runs as `agent`): the guest
agent resolves the user against the rootfs `/etc/passwd`/`/etc/group`, then
setgroups/setgid/setuid before spawning the workload, and sets
HOME/USER/LOGNAME. Also applies to exec sessions (docker exec runs as the
container user by default, `-u` overrides). The OpenCode image now pulls
successfully after hard-link replacement handling was added, but its VM
startup still stops before this identity behavior can be tested.

## 3. Credentials injection (design plan)
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

## 4. Console polish for `mvm run -it`
No winsize propagation to the guest console (exec -it has full resize),
and TERM isn't injected on the console path (`-e TERM=xterm-256color`
works around it). Agent-side: TIOCSWINSZ on the console + a resize
message keyed to the sandbox rather than an exec session.

## 5. Pull memory usage
Layer blobs are buffered fully in RAM during pull (fetch + sha256 +
unpack from `Vec<u8>`); a 300 MB layer means a 300 MB spike. Stream to a
temp file with incremental hashing instead — matters for images like
`docker/sandbox-templates:opencode` (~750 MB compressed).

---

## Done

- **E2E integration** — 21/21 on real KVM in rootless userns mode with
  the overlay driver (2026-08-01).
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
