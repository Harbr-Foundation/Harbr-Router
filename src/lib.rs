// src/lib.rs
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, RwLock};

pub mod client;
pub mod config;
pub mod metrics;
pub mod http_proxy;
pub mod tcp_proxy;
pub mod udp_proxy;
pub mod dynamic_config;
pub mod config_api;

/// The main Router struct that manages all proxy services
pub struct Router {
    config_manager: Arc<DynamicConfigManager>,
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl Router {
    /// Create a new Router with the provided configuration manager
    pub fn new_with_manager(config_manager: Arc<DynamicConfigManager>) -> Self {
        Router {
            config_manager,
            shutdown_tx: None,
        }
    }

    /// Create a new Router with the provided configuration
    pub fn new(config: config::ProxyConfig) -> Self {
        let config_manager = Arc::new(DynamicConfigManager::new(config));
        Self::new_with_manager(config_manager)
    }

    /// Create a new Router by loading configuration from a file
    pub async fn from_file(config_path: &str) -> Result<Self> {
        let config_manager = DynamicConfigManager::from_file(config_path).await?;
        Ok(Router::new_with_manager(Arc::new(config_manager)))
    }

    /// Get the configuration manager
    pub fn config_manager(&self) -> Arc<DynamicConfigManager> {
        self.config_manager.clone()
    }

    /// Start the router service with all enabled proxies
    pub async fn start(&mut self) -> Result<()> {
        // Initialize metrics
        metrics::init_metrics()?;

        // Create a shutdown channel
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        // Set up config change listener
        let mut config_rx = self.config_manager.subscribe();
        let config_manager = self.config_manager.clone();

        // Initial configuration
        let initial_config = self.config_manager.get_config().read().await.clone();

        // Start the HTTP proxy server
        let http_config = self.config_manager.get_config().clone();
        let http_handle = tokio::spawn(async move {
            if let Err(e) = http_proxy::run_server(http_config).await {
                tracing::error!("HTTP Server error: {}", e);
            }
        });

        // Check for database routes that should be handled as TCP
        let has_db_routes = initial_config.routes.iter().any(|(_, route)| {
            config::is_likely_database(route)
        });

        // Check for UDP routes
        let has_udp_routes = initial_config.routes.iter().any(|(_, route)| {
            route.is_udp.unwrap_or(false)
        });

        // Start TCP proxy if enabled or if database routes are detected
        let tcp_handle = if initial_config.tcp_proxy.enabled || has_db_routes {
            tracing::info!("TCP proxy support enabled");
            let tcp_config = self.config_manager.get_config().clone();
            
            let handle = tokio::spawn(async move {
                let tcp_proxy = tcp_proxy::TcpProxyServer::new(tcp_config).await;
                if let Err(e) = tcp_proxy.run(&initial_config.tcp_proxy.listen_addr).await {
                    tracing::error!("TCP proxy server error: {}", e);
                }
            });
            Some(handle)
        } else {
            None
        };
        
        // Start UDP proxy if enabled or if UDP routes are detected
        let udp_handle = if initial_config.tcp_proxy.udp_enabled || has_udp_routes {
            tracing::info!("UDP proxy support enabled");
            let udp_config = self.config_manager.get_config().clone();
            
            // Use the same address as TCP proxy by default
            let udp_listen_addr = initial_config.tcp_proxy.udp_listen_addr.clone();
            
            let handle = tokio::spawn(async move {
                let udp_proxy = udp_proxy::UdpProxyServer::new(udp_config);
                if let Err(e) = udp_proxy.run(&udp_listen_addr).await {
                    tracing::error!("UDP proxy server error: {}", e);
                }
            });
            Some(handle)
        } else {
            None
        };

        // Set up the config API
        let api_handle = {
            let api_routes = config_api::config_api_routes(self.config_manager.clone());
            
            // Create a separate server for the API on a different port
            let api_port = 8082; // Could be configurable
            let api_addr = format!("0.0.0.0:{}", api_port);
            
            tracing::info!("Starting configuration API server on {}", api_addr);
            
            tokio::spawn(async move {
                warp::serve(api_routes)
                    .run(api_addr.parse::<std::net::SocketAddr>().unwrap())
                    .await;
            })
        };

        // Listen for configuration changes and handle them
        let config_change_handle = tokio::spawn(async move {
            tracing::info!("Starting configuration change listener");
            
            loop {
                tokio::select! {
                    // Handle shutdown signal
                    _ = shutdown_rx.recv() => {
                        tracing::info!("Received shutdown signal, stopping config listener");
                        break;
                    }
                    
                    // Handle configuration changes
                    result = config_rx.recv() => {
                        match result {
                            Ok(event) => {
                                tracing::info!("Received configuration change event: {:?}", event);
                                
                                match event {
                                    ConfigEvent::RouteAdded(name, config) => {
                                        tracing::info!("Route added: {}", name);
                                        // No need to restart services, they'll pick up the change
                                    }
                                    ConfigEvent::RouteUpdated(name, config) => {
                                        tracing::info!("Route updated: {}", name);
                                        // No need to restart services, they'll pick up the change
                                    }
                                    ConfigEvent::RouteRemoved(name) => {
                                        tracing::info!("Route removed: {}", name);
                                        // No need to restart services, they'll pick up the change
                                    }
                                    ConfigEvent::TcpConfigUpdated(tcp_config) => {
                                        tracing::warn!("TCP configuration updated - some changes may require restart");
                                        // Here we could potentially restart TCP services if needed
                                    }
                                    ConfigEvent::GlobalSettingsUpdated { .. } => {
                                        tracing::warn!("Global settings updated - some changes may require restart");
                                        // Here we could potentially restart services if needed
                                    }
                                    ConfigEvent::FullUpdate(_) => {
                                        tracing::warn!("Full configuration replaced - some changes may require restart");
                                        // Here we could potentially restart all services if needed
                                    }
                                }
                            }
                            Err(e) => {
                                if matches!(e, tokio::sync::broadcast::error::RecvError::Lagged(_)) {
                                    tracing::warn!("Config listener lagged and missed messages");
                                } else {
                                    tracing::error!("Error receiving config change: {}", e);
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        });

        // TODO: Add this
        // // Optional: start file watcher if config was loaded from file
        // if let Some(path) = config_manager.file_path() {
        //     if let Err(e) = config_manager.start_file_watcher(30).await {
        //         tracing::error!("Failed to start file watcher: {}", e);
        //     }
        // }

        // Wait for Ctrl+C or other shutdown signal
        tokio::signal::ctrl_c().await?;
        tracing::info!("Received shutdown signal");
        
        // Attempt graceful shutdown
        if let Some(tx) = &self.shutdown_tx {
            let _ = tx.send(()).await;
        }
        
        Ok(())
    }

    /// Manually trigger a configuration reload from file
    pub async fn reload_config(&self) -> Result<()> {
        self.config_manager.reload_from_file().await
    }
    
    /// Get the current configuration
    pub async fn get_config(&self) -> config::ProxyConfig {
        self.config_manager.get_config().read().await.clone()
    }
    
    /// Update a specific route
    pub async fn update_route(&self, route_name: &str, route_config: config::RouteConfig) -> Result<()> {
        self.config_manager.update_route(route_name, route_config).await
    }
    
    /// Add a new route
    pub async fn add_route(&self, route_name: &str, route_config: config::RouteConfig) -> Result<()> {
        self.config_manager.add_route(route_name, route_config).await
    }
    
    /// Remove a route
    pub async fn remove_route(&self, route_name: &str) -> Result<()> {
        self.config_manager.remove_route(route_name).await
    }
    
    /// Update TCP proxy configuration
    pub async fn update_tcp_config(&self, tcp_config: config::TcpProxyConfig) -> Result<()> {
        self.config_manager.update_tcp_config(tcp_config).await
    }
    
    /// Update global settings
    pub async fn update_global_settings(
        &self,
        listen_addr: Option<String>,
        global_timeout_ms: Option<u64>,
        max_connections: Option<usize>,
    ) -> Result<()> {
        self.config_manager.update_global_settings(listen_addr, global_timeout_ms, max_connections).await
    }
    
    /// Replace the entire configuration
    pub async fn replace_config(&self, new_config: config::ProxyConfig) -> Result<()> {
        self.config_manager.replace_config(new_config).await
    }
}

// Re-export types for easier usage
pub use config::{ProxyConfig, RouteConfig, TcpProxyConfig};
pub use dynamic_config::{DynamicConfigManager, ConfigEvent};
pub use client::ConfigClient;