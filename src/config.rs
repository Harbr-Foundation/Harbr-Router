// src/config.rs - JSON configuration structure
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use anyhow::{Context, Result};

/// Main router configuration - now JSON-based with plugins
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterConfig {
    pub version: String,
    pub server: ServerConfig,
    pub plugins: PluginSystemConfig,
    pub proxies: Vec<ProxyInstanceConfig>,
    pub domains: HashMap<String, DomainConfig>,
    pub load_balancing: HashMap<String, LoadBalancingConfig>,
    pub middleware: Vec<MiddlewareConfig>,
    pub monitoring: MonitoringConfig,
    pub security: SecurityConfig,
    pub logging: LoggingConfig,
    
    // Legacy compatibility - these will be converted to plugin instances
    #[serde(default)]
    pub legacy_routes: HashMap<String, LegacyRouteConfig>,
    #[serde(default)]
    pub legacy_tcp_proxy: Option<LegacyTcpProxyConfig>,
}

impl RouterConfig {
    /// Load configuration from JSON file
    pub async fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = tokio::fs::read_to_string(path.as_ref()).await
            .with_context(|| format!("Failed to read config file: {}", path.as_ref().display()))?;
        
        let mut config: RouterConfig = serde_json::from_str(&content)
            .with_context(|| "Failed to parse JSON configuration")?;
        
        // Convert legacy configuration to plugin instances
        config.convert_legacy_config().await?;
        
        // Validate configuration
        config.validate().await?;
        
        Ok(config)
    }
    
    /// Save configuration to JSON file
    pub async fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let json = serde_json::to_string_pretty(self)
            .context("Failed to serialize configuration")?;
        
        tokio::fs::write(path.as_ref(), json).await
            .with_context(|| format!("Failed to write config file: {}", path.as_ref().display()))?;
        
        Ok(())
    }
    
    /// Convert legacy YAML-style configuration to plugin instances
    async fn convert_legacy_config(&mut self) -> Result<()> {
        // Convert legacy HTTP routes to HTTP plugin instances
        if !self.legacy_routes.is_empty() {
            let http_plugin = ProxyInstanceConfig {
                name: "legacy_http_proxy".to_string(),
                plugin_type: "http_proxy".to_string(),
                enabled: true,
                priority: 0,
                ports: vec![8080], // Default port
                bind_addresses: vec!["0.0.0.0".to_string()],
                domains: self.legacy_routes.keys().cloned().collect(),
                plugin_config: serde_json::to_value(&self.legacy_routes)?,
                middleware: vec![],
                load_balancing: None,
                health_check: Some(HealthCheckConfig {
                    enabled: true,
                    interval_seconds: 30,
                    timeout_seconds: 10,
                    path: Some("/health".to_string()),
                    expected_status: Some(200),
                    custom_check: None,
                }),
                circuit_breaker: None,
                rate_limiting: None,
                ssl_config: None,
            };
            
            self.proxies.push(http_plugin);
            self.legacy_routes.clear();
        }
        
        // Convert legacy TCP proxy to TCP plugin instance
        if let Some(tcp_config) = &self.legacy_tcp_proxy {
            let tcp_plugin = ProxyInstanceConfig {
                name: "legacy_tcp_proxy".to_string(),
                plugin_type: "tcp_proxy".to_string(),
                enabled: tcp_config.enabled,
                priority: 100,
                ports: vec![tcp_config.listen_port],
                bind_addresses: vec![tcp_config.listen_addr.clone()],
                domains: vec![],
                plugin_config: serde_json::to_value(tcp_config)?,
                middleware: vec![],
                load_balancing: None,
                health_check: Some(HealthCheckConfig {
                    enabled: true,
                    interval_seconds: 60,
                    timeout_seconds: 5,
                    path: None,
                    expected_status: None,
                    custom_check: Some(serde_json::json!({
                        "type": "tcp_connect"
                    })),
                }),
                circuit_breaker: None,
                rate_limiting: None,
                ssl_config: None,
            };
            
            self.proxies.push(tcp_plugin);
            self.legacy_tcp_proxy = None;
        }
        
        Ok(())
    }
    
    /// Validate the configuration
    async fn validate(&self) -> Result<()> {
        // Validate server config
        if self.server.listen_addresses.is_empty() {
            return Err(anyhow::anyhow!("At least one listen address must be specified"));
        }
        
        // Validate proxy instances
        let mut used_ports = std::collections::HashSet::new();
        for proxy in &self.proxies {
            for port in &proxy.ports {
                if used_ports.contains(port) {
                    return Err(anyhow::anyhow!("Port {} is used by multiple proxies", port));
                }
                used_ports.insert(*port);
            }
        }
        
        // Validate domain mappings
        for (domain, config) in &self.domains {
            if !self.proxies.iter().any(|p| p.domains.contains(domain)) {
                tracing::warn!("Domain '{}' is not handled by any proxy", domain);
            }
        }
        
        Ok(())
    }
    
    /// Create a default configuration
    pub fn default() -> Self {
        Self {
            version: "2.0.0".to_string(),
            server: ServerConfig::default(),
            plugins: PluginSystemConfig::default(),
            proxies: vec![],
            domains: HashMap::new(),
            load_balancing: HashMap::new(),
            middleware: vec![],
            monitoring: MonitoringConfig::default(),
            security: SecurityConfig::default(),
            logging: LoggingConfig::default(),
            legacy_routes: HashMap::new(),
            legacy_tcp_proxy: None,
        }
    }
}

