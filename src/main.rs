// src/main.rs
use anyhow::Result;
use harbr_router::{Router, ProxyConfig, RouteConfig, TcpProxyConfig};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter("info,harbr_router=debug")
        .init();

    // Load configuration from file by default
    let config_path = std::env::var("CONFIG_FILE").unwrap_or_else(|_| "config.yml".to_string());
    let router = Router::from_file(&config_path)?;

    // Start the router
    router.start().await?;

    Ok(())
}