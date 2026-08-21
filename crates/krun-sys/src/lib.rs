//! Raw FFI bindings to the subset of libkrun used by mvm.
//!
//! Signatures intentionally mirror `/usr/include/libkrun.h`.
//! If upgrading libkrun, review this file against that header.

#![allow(non_snake_case)]

use libc::{c_char, c_int, c_uchar, c_uint, c_ulonglong};

/// The virtiofs tag used for the root filesystem.
pub const KRUN_FS_ROOT_TAG: &str = "/dev/root";

pub const KRUN_LOG_LEVEL_OFF: u32 = 0;
pub const KRUN_LOG_LEVEL_ERROR: u32 = 1;
pub const KRUN_LOG_LEVEL_WARN: u32 = 2;
pub const KRUN_LOG_LEVEL_INFO: u32 = 3;
pub const KRUN_LOG_LEVEL_DEBUG: u32 = 4;
pub const KRUN_LOG_LEVEL_TRACE: u32 = 5;

/// Keep libkrun's default log target (stderr).
pub const KRUN_LOG_TARGET_DEFAULT: i32 = -1;

pub const KRUN_LOG_STYLE_AUTO: u32 = 0;
pub const KRUN_LOG_STYLE_ALWAYS: u32 = 1;
/// No terminal escape sequences — required for a log written to a file.
pub const KRUN_LOG_STYLE_NEVER: u32 = 2;

/// Don't let `RUST_LOG` and friends override the configured settings.
pub const KRUN_LOG_OPTION_NO_ENV: u32 = 1;

#[link(name = "krun")]
unsafe extern "C" {
    /// Creates a configuration context. Returns ctx id or negative errno.
    pub fn krun_create_ctx() -> i32;

    /// Frees an existing configuration context.
    pub fn krun_free_ctx(ctx_id: c_uint) -> c_int;

    /// Sets the basic configuration parameters for the microVM.
    pub fn krun_set_vm_config(ctx_id: c_uint, num_vcpus: c_uchar, ram_mib: c_uint) -> c_int;

    /// Sets the path to be used as root for the microVM (virtiofs).
    pub fn krun_set_root(ctx_id: c_uint, root_path: *const c_char) -> c_int;

    /// Adds a virtio-net device backed by a connected unixgram fd
    /// (disables the default TSI backend).
    pub fn krun_add_net_unixgram(
        ctx_id: c_uint,
        c_path: *const c_char,
        fd: c_int,
        c_mac: *const c_uchar,
        features: c_uint,
        flags: c_uint,
    ) -> c_int;

    /// Adds a raw disk image as a virtio-blk device.
    pub fn krun_add_disk(
        ctx_id: c_uint,
        c_block_id: *const c_char,
        c_disk_path: *const c_char,
        read_only: bool,
    ) -> c_int;

    /// Adds a virtio-fs device with an explicit tag, shared-memory size and
    /// read-only flag.
    pub fn krun_add_virtiofs3(
        ctx_id: c_uint,
        c_tag: *const c_char,
        c_path: *const c_char,
        shm_size: c_ulonglong,
        read_only: bool,
    ) -> c_int;

    /// Sets the working directory inside the microVM.
    pub fn krun_set_workdir(ctx_id: c_uint, workdir_path: *const c_char) -> c_int;

    /// Sets the executable, argv and envp to run inside the microVM.
    pub fn krun_set_exec(
        ctx_id: c_uint,
        exec_path: *const c_char,
        argv: *const *const c_char,
        envp: *const *const c_char,
    ) -> c_int;

    /// Sets environment variables (used when krun_set_exec is not).
    pub fn krun_set_env(ctx_id: c_uint, envp: *const *const c_char) -> c_int;

    /// Redirects console output to a file instead of stdio.
    pub fn krun_set_console_output(ctx_id: c_uint, c_filepath: *const c_char) -> c_int;

    /// Disables the implicit virtio-console attached to stdio.
    pub fn krun_disable_implicit_console(ctx_id: c_uint) -> c_int;

    /// Disables the implicit vsock device.
    pub fn krun_disable_implicit_vsock(ctx_id: c_uint) -> c_int;

    /// Adds a vsock port mapping backed by a host unix socket.
    /// `listen` = true if the guest expects connections initiated host-side.
    pub fn krun_add_vsock_port2(
        ctx_id: c_uint,
        port: c_uint,
        c_filepath: *const c_char,
        listen: bool,
    ) -> c_int;

    /// Adds an independent virtio-net device attached to a TAP device.
    pub fn krun_add_net_tap(
        ctx_id: c_uint,
        c_tap_name: *mut c_char,
        c_mac: *const c_uchar,
        features: c_uint,
        flags: c_uint,
    ) -> c_int;

    /// Uses gvproxy (vmnet) userspace networking; path to gvproxy socket.
    pub fn krun_set_gvproxy_path(ctx_id: c_uint, c_path: *mut c_char) -> c_int;

    /// Sets the port map ("host:guest" strings) for userspace networking.
    pub fn krun_set_port_map(ctx_id: c_uint, port_map: *const *const c_char) -> c_int;

    /// Sets rlimits for the workload ("RLIMIT_NOFILE=1024:1024" style).
    pub fn krun_set_rlimits(ctx_id: c_uint, rlimits: *const *const c_char) -> c_int;

    /// Configures libkrun logging level. Logs go to stderr — which for the
    /// shim *is* the guest console, so prefer `krun_init_log` with a
    /// dedicated fd.
    pub fn krun_set_log_level(level: c_uint) -> c_int;

    /// Initializes libkrun logging against a specific fd.
    /// `target_fd` = `KRUN_LOG_TARGET_DEFAULT` keeps stderr.
    pub fn krun_init_log(target_fd: c_int, level: c_uint, style: c_uint, options: c_uint) -> c_int;

    /// Starts and enters the microVM. Only returns on error; on success the
    /// process exits with the guest workload's exit status.
    pub fn krun_start_enter(ctx_id: c_uint) -> c_int;

    /// Returns an eventfd signalled when the microVM shuts down.
    pub fn krun_get_shutdown_eventfd(ctx_id: c_uint) -> c_int;
}
