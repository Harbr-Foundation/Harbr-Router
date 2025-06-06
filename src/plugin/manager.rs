// src/plugin/manager.rs - Plugin manager coordinating loader and registry
use super::*;
use super::loader::{PluginLoader, LoaderConfig};
use super::registry::PluginRegistry;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

/// Plugin manager configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManagerConfig {
    pub loader: LoaderConfig,
    pub enable_inter_plugin_communication: bool,
    pub max_concurrent_loads: usize,
    pub plugin_startup_timeout_seconds: u64,
    pub health_check_interval_seconds: u64,
    pub metrics_collection_interval_seconds: u64,
    pub enable_plugin_isolation: bool,
    pub default_plugin_config: serde_json::Value,
}

impl Default for PluginManagerConfig {
    fn default() -> Self {
        Self {
            loader: LoaderConfig::default(),
            enable_inter_plugin_communication: true,
            max_concurrent_loads: 10,
            plugin_startup_timeout_seconds: 30,
            health_check_interval_seconds: 30,
            metrics_collection_interval_seconds: 60,
            enable_plugin_isolation: true,
            default_plugin_config: serde_json::json!({}),
        }
    }
}

/// Main plugin manager that coordinates everything
pub struct PluginManager {
    config: PluginManagerConfig,
    loader: Arc<PluginLoader>,
    registry: Arc<PluginRegistry>,
    router_event_sender: mpsc::UnboundedSender<RouterEvent>,
    router_event_receiver: Arc<RwLock<Option<mpsc::UnboundedReceiver<RouterEvent>>>>,
    health_monitor: Arc<HealthMonitor>,
    metrics_collector: Arc<MetricsCollector>,
    event_bus: Arc<EventBus>,
    running: Arc<RwLock<bool>>,
}

impl PluginManager {
    pub fn new(config: PluginManagerConfig) -> Self {
        let (router_tx, router_rx) = mpsc::unbounded_channel();
        
        let loader = Arc::new(PluginLoader::new(config.loader.clone()));
        let registry = Arc::new(PluginRegistry::new(router_tx.clone()));
        let health_monitor = Arc::new(HealthMonitor::new(
            config.health_check_interval_seconds
        ));
        let metrics_collector = Arc::new(MetricsCollector::new(
            config.metrics_collection_interval_seconds
        ));
        let event_bus = Arc::new(EventBus::new());
        
        Self {
            config,
            loader,
            registry,
            router_event_sender: router_tx,
            router_event_receiver: Arc::new(RwLock::new(Some(router_rx))),
            health_monitor,
            metrics_collector,
            event_bus,
            running: Arc::new(RwLock::new(false)),
        }
    }
    
    /// Start the plugin manager
    pub async fn start(&self) -> Result<()> {
        *self.running.write().await = true;
        
        tracing::info!("Starting plugin manager...");
        
        // Start event processing
        self.start_event_processor().await?;
        
        // Start health monitoring
        self.health_monitor.start(self.registry.clone()).await;
        
        // Start metrics collection
        self.metrics_collector.start(self.registry.clone()).await;
        
        // Load all plugins from directories
        self.load_all_plugins().await?;
        
        tracing::info!("Plugin manager started successfully");
        Ok(())
    }
    
    /// Stop the plugin manager
    pub async fn stop(&self) -> Result<()> {
        *self.running.write().await = false;
        
        tracing::info!("Stopping plugin manager...");
        
        // Stop all plugins
        self.registry.shutdown_all().await?;
        
        // Stop health monitor
        self.health_monitor.stop().await;
        
        // Stop metrics collector
        self.metrics_collector.stop().await;
        
        // Shutdown loader
        self.loader.shutdown().await?;
        
        tracing::info!("Plugin manager stopped");
        Ok(())
    }
    
