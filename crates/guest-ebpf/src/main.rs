#![no_std]
#![no_main]

use aya_ebpf::{
    macros::{cgroup_skb, cgroup_sock_addr, map},
    maps::HashMap,
    programs::{SkBuffContext, SockAddrContext},
};

#[map]
static ALLOWED_DNS_IPV4: HashMap<u32, u8> = HashMap::with_max_entries(16, 0);

/// Bootstrap hook: prove that MVM can install a cgroup socket policy.
/// Returning one permits the operation; policy is added in a later phase.
#[cgroup_sock_addr(connect4)]
pub fn connect4(_ctx: SockAddrContext) -> i32 {
    1
}

#[cgroup_skb(egress)]
pub fn dns_egress(ctx: SkBuffContext) -> i32 {
    let version = match ctx.load::<u8>(0) {
        Ok(value) => value >> 4,
        Err(_) => return 0,
    };
    let (protocol, destination, port_offset) = if version == 4 {
        let ihl = match ctx.load::<u8>(0) {
            Ok(value) => ((value & 0x0f) as usize) * 4,
            Err(_) => return 0,
        };
        if ihl < 20 {
            return 0;
        }
        let fragment = match ctx.load::<u16>(6) {
            Ok(value) => u16::from_be(value),
            Err(_) => return 0,
        };
        if fragment & 0x3fff != 0 {
            return 0;
        }
        let protocol = match ctx.load::<u8>(9) {
            Ok(value) => value,
            Err(_) => return 0,
        };
        let destination = match ctx.load::<u32>(16) {
            Ok(value) => u32::from_be(value),
            Err(_) => return 0,
        };
        (protocol, Some(destination), ihl)
    } else if version == 6 {
        let protocol = match ctx.load::<u8>(6) {
            Ok(value) => value,
            Err(_) => return 0,
        };
        (protocol, None, 40)
    } else {
        return 1;
    };
    if protocol != 6 && protocol != 17 {
        return 1;
    }
    let port = match ctx.load::<u16>(port_offset + 2) {
        Ok(value) => u16::from_be(value),
        Err(_) => return 0,
    };
    if port != 53 {
        return 1;
    }
    match destination {
        Some(ip) => unsafe { ALLOWED_DNS_IPV4.get(&ip).is_some() as i32 },
        None => 0,
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
