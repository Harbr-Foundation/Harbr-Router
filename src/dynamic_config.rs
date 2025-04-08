// src/dynamic_config.rs
use crate::config::{ProxyConfig, RouteConfig, TcpProxyConfig};
use anyhow::{Context, Result};
use dashmap::DashMap;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio::time::{Duration, interval};
use tokio::fs::File;
use tokio::io::AsyncReadExt;

pub type SharedConfig = Arc<RwLock<ProxyConfig>>;

/// Event types for configuration changes
#[derive(Debug, Clone)]
pub enum ConfigEvent {
    FullUpdate(ProxyConfig),
    RouteAdded(String, RouteConfig),
    RouteUpdated(String, RouteConfig),
    RouteRemoved(String),
    TcpConfigUpdated(TcpProxyConfig),
    GlobalSettingsUpdated {
        listen_addr: Option<String>,
        global_timeout_ms: Option<u64>,
        max_connections: Option<usize>,
    },
}

/// Manages dynamic configuration updates
pub struct DynamicConfigManager {
    config: SharedConfig,
    event_tx: broadcast::Sender<ConfigEvent>,
    file_path: Option<PathBuf>,
    last_modified: Arc<RwLock<Option<std::time::SystemTime>>>,
    active_watchers: Arc<DashMap<String, ()>>,
}

impl DynamicConfigManager {
    /// Create a new configuration manager with the given config
    pub fn new(config: ProxyConfig) -> Self {
        let (event_tx, _) = broadcast::channel(100);
        
        Self {
            config: Arc::new(RwLock::new(config)),
            event_tx,
            file_path: None,
            last_modified: Arc::new(RwLock::new(None)),
            active_watchers: Arc::new(DashMap::new()),
        }
    }

    /// Create a new configuration manager from a file
    pub async fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read config from {}", path.display()))?;
        
        let config: ProxyConfig = serde_yaml::from_str(&content)
            .with_context(|| format!("Failed to parse config from {}", path.display()))?;
        
        let mut manager = Self::new(config);
        manager.file_path = Some(path);
        *manager.last_modified.write().await = fs::metadata(&manager.file_path.as_ref().unwrap())
            .ok()
            .and_then(|m| m.modified().ok());
        
