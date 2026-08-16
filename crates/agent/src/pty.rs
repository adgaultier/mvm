use std::os::fd::OwnedFd;
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use crate::identity::{apply_user, GuestUser};

pub(super) fn spawn_tty_workload(
    workload_argv: &[String],
    size: Option<(u16, u16)>,
    user: &GuestUser,
    strict: bool,
) -> Option<(
    std::process::Child,
    std::thread::JoinHandle<()>,
    Option<OwnedFd>,
)> {
    let winsize = size.map(|(cols, rows)| libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    });
    let mut fds = [-1; 2];
    let rc = unsafe {
        libc::openpty(
            &mut fds[0],
            &mut fds[1],
            std::ptr::null_mut(),
            std::ptr::null(),
            winsize
                .as_ref()
                .map(|w| w as *const libc::winsize)
                .unwrap_or(std::ptr::null()),
        )
    };
    if rc != 0 {
        eprintln!(
            "mvm-agent: openpty failed: {}",
            std::io::Error::last_os_error()
        );
        return None;
    }
    interactive_termios(fds[1]);
    if !user.is_root() {
        unsafe { libc::fchown(fds[1], user.uid, user.gid) };
    }
    let master = unsafe { std::fs::File::from_raw_fd(fds[0]) };
    let slave = unsafe { std::fs::File::from_raw_fd(fds[1]) };
    let slave_out = slave.try_clone().ok()?;
    let slave_err = slave.try_clone().ok()?;
    let mut cmd = Command::new(&workload_argv[0]);
    cmd.args(&workload_argv[1..])
        .stdin(Stdio::from(slave))
        .stdout(Stdio::from(slave_out))
        .stderr(Stdio::from(slave_err));
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    if strict {
        crate::linux::apply_strict_seccomp(&mut cmd);
    }
    apply_user(&mut cmd, user);
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("mvm-agent: failed to spawn {:?}: {e}", workload_argv[0]);
            return None;
        }
    };

    let input_fd = unsafe { libc::dup(0) };
    let output_fd = unsafe { libc::dup(1) };
    if input_fd < 0 || output_fd < 0 {
        return Some((child, std::thread::spawn(|| {}), None));
    }
    let mut input = unsafe { std::fs::File::from_raw_fd(input_fd) };
    let Ok(mut input_master) = master.try_clone() else {
        return Some((child, std::thread::spawn(|| {}), None));
    };
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut input, &mut input_master);
    });
    let mut output = unsafe { std::fs::File::from_raw_fd(output_fd) };
    let mut output_master = master;
    let console_pty = output_master.try_clone().ok().map(OwnedFd::from);
    let output_handle = std::thread::spawn(move || {
        let _ = std::io::copy(&mut output_master, &mut output);
    });
    Some((child, output_handle, console_pty))
}

pub(super) fn raw_console_termios() {
    unsafe {
        let mut term = std::mem::zeroed();
        if libc::tcgetattr(0, &mut term) == 0 {
            libc::cfmakeraw(&mut term);
            let _ = libc::tcsetattr(0, libc::TCSANOW, &term);
        }
    }
}

pub(super) fn normalize_console_termios() {
    unsafe {
        let mut term = std::mem::zeroed();
        if libc::tcgetattr(0, &mut term) == 0 {
            term.c_oflag &= !libc::ONLCR;
            let _ = libc::tcsetattr(0, libc::TCSANOW, &term);
        }
    }
}

fn interactive_termios(slave: RawFd) {
    unsafe {
        let mut term: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(slave, &mut term) != 0 {
            return;
        }
        term.c_iflag |= libc::ICRNL | libc::IXON | libc::BRKINT;
        term.c_oflag |= libc::OPOST | libc::ONLCR;
        term.c_lflag |= libc::ISIG
            | libc::ICANON
            | libc::ECHO
            | libc::ECHOE
            | libc::ECHOK
            | libc::ECHOCTL
            | libc::ECHOKE
            | libc::IEXTEN;
        let _ = libc::tcsetattr(slave, libc::TCSANOW, &term);
    }
}

pub(super) fn ensure_devpts() {
    let _ = std::fs::create_dir_all("/dev/pts");
    let c_src = std::ffi::CString::new("devpts").unwrap();
    let c_target = std::ffi::CString::new("/dev/pts").unwrap();
    let c_data = std::ffi::CString::new("mode=0620,ptmxmode=0666").unwrap();
    unsafe {
        libc::mount(
            c_src.as_ptr(),
            c_target.as_ptr(),
            c_src.as_ptr(),
            0,
            c_data.as_ptr() as *const libc::c_void,
        );
    }
    let needs_ptmx_link = std::fs::symlink_metadata("/dev/ptmx")
        .map(|m| !m.file_type().is_symlink())
        .unwrap_or(true);
    if needs_ptmx_link {
        let _ = std::fs::remove_file("/dev/ptmx");
        let _ = std::os::unix::fs::symlink("/dev/pts/ptmx", "/dev/ptmx");
    }
}
