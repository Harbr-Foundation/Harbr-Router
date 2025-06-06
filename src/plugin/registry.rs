// src/plugin/registry.rs - Plugin registry for managing loaded plugins
use super::*;
use anyhow::{Context, Result};
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

/// Plugin registry manages all loaded plugins
pub struct PluginRegistry {
    pub plugins: Arc<DashMap<String, Arc<RwLock<PluginWrapper>>>>,
    pub plugin_configs: Arc<RwLock<HashMap<String, PluginConfig>>>,
    pub domain_mappings: Arc<RwLock<HashMap<String, Vec<String>>>>, // domain -> plugin names
    pub port_mappings: Arc<RwLock<HashMap<u16, String>>>, // port -> plugin name
    pub event_router: Arc<EventRouter>,
    pub router_event_sender: mpsc::UnboundedSender<RouterEvent>,
    pub metrics_aggregator: Arc<MetricsAggregator>,
}

impl PluginRegistry {
    pub fn new(router_event_sender: mpsc::UnboundedSender<RouterEvent>) -> Self {
        Self {
            plugins: Arc::new(DashMap::new()),
            plugin_configs: Arc::new(RwLock::new(HashMap::new())),
            domain_mappings: Arc::new(RwLock::new(HashMap::new())),
            port_mappings: Arc::new(RwLock::new(HashMap::new())),
            event_router: Arc::new(EventRouter::new()),
            router_event_sender,
            metrics_aggregator: Arc::new(MetricsAggregator::new()),
        }
    }
    
    /// Register a new plugin
    pub async fn register_plugin(
        &self,
        name: String,
        plugin: Box<dyn Plugin>,
        config: PluginConfig,
    ) -> Result<()> {
        // Check if plugin already exists
        if self.plugins.contains_key(&name) {
            return Err(anyhow::anyhow!("Plugin '{}' is already registered", name));
        }
        
        // Validate configuration
        self.validate_plugin_config(&name, &config).await?;
        
        // Check port conflicts
        for port in &config.bind_ports {
            if let Some(existing_plugin) = self.port_mappings.read().await.get(port) {
                return Err(anyhow::anyhow!(
                    "Port {} is already in use by plugin '{}'",
                    port,
                    existing_plugin
                ));
            }
        }
        
        // Create plugin context
        let context = PluginContext {
            plugin_name: name.clone(),
            config: config.clone(),
            event_sender: mpsc::unbounded_channel().0, // Placeholder, will be replaced
            router_sender: self.router_event_sender.clone(),
            metrics: Arc::new(RwLock::new(PluginMetrics {
                connections_total: 0,
                connections_active: 0,
                bytes_sent: 0,
                bytes_received: 0,
                errors_total: 0,
                custom_metrics: HashMap::new(),
                last_updated: std::time::SystemTime::now(),
            })),
            health: Arc::new(RwLock::new(PluginHealth {
                healthy: false,
                message: "Initializing".to_string(),
                last_check: std::time::SystemTime::now(),
                response_time_ms: None,
                custom_health_data: HashMap::new(),
            })),
        };
        
        // Create plugin wrapper
        let (wrapper, _event_rx) = PluginWrapper::new(plugin, context);
        let wrapper = Arc::new(RwLock::new(wrapper));
        
        // Register domain mappings
        {
            let mut domain_mappings = self.domain_mappings.write().await;
            for domain in &config.domains {
                domain_mappings
                    .entry(domain.clone())
                    .or_insert_with(Vec::new)
                    .push(name.clone());
            }
        }
        
        // Register port mappings
        {
            let mut port_mappings = self.port_mappings.write().await;
            for port in &config.bind_ports {
                port_mappings.insert(*port, name.clone());
            }
        }
        
        // Store plugin configuration
        {
            let mut configs = self.plugin_configs.write().await;
            configs.insert(name.clone(), config);
        }
        
        // Add to registry
        self.plugins.insert(name.clone(), wrapper);
        
        tracing::info!("Plugin '{}' registered successfully", name);
        Ok(())
    }
    
    /// Unregister a plugin
    pub async fn unregister_plugin(&self, name: &str) -> Result<()> {
        let wrapper = self.plugins.get(name)
            .ok_or_else(|| anyhow::anyhow!("Plugin '{}' not found", name))?
            .clone();
        
        // Stop the plugin
        {
            let mut plugin = wrapper.write().await;
            plugin.stop().await?;
        }
        
        // Remove from registries
        self.plugins.remove(name);
        
        // Clean up domain mappings
        {
            let mut domain_mappings = self.domain_mappings.write().await;
            domain_mappings.retain(|_domain, plugins| {
                plugins.retain(|plugin_name| plugin_name != name);
                !plugins.is_empty()
            });
        }
        
        // Clean up port mappings
        {
            let mut port_mappings = self.port_mappings.write().await;
            port_mappings.retain(|_port, plugin_name| plugin_name != name);
        }
        
        // Remove configuration
        {
            let mut configs = self.plugin_configs.write().await;
            configs.remove(name);
        }
        
        tracing::info!("Plugin '{}' unregistered successfully", name);
        Ok(())
    }
    
