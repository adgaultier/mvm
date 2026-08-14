//! mvm: microVM sandbox manager (docker-sbx style, on libkrun).

mod client;
mod run;
#[cfg(target_os = "linux")]
mod userns;

use clap::{Args, Parser, Subcommand};
use client::Client;
use mvm_common::{DataDir, Mount, NetworkMode, Sandbox, SandboxSpec};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "mvm",
    version,
    about = "MicroVM sandbox manager (libkrun + OCI images)"
)]
struct Cli {
    /// Daemon address.
    #[arg(
        long,
        global = true,
        env = "MVM_HOST",
        default_value = "http://127.0.0.1:24642"
    )]
    host: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon (HTTP API).
    Serve {
        #[arg(long, default_value = "127.0.0.1:24642")]
        addr: SocketAddr,
        /// Address for the VM-authenticated Agent API (`/agent/v1`).
        #[cfg(feature = "agent-api")]
        #[arg(long, env = "MVM_AGENT_ADDR", default_value = "127.0.0.1:24643")]
        agent_addr: SocketAddr,
    },
    /// Pull an OCI image.
    Pull { image: String },
    /// Load an OCI image layout archive (.tar) into the local store.
    Load {
        /// Archive path (an OCI layout .tar, e.g. `podman save --format oci-archive`).
        file: String,
        /// Name/tag to store it under (the archive carries no name).
        #[arg(long)]
        name: String,
    },
    /// List local images.
    Images,
    /// Remove a local image.
    Rmi { image: String },
    /// Create a sandbox (without starting it).
    Create(BoxArgs),
    /// Clone a sandbox: new record with the source's spec (flags override
    /// it); `--fork` carries the source's current disk.
    Clone {
        /// Source sandbox (id, id prefix, or name).
        sandbox: String,
        /// Carry the source's current disk into the clone.
        #[arg(long)]
        fork: bool,
        #[command(flatten)]
        overrides: CloneArgs,
    },
    /// Create and start a sandbox, stream its output (kept unless --rm).
    Run(BoxArgs),
    /// List sandboxes.
    Ps {
        /// Show all sandboxes (default shows running only).
        #[arg(short, long)]
        all: bool,
    },
    /// Start a created/stopped sandbox (by id, id prefix, or name).
    Start {
        sandbox: String,
        /// Attach to the console after starting (like `mvm attach`).
        #[arg(short, long)]
        attach: bool,
    },
    /// Attach the terminal to a running sandbox's console.
    ///
    /// Detach with ctrl-p ctrl-q, which leaves the workload running; whether
    /// stdin is forwarded and whether the workload has a pty were fixed when
    /// the sandbox was created (`-i` / `-t`).
    Attach {
        sandbox: String,
        /// Stream output only; don't forward local stdin.
        #[arg(long)]
        no_stdin: bool,
    },
    /// Stop a running sandbox.
    Stop { sandbox: String },
    /// Change a sandbox's vcpu/RAM allocation (applies on next start).
    Resize {
        sandbox: String,
        /// Number of vCPUs.
        #[arg(long)]
        cpus: Option<u8>,
        /// Memory in MiB.
        #[arg(short, long)]
        memory: Option<u32>,
        /// Restart the sandbox now so the new size takes effect.
        #[arg(long)]
        restart: bool,
    },
    /// Remove a sandbox.
    Rm {
        sandbox: String,
        /// Force removal of a running sandbox.
        #[arg(short, long)]
        force: bool,
    },
    /// Show sandbox details.
    Inspect { sandbox: String },
    /// Print sandbox console logs.
    Logs {
        sandbox: String,
        /// Follow log output.
        #[arg(short, long)]
        follow: bool,
        /// Show only the last N lines.
        #[arg(short = 'n', long)]
        tail: Option<usize>,
    },
    /// Execute a command inside a running sandbox.
    Exec {
        sandbox: String,
        /// Keep stdin open and forward it to the command.
        #[arg(short, long)]
        interactive: bool,
        /// Allocate a pseudo-terminal (combine with -i for a shell).
        #[arg(short, long)]
        tty: bool,
        /// Run as this user (name/uid[:group/gid]); default = the workload's
        /// own identity, like `docker exec`.
        #[arg(short, long)]
        user: Option<String>,
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,
    },
    /// Internal: VM shim entry point (not for direct use).
    #[command(hide = true, name = "__vm-shim")]
    VmShim { config: PathBuf },
}

