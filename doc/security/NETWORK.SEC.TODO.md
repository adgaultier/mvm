# MVM Network Security Architecture
```
MVM Network Security
│
├── Guest network enforcement
│   ├── seccomp
│   └── eBPF
│
├── VM network isolation
│   └── passt / gvproxy
│
├── Host egress enforcement
│
├── Flow identity
│
├── Destination identity
│
├── DNS / IP / routing security
│
├── Transparent L4/L7 proxy
│
├── TLS interception
│
├── HTTP policy
│
└── Credential authorization & injection
    ├── PolicyEngine
    ├── CredentialBroker
    └── CredentialInjector
```

>  TSI mode is not considered safe , but kept as possile net backend


## 1. Goal and Security Invariant

MVM provides transparent, host-controlled credential injection for applications
running inside microVMs.

The workload uses ordinary HTTPS. It does not configure `HTTP_PROXY`, use a
special SDK, or change its endpoint.

Example:

```text
OPENAI_API_KEY=mvm-managed
```

The real credential exists only on the host.

The fundamental security invariant is:

> **A compromised workload userspace may originate network requests, but it
> cannot select, read, or modify the credentials used for those requests.**

Every egress flow is subject to host-side VM identity and network enforcement.
Credential injection occurs only after the host establishes:

```text
(VM identity, destination identity, request) → authorization decision
```

The guest can choose a destination and request contents, but never a
credential identity.

Credential injection is therefore an **authorization decision**, not a header
replacement feature.

---

## 2. Threat Model

### 2.1 Trusted components

The following are part of the trusted computing base:

* MVM host
* MVM lifecycle manager
* host-side networking
* `passt`
* `gvproxy`
* guest kernel
* MVM-installed eBPF enforcement programs
* security-critical eBPF maps/state
* host-side TLS/L7 proxy
* policy engine
* credential broker/store

### 2.2 Untrusted components

The workload userspace is considered fully compromised.

An attacker may:

* execute arbitrary processes;
* create arbitrary normal sockets;
* issue arbitrary TCP/UDP requests permitted by guest policy;
* control HTTP requests;
* control TLS ClientHello/SNI;
* control DNS requests;
* attempt direct-IP access;
* attempt redirects;
* attempt DNS rebinding;
* attempt to manipulate HTTP authentication headers;
* attempt to exhaust networking/proxy resources.

### 2.3 Out of scope

A compromised guest kernel is outside the threat model.

The guest kernel is trusted because MVM establishes the network enforcement
before launching untrusted workload userspace.

### 2.4 Guest eBPF trust boundary

MVM loads the required eBPF programs before launching the workload.

The workload is then prevented from acquiring the capabilities required to:

* unload eBPF programs;
* detach enforcement programs;
* replace enforcement programs;
* modify security-critical BPF maps;
* otherwise disable or weaken network enforcement.

The sequence must be:

```text
guest boot
    ↓
network setup
    ↓
eBPF enforcement installation
    ↓
verify enforcement
    ↓
security hardening / capability drop
    ↓
launch workload
```

Failure to establish enforcement must prevent workload startup.

---

# 3. Security Architecture

MVM uses multiple independent enforcement layers:

```text
                   UNTRUSTED WORKLOAD
                           │
                           ▼
                 ┌───────────────────┐
                 │ Guest kernel      │
                 │                   │
                 │ seccomp           │
                 │ eBPF net policy   │
                 └─────────┬─────────┘
                           │
                       virtio-net
                           │
                           ▼
                 ┌───────────────────┐
                 │ passt / gvproxy   │
                 │                   │
                 │ VM network        │
                 │ boundary          │
                 └─────────┬─────────┘
                           │
                           ▼
                 ┌───────────────────┐
                 │ Host enforcement  │
                 │                   │
                 │ flow → VM         │
                 │ direct-egress     │
                 │ enforcement       │
                 └─────────┬─────────┘
                           │
                           ▼
                 ┌───────────────────┐
                 │ TLS/L7 Proxy      │
                 │                   │
                 │ TLS termination   │
                 │ HTTP parsing      │
                 │ destination       │
                 │ authorization     │
                 └─────────┬─────────┘
                           │
                           ▼
                 ┌───────────────────┐
                 │ CredentialBroker  │
                 └─────────┬─────────┘
                           │
                           ▼
                        Internet
```

The layers have deliberately different responsibilities.

