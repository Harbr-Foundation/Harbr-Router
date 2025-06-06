// src/lib.rs - Main library entry point for embeddable router
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

pub mod config;
pub mod plugin;
pub mod server;
pub mod router;
pub mod middleware;
pub mod metrics;
pub mod error;
pub mod builtin_plugins;
pub mod management_api;
pub mod health;
pub mod logging;

// Re-export main types for easy usage
pub use config::RouterConfig;
pub use plugin::{Plugin, PluginConfig, PluginInfo, PluginCapabilities, PluginEvent, RouterEvent};
pub use server::ProxyServer;
pub use error::{RouterError, Result as RouterResult};

/// Main Router struct - embeddable and programmable
pub struct Router {
    server: Option<Arc<ProxyServer>>,
    config: Arc<tokio::sync::RwLock<RouterConfig>>,
    shutdown_sender: Option<broadcast::Sender<()>>,
    event_sender: Option<mpsc::UnboundedSender<RouterEvent>>,
    event_receiver: Option<mpsc::UnboundedReceiver<RouterEvent>>,
}

impl Router {
    /// Create a new router with configuration
    pub async fn new(config: RouterConfig) -> Result<Self> {
        let server = Arc::new(ProxyServer::new(config.clone()).await?);
        let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        
        Ok(Self {
            server: Some(server),
            config: Arc::new(tokio::sync::RwLock::new(config)),
            shutdown_sender: Some(shutdown_tx),
            event_sender: Some(event_tx),
            event_receiver: Some(event_rx),
        })
    }
    
    /// Create a router from JSON configuration file
    pub async fn from_config_file<P: AsRef<std::path::Path>>(path: P) -> Result<Self> {
        let config = RouterConfig::load_from_file(path).await?;
        Self::new(config).await
    }
    
    /// Create a router with default configuration
    pub async fn with_defaults() -> Result<Self> {
        let config = RouterConfig::default();
        Self::new(config).await
    }
    
    /// Start the router server
    pub async fn start(&mut self) -> Result<()> {
        if let Some(server) = &self.server {
            server.start().await?;
        } else {
            return Err(anyhow::anyhow!("Server not initialized"));
        }
        Ok(())
    }
    
    /// Stop the router server
    pub async fn stop(&mut self) -> Result<()> {
        if let Some(sender) = &self.shutdown_sender {
            let _ = sender.send(());
        }
        
        if let Some(server) = &self.server {
            server.stop().await?;
        }
        
        Ok(())
    }
    
    /// Get the plugin manager for programmatic plugin management
    pub fn plugin_manager(&self) -> Option<Arc<plugin::manager::PluginManager>> {
        self.server.as_ref().map(|s| s.plugin_manager())
    }
    
    /// Load a plugin from a file
    pub async fn load_plugin<P: AsRef<std::path::Path>>(
        &self,
        path: P,
        config: Option<PluginConfig>,
    ) -> Result<String> {
        if let Some(pm) = self.plugin_manager() {
            pm.load_plugin_from_path(path, config).await
        } else {
            Err(anyhow::anyhow!("Plugin manager not available"))
        }
    }
    
    /// Unload a plugin
    pub async fn unload_plugin(&self, name: &str) -> Result<()> {
        if let Some(pm) = self.plugin_manager() {
            pm.unload_plugin(name).await
        } else {
            Err(anyhow::anyhow!("Plugin manager not available"))
        }
    }
    
    /// Register a plugin programmatically (for embedded usage)
    pub async fn register_plugin(
        &self,
        name: String,
        plugin: Box<dyn Plugin>,
        config: PluginConfig,
    ) -> Result<()> {
        if let Some(pm) = self.plugin_manager() {
            pm.registry().register_plugin(name.clone(), plugin, config).await?;
            pm.registry().start_plugin(&name).await?;
            Ok(())
        } else {
            Err(anyhow::anyhow!("Plugin manager not available"))
        }
    }
    
    /// Add a route programmatically
    pub async fn add_route(&self, domain: String, proxy_instance: String) -> Result<()> {
        let mut config = self.config.write().await;
        
        // Find the proxy instance
        if !config.proxies.iter().any(|p| p.name == proxy_instance) {
            return Err(anyhow::anyhow!("Proxy instance '{}' not found", proxy_instance));
        }
        
        // Add domain mapping
        config.domains.insert(domain.clone(), config::DomainConfig {
            proxy_instance,
            backend_service: None,
            ssl_config: None,
            cors_config: None,
            cache_config: None,
            custom_headers: std::collections::HashMap::new(),
            rewrite_rules: vec![],
            access_control: None,
        });
        
        // Update the proxy instance to include this domain
        for proxy in &mut config.proxies {
            if proxy.name == proxy_instance {
                if !proxy.domains.contains(&domain) {
                    proxy.domains.push(domain);
                }
                break;
            }
        }
        
        // Notify server of configuration change
        if let Some(server) = &self.server {
            server.reload_config(config.clone()).await?;
        }
        
        Ok(())
    }
    