#[derive(Args)]
pub(crate) struct BoxArgs {
    image: String,
    #[arg(trailing_var_arg = true)]
    command: Vec<String>,
    /// Sandbox name.
    #[arg(long)]
    name: Option<String>,
    /// Environment variables (KEY=VAL).
    #[arg(short, long)]
    env: Vec<String>,
    /// Bind mounts (host:guest[:ro]).
    #[arg(short, long)]
    volume: Vec<String>,
    /// Port mappings (hostPort:guestPort).
    #[arg(short, long)]
    publish: Vec<String>,
    /// Network mode: none | tsi | gvproxy[:<socket>] | tap:<dev>.
    #[arg(long, default_value = "none")]
    net: String,
    /// Number of vCPUs.
    #[arg(long, default_value = "1")]
    cpus: u8,
    /// Memory in MiB.
    #[arg(short, long, default_value = "512")]
    memory: u32,
    /// Working directory inside the guest.
    #[arg(short, long)]
    workdir: Option<String>,
    /// Run the workload as this user (name/uid[:group/gid]), overriding the
    /// image's USER.
    #[arg(short, long)]
    user: Option<String>,
    /// Remove the sandbox when the workload exits (docker --rm; kept by
    /// default).
    #[arg(long)]
    rm: bool,
    /// Keep the guest console's stdin open and forward local stdin to it.
    /// Recorded in the spec, so a `create`d sandbox stays attachable later.
    #[arg(short, long)]
    interactive: bool,
    /// Give the workload its own guest pty (and use raw mode locally while
    /// attached). Combine with -i for a shell.
    #[arg(short, long)]
    tty: bool,
    /// Security profile: default | strict. Strict installs an additional
    /// guest-side seccomp filter denying high-risk syscalls (bpf, keyctl,
    /// perf_event_open, userfaultfd, io_uring) in the workload's spawn path —
    /// for hostile/untrusted workloads.
    #[arg(long, default_value = "default")]
    security: String,
}

/// `mvm clone` flag overrides. Absent = inherit from the source. The source's
/// *name* is never inherited (it is still the source's, so reusing it is an
/// error); without `--name` the daemon assigns a generated name.
#[derive(Args)]
struct CloneArgs {
    /// Override the image.
    #[arg(long)]
    image: Option<String>,
    /// Sandbox name.
    #[arg(long)]
    name: Option<String>,
    /// Environment variables (KEY=VAL); *replaces* the source's env.
    #[arg(short, long)]
    env: Vec<String>,
    /// Bind mounts (host:guest[:ro]); *replaces* the source's mounts.
    #[arg(short, long)]
    volume: Vec<String>,
    /// Port mappings (hostPort:guestPort); *replaces* the source's ports.
    #[arg(short, long)]
    publish: Vec<String>,
    /// Network mode: none | tsi | gvproxy[:<socket>] | tap:<dev>.
    #[arg(long)]
    net: Option<String>,
    /// Number of vCPUs.
    #[arg(long)]
    cpus: Option<u8>,
    /// Memory in MiB.
    #[arg(short, long)]
    memory: Option<u32>,
    /// Working directory inside the guest.
    #[arg(short, long)]
    workdir: Option<String>,
    /// Run the workload as this user (name/uid[:group/gid]).
    #[arg(short, long)]
    user: Option<String>,
    /// Keep the guest console's stdin open and writable.
    #[arg(short, long, num_args = 0..=1, default_missing_value = "true", require_equals = true)]
    interactive: Option<bool>,
    /// Give the workload its own guest pty.
    #[arg(short, long, num_args = 0..=1, default_missing_value = "true", require_equals = true)]
    tty: Option<bool>,
    /// Security profile: default | strict.
    #[arg(long)]
    security: Option<String>,
    /// Override the guest command (docker-style trailing args).
    #[arg(trailing_var_arg = true)]
    command: Vec<String>,
}

fn main() {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mvm=info,warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let code = match cli.command {
        Command::Serve {
            addr,
            #[cfg(feature = "agent-api")]
            agent_addr,
        } => {
            // Must run before the tokio runtime exists (single-threaded
            // requirement of unshare) — may re-exec and never return.
            #[cfg(target_os = "linux")]
            userns::maybe_enter_userns();
            #[cfg(feature = "agent-api")]
            {
                serve(addr, agent_addr)
            }
            #[cfg(not(feature = "agent-api"))]
            {
                serve(addr)
            }
        }
        Command::VmShim { config } => vm_shim(&config),
        other => {
            let client = Client::new(&cli.host);
            if !client.ping() {
                eprintln!(
                    "error: cannot reach mvm daemon at {} — start it with `mvm serve`",
                    cli.host
                );
                std::process::exit(1);
            }
            match dispatch(&client, other) {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            }
        }
    };
    std::process::exit(code);
}

