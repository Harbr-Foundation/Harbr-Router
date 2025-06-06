// src/main.rs
use anyhow::Result;
use harbr_router::{Router, DynamicConfigManager};
use std::sync::Arc;
use std::path::Path;
use clap::{Command, Arg};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter("info,harbr_router=debug")
        .init();

    // Parse command line arguments
    let matches = Command::new("Harbr Router")
        .version(env!("CARGO_PKG_VERSION"))
        .about("A modular reverse proxy service")
        .arg(
            Arg::new("config")
                .short('c')
                .long("config")
                .value_name("FILE")
                .help("Configuration file path")
                .default_value("config.yml")
                .action(clap::ArgAction::Set),
        )
        .arg(
            Arg::new("enable-api")
                .long("enable-api")
                .help("Enable the configuration API")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("api-port")
                .long("api-port")
                .value_name("PORT")
                .help("Port for the configuration API")
                .default_value("8082")
                .value_parser(clap::value_parser!(u16)),
        )
        .arg(
            Arg::new("watch-config")
                .long("watch-config")
                .help("Watch configuration file for changes")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("watch-interval")
                .long("watch-interval")
                .value_name("SECONDS")
                .help("Interval for checking configuration file changes")
                .default_value("30")
                .value_parser(clap::value_parser!(u64)),
        )
        .get_matches();

    // Get configuration file path
    let config_path = matches.get_one::<String>("config").map(String::as_str).unwrap_or("config.yml");
    let enable_api = matches.contains_id("enable-api");
    let api_port = *matches.get_one::<u16>("api-port").unwrap_or(&8082);
    let watch_config = matches.get_flag("watch-config");
    let watch_interval = *matches.get_one::<u64>("watch-interval").unwrap_or(&30);

    // Check if config file exists
    if !Path::new(config_path).exists() {
        tracing::error!("Configuration file not found: {}", config_path);
        std::process::exit(1);
    }

    // Load configuration using the dynamic configuration manager
    let config_manager = Arc::new(DynamicConfigManager::from_file(config_path).await?);
    
    // Start file watcher if requested
    if watch_config {
        tracing::info!("Starting configuration file watcher with interval of {} seconds", watch_interval);
        
        if let Err(e) = config_manager.start_file_watcher(watch_interval).await {
            tracing::error!("Failed to start file watcher: {}", e);
        }
    }

    // Create and start the router with the dynamic configuration manager
    let mut router = Router::new_with_manager(config_manager.clone());
    
    // Start the router
    router.start().await?;

    // We should never get here as router.start() blocks until shutdown
    Ok(())
}