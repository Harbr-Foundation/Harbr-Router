// src/builtin_plugins/mod.rs - Built-in plugin implementations
use crate::plugin::*;
use crate::error::{RouterError, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use reqwest::Client;
use std::time::{Duration, Instant};
use tokio::net::{TcpStream, UdpSocket};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub mod tcp_proxy;
// pub mod http_proxy;
// pub mod udp_proxy;
// pub mod load_balancer;
// pub mod static_files;

pub use tcp_proxy::TcpProxyPlugin;
// pub use http_proxy::HttpProxyPlugin;
// pub use udp_proxy::UdpProxyPlugin;
// pub use load_balancer::LoadBalancerPlugin;
// pub use static_files::StaticFilesPlugin;

/// HTTP Proxy Plugin - handles HTTP/HTTPS requests
#[derive(Debug)]
pub struct HttpProxyPlugin {
    name: String,
    config: Option<HttpProxyConfig>,
    client: Option<Client>,
    metrics: Arc<RwLock<PluginMetrics>>,
    health: Arc<RwLock<PluginHealth>>,
    backends: Arc<RwLock<Vec<BackendInfo>>>,
    circuit_breaker: Arc<RwLock<CircuitBreakerState>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpProxyConfig {
    pub default_backend: String,
    pub timeout_ms: u64,
    pub retry_count: u32,
    pub preserve_host_header: bool,
    pub follow_redirects: bool,
    pub max_redirects: u32,
    pub buffer_size: usize,
    pub connection_pool_size: usize,
    pub connection_timeout_ms: u64,
    pub read_timeout_ms: u64,
    pub write_timeout_ms: u64,
    pub enable_compression: bool,
    pub custom_headers: HashMap<String, String>,
    pub remove_headers: Vec<String>,
    pub circuit_breaker: CircuitBreakerConfig,
    pub retry_strategy: RetryStrategy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerConfig {
    pub enabled: bool,
    pub failure_threshold: u32,
    pub recovery_timeout_ms: u64,
    pub half_open_max_calls: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RetryStrategy {
    Fixed { delay_ms: u64 },
    Exponential { base_delay_ms: u64, max_delay_ms: u64 },
    Linear { initial_delay_ms: u64, increment_ms: u64 },
}

#[derive(Debug, Clone)]
pub struct BackendInfo {
    pub url: String,
    pub healthy: bool,
    pub last_check: Instant,
    pub response_time_ms: u64,
    pub error_count: u32,
}

#[derive(Debug, Clone)]
pub enum CircuitBreakerState {
    Closed,
    Open { opened_at: Instant },
    HalfOpen { calls_made: u32 },
}

impl HttpProxyPlugin {
    pub fn new() -> Self {
        Self {
            name: "http_proxy".to_string(),
            config: None,
            client: None,
            metrics: Arc::new(RwLock::new(PluginMetrics {
                connections_total: 0,
                connections_active: 0,
                bytes_sent: 0,
                bytes_received: 0,
                errors_total: 0,
                custom_metrics: HashMap::new(),
                last_updated: std::time::SystemTime::now(),
            })),
            health: Arc::new(RwLock::new(PluginHealth {
                healthy: false,
                message: "Not initialized".to_string(),
                last_check: std::time::SystemTime::now(),
                response_time_ms: None,
                custom_health_data: HashMap::new(),
            })),
            backends: Arc::new(RwLock::new(Vec::new())),
            circuit_breaker: Arc::new(RwLock::new(CircuitBreakerState::Closed)),
        }
    }
    
    async fn proxy_request(&self, metadata: RequestMetadata, body: Vec<u8>) -> Result<ProxyResponse> {
        let config = self.config.as_ref().ok_or_else(|| {
            RouterError::PluginError {
                plugin: self.name.clone(),
                message: "Plugin not initialized".to_string(),
            }
        })?;
        
        let client = self.client.as_ref().ok_or_else(|| {
            RouterError::PluginError {
                plugin: self.name.clone(),
                message: "HTTP client not initialized".to_string(),
            }
        })?;
        
        // Check circuit breaker
        if !self.is_circuit_breaker_closed().await {
            return Err(RouterError::CircuitBreakerOpen {
                service: self.name.clone(),
            });
        }
        
        let start_time = Instant::now();
        
        // Build target URL
        let target_url = if let Some(upstream) = &metadata.upstream_url {
            upstream.clone()
        } else {
            format!("{}{}", config.default_backend, metadata.path)
        };
        
        // Parse URL and add query parameters
        let mut url = reqwest::Url::parse(&target_url)
            .map_err(|e| RouterError::UpstreamError {
                message: format!("Invalid upstream URL: {}", e),
            })?;
        
        // Add query parameters
        for (key, value) in &metadata.query_params {
            url.query_pairs_mut().append_pair(key, value);
        }
        
        // Create request
        let method = reqwest::Method::from_bytes(metadata.method.as_bytes())
            .map_err(|e| RouterError::RequestError {
                message: format!("Invalid HTTP method: {}", e),
            })?;
        
        let mut request_builder = client.request(method, url)
            .timeout(Duration::from_millis(config.timeout_ms))
            .body(body);
        
        // Add headers
        for (key, value) in &metadata.headers {
            if !config.remove_headers.contains(key) {
                request_builder = request_builder.header(key, value);
            }
        }
        
        // Add custom headers
        for (key, value) in &config.custom_headers {
            request_builder = request_builder.header(key, value);
        }
        
        // Handle host header
        if !config.preserve_host_header {
            if let Some(host) = target_url.split("://").nth(1).and_then(|s| s.split('/').next()) {
                request_builder = request_builder.header("Host", host);
            }
        }
        
        // Execute request with retries
        let mut last_error = None;
        for attempt in 0..=config.retry_count {
            match request_builder.try_clone().unwrap().send().await {
                Ok(response) => {
                    let status_code = response.status().as_u16();
                    let response_headers: HashMap<String, String> = response.headers()
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                        .collect();
                    
                    let response_body = response.bytes().await
                        .map_err(|e| RouterError::UpstreamError {
                            message: format!("Failed to read response body: {}", e),
                        })?
                        .to_vec();
                    
                    // Update metrics
                    let duration = start_time.elapsed();
                    self.update_metrics(response_body.len(), duration).await;
                    
                    // Update circuit breaker on success
                    self.record_success().await;
                    
                    return Ok(ProxyResponse {
                        status_code,
                        headers: response_headers,
                        body: response_body,
                        metadata: HashMap::new(),
                        upstream_response_time_ms: Some(duration.as_millis() as u64),
                        cache_headers: None,
                    });
                }
                Err(e) => {
                    last_error = Some(e);
                    
                    if attempt < config.retry_count {
                        // Apply retry strategy
                        let delay = self.calculate_retry_delay(&config.retry_strategy, attempt).await;
                        tokio::time::sleep(delay).await;
                    }
                }
            }
        }
        
        // Record failure
        self.record_failure().await;
        
        Err(RouterError::UpstreamError {
            message: format!("All retry attempts failed: {}", 
                           last_error.unwrap_or_else(|| reqwest::Error::from(reqwest::ErrorKind::Request))),
        })
    }
    
    async fn is_circuit_breaker_closed(&self) -> bool {
        let state = self.circuit_breaker.read().await;
        match *state {
            CircuitBreakerState::Closed => true,
            CircuitBreakerState::Open { opened_at } => {
                let config = self.config.as_ref().unwrap();
                if opened_at.elapsed().as_millis() as u64 > config.circuit_breaker.recovery_timeout_ms {
                    // Transition to half-open
                    drop(state);
                    *self.circuit_breaker.write().await = CircuitBreakerState::HalfOpen { calls_made: 0 };
                    true
                } else {
                    false
                }
            }
            CircuitBreakerState::HalfOpen { calls_made } => {
                let config = self.config.as_ref().unwrap();
                calls_made < config.circuit_breaker.half_open_max_calls
            }
        }
    }
    
    async fn record_success(&self) {
        let mut state = self.circuit_breaker.write().await;
        match *state {
            CircuitBreakerState::HalfOpen { .. } => {
                *state = CircuitBreakerState::Closed;
            }
            _ => {}
        }
    }
    
    async fn record_failure(&self) {
        let config = self.config.as_ref().unwrap();
        if !config.circuit_breaker.enabled {
            return;
        }
        
        let mut state = self.circuit_breaker.write().await;
        match *state {
            CircuitBreakerState::Closed => {
                // Track failures and potentially open circuit
                // For simplicity, open immediately - in practice you'd track failure count
                *state = CircuitBreakerState::Open { opened_at: Instant::now() };
            }
            CircuitBreakerState::HalfOpen { .. } => {
                *state = CircuitBreakerState::Open { opened_at: Instant::now() };
            }
            _ => {}
        }
    }
    
    async fn calculate_retry_delay(&self, strategy: &RetryStrategy, attempt: u32) -> Duration {
        match strategy {
            RetryStrategy::Fixed { delay_ms } => Duration::from_millis(*delay_ms),
            RetryStrategy::Exponential { base_delay_ms, max_delay_ms } => {
                let delay = base_delay_ms * 2_u64.pow(attempt);
                Duration::from_millis(delay.min(*max_delay_ms))
            }
            RetryStrategy::Linear { initial_delay_ms, increment_ms } => {
                Duration::from_millis(initial_delay_ms + (increment_ms * attempt as u64))
            }
        }
    }
    
    async fn update_metrics(&self, bytes_sent: usize, duration: Duration) {
        let mut metrics = self.metrics.write().await;
        metrics.connections_total += 1;
        metrics.bytes_sent += bytes_sent as u64;
        metrics.last_updated = std::time::SystemTime::now();
        
        // Update custom metrics
        metrics.custom_metrics.insert(
            "avg_response_time_ms".to_string(),
            duration.as_millis() as f64,
        );
    }
}

#[async_trait]
impl Plugin for HttpProxyPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: self.name.clone(),
            version: "1.0.0".to_string(),
            description: "HTTP reverse proxy with load balancing and circuit breaker".to_string(),
            author: "Router Team".to_string(),
            license: "MIT".to_string(),
            repository: Some("https://github.com/router/plugins".to_string()),
            min_router_version: "2.0.0".to_string(),
            config_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "default_backend": {"type": "string"},
                    "timeout_ms": {"type": "number"},
                    "retry_count": {"type": "number"}
                },
                "required": ["default_backend"]
            })),
            dependencies: vec![],
            tags: vec!["http", "proxy", "load-balancer"].iter().map(|s| s.to_string()).collect(),
        }
    }
    
    fn capabilities(&self) -> PluginCapabilities {
        PluginCapabilities {
            can_handle_tcp: true,
            can_handle_udp: false,
            can_handle_unix_socket: false,
            requires_dedicated_port: false,
            supports_hot_reload: true,
            supports_load_balancing: true,
            custom_protocols: vec!["HTTP".to_string(), "HTTPS".to_string()],
            port_requirements: vec![],
        }
    }
    
    async fn initialize(&mut self, config: PluginConfig, _event_sender: PluginSender) -> Result<()> {
        let http_config: HttpProxyConfig = serde_json::from_value(config.config)
            .map_err(|e| RouterError::ConfigError {
                message: format!("Invalid HTTP proxy configuration: {}", e),
            })?;
        
        // Create HTTP client
        let client = Client::builder()
            .timeout(Duration::from_millis(http_config.timeout_ms))
            .redirect(if http_config.follow_redirects {
                reqwest::redirect::Policy::limited(http_config.max_redirects as usize)
            } else {
                reqwest::redirect::Policy::none()
            })
            .build()
            .map_err(|e| RouterError::PluginInitError {
                plugin: self.name.clone(),
                reason: format!("Failed to create HTTP client: {}", e),
            })?;
        
        self.config = Some(http_config);
        self.client = Some(client);
        
        // Update health status
        {
            let mut health = self.health.write().await;
            health.healthy = true;
            health.message = "HTTP proxy initialized successfully".to_string();
            health.last_check = std::time::SystemTime::now();
        }
        
        Ok(())
    }
    
    async fn start(&mut self) -> Result<()> {
        // HTTP proxy doesn't need to start listeners - it handles requests via handle_event
        Ok(())
    }
    
    async fn stop(&mut self) -> Result<()> {
        // Clean up resources
        self.client = None;
        Ok(())
    }
    
    async fn handle_event(&mut self, event: PluginEvent) -> Result<()> {
        match event {
            PluginEvent::DataReceived(packet) => {
                // Parse HTTP request and proxy it
                // This is a simplified implementation - in practice you'd need
                // a full HTTP parser and connection handler
                tracing::debug!("Received HTTP data: {} bytes", packet.data.len());
            }
            PluginEvent::ConnectionEstablished(context) => {
                tracing::debug!("HTTP connection established: {}", context.connection_id);
            }
            PluginEvent::ConnectionClosed(connection_id) => {
                tracing::debug!("HTTP connection closed: {}", connection_id);
            }
            _ => {
                // Handle other events as needed
            }
        }
        Ok(())
    }
    
    async fn health(&self) -> Result<PluginHealth> {
        let health = self.health.read().await.clone();
        Ok(health)
    }
    
    async fn metrics(&self) -> Result<PluginMetrics> {
        let metrics = self.metrics.read().await.clone();
        Ok(metrics)
    }
    
    async fn update_config(&mut self, config: PluginConfig) -> Result<()> {
        self.initialize(config, mpsc::unbounded_channel().0).await
    }
    
    async fn handle_command(&self, command: &str, args: serde_json::Value) -> Result<serde_json::Value> {
        match command {
            "get_backends" => {
                let backends = self.backends.read().await;
                Ok(serde_json::to_value(&*backends)?)
            }
            "health_check_backend" => {
                let backend_url = args.get("url").and_then(|v| v.as_str())
                    .ok_or_else(|| RouterError::InvalidApiRequest {
                        reason: "Missing 'url' parameter".to_string(),
                    })?;
                
                // Perform health check
                let healthy = self.check_backend_health(backend_url).await?;
                Ok(serde_json::json!({ "healthy": healthy }))
            }
            "circuit_breaker_status" => {
                let state = self.circuit_breaker.read().await;
                let status = match *state {
                    CircuitBreakerState::Closed => "closed",
                    CircuitBreakerState::Open { .. } => "open",
                    CircuitBreakerState::HalfOpen { .. } => "half_open",
                };
                Ok(serde_json::json!({ "status": status }))
            }
            _ => Err(RouterError::NotSupported {
                operation: format!("Command: {}", command),
            }),
        }
    }
    
    async fn shutdown(&mut self) -> Result<()> {
        self.stop().await
    }
}

