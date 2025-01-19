use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;

mod config;
mod metrics;
mod proxy;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter("info,rust_proxy=debug")
        .init();

    // Load configuration
    let config = config::load_config("config.yml")?;
    let config = Arc::new(RwLock::new(config));

    // Initialize metrics
    metrics::init_metrics()?;

    // Start the proxy server
    proxy::run_server(config)
        .await
        .map_err(|e| anyhow::anyhow!("Server error: {}", e))?;

    Ok(())
}
