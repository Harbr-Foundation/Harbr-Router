use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;

mod config;
mod metrics;
mod proxy;
mod tcp_proxy;  // Add the new TCP proxy module

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

    // Start TCP proxy if enabled or if database routes are detected
    if config.tcp_proxy.enabled || has_db_routes {
        tracing::info!("TCP proxy support enabled");
        let tcp_config = config_arc.clone();
        
        // Spawn TCP proxy server in a separate task
        tokio::spawn(async move {
            let tcp_proxy = tcp_proxy::TcpProxyServer::new(tcp_config);
            if let Err(e) = tcp_proxy.run(&config.tcp_proxy.listen_addr).await {
                tracing::error!("TCP proxy server error: {}", e);
            }
        });
    }

    // Start the HTTP proxy server
    proxy::run_server(config_arc)
        .await
        .map_err(|e| anyhow::anyhow!("HTTP Server error: {}", e))?;

    Ok(())
}