    /// Remove a route
    pub async fn remove_route(&self, domain: &str) -> Result<()> {
        let mut config = self.config.write().await;
        
        // Remove from domain mappings
        if let Some(domain_config) = config.domains.remove(domain) {
            // Remove from proxy instance
            for proxy in &mut config.proxies {
                if proxy.name == domain_config.proxy_instance {
                    proxy.domains.retain(|d| d != domain);
                    break;
                }
            }
            
            // Notify server of configuration change
            if let Some(server) = &self.server {
                server.reload_config(config.clone()).await?;
            }
        }
        
        Ok(())
    }
    
    /// Update configuration
    pub async fn update_config(&self, new_config: RouterConfig) -> Result<()> {
        {
            let mut config = self.config.write().await;
            *config = new_config.clone();
        }
        
        if let Some(server) = &self.server {
            server.reload_config(new_config).await?;
        }
        
        Ok(())
    }
    
    /// Get current configuration
    pub async fn get_config(&self) -> RouterConfig {
        self.config.read().await.clone()
    }
    
    /// Get plugin information
    pub async fn get_plugin_info(&self, name: &str) -> Option<PluginInfo> {
        self.plugin_manager()?.get_plugin_info(name).await
    }
    
    /// List all plugins
    pub fn list_plugins(&self) -> Vec<String> {
        self.plugin_manager().map(|pm| pm.list_plugins()).unwrap_or_default()
    }
    
    /// Get plugin health
    pub async fn get_plugin_health(&self, name: &str) -> Option<plugin::PluginHealth> {
        self.plugin_manager()?.get_plugin_health(name).await
    }
    
    /// Get plugin metrics
    pub async fn get_plugin_metrics(&self, name: &str) -> Option<plugin::PluginMetrics> {
        self.plugin_manager()?.get_plugin_metrics(name).await
    }
    
    /// Get all metrics
    pub async fn get_all_metrics(&self) -> std::collections::HashMap<String, plugin::PluginMetrics> {
        self.plugin_manager().map(|pm| 
            futures::executor::block_on(pm.get_all_metrics())
        ).unwrap_or_default()
    }
    
    /// Send event to plugin
    pub async fn send_event_to_plugin(&self, plugin_name: &str, event: PluginEvent) -> Result<()> {
        if let Some(pm) = self.plugin_manager() {
            pm.send_event_to_plugin(plugin_name, event).await
        } else {
            Err(anyhow::anyhow!("Plugin manager not available"))
        }
    }
    
    /// Broadcast event to all plugins
    pub async fn broadcast_event(&self, event: PluginEvent) {
        if let Some(pm) = self.plugin_manager() {
            pm.broadcast_event(event).await;
        }
    }
    
    /// Execute plugin command
    pub async fn execute_plugin_command(
        &self,
        plugin_name: &str,
        command: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        if let Some(pm) = self.plugin_manager() {
            pm.execute_plugin_command(plugin_name, command, args).await
        } else {
            Err(anyhow::anyhow!("Plugin manager not available"))
        }
    }
    
    /// Enable management API
    pub async fn enable_management_api(&self, port: u16) -> Result<()> {
        if let Some(server) = &self.server {
            server.enable_management_api(port).await
        } else {
            Err(anyhow::anyhow!("Server not available"))
        }
    }
    
    /// Get event receiver for monitoring router events
    pub fn take_event_receiver(&mut self) -> Option<mpsc::UnboundedReceiver<RouterEvent>> {
        self.event_receiver.take()
    }
    
    /// Get event sender for sending custom events
    pub fn event_sender(&self) -> Option<mpsc::UnboundedSender<RouterEvent>> {
        self.event_sender.clone()
    }
    
    /// Check if router is running
    pub async fn is_running(&self) -> bool {
        if let Some(server) = &self.server {
            server.is_running().await
        } else {
            false
        }
    }
    
    /// Get server statistics
    pub async fn get_statistics(&self) -> Option<server::ServerStatistics> {
        if let Some(server) = &self.server {
            Some(server.get_statistics().await)
        } else {
            None
        }
    }
    
    /// Graceful shutdown with timeout
    pub async fn shutdown_with_timeout(&mut self, timeout: std::time::Duration) -> Result<()> {
        let shutdown_future = self.stop();
        
        match tokio::time::timeout(timeout, shutdown_future).await {
            Ok(result) => result,
            Err(_) => {
                tracing::warn!("Graceful shutdown timed out, forcing shutdown");
                // Force shutdown if needed
                Ok(())
            }
        }
    }
    
    /// Wait for router to finish (blocking)
    pub async fn wait(&self) -> Result<()> {
        if let Some(server) = &self.server {
            server.wait().await
        } else {
            Ok(())
        }
    }
}

impl Drop for Router {
    fn drop(&mut self) {
        // Attempt graceful shutdown on drop
        if let Some(sender) = &self.shutdown_sender {
            let _ = sender.send(());
        }
    }
}

/// Builder pattern for Router configuration
pub struct RouterBuilder {
    config: RouterConfig,
}

impl RouterBuilder {
    pub fn new() -> Self {
        Self {
            config: RouterConfig::default(),
        }
    }
    
