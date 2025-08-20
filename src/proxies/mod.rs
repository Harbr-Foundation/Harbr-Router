use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use anyhow::Result;
use serde::{Deserialize, Serialize};

pub mod http;
pub mod tcp;
pub mod udp;

pub use http::HttpProxy;
pub use tcp::TcpProxy;
pub use udp::UdpProxy;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyInstance {
    pub name: String,
    pub proxy_type: String,
    pub listen_addr: String,
    pub config: serde_yaml::Value,
    pub hosts: Vec<HostConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostConfig {
    pub name: String,
    pub upstream: String,
    pub weight: Option<u32>,
    pub health_check: Option<HealthCheckConfig>,
    pub timeout_ms: Option<u64>,
    pub retry_count: Option<u32>,
    pub priority: Option<i32>,
    pub preserve_host_header: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckConfig {
    pub enabled: bool,
    pub interval_ms: u64,
    pub timeout_ms: u64,
    pub path: Option<String>,
    pub expected_status: Option<u16>,
}

#[async_trait]
pub trait Proxy: Send + Sync {
    fn proxy_type(&self) -> &'static str;
    
    async fn start(&self, instance: &ProxyInstance) -> Result<()>;
    
    async fn stop(&self) -> Result<()>;
    
    async fn add_host(&self, host: HostConfig) -> Result<()>;
    
    async fn remove_host(&self, host_name: &str) -> Result<()>;
    
    async fn update_host(&self, host: HostConfig) -> Result<()>;
    
    async fn get_hosts(&self) -> Result<Vec<HostConfig>>;
    
    async fn health_check(&self) -> Result<bool>;
    
    fn validate_config(&self, config: &serde_yaml::Value) -> Result<()>;
}

pub struct ProxyManager {
    proxies: Arc<RwLock<HashMap<String, Box<dyn Proxy>>>>,
    instances: Arc<RwLock<HashMap<String, ProxyInstance>>>,
}

impl ProxyManager {
    pub fn new() -> Self {
        Self {
            proxies: Arc::new(RwLock::new(HashMap::new())),
            instances: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    pub async fn register_proxy_type(&self, proxy: Box<dyn Proxy>) {
        let proxy_type = proxy.proxy_type().to_string();
        self.proxies.write().await.insert(proxy_type, proxy);
    }
    
    pub async fn create_instance(&self, instance: ProxyInstance) -> Result<()> {
        let proxy_type = instance.proxy_type.clone();
        let instance_name = instance.name.clone();
        
        let proxies = self.proxies.read().await;
        let proxy = proxies.get(&proxy_type)
            .ok_or_else(|| anyhow::anyhow!("Unknown proxy type: {}", proxy_type))?;
            
        proxy.validate_config(&instance.config)?;
        proxy.start(&instance).await?;
        
        self.instances.write().await.insert(instance_name, instance);
        Ok(())
    }
    
    pub async fn remove_instance(&self, instance_name: &str) -> Result<()> {
        let mut instances = self.instances.write().await;
        let instance = instances.remove(instance_name)
            .ok_or_else(|| anyhow::anyhow!("Instance not found: {}", instance_name))?;
            
        let proxies = self.proxies.read().await;
        let proxy = proxies.get(&instance.proxy_type)
            .ok_or_else(|| anyhow::anyhow!("Unknown proxy type: {}", instance.proxy_type))?;
            
        proxy.stop().await?;
        Ok(())
    }
    
    pub async fn add_host_to_instance(&self, instance_name: &str, host: HostConfig) -> Result<()> {
        let mut instances = self.instances.write().await;
        let instance = instances.get_mut(instance_name)
            .ok_or_else(|| anyhow::anyhow!("Instance not found: {}", instance_name))?;
            
        let proxies = self.proxies.read().await;
        let proxy = proxies.get(&instance.proxy_type)
            .ok_or_else(|| anyhow::anyhow!("Unknown proxy type: {}", instance.proxy_type))?;
            
        proxy.add_host(host.clone()).await?;
        instance.hosts.push(host);
        Ok(())
    }
    
    pub async fn remove_host_from_instance(&self, instance_name: &str, host_name: &str) -> Result<()> {
        let mut instances = self.instances.write().await;
        let instance = instances.get_mut(instance_name)
            .ok_or_else(|| anyhow::anyhow!("Instance not found: {}", instance_name))?;
            
        let proxies = self.proxies.read().await;
        let proxy = proxies.get(&instance.proxy_type)
            .ok_or_else(|| anyhow::anyhow!("Unknown proxy type: {}", instance.proxy_type))?;
            
        proxy.remove_host(host_name).await?;
        instance.hosts.retain(|h| h.name != host_name);
        Ok(())
    }
    
    pub async fn list_instances(&self) -> Vec<ProxyInstance> {
        self.instances.read().await.values().cloned().collect()
    }
    
    pub async fn get_instance(&self, instance_name: &str) -> Option<ProxyInstance> {
        self.instances.read().await.get(instance_name).cloned()
    }
}

impl Default for ProxyManager {
    fn default() -> Self {
        Self::new()
    }
}