fn dispatch(client: &Client, cmd: Command) -> Result<i32, String> {
    match cmd {
        Command::Pull { image } => run::pull(client, &image),
        Command::Load { file, name } => run::load(client, &file, &name),
        Command::Images => {
            let images = client.list_images()?;
            println!("{:<40} {:<20} {:>10}", "IMAGE", "DIGEST", "SIZE");
            for img in images {
                let digest: String = img.digest.chars().take(19).collect();
                println!(
                    "{:<40} {:<20} {:>10}",
                    img.reference,
                    digest,
                    human_size(img.size)
                );
            }
            Ok(0)
        }
        Command::Rmi { image } => {
            client.remove_image(&image)?;
            println!("removed {image}");
            Ok(0)
        }
        Command::Create(args) => {
            let sb = client.create_sandbox(&args.spec()?)?;
            println!("{}", sb.id);
            if args.name.is_none() {
                if let Some(n) = &sb.spec.name {
                    eprintln!("mvm: created sandbox '{n}'");
                }
            }
            Ok(0)
        }
        Command::Clone {
            sandbox,
            fork,
            overrides,
        } => {
            let source = client.get_sandbox(&sandbox)?;
            let named = overrides.name.is_none();
            let sb =
                client.clone_sandbox(source.id.as_str(), &clone_spec(&source, overrides)?, fork)?;
            println!("{}", sb.id);
            if named {
                if let Some(n) = &sb.spec.name {
                    eprintln!("mvm: cloned as sandbox '{n}'");
                }
            }
            Ok(0)
        }
        Command::Run(args) => run::run(client, args),
        Command::Ps { all } => {
            let mut sandboxes = client.list_sandboxes()?;
            if !all {
                sandboxes.retain(|s| s.state.is_alive());
            }
            println!(
                "{:<14} {:<20} {:<24} {:<10} {:<6} COMMAND",
                "ID", "NAME", "IMAGE", "STATE", "EXIT"
            );
            for sb in sandboxes {
                println!(
                    "{:<14} {:<20} {:<24} {:<10} {:<6} {}",
                    sb.id.to_string(),
                    sb.spec.name.as_deref().unwrap_or("-"),
                    sb.spec.image,
                    sb.state.to_string(),
                    sb.exit_code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "-".into()),
                    sb.spec.command.join(" ")
                );
            }
            Ok(0)
        }
        Command::Start { sandbox, attach } => {
            let sb = client.start_sandbox(&sandbox)?;
            if attach {
                return run::attach(client, &sandbox, false);
            }
            println!("{}", sb.id);
            Ok(0)
        }
        Command::Attach { sandbox, no_stdin } => run::attach(client, &sandbox, no_stdin),
        Command::Stop { sandbox } => {
            let sb = client.stop_sandbox(&sandbox)?;
            println!("{}", sb.id);
            Ok(0)
        }
        Command::Resize {
            sandbox,
            cpus,
            memory,
            restart,
        } => {
            let sb = client.resize_sandbox(&sandbox, cpus, memory)?;
            let was_running = sb.state.is_alive();
            if was_running && restart {
                client.stop_sandbox(&sandbox)?;
                client.start_sandbox(&sandbox)?;
            }
            println!(
                "{} resized to {} vcpu / {} MiB{}",
                sb.id,
                sb.spec.vcpus,
                sb.spec.ram_mib,
                match (was_running, restart) {
                    // A microVM's allocation is fixed at boot.
                    (true, false) => " (restart to apply)",
                    (true, true) => " (restarted)",
                    _ => "",
                }
            );
            Ok(0)
        }
        Command::Rm { sandbox, force } => {
            if force {
                let _ = client.stop_sandbox(&sandbox);
            }
            client.remove_sandbox(&sandbox)?;
            println!("{}", sandbox);
            Ok(0)
        }
        Command::Inspect { sandbox } => {
            let sb = client.get_sandbox(&sandbox)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&sb).map_err(|e| e.to_string())?
            );
            Ok(0)
        }
        Command::Logs {
            sandbox,
            follow,
            tail,
        } => {
            // Not raw: `logs` only reads, so terminal queries must be
            // filtered out of the live tail the way they already are out of
            // the recording — a reader that never answers would have its own
            // terminal reply into its own input buffer.
            let mut resp = client.logs(&sandbox, follow, tail, false)?;
            let mut out = std::io::stdout();
            std::io::copy(&mut resp, &mut out).map_err(|e| e.to_string())?;
            Ok(0)
        }
        Command::Exec {
            sandbox,
            interactive,
            tty,
            user,
            command,
        } => run::exec(client, &sandbox, command, interactive, tty, user),
        Command::Serve { .. } | Command::VmShim { .. } => unreachable!(),
    }
}

