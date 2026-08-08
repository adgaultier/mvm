//! mvm-agent entry point.
//!
//! The real agent only runs inside Linux guests (see linux.rs); the musl
//! cross-builds compile it directly. On non-Linux hosts this stub keeps
//! `cargo build --workspace` green and explains the mistake if the host
//! binary is ever executed.

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
fn main() {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "mvm-agent only runs inside Linux guests; build it for a musl target \
         (e.g. cargo zigbuild --release -p mvm-agent --target aarch64-unknown-linux-musl)"
    );
    std::process::exit(1);
}
