//! mvm-guestd entry point.
//!
//! The real guestd only runs inside Linux guests (see linux.rs); the musl
//! cross-builds compile it directly. On non-Linux hosts this stub keeps
//! `cargo build --workspace` green and explains the mistake if the host
//! binary is ever executed.

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
mod identity;

#[cfg(target_os = "linux")]
mod pty;

#[cfg(target_os = "linux")]
mod network;

#[cfg(target_os = "linux")]
mod seccomp;

#[cfg(target_os = "linux")]
fn main() {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "mvm-guestd only runs inside Linux guests; build it for a musl target \
         (e.g. cargo zigbuild --release -p mvm-guestd --target aarch64-unknown-linux-musl)"
    );
    std::process::exit(1);
}