    /// Load and register all plugins from configured directories
    pub async fn load_all_plugins(&self) -> Result<()> {
        tracing::info!("Loading all plugins...");
        
        let loaded_plugins = self.loader.load_all_plugins().await?;
        
        // Register each loaded plugin
        for (name, plugin) in loaded_plugins {
            let config = self.create_default_plugin_config(&name).await;
            
            match self.registry.register_plugin(name.clone(), plugin, config).await {
                Ok(_) => {
                    // Start the plugin
                    if let Err(e) = self.registry.start_plugin(&name).await {
                        tracing::error!("Failed to start plugin '{}': {}", name, e);
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to register plugin '{}': {}", name, e);
                }
            }
        }
        
        tracing::info!("Finished loading plugins");
        Ok(())
    }
    
    /// Load a specific plugin by path
    pub async fn load_plugin_from_path<P: AsRef<std::path::Path>>(
        &self,
        path: P,
        config: Option<PluginConfig>,
    ) -> Result<String> {
        let (name, plugin) = self.loader.load_plugin(path).await?;
        
        let plugin_config = config.unwrap_or_else(|| {
            futures::executor::block_on(self.create_default_plugin_config(&name))
        });
        
        self.registry.register_plugin(name.clone(), plugin, plugin_config).await?;
        self.registry.start_plugin(&name).await?;
        
        tracing::info!("Successfully loaded and started plugin: {}", name);
        Ok(name)
    }
    
    /// Unload a plugin
    pub async fn unload_plugin(&self, name: &str) -> Result<()> {
        // Stop and unregister from registry
        self.registry.stop_plugin(name).await?;
        self.registry.unregister_plugin(name).await?;
        
        // Unload from loader
        self.loader.unload_plugin(name).await?;
        
        tracing::info!("Successfully unloaded plugin: {}", name);
        Ok(())
    }
    
    /// Reload a plugin
    pub async fn reload_plugin(&self, name: &str) -> Result<()> {
        tracing::info!("Reloading plugin: {}", name);
        
        // Get current config before unloading
        let current_config = self.registry.get_plugin_config(name).await;
        
        // Unload the plugin
        self.unload_plugin(name).await?;
        
        // Find the plugin file and reload it
        let loaded_plugins = self.loader.get_loaded_plugins().await;
        if let Some(loaded_info) = loaded_plugins.get(name) {
            let path = loaded_info.path.clone();
            self.load_plugin_from_path(path, current_config).await?;
        } else {
            return Err(anyhow::anyhow!("Cannot find plugin file for '{}'", name));
        }
        
        tracing::info!("Successfully reloaded plugin: {}", name);
        Ok(())
    }
    
    /// Get plugin registry
    pub fn registry(&self) -> Arc<PluginRegistry> {
        self.registry.clone()
    }
    
    /// Get plugin loader
    pub fn loader(&self) -> Arc<PluginLoader> {
        self.loader.clone()
    }
    
    /// Route a request to appropriate plugin(s)
    pub async fn route_request(
        &self,
        domain: &str,
        path: &str,
        metadata: HashMap<String, String>,
    ) -> Result<Vec<String>> {
        let plugins = self.registry.find_plugins_for_domain(domain, path).await;
        
        if plugins.is_empty() {
            return Err(anyhow::anyhow!("No plugin found for domain: {}", domain));
        }
        
        Ok(plugins)
    }
    
    /// Send event to plugin
    pub async fn send_event_to_plugin(
        &self,
        plugin_name: &str,
        event: PluginEvent,
    ) -> Result<()> {
        self.registry.send_event_to_plugin(plugin_name, event).await
    }
    
    /// Broadcast event to all plugins
    pub async fn broadcast_event(&self, event: PluginEvent) {
        self.registry.broadcast_event(event).await;
    }
    
    /// Get plugin health status
    pub async fn get_plugin_health(&self, name: &str) -> Option<PluginHealth> {
        self.registry.get_plugin_health(name).await
    }
    
    /// Get plugin metrics
    pub async fn get_plugin_metrics(&self, name: &str) -> Option<PluginMetrics> {
        self.registry.get_plugin_metrics(name).await
    }
    
    /// Get all plugin metrics
    pub async fn get_all_metrics(&self) -> HashMap<String, PluginMetrics> {
        self.registry.get_aggregated_metrics().await
    }
    
    /// List all loaded plugins
    pub fn list_plugins(&self) -> Vec<String> {
        self.registry.list_plugins()
    }
    
    /// Get plugin information
    pub async fn get_plugin_info(&self, name: &str) -> Option<PluginInfo> {
        self.registry.get_plugin_info(name).await
    }
    
    /// Update plugin configuration
    pub async fn update_plugin_config(
        &self,
        name: &str,
        config: PluginConfig,
    ) -> Result<()> {
        self.registry.update_plugin_config(name, config).await
    }
    
    /// Execute plugin command
    pub async fn execute_plugin_command(
        &self,
        plugin_name: &str,
        command: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        if let Some(wrapper) = self.registry.get_plugin(plugin_name) {
            let plugin = wrapper.read().await;
            plugin.plugin.handle_command(command, args).await
        } else {
            Err(anyhow::anyhow!("Plugin '{}' not found", plugin_name))
        }
    }
    
    /// Start event processor
    async fn start_event_processor(&self) -> Result<()> {
        let mut receiver = {
            let mut rx_guard = self.router_event_receiver.write().await;
            rx_guard.take().ok_or_else(|| anyhow::anyhow!("Event processor already started"))?
        };
        
        let running = self.running.clone();
        let event_bus = self.event_bus.clone();
        
        tokio::spawn(async move {
            while *running.read().await {
                tokio::select! {
                    Some(event) = receiver.recv() => {
                        if let Err(e) = Self::handle_router_event(event, event_bus.clone()).await {
                            tracing::error!("Error handling router event: {}", e);
                        }
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                        // Periodic check
                    }
                }
            }
        });
        
        Ok(())
    }
    
    /// Handle router events
    async fn handle_router_event(
        event: RouterEvent,
        event_bus: Arc<EventBus>,
    ) -> Result<()> {
        match event {
            RouterEvent::PluginStarted(name) => {
                tracing::info!("Plugin started: {}", name);
                event_bus.publish("plugin.started", serde_json::json!({ "name": name })).await;
            }
            RouterEvent::PluginStopped(name) => {
                tracing::info!("Plugin stopped: {}", name);
                event_bus.publish("plugin.stopped", serde_json::json!({ "name": name })).await;
            }
            RouterEvent::PluginError { plugin_name, error } => {
                tracing::error!("Plugin '{}' error: {}", plugin_name, error);
                event_bus.publish("plugin.error", serde_json::json!({
                    "plugin": plugin_name,
                    "error": error
                })).await;
            }
            RouterEvent::MetricsUpdated { plugin_name, metrics } => {
                event_bus.publish("plugin.metrics", serde_json::json!({
                    "plugin": plugin_name,
                    "metrics": metrics
                })).await;
            }
            RouterEvent::HealthUpdated { plugin_name, health } => {
                event_bus.publish("plugin.health", serde_json::json!({
                    "plugin": plugin_name,
                    "health": health
                })).await;
            }
            RouterEvent::CustomEvent { plugin_name, event } => {
                event_bus.publish(&format!("plugin.{}.custom", plugin_name), event).await;
            }
            RouterEvent::LogMessage { plugin_name, level, message } => {
                match level.as_str() {
                    "error" => tracing::error!("[{}] {}", plugin_name, message),
                    "warn" => tracing::warn!("[{}] {}", plugin_name, message),
                    "info" => tracing::info!("[{}] {}", plugin_name, message),
                    "debug" => tracing::debug!("[{}] {}", plugin_name, message),
                    _ => tracing::info!("[{}] {}", plugin_name, message),
                }
            }
        }
        
        Ok(())
    }
    
    /// Create default plugin configuration
    async fn create_default_plugin_config(&self, plugin_name: &str) -> PluginConfig {
        PluginConfig {
            name: plugin_name.to_string(),
            enabled: true,
            priority: 0,
            bind_ports: Vec::new(),
            bind_addresses: vec!["0.0.0.0".to_string()],
            domains: Vec::new(),
            config: self.config.default_plugin_config.clone(),
            middleware_chain: Vec::new(),
            load_balancing: None,
            health_check: Some(HealthCheckConfig {
                enabled: true,
                interval_seconds: self.config.health_check_interval_seconds,
                timeout_seconds: 10,
                custom_check: None,
            }),
        }
    }
    
    /// Get manager statistics
    pub async fn get_statistics(&self) -> PluginManagerStatistics {
        let loader_stats = self.loader.get_statistics().await;
        let plugins = self.registry.list_plugins();
        let metrics = self.registry.get_aggregated_metrics().await;
        
        PluginManagerStatistics {
            total_plugins: plugins.len(),
            running_plugins: plugins.len(), // Simplified
            loader_statistics: loader_stats,
            total_connections: metrics.values().map(|m| m.connections_active).sum(),
            total_requests: metrics.values().map(|m| m.connections_total).sum(),
            total_errors: metrics.values().map(|m| m.errors_total).sum(),
            uptime_seconds: 0, // Would need to track start time
        }
    }
}

/// Health monitor for plugins
pub struct HealthMonitor {
    interval_seconds: u64,
    running: Arc<RwLock<bool>>,
    handle: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
}

impl HealthMonitor {
    pub fn new(interval_seconds: u64) -> Self {
        Self {
            interval_seconds,
            running: Arc::new(RwLock::new(false)),
            handle: Arc::new(RwLock::new(None)),
        }
    }
    
