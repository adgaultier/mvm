# Bootstrap plan — Aya eBPF
## 1. Create new crate

crates/
  guest-ebpf/     # no_std eBPF program
  guestd/         # normal static-musl userspace

## 2. guest-ebpf uses aya-ebpf

#![no_std]
#![no_main]

use aya_ebpf::{
    bindings::BPF_SOCK_ADDR,
    macros::cgroup_sock_addr,
    programs::CgroupSockAddr,
};

#[cgroup_sock_addr(connect4)]
pub fn connect4(ctx: CgroupSockAddr) -> i32 {
    1 // initially allow everything
}

## 3. Build guest-ebpf for the BPF target

bpfel-unknown-none

using the Aya build tooling / bpf-linker. The build produces the BPF ELF as a build artifact; it doesn't need to be manually maintained as network.bpf.o.

## 4. Make guestd consume that build artifact

Use the Aya userspace crate (aya) in guestd.
Load the generated BPF ELF from the build output.
Initially, just load and attach connect4.
Keep the program/link alive for the lifetime of guestd.
## 5. First test: prove the complete pipeline

guestd
  ↓
Aya
  ↓
generated eBPF ELF
  ↓
kernel verifier
  ↓
cgroup/connect4
  ↓
workload

Test the same artifact on both your x86_64 and aarch64 libkrun kernels.

## 6. Then add a BPF map

guest-ebpf: lookup destination/port and return allow/deny.
guestd: parse the user's configuration and populate the map.
Keep all configuration parsing and validation in Rust userspace.
## 7. Only after that add connect6, bind4/6, etc.

One additional correction: Aya's build tooling can arrange for the eBPF artifact to be embedded into the userspace binary, so you don't necessarily need a runtime .o file in the VM filesystem. That's what I'd prefer for guestd:

build time:

guest-ebpf Rust
      ↓
  bpf-linker
      ↓
   BPF ELF
      ↓
   embed into
      ↓
     guestd
      ↓
static musl binary

Then the VM only needs one guestd binary, which is particularly nice for your deployment model.