    /// Start a plugin
    pub async fn start_plugin(&self, name: &str) -> Result<()> {
        let wrapper = self.plugins.get(name)
            .ok_or_else(|| anyhow::anyhow!("Plugin '{}' not found", name))?
            .clone();
        
        let mut plugin = wrapper.write().await;
        plugin.start().await?;
        
        // Start event loop
        let wrapper_clone = wrapper.clone();
        tokio::spawn(async move {
            let mut plugin = wrapper_clone.write().await;
            if let Err(e) = plugin.run_event_loop().await {
                tracing::error!("Plugin event loop error: {}", e);
            }
        });
        
        tracing::info!("Plugin '{}' started successfully", name);
        Ok(())
    }
    
    /// Stop a plugin
    pub async fn stop_plugin(&self, name: &str) -> Result<()> {
        let wrapper = self.plugins.get(name)
            .ok_or_else(|| anyhow::anyhow!("Plugin '{}' not found", name))?
            .clone();
        
        let mut plugin = wrapper.write().await;
        plugin.stop().await?;
        
        tracing::info!("Plugin '{}' stopped successfully", name);
        Ok(())
    }
    
    /// Get plugin by name
    pub fn get_plugin(&self, name: &str) -> Option<Arc<RwLock<PluginWrapper>>> {
        self.plugins.get(name).map(|p| p.clone())
    }
    
    /// Find plugins that can handle a domain
    pub async fn find_plugins_for_domain(&self, domain: &str, path: &str) -> Vec<String> {
        let domain_mappings = self.domain_mappings.read().await;
        let mut matching_plugins = Vec::new();
        
        // Exact match first
        if let Some(plugins) = domain_mappings.get(domain) {
            matching_plugins.extend(plugins.clone());
        }
        
        // Wildcard matches
        for (pattern, plugins) in domain_mappings.iter() {
            if pattern.starts_with("*.") {
                let suffix = &pattern[2..];
                if domain.ends_with(suffix) {
                    matching_plugins.extend(plugins.clone());
                }
            }
        }
        
        // Sort by priority (would need to get plugin configs for this)
        matching_plugins.sort();
        matching_plugins.dedup();
        
        matching_plugins
    }
    
    /// Find plugin that handles a specific port
    pub async fn find_plugin_for_port(&self, port: u16) -> Option<String> {
        self.port_mappings.read().await.get(&port).cloned()
    }
    
    /// List all plugins
    pub fn list_plugins(&self) -> Vec<String> {
        self.plugins.iter().map(|entry| entry.key().clone()).collect()
    }
    
    /// Get plugin info
    pub async fn get_plugin_info(&self, name: &str) -> Option<PluginInfo> {
        if let Some(wrapper) = self.plugins.get(name) {
            let plugin = wrapper.read().await;
            Some(plugin.info())
        } else {
            None
        }
    }
    
    /// Get plugin capabilities
    pub async fn get_plugin_capabilities(&self, name: &str) -> Option<PluginCapabilities> {
        if let Some(wrapper) = self.plugins.get(name) {
            let plugin = wrapper.read().await;
            Some(plugin.capabilities())
        } else {
            None
        }
    }
    
    /// Get plugin health
    pub async fn get_plugin_health(&self, name: &str) -> Option<PluginHealth> {
        if let Some(wrapper) = self.plugins.get(name) {
            let plugin = wrapper.read().await;
            plugin.plugin.health().await.ok()
        } else {
            None
        }
    }
    
    /// Get plugin metrics
    pub async fn get_plugin_metrics(&self, name: &str) -> Option<PluginMetrics> {
        if let Some(wrapper) = self.plugins.get(name) {
            let plugin = wrapper.read().await;
            plugin.plugin.metrics().await.ok()
        } else {
            None
        }
    }
    
    /// Update plugin configuration
    pub async fn update_plugin_config(&self, name: &str, config: PluginConfig) -> Result<()> {
        let wrapper = self.plugins.get(name)
            .ok_or_else(|| anyhow::anyhow!("Plugin '{}' not found", name))?
            .clone();
        
        // Validate new configuration
        self.validate_plugin_config(name, &config).await?;
        
        // Update plugin
        {
            let mut plugin = wrapper.write().await;
            plugin.plugin.update_config(config.clone()).await?;
        }
        
        // Update stored configuration
        {
            let mut configs = self.plugin_configs.write().await;
            configs.insert(name.to_string(), config);
        }
        
        tracing::info!("Plugin '{}' configuration updated", name);
        Ok(())
    }
    
