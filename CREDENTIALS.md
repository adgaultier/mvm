# TODO — Secure L7 Credential Injection for MicroVMs

## Goal

Build a host-side Rust L7 egress proxy following the Docker Sandbox model:

> The microVM can request a destination, but can never choose or obtain the credential injected into that request.

---

## 1. Core architecture

* [ ] Host-side Rust L7 proxy
* [ ] Proxy shared by multiple microVMs
* [ ] MicroVMs have no access to real credentials
* [ ] Enforce all outbound traffic through proxy
* [ ] Prevent direct network bypass
* [ ] Use eBPF only if needed for transparent interception/performance

```text
MicroVM
   │
   │ outbound request
   ▼
Rust L7 Proxy
   │
   ├── VM identity
   ├── destination normalization
   ├── policy evaluation
   ├── credential lookup
   └── credential injection
   │
   ▼
External API
```

---

## 2. Security invariant

* [ ] Credential injection requires explicit authorization of:

```text
(VM identity, destination, credential)
```

* [ ] Guest cannot select the credential
* [ ] Guest cannot grant itself credential access
* [ ] Guest cannot access credential store
* [ ] Guest cannot bypass proxy

---

## 3. VM identity

* [ ] Assign stable identity to every microVM
* [ ] Map every outbound connection to VM identity
* [ ] Prevent VM identity spoofing
* [ ] Isolate credential policies between VMs

Example:

```text
VM-123 → OpenAI credential
VM-456 → Anthropic credential
VM-789 → no credentials
```

---

## 4. Destination policy

* [ ] Credential policies are destination-specific
* [ ] Prefer exact FQDNs over wildcards
* [ ] Match port where appropriate
* [ ] Normalize hostnames
* [ ] Correctly handle trailing dots/case
* [ ] Prevent `evil-example.com` matching `example.com`
* [ ] Handle IPv4/IPv6
* [ ] Control DNS resolution
* [ ] Consider DNS rebinding
* [ ] Do not blindly trust guest-supplied IP/Host combinations

Example:

```yaml
vm: vm-123
credential: openai-prod
destinations:
  - api.openai.com:443
```

---

## 5. Credential selection

* [ ] Guest never specifies `credential_ref`
* [ ] Guest only specifies destination
* [ ] Policy engine determines credential
* [ ] Credential store receives only trusted credential references
* [ ] Never expose real credential to guest
* [ ] Support sentinel value such as `proxy-managed`
* [ ] Never allow sentinel alone to authorize injection

```text
VM
 │
 │ https://api.openai.com
 ▼
Policy
 │
 └── api.openai.com → openai-prod
                         │
                         ▼
                    Secret Store
```

---

## 6. Header injection

* [ ] Remove guest-supplied managed `Authorization` header
* [ ] Inject only policy-authorized credential
* [ ] Never merge guest and managed credentials
* [ ] Support Bearer tokens
* [ ] Support API-key headers
* [ ] Optionally support Basic Auth
* [ ] Never log injected credentials

```text
Guest:
Authorization: Bearer proxy-managed

Proxy:
Authorization: Bearer <REAL_SECRET>
```

---

## 7. Redirect security

* [ ] Treat destination changes as new authorization decisions
* [ ] Never automatically forward credentials to a new host
* [ ] Strip managed credentials on cross-host redirects
* [ ] Re-evaluate policy after redirects
* [ ] Prefer exact destination binding

```text
api.example.com
      │
      │ 302
      ▼
attacker.com
      │
      └── Authorization removed
```

---

## 8. HTTPS

### Explicit proxy / CONNECT

* [ ] Support HTTP proxy
* [ ] Support `CONNECT`
* [ ] Authenticate/authorize CONNECT destination
* [ ] Verify upstream TLS
* [ ] Understand that CONNECT alone cannot inject HTTP headers

### HTTPS credential injection

If actual HTTP header injection is required:

* [ ] Terminate TLS at host proxy
* [ ] Maintain private sandbox CA
* [ ] Install CA into microVM
* [ ] Generate destination certificates
* [ ] Inspect decrypted HTTP request
* [ ] Inject credential
* [ ] Establish separate upstream TLS connection

```text
MicroVM ──TLS₁──► Proxy ──TLS₂──► API
                    │
                 inject
                 credential
```

---

## 9. Redirect / DNS / destination attacks

* [ ] Protect against credential leakage through redirects
* [ ] Protect against DNS rebinding
* [ ] Prevent arbitrary IP access from receiving hostname-bound credentials
* [ ] Validate destination after DNS resolution
* [ ] Handle IPv4/IPv6 consistently
* [ ] Prevent wildcard policy abuse
* [ ] Re-authorize destination changes

---

## 10. Network enforcement

* [ ] Force microVM egress through proxy
* [ ] Block direct Internet access
* [ ] Prevent alternate proxy configuration
* [ ] Prevent raw-IP bypass where policy requires hostname validation
* [ ] Decide whether DNS must also be proxied
* [ ] Test proxy-bypass attempts
* [ ] Optionally investigate eBPF for transparent interception

Required invariant:

```text
microVM ──X──► Internet

microVM ─────► Proxy ─────► Internet
```

---

## 11. Rust components

Implement:

```text
ProxyServer
├── VmIdentityResolver
├── DestinationNormalizer
├── PolicyEngine
├── CredentialStore
├── CredentialInjector
└── UpstreamClient
```

Suggested crates:

* [ ] `tokio`
* [ ] `hyper`
* [ ] `rustls`
* [ ] `http`

---

## 12. Security testing

* [ ] VM cannot request arbitrary credential
* [ ] VM cannot access another VM's credential
* [ ] Attacker-controlled destination receives no credential
* [ ] Redirect cannot leak credential
* [ ] DNS rebinding cannot leak credential
* [ ] Wildcard hostname cannot unexpectedly receive credential
* [ ] Guest `Authorization` header cannot override managed credential
* [ ] Guest cannot bypass proxy
* [ ] Credentials never appear in logs
* [ ] Credentials never appear in proxy errors
* [ ] Credential rotation works
* [ ] Expired credentials are rejected
* [ ] Proxy compromise is treated as credential-store compromise

---

## 13. Critical security properties

### Property 1 — Guest cannot select credentials

```text
Guest → destination
Proxy → credential
```

Never:

```text
Guest → destination + credential
```

### Property 2 — Credential is destination-bound

```text
credential A → api.example.com:443
```

not:

```text
credential A → arbitrary HTTPS destination
```

### Property 3 — Destination changes require reauthorization

```text
A → redirect → B

credential(A) ≠ credential(B)
```

unless policy explicitly authorizes B.

### Property 4 — Proxy is the security boundary

Assume:

```text
microVM = fully compromised
```

The attacker inside the VM must still be unable to obtain or exfiltrate credentials.

---

## MVP

* [ ] Rust host-side proxy
* [ ] Firecracker VM identity
* [ ] Enforced proxy egress
* [ ] Exact hostname + port policies
* [ ] Static credential store
* [ ] Bearer/API-key injection
* [ ] Sentinel credential support
* [ ] Redirect protection
* [ ] Secret-safe logging
* [ ] Security tests for malicious destinations
* [ ] Integration test with compromised microVM

## Key Principle

> **Credential injection must be an authorization decision, not a header-replacement feature.**
