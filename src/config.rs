//! Configuration management for Harbr Router.
//!
//! This module provides a unified configuration system that supports multiple formats
//! (YAML, JSON, TOML) with comprehensive validation and hot-reloading capabilities.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

/// Main configuration structure for Harbr Router.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Global configuration settings
    pub global: GlobalConfig,
    /// Proxy instance configurations
    pub proxy_instances: Vec<ProxyInstanceConfig>,
}

/// Global configuration settings that apply to all proxy instances.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalConfig {
    /// Metrics configuration
    pub metrics: MetricsConfig,
    /// Health check configuration
    pub health_check: HealthCheckConfig,
    /// Performance settings
    pub performance: PerformanceConfig,
    /// Logging configuration
    pub logging: LoggingConfig,
}

/// Metrics collection and export configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    /// Enable metrics collection
    pub enabled: bool,
    /// Metrics server listen address
    pub listen_addr: SocketAddr,
    /// Metrics endpoint path
    #[serde(default = "default_metrics_path")]
    pub path: String,
    /// Metrics collection interval in seconds
    #[serde(default = "default_metrics_interval")]
    pub interval_secs: u64,
}

/// Health check endpoint configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckConfig {
    /// Enable health check endpoint
    pub enabled: bool,
    /// Health check server listen address
    pub listen_addr: SocketAddr,
    /// Health check endpoint path
    #[serde(default = "default_health_path")]
    pub path: String,
}

/// Performance tuning configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceConfig {
    /// Number of worker threads (0 = auto-detect)
    #[serde(default)]
    pub worker_threads: usize,
    /// Enable CPU affinity binding
    #[serde(default = "default_true")]
    pub cpu_affinity: bool,
    /// Buffer pool configuration
    pub buffer_pool: BufferPoolConfig,
    /// Connection pool configuration
    pub connection_pool: ConnectionPoolConfig,
}

/// Buffer pool configuration for zero-copy operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferPoolConfig {
    /// Initial buffer pool size per size class
    #[serde(default = "default_buffer_pool_size")]
    pub initial_size: usize,
    /// Maximum buffer pool size per size class
    #[serde(default = "default_buffer_pool_max_size")]
    pub max_size: usize,
    /// Enable buffer pool statistics
    #[serde(default)]
    pub enable_stats: bool,
}

/// Connection pool configuration for upstream connections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionPoolConfig {
    /// Maximum connections per upstream
    #[serde(default = "default_max_connections")]
    pub max_connections_per_upstream: usize,
    /// Connection idle timeout in seconds
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_secs: u64,
    /// Connection keep-alive timeout in seconds
    #[serde(default = "default_keepalive_timeout")]
    pub keepalive_timeout_secs: u64,
}

/// Logging configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Log level (error, warn, info, debug, trace)
    #[serde(default = "default_log_level")]
    pub level: String,
    /// Log format (json, pretty, compact)
    #[serde(default = "default_log_format")]
    pub format: String,
    /// Enable request tracing
    #[serde(default)]
    pub enable_tracing: bool,
}

/// Configuration for a single proxy instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyInstanceConfig {
    /// Unique instance name
    pub name: String,
    /// Proxy type (http, tcp, udp)
    pub proxy_type: ProxyType,
    /// Listen address for incoming connections
    pub listen_addr: SocketAddr,
    /// Upstream host configurations
    pub upstreams: Vec<UpstreamConfig>,
    /// Proxy-specific configuration
    #[serde(default)]
    pub config: ProxySpecificConfig,
}

/// Supported proxy types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyType {
    Http,
    Tcp,
    Udp,
}

/// Configuration for an upstream server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamConfig {
    /// Unique upstream name/identifier
    pub name: String,
    /// Upstream server address
    pub address: String,
    /// Load balancing weight (default: 1)
    #[serde(default = "default_weight")]
    pub weight: u32,
    /// Route priority (higher = preferred)
    #[serde(default)]
    pub priority: i32,
    /// Request timeout in milliseconds
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Retry configuration
    #[serde(default)]
    pub retry: RetryConfig,
    /// Health check configuration
    #[serde(default)]
    pub health_check: Option<UpstreamHealthCheckConfig>,
    /// TLS configuration for upstream connection
    #[serde(default)]
    pub tls: Option<TlsConfig>,
}

