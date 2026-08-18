# mvm Security / Escape Testing TODO

## Goal

Build a reproducible adversarial security-testing framework for `mvm`.

The primary security question is:

> **If an attacker achieves arbitrary code execution, root, or kernel compromise inside an `mvm` guest, can they cross the guest → `libkrun` → KVM → host isolation boundary, affect another VM, or violate an explicitly granted capability?**

A successful exploit **inside the guest is not an `mvm` failure**.

The security boundary being tested is:

```text
┌─────────────────────────────────────────────┐
│                   HOST                      │
│                                             │
│   mvm daemon → libkrun → KVM / HVF          │
│        │                                    │
│        │                                    │
│   ┌────▼─────────┐                          │
│   │    VM A      │  ← fully compromised     │
│   │ guest kernel │                          │
│   └──────────────┘                          │
│                                             │
│   ┌──────────────┐                          │
│   │    VM B      │  ← isolation canary      │
│   └──────────────┘                          │
│                                             │
└─────────────────────────────────────────────┘
```

The test framework must assume the guest is hostile and independently determine whether any protected host or cross-VM invariant was violated.

---

# 1. Threat Model

## 1.1 Attacker capabilities

Assume the attacker can obtain:

* arbitrary code execution as an unprivileged guest user
* guest root
* guest kernel code execution
* arbitrary guest filesystem modification
* arbitrary network traffic permitted by the configured network mode
* arbitrary malformed input to exposed virtual devices
* arbitrary interaction with guest-visible vsock services
* arbitrary resource consumption within configured guest limits
* arbitrary process creation and termination
* arbitrary attempts to abuse filesystem, network, device, and lifecycle interfaces

For relevant tests, assume the attacker has **complete guest compromise**.

## 1.2 Attacker goals

The following constitute isolation failures:

* host code execution
* host filesystem read/write/delete outside explicitly granted paths
* host process inspection or manipulation
* host process creation
* host kernel compromise
* host information disclosure
* access to `mvm` control-plane interfaces
* access to another VM
* modification of another VM's state
* persistent attacker-controlled host state
* unintended host network access
* unintended access to host-local services
* host denial of service caused by guest-controlled input
* `mvm` state corruption that allows attacker influence after teardown
* cross-VM state leakage

## 1.3 Explicitly non-failing outcomes

These are successful adversarial workloads but **not escapes**:

* guest process crash
* guest kernel crash
* guest root
* guest kernel compromise
* guest filesystem corruption
* guest-only denial of service
* guest network compromise
* successful exploitation of a vulnerable application inside the VM

Record these outcomes because they establish that the adversary model was achieved.

---

# 2. Security Invariants

Every test campaign should define explicit invariants before execution.

## 2.1 Host filesystem

A compromised guest must not be able to:

* read host-only canary files
* modify host-only canary files
* delete host-only canary files
* create files in protected host directories
* access unintended host sockets
* traverse outside explicitly granted mount boundaries
* abuse symlinks or hard links to escape an allowed path
* modify mvm state outside explicitly granted storage

Test:

* absolute paths
* `..`
* symlinks
* hard links
* rename races
* concurrent deletion
* xattrs
* ACLs
* ownership changes
* special files
* mount-related behavior

## 2.2 Host processes

A compromised guest must not be able to:

* enumerate unintended host processes
* read host process memory
* signal host processes
* ptrace host processes
* create host processes
* manipulate host namespaces/cgroups
* influence unrelated host processes

Create a dedicated host canary process and continuously verify its:

* PID
* credentials
* command line
* environment
* open files
* memory/state
* liveness

## 2.3 Host network

Test guest access to:

* host loopback
* host-local services
* host interfaces
* private networks
* metadata services where applicable
* explicitly allowed destinations
* explicitly denied destinations

Verify both:

```text
guest → host
guest → host localhost
guest → private network
guest → other VM
```

and the reverse direction where relevant.

## 2.4 Control plane

A compromised guest must not gain unintended access to:

* the mvm HTTP API
* Unix sockets
* management sockets
* host-side vsock bridges
* inherited file descriptors
* host services used by mvm
* libkrun control interfaces

Test both normal and malformed interaction attempts.

## 2.5 Cross-VM isolation

Run at least two simultaneous VMs:

```text
VM A = attacker
VM B = canary
```

VM A should be able to obtain full guest compromise.

VM A must not be able to:

* inspect VM B
* read VM B's filesystem
* modify VM B's filesystem
* signal VM B
* access VM B's network namespace
* access VM B's vsock/control interfaces
* corrupt VM B's state
* influence VM B's execution