### Guest eBPF

Answers:

> Can this workload create/send this class of network flow?

### `passt` / `gvproxy`

Provides:

> The VM's host-controlled network boundary.

### Host enforcement

Answers:

> Which VM owns this flow, and can this flow bypass the controlled path?

### L7 proxy

Answers:

> What destination and HTTP request is this, and is credential use
> authorized?

No layer should trust guest-supplied identity or credential identifiers.

---

# 4. Network Architecture

MVM supports the `passt` / `gvproxy` networking architecture.

Each VM has a host-created networking instance whose lifetime is bound to the
VM identity.

Conceptually:

```text
VM-123
   │
   │ virtio-net
   ▼
passt/gvproxy instance A
   │
   ▼
host networking
```

and:

```text
VM-456
   │
   │ virtio-net
   ▼
passt/gvproxy instance B
   │
   ▼
host networking
```

The host maintains an immutable mapping:

```text
network instance → VM identity
```

For example:

```text
passt-123 → vm-123
passt-456 → vm-456
```

The workload cannot modify this mapping.

Source IP, SNI, HTTP headers, or guest-presented tokens must not be the root of
VM identity.

vsock is reserved for the MVM control plane. Guest Internet traffic MUST use the virtio-net data plane and MUST NOT use vsock as an alternate network path.

---

# 5. Guest eBPF Network Enforcement

Guest eBPF is a trusted network enforcement layer against compromised workload userspace. It complements the existing guestd seccomp-BPF enforcement: seccomp restricts dangerous kernel/network interfaces such as raw sockets and AF_PACKET, while eBPF enforces the permitted network path and transport policy.

Guest eBPF is not responsible for TLS, HTTP, credential selection, or secret management.




## Responsibilities

* [ ] enforce allowed socket/network operations;
* [ ] prevent prohibited raw networking;
* [ ] prevent `AF_PACKET`;
* [ ] prevent IPv4/IPv6 raw sockets;
* [ ] prevent `AF_ALG`;
* [ ] prevent `AF_VSOCK` unless explicitly required;
* [ ] constrain outbound TCP/UDP flows;
* [ ] enforce allowed destination classes where practical;
* [ ] prevent alternate guest network paths;
* [ ] prevent workload modification of enforcement state;
* [ ] establish enforcement before workload startup;
* [ ] fail closed if enforcement cannot be established;
* [ ] fail closed if enforcement is lost during VM lifetime.

The eBPF layer should enforce **transport/network policy**, not hostname-bound
credential policy.

Avoid putting credential identifiers or secrets into eBPF.

---

# 6. eBPF Lifecycle and Immutability

The startup sequence must guarantee that untrusted userspace never runs before
network enforcement is active.

```text
VM boot
  ↓
guest network initialization
  ↓
MVM loads eBPF programs
  ↓
MVM initializes security-critical maps
  ↓
MVM verifies program attachment
  ↓
MVM verifies map ownership/access
  ↓
PR_SET_NO_NEW_PRIVS
  ↓
drop unnecessary capabilities
  ↓
launch workload
```

The workload must not retain privileges capable of:

* loading BPF;
* unloading BPF;
* replacing BPF programs;
* detaching enforcement;
* modifying security-critical maps.

Security-critical BPF maps must be treated as trusted state.

A trusted program with attacker-controlled policy maps is not a trusted
security boundary.

---

# 7. Host Network Enforcement

Guest eBPF is not the final network security boundary. The host must independently ensure that every guest flow reaches the controlled egress path.

The desired path is:
```
VM
 │
 ▼
guest eBPF
 │
 ▼
virtio-net
 │
 ▼
passt/gvproxy
 │
 ▼
host network enforcement
 │
 ▼
transparent L7 proxy
 │
 ▼
Internet
```

The host maintains the trusted mapping between each network instance and VM identity. Direct Internet access outside this path must be denied.

The host enforcement layer must ensure:

every Internet-bound flow has a host-derived VM identity;
IPv4 and IPv6 have equivalent enforcement;
alternate routes/interfaces cannot bypass enforcement;
VM-to-VM and host/private-network access are explicitly controlled;
DNS cannot provide an independent egress bypass;
UDP/443 cannot bypass credential interception through QUIC;
failure of enforcement results in denied networking, not unrestricted networking.

The host proxy is responsible for policy requiring L7 visibility: destination identity, URL/method/path policy, and credential authorization/injection.

