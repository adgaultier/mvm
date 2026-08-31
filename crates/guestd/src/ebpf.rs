use std::fs::{self, File};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use aya::programs::{CgroupAttachMode, CgroupSockAddr};
use aya::Ebpf;

pub(crate) const WORKLOAD_CGROUP: &str = "/sys/fs/cgroup/mvm-workload";

pub(crate) struct Installed {
    pub(crate) _bpf: Ebpf,
    pub(crate) cgroup_procs: PathBuf,
}

pub(crate) fn install() -> Result<Installed, Box<dyn std::error::Error>> {
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

    Ok(Installed {
        _bpf: bpf,
        cgroup_procs: cgroup.join("cgroup.procs"),
    })
}
