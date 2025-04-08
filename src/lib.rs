// src/lib.rs
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;

pub mod config;
pub mod metrics;
pub mod http_proxy;
pub mod tcp_proxy;  // TCP proxy module
pub mod udp_proxy;  // UDP proxy module

/// The main Router struct that manages all proxy services
pub struct Router {
    config: Arc<RwLock<config::ProxyConfig>>,
}

impl Router {
    /// Create a new Router with the provided configuration
    pub fn new(config: config::ProxyConfig) -> Self {
        Router {
            config: Arc::new(RwLock::new(config)),
        }
    }

    /// Create a new Router by loading configuration from a file
    pub fn from_file(config_path: &str) -> Result<Self> {
        let config = config::load_config(config_path)?;
        Ok(Router::new(config))
    }

    /// Start the router service with all enabled proxies
    pub async fn start(&self) -> Result<()> {
        // Initialize metrics
        metrics::init_metrics()?;

        let config = self.config.read().await.clone();

        // Check for database routes that should be handled as TCP
        let has_db_routes = config.routes.iter().any(|(_, route)| {
            config::is_likely_database(route)
        });

        // Check for UDP routes
        let has_udp_routes = config.routes.iter().any(|(_, route)| {
            route.is_udp.unwrap_or(false)
        });

        // Start TCP proxy if enabled or if database routes are detected
        if config.tcp_proxy.enabled || has_db_routes {
            tracing::info!("TCP proxy support enabled");
            let tcp_config = self.config.clone();
            
            // Spawn TCP proxy server in a separate task
            tokio::spawn(async move {
                let tcp_proxy = tcp_proxy::TcpProxyServer::new(tcp_config).await;
                if let Err(e) = tcp_proxy.run(&config.tcp_proxy.listen_addr).await {
                    tracing::error!("TCP proxy server error: {}", e);
                }
            });
        }
        
        // Start UDP proxy if enabled or if UDP routes are detected
        if config.tcp_proxy.udp_enabled || has_udp_routes {
            tracing::info!("UDP proxy support enabled");
            let udp_config = self.config.clone();
            
            // Use the same address as TCP proxy by default
            let udp_listen_addr = config.tcp_proxy.udp_listen_addr.clone();
            
            // Spawn UDP proxy server in a separate task
            tokio::spawn(async move {
                let udp_proxy = udp_proxy::UdpProxyServer::new(udp_config);
                if let Err(e) = udp_proxy.run(&udp_listen_addr).await {
                    tracing::error!("UDP proxy server error: {}", e);
                }
            });
        }

        // Start the HTTP proxy server
        http_proxy::run_server(self.config.clone())
            .await
            .map_err(|e| anyhow::anyhow!("HTTP Server error: {}", e))?;

        Ok(())
    }
}

// Re-export types for easier usage
pub use config::{ProxyConfig, RouteConfig, TcpProxyConfig};