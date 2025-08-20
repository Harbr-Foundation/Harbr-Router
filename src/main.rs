//! Harbr Router - High-performance reverse proxy
//!
//! A production-ready reverse proxy built with Rust, featuring:
//! - Multi-protocol support (HTTP/HTTPS, TCP, UDP)
//! - Advanced load balancing strategies
//! - Comprehensive metrics and monitoring
//! - Hot configuration reloading
//! - High-performance buffer management

use clap::{Arg, Command};
use harbr_router::{affinity, Config, Router, Result, DEFAULT_CONFIG_FILE, VERSION};
use std::path::Path;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging early
    init_logging();

    // Parse command line arguments
    let matches = Command::new("Harbr Router")
        .version(VERSION)
        .author("Harbr Foundation")
        .about("A high-performance reverse proxy built with Rust")
        .arg(
            Arg::new("config")
                .short('c')
                .long("config")
                .value_name("FILE")
                .help("Path to configuration file")
                .default_value(DEFAULT_CONFIG_FILE),
        )
        .arg(
            Arg::new("validate")
                .long("validate")
                .help("Validate configuration and exit")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("verbose")
                .short('v')
                .long("verbose")
                .help("Enable verbose logging")
                .action(clap::ArgAction::Count),
        )
        .get_matches();

    // Get configuration file path
    let config_path = matches.get_one::<String>("config").unwrap();
    let validate_only = matches.get_flag("validate");
    let verbosity = matches.get_count("verbose");

    // Adjust logging level based on verbosity
    adjust_logging_level(verbosity);

    info!("Starting Harbr Router v{}", VERSION);
    info!("Configuration file: {}", config_path);

    // Check if config file exists
    if !Path::new(config_path).exists() {
        error!("Configuration file not found: {}", config_path);
        std::process::exit(1);
    }

    // Load and validate configuration
    let config = match Config::from_file(config_path) {
        Ok(config) => {
            info!("Configuration loaded successfully");
            config
        }
        Err(e) => {
            error!("Failed to load configuration: {}", e);
            std::process::exit(1);
        }
    };

    // If validation only, exit after successful validation
    if validate_only {
        info!("Configuration is valid");
        return Ok(());
    }

    // Show system information
    display_system_info(&config);

    // Apply CPU affinity optimizations if enabled
    if config.global.performance.cpu_affinity {
        info!("CPU affinity enabled, applying optimizations");
        if let Err(e) = affinity::optimize_current_thread() {
            warn!("Failed to optimize current thread: {}", e);
        }
    } else {
        info!("Using default Tokio runtime");
    }

    // Run the router
    run_router(config).await
}

/// Initialize logging with sensible defaults
fn init_logging() {
    use tracing_subscriber::fmt;
    
    fmt()
        .with_env_filter("info,harbr_router=debug")
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .compact()
        .init();
}

/// Adjust logging level based on verbosity flag
fn adjust_logging_level(verbosity: u8) {
    // This is a simplified implementation
    // In a full implementation, you'd dynamically adjust the log level
    match verbosity {
        0 => {} // Default level (info)
        1 => info!("Verbose logging enabled"),
        2 => info!("Debug logging enabled"),
        _ => info!("Trace logging enabled"),
    }
}

/// Display system and configuration information
fn display_system_info(config: &Config) {
    info!("System Information:");
    info!("  CPU cores: {}", affinity::cpu_count());
    info!("  Optimal workers: {}", affinity::optimal_worker_count());
    info!("  
Configuration Summary:");
    info!("  Proxy instances: {}", config.proxy_instances.len());
    info!("  Metrics enabled: {}", config.global.metrics.enabled);
    info!("  Health checks enabled: {}", config.global.health_check.enabled);
    info!("  CPU affinity enabled: {}", config.global.performance.cpu_affinity);

    // Display proxy instances
    for instance in &config.proxy_instances {
        info!(
            "  Instance '{}': {:?} proxy on {} with {} upstreams",
            instance.name,
            instance.proxy_type,
            instance.listen_addr,
            instance.upstreams.len()
        );
    }
}

/// Main router execution
async fn run_router(config: Config) -> Result<()> {
    // Create and start the router
    let mut router = Router::new(config);
    
    // Handle startup errors gracefully
    match router.start().await {
        Ok(_) => {
            info!("Harbr Router shut down gracefully");
            Ok(())
        }
        Err(e) => {
            error!("Router failed: {}", e);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version() {
        assert!(!VERSION.is_empty());
        assert!(VERSION.contains('.'));
    }

    #[test] 
    fn test_default_config_file() {
        assert_eq!(DEFAULT_CONFIG_FILE, "config.yml");
    }
}