impl HttpProxyPlugin {
    async fn check_backend_health(&self, backend_url: &str) -> Result<bool> {
        if let Some(client) = &self.client {
            match client.get(backend_url).timeout(Duration::from_secs(5)).send().await {
                Ok(response) => Ok(response.status().is_success()),
                Err(_) => Ok(false),
            }
        } else {
            Ok(false)
        }
    }
}

/// Plugin factory function for HTTP proxy
#[no_mangle]
pub extern "C" fn create_http_proxy_plugin() -> *mut dyn Plugin {
    Box::into_raw(Box::new(HttpProxyPlugin::new()))
}

/// Plugin info function for HTTP proxy
#[no_mangle]
pub extern "C" fn get_http_proxy_plugin_info() -> PluginInfo {
    PluginInfo {
        name: "http_proxy".to_string(),
        version: "1.0.0".to_string(),
        description: "HTTP reverse proxy with advanced features".to_string(),
        author: "Router Team".to_string(),
        license: "MIT".to_string(),
        repository: Some("https://github.com/Harbr-Foundation/Harbr-Router".to_string()),
        min_router_version: "2.0.0".to_string(),
        config_schema: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "default_backend": {"type": "string"},
                "timeout_ms": {"type": "number", "default": 30000},
                "retry_count": {"type": "number", "default": 3}
            },
            "required": ["default_backend"]
        })),
        dependencies: vec![],
        tags: vec!["http", "proxy", "reverse-proxy"].iter().map(|s| s.to_string()).collect(),
    }
}