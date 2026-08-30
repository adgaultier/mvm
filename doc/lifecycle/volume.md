Plan: fix Permission denied on -v bind mounts via LinuxSimplified virtiofs semantics

Problem:
In userns mode, -v bind mounts are virtiofs passthroughs of live host
directories. Files owned by the real host user can appear as root:root in
the guest, so a non-root OCI workload gets Permission denied when accessing
the mount.

Mechanism:
Use libkrun's krun_add_virtiofs4() with
KRUN_SEMANTICS_LINUX_SIMPLIFIED = 1 for -v mounts.

LinuxSimplified is a permission semantic, not UID squashing. It changes how
virtiofs enforces host ownership/mode information for guest access, avoiding
normal guest-side Unix DAC failures caused by mismatched host ownership.

The concrete host-side ownership behavior of create/chown operations must be
verified experimentally and documented from the observed behavior rather than
inferred from the semantic name.

Changes:

1. crates/krun-sys/src/lib.rs
   - Add:
     krun_add_virtiofs4(
         ctx_id: c_uint,
         c_tag: *const c_char,
         c_path: *const c_char,
         shm_size: c_ulonglong,
         read_only: bool,
         semantics: c_uint
     ) -> c_int
   - Keep the declaration synchronized with the supported libkrun header.
   - Add named constants:
       KRUN_SEMANTICS_LINUX_COMPLETE = 0
       KRUN_SEMANTICS_LINUX_SIMPLIFIED = 1

2. crates/runtime/src/vm.rs::add_virtiofs
   - Replace krun_add_virtiofs3() with krun_add_virtiofs4().
   - Pass:
       KRUN_SEMANTICS_LINUX_SIMPLIFIED
   - Apply this only to additional -v virtiofs mounts.
   - Leave the rootfs path (set_root / krun_set_root) unchanged, preserving
     LinuxComplete semantics there.

3. Integration verification
   Behavior-test the actual filesystem semantics rather than relying on the
   meaning of the enum name.

   Verify:
   - non-root (-u 1000) workload can create/write files on -v host:dir:rw;
   - root (-u root) workload can create/write files;
   - root workload can attempt chown 0:0;
   - root workload can attempt chown to an arbitrary UID/GID;
   - host-side ownership after each operation is recorded and asserted
     according to the intended security invariant;
   - chmod behavior is also checked if relevant to the security model.

   Run separately on Linux and macOS where supported.

4. Documentation
   - README volume documentation:
     - -v mounts use LinuxSimplified semantics;
     - host ownership/mode bits do not enforce normal guest Unix DAC checks;
     - this intentionally differs from the rootfs, which retains
       LinuxComplete semantics.
   - AGENTS.md:
     - document this as a deliberate permission/ownership semantic boundary;
     - explicitly warn that -v exposes a live host directory;
     - document the tested create/chown/chmod behavior.

Notes:
- No Mount/CLI/wire/struct changes.
- parse_volume, validate_mounts, mount_bind_shares, and Mount remain unchanged.
- macOS-specific guestd mount/chown handling remains unchanged.
- Ensure the project's minimum supported libkrun version provides
  krun_add_virtiofs4().

