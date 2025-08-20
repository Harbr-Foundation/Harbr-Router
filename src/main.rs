// src/main.rs
use anyhow::Result;
use harbr_router::ModernRouter;
use std::path::Path;
use clap::{Command, Arg};

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
        .get_matches();

    // Get configuration file path
    let config_path = matches.get_one::<String>("config").map(String::as_str).unwrap_or("config.yml");

    // Check if config file exists
    if !Path::new(config_path).exists() {
        tracing::error!("Configuration file not found: {}", config_path);
        std::process::exit(1);
    }

    tracing::info!("Starting Harbr Router with modular proxy system");
    
    let mut modern_router = ModernRouter::from_file(config_path).await?;
    modern_router.start().await?;

    // We should never get here as router.start() blocks until shutdown
    Ok(())
}