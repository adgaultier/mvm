//! Blocking HTTP client for the mvm daemon.

use mvm_common::api::{ErrorResponse, ExecRequest, PullRequest};
use mvm_common::{ImageInfo, Sandbox, SandboxSpec};

pub struct Client {
    base: String,
    http: reqwest::blocking::Client,
}

impl Client {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            http: reqwest::blocking::Client::new(),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn ping(&self) -> bool {
        self.http
            .get(format!("{}/health", self.base))
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    pub fn list_sandboxes(&self) -> Result<Vec<Sandbox>, String> {
        self.get_json("/api/v1/sandboxes")
    }

    pub fn get_sandbox(&self, id: &str) -> Result<Sandbox, String> {
        self.get_json(&format!("/api/v1/sandboxes/{id}"))
    }

    pub fn create_sandbox(&self, spec: &SandboxSpec) -> Result<Sandbox, String> {
        self.post_json("/api/v1/sandboxes", spec)
    }

    pub fn start_sandbox(&self, id: &str) -> Result<Sandbox, String> {
        self.post_json(&format!("/api/v1/sandboxes/{id}/start"), &serde_json::json!({}))
    }

    pub fn stop_sandbox(&self, id: &str) -> Result<Sandbox, String> {
        self.post_json(&format!("/api/v1/sandboxes/{id}/stop"), &serde_json::json!({}))
    }

    pub fn remove_sandbox(&self, id: &str) -> Result<(), String> {
        let resp = self
            .http
            .delete(format!("{}/api/v1/sandboxes/{id}", self.base))
            .send()
            .map_err(|e| e.to_string())?;
        Self::expect(resp, &[204]).map(|_| ())
    }

    pub fn list_images(&self) -> Result<Vec<ImageInfo>, String> {
        self.get_json("/api/v1/images")
    }

    pub fn remove_image(&self, name: &str) -> Result<(), String> {
        let resp = self
            .http
            .delete(format!("{}/api/v1/images/{name}", self.base))
            .send()
            .map_err(|e| e.to_string())?;
        Self::expect(resp, &[204]).map(|_| ())
    }

    /// Streaming pull: returns the raw streaming response.
    pub fn pull(&self, reference: &str) -> Result<reqwest::blocking::Response, String> {
        let resp = self
            .http
            .post(format!("{}/api/v1/images/pull", self.base))
            .json(&PullRequest {
                reference: reference.to_string(),
            })
            .send()
            .map_err(|e| e.to_string())?;
        Self::expect(resp, &[200])
    }

    /// Streaming logs.
    pub fn logs(&self, id: &str, follow: bool) -> Result<reqwest::blocking::Response, String> {
        let resp = self
            .http
            .get(format!("{}/api/v1/sandboxes/{id}/logs", self.base))
            .query(&[("follow", follow.to_string())])
            .send()
            .map_err(|e| e.to_string())?;
        Self::expect(resp, &[200])
    }

    /// Streaming exec. Returns the exec session id (for stdin routing)
    /// and the framed event stream.
    #[allow(clippy::too_many_arguments)]
    pub fn exec(
        &self,
        id: &str,
        argv: Vec<String>,
        env: Vec<String>,
        workdir: Option<String>,
        tty: bool,
        cols: u16,
        rows: u16,
    ) -> Result<(u32, reqwest::blocking::Response), String> {
        let resp = self
            .http
            .post(format!("{}/api/v1/sandboxes/{id}/exec", self.base))
            .json(&ExecRequest {
                argv,
                env,
                workdir,
                tty,
                cols,
                rows,
            })
            .send()
            .map_err(|e| e.to_string())?;
        let resp = Self::expect(resp, &[200])?;
        let session = resp
            .headers()
            .get("x-mvm-exec-session")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .ok_or("daemon did not return an exec session id")?;
        Ok((session, resp))
    }

    /// Send stdin bytes to a live exec session.
    pub fn exec_stdin(&self, id: &str, session: u32, data: Vec<u8>) -> Result<(), String> {
        let resp = self
            .http
            .post(format!(
                "{}/api/v1/sandboxes/{id}/exec/{session}/stdin",
                self.base
            ))
            .body(data)
            .send()
            .map_err(|e| e.to_string())?;
        Self::expect(resp, &[204]).map(|_| ())
    }

    /// Resize a live tty exec session.
    pub fn exec_resize(&self, id: &str, session: u32, cols: u16, rows: u16) -> Result<(), String> {
        let resp = self
            .http
            .post(format!(
                "{}/api/v1/sandboxes/{id}/exec/{session}/resize",
                self.base
            ))
            .json(&mvm_common::api::ResizeRequest { cols, rows })
            .send()
            .map_err(|e| e.to_string())?;
        Self::expect(resp, &[204]).map(|_| ())
    }

    /// Close stdin of a live exec session.
    pub fn exec_stdin_eof(&self, id: &str, session: u32) -> Result<(), String> {
        let resp = self
            .http
            .post(format!(
                "{}/api/v1/sandboxes/{id}/exec/{session}/stdin?eof=true",
                self.base
            ))
            .send()
            .map_err(|e| e.to_string())?;
        Self::expect(resp, &[204]).map(|_| ())
    }

    // ---- helpers ----

    fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let resp = self
            .http
            .get(format!("{}{}", self.base, path))
            .send()
            .map_err(|e| e.to_string())?;
        Self::expect(resp, &[200])?.json().map_err(|e| e.to_string())
    }

    fn post_json<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, String> {
        let resp = self
            .http
            .post(format!("{}{}", self.base, path))
            .json(body)
            .send()
            .map_err(|e| e.to_string())?;
        Self::expect(resp, &[200, 201])?
            .json()
            .map_err(|e| e.to_string())
    }

    fn expect(
        resp: reqwest::blocking::Response,
        ok: &[u16],
    ) -> Result<reqwest::blocking::Response, String> {
        let status = resp.status().as_u16();
        if ok.contains(&status) {
            return Ok(resp);
        }
        let msg = resp
            .json::<ErrorResponse>()
            .map(|e| e.error)
            .unwrap_or_else(|_| format!("daemon returned status {status}"));
        Err(msg)
    }
}
