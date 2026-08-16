# MVM Snapshot → CoW VM Fork Plan
```
                    MVM
                     │
                  libkrun
                     │
          ┌──────────┴──────────┐
          │                     │
       x86_64                 aarch64
          │                     │
         KVM                   HVF
          │                     │
          └──────────┬──────────┘
                     │
              snapshot layer
                     │
          ┌──────────┴──────────┐
          │                     │
       VM state              RAM backing
          │                     │
     arch-specific          CoW mapping
```
1. **Keep libkrun as the sole VMM.**
   Do not add a second KVM/VMM implementation to MVM. Extend libkrun's existing Linux KVM path.

2. **Reuse libkrun's existing VM/vCPU state machinery.**
   Leverage the existing `Vm::save_state()` / `restore_state()` and `Vcpu::save_state()` / `restore_state()` implementations for CPU, interrupt-controller, timer, MSR, XSAVE/XCRS, and related KVM state.

3. **Introduce a libkrun snapshot abstraction.**
   Define a snapshot containing:

   * guest-memory backing
   * VM state
   * vCPU state
   * snapshot metadata/version/architecture information
     Never persist host virtual addresses or host file descriptors.

4. **Make guest-memory registration backend-independent.**
   Refactor the current `GuestMemoryMmap → KVM_SET_USER_MEMORY_REGION` path so memory can come either from normal libkrun allocations or from an externally `mmap()`-ed snapshot.

5. **Implement CoW restore with `MAP_PRIVATE`.**
   On fork, map the immutable snapshot memory using `mmap(..., PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_NORESERVE, ...)`, then register those mappings with KVM. This gives each VM private pages only after guest writes.

6. **Restore into a fresh KVM VM.**
   Each fork creates a new libkrun VM/vCPU set, restores the saved KVM/vCPU state, and starts its own shim process. The source VM does not need to remain alive.

7. **Separate guest state from host backends.**
   Snapshot guest-visible device state, but recreate/rebind host resources for every fork: virtiofs, networking, vsock, sockets, file descriptors, etc. Avoid serializing host-specific state.

8. **Add persistent snapshots to MVM.**
   Introduce a snapshot/template abstraction such as:

   ```text
   snapshot/
     metadata.json
     memory.bin
     vmstate.bin
   ```

   Initially use a regular file or memfd-backed file; optimize later if needed.

9. **Add `snapshot → fork` to the MVM lifecycle.**
   First implement:

   ```text
   running VM
       → pause
       → capture state + RAM
       → snapshot
       → restore N independent VMs
   ```

   Keep the existing disk reflink/OverlayFS mechanism for filesystem CoW.

10. **Prototype and validate libkrun first, then integrate MVM.**
    First prove `snapshot → restore → N CoW VMs` entirely inside libkrun, including memory isolation and device rebinding. Then add the MVM daemon/API/shim integration. The direct inspiration for the CoW memory mechanism is the [`fork_cow()` implementation](https://github.com/zerobootdev/zeroboot/blob/main/src/vmm/kvm.rs).