    pub async fn start(&self, registry: Arc<PluginRegistry>) {
        *self.running.write().await = true;
        
        let running = self.running.clone();
        let interval = self.interval_seconds;
        
        let handle = tokio::spawn(async move {
            let mut interval_timer = tokio::time::interval(
                std::time::Duration::from_secs(interval)
            );
            
            while *running.read().await {
                interval_timer.tick().await;
                
                let plugins = registry.list_plugins();
                for plugin_name in plugins {
                    if let Some(health) = registry.get_plugin_health(&plugin_name).await {
                        if !health.healthy {
                            tracing::warn!("Plugin '{}' is unhealthy: {}", plugin_name, health.message);
                        }
                    }
                }
            }
        });
        
        *self.handle.write().await = Some(handle);
    }
    
    pub async fn stop(&self) {
        *self.running.write().await = false;
        
        if let Some(handle) = self.handle.write().await.take() {
            handle.abort();
        }
    }
}

/// Metrics collector for plugins
pub struct MetricsCollector {
    interval_seconds: u64,
    running: Arc<RwLock<bool>>,
    handle: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
}

impl MetricsCollector {
    pub fn new(interval_seconds: u64) -> Self {
        Self {
            interval_seconds,
            running: Arc::new(RwLock::new(false)),
            handle: Arc::new(RwLock::new(None)),
        }
    }
    
