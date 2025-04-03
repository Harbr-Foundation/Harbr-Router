// src/main.rs (updated)
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;

mod config;
mod metrics;
mod http_proxy;
mod tcp_proxy;  // TCP proxy module
mod udp_proxy;  // UDP proxy module

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter("info,harbr_router=debug")
        .init();

    // Load configuration
    let config_path = std::env::var("CONFIG_FILE").unwrap_or_else(|_| "config.yml".to_string());
    let config = config::load_config(&config_path)?;
    let config_arc = Arc::new(RwLock::new(config.clone()));

    // Initialize metrics
    metrics::init_metrics()?;

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
        let tcp_config = config_arc.clone();
        
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
        let udp_config = config_arc.clone();
        
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
    http_proxy::run_server(config_arc)
        .await
        .map_err(|e| anyhow::anyhow!("HTTP Server error: {}", e))?;

    Ok(())
}