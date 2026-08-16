/// Configure eth0 statically from MVM_NET_CONFIG="<ip>/<prefix>,<gateway>".
pub(super) fn configure_network() {
    let Ok(spec) = std::env::var("MVM_NET_CONFIG") else {
        return;
    };
    let parsed = (|| {
        let (addr, gw) = spec.split_once(',')?;
        let (ip, prefix) = addr.split_once('/')?;
        let ip: std::net::Ipv4Addr = ip.parse().ok()?;
        let prefix: u32 = prefix.parse().ok()?;
        let gw: std::net::Ipv4Addr = gw.parse().ok()?;
        Some((ip, prefix.min(32), gw))
    })();
    let Some((ip, prefix, gw)) = parsed else {
        eprintln!("mvm-agent: bad MVM_NET_CONFIG '{spec}'");
        return;
    };

    unsafe {
        let sock = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
        if sock < 0 {
            return;
        }
        set_iface_flags(sock, "lo");

        let mut req = ifreq("eth0");
        if libc::ioctl(sock, libc::SIOCGIFADDR as _, &mut req) == 0 {
            let sin = &req.ifr_ifru.ifru_addr as *const libc::sockaddr
                as *const libc::sockaddr_in;
            if (*sin).sin_addr.s_addr != 0 {
                libc::close(sock);
                write_resolv_conf(gw);
                return;
            }
        }

        let mut req = ifreq("eth0");
        put_sockaddr_in(&mut req.ifr_ifru.ifru_addr, ip);
        if libc::ioctl(sock, libc::SIOCSIFADDR as _, &req) != 0 {
            eprintln!("mvm-agent: SIOCSIFADDR: {}", std::io::Error::last_os_error());
            libc::close(sock);
            return;
        }
        let mask = std::net::Ipv4Addr::from(u32::MAX.checked_shl(32 - prefix).unwrap_or(0));
        let mut req = ifreq("eth0");
        put_sockaddr_in(&mut req.ifr_ifru.ifru_netmask, mask);
        libc::ioctl(sock, libc::SIOCSIFNETMASK as _, &req);
        set_iface_flags(sock, "eth0");

        let mut route: libc::rtentry = std::mem::zeroed();
        put_sockaddr_in_raw(&mut route.rt_dst, std::net::Ipv4Addr::UNSPECIFIED);
        put_sockaddr_in_raw(&mut route.rt_genmask, std::net::Ipv4Addr::UNSPECIFIED);
        put_sockaddr_in_raw(&mut route.rt_gateway, gw);
        route.rt_flags = libc::RTF_UP | libc::RTF_GATEWAY;
        if libc::ioctl(sock, libc::SIOCADDRT as _, &route) != 0 {
            eprintln!("mvm-agent: SIOCADDRT: {}", std::io::Error::last_os_error());
        }
        libc::close(sock);
    }
    write_resolv_conf(gw);
}

fn write_resolv_conf(dns: std::net::Ipv4Addr) {
    let _ = std::fs::create_dir_all("/etc");
    let _ = std::fs::write("/etc/resolv.conf", format!("nameserver {dns}\n"));
}

unsafe fn set_iface_flags(sock: i32, name: &str) {
    let mut req = ifreq(name);
    if unsafe { libc::ioctl(sock, libc::SIOCGIFFLAGS as _, &mut req) } == 0 {
        unsafe {
            req.ifr_ifru.ifru_flags |= (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;
            libc::ioctl(sock, libc::SIOCSIFFLAGS as _, &req);
        }
    }
}

fn ifreq(name: &str) -> libc::ifreq {
    let mut req: libc::ifreq = unsafe { std::mem::zeroed() };
    for (i, b) in name.as_bytes().iter().take(libc::IFNAMSIZ - 1).enumerate() {
        req.ifr_name[i] = *b as libc::c_char;
    }
    req
}

fn put_sockaddr_in(slot: &mut libc::sockaddr, ip: std::net::Ipv4Addr) {
    put_sockaddr_in_raw(slot, ip);
}

fn put_sockaddr_in_raw(slot: &mut libc::sockaddr, ip: std::net::Ipv4Addr) {
    let sin = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 0,
        sin_addr: libc::in_addr {
            s_addr: u32::from(ip).to_be(),
        },
        sin_zero: [0; 8],
    };
    unsafe {
        std::ptr::write(slot as *mut libc::sockaddr as *mut libc::sockaddr_in, sin);
    }
}