## Required properties

* [ ] Every Internet-bound flow has a host-derived VM identity.
* [ ] Direct host/network bypass is denied.
* [ ] IPv4 and IPv6 have equivalent enforcement.
* [ ] TCP and UDP have explicit policy.
* [ ] UDP/443 cannot bypass credential interception through QUIC.
* [ ] Alternate routes cannot bypass enforcement.
* [ ] Alternate network interfaces cannot bypass enforcement.
* [ ] Host/private-network access is separately controlled.
* [ ] VM-to-VM traffic is denied unless explicitly authorized.
* [ ] Host management services are not implicitly exposed.
* [ ] DNS cannot provide an independent egress bypass.
* [ ] Proxy access is explicitly permitted.
* [ ] Failure of enforcement results in denied networking, not unrestricted
  networking.

---

# 8. Flow Identity

The L7 proxy must receive a trusted VM identity from host-side networking.

The guest never presents its identity to the proxy.

Conceptually:

```text
guest flow
    ↓
passt/gvproxy network instance
    ↓
MVM-owned mapping
    ↓
VM identity
```

Example:

```rust
struct FlowIdentity {
    vm_id: VmId,
    flow_id: FlowId,
}
```

The proxy should consume:

```text
FlowId → VmId
```

as a trusted interface.

It must not derive VM identity from:

* HTTP headers;
* TLS SNI;
* HTTP `Host`;
* source-controlled identity headers;
* guest-presented bearer tokens;
* credential sentinel values;
* arbitrary guest metadata.

---

# 9. Guest Agent Token

MVM's existing `MVM_GUEST_TOKEN` remains an authentication mechanism for
guest-initiated host services.

It is not the proxy's identity mechanism.

The token:

* is minted per VM;
* is regenerated for new VM identities;
* is revoked when the VM stops/exits;
* is not persisted;
* is represented host-side by a SHA-256 hash;
* is not accepted by the transparent proxy;
* does not select credentials.

The security rule is:

> **The token authenticates a guest to a host service; it never authorizes
> credential selection.**

Even a guest that possesses its own token cannot select another credential.

---

# 10. Destination Identity

Credential injection requires a trustworthy destination identity.

The proxy should construct an explicit destination object:

```rust
struct DestinationIdentity {
    original_ip: IpAddr,
    original_port: u16,

    tls_sni: Option<Hostname>,

    host_resolution: Option<ResolutionObservation>,

    validated_hostname: Option<Hostname>,
}
```

Potential evidence includes:

* original intercepted destination IP;
* original destination port;
* TLS SNI;
* HTTP/1.1 `Host`;
* HTTP/2 `:authority`;
* host-controlled DNS resolution;
* upstream TLS certificate validation.

These sources are not equally trustworthy.

---

# 11. SNI Is Not Destination Authentication

SNI is guest-controlled input.

This:

```text
connect 1.2.3.4:443
SNI: api.example.com
```

does not by itself prove that `1.2.3.4` is the legitimate address for
`api.example.com`.

Credential authorization should require consistency between:

```text
original destination
+
SNI/HTTP authority
+
host-controlled resolution
+
upstream certificate validation
```

where required by policy.

The proxy must never blindly authorize based on SNI alone.

---

# 12. DNS

Guest DNS observations may be used as evidence but must not be the sole
authority for hostname-bound credentials.

Prefer host-controlled resolution:

```text
authorized hostname
       ↓
host DNS resolution
       ↓
validated IP
       ↓
upstream connection
```

The proxy must explicitly handle:

* DNS rebinding;
* CNAME chains;
* multiple A/AAAA records;
* IPv4/IPv6;
* stale DNS observations;
* private-address results;
* loopback results;
* link-local addresses;
* multicast addresses;
* unspecified addresses.

A credential-bound hostname must not unexpectedly resolve to a prohibited
private or local address.

---

# 13. Private and Local Address Protection

Hostname authorization must not implicitly grant SSRF capability.

After every relevant resolution step, validate the resulting IP.

Explicitly consider:

```text
127.0.0.0/8
10.0.0.0/8
172.16.0.0/12
192.168.0.0/16
169.254.0.0/16

::1
fc00::/7
fe80::/10
```

and other special-use address ranges as appropriate.

Policy may explicitly allow private destinations, but that must be an explicit
decision rather than an accidental consequence of hostname matching.