/// Everything after the image is the guest command (docker-style trailing
/// args) — so an mvm option placed there would silently run *inside* the
/// guest. Reject the obvious cases instead.
fn validate_guest_command(command: &[String]) -> Result<(), String> {
    const MVM_FLAGS: &[&str] = &[
        "--name",
        "--env",
        "--volume",
        "--publish",
        "--net",
        "--cpus",
        "--memory",
        "--workdir",
        "--rm",
        "--host",
        "--interactive",
        "--tty",
        "--security",
    ];
    if let Some(first) = command.first() {
        if first.starts_with('-') {
            return Err(format!(
                "guest command starts with '{first}'; mvm options must come before the image \
                 (e.g. `mvm run --net gvproxy <image> <command>`)"
            ));
        }
    }
    if let Some(flag) = command.iter().find(|t| MVM_FLAGS.contains(&t.as_str())) {
        return Err(format!(
            "'{flag}' appears after the image and would be passed to the guest command; \
             place mvm options before the image (wrap the command in sh -c '...' if the \
             guest really needs a literal '{flag}')"
        ));
    }
    Ok(())
}

impl BoxArgs {
    pub(crate) fn spec(&self) -> Result<SandboxSpec, String> {
        validate_guest_command(&self.command)?;
        let network: NetworkMode = self.net.parse().map_err(|e: String| e)?;
        let mounts = self
            .volume
            .iter()
            .map(|v| parse_volume(v))
            .collect::<Result<Vec<_>, _>>()?;
        // With a terminal, carry our TERM into the guest (libkrun's init would
        // otherwise leave it at "linux"); an explicit -e TERM=... still wins.
        let mut env = self.env.clone();
        if self.tty && !env.iter().any(|kv| kv.starts_with("TERM=")) {
            if let Ok(term) = std::env::var("TERM") {
                env.push(format!("TERM={term}"));
            }
        }
        Ok(SandboxSpec {
            name: self.name.clone(),
            image: self.image.clone(),
            command: self.command.clone(),
            env,
            workdir: self.workdir.clone(),
            user: self.user.clone(),
            vcpus: self.cpus,
            ram_mib: self.memory,
            attach_stdin: self.interactive,
            tty: self.tty,
            tty_size: if self.tty { run::term_size() } else { None },
            network,
            ports: self.publish.clone(),
            mounts,
            security: self
                .security
                .parse()
                .map_err(|e: String| format!("--security: {e}"))?,
            labels: Default::default(),
        })
    }
}

/// Build the clone's spec: the source's, with every explicitly-given flag
/// applied. Absent flags inherit — the only structural difference from
/// `create` is that the source's *name* is not reused (it is still the
/// source's); without `--name` the daemon assigns a fresh generated name.
fn clone_spec(source: &Sandbox, overrides: CloneArgs) -> Result<SandboxSpec, String> {
    validate_guest_command(&overrides.command)?;
    let mut spec = source.spec.clone();
    if let Some(image) = overrides.image {
        spec.image = image;
    }
    spec.name = overrides.name;
    if !overrides.command.is_empty() {
        spec.command = overrides.command;
    }
    if !overrides.env.is_empty() {
        spec.env = overrides.env;
    }
    if !overrides.volume.is_empty() {
        spec.mounts = overrides
            .volume
            .iter()
            .map(|v| parse_volume(v))
            .collect::<Result<Vec<_>, _>>()?;
    }
    if !overrides.publish.is_empty() {
        spec.ports = overrides.publish;
    }
    if let Some(net) = overrides.net {
        spec.network = net.parse().map_err(|e: String| e)?;
    }
    if let Some(cpus) = overrides.cpus {
        spec.vcpus = cpus;
    }
    if let Some(memory) = overrides.memory {
        spec.ram_mib = memory;
    }
    if let Some(workdir) = overrides.workdir {
        spec.workdir = Some(workdir);
    }
    if let Some(user) = overrides.user {
        spec.user = Some(user);
    }
    if let Some(interactive) = overrides.interactive {
        spec.attach_stdin = interactive;
    }
    if let Some(tty) = overrides.tty {
        spec.tty = tty;
        spec.tty_size = if tty {
            // Mirror `create`: carry our TERM in, record the current size.
            if !spec.env.iter().any(|kv| kv.starts_with("TERM=")) {
                if let Ok(term) = std::env::var("TERM") {
                    spec.env.push(format!("TERM={term}"));
                }
            }
            run::term_size()
        } else {
            None
        };
    }
    if let Some(security) = overrides.security {
        spec.security = security
            .parse()
            .map_err(|e: String| format!("--security: {e}"))?;
    }
    Ok(spec)
}