/// Retry configuration for upstream requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    /// Maximum number of retry attempts
    #[serde(default)]
    pub max_attempts: u32,
    /// Retry backoff strategy
    #[serde(default)]
    pub backoff: BackoffStrategy,
}

/// Backoff strategies for retries.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackoffStrategy {
    /// Fixed delay between retries
    Fixed { delay_ms: u64 },
    /// Exponential backoff with optional jitter
    Exponential { base_delay_ms: u64, max_delay_ms: u64, jitter: bool },
    /// Linear backoff
    Linear { initial_delay_ms: u64, increment_ms: u64 },
}

/// Health check configuration for upstreams.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamHealthCheckConfig {
    /// Health check interval in seconds
    #[serde(default = "default_health_interval")]
    pub interval_secs: u64,
    /// Health check timeout in seconds
    #[serde(default = "default_health_timeout")]
    pub timeout_secs: u64,
    /// Number of consecutive failures before marking unhealthy
    #[serde(default = "default_failure_threshold")]
    pub failure_threshold: u32,
    /// Number of consecutive successes before marking healthy
    #[serde(default = "default_success_threshold")]
    pub success_threshold: u32,
    /// Protocol-specific health check configuration
    #[serde(flatten)]
    pub protocol_config: HealthCheckProtocolConfig,
}

/// Protocol-specific health check configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum HealthCheckProtocolConfig {
    /// HTTP health check
    Http {
        /// HTTP path to check
        #[serde(default = "default_health_path")]
        path: String,
        /// Expected HTTP status codes
        #[serde(default = "default_expected_status")]
        expected_status: Vec<u16>,
        /// Request headers
        #[serde(default)]
        headers: HashMap<String, String>,
    },
    /// TCP health check
    Tcp,
    /// UDP health check (echo test)
    Udp,
}

/// TLS configuration for upstream connections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsConfig {
    /// Enable TLS
    pub enabled: bool,
    /// Skip certificate verification (dangerous!)
    #[serde(default)]
    pub insecure_skip_verify: bool,
    /// CA certificate file path
    pub ca_cert_file: Option<String>,
    /// Client certificate file path
    pub client_cert_file: Option<String>,
    /// Client private key file path
    pub client_key_file: Option<String>,
    /// SNI server name
    pub server_name: Option<String>,
}

/// Proxy-specific configuration options.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProxySpecificConfig {
    /// HTTP-specific configuration
    #[serde(default)]
    pub http: HttpProxyConfig,
    /// TCP-specific configuration
    #[serde(default)]
    pub tcp: TcpProxyConfig,
    /// UDP-specific configuration
    #[serde(default)]
    pub udp: UdpProxyConfig,
}

/// HTTP proxy specific configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpProxyConfig {
    /// Enable HTTP/2 support
    #[serde(default = "default_true")]
    pub http2_enabled: bool,
    /// Enable request/response compression
    #[serde(default = "default_true")]
    pub compression_enabled: bool,
    /// Maximum request body size in bytes
    #[serde(default = "default_max_request_size")]
    pub max_request_size: usize,
    /// Request timeout in seconds
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,
    /// Headers to add to all requests
    #[serde(default)]
    pub add_headers: HashMap<String, String>,
    /// Headers to remove from all requests
    #[serde(default)]
    pub remove_headers: Vec<String>,
    /// Enable sticky sessions
    #[serde(default)]
    pub sticky_sessions: bool,
}

/// TCP proxy specific configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpProxyConfig {
    /// Enable connection pooling
    #[serde(default = "default_true")]
    pub connection_pooling: bool,
    /// Buffer size for data transfer
    #[serde(default = "default_tcp_buffer_size")]
    pub buffer_size: usize,
    /// TCP keep-alive settings
    #[serde(default)]
    pub keep_alive: Option<TcpKeepAliveConfig>,
    /// Enable Nagle's algorithm
    #[serde(default)]
    pub nodelay: bool,
}

/// TCP keep-alive configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpKeepAliveConfig {
    /// Enable TCP keep-alive
    pub enabled: bool,
    /// Time before first keep-alive probe (seconds)
    pub idle_secs: u64,
    /// Interval between keep-alive probes (seconds)
    pub interval_secs: u64,
    /// Number of probes before declaring connection dead
    pub retries: u32,
}