---

# 14. Hostname Normalization

Destination matching must:

* [ ] normalize case;
* [ ] normalize trailing dots;
* [ ] handle IDNA/punycode consistently;
* [ ] distinguish exact names from subdomains;
* [ ] prevent `evil-example.com` matching `example.com`;
* [ ] prevent `example.com.attacker.com` matching `example.com`;
* [ ] handle IPv4/IPv6 consistently;
* [ ] match ports where required;
* [ ] prefer exact FQDN policies;
* [ ] make wildcard semantics explicit.

For example:

```text
example.com
```

must not match:

```text
evil-example.com
```

or:

```text
example.com.attacker.com
```

---

# 15. Transparent TLS Proxy

The proxy terminates two independent TLS connections:

```text
VM
 │
 │ TLS 1
 ▼
MVM TLS proxy
 │
 │ TLS 2
 ▼
destination
```

The VM sees:

```text
SAN = api.example.com
Issuer = MVM VM-specific CA
```

The upstream connection independently performs normal TLS server
authentication.

---

# 16. Per-VM Ephemeral CA

Each VM receives a unique ephemeral CA certificate.

The CA private key remains host-side.

Lifecycle:

```text
VM creation
    ↓
generate VM identity
    ↓
generate ephemeral CA
    ↓
install CA certificate into guest
    ↓
launch VM
```

Leaf certificates are generated/cached per:

```text
(VM identity, hostname)
```

for the VM lifetime.

The CA private key and leaf private keys must never be exposed to the guest.

---

# 17. CA Lifecycle and Snapshots

Define VM lifecycle precisely.

A new VM identity must receive:

* a new agent token;
* a new credential authorization context;
* a new ephemeral CA;
* a new network identity.

Clones/forks must not inherit the source VM's security identity.

Snapshot/restore behavior must explicitly prevent:

* reuse of an old VM identity;
* reuse of an old authorization context;
* reuse of host-side CA private keys;
* credential state being embedded into a guest snapshot.

A guest snapshot may contain the public CA certificate, but must never contain
the corresponding private key.

---

# 18. TLS Compatibility

Applications using:

* custom trust stores;
* certificate pinning;
* custom TLS verification;
* incompatible TLS implementations

may not support transparent interception.

Credential injection must fail closed when TLS interception cannot be
performed.

Never silently fall back to:

```text
VM → direct Internet
```

when credential interception fails.

---

# 19. Transparent L7 Proxy

The egress proxy is transparent to the workload. The workload uses ordinary TCP/TLS/HTTP and does not configure an HTTP proxy or use a special SDK.

The proxy terminates the guest-side TLS connection and establishes an independent upstream TLS connection.

It must support:

* HTTP/1.1;
* HTTP/2;
* HTTP `Host`;
* HTTP/2 `:authority`;
* request bodies and streaming;
* connection reuse.


The host proxy is responsible for policy requiring L7 visibility:
* destination identity, URL/method/path policy, and credential
* authorization/injection. Host-side network enforcement remains responsible
* for ensuring that traffic cannot bypass the proxy.


HTTP/3/QUIC is initially outside the credential-injection path; UDP/443 is therefore denied as specified in §20.

---

# 20. QUIC / UDP

Because `passt` can provide UDP connectivity, UDP/443 is a potential bypass
of a TCP/TLS interception architecture.

Therefore the initial policy should be:

```text
TCP/443 → transparent TLS/L7 proxy

UDP/443 → DENY
```

unless and until MVM implements an explicit QUIC-aware credential architecture.

This is a security requirement, not merely an implementation limitation.

Otherwise:

```text
TCP/443 → protected
UDP/443 → Internet
```

would invalidate the intended egress invariant.

---

# 21. Credential Policy

Credential selection is a host-side authorization decision, separate from transport interception.

The logical pipeline is:

Egress flow
    ↓
VM identity
    ↓
destination identity
    ↓
request parsing
    ↓
network/request policy
    ↓
AuthorizationEngine
    ↓
opaque CredentialHandle
    ↓
CredentialBroker
    ↓
CredentialInjector


The guest may indicate that managed authentication is expected, but it never provides a credential reference or selects a credential.

A credential is authorized for an explicit destination and may additionally be constrained by HTTP method and path.

Destination changes, redirects, and cross-origin requests require independent authorization.

If destination identity or authorization cannot be established, credential injection fails closed.

