use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use anyhow::Result;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProxyConfig {
    pub listen_addr: String,
    pub routes: HashMap<String, RouteConfig>,
    pub global_timeout_ms: u64,
    pub max_connections: usize,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct RouteConfig {
    pub upstream: String,
    pub timeout_ms: Option<u64>,
    pub retry_count: Option<u32>,
    #[serde(default)]  // This makes priority optional with a default of 0
    pub priority: Option<i32>,
}

pub fn load_config(path: &str) -> Result<ProxyConfig> {
    let content = fs::read_to_string(path)?;
    let config: ProxyConfig = serde_yaml::from_str(&content)?;
    Ok(config)
}
