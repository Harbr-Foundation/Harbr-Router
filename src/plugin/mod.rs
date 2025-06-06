// src/plugin/mod.rs - Generic plugin interface
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Debug;
use std::net::SocketAddr;
use std::sync::Arc;
use anyhow::Result;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::mpsc;

pub mod registry;
pub mod loader;
pub mod manager;

/// Generic connection context - plugins define what this means
#[derive(Debug, Clone)]
pub struct ConnectionContext {
    pub connection_id: String,
    pub client_addr: SocketAddr,
    pub server_addr: SocketAddr,
    pub metadata: HashMap<String, String>,
    pub created_at: std::time::SystemTime,
}

/// Generic data packet - completely opaque to the router
#[derive(Debug, Clone)]
pub struct DataPacket {
    pub data: Vec<u8>,
    pub metadata: HashMap<String, String>,
    pub context: ConnectionContext,
}

/// Plugin capabilities - what the plugin can handle
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginCapabilities {
    pub can_handle_tcp: bool,
    pub can_handle_udp: bool,
    pub can_handle_unix_socket: bool,
    pub requires_dedicated_port: bool,
    pub supports_hot_reload: bool,
    pub supports_load_balancing: bool,
    pub custom_protocols: Vec<String>,
    pub port_requirements: Vec<u16>, // Specific ports this plugin needs
}

/// Plugin configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfig {
    pub name: String,
    pub enabled: bool,
    pub priority: i32,
    pub bind_ports: Vec<u16>,
    pub bind_addresses: Vec<String>,
    pub domains: Vec<String>, // Domains this plugin should handle
    pub config: serde_json::Value, // Plugin-specific configuration
    pub middleware_chain: Vec<String>,
    pub load_balancing: Option<LoadBalancingConfig>,
    pub health_check: Option<HealthCheckConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadBalancingConfig {
    pub strategy: String, // Plugin defines what strategies it supports
    pub backends: Vec<BackendConfig>,
    pub session_affinity: bool,
    pub health_check_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendConfig {
    pub address: String,
    pub weight: Option<u32>,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckConfig {
    pub enabled: bool,
    pub interval_seconds: u64,
    pub timeout_seconds: u64,
    pub custom_check: Option<serde_json::Value>,
}

/// Plugin metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMetrics {
    pub connections_total: u64,
    pub connections_active: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub errors_total: u64,
    pub custom_metrics: HashMap<String, f64>,
    pub last_updated: std::time::SystemTime,
}

/// Plugin health status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHealth {
    pub healthy: bool,
    pub message: String,
    pub last_check: std::time::SystemTime,
    pub response_time_ms: Option<u64>,
    pub custom_health_data: HashMap<String, serde_json::Value>,
}

/// Events that plugins can emit or receive
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PluginEvent {
    ConnectionEstablished(ConnectionContext),
    ConnectionClosed(String), // connection_id
    DataReceived(DataPacket),
    ConfigurationChanged(serde_json::Value),
    HealthCheckRequested,
    MetricsRequested,
    ShutdownRequested,
    Custom {
        event_type: String,
        data: serde_json::Value,
    },
}

/// Communication channel for plugins
pub type PluginSender = mpsc::UnboundedSender<PluginEvent>;
pub type PluginReceiver = mpsc::UnboundedReceiver<PluginEvent>;

/// Main plugin trait - completely generic
#[async_trait]
pub trait Plugin: Send + Sync + Debug {
    /// Plugin identification
    fn info(&self) -> &PluginInfo;

    /// What this plugin can do
    fn capabilities(&self) -> &PluginCapabilities;

    /// Initialize the plugin
    async fn initialize(
        &mut self,
        config: PluginConfig,
        event_sender: PluginSender,
    ) -> Result<()>;

    /// Start the plugin - it manages its own listeners/connections
    async fn start(&mut self) -> Result<()>;

    /// Stop the plugin
    async fn stop(&mut self) -> Result<()>;

    /// Handle events from the router or other plugins
    async fn handle_event(&mut self, event: PluginEvent) -> Result<()>;

    /// Get current health status
    async fn health(&self) -> Result<PluginHealth>;

    /// Get current metrics
    async fn metrics(&self) -> Result<PluginMetrics>;

    /// Update configuration at runtime
    async fn update_config(&mut self, config: PluginConfig) -> Result<()>;

    /// Custom command handler for management
    async fn handle_command(
        &self,
        command: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value>;

    /// Graceful shutdown
    async fn shutdown(&mut self) -> Result<()>;
}

/// Plugin metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub license: String,
    pub repository: Option<String>,
    pub min_router_version: String,
    pub config_schema: Option<serde_json::Value>,
    pub dependencies: Vec<String>,
    pub tags: Vec<String>,
}

/// Plugin factory function signature
pub type PluginFactory = unsafe extern "C" fn() -> *mut dyn Plugin;

/// Plugin registration function signature
pub type PluginRegisterFn = unsafe extern "C" fn() -> PluginInfo;

/// Domain matcher trait - plugins can implement custom domain matching
pub trait DomainMatcher: Send + Sync + Debug {
    fn matches(&self, domain: &str, path: &str) -> bool;
    fn priority(&self) -> i32;
}

/// Simple domain matcher implementation
#[derive(Debug, Clone)]
pub struct SimpleDomainMatcher {
    pub patterns: Vec<String>,
    pub priority: i32,
}

