//! Sandbox lifecycle manager: create/start/exec/stop/rm, log streaming,
//! persistence, and the host side of the guest-agent control channel.

mod agent_conn;
mod gvproxy;

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use bytes::Bytes;
use mvm_common::{
    protocol, DataDir, Error, Result, Sandbox, SandboxId, SandboxSpec, SandboxState,
};
use mvm_image::{ImageStore, StoredImage};
use mvm_runtime::{spawn_shim, ShimConfig};
use mvm_storage::{default_driver, StorageDriver};
use tokio::sync::{broadcast, mpsc};

use agent_conn::AgentConn;

const LOG_BROADCAST_CAP: usize = 256;
const REGISTRY_FILE: &str = "sandboxes.json";

/// A live exec session inside one sandbox.
struct ExecSession {
    tx: mpsc::Sender<protocol::AgentEvent>,
}

struct SandboxEntry {
    info: Sandbox,
    log_tx: broadcast::Sender<Bytes>,
    agent: Option<AgentConn>,
    exec_sessions: HashMap<u32, ExecSession>,
    /// Write end of the guest console (attach_stdin sandboxes only).
    /// Shared so writes happen outside the registry lock.
    console_stdin: Option<Arc<Mutex<File>>>,
    stop_requested: bool,
}

impl SandboxEntry {
    fn new(info: Sandbox) -> Self {
        let (log_tx, _) = broadcast::channel(LOG_BROADCAST_CAP);
        Self {
            info,
            log_tx,
            agent: None,
            exec_sessions: HashMap::new(),
            console_stdin: None,
            stop_requested: false,
        }
    }
}

/// Central registry + lifecycle driver. Cloneable; shares all state.
#[derive(Clone)]
pub struct Manager {
    inner: Arc<ManagerInner>,
}

struct ManagerInner {
    data_dir: DataDir,
    images: ImageStore,
    storage: Box<dyn StorageDriver>,
    sandboxes: RwLock<HashMap<String, SandboxEntry>>,
    session_counter: AtomicU32,
    persist_lock: Mutex<()>,
    gvproxy_control: Option<PathBuf>,
}

impl Manager {
    /// Open (or create) the state directory and load the registry.
    pub fn new(data_dir: DataDir) -> Result<Self> {
        data_dir.ensure()?;
        let images = ImageStore::new(data_dir.clone())?;
        let storage = default_driver(data_dir.clone());
        tracing::info!("storage driver: {}", storage.name());

        let mut sandboxes = HashMap::new();
        let registry_path = data_dir.root().join(REGISTRY_FILE);
        if registry_path.exists() {
            let data = std::fs::read_to_string(&registry_path)?;
            let saved: Vec<Sandbox> = serde_json::from_str(&data).unwrap_or_default();
            for mut sb in saved {
                if sb.state == SandboxState::Running {
                    // The previous daemon is gone; if the shim is somehow
                    // still alive, keep it killable but mark it failed.
                    sb.state = match sb.pid {
                        Some(pid) if pid_alive(pid) => SandboxState::Failed,
                        _ => SandboxState::Stopped,
                    };
                }
                sandboxes.insert(sb.id.to_string(), SandboxEntry::new(sb));
            }
        }

        Ok(Self {
            inner: Arc::new(ManagerInner {
                data_dir,
                images,
                storage,
                sandboxes: RwLock::new(sandboxes),
                session_counter: AtomicU32::new(1),
                persist_lock: Mutex::new(()),
                gvproxy_control: std::env::var_os("MVM_GVPROXY_CONTROL").map(PathBuf::from),
            }),
        })
    }

    pub fn images(&self) -> &ImageStore {
        &self.inner.images
    }

    pub fn data_dir(&self) -> &DataDir {
        &self.inner.data_dir
    }

