use std::time::Duration;

use mvm_common::agent_api::AgentView;
use mvm_common::Sandbox;

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

    fn http() -> Result<reqwest::blocking::Client, String> {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| e.to_string())
    }

    pub fn list_agents(&self) -> Result<Vec<AgentView>, String> {
        let res = Self::http()?
            .get(format!("{}/api/v1/agents", self.base))
            .send()
            .map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(format!("GET /agents: {}", res.status()));
        }
        res.json::<Vec<AgentView>>().map_err(|e| e.to_string())
    }

    /// Full sandbox record (the detail modal's `mvm inspect` data).
    pub fn get_sandbox(&self, id: &str) -> Result<Sandbox, String> {
        let res = Self::http()?
            .get(format!("{}/api/v1/sandboxes/{id}", self.base))
            .send()
            .map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(format!("GET /sandboxes/{id}: {}", res.status()));
        }
        res.json::<Sandbox>().map_err(|e| e.to_string())
    }

    pub fn start_sandbox(&self, id: &str) -> Result<(), String> {
        self.action(id, "start")
    }

    pub fn stop_sandbox(&self, id: &str) -> Result<(), String> {
        self.action(id, "stop")
    }

    fn action(&self, id: &str, op: &str) -> Result<(), String> {
        let res = Self::http()?
            .post(format!("{}/api/v1/sandboxes/{id}/{op}", self.base))
            .json(&serde_json::json!({}))
            .send()
            .map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(format!("POST /sandboxes/{id}/{op}: {}", res.status()));
        }
        Ok(())
    }

    pub fn remove_sandbox(&self, id: &str) -> Result<(), String> {
        let res = Self::http()?
            .delete(format!("{}/api/v1/sandboxes/{id}", self.base))
            .send()
            .map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(format!("DELETE /sandboxes/{id}: {}", res.status()));
        }
        Ok(())
    }
}