        Ok(manager)
    }

    /// Get a clone of the shared configuration
    pub fn get_config(&self) -> SharedConfig {
        self.config.clone()
    }

    /// Get a subscription to configuration events
    pub fn subscribe(&self) -> broadcast::Receiver<ConfigEvent> {
        self.event_tx.subscribe()
    }

    /// Update a specific route
    pub async fn update_route(&self, route_name: &str, route_config: RouteConfig) -> Result<()> {
        let mut config = self.config.write().await;
        config.routes.insert(route_name.to_string(), route_config.clone());
        
        // Broadcast the event
        let _ = self.event_tx.send(ConfigEvent::RouteUpdated(
            route_name.to_string(), 
            route_config
        ));
        
        // If we have a file path, save the updated config
        if let Some(path) = &self.file_path {
            self.save_config_to_file(&config, path).await?;
        }
        
        Ok(())
    }

    /// Add a new route
    pub async fn add_route(&self, route_name: &str, route_config: RouteConfig) -> Result<()> {
        let mut config = self.config.write().await;
        
        // Check if route already exists
        if config.routes.contains_key(route_name) {
            return Err(anyhow::anyhow!("Route '{}' already exists", route_name));
        }
        
        config.routes.insert(route_name.to_string(), route_config.clone());
        
        // Broadcast the event
        let _ = self.event_tx.send(ConfigEvent::RouteAdded(
            route_name.to_string(), 
            route_config
        ));
        
        // If we have a file path, save the updated config
        if let Some(path) = &self.file_path {
            self.save_config_to_file(&config, path).await?;
        }
        
        Ok(())
    }

    /// Remove a route
    pub async fn remove_route(&self, route_name: &str) -> Result<()> {
        let mut config = self.config.write().await;
        
        if config.routes.remove(route_name).is_none() {
            return Err(anyhow::anyhow!("Route '{}' does not exist", route_name));
        }
        
        // Broadcast the event
        let _ = self.event_tx.send(ConfigEvent::RouteRemoved(route_name.to_string()));
        
        // If we have a file path, save the updated config
        if let Some(path) = &self.file_path {
            self.save_config_to_file(&config, path).await?;
        }
        
        Ok(())
    }

    /// Update TCP proxy configuration
    pub async fn update_tcp_config(&self, tcp_config: TcpProxyConfig) -> Result<()> {
        let mut config = self.config.write().await;
        config.tcp_proxy = tcp_config.clone();
        
        // Broadcast the event
        let _ = self.event_tx.send(ConfigEvent::TcpConfigUpdated(tcp_config));
        
        // If we have a file path, save the updated config
        if let Some(path) = &self.file_path {
            self.save_config_to_file(&config, path).await?;
        }
        
        Ok(())
    }

    /// Update global settings
    pub async fn update_global_settings(
        &self,
        listen_addr: Option<String>,
        global_timeout_ms: Option<u64>,
        max_connections: Option<usize>,
    ) -> Result<()> {
        let mut config = self.config.write().await;
        
        if let Some(addr) = listen_addr.clone() {
            config.listen_addr = addr;
        }
        
        if let Some(timeout) = global_timeout_ms {
            config.global_timeout_ms = timeout;
        }
        
        if let Some(connections) = max_connections {
            config.max_connections = connections;
        }
        
        // Broadcast the event
        let _ = self.event_tx.send(ConfigEvent::GlobalSettingsUpdated {
            listen_addr: listen_addr,
            global_timeout_ms: global_timeout_ms,
            max_connections: max_connections,
        });
        
        // If we have a file path, save the updated config
        if let Some(path) = &self.file_path {
            self.save_config_to_file(&config, path).await?;
        }
        
        Ok(())
    }

    /// Replace the entire configuration
    pub async fn replace_config(&self, new_config: ProxyConfig) -> Result<()> {
        {
            let mut config = self.config.write().await;
            *config = new_config.clone();
        }
        
        // Broadcast the event
        let _ = self.event_tx.send(ConfigEvent::FullUpdate(new_config));
        
        // If we have a file path, save the updated config
        if let Some(path) = &self.file_path {
            let config = self.config.read().await;
            self.save_config_to_file(&config, path).await?;
        }
        
        Ok(())
    }

    /// Reload configuration from file
    pub async fn reload_from_file(&self) -> Result<()> {
        if let Some(path) = &self.file_path {
            let content = fs::read_to_string(path)
                .with_context(|| format!("Failed to read config from {}", path.display()))?;
            
            let new_config: ProxyConfig = serde_yaml::from_str(&content)
                .with_context(|| format!("Failed to parse config from {}", path.display()))?;
            
            self.replace_config(new_config).await?;
            let mut last_modified = self.last_modified.write().await;
            *last_modified = fs::metadata(path).ok().map(|m| m.modified().unwrap_or_else(|_| std::time::SystemTime::now()));
            
            tracing::info!("Configuration reloaded from {}", path.display());
        } else {
            return Err(anyhow::anyhow!("No configuration file path set"));
        }
        
        Ok(())
    }

    /// Save current configuration to a file
    async fn save_config_to_file(&self, config: &ProxyConfig, path: &Path) -> Result<()> {
        let yaml = serde_yaml::to_string(config)?;
        fs::write(path, yaml)?;
        *self.last_modified.write().await = fs::metadata(path).ok().map(|m| m.modified().unwrap_or_else(|_| std::time::SystemTime::now()));
        tracing::info!("Configuration saved to {}", path.display());
        Ok(())
    }

    /// Start file watching for automatic reloading
    pub async fn start_file_watcher(&self, interval_secs: u64) -> Result<()> {
        if self.file_path.is_none() {
            return Err(anyhow::anyhow!("No configuration file path set for watching"));
        }
        
        let watcher_id = uuid::Uuid::new_v4().to_string();
        self.active_watchers.insert(watcher_id.clone(), ());
        
        let file_path = self.file_path.clone().unwrap();
        let config_manager = self.clone();
        let watchers = self.active_watchers.clone();
        
        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(interval_secs));
            
            loop {
                interval.tick().await;
                
                // Check if this watcher has been cancelled
                if !watchers.contains_key(&watcher_id) {
                    tracing::info!("File watcher stopped");
                    break;
                }
                
                // Check if file has been modified
                if let Ok(metadata) = fs::metadata(&file_path) {
                    if let Ok(modified) = metadata.modified() {
                        let last_mod = config_manager.last_modified.read().await.unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                        
                        if modified > last_mod {
                            tracing::info!("Configuration file changed, reloading");
                            if let Err(e) = config_manager.reload_from_file().await {
                                tracing::error!("Failed to reload configuration: {}", e);
                            }
                        }
                    }
                }
            }
        });
        
        tracing::info!("Started file watcher for {}", self.file_path.as_ref().unwrap().display());
        Ok(())
    }

    /// Stop all file watchers
    pub fn stop_file_watchers(&self) {
        self.active_watchers.clear();
        tracing::info!("Stopped all file watchers");
    }
    
    /// Clone the manager
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            event_tx: self.event_tx.clone(),
            file_path: self.file_path.clone(),
            last_modified: self.last_modified.clone(),
            active_watchers: self.active_watchers.clone(),
        }
    }
}

