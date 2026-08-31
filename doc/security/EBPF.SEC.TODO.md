# Bootstrap plan — Aya eBPF
## 1. Create new crate

Implemented:

```text
crates/
  guest-ebpf/     # no_std eBPF program
  guestd/         # normal static-musl userspace and Aya loader
```

## 2. guest-ebpf uses aya-ebpf

The initial `connect4` program returns allow for every flow. It is a
load/attach bootstrap, not network policy yet.

use aya_ebpf::{macros::cgroup_sock_addr, programs::SockAddrContext};

#[cgroup_sock_addr(connect4)]
pub fn connect4(_ctx: SockAddrContext) -> i32 {
    1 // initially allow everything
}

## 3. Build guest-ebpf for the BPF target

bpfel-unknown-none

Using the Aya build tooling / bpf-linker. `guestd/build.rs` builds the artifact
only for Linux targets and embeds it into the static guestd binary. The build
produces the BPF ELF as a build artifact; it doesn't need to be manually
maintained as `network.bpf.o`.

## 4. Make guestd consume that build artifact

Use the Aya userspace crate (aya) in guestd.
Load the generated BPF ELF from the build output.
Initially, just load and attach `connect4` to `/sys/fs/cgroup/mvm-workload`.
The loaded `Ebpf` object is retained for the lifetime of guestd, keeping the
attachment alive.
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

The static guestd target build is checked on both x86_64 and aarch64 hosts;
the integration probe also checks that the guest kernel accepts
`cgroup_sock_addr/connect4`.

The workload and every exec session move into the protected cgroup in their
`pre_exec` path, before UID dropping and before untrusted code runs.

## 6. DNS enforcement

The first enforcing policy is limited to NIC-backed modes. TSI is intentionally
excluded because it is an insecure test backend. `guestd` derives the resolver
from the NIC gateway, populates `ALLOWED_DNS_IPV4`, and attaches a
`cgroup_skb/egress` program. IPv4 TCP/UDP port 53 is allowed only to that
resolver; IPv6 DNS is denied by the bootstrap program.

## 7. Then add a BPF map

guest-ebpf: lookup destination/port and return allow/deny.
guestd: parse the user's configuration and populate the map.
Keep all configuration parsing and validation in Rust userspace.
## 8. Only after that add connect6, bind4/6, etc.

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