---

# 22. Credential Capability

Do not allow request-derived strings to directly become credential-store lookup
keys.

Avoid:

```rust
credential_broker.get(request.credential_ref)
```

Instead:

```rust
enum AuthorizationDecision {
    Deny,

    AllowNoCredential,

    AllowCredential {
        credential: CredentialHandle,
        constraints: RequestConstraints,
    },
}
```

Only the trusted authorization engine can produce a `CredentialHandle`.

The credential broker accepts the opaque handle, not a guest-supplied string.

This makes it structurally difficult to turn:

```text
guest → credential_ref
```

into:

```text
credential_ref → secret
```

---

# 23. Credential Policy Example

```yaml
credentials:
  openai-prod:
    source: env:OPENAI_API_KEY

    destinations:
      - api.openai.com:443

    auth:
      type: bearer
```

A stronger policy may additionally constrain requests:

```yaml
credentials:
  openai-prod:
    destinations:
      - api.openai.com:443

    requests:
      methods:
        - POST

      paths:
        - /v1/responses
        - /v1/chat/completions

    auth:
      type: bearer
```

Whether path/method restrictions are required is a product decision, but the
authorization model should permit them.

---

# 24. Sentinel Credentials

The sentinel is a compatibility and intent mechanism.

Example:

```text
OPENAI_API_KEY=mvm-managed
```

The sentinel may result in the application producing:

```http
Authorization: Bearer mvm-managed
```

The proxy can recognize the sentinel and replace it with the authorized
credential.

The security rule is:

```text
sentinel = request intent
policy = authorization
```

Never:

```text
sentinel = authorization
```

A request containing the sentinel does not authorize access to a credential
by itself.

---

# 25. Credential Injection

The credential injector owns the final authentication state.

For managed authentication:

```text
Guest:
Authorization: Bearer mvm-managed

Proxy:
Authorization: Bearer <REAL_SECRET>
```

The proxy must:

* [ ] remove/overwrite guest-supplied managed authentication;
* [ ] never merge guest and managed credentials;
* [ ] support Bearer authentication;
* [ ] support API-key headers;
* [ ] optionally support Basic authentication;
* [ ] make authentication deterministic;
* [ ] never log credentials;
* [ ] never expose credentials in errors.

The injector should own all headers relevant to the selected authentication
scheme.

Examples include:

```text
Authorization
Proxy-Authorization
X-API-Key
```

where applicable.

---

# 26. Redirects

Every destination change is a new authorization decision.

Example:

```text
api.example.com
      │
      │ 302
      ▼
attacker.example.com
```

The managed credential must not automatically follow the redirect.

Instead:

```text
A
 ↓
redirect to B
 ↓
new destination authorization
 ↓
credential for B, if explicitly authorized
```

The proxy must:

* [ ] strip managed authentication across origins;
* [ ] re-evaluate destination policy;
* [ ] revalidate DNS;
* [ ] revalidate destination IP;
* [ ] revalidate TLS;
* [ ] prevent credential forwarding to unauthorized hosts.

This applies to destination changes generally, not only HTTP redirects.

---

# 27. Cross-Origin Credential Protection

Any new TLS origin must trigger a new authorization decision.

Do not assume that because:

```text
api.example.com
```

was authorized, a subsequent:

```text
cdn.example.com
```

is also authorized.

The authorization boundary is the destination identity.

---

# 28. Credential Confidentiality

Real credentials must never enter the VM.

They must never appear in:

* logs;
* tracing;
* metrics;
* panic messages;
* proxy errors;
* request dumps;
* debug output;
* crash dumps;
* temporary files;
* cache files;
* guest-visible responses.

Use a dedicated secret type that does not implement accidental `Debug` or
`Display`.

For example:

```rust
struct SecretString(/* private */);
```

The credential broker should expose secrets only for the minimum lifetime
required by the injection operation.

---

# 29. Credential Rotation

Credentials should have explicit versions/generations.

Conceptually:

```text
openai-prod:v17
```

Authorization may produce:

```rust
struct CredentialHandle {
    credential_id: CredentialId,
    generation: CredentialGeneration,
}
```

Define behavior for:

* rotation;
* expiration;
* revocation;
* in-flight requests;
* long-lived streams;
* policy changes.

Avoid races where a request is authorized against one credential generation but
later retrieves an unintended generation.

---

# 30. Resource Isolation

