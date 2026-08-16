# libkrunfw Guest Kernel Security Capabilities

## Scope

This focuses exclusively on the **guest kernel** controlled through `libkrunfw`, assuming the threat model is a **hostile container that obtains root inside the VM**.

The relevant distinction is that these controls are enforced by the guest kernel and are therefore independent of the host-side shim/VMM security configuration.


## Kernel capabilities

| Capability              | x86_64 | AArch64 | Relevance                                                                                              |
| ----------------------- | -----: | ------: | ------------------------------------------------------------------------------------------------------ |
| `CONFIG_BPF`            |      ✅ |       ✅ | eBPF infrastructure                                                                                    |
| `CONFIG_BPF_SYSCALL`    |      ✅ |       ✅ | Allows use of the `bpf()` syscall                                                                      |
| `CONFIG_CGROUP_BPF`     |      ✅ |       ✅ | Attach BPF programs to cgroups                                                                         |
| `CONFIG_SECCOMP`        |      ✅ |       ✅ | Seccomp support                                                                                        |
| `CONFIG_SECCOMP_FILTER` |      ✅ |       ✅ | Seccomp-BPF syscall filtering                                                                          |
| PID namespaces          |      ✅ |       ✅ | Process isolation                                                                                      |
| Network namespaces      |      ✅ |       ✅ | Network isolation                                                                                      |
| User namespaces         |      ✅ |       ✅ | UID/capability isolation                                                                               |
| LSM framework           |      ✅ |       ❌ | Mandatory-access-control infrastructure                                                                |
| SELinux                 |      ✅ |       ❌ | Label-based MAC                                                                                        |
| AppArmor                |      ❌ |       ❌ | Not enabled                                                                                            |
| BPF LSM                |     ⚠️ |       ❌ | Requires `CONFIG_BPF_LSM`; should be verified explicitly rather than inferred from generic BPF support |
| BPF JIT                |      ❌ |       ❌ | `CONFIG_BPF_JIT` is disabled in the configs                                                            |
| `CONFIG_DEBUG_INFO_BTF`|      ❌ |       ❌ | Not enabled — **the blocker for Aya/CO-RE and for BPF LSM.** Not verified on either arch; `scripts/integration/probes/bpfprobe.c` reports it |

### Important distinctions

* **BPF is not the same as BPF LSM.** `CONFIG_BPF=y` and `CONFIG_CGROUP_BPF=y` do not imply that BPF LSM is available.
* **Seccomp-BPF is available on both architectures.** This provides a direct mechanism for restricting the syscall surface of the container.
* **x86_64 has substantially more guest-side security infrastructure** because the LSM framework and SELinux are enabled.
* **AArch64 currently has no LSM framework enabled**, so SELinux/AppArmor/BPF-LSM-based enforcement is not available from the stock configuration.
* `CONFIG_BPF_SYSCALL=y` means a guest process can potentially use the `bpf()` interface; therefore, a security design that loads trusted BPF policies must also prevent the hostile container from acquiring sufficient privilege to create/modify its own BPF programs or maps.
* **BTF (BPF Type Format) is the unstated prerequisite for CO-RE/Aya and BPF LSM.** The cgroup_skb path probed below does not require BTF because the probe uses a non-relocatable program with no kernel type access. The practical question is therefore split: cgroup-BPF capability is measured by `progl`/`attach`, while CO-RE and BPF LSM still require `/sys/kernel/btf/vmlinux`.

## Verification (runtime probe)

`scripts/integration/probes/bpfprobe.c` is a static binary run inside a real guest (via
`just bpfprobe`) that measures what the *actual* libkrunfw kernel
offers, rather than trusting a hand-maintained table:

```text
btf=1|0            /sys/kernel/btf/vmlinux present
configgz=1|0       /proc/config.gz readable (CONFIG_* greppable)
jit=<n>|na         /proc/sys/net/core/bpf_jit_enable
cgroup2=<errno>    mount("cgroup2") result
bpffs=<errno>      mount("bpf") result
progl=<errno>      BPF_PROG_LOAD of a trivial cgroup_skb program
attach=<errno>     BPF_PROG_ATTACH of that program to the cgroup
```

The `progl=0` + `attach=0` verdict is the gate for the in-guest
cgroup_skb/egress policy (see SEC.TODO.md P2 "Guest syscall hardening"). The
table above must be re-checked against this probe whenever libkrunfw is
bumped — the two drift.

