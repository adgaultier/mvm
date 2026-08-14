
# Security Hardening & Production Sandbox Threat Model
 Security hardening: make mvm suitable for hostile-code / AI-agent sandboxing
## Establish and document a formal threat model:
Assume the guest controls root, arbitrary native code, syscalls, filesystem contents, and network traffic.
Assume OCI images may be malicious.
Assume API clients may be malicious or compromised.

>  The guest must not be able to escape the VM, access host files or credentials, affect other sandboxes, bypass network policy, or exhaust host resources.

---

## P0 — Secure the control plane
Add authentication and authorization to the HTTP API.
Prefer a Unix-domain socket with appropriate filesystem permissions for local operation.
Require authentication when binding TCP beyond loopback.
Reject or warn on --addr 0.0.0.0 / non-loopback listeners without an explicit authentication mechanism.
Introduce sandbox ownership/tenant identity and enforce authorization on every sandbox operation.
Define capabilities such as inspect, exec, start, stop, delete, mount, and network.
Ensure an authenticated API client cannot control another user's sandbox unless explicitly authorized.
####  VM-scoped authentication for Agent API

> **DONE (2026-08-13).** A separate `/agent/v1` listener (`--agent-addr` /
> `MVM_AGENT_ADDR`, default `127.0.0.1:24643`) authenticates every request with
> `Authorization: Bearer <token>`, resolved to `Principal::Vm(vm_id)` — the
> routes carry no `{id}`, so a VM can only act on itself (`inspect`/`stop`/
> `delegate`, the last still a stub). Token mechanics: 32 random bytes minted
> in `Manager::start`; only `agent_token_hash` (SHA-256) is kept in the
> manager's memory (`#[serde(skip)]`: not in API responses, not persisted),
> cleared on stop/exit. The plaintext is passed to the shim as a process env
> var and rides the `MVM_*` channel into the guest (readable via
> `/proc/cmdline`, deliberately not scrubbed so the workload's tooling can
> present it); it is never persisted. Regenerated on restart, revoked on
> stop/removal; the control plane never accepts it.

Provision each VM with a cryptographically random, VM-scoped bearer token for authenticating to the restricted Agent API.

- [x] Generate 32 random bytes when the VM is created.
- [x] Store only a hash of the token on the host, associated with the VM identity and expiry/revocation state.
- [x] Provision the plaintext token to the VM at startup without putting it on disk or in command-line arguments.
- [x] Authenticate Agent API requests with Authorization: Bearer <token>.
- [x] Resolve the token to Principal::Vm(vm_id), then apply the existing authorization/delegation rules.
- [x] Never accept this token on the privileged control-plane API.
- [x] Revoke the token when the VM is destroyed; generate a new token on restart.
- [x] Keep the token opaque: permissions/capabilities must come from the authorization system, not from the token itself.
### P0 — Resource governance / DoS protection
Add per-sandbox limits for:
- vCPUs
- memory
- disk/storage
- process count
- concurrent exec sessions
- log size
- image size
- image layer count
- filesystem expansion
Add host-wide aggregate budgets:
- maximum running sandboxes
- maximum allocated memory
- maximum allocated vCPUs
- maximum storage
- maximum concurrent image pulls
- maximum concurrent exec sessions

Fail closed when requested resources exceed policy.
Replace unbounded host-side queues with bounded queues/backpressure.
Add console log rotation or hard limits to prevent host-disk exhaustion.
### P0 — Credential isolation
Implement the credential-proxy architecture described in `security/TODO.CREDENTIALS.md`
Never expose long-lived host/API credentials directly to the guest.
Associate credentials with sandbox identity and explicit destination policy.
Enforce destination authorization after DNS/connection resolution rather than trusting guest-supplied hostnames alone.
Defend against:
- DNS rebinding
- redirects
- raw IP access
- IPv4/IPv6 bypasses
- alternate ports
- HTTP CONNECT
- proxy tunneling
- SNI/Host mismatches
- IDN/punycode ambiguity
- trailing-dot hostnames
- IPv4-mapped IPv6
- WebSockets/SSE
- HTTP/2 and HTTP/3 where applicable

Strip credentials on unauthorized redirects.
Audit credential use by sandbox identity and destination.

## P1 — Authenticate the guest-agent control channel
Do not trust a connection merely because it arrived on the expected vsock CID/port.
Generate a cryptographically random per-sandbox secret/capability during VM creation.
Establish an authenticated handshake between guest agent and host.
Bind authentication to sandbox identity and protocol version.
Accept only the first authenticated agent connection; reject unauthenticated or duplicate connections.
Add adversarial integration tests where the guest workload attempts to impersonate the agent.
### P1 — Harden host filesystem access
Treat host bind mounts as privileged capabilities.
Add configurable allowlisted host mount roots.
Reject arbitrary mounts such as /, /etc, /proc, /sys, /dev, other users' home directories, etc., unless explicitly authorized.
Resolve mount paths safely and prevent symlink/path-race escapes.
Use Linux openat2()/RESOLVE_BENEATH-style mechanisms where available.
Add race-condition tests for symlink, rename, hard-link, and mount-path attacks.
### P1 — Make image ingestion resource-bounded
Change OCI blob downloading from whole-layer buffering to true streaming into bounded temporary storage.
Enforce compressed and uncompressed image/layer size limits.
Enforce maximum layer count, file count, path length, and filesystem expansion.
Preserve digest verification while streaming.
Add cleanup guarantees for interrupted/failed pulls.
Fuzz the OCI/tar extractor with malicious:
- traversal paths
- symlinks
- hard links
- whiteouts
- opaque whiteouts
- duplicate paths
- PAX/GNU metadata
- device nodes
- FIFOs
- extreme expansion ratios
### P1 — Strict security profile
Add an explicit --security=strict mode intended for hostile workloads.
In strict mode:
- fail closed if required user namespaces are unavailable
- use no_new_privs
- drop unnecessary capabilities
- prefer non-root execution
- require resource limits
- restrict host mounts to explicit allowlists
- default to --net=none
- require authenticated guest-agent communication
- enable the strongest available seccomp policy