fn parse_volume(v: &str) -> Result<Mount, String> {
    let mut parts = v.splitn(3, ':');
    let host = parts.next().ok_or("missing host path")?;
    let guest = parts.next().ok_or("volume must be host:guest[:ro]")?;
    let read_only = parts.next() == Some("ro");
    // Resolve the host path up front: libkrun's virtiofs opens it relative to
    // the daemon's cwd, so a relative (or dangling) path only fails later, at
    // VM boot, as a virtio-fs "BadActivate" panic. Canonicalize to an
    // absolute, symlink-free path and reject mounts that don't exist.
    let host = std::fs::canonicalize(host)
        .map_err(|e| format!("volume host path '{host}' is not accessible: {e}"))?;
    Ok(Mount {
        host,
        guest: PathBuf::from(guest),
        read_only,
    })
}

#[cfg(feature = "agent-api")]
fn serve(addr: SocketAddr, agent_addr: SocketAddr) -> i32 {
    let data_dir = match DataDir::resolve() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot resolve data dir: {e}");
            return 1;
        }
    };
    eprintln!("mvm: data dir {}", data_dir.root().display());
    // The manager owns a reqwest::blocking registry client, which must be
    // constructed outside the tokio runtime.
    let manager = match mvm_manager::Manager::new(data_dir) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: cannot initialize manager: {e}");
            return 1;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start async runtime: {e}");
            return 1;
        }
    };
    if let Err(e) = runtime.block_on(mvm_api::serve(addr, agent_addr, manager)) {
        eprintln!("error: server: {e}");
        return 1;
    }
    0
}

#[cfg(not(feature = "agent-api"))]
fn serve(addr: SocketAddr) -> i32 {
    let data_dir = match DataDir::resolve() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot resolve data dir: {e}");
            return 1;
        }
    };
    eprintln!("mvm: data dir {}", data_dir.root().display());
    // The manager owns a reqwest::blocking registry client, which must be
    // constructed outside the tokio runtime.
    let manager = match mvm_manager::Manager::new(data_dir) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: cannot initialize manager: {e}");
            return 1;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start async runtime: {e}");
            return 1;
        }
    };
    if let Err(e) = runtime.block_on(mvm_api::serve(addr, manager)) {
        eprintln!("error: server: {e}");
        return 1;
    }
    0
}

fn vm_shim(config: &Path) -> i32 {
    let config = match mvm_runtime::ShimConfig::load(config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mvm shim: cannot load config: {e}");
            return 1;
        }
    };
    match mvm_runtime::run_shim(&config) {
        Ok(()) => 0, // unreachable on success (start_enter exits the process)
        Err(e) => {
            eprintln!("mvm shim: {e}");
            1
        }
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1}{}", UNITS[unit])
}

#[allow(dead_code)]
fn print_sandbox(sb: &Sandbox) {
    println!("{} ({})", sb.id, sb.state);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn rejects_mvm_flags_after_image() {
        assert!(
            validate_guest_command(&cmd(&["sh", "-c", "apk add curl", "--net", "gvproxy"]))
                .is_err()
        );
        assert!(validate_guest_command(&cmd(&["--rm", "sh"])).is_err());
        assert!(validate_guest_command(&cmd(&["-x"])).is_err());
    }

    #[test]
    fn allows_normal_guest_commands() {
        assert!(validate_guest_command(&cmd(&[])).is_ok());
        assert!(validate_guest_command(&cmd(&["sh", "-c", "echo --net gvproxy"])).is_ok());
        assert!(validate_guest_command(&cmd(&["grep", "-e", "pattern", "file"])).is_ok());
        assert!(validate_guest_command(&cmd(&["cat", "-v", "file"])).is_ok());
    }

    #[test]
    fn volume_host_path_is_canonicalized_to_absolute() {
        let dir = std::env::temp_dir().join(format!("mvm-vol-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        let m = parse_volume(&format!("{}:/data", dir.display())).unwrap();
        assert!(m.host.is_absolute(), "host path should be absolute");

        let m = parse_volume(&format!("{}:/data:ro", dir.display())).unwrap();
        assert!(m.read_only);
        assert_eq!(m.guest, PathBuf::from("/data"));

        // A path that does not exist is rejected up front.
        assert!(parse_volume(&format!("{}/nope:/data", dir.display())).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