    /// Create a sandbox (does not start it).
    pub fn create(&self, spec: SandboxSpec) -> Result<Sandbox> {
        // Validate the image exists up-front.
        self.inner.images.get(&spec.image)?;
        mvm_network::validate(&spec.network)?;
        for p in &spec.ports {
            mvm_network::parse_port_map(p)?;
        }

        if let Some(name) = &spec.name {
            let sandboxes = self.inner.sandboxes.read().unwrap();
            if sandboxes.values().any(|e| e.spec().name.as_ref() == Some(name)) {
                return Err(Error::Other(format!("name '{name}' is already in use")));
            }
        }

        let sandbox = Sandbox::new(spec);
        std::fs::create_dir_all(self.inner.data_dir.sandbox_dir(&sandbox.id))?;
        self.inner
            .sandboxes
            .write()
            .unwrap()
            .insert(sandbox.id.to_string(), SandboxEntry::new(sandbox.clone()));
        self.persist()?;
        Ok(sandbox)
    }

    /// Start a created/stopped sandbox: prepare rootfs, inject the agent,
    /// spawn the VM shim, wire logs + control channel.
    pub async fn start(&self, id_or_name: &str) -> Result<Sandbox> {
        let id = self.resolve(id_or_name)?;
        {
            let sandboxes = self.inner.sandboxes.read().unwrap();
            let entry = sandboxes
                .get(&id)
                .ok_or_else(|| Error::SandboxNotFound(id_or_name.to_string()))?;
            if entry.info.state.is_alive() {
                return Err(Error::InvalidState("sandbox is already running".into()));
            }
        }

        let (spec, log_tx) = {
            let sandboxes = self.inner.sandboxes.read().unwrap();
            let entry = sandboxes.get(&id).unwrap();
            (entry.info.spec.clone(), entry.log_tx.clone())
        };

        let image: StoredImage = self.inner.images.get(&spec.image)?;
        let sandbox_id = SandboxId::from(id.clone());
        let sb_dir = self.inner.data_dir.sandbox_dir(&sandbox_id);

        // 1. Writable rootfs.
        let prepared = self.inner.storage.create(&sandbox_id, &image.rootfs)?;

        // 2. Inject the guest agent (enables exec). Not fatal if missing —
        // except for block-device roots, where the agent performs the pivot.
        let injected = self.inject_agent(&prepared.rootfs);
        if prepared.root_disk.is_some() {
            injected.as_ref().map_err(|e| {
                Error::Runtime(format!(
                    "the '{}' storage driver requires the mvm-agent binary: {e}",
                    self.inner.storage.name()
                ))
            })?;
            // Stage the image's ownership manifest next to the agent so it
            // can restore real file owners on the disk at first boot.
            if let Some(manifest) = &image.ownership {
                std::fs::copy(
                    manifest,
                    prepared
                        .rootfs
                        .join(protocol::GUEST_OWNERSHIP_PATH.trim_start_matches('/')),
                )?;
            }
        }
        let agent_socket = injected.ok().map(|_| {
            let sock = sb_dir.join("agent.sock");
            let _ = std::fs::remove_file(&sock);
            sock
        });

        // 3. Open the control-channel listener before the guest boots.
        let agent_listener = match &agent_socket {
            Some(path) => Some(tokio::net::UnixListener::bind(path)?),
            None => None,
        };

        // 4. Build the shim config.
        let exec = image.config.resolve_command(&spec.command);
        let mut env = image.config.env.clone();
        env.extend(spec.env.iter().cloned());
        let workdir = spec.workdir.clone().or(image.config.workdir.clone());
        let config = ShimConfig {
            sandbox_id: id.clone(),
            rootfs: prepared.rootfs,
            root_disk: prepared.root_disk,
            exec,
            env,
            workdir,
            vcpus: spec.vcpus,
            ram_mib: spec.ram_mib,
            network: spec.network.clone(),
            ports: spec.ports.clone(),
            mounts: spec.mounts.clone(),
            agent_socket: agent_socket.clone(),
        };

        // 5. Register gvproxy port forwards before booting the guest.
        let gvproxy_ports = self.configure_gvproxy_ports(&spec).await?;

        // 6. Spawn the shim.
        let handle = match spawn_shim(&config, &sb_dir, spec.attach_stdin) {
            Ok(handle) => handle,
            Err(error) => {
                self.remove_gvproxy_ports(gvproxy_ports).await;
                return Err(error);
            }
        };
        let pid = handle.child.id();
        let console_stdin = handle.console_stdin.map(|s| Arc::new(Mutex::new(s)));
        let mut child = handle.child;

        // 7. Log pump: shim stdout -> console.log + broadcast.
        let log_path = sb_dir.join("console.log");
        let pump_tx = log_tx.clone();
        std::thread::spawn(move || {
            let mut console = handle.console;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .expect("console.log");
            let mut buf = [0u8; 8192];
            loop {
                match console.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let _ = file.write_all(&buf[..n]);
                        let _ = file.flush();
                        let _ = pump_tx.send(Bytes::copy_from_slice(&buf[..n]));
                    }
                    Err(_) => break,
                }
            }
        });

        // 8. Update state.
        {
            let mut sandboxes = self.inner.sandboxes.write().unwrap();
            let entry = sandboxes.get_mut(&id).unwrap();
            entry.info.state = SandboxState::Running;
            entry.info.pid = Some(pid);
            entry.info.started_at = Some(chrono::Utc::now());
            entry.info.finished_at = None;
            entry.info.exit_code = None;
            entry.console_stdin = console_stdin;
            entry.stop_requested = false;
        }
        self.persist()?;

        // 9. Agent control channel accept task.
        if let Some(listener) = agent_listener {
            let mgr = self.clone();
            let cid = id.clone();
            tokio::spawn(async move {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        tracing::info!(sandbox = %cid, "guest agent connected");
                        mgr.attach_agent(&cid, stream).await;
                    }
                    Err(e) => tracing::warn!("agent accept failed: {e}"),
                }
            });
        }

        // 10. Child watcher.
        {
            let mgr = self.clone();
            let cid = id.clone();
            tokio::spawn(async move {
                let status = tokio::task::spawn_blocking(move || child.wait()).await;
                mgr.on_shim_exit(&cid, status.ok().and_then(|r| r.ok()));
            });
        }

        // 11. Exec-readiness barrier: don't return "running" until the guest
        // agent has connected (or the sandbox died / never had an agent).
        // Callers that exec immediately after start would otherwise race the
        // agent's vsock connection.
        if agent_socket.is_some() {
            const AGENT_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
            let deadline = std::time::Instant::now() + AGENT_WAIT;
            loop {
                {
                    let sandboxes = self.inner.sandboxes.read().unwrap();
                    match sandboxes.get(&id) {
                        Some(e) if e.agent.is_some() || !e.info.state.is_alive() => break,
                        None => break,
                        _ => {}
                    }
                }
                if std::time::Instant::now() >= deadline {
                    tracing::warn!(sandbox = %id, "agent did not connect within {AGENT_WAIT:?}");
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }

        Ok(self.get(&id)?)
    }

    /// Stop a running sandbox (SIGTERM the shim, escalate to SIGKILL).
    pub async fn stop(&self, id_or_name: &str) -> Result<Sandbox> {
        let id = self.resolve(id_or_name)?;
        let pid = {
            let mut sandboxes = self.inner.sandboxes.write().unwrap();
            let entry = sandboxes
                .get_mut(&id)
                .ok_or_else(|| Error::SandboxNotFound(id_or_name.to_string()))?;
            if !entry.info.state.is_alive() {
                return Err(Error::InvalidState("sandbox is not running".into()));
            }
            entry.stop_requested = true;
            entry.info.pid
        };

        if let Some(pid) = pid {
            terminate(pid, true).await;
        }

        self.persist()?;
        Ok(self.get(&id)?)
    }

    /// Remove a sandbox (stopping it first if needed) and its filesystem.
    pub async fn remove(&self, id_or_name: &str) -> Result<()> {
        let id = self.resolve(id_or_name)?;
        let alive = {
            let sandboxes = self.inner.sandboxes.read().unwrap();
            sandboxes
                .get(&id)
                .ok_or_else(|| Error::SandboxNotFound(id_or_name.to_string()))?
                .info
                .state
                .is_alive()
        };
        if alive {
            self.stop(&id).await?;
            // Give the watcher a moment to settle.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        let sandbox_id = SandboxId::from(id.clone());
        self.inner.storage.destroy(&sandbox_id)?;
        let sb_dir = self.inner.data_dir.sandbox_dir(&sandbox_id);
        if sb_dir.exists() {
            std::fs::remove_dir_all(&sb_dir)?;
        }
        self.inner.sandboxes.write().unwrap().remove(&id);
        self.persist()?;
        Ok(())
    }

    /// Get one sandbox record.
    pub fn get(&self, id_or_name: &str) -> Result<Sandbox> {
        let id = self.resolve(id_or_name)?;
        let sandboxes = self.inner.sandboxes.read().unwrap();
        sandboxes
            .get(&id)
            .map(|e| e.info.clone())
            .ok_or_else(|| Error::SandboxNotFound(id_or_name.to_string()))
    }

    /// List all sandboxes, newest first.
    pub fn list(&self) -> Vec<Sandbox> {
        let sandboxes = self.inner.sandboxes.read().unwrap();
        let mut out: Vec<_> = sandboxes.values().map(|e| e.info.clone()).collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        out
    }

    /// Read the console log backlog and optionally subscribe for live data.
    /// A follow subscription ends (channel closes) when the shim exits; for
    /// a sandbox that is not running, only the backlog is returned so
    /// followers don't hang waiting for a future boot.
    pub fn logs(
        &self,
        id_or_name: &str,
        follow: bool,
    ) -> Result<(Vec<u8>, Option<broadcast::Receiver<Bytes>>)> {
        let id = self.resolve(id_or_name)?;
        let sb_dir = self.inner.data_dir.sandbox_dir(&SandboxId::from(id.clone()));
        // Subscribe (under the same lock as the liveness check, so an exit
        // in between can't leave us on a channel nobody will ever close)
        // *before* reading the backlog file, so no bytes fall in the gap.
        let rx = if follow {
            let sandboxes = self.inner.sandboxes.read().unwrap();
            sandboxes
                .get(&id)
                .filter(|e| e.info.state.is_alive())
                .map(|e| e.log_tx.subscribe())
        } else {
            None
        };
        let backlog = std::fs::read(sb_dir.join("console.log")).unwrap_or_default();
        Ok((backlog, rx))
    }

    /// Run a command inside a running sandbox via the guest agent.
    /// Returns the session id (for stdin routing) and a stream of events
    /// (stdout/stderr/exit).
    #[allow(clippy::too_many_arguments)]
    pub async fn exec(
        &self,
        id_or_name: &str,
        argv: Vec<String>,
        env: Vec<String>,
        workdir: Option<String>,
        tty: bool,
        cols: u16,
        rows: u16,
    ) -> Result<(u32, mpsc::Receiver<protocol::AgentEvent>)> {
        let id = self.resolve(id_or_name)?;
        let (tx, rx) = mpsc::channel(64);
        let session_id = self.inner.session_counter.fetch_add(1, Ordering::SeqCst);

        // A concurrent `start` may have flipped the state to Running before
        // the agent's vsock channel is up; give it a moment rather than 500.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let sender = loop {
            {
                let mut sandboxes = self.inner.sandboxes.write().unwrap();
                let entry = sandboxes
                    .get_mut(&id)
                    .ok_or_else(|| Error::SandboxNotFound(id_or_name.to_string()))?;
                if !entry.info.state.is_alive() {
                    return Err(Error::InvalidState("sandbox is not running".into()));
                }
                if let Some(agent) = entry.agent.as_ref() {
                    let sender = agent.sender();
                    entry.exec_sessions.insert(session_id, ExecSession { tx });
                    break sender;
                }
                if std::time::Instant::now() >= deadline {
                    return Err(Error::Runtime(
                        "sandbox has no agent connection (was it started with exec support?)"
                            .into(),
                    ));
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };

        sender
            .send(protocol::AgentRequest::Exec {
                id: session_id,
                argv,
                env,
                workdir,
                tty,
                cols,
                rows,
            })
            .map_err(|_| Error::Runtime("agent channel closed".into()))?;
        Ok((session_id, rx))
    }

    /// Remove a stored image, refusing while any sandbox references it.
    pub fn remove_image(&self, name: &str) -> Result<()> {
        let target = self.inner.images.get(name)?;
        {
            let sandboxes = self.inner.sandboxes.read().unwrap();
            for entry in sandboxes.values() {
                if let Ok(img) = self.inner.images.get(&entry.info.spec.image) {
                    if img.rootfs == target.rootfs {
                        return Err(Error::InvalidState(format!(
                            "image '{name}' is in use by sandbox {} — remove it first",
                            entry.info.id
                        )));
                    }
                }
            }
        }
        self.inner.images.remove(name)
    }

    /// Write to the guest console's stdin (`attach_stdin` sandboxes only).
    /// `None` = EOF: the console is a tty, which has no pipe-style EOF, so
    /// VEOF (^D) is sent for the guest line discipline to translate, then
    /// the write end is dropped. Blocking write — call off the async runtime.
    pub fn console_stdin(&self, id_or_name: &str, data: Option<Vec<u8>>) -> Result<()> {
        let id = self.resolve(id_or_name)?;
        let (handle, payload, is_eof) = {
            let mut sandboxes = self.inner.sandboxes.write().unwrap();
            let entry = sandboxes
                .get_mut(&id)
                .ok_or_else(|| Error::SandboxNotFound(id_or_name.to_string()))?;
            match data {
                None => (entry.console_stdin.take(), vec![0x04u8], true),
                Some(bytes) => {
                    let h = entry.console_stdin.clone().ok_or_else(|| {
                        Error::InvalidState(
                            "sandbox console stdin is not attached (create it with -i)".into(),
                        )
                    })?;
                    (Some(h), bytes, false)
                }
            }
        };
        // Write outside the registry lock: a full pipe must not stall the
        // whole daemon.
        let Some(handle) = handle else {
            return Ok(()); // EOF on a non-attached console: nothing to do
        };
        let mut stdin = handle.lock().unwrap();
        let result = stdin.write_all(&payload).and_then(|_| stdin.flush());
        if is_eof {
            return Ok(()); // best effort; the handle drops (real EOF) here
        }
        result.map_err(|e| Error::Runtime(format!("console stdin: {e}")))
    }

    /// Kill a live exec session (SIGKILL in the guest).
    pub fn exec_kill(&self, id_or_name: &str, session: u32) -> Result<()> {
        self.send_to_agent(id_or_name, protocol::AgentRequest::Kill { id: session })
    }

    /// Resize a live tty exec session.
    pub fn exec_resize(&self, id_or_name: &str, session: u32, cols: u16, rows: u16) -> Result<()> {
        self.send_to_agent(
            id_or_name,
            protocol::AgentRequest::Resize { id: session, cols, rows },
        )
    }

    fn send_to_agent(&self, id_or_name: &str, req: protocol::AgentRequest) -> Result<()> {
        let id = self.resolve(id_or_name)?;
        let sandboxes = self.inner.sandboxes.read().unwrap();
        let entry = sandboxes
            .get(&id)
            .ok_or_else(|| Error::SandboxNotFound(id_or_name.to_string()))?;
        let agent = entry
            .agent
            .as_ref()
            .ok_or_else(|| Error::Runtime("sandbox has no agent connection".into()))?;
        agent
            .sender()
            .send(req)
            .map_err(|_| Error::Runtime("agent channel closed".into()))
    }

    /// Feed stdin data into a live exec session; `None` closes stdin (EOF).
    pub fn exec_stdin(&self, id_or_name: &str, session: u32, data: Option<Vec<u8>>) -> Result<()> {
        let req = match data {
            Some(data) => protocol::AgentRequest::Stdin { id: session, data },
            None => protocol::AgentRequest::StdinEof { id: session },
        };
        self.send_to_agent(id_or_name, req)
    }

    // ---- internals -------------------------------------------------------

    fn inject_agent(&self, rootfs: &std::path::Path) -> Result<()> {
        let agent = mvm_common::agent_binary()
            .ok_or_else(|| Error::Runtime("mvm-agent binary not found".into()))?;
        let dest_dir = rootfs.join(".mvm");
        std::fs::create_dir_all(&dest_dir)?;
        let dest = dest_dir.join("agent");
        std::fs::copy(&agent, &dest)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))?;
        Ok(())
    }

    /// Called by the watcher task when the shim process exits.
    fn on_shim_exit(&self, id: &str, status: Option<std::process::ExitStatus>) {
        let ports = {
            let sandboxes = self.inner.sandboxes.read().unwrap();
            sandboxes.get(id).and_then(|entry| {
                if matches!(entry.info.spec.network, mvm_common::NetworkMode::Gvproxy { .. })
                    && !entry.info.spec.ports.is_empty()
                {
                    Some(entry.info.spec.ports.clone())
                } else {
                    None
                }
            })
        };
        let mut sandboxes = self.inner.sandboxes.write().unwrap();
        if let Some(entry) = sandboxes.get_mut(id) {
            let code = status.and_then(|s| s.code());
            entry.info.pid = None;
            entry.info.finished_at = Some(chrono::Utc::now());
            entry.info.exit_code = code;
            entry.agent = None;
            entry.exec_sessions.clear();
            entry.console_stdin = None;
            entry.info.state = if entry.stop_requested {
                SandboxState::Stopped
            } else if status.is_none() {
                SandboxState::Failed
            } else {
                SandboxState::Exited
            };
            // Swap in a fresh log channel: dropping the registry's sender
            // lets follow streams end once the pump thread (which holds the
            // only other sender and drains the console pipe to EOF) exits.
            let (log_tx, _) = broadcast::channel(LOG_BROADCAST_CAP);
            entry.log_tx = log_tx;
            tracing::info!(sandbox = %id, state = %entry.info.state, "shim exited");
        }
        drop(sandboxes);
        let _ = self.persist();
        if let Some(ports) = ports {
            let mgr = self.clone();
            tokio::spawn(async move { mgr.remove_gvproxy_ports(Some(ports)).await });
        }
    }

    async fn configure_gvproxy_ports(&self, spec: &SandboxSpec) -> Result<Option<Vec<String>>> {
        if !matches!(spec.network, mvm_common::NetworkMode::Gvproxy { .. })
            || spec.ports.is_empty()
        {
            return Ok(None);
        }
        let control = self.inner.gvproxy_control.clone().ok_or_else(|| {
            Error::Network(
                "gvproxy port mappings require MVM_GVPROXY_CONTROL pointing to gvproxy's --listen Unix socket".into(),
            )
        })?;
        let ports = spec
            .ports
            .iter()
            .map(|p| mvm_network::parse_port_map(p))
            .collect::<Result<Vec<_>>>()?;
        tokio::task::spawn_blocking(move || gvproxy::expose(&control, &ports))
            .await
            .map_err(|e| Error::Network(format!("gvproxy setup task failed: {e}")))??;
        Ok(Some(spec.ports.clone()))
    }

    async fn remove_gvproxy_ports(&self, mappings: Option<Vec<String>>) {
        let Some(mappings) = mappings else { return };
        let Some(control) = self.inner.gvproxy_control.clone() else { return };
        let ports = mappings
            .iter()
            .filter_map(|p| mvm_network::parse_port_map(p).ok())
            .collect::<Vec<_>>();
        let _ = tokio::task::spawn_blocking(move || gvproxy::unexpose(&control, &ports)).await;
    }

    /// Attach a freshly connected guest agent stream.
    async fn attach_agent(&self, id: &str, stream: tokio::net::UnixStream) {
        use tokio::io::{AsyncReadExt, ReadHalf, WriteHalf};

        let (mut reader, writer): (ReadHalf<_>, WriteHalf<_>) = tokio::io::split(stream);
        let conn = AgentConn::spawn(writer);

        {
            let mut sandboxes = self.inner.sandboxes.write().unwrap();
            if let Some(entry) = sandboxes.get_mut(id) {
                entry.agent = Some(conn);
            } else {
                return;
            }
        }

        let mut decoder = protocol::FrameDecoder::default();
        let mut buf = [0u8; 16384];
        loop {
            let n = match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            let events: Vec<protocol::AgentEvent> = match decoder.feed(&buf[..n]) {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("bad agent frame: {e}");
                    break;
                }
            };
            for event in events {
                self.dispatch_agent_event(id, event);
            }
        }

        // Agent disconnected: clear it so exec fails fast.
        let mut sandboxes = self.inner.sandboxes.write().unwrap();
        if let Some(entry) = sandboxes.get_mut(id) {
            entry.agent = None;
        }
    }

    fn dispatch_agent_event(&self, id: &str, event: protocol::AgentEvent) {
        use protocol::AgentEvent::*;
        let mut sandboxes = self.inner.sandboxes.write().unwrap();
        let Some(entry) = sandboxes.get_mut(id) else { return };
        match event {
            Stdout { id: sid, .. } | Stderr { id: sid, .. } => {
                if let Some(session) = entry.exec_sessions.get(&sid) {
                    let _ = session.tx.try_send(event);
                }
            }
            Exit { id: sid, .. } => {
                if let Some(session) = entry.exec_sessions.remove(&sid) {
                    let _ = session.tx.try_send(event);
                }
            }
            Ready { workload_pid } => {
                tracing::info!(sandbox = %id, workload_pid, "agent ready");
            }
            WorkloadExit { code } => {
                tracing::info!(sandbox = %id, code, "workload exited");
            }
            Pong => {}
            Error { message } => {
                tracing::warn!(sandbox = %id, "agent error: {message}");
            }
        }
    }

    /// Resolve an id/name/prefix to a canonical sandbox id.
    pub fn resolve(&self, id_or_name: &str) -> Result<String> {
        let sandboxes = self.inner.sandboxes.read().unwrap();
        if sandboxes.contains_key(id_or_name) {
            return Ok(id_or_name.to_string());
        }
        let matches: Vec<&String> = sandboxes
            .keys()
            .filter(|k| {
                k.starts_with(id_or_name)
                    || sandboxes[*k].info.spec.name.as_deref() == Some(id_or_name)
            })
            .collect();
        match matches.len() {
            1 => Ok(matches[0].clone()),
            0 => Err(Error::SandboxNotFound(id_or_name.to_string())),
            _ => Err(Error::Other(format!("ambiguous sandbox '{id_or_name}'"))),
        }
    }

    fn persist(&self) -> Result<()> {
        let _guard = self.inner.persist_lock.lock().unwrap();
        let sandboxes: Vec<Sandbox> = {
            let map = self.inner.sandboxes.read().unwrap();
            map.values().map(|e| e.info.clone()).collect()
        };
        let path = self.inner.data_dir.root().join(REGISTRY_FILE);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&sandboxes)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

impl SandboxEntry {
    fn spec(&self) -> &SandboxSpec {
        &self.info.spec
    }
}

fn pid_alive(pid: u32) -> bool {
    PathBuf::from(format!("/proc/{pid}")).exists()
}

/// SIGTERM, wait, then SIGKILL. Kills the whole process group (the shim is
/// a session leader).
async fn terminate(pid: u32, escalate: bool) {
    let pgid = -(pid as i32); // shim called setsid: pgid == pid
    unsafe {
        libc::kill(pgid, libc::SIGTERM);
    }
    for _ in 0..30 {
        if !pid_alive(pid) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    if escalate && pid_alive(pid) {
        unsafe {
            libc::kill(pgid, libc::SIGKILL);
        }
        // Reap is done by the watcher task; just wait for /proc to vanish.
        for _ in 0..20 {
            if !pid_alive(pid) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
}