/// Server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub listen_addresses: Vec<String>,
    pub management_port: u16,
    pub health_check_port: u16,
    pub worker_threads: Option<usize>,
    pub max_connections: usize,
    pub connection_timeout_seconds: u64,
    pub graceful_shutdown_timeout_seconds: u64,
    pub enable_http2: bool,
    pub enable_websockets: bool,
    pub request_id_header: String,
    pub server_tokens: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen_addresses: vec!["0.0.0.0:8080".to_string()],
            management_port: 8081,
            health_check_port: 8082,
            worker_threads: None,
            max_connections: 10000,
            connection_timeout_seconds: 30,
            graceful_shutdown_timeout_seconds: 30,
            enable_http2: true,
            enable_websockets: true,
            request_id_header: "X-Request-ID".to_string(),
            server_tokens: false,
        }
    }
}

/// Plugin system configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSystemConfig {
    pub plugin_directories: Vec<String>,
    pub auto_reload: bool,
    pub reload_interval_seconds: u64,
    pub max_plugin_memory_mb: usize,
    pub plugin_timeout_seconds: u64,
    pub enable_plugin_isolation: bool,
    pub allowed_plugins: Option<Vec<String>>,
    pub blocked_plugins: Vec<String>,
    pub require_signature: bool,
    pub signature_key_path: Option<String>,
    pub max_concurrent_loads: usize,
    pub health_check_interval_seconds: u64,
    pub metrics_collection_interval_seconds: u64,
    pub enable_inter_plugin_communication: bool,
}

impl Default for PluginSystemConfig {
    fn default() -> Self {
        Self {
            plugin_directories: vec!["./plugins".to_string()],
            auto_reload: false,
            reload_interval_seconds: 60,
            max_plugin_memory_mb: 100,
            plugin_timeout_seconds: 30,
            enable_plugin_isolation: true,
            allowed_plugins: None,
            blocked_plugins: vec![],
            require_signature: false,
            signature_key_path: None,
            max_concurrent_loads: 10,
            health_check_interval_seconds: 30,
            metrics_collection_interval_seconds: 60,
            enable_inter_plugin_communication: true,
        }
    }
}

/// Proxy instance configuration - represents a loaded plugin instance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyInstanceConfig {
    pub name: String,
    pub plugin_type: String, // Which plugin to use
    pub enabled: bool,
    pub priority: i32,
    pub ports: Vec<u16>,
    pub bind_addresses: Vec<String>,
    pub domains: Vec<String>,
    pub plugin_config: serde_json::Value, // Plugin-specific configuration
    pub middleware: Vec<String>,
    pub load_balancing: Option<String>, // Reference to load_balancing config
    pub health_check: Option<HealthCheckConfig>,
    pub circuit_breaker: Option<CircuitBreakerConfig>,
    pub rate_limiting: Option<RateLimitingConfig>,
    pub ssl_config: Option<SslConfig>,
}

