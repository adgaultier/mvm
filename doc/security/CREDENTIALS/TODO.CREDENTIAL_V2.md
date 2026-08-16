# MVM Credential Injection — Proposed Architecture
## 1. Goal and security invariant

Credential injection must be transparent to applications: no HTTP_PROXY, special SDK, custom endpoint, or application changes.

The guest application uses normal HTTPS and a sentinel credential such as:

OPENAI_API_KEY=mvm-managed

The actual authorization decision is host-side:

(VM identity, destination, credential, auth scheme)

The guest never receives the real credential.

## 2. Transparent interception, not explicit proxying

Traffic should normally look like:

Application → normal TCP/HTTPS → MVM networking → Internet

but MVM transparently redirects eligible traffic to a host-side Rust proxy:

VM → eBPF interception/enforcement → MVM TLS/L7 proxy → Internet

The application does not know that interception is happening.

## 3. Split responsibilities: eBPF vs Rust

Use eBPF for network enforcement and flow identity, not for TLS or HTTP:

associate traffic with a VM identity
prevent direct/bypass egress
redirect TCP flows to the proxy
preserve original destination metadata
optionally expose flow/DNS information

Use the Rust userspace proxy for:

TLS termination
HTTP parsing
destination validation
policy evaluation
credential lookup
credential injection
upstream TLS

Do not implement TLS/HTTP/secret injection in eBPF.

## 4. TLS interception and certificates

The proxy terminates two TLS connections:

VM ← TLS 1 → MVM proxy ← TLS 2 → api.example.com

MVM creates an ephemeral CA per VM lifetime. The VM receives only the CA public certificate; the CA private key stays on the host.

For api.example.com, the proxy dynamically creates/caches a leaf certificate:

SAN = api.example.com
Issuer = MVM VM-specific CA

Cache leaf certificates per (VM, hostname) for the VM lifetime. Destroy the CA and cache when the VM is destroyed.

The CA must be installed into the guest's normal Linux trust store during VM setup.

Applications using custom trust stores or incompatible certificate pinning may not support transparent TLS interception; fail closed for credential injection rather than bypassing the policy.

## 5. Destination identity

Credential injection requires a trustworthy destination identity.

Prefer combining:

original destination from the intercepted flow
VM-controlled DNS mapping (hostname → IP)
TLS SNI when available

Do not blindly authorize based on IP reverse DNS or a guest-supplied hostname.

If the destination cannot be established reliably, allow ordinary networking only if policy permits, but do not inject a hostname-bound credential.

DNS rebinding and private-address targets must be handled explicitly.

## 6. VM identity must be host-derived

Never trust the guest to assert:

X-MVM-VM-ID: vm-123

MVM already has a strong architectural primitive: one libkrun shim process per VM.

Bind the shim/cgroup/network context to the MVM VM identity and let the kernel/host networking path establish:

network flow → VM identity

This is especially promising for TSI, where the shim performs host-side socket operations, and for virtio-net/gvproxy where the guest networking boundary can be enforced separately.

## 7. Policy and credential broker

Keep credential selection separate from transport interception:

Egress flow
  → VM identity
  → destination normalization
  → PolicyEngine
  → InjectionCapability
  → CredentialBroker
  → HTTP authenticator

The guest chooses a destination, never a credential ID.

Conceptually:

credentials:
  openai:
    source: env:OPENAI_API_KEY
    destinations:
      - api.openai.com:443
    auth:
      type: bearer

The sentinel environment variable is only a compatibility/UX mechanism; it must never authorize access by itself.

## 8. eBPF/networking implementation strategy

Treat networking as an abstraction with backend-specific interception:

EgressController
├── TSI backend
└── virtio-net/gvproxy backend

Both feed the same policy/proxy layer.

For eBPF, investigate:

cgroup/connect*
socket/cgroup identity
sockops
tc
transparent redirection/TProxy mechanisms

Preserve enough metadata to recover:

VM identity + original destination + connection

Do not force TSI and virtio-net/gvproxy to share identical interception mechanics; first prove the strongest path independently.

## ## 9. Fail-closed security properties

The implementation must guarantee:

VM cannot bypass the proxy to obtain a managed credential.
Guest cannot select arbitrary credentials.
Real credentials never enter the VM.
Redirected flows are re-authorized.
Raw IP access does not automatically qualify for hostname-bound credentials.
Duplicate/conflicting guest Authorization headers are not merged with managed credentials.
Credentials and CA private keys never appear in logs/errors.
Unknown/unverifiable destination identity means no credential injection.
Clones/forks receive a new VM identity and new ephemeral CA; they do not inherit the source VM identity.
10. Recommended implementation order
Build the Rust TLS/L7 proxy independently of traffic interception.
Implement per-VM ephemeral CA + leaf certificate cache + guest trust-store installation.
Implement policy engine, credential broker, and destination-bound authorization.
Prototype eBPF interception with one networking mode and prove VM identity + original-destination recovery.
Add hard direct-egress blocking so interception is an enforcement boundary, not a convention.
Add DNS/hostname binding and rebinding protections.
Add the second networking backend (TSI or virtio-net/gvproxy) behind the same EgressController abstraction.

Recommended end state:

Transparent eBPF-assisted interception
              +
Host-side Rust TLS/L7 proxy
              +
Host-derived VM identity
              +
Destination-bound credential policy

This preserves application compatibility while keeping the real credential and authorization decision entirely outside the VM.
