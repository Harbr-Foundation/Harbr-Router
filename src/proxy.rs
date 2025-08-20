//! Proxy implementations for different protocols.
//!
//! This module provides high-performance proxy implementations for HTTP, TCP, and UDP
//! protocols with advanced features like connection pooling, health checking, and metrics.

pub mod http;
pub mod tcp;
pub mod udp;

use crate::{
    config::{ProxyInstanceConfig, ProxyType},
    routing::{HealthState, Router, UpstreamInfo},
    Error, Result,
};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::broadcast;

pub use http::HttpProxy;
pub use tcp::TcpProxy;
pub use udp::UdpProxy;

/// Shutdown signal type for coordinating proxy shutdown
pub type ShutdownSignal = broadcast::Receiver<()>;
pub type ShutdownSender = broadcast::Sender<()>;

/// Trait for proxy implementations.
#[async_trait]
pub trait Proxy: Send + Sync + 'static {
    /// Get the proxy type identifier.
    fn proxy_type(&self) -> ProxyType;

    /// Start the proxy with the given configuration.
    async fn start(
        &self,
        config: ProxyInstanceConfig,
        router: Arc<Router>,
        shutdown: ShutdownSignal,
    ) -> Result<()>;

    /// Validate proxy-specific configuration.
    fn validate_config(&self, config: &ProxyInstanceConfig) -> Result<()>;

    /// Get proxy-specific metrics (optional).
    async fn get_metrics(&self) -> Result<ProxyMetrics> {
        Ok(ProxyMetrics::default())
    }

    /// Perform health check on the proxy itself (optional).
    async fn health_check(&self) -> Result<ProxyHealth> {
        Ok(ProxyHealth {
            status: HealthState::Healthy,
            message: "OK".to_string(),
        })
    }
}

/// Metrics collected by proxy implementations.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct ProxyMetrics {
    /// Total requests/connections handled
    pub total_requests: u64,
    /// Currently active connections
    pub active_connections: u64,
    /// Total bytes sent
    pub bytes_sent: u64,
    /// Total bytes received
    pub bytes_received: u64,
    /// Request rate (requests per second)
    pub request_rate: f64,
    /// Error rate (0.0 to 1.0)
    pub error_rate: f64,
    /// Average response time in milliseconds
    pub avg_response_time_ms: f64,
}

/// Health check result for a proxy.
#[derive(Debug, Clone)]
pub struct ProxyHealth {
    /// Current health state
    pub status: HealthState,
    /// Human-readable health message
    pub message: String,
}

/// Manager for proxy instances.
pub struct ProxyManager {
    /// Available proxy implementations
    proxies: std::collections::HashMap<ProxyType, Box<dyn Proxy>>,
    /// Shutdown signal broadcaster
    shutdown_sender: ShutdownSender,
}

impl ProxyManager {
    /// Create a new proxy manager.
    pub fn new() -> Self {
        let (shutdown_sender, _) = broadcast::channel(16);
        
        let mut proxies: std::collections::HashMap<ProxyType, Box<dyn Proxy>> = std::collections::HashMap::new();
        proxies.insert(ProxyType::Http, Box::new(HttpProxy::new()));
        proxies.insert(ProxyType::Tcp, Box::new(TcpProxy::new()));
        proxies.insert(ProxyType::Udp, Box::new(UdpProxy::new()));

        Self {
            proxies,
            shutdown_sender,
        }
    }

    /// Start a proxy instance.
    pub async fn start_proxy(
        &self,
        config: ProxyInstanceConfig,
        router: Arc<Router>,
    ) -> Result<()> {
        let proxy = self.proxies.get(&config.proxy_type)
            .ok_or_else(|| Error::config(format!("Unsupported proxy type: {:?}", config.proxy_type)))?;

        // Validate configuration
        proxy.validate_config(&config)?;

        // Create shutdown receiver
        let shutdown = self.shutdown_sender.subscribe();

        // Start the proxy
        proxy.start(config, router, shutdown).await
    }

    /// Validate a proxy configuration without starting it.
    pub fn validate_config(&self, config: &ProxyInstanceConfig) -> Result<()> {
        let proxy = self.proxies.get(&config.proxy_type)
            .ok_or_else(|| Error::config(format!("Unsupported proxy type: {:?}", config.proxy_type)))?;

        proxy.validate_config(config)
    }