/// Domain configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainConfig {
    pub proxy_instance: String, // Which proxy instance handles this domain
    pub backend_service: Option<String>, // For plugin configuration
    pub ssl_config: Option<SslConfig>,
    pub cors_config: Option<CorsConfig>,
    pub cache_config: Option<CacheConfig>,
    pub custom_headers: HashMap<String, String>,
    pub rewrite_rules: Vec<RewriteRule>,
    pub access_control: Option<AccessControlConfig>,
}

/// Load balancing configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadBalancingConfig {
    pub strategy: String, // round_robin, weighted, least_connections, ip_hash, custom
    pub backends: Vec<BackendConfig>,
    pub session_affinity: bool,
    pub health_check: HealthCheckConfig,
    pub failover: FailoverConfig,
    pub custom_config: serde_json::Value, // Plugin-specific LB config
}

/// Backend server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendConfig {
    pub address: String,
    pub weight: Option<u32>,
    pub max_connections: Option<u32>,
    pub backup: bool,
    pub metadata: HashMap<String, String>,
    pub ssl_config: Option<SslConfig>,
    pub health_check_override: Option<HealthCheckConfig>,
}

/// Health check configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckConfig {
    pub enabled: bool,
    pub interval_seconds: u64,
    pub timeout_seconds: u64,
    pub path: Option<String>, // For HTTP health checks
    pub expected_status: Option<u16>,
    pub custom_check: Option<serde_json::Value>, // Plugin-specific health check
}

/// Circuit breaker configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerConfig {
    pub enabled: bool,
    pub failure_threshold: u32,
    pub success_threshold: u32,
    pub timeout_seconds: u64,
    pub half_open_max_calls: u32,
    pub metrics_window_seconds: u64,
}

/// Rate limiting configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitingConfig {
    pub enabled: bool,
    pub requests_per_second: u32,
    pub burst_size: u32,
    pub per_ip: bool,
    pub per_domain: bool,
    pub custom_key: Option<String>, // Custom rate limiting key
    pub whitelist: Vec<String>,
    pub blacklist: Vec<String>,
}

/// SSL/TLS configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SslConfig {
    pub enabled: bool,
    pub cert_path: String,
    pub key_path: String,
    pub ca_path: Option<String>,
    pub protocols: Vec<String>,
    pub ciphers: Vec<String>,
    pub client_cert_required: bool,
    pub verify_client: bool,
    pub sni_callback: Option<String>, // Plugin callback for SNI
}

/// CORS configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorsConfig {
    pub enabled: bool,
    pub allowed_origins: Vec<String>,
    pub allowed_methods: Vec<String>,
    pub allowed_headers: Vec<String>,
    pub exposed_headers: Vec<String>,
    pub max_age_seconds: u64,
    pub allow_credentials: bool,
}

/// Cache configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    pub enabled: bool,
    pub ttl_seconds: u64,
    pub max_size_mb: usize,
    pub cache_key_headers: Vec<String>,
    pub vary_headers: Vec<String>,
    pub bypass_headers: Vec<String>,
    pub custom_cache_logic: Option<String>, // Plugin callback
}

/// URL rewrite rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewriteRule {
    pub pattern: String,
    pub replacement: String,
    pub flags: Vec<String>, // redirect, last, etc.
    pub conditions: Vec<RewriteCondition>,
}

/// Rewrite condition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewriteCondition {
    pub test_string: String,
    pub condition_pattern: String,
    pub flags: Vec<String>,
}

/// Access control configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessControlConfig {
    pub allowed_ips: Vec<String>,
    pub blocked_ips: Vec<String>,
    pub allowed_countries: Vec<String>,
    pub blocked_countries: Vec<String>,
    pub custom_rules: Vec<AccessRule>,
}

/// Access rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessRule {
    pub name: String,
    pub condition: String, // Expression to evaluate
    pub action: String,    // allow, deny, redirect
    pub value: Option<String>, // For redirect actions
}