    /// Set listen addresses
    pub fn listen_on(mut self, addresses: Vec<String>) -> Self {
        self.config.server.listen_addresses = addresses;
        self
    }
    
    /// Add plugin directory
    pub fn plugin_directory<S: Into<String>>(mut self, directory: S) -> Self {
        self.config.plugins.plugin_directories.push(directory.into());
        self
    }
    
    /// Enable auto-reload
    pub fn auto_reload(mut self, enabled: bool) -> Self {
        self.config.plugins.auto_reload = enabled;
        self
    }
    
    /// Set max connections
    pub fn max_connections(mut self, max: usize) -> Self {
        self.config.server.max_connections = max;
        self
    }
    
    /// Enable metrics
    pub fn enable_metrics(mut self, port: u16) -> Self {
        self.config.monitoring.metrics_enabled = true;
        self.config.monitoring.metrics_port = port;
        self
    }
    
    /// Enable health checks
    pub fn enable_health_checks(mut self, port: u16) -> Self {
        self.config.server.health_check_port = port;
        self
    }
    
    /// Add proxy instance
    pub fn add_proxy(mut self, proxy: config::ProxyInstanceConfig) -> Self {
        self.config.proxies.push(proxy);
        self
    }
    
    /// Add domain mapping
    pub fn add_domain(mut self, domain: String, config: config::DomainConfig) -> Self {
        self.config.domains.insert(domain, config);
        self
    }
    
    /// Build the router
    pub async fn build(self) -> Result<Router> {
        Router::new(self.config).await
    }
}

impl Default for RouterBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience functions for quick setup
pub async fn create_http_proxy(
    listen_port: u16,
    domain: String,
    backend_url: String,
) -> Result<Router> {
    let proxy_config = config::ProxyInstanceConfig {
        name: "http_proxy".to_string(),
        plugin_type: "http_proxy".to_string(),
        enabled: true,
        priority: 0,
        ports: vec![listen_port],
        bind_addresses: vec!["0.0.0.0".to_string()],
        domains: vec![domain.clone()],
        plugin_config: serde_json::json!({
            "backend_url": backend_url,
            "timeout_ms": 30000,
            "retry_count": 3
        }),
        middleware: vec![],
        load_balancing: None,
        health_check: Some(config::HealthCheckConfig {
            enabled: true,
            interval_seconds: 30,
            timeout_seconds: 10,
            path: Some("/health".to_string()),
            expected_status: Some(200),
            custom_check: None,
        }),
        circuit_breaker: None,
        rate_limiting: None,
        ssl_config: None,
    };
    
    let domain_config = config::DomainConfig {
        proxy_instance: "http_proxy".to_string(),
        backend_service: Some(backend_url),
        ssl_config: None,
        cors_config: None,
        cache_config: None,
        custom_headers: std::collections::HashMap::new(),
        rewrite_rules: vec![],
        access_control: None,
    };
    
    RouterBuilder::new()
        .listen_on(vec![format!("0.0.0.0:{}", listen_port)])
        .add_proxy(proxy_config)
        .add_domain(domain, domain_config)
        .build()
        .await
}

pub async fn create_tcp_proxy(
    listen_port: u16,
    backend_address: String,
) -> Result<Router> {
    let proxy_config = config::ProxyInstanceConfig {
        name: "tcp_proxy".to_string(),
        plugin_type: "tcp_proxy".to_string(),
        enabled: true,
        priority: 0,
        ports: vec![listen_port],
        bind_addresses: vec!["0.0.0.0".to_string()],
        domains: vec![],
        plugin_config: serde_json::json!({
            "backend_address": backend_address,
            "connection_pooling": true,
            "max_idle_time_secs": 60
        }),
        middleware: vec![],
        load_balancing: None,
        health_check: Some(config::HealthCheckConfig {
            enabled: true,
            interval_seconds: 60,
            timeout_seconds: 5,
            path: None,
            expected_status: None,
            custom_check: Some(serde_json::json!({
                "type": "tcp_connect"
            })),
        }),
        circuit_breaker: None,
        rate_limiting: None,
        ssl_config: None,
    };
    
    RouterBuilder::new()
        .listen_on(vec![format!("0.0.0.0:{}", listen_port)])
        .add_proxy(proxy_config)
        .build()
        .await
}

/// Async-friendly router handle for embedding in other async applications
pub struct RouterHandle {
    router: Arc<tokio::sync::Mutex<Router>>,
    handle: tokio::task::JoinHandle<Result<()>>,
}

impl RouterHandle {
    pub async fn spawn(mut router: Router) -> Result<Self> {
        let router_arc = Arc::new(tokio::sync::Mutex::new(router));
        let router_clone = router_arc.clone();
        
        let handle = tokio::spawn(async move {
            let mut router = router_clone.lock().await;
            router.start().await?;
            router.wait().await
        });
        
        Ok(Self {
            router: router_arc,
            handle,
        })
    }
    
    pub async fn stop(&self) -> Result<()> {
        let mut router = self.router.lock().await;
        router.stop().await?;
        self.handle.abort();
        Ok(())
    }
    
    pub async fn router(&self) -> tokio::sync::MutexGuard<Router> {
        self.router.lock().await
    }
}