Keep a compatibility profile for ordinary container workloads that need root/setuid semantics.
Expose effective security properties through sandbox inspection/status so callers can verify what protections are actually active.

### P1 — Network isolation and egress policy
Clearly classify none, gvproxy, tap, and tsi by security guarantees.
Treat tsi as a reduced-isolation/host-network-like mode.
Add explicit per-sandbox egress policy.
Implement passt support where appropriate.
Prevent access to sensitive host/link-local destinations unless explicitly allowed.
Test:
- localhost
- RFC1918/private ranges
- link-local addresses
- IPv4/IPv6
- raw sockets
- DNS rebinding
- alternate ports
- proxy bypasses
- AF_VSOCK abuse

### P1 — Resource and lifecycle robustness
Test cleanup after:
- VM crash
- daemon crash
- SIGKILL
- host OOM
- filesystem-full conditions
- open-but-deleted files
- processes holding working directories
- failed overlay unmounts
- interrupted image extraction


Ensure resources are eventually reclaimed and cannot accumulate indefinitely.
Add startup/shutdown state-machine tests for every failure point.

## P2 — Guest syscall hardening
Keep the current raw-socket/AF_PACKET seccomp protections.
Evaluate additional restrictions for:
- keyctl
- bpf
- perf_event_open
- userfaultfd
- io_uring
- ptrace
- mount
- unshare
- setns
- module loading
- kexec
- other high-risk capabilities

> **PARTIALLY DONE (2026-08-14):** `--security=strict` (`mvm run --security=strict`,
> also on `create`/`clone`) installs a workload-scoped second seccomp filter
> (`build_strict` in `crates/agent/src/seccomp.rs`) denying `bpf`, `keyctl`,
> `perf_event_open`, `userfaultfd`, and the `io_uring_*` trio with `EPERM`,
> gated per-sandbox via `SandboxSpec.security` → `ShimConfig.security` →
> `MVM_SECURITY_STRICT`. Still open: ptrace, mount, unshare, setns, module
> loading, kexec, and the compatibility/security matrix below. The in-guest
> eBPF probe (`scripts/probes/bpfprobe.c`, run by `integration.sh`) reports
> whether the libkrunfw kernel could host a Phase 2 cgroup_skb/egress policy —
> that path stays gated on `progl=0` + `attach=0`.

Do not rely on seccomp as the primary isolation boundary; KVM remains the principal security boundary.
Add a compatibility/security matrix documenting which restrictions can safely be enabled.

# P2 — Virtiofs/libkrun attack-surface review
Treat the guest filesystem → virtiofs → libkrun → host filesystem path as a critical security boundary.

Audit/fuzz filesystem operations including:
- open
- rename
- unlink
- link
- symlink
- chmod/chown
- xattrs
- mmap
- truncate
- concurrent operations

Track and pin known-good libkrun versions.
Maintain an explicit supported kernel/libkrun/hypervisor matrix.
Security regression suite

Create a dedicated adversarial integration suite covering at minimum:
VM escape attempts
- /dev/kvm access
- virtiofs abuse
- vsock/agent impersonation
- host filesystem access
- mount/symlink races
- malicious OCI archives
- OCI decompression bombs
- resource exhaustion
- log flooding
- image-pull exhaustion
- network isolation bypasses
- DNS rebinding
- credential exfiltration attempts
- API authorization bypasses
- cross-sandbox access
- daemon crash/restart recovery


Run these tests against all supported Linux/macOS configurations and supported libkrun versions.
Add fuzzing targets for OCI extraction, protocol framing, API input/specification parsing, and guest-agent communication.


### Security documentation
Publish a formal threat model and security architecture.
Document what mvm guarantees and what it explicitly does not guarantee.
Document security implications of:
- root workloads
- host mounts
- tsi
- disabled user namespaces
- compatibility mode
- privileged networking

Add a security policy and vulnerability-reporting process.
Do not describe mvm as a production hostile-code sandbox until the above controls have been implemented and validated.

### Acceptance criteria
- No unauthenticated remote control of the daemon.
- Sandbox ownership/authorization enforced.
- Host-wide and per-sandbox resource limits enforced.
- No long-lived credentials exposed to guests.
- Guest-agent channel authenticated.
- Host mounts explicitly policy-controlled.
- Image ingestion bounded in memory and storage.
- Strict security mode fails closed when required guarantees are unavailable.
- Adversarial security suite passes consistently.
- Security properties are observable and testable through the API/CLI.
- Publish a security review before declaring production hostile-code/AI-agent sandbox support.