---

# 3. Independent Escape Oracles

Never determine escape success from the exploit's own output.

A workload may claim:

```text
root!
escaped!
shell!
```

without having escaped the VM.

Conversely, an exploit may silently modify the host.

All escape determinations must come from independent host-side and cross-VM oracles.

## 3.1 Filesystem oracle

Before every test:

1. Create host-only canary files.
2. Record hashes, metadata, and ownership.
3. Start the VM.
4. Execute the workload.
5. Destroy the VM.
6. Verify all canaries.

Detect:

* read access
* modification
* deletion
* creation
* metadata changes
* unexpected files
* unexpected mounts
* unexpected sockets

## 3.2 Process oracle

Create a host canary process.

Verify:

* process remains alive
* credentials remain unchanged
* command line remains unchanged
* environment remains unchanged
* memory/state remains unchanged
* no unexpected signals occur
* no unexpected processes appear

Capture the host process tree before and after the workload.

## 3.3 Network oracle

Record host:

* interfaces
* routes
* listening sockets
* active connections
* firewall state where applicable

Detect unexpected changes after guest compromise.

## 3.4 VM oracle

For two-VM tests, VM B acts as a canary.

VM B should expose unique:

* filesystem secrets
* process identifiers
* network services
* VM-specific markers

VM A must not gain access to them.

## 3.5 Resource oracle

Record:

* host CPU
* host memory
* file descriptors
* process count
* network resources
* TAP devices
* sockets
* mounts
* temporary files
* mvm state

Detect resource leaks and host-level exhaustion.

---

# 4. Test Profiles

Every workload should run under an explicit mvm capability profile.

Example:

```yaml
name: kernelctf-example

network: none

mounts: []

devices:
  - virtio-console
  - virtio-block
  - vsock

resources:
  cpus: 2
  memory: 2G
```

Profiles should cover combinations of:

* network disabled
* TSI
* gvproxy
* external gvproxy
* TAP
* explicit filesystem mounts
* different device configurations
* resource limits
* snapshot/restore
* cloning/forking where applicable

The same adversarial workload should be repeated across relevant profiles.

Goal:

> Determine which explicitly granted capabilities affect the attack surface.

---

# 5. ExploitGym Integration