A compromised VM can intentionally attack the proxy's resources.

Per-VM limits should exist for:

* [ ] concurrent connections;
* [ ] TLS handshakes;
* [ ] certificate generation;
* [ ] certificate cache entries;
* [ ] HTTP request size;
* [ ] header size;
* [ ] HTTP/2 streams;
* [ ] upstream connections;
* [ ] connection lifetime where appropriate;
* [ ] request rate.

One VM must not be able to exhaust resources required by another VM.

---

# 31. Proxy Control Plane Isolation

Separate the proxy data plane from management/control APIs.

The workload must not be able to access administrative endpoints such as:

```text
/debug
/config
/reload
/credentials
/policy
/metrics
```

unless explicitly intended.

The guest should only interact with the transparent data-plane path.

---

# 32. Proxy Compromise

The L7 proxy is a high-value trusted component.

A proxy compromise must be treated as equivalent to compromise of the
credential broker for credentials accessible to that proxy.

The architecture must therefore minimize:

* credential lifetime in memory;
* credential visibility;
* logging;
* administrative surface;
* unnecessary filesystem access;
* unnecessary network access.

The proxy should not have access to credentials it is not authorized to use.

---

# 33. Security Properties

## Property 1 — Guest cannot select credentials

```text
Guest → destination/request
Proxy → credential
```

Never:

```text
Guest → destination + credential_ref
```

---

## Property 2 — Credential is destination-bound

```text
credential A → api.example.com:443
```

not:

```text
credential A → arbitrary HTTPS destination
```

---

## Property 3 — Destination changes require reauthorization

```text
A → B
```

requires:

```text
authorize(B)
```

---

## Property 4 — Real credentials never enter the VM

```text
VM memory
VM filesystem
VM environment
VM logs
VM responses
```

must never contain the real credential.

The sentinel is not the real credential.

---

## Property 5 — VM identity is host-derived

The proxy must never trust:

```text
X-MVM-VM-ID
```

or equivalent guest-provided identity.

Identity comes from the host-controlled network boundary.

---

## Property 6 — Guest userspace cannot disable eBPF enforcement

Because eBPF is installed before workload startup and workload capabilities
are subsequently dropped, compromised userspace cannot remove or replace the
enforcement layer.

---

## Property 7 — Direct egress is impossible

The VM cannot obtain Internet access through an alternate path that avoids
host enforcement.

This includes:

* IPv4;
* IPv6;
* TCP;
* UDP;
* QUIC;
* alternate interfaces;
* alternate routes;
* raw packet paths.

---

## Property 8 — Unknown destination means no credential

If destination identity cannot be established reliably:

```text
credential injection = DENY
```

Ordinary non-credentialed networking may be allowed only if explicitly
permitted by policy.

---

## Property 9 — Credential authorization is host-side

The credential decision depends on trusted host-side state:

```text
VM identity
+
validated destination
+
request
+
policy
```

The guest does not participate in credential selection.

---

# 34. Recommended Rust Components

```text
ProxyServer
├── FlowIdentityResolver
├── DestinationNormalizer
├── DestinationResolver
├── AuthorizationEngine
├── InjectionCapability
├── CredentialBroker
├── CredentialStore
├── CredentialInjector
├── UpstreamClient
├── RequestPolicy
└── VmCertificateAuthority
    └── LeafCertificateCache
```

Suggested crates:

```text
tokio
hyper
rustls
http
```

Potential additional components may be introduced as implementation requires,
but security-sensitive responsibilities should remain narrowly separated.

---

# 35. Suggested Core Data Model

```rust
struct FlowIdentity {
    vm_id: VmId,
    flow_id: FlowId,
}

struct DestinationIdentity {
    original_ip: IpAddr,
    original_port: u16,
    tls_sni: Option<Hostname>,
    validated_hostname: Option<Hostname>,
}

struct RequestContext {
    flow: FlowIdentity,
    destination: DestinationIdentity,
    request: HttpRequestMetadata,
}

enum AuthorizationDecision {
    Deny,

    AllowNoCredential,

    AllowCredential {
        credential: CredentialHandle,
        constraints: RequestConstraints,
    },
}
```

The critical invariant is:

```text
CredentialHandle
```

can only be produced by trusted authorization logic.

---

# 36. Network Enforcement Abstraction

MVM currently targets `passt` / `gvproxy`.

The security abstraction should be minimal:

```rust
trait FlowIdentityResolver {
    fn resolve(&self, flow: FlowId) -> Result<VmId>;
}
```

The L7 proxy does not need to understand guest networking internals.

The networking layer provides:

```text
flow → VM identity
```

and:

```text
flow → original destination
```

The initial implementation can remain specific to the actual MVM networking
architecture rather than prematurely supporting unrelated backends.

---

# 37. Recommended Implementation Order

## Phase 1 — Guest eBPF enforcement

Prove:

```text
workload
    ↓
guest kernel/eBPF
    ↓
only permitted networking
```

Test with fully compromised userspace.

Do not add credentials yet.

---

## Phase 2 — passt/gvproxy identity boundary

Prove:

```text
VM
 ↓
network instance
 ↓
host flow
 ↓
VM identity
```

Verify that one VM cannot impersonate another.

---

## Phase 3 — Host direct-egress enforcement

Prove:

```text
VM → Internet
```

is impossible except through the controlled path.

Test:

* IPv4;
* IPv6;
* TCP;
* UDP;
* direct IP;
* alternate routes;
* alternate interfaces;
* QUIC;
* private addresses.

---

## Phase 4 — Transparent TLS proxy

Implement:

```text
VM TLS
   ↓
per-VM CA
   ↓
proxy
   ↓
upstream TLS
```

without credential injection.

Test:

* TLS 1.2;
* TLS 1.3;
* HTTP/1.1;
* HTTP/2;
* SNI;
* certificate validation;
* custom trust stores;
* certificate pinning;
* connection reuse.

---

## Phase 5 — Destination identity

Implement:

```text
original IP
+
port
+
SNI
+
host-controlled DNS
+
upstream certificate validation
```

and prove hostile combinations cannot obtain a credential.

---

## Phase 6 — Authorization engine

Implement:

```text
VM identity
+
destination
+
request
    ↓
AuthorizationDecision
```

with no credential lookup based directly on guest input.

---

## Phase 7 — Credential broker

Implement opaque:

```text
CredentialHandle
```

and credential retrieval.

Add:

* rotation;
* expiration;
* revocation;
* generation tracking.

---

## Phase 8 — Credential injection

Implement sentinel replacement and managed authentication.

At this point injection should be a relatively small component because the
security decisions already exist elsewhere.

---

## Phase 9 — Adversarial integration testing

Run the complete suite from a fully compromised workload userspace.

---

# 38. Security Testing

## Guest enforcement

* [ ] Normal TCP works.
* [ ] Normal UDP follows policy.
* [ ] `SOCK_RAW` fails.
* [ ] `AF_PACKET` fails.
* [ ] `AF_ALG` fails.
* [ ] unauthorized socket operations fail.
* [ ] workload cannot detach eBPF.
* [ ] workload cannot replace eBPF.
* [ ] workload cannot modify security-critical BPF maps.
* [ ] enforcement is active before workload startup.
* [ ] enforcement loss fails closed.

## Identity

* [ ] VM A cannot become VM B.
* [ ] VM cannot spoof VM identity.
* [ ] cloned VM gets a new identity.
* [ ] destroyed VM identity cannot be reused accidentally.
* [ ] guest headers cannot influence identity.
* [ ] guest token cannot influence transparent proxy identity.

## Network bypass

* [ ] direct IPv4 TCP fails.
* [ ] direct IPv6 TCP fails.
* [ ] direct UDP fails where prohibited.
* [ ] UDP/443 cannot bypass credential interception.
* [ ] QUIC cannot bypass policy.
* [ ] raw packets fail.
* [ ] alternate routes fail.
* [ ] alternate interfaces fail.
* [ ] host-loopback access follows explicit policy.
* [ ] VM-to-VM access follows explicit policy.
* [ ] DNS bypass cannot create unrestricted egress.

## Destination attacks

* [ ] SNI mismatch.
* [ ] HTTP `Host` mismatch.
* [ ] HTTP/2 `:authority` mismatch.
* [ ] direct IP access.
* [ ] DNS rebinding.
* [ ] CNAME to private IP.
* [ ] IPv6 rebinding.
* [ ] private destination.
* [ ] loopback destination.
* [ ] link-local destination.
* [ ] trailing-dot hostname.
* [ ] hostname case normalization.
* [ ] IDN/punycode.
* [ ] wildcard boundary.
* [ ] unauthorized port.

