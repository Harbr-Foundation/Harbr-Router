//! Main router implementation with graceful shutdown and lifecycle management.
//!
//! This module provides the central Router that orchestrates all proxy instances,
//! metrics collection, health monitoring, and configuration management.

use crate::{
    affinity,
    config::{Config, ProxyInstanceConfig},
    routing::LoadBalancingStrategy,
    metrics::{MetricsServer, global_registry},
    proxy::ProxyManager,
    routing::Router as LoadBalancer,
    server::HealthServer,
    Error, Result,
};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    signal,
    sync::broadcast,
    time::{interval, timeout},
};
use tracing::{error, info, warn};

/// Main router that orchestrates all system components.
pub struct Router {
    /// System configuration
    config: Config,
    /// Proxy manager for handling different proxy types
    proxy_manager: Arc<ProxyManager>,
    /// Metrics server for observability
    metrics_server: Option<Arc<MetricsServer>>,
    /// Health server for monitoring
    health_server: Option<Arc<HealthServer>>,
    /// Shutdown signal broadcaster
    shutdown_sender: broadcast::Sender<()>,
    /// Running proxy instances with their load balancers
    running_instances: HashMap<String, Arc<LoadBalancer>>,
}

impl Router {
    /// Create a new router from configuration.
    pub fn new(config: Config) -> Self {
        let (shutdown_sender, _) = broadcast::channel(32);
        
        let metrics_server = if config.global.metrics.enabled {
            Some(Arc::new(MetricsServer::new(config.global.metrics.clone())))
        } else {
            None
        };

        let health_server = if config.global.health_check.enabled {
            Some(Arc::new(HealthServer::new(config.global.health_check.clone())))
        } else {
            None
        };

        Self {
            config,
            proxy_manager: Arc::new(ProxyManager::new()),
            metrics_server,
            health_server,
            shutdown_sender,
            running_instances: HashMap::new(),
        }
    }

    /// Load router from configuration file.
    pub async fn from_file<P: AsRef<std::path::Path>>(path: P) -> Result<Self> {
        let config = Config::from_file(path)?;
        Ok(Self::new(config))
    }

    /// Start the router and all its components.
    pub async fn start(&mut self) -> Result<()> {
        info!("Starting Harbr Router v{}", crate::VERSION);

        // Optimize the current thread for performance
        if let Err(e) = affinity::optimize_current_thread() {
            warn!("Failed to optimize main thread: {}", e);
        }

        // Start metrics server if enabled
        if let Some(metrics_server) = &self.metrics_server {
            let server = metrics_server.clone();
            tokio::spawn(async move {
                if let Err(e) = server.start().await {
                    error!("Metrics server failed: {}", e);
                }
            });
        }

        // Start health server if enabled
        if let Some(health_server) = &self.health_server {
            let server = health_server.clone();
            tokio::spawn(async move {
                if let Err(e) = server.start().await {
                    error!("Health server failed: {}", e);
                }
            });
        }

        // Start all proxy instances
        self.start_proxy_instances().await?;

        // Start periodic tasks
        self.start_periodic_tasks().await;

        // Update global metrics
        global_registry().set_proxy_instances_count(self.config.proxy_instances.len());

        info!("Harbr Router started successfully with {} proxy instances", self.config.proxy_instances.len());

        // Wait for shutdown signal
        self.wait_for_shutdown().await;

        // Perform graceful shutdown
        self.shutdown().await?;

        Ok(())
    }

    /// Start all configured proxy instances.
    async fn start_proxy_instances(&mut self) -> Result<()> {
        let instance_configs = self.config.proxy_instances.clone();
        for instance_config in instance_configs {
            self.start_proxy_instance(instance_config).await?;
        }
        Ok(())
    }

    /// Start a single proxy instance.
    async fn start_proxy_instance(&mut self, config: ProxyInstanceConfig) -> Result<()> {
        info!("Starting proxy instance '{}' ({})", config.name, config.proxy_type as u8);

        // Create load balancer for this instance
        let strategy = match config.proxy_type {
            crate::config::ProxyType::Http => LoadBalancingStrategy::HealthAware,
            crate::config::ProxyType::Tcp => LoadBalancingStrategy::LeastConnections,
            crate::config::ProxyType::Udp => LoadBalancingStrategy::WeightedRoundRobin,
        };

        let load_balancer = Arc::new(LoadBalancer::new(strategy));
        
        // Update load balancer with upstreams
        load_balancer.update_upstreams(config.upstreams.clone())?;

        // Add upstreams to global metrics tracking
        for upstream_config in &config.upstreams {
            let upstream_info = Arc::new(crate::routing::UpstreamInfo::new(upstream_config.clone()));
            global_registry().add_upstream(upstream_info);
        }

        // Start the proxy
        self.proxy_manager.start_proxy(config.clone(), load_balancer.clone()).await?;

        // Track the running instance
        self.running_instances.insert(config.name.clone(), load_balancer);

        info!("Proxy instance '{}' started successfully", config.name);
        Ok(())
    }

