use std::fs::{self, File};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use aya::maps::HashMap;
use aya::programs::{CgroupAttachMode, CgroupSkb, CgroupSkbAttachType, CgroupSockAddr};
use aya::Ebpf;

pub(crate) const WORKLOAD_CGROUP: &str = "/sys/fs/cgroup/mvm-workload";

pub(crate) struct Installed {
    pub(crate) _bpf: Ebpf,
    pub(crate) cgroup_procs: PathBuf,
}

pub(crate) fn install(
    dns_servers: Option<Vec<std::net::Ipv4Addr>>,
) -> Result<Installed, Box<dyn std::error::Error>> {
    let root = Path::new("/sys/fs/cgroup");
    fs::create_dir_all(root)?;
    let source = std::ffi::CString::new("cgroup2")?;
    let target = std::ffi::CString::new(root.as_os_str().as_bytes())?;
    unsafe {
        if libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            source.as_ptr(),
            0,
            std::ptr::null(),
        ) != 0
            && std::io::Error::last_os_error().raw_os_error() != Some(libc::EBUSY)
        {
            return Err(Box::new(std::io::Error::last_os_error()));
        }
    }

    let cgroup = Path::new(WORKLOAD_CGROUP);
    fs::create_dir_all(cgroup)?;
    let cgroup_file = File::open(cgroup)?;
    let mut bpf = Ebpf::load(aya::include_bytes_aligned!(concat!(
        env!("OUT_DIR"),
        "/mvm-guest-ebpf"
    )))?;
    let program: &mut CgroupSockAddr = bpf
        .program_mut("connect4")
        .ok_or("embedded connect4 program is missing")?
        .try_into()?;
    program.load()?;
    program.attach(cgroup_file, CgroupAttachMode::Single)?;

    if let Some(dns_servers) = dns_servers {
        let cgroup_file = File::open(cgroup)?;
        let dns_program: &mut CgroupSkb = bpf
            .program_mut("dns_egress")
            .ok_or("embedded dns_egress program is missing")?
            .try_into()?;
        dns_program.load()?;
        dns_program.attach(
            cgroup_file,
            CgroupSkbAttachType::Egress,
            CgroupAttachMode::Single,
        )?;
        let mut allowed: HashMap<_, u32, u8> = HashMap::try_from(
            bpf.map_mut("ALLOWED_DNS_IPV4")
                .ok_or("DNS map is missing")?,
        )?;
        for server in dns_servers {
            allowed.insert(u32::from_be_bytes(server.octets()), 1, 0)?;
        }
    }

    Ok(Installed {
        _bpf: bpf,
        cgroup_procs: cgroup.join("cgroup.procs"),
    })
}
