#![no_std]
#![no_main]

use aya_ebpf::{macros::cgroup_sock_addr, programs::SockAddrContext};

/// Bootstrap hook: prove that MVM can install a cgroup socket policy.
/// Returning one permits the operation; policy is added in a later phase.
#[cgroup_sock_addr(connect4)]
pub fn connect4(_ctx: SockAddrContext) -> i32 {
    1
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
