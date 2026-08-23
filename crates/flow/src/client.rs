use std::time::Duration;

use mvm_common::agent_api::AgentView;

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
}