    /// Send event to specific plugin
    pub async fn send_event_to_plugin(&self, plugin_name: &str, event: PluginEvent) -> Result<()> {
        if let Some(wrapper) = self.plugins.get(plugin_name) {
            let plugin = wrapper.read().await;
            plugin.context.event_sender.send(event)
                .map_err(|e| anyhow::anyhow!("Failed to send event: {}", e))?;
            Ok(())
        } else {
            Err(anyhow::anyhow!("Plugin '{}' not found", plugin_name))
        }
    }
    
    /// Broadcast event to all plugins
    pub async fn broadcast_event(&self, event: PluginEvent) {
        for plugin_entry in self.plugins.iter() {
            let wrapper = plugin_entry.value().clone();
            let plugin = wrapper.read().await;
            let _ = plugin.context.event_sender.send(event.clone());
        }
    }
    
    /// Get aggregated metrics from all plugins
    pub async fn get_aggregated_metrics(&self) -> HashMap<String, PluginMetrics> {
        let mut all_metrics = HashMap::new();
        
        for plugin_entry in self.plugins.iter() {
            let name = plugin_entry.key().clone();
            let wrapper = plugin_entry.value().clone();
            let plugin = wrapper.read().await;
            
            if let Ok(metrics) = plugin.plugin.metrics().await {
                all_metrics.insert(name, metrics);
            }
        }
        
        all_metrics
    }
    
    /// Validate plugin configuration
    async fn validate_plugin_config(&self, name: &str, config: &PluginConfig) -> Result<()> {
        // Basic validation
        if config.name != name {
            return Err(anyhow::anyhow!("Plugin name mismatch"));
        }
        
        if config.bind_ports.is_empty() && config.domains.is_empty() {
            return Err(anyhow::anyhow!("Plugin must bind to at least one port or handle at least one domain"));
        }
        
        // Validate port ranges
        for port in &config.bind_ports {
            if *port == 0 || *port > 65535 {
                return Err(anyhow::anyhow!("Invalid port: {}", port));
            }
        }
        
        // Validate domain patterns
        for domain in &config.domains {
            if domain.is_empty() {
                return Err(anyhow::anyhow!("Empty domain pattern"));
            }
        }
        
        Ok(())
    }
    
    /// Graceful shutdown of all plugins
    pub async fn shutdown_all(&self) -> Result<()> {
        tracing::info!("Shutting down all plugins...");
        
        let plugin_names: Vec<String> = self.plugins.iter()
            .map(|entry| entry.key().clone())
            .collect();
        
        for name in plugin_names {
            if let Err(e) = self.stop_plugin(&name).await {
                tracing::error!("Error stopping plugin '{}': {}", name, e);
            }
        }
        
        tracing::info!("All plugins shut down");
        Ok(())
    }
}

/// Event router for inter-plugin communication
pub struct EventRouter {
    subscribers: Arc<RwLock<HashMap<String, Vec<String>>>>, // event_type -> plugin_names
}

impl EventRouter {
    pub fn new() -> Self {
        Self {
            subscribers: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    pub async fn subscribe(&self, plugin_name: String, event_type: String) {
        let mut subscribers = self.subscribers.write().await;
        subscribers
            .entry(event_type)
            .or_insert_with(Vec::new)
            .push(plugin_name);
    }
    
    pub async fn unsubscribe(&self, plugin_name: &str, event_type: &str) {
        let mut subscribers = self.subscribers.write().await;
        if let Some(plugin_list) = subscribers.get_mut(event_type) {
            plugin_list.retain(|name| name != plugin_name);
        }
    }
    
    pub async fn get_subscribers(&self, event_type: &str) -> Vec<String> {
        let subscribers = self.subscribers.read().await;
        subscribers.get(event_type).cloned().unwrap_or_default()
    }
}

/// Metrics aggregator
pub struct MetricsAggregator {
    // Could store historical metrics, calculate averages, etc.
}

impl MetricsAggregator {
    pub fn new() -> Self {
        Self {}
    }
    
    pub async fn aggregate_metrics(&self, plugin_metrics: &HashMap<String, PluginMetrics>) -> serde_json::Value {
        let mut total_connections = 0u64;
        let mut total_bytes_sent = 0u64;
        let mut total_bytes_received = 0u64;
        let mut total_errors = 0u64;
        
        for (_plugin_name, metrics) in plugin_metrics {
            total_connections += metrics.connections_total;
            total_bytes_sent += metrics.bytes_sent;
            total_bytes_received += metrics.bytes_received;
            total_errors += metrics.errors_total;
        }
        
        serde_json::json!({
            "total_connections": total_connections,
            "total_bytes_sent": total_bytes_sent,
            "total_bytes_received": total_bytes_received,
            "total_errors": total_errors,
            "plugin_count": plugin_metrics.len(),
            "plugins": plugin_metrics
        })
    }
}