    pub async fn start(&self, registry: Arc<PluginRegistry>) {
        *self.running.write().await = true;
        
        let running = self.running.clone();
        let interval = self.interval_seconds;
        
        let handle = tokio::spawn(async move {
            let mut interval_timer = tokio::time::interval(
                std::time::Duration::from_secs(interval)
            );
            
            while *running.read().await {
                interval_timer.tick().await;
                
                let _metrics = registry.get_aggregated_metrics().await;
                // Could store metrics in a time series database here
            }
        });
        
        *self.handle.write().await = Some(handle);
    }
    
    pub async fn stop(&self) {
        *self.running.write().await = false;
        
        if let Some(handle) = self.handle.write().await.take() {
            handle.abort();
        }
    }
}

/// Event bus for inter-plugin communication
pub struct EventBus {
    subscribers: Arc<RwLock<HashMap<String, Vec<mpsc::UnboundedSender<serde_json::Value>>>>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            subscribers: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    pub async fn subscribe(&self, event_type: &str) -> mpsc::UnboundedReceiver<serde_json::Value> {
        let (tx, rx) = mpsc::unbounded_channel();
        
        let mut subscribers = self.subscribers.write().await;
        subscribers
            .entry(event_type.to_string())
            .or_insert_with(Vec::new)
            .push(tx);
        
        rx
    }
    
    pub async fn publish(&self, event_type: &str, data: serde_json::Value) {
        let subscribers = self.subscribers.read().await;
        
        if let Some(senders) = subscribers.get(event_type) {
            for sender in senders {
                let _ = sender.send(data.clone());
            }
        }
    }
}

/// Plugin manager statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManagerStatistics {
    pub total_plugins: usize,
    pub running_plugins: usize,
    pub loader_statistics: super::loader::PluginLoaderStatistics,
    pub total_connections: u64,
    pub total_requests: u64,
    pub total_errors: u64,
    pub uptime_seconds: u64,
}

// Extension to registry for additional methods
impl PluginRegistry {
    pub async fn get_plugin_config(&self, name: &str) -> Option<PluginConfig> {
        let configs = self.plugin_configs.read().await;
        configs.get(name).cloned()
    }
}