/// Failover configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailoverConfig {
    pub enabled: bool,
    pub max_failures: u32,
    pub retry_interval_seconds: u64,
    pub fallback_backend: Option<String>,
    pub custom_failover_logic: Option<String>, // Plugin callback
}

/// Middleware configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiddlewareConfig {
    pub name: String,
    pub plugin_type: String, // Which plugin provides this middleware
    pub enabled: bool,
    pub order: u32,
    pub config: serde_json::Value,
    pub apply_to: Vec<String>, // Which proxy instances to apply to
}

/// Monitoring configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitoringConfig {
    pub metrics_enabled: bool,
    pub metrics_port: u16,
    pub metrics_path: String,
    pub prometheus_enabled: bool,
    pub custom_metrics: Vec<CustomMetricConfig>,
    pub tracing_enabled: bool,
    pub tracing_endpoint: Option<String>,
    pub log_sampling_rate: f64,
    pub alert_rules: Vec<AlertRule>,
}

impl Default for MonitoringConfig {
    fn default() -> Self {
        Self {
            metrics_enabled: true,
            metrics_port: 9090,
            metrics_path: "/metrics".to_string(),
            prometheus_enabled: true,
            custom_metrics: vec![],
            tracing_enabled: false,
            tracing_endpoint: None,
            log_sampling_rate: 1.0,
            alert_rules: vec![],
        }
    }
}

/// Custom metric configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomMetricConfig {
    pub name: String,
    pub metric_type: String, // counter, gauge, histogram
    pub description: String,
    pub labels: Vec<String>,
    pub plugin_callback: Option<String>,
}

/// Alert rule configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertRule {
    pub name: String,
    pub condition: String, // PromQL-like expression
    pub threshold: f64,
    pub duration_seconds: u64,
    pub severity: String,
    pub action: AlertAction,
}

/// Alert action
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertAction {
    pub action_type: String, // webhook, email, plugin
    pub endpoint: Option<String>,
    pub config: serde_json::Value,
}

/// Security configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    pub request_size_limit_mb: usize,
    pub header_size_limit_kb: usize,
    pub timeout_seconds: u64,
    pub enable_request_logging: bool,
    pub sensitive_headers: Vec<String>,
    pub security_headers: HashMap<String, String>,
    pub waf_enabled: bool,
    pub waf_rules: Vec<WafRule>,
    pub ddos_protection: DdosProtectionConfig,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            request_size_limit_mb: 100,
            header_size_limit_kb: 64,
            timeout_seconds: 30,
            enable_request_logging: true,
            sensitive_headers: vec![
                "Authorization".to_string(),
                "Cookie".to_string(),
                "X-API-Key".to_string(),
            ],
            security_headers: [
                ("X-Frame-Options".to_string(), "DENY".to_string()),
                ("X-Content-Type-Options".to_string(), "nosniff".to_string()),
                ("X-XSS-Protection".to_string(), "1; mode=block".to_string()),
            ].iter().cloned().collect(),
            waf_enabled: false,
            waf_rules: vec![],
            ddos_protection: DdosProtectionConfig::default(),
        }
    }
}

/// WAF rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WafRule {
    pub name: String,
    pub pattern: String,
    pub action: String, // block, log, redirect
    pub severity: String,
    pub enabled: bool,
}

/// DDoS protection configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DdosProtectionConfig {
    pub enabled: bool,
    pub requests_per_second_per_ip: u32,
    pub burst_size_per_ip: u32,
    pub ban_duration_seconds: u64,
    pub whitelist: Vec<String>,
    pub custom_logic: Option<String>, // Plugin callback
}

impl Default for DdosProtectionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            requests_per_second_per_ip: 100,
            burst_size_per_ip: 200,
            ban_duration_seconds: 300,
            whitelist: vec![],
            custom_logic: None,
        }
    }
}

/// Logging configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub level: String,
    pub format: String, // json, text
    pub output: Vec<LogOutput>,
    pub access_log: AccessLogConfig,
    pub error_log: ErrorLogConfig,
    pub audit_log: Option<AuditLogConfig>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: "json".to_string(),
            output: vec![LogOutput {
                output_type: "stdout".to_string(),
                config: serde_json::json!({}),
            }],
            access_log: AccessLogConfig::default(),
            error_log: ErrorLogConfig::default(),
            audit_log: None,
        }
    }
}