    /// Start periodic maintenance tasks.
    async fn start_periodic_tasks(&self) {
        // Start metrics collection task
        self.start_metrics_collection_task().await;
        
        // Start health monitoring task
        self.start_health_monitoring_task().await;
    }

    /// Start metrics collection task.
    async fn start_metrics_collection_task(&self) {
        let proxy_manager = self.proxy_manager.clone();
        let metrics_interval = Duration::from_secs(self.config.global.metrics.interval_secs);

        tokio::spawn(async move {
            let mut ticker = interval(metrics_interval);
            
            loop {
                ticker.tick().await;
                
                // Collect metrics from all proxies
                match proxy_manager.get_all_metrics().await {
                    Ok(all_metrics) => {
                        for (proxy_type, metrics) in all_metrics {
                            let instance_name = format!("{:?}", proxy_type);
                            global_registry().update_proxy_metrics(instance_name, metrics);
                        }
                    }
                    Err(e) => {
                        warn!("Failed to collect proxy metrics: {}", e);
                    }
                }
            }
        });
    }

    /// Start health monitoring task.
    async fn start_health_monitoring_task(&self) {
        let proxy_manager = self.proxy_manager.clone();
        let health_check_interval = Duration::from_secs(30); // Check every 30 seconds

        tokio::spawn(async move {
            let mut ticker = interval(health_check_interval);
            
            loop {
                ticker.tick().await;
                
                // Perform health checks on all proxies
                match proxy_manager.health_check_all().await {
                    Ok(health_results) => {
                        for (proxy_type, health) in &health_results {
                            match health.status {
                                crate::routing::HealthState::Unhealthy => {
                                    warn!("Proxy {:?} is unhealthy: {}", proxy_type, health.message);
                                }
                                crate::routing::HealthState::Unknown => {
                                    info!("Proxy {:?} status unknown: {}", proxy_type, health.message);
                                }
                                crate::routing::HealthState::Healthy => {
                                    // Healthy - no need to log unless debug level
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to perform health checks: {}", e);
                    }
                }
            }
        });
    }

    /// Wait for shutdown signal (SIGTERM, SIGINT, or CTRL+C).
    async fn wait_for_shutdown(&self) {
        let mut shutdown_receiver = self.shutdown_sender.subscribe();

        #[cfg(unix)]
        {
            tokio::select! {
                // Wait for OS signal
                _ = signal::ctrl_c() => {
                    info!("Received CTRL+C signal");
                }
                // Wait for programmatic shutdown
                _ = shutdown_receiver.recv() => {
                    info!("Received programmatic shutdown signal");
                }
                // On Unix systems, also listen for SIGTERM
                _ = self.wait_for_sigterm() => {
                    info!("Received SIGTERM signal");
                }
            }
        }
        
        #[cfg(not(unix))]
        {
            tokio::select! {
                // Wait for OS signal
                _ = signal::ctrl_c() => {
                    info!("Received CTRL+C signal");
                }
                // Wait for programmatic shutdown
                _ = shutdown_receiver.recv() => {
                    info!("Received programmatic shutdown signal");
                }
            }
        }
    }

    /// Wait for SIGTERM signal on Unix systems.
    #[cfg(unix)]
    async fn wait_for_sigterm(&self) {
        use tokio::signal::unix::{signal, SignalKind};
        
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(e) => {
                warn!("Failed to register SIGTERM handler: {}", e);
                return;
            }
        };
        
        sigterm.recv().await;
    }

    /// Perform graceful shutdown of all components.
    async fn shutdown(&self) -> Result<()> {
        info!("Starting graceful shutdown...");

        // Shutdown proxy manager (this will stop all proxies)
        if let Err(e) = self.proxy_manager.shutdown() {
            warn!("Error shutting down proxy manager: {}", e);
        }

        // Shutdown metrics server
        if let Some(metrics_server) = &self.metrics_server {
            if let Err(e) = metrics_server.shutdown() {
                warn!("Error shutting down metrics server: {}", e);
            }
        }

        // Shutdown health server
        if let Some(health_server) = &self.health_server {
            if let Err(e) = health_server.shutdown() {
                warn!("Error shutting down health server: {}", e);
            }
        }

        // Give components time to shutdown gracefully
        let shutdown_timeout = Duration::from_secs(30);
        if let Err(_) = timeout(shutdown_timeout, self.wait_for_component_shutdown()).await {
            warn!("Shutdown timeout exceeded, forcing exit");
        }

        info!("Graceful shutdown completed");
        Ok(())
    }

    /// Wait for all components to shut down gracefully.
    async fn wait_for_component_shutdown(&self) {
        // In a full implementation, you'd wait for all spawned tasks to complete
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    /// Trigger programmatic shutdown.
    pub fn trigger_shutdown(&self) -> Result<()> {
        if let Err(e) = self.shutdown_sender.send(()) {
            return Err(Error::internal(format!("Failed to send shutdown signal: {}", e)));
        }
        Ok(())
    }

    /// Get configuration for a specific proxy instance.
    pub fn get_instance_config(&self, name: &str) -> Option<&ProxyInstanceConfig> {
        self.config.get_instance(name)
    }

    /// Get all proxy instance configurations.
    pub fn get_all_instance_configs(&self) -> &[ProxyInstanceConfig] {
        &self.config.proxy_instances
    }

    /// Get the current system configuration.
    pub fn get_config(&self) -> &Config {
        &self.config
    }

    /// Reload configuration from file.
    pub async fn reload_config<P: AsRef<std::path::Path>>(&mut self, path: P) -> Result<()> {
        info!("Reloading configuration from file");

        let new_config = Config::from_file(path)?;
        
        // Validate new configuration
        // In a full implementation, you'd compare configs and restart only changed instances
        
        self.config = new_config;
        global_registry().record_config_reload();
        
        info!("Configuration reloaded successfully");
        Ok(())
    }
}

impl Default for Router {
    fn default() -> Self {
        let default_config = Config {
            global: crate::config::GlobalConfig::default(),
            proxy_instances: Vec::new(),
        };
        Self::new(default_config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;

    fn create_test_config() -> Config {
        Config {
            global: GlobalConfig {
                metrics: MetricsConfig {
                    enabled: false,
                    listen_addr: "127.0.0.1:0".parse().unwrap(),
                    path: "/metrics".to_string(),
                    interval_secs: 60,
                },
                health_check: HealthCheckConfig {
                    enabled: false,
                    listen_addr: "127.0.0.1:0".parse().unwrap(),
                    path: "/health".to_string(),
                },
                performance: PerformanceConfig::default(),
                logging: LoggingConfig::default(),
            },
            proxy_instances: vec![
                ProxyInstanceConfig {
                    name: "test-http".to_string(),
                    proxy_type: ProxyType::Http,
                    listen_addr: "127.0.0.1:8080".parse().unwrap(),
                    upstreams: vec![UpstreamConfig {
                        name: "test-upstream".to_string(),
                        address: "127.0.0.1:8081".to_string(),
                        weight: 1,
                        priority: 0,
                        timeout_ms: 5000,
                        retry: RetryConfig::default(),
                        health_check: None,
                        tls: None,
                    }],
                    config: ProxySpecificConfig::default(),
                }
            ],
        }
    }

    #[test]
    fn test_router_creation() {
        let config = create_test_config();
        let router = Router::new(config);
        
        assert_eq!(router.config.proxy_instances.len(), 1);
        assert!(!router.config.global.metrics.enabled);
        assert!(!router.config.global.health_check.enabled);
    }

    #[test]
    fn test_router_config_access() {
        let config = create_test_config();
        let router = Router::new(config);
        
        assert!(router.get_instance_config("test-http").is_some());
        assert!(router.get_instance_config("non-existent").is_none());
        
        assert_eq!(router.get_all_instance_configs().len(), 1);
    }

    #[tokio::test]
    async fn test_router_shutdown() {
        let config = create_test_config();
        let router = Router::new(config);
        
        // Test programmatic shutdown
        assert!(router.trigger_shutdown().is_ok());
    }
}