    /// Get metrics from all running proxies.
    pub async fn get_all_metrics(&self) -> Result<std::collections::HashMap<ProxyType, ProxyMetrics>> {
        let mut metrics = std::collections::HashMap::new();
        
        for (proxy_type, proxy) in &self.proxies {
            match proxy.get_metrics().await {
                Ok(proxy_metrics) => {
                    metrics.insert(*proxy_type, proxy_metrics);
                }
                Err(e) => {
                    tracing::warn!("Failed to get metrics for {:?} proxy: {}", proxy_type, e);
                }
            }
        }
        
        Ok(metrics)
    }

    /// Perform health checks on all proxies.
    pub async fn health_check_all(&self) -> Result<std::collections::HashMap<ProxyType, ProxyHealth>> {
        let mut health_results = std::collections::HashMap::new();
        
        for (proxy_type, proxy) in &self.proxies {
            match proxy.health_check().await {
                Ok(health) => {
                    health_results.insert(*proxy_type, health);
                }
                Err(e) => {
                    health_results.insert(*proxy_type, ProxyHealth {
                        status: HealthState::Unhealthy,
                        message: format!("Health check failed: {}", e),
                    });
                }
            }
        }
        
        Ok(health_results)
    }

    /// Initiate graceful shutdown of all proxies.
    pub fn shutdown(&self) -> Result<()> {
        if let Err(e) = self.shutdown_sender.send(()) {
            tracing::warn!("Failed to send shutdown signal: {}", e);
        }
        Ok(())
    }

    /// Get the list of supported proxy types.
    pub fn supported_proxy_types(&self) -> Vec<ProxyType> {
        self.proxies.keys().copied().collect()
    }
}

impl Default for ProxyManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Utility functions for proxy implementations.
pub mod utils {
    use super::*;
    use std::net::SocketAddr;
    use std::time::Duration;

    /// Parse an address string into a SocketAddr.
    pub fn parse_address(address: &str) -> Result<SocketAddr> {
        address.parse()
            .map_err(|e| Error::config(format!("Invalid address '{}': {}", address, e)))
    }

    /// Convert milliseconds to Duration.
    pub fn millis_to_duration(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    /// Convert Duration to milliseconds as f64.
    pub fn duration_to_millis_f64(duration: Duration) -> f64 {
        duration.as_secs_f64() * 1000.0
    }

    /// Check if an address is a valid upstream target.
    pub fn validate_upstream_address(address: &str) -> Result<()> {
        if address.is_empty() {
            return Err(Error::config("Upstream address cannot be empty"));
        }

        if !address.contains(':') {
            return Err(Error::config("Upstream address must include port"));
        }

        // Try to parse as socket address
        parse_address(address)?;
        Ok(())
    }

    /// Create a connection timeout from configuration.
    pub fn create_timeout(timeout_ms: u64, operation: &str) -> Result<Duration> {
        if timeout_ms == 0 {
            return Err(Error::config(format!("{} timeout cannot be zero", operation)));
        }

        if timeout_ms > 300_000 {
            return Err(Error::config(format!("{} timeout cannot exceed 5 minutes", operation)));
        }

        Ok(Duration::from_millis(timeout_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;

    fn create_test_config(proxy_type: ProxyType, name: &str) -> ProxyInstanceConfig {
        ProxyInstanceConfig {
            name: name.to_string(),
            proxy_type,
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
    }

    #[test]
    fn test_proxy_manager_creation() {
        let manager = ProxyManager::new();
        let supported_types = manager.supported_proxy_types();
        
        assert!(supported_types.contains(&ProxyType::Http));
        assert!(supported_types.contains(&ProxyType::Tcp));
        assert!(supported_types.contains(&ProxyType::Udp));
        assert_eq!(supported_types.len(), 3);
    }

    #[test]
    fn test_config_validation() {
        let manager = ProxyManager::new();
        
        // Test valid configuration
        let config = create_test_config(ProxyType::Http, "test-http");
        assert!(manager.validate_config(&config).is_ok());
        
        // Test invalid configuration (empty upstreams)
        let mut invalid_config = config.clone();
        invalid_config.upstreams.clear();
        assert!(manager.validate_config(&invalid_config).is_err());
    }

    #[test]
    fn test_utils() {
        use utils::*;
        
        // Test address parsing
        assert!(parse_address("127.0.0.1:8080").is_ok());
        assert!(parse_address("invalid").is_err());
        
        // Test address validation
        assert!(validate_upstream_address("127.0.0.1:8080").is_ok());
        assert!(validate_upstream_address("").is_err());
        assert!(validate_upstream_address("no-port").is_err());
        
        // Test timeout creation
        assert!(create_timeout(1000, "test").is_ok());
        assert!(create_timeout(0, "test").is_err());
        assert!(create_timeout(400_000, "test").is_err());
    }
}