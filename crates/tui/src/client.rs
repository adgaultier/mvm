//! Minimal blocking client for the mvm daemon (TUI subset).

use mvm_common::{ImageInfo, Sandbox};

#[derive(Clone)]
pub struct Client {
    base: String,
}

impl Client {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
        }
    }

    fn http(&self) -> reqwest::blocking::Client {
        reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new())
    }

    pub fn list_sandboxes(&self) -> Result<Vec<Sandbox>, String> {
        self.http()
            .get(format!("{}/api/v1/sandboxes", self.base))
            .send()
            .and_then(|r| r.json())
            .map_err(|e| e.to_string())
    }

    pub fn list_images(&self) -> Result<Vec<ImageInfo>, String> {
        self.http()
            .get(format!("{}/api/v1/images", self.base))
            .send()
            .and_then(|r| r.json())
            .map_err(|e| e.to_string())
    }

    /// Full sandbox record (the TUI's `mvm inspect`).
    pub fn get_sandbox(&self, id: &str) -> Result<Sandbox, String> {
        let resp = self
            .http()
            .get(format!("{}/api/v1/sandboxes/{id}", self.base))
            .send()
            .map_err(|e| e.to_string())?;
        Self::check(resp, "inspect")?
            .json()
            .map_err(|e| e.to_string())
    }

    pub fn start(&self, id: &str) -> Result<(), String> {
        self.action(id, "start")
    }

    pub fn stop(&self, id: &str) -> Result<(), String> {
        self.action(id, "stop")
    }

    /// Change the sandbox's vcpu/RAM allocation.
    pub fn resize(&self, id: &str, vcpus: u8, ram_mib: u32) -> Result<Sandbox, String> {
        let resp = self
            .http()
            .post(format!("{}/api/v1/sandboxes/{id}/resize", self.base))
            .json(&mvm_common::api::SandboxResizeRequest {
                vcpus: Some(vcpus),
                ram_mib: Some(ram_mib),
            })
            .send()
            .map_err(|e| e.to_string())?;
        Self::check(resp, "resize")?.json().map_err(|e| e.to_string())
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        let resp = self
            .http()
            .delete(format!("{}/api/v1/sandboxes/{id}", self.base))
            .send()
            .map_err(|e| e.to_string())?;
        Self::check(resp, "remove").map(|_| ())
    }

    /// Turn a non-2xx response into the daemon's own message. Swallowing these
    /// let the TUI report success for work that never happened — a refused
    /// start would still have said "restarted".
    fn check(
        resp: reqwest::blocking::Response,
        what: &str,
    ) -> Result<reqwest::blocking::Response, String> {
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status();
        Err(resp
            .json::<mvm_common::api::ErrorResponse>()
            .map(|e| e.error)
            .unwrap_or_else(|_| format!("{what} failed ({status})")))
    }

    fn action(&self, id: &str, action: &str) -> Result<(), String> {
        let resp = self
            .http()
            .post(format!("{}/api/v1/sandboxes/{id}/{action}", self.base))
            .send()
            .map_err(|e| e.to_string())?;
        Self::check(resp, action).map(|_| ())
    }
}