## Credential attacks

* [ ] guest cannot specify credential ID.
* [ ] guest cannot select another VM's credential.
* [ ] sentinel alone cannot authorize a credential.
* [ ] guest `Authorization` cannot override managed authentication.
* [ ] duplicate authentication headers are handled safely.
* [ ] header casing cannot bypass injection policy.
* [ ] API-key headers are handled safely.
* [ ] Basic authentication is handled safely if enabled.
* [ ] redirect cannot leak credentials.
* [ ] cross-origin request cannot inherit credentials.
* [ ] DNS rebinding cannot leak credentials.
* [ ] wrong destination cannot receive credential.
* [ ] credential rotation works.
* [ ] expired credentials are rejected.
* [ ] revoked credentials are rejected.

## Confidentiality

* [ ] credentials never enter guest memory.
* [ ] credentials never enter guest filesystem.
* [ ] credentials never appear in logs.
* [ ] credentials never appear in proxy errors.
* [ ] credentials never appear in metrics.
* [ ] credentials never appear in traces.
* [ ] CA private keys never enter the guest.
* [ ] CA private keys never enter snapshots.

## Resource isolation

* [ ] VM A cannot exhaust proxy connections for VM B.
* [ ] VM A cannot exhaust TLS handshakes for VM B.
* [ ] certificate-cache limits are enforced.
* [ ] request-size limits are enforced.
* [ ] HTTP/2 stream limits are enforced.
* [ ] upstream connection limits are enforced.

---

# 39. Final Architecture

The recommended MVM architecture is:

```text
                     MVM HOST
┌─────────────────────────────────────────────────────────┐
│                                                         │
│                     L7 Proxy                            │
│                         │                               │
│                         ▼                               │
│                AuthorizationEngine                      │
│                         │                               │
│                  CredentialHandle                       │
│                         │                               │
│                         ▼                               │
│                  CredentialBroker                       │
│                         │                               │
│                         ▼                               │
│                      Internet                           │
│                         ▲                               │
│                         │                               │
│                Host enforcement                         │
│                         ▲                               │
│                         │                               │
│                 passt / gvproxy                         │
│                         ▲                               │
└─────────────────────────┼───────────────────────────────┘
                          │
                       virtio-net
                          │
┌─────────────────────────┼───────────────────────────────┐
│                         │          GUEST                │
│                         ▼                               │
│                 Guest kernel                            │
│                         │                               │
│                  MVM eBPF policy                        │
│                         │                               │
│                         ▲                               │
│                         │                               │
│                  compromised                            │
│                  workload userspace                     │
│                                                         │
└─────────────────────────────────────────────────────────┘
```

The security chain is:

```text
compromised userspace
        │
        ▼
trusted guest kernel/eBPF
        │
        ▼
virtio
        │
        ▼
VM-specific passt/gvproxy boundary
        │
        ▼
host-derived VM identity
        │
        ▼
host direct-egress enforcement
        │
        ▼
transparent TLS interception
        │
        ▼
validated destination identity
        │
        ▼
host authorization policy
        │
        ▼
opaque credential capability
        │
        ▼
credential broker
        │
        ▼
managed HTTP authentication
        │
        ▼
Internet
```

## Key Principles

1. **The workload is fully compromised at the userspace level.**
2. **The guest kernel and pre-installed MVM eBPF enforcement are trusted.**
3. **eBPF constrains the network capabilities available to workload
   userspace.**
4. **`passt` / `gvproxy` provide the host-controlled VM network boundary.**
5. **VM identity comes from host-controlled network topology, never guest
   assertions.**
6. **Host enforcement prevents network-path bypass.**
7. **The L7 proxy establishes destination identity.**
8. **SNI and guest DNS are observations, not authorization by themselves.**
9. **Every destination change requires a new authorization decision.**
10. **The guest never selects a credential.**
11. **The sentinel expresses request intent; it does not grant authorization.**
12. **Credentials are represented internally by opaque host-side capabilities.**
13. **Real credentials never enter the VM.**
14. **TLS interception failure never falls back to unrestricted credentialed
    networking.**
15. **UDP/QUIC cannot bypass the TCP/TLS credential path.**
16. **Guest eBPF and host enforcement are independent security layers.**
17. **Failure of a security-critical enforcement layer is fail-closed.**
18. **Credential injection is an authorization decision, not a header-replacement
    feature.**
