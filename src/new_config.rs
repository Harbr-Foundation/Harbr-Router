use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use crate::proxies::{ProxyInstance, HostConfig};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewProxyConfig {
    pub proxy_instances: Vec<ProxyInstance>,
    pub global: GlobalConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalConfig {
    pub metrics: MetricsConfig,
    pub health_check: HealthCheckServerConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub listen_addr: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckServerConfig {
    pub enabled: bool,
    pub listen_addr: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub level: String,
    pub format: String,
}

impl NewProxyConfig {
    pub fn load_from_file(path: &str) -> Result<Self> {
        let content = fs::read_to_string(path)?;
        
        let config: NewProxyConfig = if path.ends_with(".toml") {
            toml::from_str(&content)?
        } else if path.ends_with(".json") {
            serde_json::from_str(&content)?
        } else {
            // Default to YAML
            serde_yaml::from_str(&content)?
        };
        
        Ok(config)
    }
    
    pub fn save_to_file(&self, path: &str) -> Result<()> {
        let content = if path.ends_with(".toml") {
            toml::to_string(self)?
        } else if path.ends_with(".json") {
            serde_json::to_string_pretty(self)?
        } else {
            // Default to YAML
            serde_yaml::to_string(self)?
        };
        
        fs::write(path, content)?;
        Ok(())
    }
    
    pub fn validate(&self) -> Result<()> {
        for instance in &self.proxy_instances {
            if instance.name.is_empty() {
                return Err(anyhow::anyhow!("Proxy instance name cannot be empty"));
            }
            
            if instance.proxy_type.is_empty() {
                return Err(anyhow::anyhow!("Proxy instance type cannot be empty"));
            }
            
            if instance.listen_addr.is_empty() {
                return Err(anyhow::anyhow!("Proxy instance listen_addr cannot be empty"));
            }
            
            for host in &instance.hosts {
                if host.name.is_empty() {
                    return Err(anyhow::anyhow!("Host name cannot be empty in instance '{}'", instance.name));
                }
                
                if host.upstream.is_empty() {
                    return Err(anyhow::anyhow!("Host upstream cannot be empty in instance '{}'", instance.name));
                }
            }
        }
        
        let mut instance_names = std::collections::HashSet::new();
        let mut listen_addrs = std::collections::HashSet::new();
        
        for instance in &self.proxy_instances {
            if !instance_names.insert(&instance.name) {
                return Err(anyhow::anyhow!("Duplicate proxy instance name: {}", instance.name));
            }
            
            if !listen_addrs.insert(&instance.listen_addr) {
                return Err(anyhow::anyhow!("Duplicate listen address: {}", instance.listen_addr));
            }
        }
        
        Ok(())
    }
}