Use [ExploitGym](https://github.com/sunblaze-ucb/exploitgym) as a standardized **adversary generator**, not as proof of mvm security.

Record the exact:

* ExploitGym commit
* task-list version
* task metadata
* image digest
* workload configuration

Do not download the entire benchmark unnecessarily.

Create an mvm-specific task list containing only selected workloads.

ExploitGym v1 currently contains:

* 186 kernel tasks
* 502 userspace tasks
* 181 V8 tasks
* 9 V8 sandbox-break tasks

References:

* [ExploitGym repository](https://github.com/sunblaze-ucb/exploitgym)
* [evaluation documentation](https://github.com/sunblaze-ucb/exploitgym/blob/main/docs/eval.md)
* [Docker/image documentation](https://github.com/sunblaze-ucb/exploitgym/blob/main/docs/docker_images.md)
* [setup documentation](https://github.com/sunblaze-ucb/exploitgym/blob/main/docs/setup.md)
* [changelog](https://github.com/sunblaze-ucb/exploitgym/blob/main/CHANGELOG.md)
* [data license](https://github.com/sunblaze-ucb/exploitgym/blob/main/DATA_LICENSE.md)

---

# 6. Priority A — V8 Sandbox-Break Workloads

Run all current `v8:*sbxbrk*` workloads.

Expected current set:

* 9 sandbox-break tasks

For each workload:

1. Start a fresh mvm guest.
2. Give the workload maximum reasonable guest privileges.
3. Confirm successful V8 sandbox compromise.
4. Run all host and cross-VM oracles.
5. Destroy the VM.
6. Re-run post-destruction oracles.
7. Preserve all artifacts.

Verify that successful V8 sandbox escape remains entirely inside the guest.

Test access to:

* host filesystem
* host processes
* host network
* other VMs
* mvm control interfaces

These workloads are primarily **guest compromise generators**.

---

# 7. Priority A — KernelCTF

Pull selected `kernel:kernelctf:*` workloads.

Start with workloads that achieve:

* guest kernel compromise
* guest privilege escalation
* arbitrary kernel code execution

For each:

* run with networking disabled
* run with networking enabled
* run across relevant device profiles
* run with resource limits
* run with maximum reasonable guest privileges

Verify that guest kernel compromise does not cross the VM boundary.

Record host-side crashes separately from guest crashes.

ExploitGym v1 contains approximately 27 KernelCTF tasks.

---

# 8. Priority A — syzbot

Pull selected `kernel:syzbot:*` workloads.

Prioritize bugs involving:

* networking
* filesystems
* io_uring
* device drivers
* KVM
* virtio
* namespaces
* privilege boundaries
* resource exhaustion

For each workload classify the result:

```text
GUEST_CRASH
GUEST_KERNEL_COMPROMISE
MVM_CRASH
MVM_HANG
HOST_KERNEL_WARNING
HOST_KERNEL_CRASH
HOST_RESOURCE_EXHAUSTION
HOST_FILESYSTEM_ACCESS
HOST_PROCESS_INTERACTION
HOST_CODE_EXECUTION
OTHER_VM_ACCESS
```

ExploitGym v1 contains approximately 159 syzbot tasks.

---

# 9. Priority B — V8 Human Exploitation Workloads

Run selected `v8:human:*` workloads.

Prefer workloads involving:

* memory corruption
* arbitrary code execution
* sandbox escape
* complex exploitation primitives

The intended sequence is:

```text
V8 compromise
      ↓
guest code execution
      ↓
guest compromise
      ↓
mvm isolation oracles
```

ExploitGym v1 contains approximately 66 human-derived V8 workloads.

---

# 10. Priority C — Generic Userspace Exploitation

Select representative:

* `user:cybergym:*`
* `user:nofuzz:*`

Prefer workloads that produce:

* arbitrary code execution
* privilege escalation
* root shells
* memory corruption

Avoid spending early campaign time on vulnerabilities that cannot produce a meaningful guest compromise.

Use these primarily to diversify the hostile-process corpus.

---

# 11. mvm Security Harness

Create:

```text
mvm-security/
├── campaign/
│   ├── manifests/
│   ├── scheduler/
│   └── runner/
│
├── exploitgym/
│   ├── tasks.txt
│   ├── adapters/
│   └── runner/
│
├── workloads/
│   ├── exploitgym/
│   ├── libkrun/
│   ├── lifecycle/
│   ├── networking/
│   └── custom/
│
├── oracles/
│   ├── filesystem/
│   ├── process/
│   ├── network/
│   ├── control_plane/
│   ├── vm/
│   └── resources/
│
├── instrumentation/
│   ├── host/
│   ├── guest/
│   └── mvm/
│
├── results/
├── regressions/
└── corpus/
```

The runner should:

1. Select a workload.
2. Select an mvm capability profile.
3. Record host baseline.
4. Create canaries.
5. Start mvm.
6. Boot the workload.
7. Wait for the expected guest condition.
8. Execute the adversarial workload.
9. Run independent escape oracles.
10. Destroy the VM.
11. Run post-destruction oracles.
12. Compare host state against the baseline.
13. Classify the result.
14. Preserve artifacts on failure.
15. Emit machine-readable results.

---

# 12. Host Instrumentation

For each test record enough information to establish what happened on the host.

Capture:

## Processes

* process tree
* process creation/deletion
* credentials
* namespaces
* cgroups
* open file descriptors
* unexpected host processes

## Filesystem

* canary hashes
* metadata
* mvm state
* mounts
* sockets
* temporary files
* unexpected files

## Network

* interfaces
* routes
* listening sockets
* active connections
* TAP devices
* relevant firewall state

## VM state

* mvm processes
* libkrun processes/state
* VM lifecycle state
* resource usage
* device state

Do not rely solely on post-test inspection for critical invariants where continuous monitoring is practical.

---

# 13. libkrun Attack Surface

Create dedicated adversarial tests for every host-facing interface.

Prioritize:

* virtio-net
* virtio-block
* virtio-console
* vsock
* MMIO
* PCI/device configuration
* serial/console interfaces
* filesystem sharing
* networking/TAP
* snapshot/restore
* VM lifecycle
* resource limits

For every interface model:

```text
guest-controlled input
        ↓
     libkrun
        ↓
       host
```

Test:

* malformed inputs
* boundary sizes
* invalid state
* unexpected ordering
* repeated initialization
* repeated destruction
* concurrent operations
* partial initialization
* interrupted operations
* resource exhaustion
* malformed lifecycle sequences

Where possible, fuzz inputs rather than relying only on hand-written cases.

---

# 14. Filesystem / virtiofs Testing

Treat filesystem sharing as a first-class isolation boundary.

Create dedicated workloads for:

* symlink traversal
* hard links
* rename races
* concurrent unlink
* path traversal
* special files
* device nodes
* xattrs
* ACLs
* ownership changes
* permission changes
* malformed filenames
* deeply nested paths
* unusual Unicode/byte sequences
* concurrent filesystem operations
* mount/unmount races where applicable
* filesystem exhaustion

Verify that guest-controlled filesystem state cannot escape the explicitly granted boundary.

---

# 15. vsock Testing

Treat vsock as a security boundary, not merely a transport.

Test:

* malformed messages
* oversized messages
* truncated messages
* unexpected message ordering
* connection floods
* repeated connect/disconnect
* concurrent connections
* invalid commands
* invalid arguments
* unexpected lifecycle state
* guestd restart
* guestd crash
* guestd replacement
* connection teardown races

Verify that guest-controlled vsock traffic cannot:

* execute unintended host operations
* corrupt mvm state
* bypass authorization assumptions
* access unintended host services
* survive VM destruction

---

# 16. Network Isolation Testing

Run every relevant workload under:

```text
network = none
network = tsi
network = gvproxy
network = external gvproxy
network = TAP
```

Where supported.

Test:

* host loopback
* host services
* private network ranges
* other VMs
* DNS
* unexpected inbound connections
* unexpected outbound connections
* malformed packets
* connection floods
* network teardown races

Record both expected and unexpected connectivity.

---

# 17. Two-VM Cross-Isolation Campaign

Make this a dedicated campaign.

Topology:

```text
Host
├── VM A — attacker
└── VM B — canary
```

VM B should continuously expose canary state.

Compromise VM A completely.

Then attempt to:

* discover VM B
* access VM B filesystem
* access VM B processes
* signal VM B
* access VM B network
* access VM B control channels
* corrupt VM B state
* consume resources on behalf of VM B

Repeat under different network/device/storage configurations.

---

# 18. Lifecycle Testing

Security testing must cover lifecycle transitions, not just steady-state execution.

Test:

```text
create → start → stop → destroy

create → start → kill → destroy

create → start → crash → destroy

start → snapshot → destroy → restore

start → shutdown → resume

repeated start/stop

repeated destroy

concurrent VM creation/destruction

malformed configuration

resource exhaustion during lifecycle transitions
```

Verify that teardown always removes attacker-controlled state.

---

# 19. Dirty Teardown

Explicitly test hostile teardown conditions.

For example:

```text
guest compromised
      ↓
kill mvm
      ↓
immediate restart
      ↓
destroy
      ↓
recreate VM
```

Perform concurrent:

* filesystem I/O
* network I/O
* vsock I/O
* exec
* cloning
* snapshotting
* resource exhaustion
* process creation/destruction

Invariant:

> **After VM teardown, no attacker-controlled state may influence the host or a subsequently created VM.**

---

# 20. Snapshot / Restore Testing

Treat snapshot and restore as a separate security boundary.

Test:

```text
boot
 ↓
compromise guest
 ↓
modify memory
 ↓
modify filesystem
 ↓
network activity
 ↓
snapshot
 ↓
destroy
 ↓
restore
 ↓
continue execution
```

Mutate:

* snapshot timing
* restore timing
* concurrent destruction
* repeated restore
* restore after crash
* restore after resource exhaustion
* restore with active network connections
* restore with active vsock connections
* restore with active I/O

Check for:

* state leakage
* stale host resources
* stale file descriptors
* cross-VM state
* attacker-controlled persistence

---

# 21. Resource Exhaustion

Test both guest-level and host-level resource exhaustion.

Cover:

* CPU
* memory
* file descriptors
* processes
* filesystem space
* filesystem inodes
* network connections
* vsock connections
* virtio queues
* device state
* VM creation rate
* concurrent VMs

Important distinction:

```text
guest resource exhaustion
        ≠
host resource exhaustion
```

A guest exhausting its configured resources is expected.

A guest causing uncontrolled host resource exhaustion is a security finding.

---

# 22. Control-Plane Isolation

Verify that a compromised guest cannot reach the mvm management plane.

Test:

* TCP ports
* loopback services
* Unix sockets
* inherited descriptors
* environment leakage
* host address discovery
* socket scanning
* malformed HTTP/API requests
* concurrent API requests
* lifecycle manipulation attempts

The intended invariant is:

```text
compromised guest
        │
        X
        │
mvm control plane
```

unless access is explicitly part of the workload's granted capabilities.

---

# 23. Regression Corpus

Every discovered security bug becomes a permanent regression.

Store:

```text
regressions/
└── MVM-XXXX/
    ├── workload/
    ├── config/
    ├── expected.json
    ├── guest.log
    ├── host.log
    ├── oracle.log
    └── README.md
```

Record:

* mvm commit
* libkrun version/commit
* host kernel version
* CPU
* architecture
* guest kernel version
* guest image digest
* workload/task identifier
* capability profile
* exact reproduction command
* expected result
* observed result
* classification
* severity

Every discovered isolation violation should become a deterministic regression test whenever possible.

---

# 24. Result Classification

Do not use a binary pass/fail result.

Use:

```text
GUEST_CRASH
GUEST_ROOT
GUEST_KERNEL_COMPROMISE

MVM_CRASH
MVM_HANG
MVM_STATE_CORRUPTION

HOST_INFORMATION_LEAK
HOST_FILESYSTEM_ACCESS
HOST_PROCESS_INTERACTION
HOST_CODE_EXECUTION

OTHER_VM_ACCESS

HOST_RESOURCE_EXHAUSTION
HOST_KERNEL_WARNING
HOST_KERNEL_CRASH
HOST_DOS
```

The fundamental distinction is:

```text
Guest compromise
       ≠
mvm escape
```

A workload that obtains guest root but cannot cross the VM boundary is:

```text
successful adversarial workload
+
successful isolation test
```

---

# 25. Severity

Suggested severity ordering:

## Critical

* host code execution
* host kernel compromise
* arbitrary host filesystem write
* arbitrary code execution through mvm control plane
* cross-VM code execution

## High

* arbitrary host filesystem read
* host process manipulation
* cross-VM filesystem access
* persistent host state modification
* host kernel crash caused by guest input

## Medium

* host information disclosure
* unintended host network access
* host process enumeration
* mvm state corruption
* persistent resource leak

## Low

* limited information leakage
* recoverable mvm crash
* bounded resource exhaustion
* non-persistent lifecycle bugs

Severity should be adjusted according to the deployment model and explicitly granted capabilities.

---

# 26. Initial Campaign

Start with approximately 75 tests.

## Exploit workloads

* 9 V8 `sbxbrk` tasks
* 5–10 KernelCTF tasks
* 10–20 syzbot tasks
* 5 V8 human tasks
* 5 generic userspace tasks

## mvm-specific workloads

* 20 libkrun/device tests
* 20 lifecycle/resource/network tests

The exact count is less important than ensuring the initial campaign exercises:

* guest compromise
* guest kernel compromise
* filesystem sharing
* networking
* vsock
* device emulation
* lifecycle
* teardown
* resource limits
* two-VM isolation
* control-plane isolation

Do not scale to the full corpus until the harness and oracles are trustworthy.

---

# 27. Campaign Matrix

Eventually run workloads across a matrix such as:

```text
                 network
                 none
                 TSI
                 gvproxy
                 TAP

devices          minimal
                 standard
                 extended

storage          rootfs
                 mounts
                 clone/fork

lifecycle        normal
                 kill
                 crash
                 snapshot
                 restore

topology         single VM
                 two VMs
```

Not every combination will be meaningful.

The campaign scheduler should select combinations based on the capabilities exercised by each workload.

---

# 28. Reproducibility

Every test result must be reproducible.

Record:

* mvm commit
* libkrun commit/version
* host kernel
* CPU model
* architecture
* OS version
* guest kernel
* guest image digest
* workload version
* ExploitGym commit
* task identifier
* capability profile
* resource configuration
* test harness commit

Prefer immutable identifiers over mutable tags.

Example:

```yaml
mvm_commit: ...
libkrun_commit: ...
host_kernel: ...
architecture: x86_64
guest_kernel: ...
image_digest: sha256:...
exploitgym_commit: ...
task: ...
profile: ...
```

---

# 29. Continuous Regression Testing

The security harness should eventually run:

### Per change

* fast mvm-specific regressions
* known escape regressions
* lifecycle regressions
* control-plane regressions

### Periodically

* selected ExploitGym workloads
* syzbot corpus
* device fuzzing
* lifecycle fuzzing
* resource exhaustion

### Before releases

Run the complete maintained security corpus.

Security regressions must block release when they violate a defined isolation invariant.

---

# 30. Fuzzing

Add fuzzing beyond known exploit workloads.

Targets should include:

* vsock protocol parsing
* guestd commands
* device configuration
* virtio inputs
* lifecycle state transitions
* configuration parsing
* filesystem paths
* mount configuration
* networking configuration
* snapshot/restore state
* resource-limit boundaries

Prefer stateful fuzzing where the interface has meaningful lifecycle state.

Example:

```text
create
 → configure
 → start
 → connect
 → I/O
 → snapshot
 → stop
 → restore
 → reconnect
 → destroy
```

Mutate both inputs and operation ordering.

---

# 31. Failure Artifact Preservation

On any unexpected result, preserve:

* guest console log
* guest kernel log
* guest workload output
* mvm log
* libkrun log where available
* host kernel log
* host process snapshot
* host network snapshot
* filesystem oracle output
* VM oracle output
* resource measurements
* exact configuration
* exact workload
* exact binary/version metadata

The test runner should never destroy the evidence before the failure is captured.

---

# 32. Security Reports

Produce machine-readable results:

```json
{
  "test": "kernelctf-example",
  "profile": "gvproxy",
  "mvm_commit": "...",
  "libkrun_commit": "...",
  "architecture": "x86_64",
  "guest_image": "sha256:...",
  "guest_result": "GUEST_KERNEL_COMPROMISE",
  "isolation_result": "PASS",
  "classification": null,
  "artifacts": "results/..."
}
```

For failures:

```json
{
  "test": "libkrun-vsock-017",
  "guest_result": "GUEST_KERNEL_COMPROMISE",
  "isolation_result": "FAIL",
  "classification": "HOST_FILESYSTEM_ACCESS",
  "severity": "CRITICAL",
  "artifacts": "results/..."
}
```

---

# 33. Security Summary

The final campaign report should distinguish:

```text
Workload success
        ↓
Was the guest compromised?
        ↓
Did an isolation invariant change?
        ↓
Which boundary was crossed?
```

Never report:

> "X exploits succeeded, therefore mvm is insecure."

Instead report:

> "X adversarial workloads achieved guest compromise. Y produced an observable violation of an mvm isolation invariant."

This makes the security claim precise and reproducible.

---

# 34. Long-Term Goal

Build a maintained adversarial corpus that combines:

```text
ExploitGym
    +
KernelCTF
    +
syzbot
    +
V8 sandbox escapes
    +
libkrun-specific fuzzing
    +
mvm-specific fuzzing
    +
lifecycle fuzzing
    +
resource-exhaustion testing
    +
cross-VM testing
```

The objective is not to demonstrate that guest exploitation is impossible.

The objective is to demonstrate that:

> **A fully compromised guest remains confined to the capabilities explicitly granted to it.**

That is the security property `mvm` needs to earn.

---

# 35. Immediate TODO

* [ ] Write the threat model and security invariants
* [ ] Create `mvm-security/`
* [ ] Pin an exact ExploitGym commit
* [ ] Extract the canonical v1 task list
* [ ] Create an mvm-specific task manifest
* [ ] Implement the first ExploitGym adapter
* [ ] Implement host filesystem canaries
* [ ] Implement host process canary
* [ ] Implement host network oracle
* [ ] Implement two-VM canary
* [ ] Implement control-plane oracle
* [ ] Implement host resource monitoring
* [ ] Implement baseline/post-test comparison
* [ ] Implement structured result format
* [ ] Run the 9 V8 `sbxbrk` workloads
* [ ] Run 5–10 KernelCTF workloads
* [ ] Run 10–20 syzbot workloads
* [ ] Run 5 V8 human workloads
* [ ] Run 5 generic userspace workloads
* [ ] Add 20 libkrun/device adversarial tests
* [ ] Add 20 lifecycle/network/resource tests
* [ ] Add two-VM variants
* [ ] Add dirty-teardown tests
* [ ] Add snapshot/restore tests
* [ ] Add regression corpus
* [ ] Add CI regression execution
* [ ] Publish initial security campaign results

---

# Security Principle

```text
┌───────────────────────────────┐
│       Fully compromised       │
│          guest VM             │
└───────────────┬───────────────┘
                │
                │ attacker-controlled
                ▼
        ┌───────────────┐
        │ mvm / libkrun │
        │   boundary    │
        └───────┬───────┘
                │
                X
                │
        ┌───────▼────────┐
        │      HOST      │
        │                │
        │    VM B        │
        │    Control     │
        │    Filesystem  │
        │    Processes   │
        └────────────────┘
```

**Guest compromise is expected.**

**Crossing the boundary is the bug.**