/// UDP proxy specific configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdpProxyConfig {
    /// Buffer size for UDP packets
    #[serde(default = "default_udp_buffer_size")]
    pub buffer_size: usize,
    /// Session timeout in seconds
    #[serde(default = "default_udp_timeout")]
    pub session_timeout_secs: u64,
    /// Enable connection tracking
    #[serde(default = "default_true")]
    pub connection_tracking: bool,
}

// Default value functions
fn default_metrics_path() -> String {
    "/metrics".to_string()
}

fn default_metrics_interval() -> u64 {
    60
}

fn default_health_path() -> String {
    "/health".to_string()
}

fn default_true() -> bool {
    true
}

fn default_buffer_pool_size() -> usize {
    1024
}

fn default_buffer_pool_max_size() -> usize {
    4096
}

fn default_max_connections() -> usize {
    100
}

fn default_idle_timeout() -> u64 {
    300
}

fn default_keepalive_timeout() -> u64 {
    60
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_log_format() -> String {
    "pretty".to_string()
}

fn default_weight() -> u32 {
    1
}

fn default_timeout_ms() -> u64 {
    30000
}

fn default_health_interval() -> u64 {
    30
}

fn default_health_timeout() -> u64 {
    5
}

fn default_failure_threshold() -> u32 {
    3
}

fn default_success_threshold() -> u32 {
    2
}

fn default_expected_status() -> Vec<u16> {
    vec![200]
}

fn default_max_request_size() -> usize {
    64 * 1024 * 1024 // 64MB
}

fn default_request_timeout() -> u64 {
    60
}

fn default_tcp_buffer_size() -> usize {
    8192
}

fn default_udp_buffer_size() -> usize {
    1500
}

fn default_udp_timeout() -> u64 {
    30
}

// Implementation defaults
impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            metrics: MetricsConfig::default(),
            health_check: HealthCheckConfig::default(),
            performance: PerformanceConfig::default(),
            logging: LoggingConfig::default(),
        }
    }
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen_addr: "0.0.0.0:9090".parse().unwrap(),
            path: default_metrics_path(),
            interval_secs: default_metrics_interval(),
        }
    }
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen_addr: "0.0.0.0:8080".parse().unwrap(),
            path: default_health_path(),
        }
    }
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            worker_threads: 0,
            cpu_affinity: true,
            buffer_pool: BufferPoolConfig::default(),
            connection_pool: ConnectionPoolConfig::default(),
        }
    }
}

impl Default for BufferPoolConfig {
    fn default() -> Self {
        Self {
            initial_size: default_buffer_pool_size(),
            max_size: default_buffer_pool_max_size(),
            enable_stats: false,
        }
    }
}

impl Default for ConnectionPoolConfig {
    fn default() -> Self {
        Self {
            max_connections_per_upstream: default_max_connections(),
            idle_timeout_secs: default_idle_timeout(),
            keepalive_timeout_secs: default_keepalive_timeout(),
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            format: default_log_format(),
            enable_tracing: false,
        }
    }
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 0,
            backoff: BackoffStrategy::Fixed { delay_ms: 1000 },
        }
    }
}

impl Default for BackoffStrategy {
    fn default() -> Self {
        Self::Fixed { delay_ms: 1000 }
    }
}

impl Default for HttpProxyConfig {
    fn default() -> Self {
        Self {
            http2_enabled: true,
            compression_enabled: true,
            max_request_size: default_max_request_size(),
            request_timeout_secs: default_request_timeout(),
            add_headers: HashMap::new(),
            remove_headers: Vec::new(),
            sticky_sessions: false,
        }
    }
}

impl Default for TcpProxyConfig {
    fn default() -> Self {
        Self {
            connection_pooling: true,
            buffer_size: default_tcp_buffer_size(),
            keep_alive: None,
            nodelay: false,
        }
    }
}

impl Default for UdpProxyConfig {
    fn default() -> Self {
        Self {
            buffer_size: default_udp_buffer_size(),
            session_timeout_secs: default_udp_timeout(),
            connection_tracking: true,
        }
    }
}

impl Config {
    /// Load configuration from a file.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .map_err(|e| Error::config(format!("Failed to read config file '{}': {}", path.display(), e)))?;

        let config = match path.extension().and_then(|s| s.to_str()) {
            Some("yml") | Some("yaml") => serde_yaml::from_str(&content)?,
            Some("json") => serde_json::from_str(&content)?,
            Some("toml") => toml::from_str(&content)?,
            _ => return Err(Error::config("Unsupported config file format. Use .yml, .json, or .toml")),
        };

