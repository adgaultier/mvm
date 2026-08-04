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

    pub fn start(&self, id: &str) -> Result<(), String> {
        self.action(id, "start")
    }

    pub fn stop(&self, id: &str) -> Result<(), String> {
        self.action(id, "stop")
    }

    /// Change the sandbox's vcpu/RAM allocation. Unlike start/stop this
    /// surfaces the daemon's error body: the values come from user input.
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
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(resp
                .json::<mvm_common::api::ErrorResponse>()
                .map(|e| e.error)
                .unwrap_or_else(|_| format!("resize failed ({status})")));
        }
        resp.json().map_err(|e| e.to_string())
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        self.http()
            .delete(format!("{}/api/v1/sandboxes/{id}", self.base))
            .send()
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn action(&self, id: &str, action: &str) -> Result<(), String> {
        self.http()
            .post(format!("{}/api/v1/sandboxes/{id}/{action}", self.base))
            .send()
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Non-following log fetch (periodic refresh approach).
    pub fn logs(&self, id: &str) -> Result<String, String> {
        use std::io::Read;
        let mut resp = self
            .http()
            .get(format!("{}/api/v1/sandboxes/{id}/logs", self.base))
            .query(&[("follow", "false")])
            .send()
            .map_err(|e| e.to_string())?;
        let mut s = String::new();
        resp.read_to_string(&mut s).map_err(|e| e.to_string())?;
        Ok(s)
    }
}
