/// Configure eth0 statically from MVM_NET_CONFIG="<ip>/<prefix>,<gateway>".
pub(super) fn configure_network() {
    let Ok(spec) = std::env::var("MVM_NET_CONFIG") else {
        return;
    };
    let (ip, prefix, gw) = match parse_net_config(&spec) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("mvm-guestd: bad MVM_NET_CONFIG '{spec}': {error}");
            return;
        }
    };

    unsafe {
        let sock = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
        if sock < 0 {
            return;
        }
        set_iface_flags(sock, "lo");

        let mut req = ifreq("eth0");
        if libc::ioctl(sock, libc::SIOCGIFADDR as _, &mut req) == 0 {
            let sin = &req.ifr_ifru.ifru_addr as *const libc::sockaddr as *const libc::sockaddr_in;
            if (*sin).sin_addr.s_addr != 0 {
                libc::close(sock);
                write_resolv_conf(gw);
                return;
            }
        }

        let mut req = ifreq("eth0");
        put_sockaddr_in(&mut req.ifr_ifru.ifru_addr, ip);
        if libc::ioctl(sock, libc::SIOCSIFADDR as _, &req) != 0 {
            eprintln!(
                "mvm-guestd: SIOCSIFADDR: {}",
                std::io::Error::last_os_error()
            );
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
            eprintln!("mvm-guestd: SIOCADDRT: {}", std::io::Error::last_os_error());
        }
        libc::close(sock);
    }
    write_resolv_conf(gw);
}

/// Return the resolver enforced by NIC-backed setup. TSI is intentionally
/// excluded from this security policy.
pub(super) fn dns_servers() -> Option<Vec<std::net::Ipv4Addr>> {
    if std::env::var_os("MVM_NET_TSI").is_some() {
        return None;
    }
    let spec = std::env::var("MVM_NET_CONFIG").ok()?;
    Some(vec![parse_net_config(&spec).ok()?.2])
}

fn parse_net_config(spec: &str) -> Result<(std::net::Ipv4Addr, u32, std::net::Ipv4Addr), String> {
    let (addr, gateway) = spec
        .split_once(',')
        .ok_or_else(|| "expected <ip>/<prefix>,<gateway>".to_string())?;
    let (ip, prefix) = addr
        .split_once('/')
        .ok_or_else(|| "missing address prefix".to_string())?;
    let ip = ip
        .parse()
        .map_err(|error| format!("invalid guest address: {error}"))?;
    let prefix: u32 = prefix
        .parse()
        .map_err(|error| format!("invalid network prefix: {error}"))?;
    if prefix > 32 {
        return Err(format!("network prefix {prefix} exceeds IPv4 limit 32"));
    }
    let gateway = gateway
        .parse()
        .map_err(|error| format!("invalid gateway address: {error}"))?;
    Ok((ip, prefix, gateway))
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

#[cfg(test)]
mod tests {
    use super::parse_net_config;

    #[test]
    fn parses_valid_static_network_config() {
        let parsed = parse_net_config("192.168.127.2/24,192.168.127.1").unwrap();
        assert_eq!(parsed.0.to_string(), "192.168.127.2");
        assert_eq!(parsed.1, 24);
        assert_eq!(parsed.2.to_string(), "192.168.127.1");
    }

    #[test]
    fn rejects_invalid_prefix_instead_of_clamping_it() {
        let error = parse_net_config("192.168.127.2/33,192.168.127.1").unwrap_err();
        assert!(error.contains("exceeds IPv4 limit 32"));
    }

    #[test]
    fn rejects_malformed_config() {
        assert!(parse_net_config("192.168.127.2,192.168.127.1").is_err());
        assert!(parse_net_config("192.168.127.2/24,not-an-ip").is_err());
    }
}