/// Log output configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogOutput {
    pub output_type: String, // stdout, file, syslog, plugin
    pub config: serde_json::Value,
}

/// Access log configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessLogConfig {
    pub enabled: bool,
    pub format: String,
    pub output: Vec<LogOutput>,
    pub fields: Vec<String>,
    pub exclude_paths: Vec<String>,
}

impl Default for AccessLogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            format: "combined".to_string(),
            output: vec![LogOutput {
                output_type: "file".to_string(),
                config: serde_json::json!({
                    "path": "./logs/access.log",
                    "rotation": "daily"
                }),
            }],
            fields: vec![
                "timestamp".to_string(),
                "client_ip".to_string(),
                "method".to_string(),
                "path".to_string(),
                "status".to_string(),
                "response_time".to_string(),
            ],
            exclude_paths: vec!["/health".to_string(), "/metrics".to_string()],
        }
    }
}

/// Error log configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorLogConfig {
    pub enabled: bool,
    pub output: Vec<LogOutput>,
    pub include_stack_trace: bool,
    pub min_level: String,
}

impl Default for ErrorLogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            output: vec![LogOutput {
                output_type: "file".to_string(),
                config: serde_json::json!({
                    "path": "./logs/error.log",
                    "rotation": "daily"
                }),
            }],
            include_stack_trace: true,
            min_level: "warn".to_string(),
        }
    }
}

/// Audit log configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLogConfig {
    pub enabled: bool,
    pub output: Vec<LogOutput>,
    pub events: Vec<String>, // Which events to audit
    pub include_request_body: bool,
    pub include_response_body: bool,
}

// Legacy configuration types for backward compatibility
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacyRouteConfig {
    pub upstream: String,
    pub timeout_ms: Option<u64>,
    pub retry_count: Option<u32>,
    pub priority: Option<i32>,
    pub preserve_host_header: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacyTcpProxyConfig {
    pub enabled: bool,
    pub listen_addr: String,
    pub listen_port: u16,
    pub connection_pooling: bool,
    pub max_idle_time_secs: u64,
    pub udp_enabled: bool,
    pub udp_listen_addr: String,
}

/// Configuration utilities
impl RouterConfig {
    /// Get proxy instance by name
    pub fn get_proxy_instance(&self, name: &str) -> Option<&ProxyInstanceConfig> {
        self.proxies.iter().find(|p| p.name == name)
    }
    
    /// Get domain configuration
    pub fn get_domain_config(&self, domain: &str) -> Option<&DomainConfig> {
        self.domains.get(domain)
    }
    
    /// Get load balancing configuration
    pub fn get_load_balancing_config(&self, name: &str) -> Option<&LoadBalancingConfig> {
        self.load_balancing.get(name)
    }
    
    /// Find proxy instances handling a domain
    pub fn find_proxies_for_domain(&self, domain: &str) -> Vec<&ProxyInstanceConfig> {
        self.proxies.iter()
            .filter(|p| p.domains.iter().any(|d| self.domain_matches(d, domain)))
            .collect()
    }
    
    /// Check if domain pattern matches
    fn domain_matches(&self, pattern: &str, domain: &str) -> bool {
        if pattern.starts_with("*.") {
            let suffix = &pattern[2..];
            domain.ends_with(suffix)
        } else {
            pattern == domain
        }
    }
    
    /// Merge with another configuration (for updates)
    pub fn merge(&mut self, other: RouterConfig) -> Result<()> {
        // Merge server config
        self.server = other.server;
        
        // Merge plugin system config
        self.plugins = other.plugins;
        
        // Replace proxy instances
        self.proxies = other.proxies;
        
        // Merge domains
        self.domains.extend(other.domains);
        
        // Merge load balancing configs
        self.load_balancing.extend(other.load_balancing);
        
        // Replace middleware
        self.middleware = other.middleware;
        
        // Merge monitoring
        self.monitoring = other.monitoring;
        
        // Merge security
        self.security = other.security;
        
        // Merge logging
        self.logging = other.logging;
        
        Ok(())
    }
}