/// Helper for serializing route configurations to/from JSON
pub mod api {
    use super::*;
    use serde::{Deserialize, Serialize};
    
    #[derive(Debug, Serialize, Deserialize)]
    pub struct RouteUpdateRequest {
        pub upstream: String,
        pub timeout_ms: Option<u64>,
        pub retry_count: Option<u32>,
        pub priority: Option<i32>,
        pub preserve_host_header: Option<bool>,
        pub is_tcp: Option<bool>,
        pub tcp_listen_port: Option<u16>,
        pub is_udp: Option<bool>,
        pub udp_listen_port: Option<u16>,
        pub db_type: Option<String>,
    }
    
    impl From<RouteUpdateRequest> for RouteConfig {
        fn from(req: RouteUpdateRequest) -> Self {
            RouteConfig {
                upstream: req.upstream,
                timeout_ms: req.timeout_ms,
                retry_count: req.retry_count,
                priority: req.priority,
                preserve_host_header: req.preserve_host_header,
                is_tcp: req.is_tcp.unwrap_or(false),
                tcp_listen_port: req.tcp_listen_port,
                is_udp: req.is_udp,
                udp_listen_port: req.udp_listen_port,
                db_type: req.db_type,
            }
        }
    }
    
    #[derive(Debug, Serialize, Deserialize)]
    pub struct TcpConfigUpdateRequest {
        pub enabled: Option<bool>,
        pub listen_addr: Option<String>,
        pub connection_pooling: Option<bool>,
        pub max_idle_time_secs: Option<u64>,
        pub udp_enabled: Option<bool>,
        pub udp_listen_addr: Option<String>,
    }
    
    impl TcpConfigUpdateRequest {
        pub fn apply_to(&self, config: &mut TcpProxyConfig) {
            if let Some(enabled) = self.enabled {
                config.enabled = enabled;
            }
            if let Some(ref listen_addr) = self.listen_addr {
                config.listen_addr = listen_addr.clone();
            }
            if let Some(connection_pooling) = self.connection_pooling {
                config.connection_pooling = connection_pooling;
            }
            if let Some(max_idle_time_secs) = self.max_idle_time_secs {
                config.max_idle_time_secs = max_idle_time_secs;
            }
            if let Some(udp_enabled) = self.udp_enabled {
                config.udp_enabled = udp_enabled;
            }
            if let Some(ref udp_listen_addr) = self.udp_listen_addr {
                config.udp_listen_addr = udp_listen_addr.clone();
            }
        }
    }
    
    #[derive(Debug, Serialize, Deserialize)]
    pub struct GlobalSettingsUpdateRequest {
        pub listen_addr: Option<String>,
        pub global_timeout_ms: Option<u64>,
        pub max_connections: Option<usize>,
    }
}