impl DomainMatcher for SimpleDomainMatcher {
    fn matches(&self, domain: &str, _path: &str) -> bool {
        self.patterns.iter().any(|pattern| {
            if pattern.starts_with("*.") {
                let suffix = &pattern[2..];
                domain.ends_with(suffix)
            } else {
                pattern == domain
            }
        })
    }
    
    fn priority(&self) -> i32 {
        self.priority
    }
}

/// Plugin context - shared state between router and plugin
#[derive(Debug)]
pub struct PluginContext {
    pub plugin_name: String,
    pub config: PluginConfig,
    pub event_sender: PluginSender,
    pub router_sender: mpsc::UnboundedSender<RouterEvent>,
    pub metrics: Arc<tokio::sync::RwLock<PluginMetrics>>,
    pub health: Arc<tokio::sync::RwLock<PluginHealth>>,
}

/// Events that plugins can send to the router
#[derive(Debug, Clone)]
pub enum RouterEvent {
    PluginStarted(String),
    PluginStopped(String),
    PluginError { plugin_name: String, error: String },
    MetricsUpdated { plugin_name: String, metrics: PluginMetrics },
    HealthUpdated { plugin_name: String, health: PluginHealth },
    CustomEvent { plugin_name: String, event: serde_json::Value },
    LogMessage { plugin_name: String, level: String, message: String },
}

/// Plugin wrapper that handles the lifecycle
pub struct PluginWrapper {
    plugin: Box<dyn Plugin>,
    context: PluginContext,
    event_receiver: PluginReceiver,
    running: Arc<tokio::sync::RwLock<bool>>,
}

impl PluginWrapper {
    pub fn new(plugin: Box<dyn Plugin>, context: PluginContext) -> (Self, PluginReceiver) {
        let (tx, rx) = mpsc::unbounded_channel();
        let context = PluginContext {
            event_sender: tx,
            ..context
        };
        
        let (_event_tx, event_rx) = mpsc::unbounded_channel();
        
        (
            Self {
                plugin,
                context,
                event_receiver: event_rx,
                running: Arc::new(tokio::sync::RwLock::new(false)),
            },
            rx,
        )
    }
    
    pub async fn start(&mut self) -> Result<()> {
        *self.running.write().await = true;
        
        // Initialize the plugin
        self.plugin.initialize(
            self.context.config.clone(),
            self.context.event_sender.clone(),
        ).await?;
        
        // Start the plugin
        self.plugin.start().await?;
        
        // Notify router
        let _ = self.context.router_sender.send(RouterEvent::PluginStarted(
            self.context.plugin_name.clone()
        ));
        
        Ok(())
    }
    
    pub async fn stop(&mut self) -> Result<()> {
        *self.running.write().await = false;
        
        self.plugin.stop().await?;
        
        let _ = self.context.router_sender.send(RouterEvent::PluginStopped(
            self.context.plugin_name.clone()
        ));
        
        Ok(())
    }
    
    pub async fn run_event_loop(&mut self) -> Result<()> {
        while *self.running.read().await {
            tokio::select! {
                Some(event) = self.event_receiver.recv() => {
                    if let Err(e) = self.plugin.handle_event(event).await {
                        let _ = self.context.router_sender.send(RouterEvent::PluginError {
                            plugin_name: self.context.plugin_name.clone(),
                            error: e.to_string(),
                        });
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                    // Periodic health check and metrics update
                    if let Ok(health) = self.plugin.health().await {
                        *self.context.health.write().await = health.clone();
                        let _ = self.context.router_sender.send(RouterEvent::HealthUpdated {
                            plugin_name: self.context.plugin_name.clone(),
                            health,
                        });
                    }
                    
                    if let Ok(metrics) = self.plugin.metrics().await {
                        *self.context.metrics.write().await = metrics.clone();
                        let _ = self.context.router_sender.send(RouterEvent::MetricsUpdated {
                            plugin_name: self.context.plugin_name.clone(),
                            metrics,
                        });
                    }
                }
            }
        }
        
        Ok(())
    }
    
    pub fn info(&self) -> &PluginInfo {
        self.plugin.info()
    }
    
    pub fn capabilities(&self) -> &PluginCapabilities {
        self.plugin.capabilities()
    }
}

/// Utility functions for plugins
pub mod utils {
    use super::*;
    
    /// Extract domain from various headers
    pub fn extract_domain(headers: &HashMap<String, String>) -> Option<String> {
        headers.get("host")
            .or_else(|| headers.get("Host"))
            .or_else(|| headers.get("SERVER_NAME"))
            .map(|host| {
                // Remove port if present
                host.split(':').next().unwrap_or(host).to_string()
            })
    }
    
    /// Parse query parameters from URL
    pub fn parse_query_params(query: &str) -> HashMap<String, String> {
        let mut params = HashMap::new();
        
        for pair in query.split('&') {
            if let Some(eq_pos) = pair.find('=') {
                let key = &pair[..eq_pos];
                let value = &pair[eq_pos + 1..];
                params.insert(
                    urlencoding::decode(key).unwrap_or_default().into_owned(),
                    urlencoding::decode(value).unwrap_or_default().into_owned(),
                );
            }
        }
        
        params
    }
    
    /// Generate unique connection ID
    pub fn generate_connection_id() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        format!("conn-{}-{}", now.as_secs(), now.subsec_nanos())
    }
    
    /// Create connection context
    pub fn create_connection_context(
        client_addr: SocketAddr,
        server_addr: SocketAddr,
        metadata: HashMap<String, String>,
    ) -> ConnectionContext {
        ConnectionContext {
            connection_id: generate_connection_id(),
            client_addr,
            server_addr,
            metadata,
            created_at: SystemTime::now(),
        }
    }
}