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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RouteConfig {
    pub upstream: String,
    pub timeout_ms: Option<u64>,
    pub retry_count: Option<u32>,
    pub preserve_host_header: Option<bool>,
    pub strip_path_prefix: bool,  // Add this field
}

pub fn load_config(path: &str) -> Result<ProxyConfig> {
    let content = fs::read_to_string(path)?;
    let config: ProxyConfig = serde_yaml::from_str(&content)?;
    Ok(config)
}