        let mut config: Config = config;
        config.validate()?;
        Ok(config)
    }

    /// Validate the configuration for correctness and consistency.
    pub fn validate(&mut self) -> Result<()> {
        // Validate unique instance names
        let mut instance_names = HashSet::new();
        for instance in &self.proxy_instances {
            if !instance_names.insert(&instance.name) {
                return Err(Error::config(format!("Duplicate proxy instance name: '{}'", instance.name)));
            }
        }

        // Validate unique listen addresses
        let mut listen_addrs = HashSet::new();
        for instance in &self.proxy_instances {
            if !listen_addrs.insert(instance.listen_addr) {
                return Err(Error::config(format!(
                    "Duplicate listen address: {} (instance: {})",
                    instance.listen_addr, instance.name
                )));
            }
        }

        // Validate each proxy instance
        for instance in &mut self.proxy_instances {
            instance.validate()?;
        }

        // Validate global configuration
        self.global.validate()?;

        Ok(())
    }

    /// Get proxy instance by name.
    pub fn get_instance(&self, name: &str) -> Option<&ProxyInstanceConfig> {
        self.proxy_instances.iter().find(|i| i.name == name)
    }

    /// Get all instances of a specific proxy type.
    pub fn instances_by_type(&self, proxy_type: ProxyType) -> Vec<&ProxyInstanceConfig> {
        self.proxy_instances
            .iter()
            .filter(|i| std::mem::discriminant(&i.proxy_type) == std::mem::discriminant(&proxy_type))
            .collect()
    }
}

impl ProxyInstanceConfig {
    /// Validate this proxy instance configuration.
    pub fn validate(&mut self) -> Result<()> {
        // Validate instance name
        if self.name.is_empty() {
            return Err(Error::config("Proxy instance name cannot be empty"));
        }

        // Validate upstreams
        if self.upstreams.is_empty() {
            return Err(Error::config(format!(
                "Proxy instance '{}' must have at least one upstream",
                self.name
            )));
        }

        // Validate unique upstream names
        let mut upstream_names = HashSet::new();
        for upstream in &self.upstreams {
            if !upstream_names.insert(&upstream.name) {
                return Err(Error::config(format!(
                    "Duplicate upstream name '{}' in instance '{}'",
                    upstream.name, self.name
                )));
            }
        }

        // Validate each upstream
        for upstream in &mut self.upstreams {
            upstream.validate()?;
        }

        Ok(())
    }
}

impl UpstreamConfig {
    /// Validate this upstream configuration.
    pub fn validate(&mut self) -> Result<()> {
        // Validate upstream name
        if self.name.is_empty() {
            return Err(Error::config("Upstream name cannot be empty"));
        }

        // Validate address format
        if self.address.is_empty() {
            return Err(Error::config(format!("Upstream '{}' address cannot be empty", self.name)));
        }

        // Basic address validation (more thorough validation happens at runtime)
        if !self.address.contains(':') {
            return Err(Error::config(format!(
                "Upstream '{}' address must include port (e.g., 'host:port')",
                self.name
            )));
        }

        // Validate weight
        if self.weight == 0 {
            return Err(Error::config(format!(
                "Upstream '{}' weight must be greater than 0",
                self.name
            )));
        }

        // Validate timeout
        if self.timeout_ms == 0 {
            return Err(Error::config(format!(
                "Upstream '{}' timeout must be greater than 0",
                self.name
            )));
        }

        Ok(())
    }
}

impl GlobalConfig {
    /// Validate global configuration.
    pub fn validate(&self) -> Result<()> {
        // Validate metrics configuration
        if self.metrics.enabled && self.metrics.interval_secs == 0 {
            return Err(Error::config("Metrics interval must be greater than 0"));
        }

        // Validate performance configuration
        if self.performance.buffer_pool.initial_size > self.performance.buffer_pool.max_size {
            return Err(Error::config("Buffer pool initial size cannot be greater than max size"));
        }

        // Validate logging level
        match self.logging.level.as_str() {
            "error" | "warn" | "info" | "debug" | "trace" => {}
            _ => return Err(Error::config("Invalid log level. Must be one of: error, warn, info, debug, trace")),
        }

        // Validate logging format
        match self.logging.format.as_str() {
            "json" | "pretty" | "compact" => {}
            _ => return Err(Error::config("Invalid log format. Must be one of: json, pretty, compact")),
        }

        Ok(())
    }
}