> **Verified measurements (2026-08-16, libkrunfw 5.5.0 / Linux 6.12 guest):**
> `btf=0 configgz=0 jit=na cgroup2=0 bpffs=0 progl=0 attach=0`. The guest
> kernel has no BTF, but cgroup2 and bpffs are mountable and a trivial
> `cgroup_skb` program loads and attaches successfully. The previous
> `progl=22` result was a probe defect: the probe used `28` (EXT, not
> CGROUP_SKB) and `2` (SOCK_CREATE, not INET_EGRESS) instead of the Linux 6.12
> UAPI values `8` and `1`. The cgroup-BPF Phase 2 capability gate is therefore
> open; CO-RE/Aya and BPF-LSM remain unavailable without guest BTF.

## Strict-mode seccomp (implemented)

`mvm run --security=strict` makes the guest agent install a second, workload-
scoped seccomp filter in the spawn path (`apply_strict_seccomp` in
`crates/agent/src/linux.rs`, `build_strict` in `crates/agent/src/seccomp.rs`).
It denies `bpf`, `keyctl`, `perf_event_open`, `userfaultfd`, the `io_uring_*`
trio, `ptrace`, namespace changes, mount/pivot-root, module loading, and kexec
with `EPERM`, while the agent itself keeps the full syscall surface. This is
the "seccomp as the syscall baseline" half of the recommended design,
delivered with zero new dependencies. Namespace-creation flags passed to
`clone`/`clone3` and capability dropping remain separate compatibility
decisions; strict workloads now set `PR_SET_NO_NEW_PRIVS` before exec.

## Recommended guest security architecture

The objective is to establish the security policy **before handing execution to an untrusted/root container**, then remove the container's ability to modify that policy.

```text
                 boot
                  │
                  ▼
          trusted guest init
                  │
       ┌──────────┼──────────┐
       │          │          │
    configure   load BPF   configure
    seccomp     policies   LSM/cgroups
       │          │          │
       └──────────┼──────────┘
                  │
             drop privilege
                  │
                  ▼
            hostile container
                  │
                root
                  │
                  ▼
          policy remains immutable
```

### Security principle

The critical step is **not merely configuring the policies**. The trusted initialization phase must also ensure that the hostile container cannot subsequently:

* remove or weaken its seccomp restrictions;
* load arbitrary privileged eBPF programs;
* create or modify security-sensitive BPF maps;
* acquire the capabilities necessary to bypass the policy;
* modify protected cgroups;
* alter the guest LSM policy;
* escape the intended namespace hierarchy.

In other words:

> **Root inside the guest should mean root within the container's permitted security domain, not unrestricted control over the guest kernel's security mechanisms.**

## Architecture implications

### x86_64

The current configuration provides the strongest foundation for this design:

```text
seccomp
   +
BPF / cgroup-BPF
   +
LSM
   +
SELinux
   +
namespaces
   +
capabilities
   +
cgroups
```

SELinux can provide a separate mandatory-access-control layer in addition to seccomp and BPF-based policies.

### AArch64

The current configuration provides:

```text
seccomp
   +
BPF / cgroup-BPF
   +
namespaces
   +
capabilities
   +
cgroups
```

but **does not enable the LSM framework or SELinux**.

If equivalent mandatory-access-control functionality is required on AArch64, the kernel configuration would need to be changed and rebuilt.

## Recommended direction

For a hardened libkrun guest, treat **seccomp as the syscall baseline**, use
**cgroup-BPF for programmable transport enforcement** where the policy is
defined, and use **LSM/SELinux on x86_64 where available**. The current probe
opens the cgroup-BPF capability gate; it does not itself install a security
policy.

The most important design property is that these policies are installed by a **trusted guest initialization phase** and that the subsequent container does not retain the privileges required to modify or bypass them.

## Next implementation gate

The kernel capability gate is now open, but policy enforcement is not yet
implemented. The next change must define an explicit egress policy in the
sandbox specification before adding a loader. That implementation needs to:

1. pass the policy to the trusted guest agent without exposing mutable policy
   state to the workload;
2. mount or reuse cgroup2 and load/attach the cgroup_skb program before the
   workload starts;
3. place the workload in the protected cgroup;
4. verify the attachment and fail closed if setup fails; and
5. keep the workload from loading, replacing, detaching, or modifying the
   enforcement program and its maps.

An allowlist format (CIDRs, ports, protocols, and behavior for NIC-backed
modes) must be selected first. TSI is explicitly excluded: its socket calls
are serviced by the host and do not traverse a guest NIC, so guest cgroup-BPF
network policy cannot enforce TSI traffic. A hard-coded allow-all or deny-all
program would not implement the documented security contract.
