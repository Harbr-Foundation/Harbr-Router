// src/server.rs - Main server implementation
use crate::config::RouterConfig;
use crate::plugin::manager::{PluginManager, PluginManagerConfig};
use crate::plugin::{PluginEvent, RouterEvent};
use crate::router::RequestRouter;
use crate::middleware::MiddlewareStack;
use crate::metrics::MetricsCollector;
use crate::health::HealthChecker;
use crate::management_api::ManagementApi;
use crate::error::{RouterError, Result};

use anyhow::Context;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

/// Main proxy server that coordinates all components
pub struct ProxyServer {
    config: Arc<RwLock<RouterConfig>>,
    plugin_manager: Arc<PluginManager>,
    request_router: Arc<RequestRouter>,
    middleware_stack: Arc<MiddlewareStack>,
    metrics_collector: Arc<MetricsCollector>,
    health_checker: Arc<HealthChecker>,
    management_api: Option<Arc<ManagementApi>>,
    
    // Server state
    running: Arc<RwLock<bool>>,
    start_time: Instant,
    shutdown_sender: Option<broadcast::Sender<()>>,
    event_router: Arc<EventRouter>,
    
    // Statistics
    statistics: Arc<RwLock<ServerStatistics>>,
}

#[derive(Debug, Clone, Default)]
pub struct ServerStatistics {
    pub start_time: std::time::SystemTime,
    pub uptime_seconds: u64,
    pub total_connections: u64,
    pub active_connections: u64,
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub plugin_count: usize,
    pub average_response_time_ms: f64,
    pub last_error: Option<String>,
    pub memory_usage_bytes: u64,
}

impl ProxyServer {
    /// Create a new proxy server with the given configuration
    pub async fn new(config: RouterConfig) -> Result<Self> {
        info!("Initializing proxy server...");
        
        // Initialize plugin manager
        let plugin_manager_config = PluginManagerConfig {
            loader: crate::plugin::loader::LoaderConfig {
                plugin_directories: config.plugins.plugin_directories.clone(),
                auto_reload: config.plugins.auto_reload,
                reload_interval_seconds: config.plugins.reload_interval_seconds,
                max_plugin_memory_mb: config.plugins.max_plugin_memory_mb,
                plugin_timeout_seconds: config.plugins.plugin_timeout_seconds,
                allowed_plugins: config.plugins.allowed_plugins.clone(),
                blocked_plugins: config.plugins.blocked_plugins.clone(),
                require_signature: config.plugins.require_signature,
                signature_key_path: config.plugins.signature_key_path.clone(),
            },
            enable_inter_plugin_communication: config.plugins.enable_inter_plugin_communication,
            max_concurrent_loads: config.plugins.max_concurrent_loads,
            plugin_startup_timeout_seconds: config.plugins.plugin_timeout_seconds,
            health_check_interval_seconds: config.plugins.health_check_interval_seconds,
            metrics_collection_interval_seconds: config.plugins.metrics_collection_interval_seconds,
            enable_plugin_isolation: config.plugins.enable_plugin_isolation,
            default_plugin_config: serde_json::json!({}),
        };
        
        let plugin_manager = Arc::new(PluginManager::new(plugin_manager_config));
        
        // Initialize request router
        let request_router = Arc::new(RequestRouter::new(
            config.domains.clone(),
            config.proxies.clone(),
        ));
        
        // Initialize middleware stack
        let middleware_stack = Arc::new(MiddlewareStack::new(config.middleware.clone()));
        
        // Initialize metrics collector
        let metrics_collector = Arc::new(MetricsCollector::new(
            config.monitoring.clone(),
        ));
        
        // Initialize health checker
        let health_checker = Arc::new(HealthChecker::new(
            config.server.health_check_port,
        ));
        
        // Create event router
        let event_router = Arc::new(EventRouter::new());
        
        // Create shutdown channel
        let (shutdown_tx, _) = broadcast::channel(1);
        
        let server = Self {
            config: Arc::new(RwLock::new(config)),
            plugin_manager,
            request_router,
            middleware_stack,
            metrics_collector,
            health_checker,
            management_api: None,
            running: Arc::new(RwLock::new(false)),
            start_time: Instant::now(),
            shutdown_sender: Some(shutdown_tx),
            event_router,
            statistics: Arc::new(RwLock::new(ServerStatistics {
                start_time: std::time::SystemTime::now(),
                ..Default::default()
            })),
        };
        
        info!("Proxy server initialized successfully");
        Ok(server)
    }
    
    /// Start the proxy server
    pub async fn start(&self) -> Result<()> {
        info!("Starting proxy server...");
        
        // Mark as running
        *self.running.write().await = true;
        
        // Start plugin manager
        self.plugin_manager.start().await
            .context("Failed to start plugin manager")?;
        
        // Load built-in plugins first
        self.load_builtin_plugins().await?;
        
        // Start metrics collector
        self.metrics_collector.start().await
            .context("Failed to start metrics collector")?;
        
        // Start health checker
        self.health_checker.start(self.plugin_manager.clone()).await
            .context("Failed to start health checker")?;
        
        // Start event processing
        self.start_event_processing().await?;
        
        // Start listeners for each configured address
        let config = self.config.read().await;
        let mut listeners = Vec::new();
        
        for listen_addr in &config.server.listen_addresses {
            let addr: SocketAddr = listen_addr.parse()
                .with_context(|| format!("Invalid listen address: {}", listen_addr))?;
            
            let listener = TcpListener::bind(addr).await
                .with_context(|| format!("Failed to bind to {}", addr))?;
            
            info!("Listening on {}", addr);
            listeners.push((listener, addr));
        }
        
        // Start connection handlers
        for (listener, addr) in listeners {
            let server = Arc::new(self.clone_for_handler());
            
            tokio::spawn(async move {
                if let Err(e) = server.handle_connections(listener, addr).await {
                    error!("Connection handler error on {}: {}", addr, e);
                }
            });
        }
        
        info!("Proxy server started successfully");
        Ok(())
    }
    
    /// Stop the proxy server
    pub async fn stop(&self) -> Result<()> {
        info!("Stopping proxy server...");
        
        // Mark as not running
        *self.running.write().await = false;
        
        // Send shutdown signal
        if let Some(sender) = &self.shutdown_sender {
            let _ = sender.send(());
        }
        
        // Stop components in reverse order
        if let Some(api) = &self.management_api {
            api.stop().await?;
        }
        
        self.health_checker.stop().await?;
        self.metrics_collector.stop().await?;
        self.plugin_manager.stop().await?;
        
        info!("Proxy server stopped");
        Ok(())
    }
    
    /// Handle incoming connections
    async fn handle_connections(&self, listener: TcpListener, addr: SocketAddr) -> Result<()> {
        let mut shutdown_rx = self.shutdown_sender.as_ref().unwrap().subscribe();
        
        loop {
            tokio::select! {
                // Handle new connections
                connection = listener.accept() => {
                    match connection {
                        Ok((stream, client_addr)) => {
                            debug!("Accepted connection from {} on {}", client_addr, addr);
                            
                            // Update statistics
                            {
                                let mut stats = self.statistics.write().await;
                                stats.total_connections += 1;
                                stats.active_connections += 1;
                            }
                            
                            // Spawn connection handler
                            let server = Arc::new(self.clone_for_handler());
                            tokio::spawn(async move {
                                if let Err(e) = server.handle_single_connection(stream, client_addr).await {
                                    error!("Connection error from {}: {}", client_addr, e);
                                }
                                
                                // Update statistics
                                {
                                    let mut stats = server.statistics.write().await;
                                    stats.active_connections = stats.active_connections.saturating_sub(1);
                                }
                            });
                        }
                        Err(e) => {
                            error!("Failed to accept connection on {}: {}", addr, e);
                        }
                    }
                }
                
                // Handle shutdown
                _ = shutdown_rx.recv() => {
                    info!("Shutting down listener on {}", addr);
                    break;
                }
            }
        }
        
        Ok(())
    }
    
    /// Handle a single connection
    async fn handle_single_connection(
        &self,
        stream: tokio::net::TcpStream,
        client_addr: SocketAddr,
    ) -> Result<()> {
        let start_time = Instant::now();
        
        // Create connection context
        let connection_context = crate::plugin::ConnectionContext {
            connection_id: crate::plugin::utils::generate_connection_id(),
            client_addr,
            server_addr: stream.local_addr().unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap()),
            metadata: HashMap::new(),
            created_at: std::time::SystemTime::now(),
        };
        
        // Try to determine which plugin should handle this connection
        // This is where the routing logic comes in
        
        // For now, we'll implement a simple protocol detection
        let mut buffer = [0u8; 1024];
        let mut stream = stream;
        
        // Peek at the first few bytes to determine protocol
        stream.readable().await?;
        let n = stream.try_read(&mut buffer)?;
        
        if n == 0 {
            return Ok(()); // Connection closed immediately
        }
        
        // Determine protocol and route to appropriate plugin
        let protocol_hint = self.detect_protocol(&buffer[..n])?;
        
        match protocol_hint {
            ProtocolHint::Http => {
                self.handle_http_connection(stream, client_addr, &buffer[..n]).await?;
            }
            ProtocolHint::Tcp => {
                self.handle_tcp_connection(stream, client_addr, connection_context).await?;
            }
            ProtocolHint::Unknown => {
                // Try to find a plugin that can handle unknown protocols
                self.handle_unknown_connection(stream, client_addr, &buffer[..n]).await?;
            }
        }
        
        // Update statistics
        let duration = start_time.elapsed();
        {
            let mut stats = self.statistics.write().await;
            stats.total_requests += 1;
            stats.successful_requests += 1;
            
            // Update average response time (simple moving average)
            let duration_ms = duration.as_millis() as f64;
            if stats.average_response_time_ms == 0.0 {
                stats.average_response_time_ms = duration_ms;
            } else {
                stats.average_response_time_ms = (stats.average_response_time_ms * 0.9) + (duration_ms * 0.1);
            }
        }
        
        Ok(())
    }
    
    /// Handle HTTP connection
    async fn handle_http_connection(
        &self,
        stream: tokio::net::TcpStream,
        client_addr: SocketAddr,
        initial_data: &[u8],
    ) -> Result<()> {
        // Parse HTTP request from initial data
        let request_info = self.parse_http_request(initial_data)?;
        
        // Route to appropriate plugin based on domain
        let plugins = self.request_router.find_plugins_for_request(
            &request_info.domain,
            &request_info.path,
            "HTTP",
        ).await;
        
        if plugins.is_empty() {
            warn!("No plugin found for HTTP request to {}{}", request_info.domain, request_info.path);
            return self.send_http_error(stream, 404, "Not Found").await;
        }
        
        // Use the first matching plugin
        let plugin_name = &plugins[0];
        
        // Create request metadata
        let metadata = crate::plugin::RequestMetadata {
            domain: request_info.domain.clone(),
            path: request_info.path.clone(),
            method: request_info.method.clone(),
            headers: request_info.headers.clone(),
            query_params: crate::plugin::utils::parse_query_params(&request_info.query),
            client_addr,
            protocol: crate::plugin::ProtocolType::Http,
            route_name: Some(plugin_name.clone()),
            upstream_url: None,
            request_id: crate::plugin::utils::generate_connection_id(),
            timestamp: std::time::SystemTime::now(),
        };
        
        // Send event to plugin
        let event = PluginEvent::DataReceived(crate::plugin::DataPacket {
            data: initial_data.to_vec(),
            metadata: HashMap::new(),
            context: crate::plugin::ConnectionContext {
                connection_id: metadata.request_id.clone(),
                client_addr,
                server_addr: stream.local_addr().unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap()),
                metadata: HashMap::new(),
                created_at: std::time::SystemTime::now(),
            },
        });
        
        self.plugin_manager.send_event_to_plugin(plugin_name, event).await?;
        
        Ok(())
    }
    
    /// Handle TCP connection
    async fn handle_tcp_connection(
        &self,
        stream: tokio::net::TcpStream,
        client_addr: SocketAddr,
        context: crate::plugin::ConnectionContext,
    ) -> Result<()> {
        // Find TCP plugins
        let plugins = self.request_router.find_plugins_for_protocol("TCP").await;
        
        if plugins.is_empty() {
            warn!("No TCP plugin available for connection from {}", client_addr);
            return Ok(());
        }
        
        // Use the first TCP plugin
        let plugin_name = &plugins[0];
        
        // Create connection event
        let event = PluginEvent::ConnectionEstablished(context);
        
        self.plugin_manager.send_event_to_plugin(plugin_name, event).await?;
        
        Ok(())
    }
    
    /// Handle unknown protocol connection
    async fn handle_unknown_connection(
        &self,
        stream: tokio::net::TcpStream,
        client_addr: SocketAddr,
        initial_data: &[u8],
    ) -> Result<()> {
        // Try to find a plugin that accepts unknown protocols
        let plugins = self.plugin_manager.list_plugins();
        
        for plugin_name in plugins {
            if let Some(capabilities) = self.plugin_manager.registry().get_plugin_capabilities(&plugin_name).await {
                if capabilities.custom_protocols.contains(&"unknown".to_string()) {
                    let event = PluginEvent::DataReceived(crate::plugin::DataPacket {
                        data: initial_data.to_vec(),
                        metadata: HashMap::new(),
                        context: crate::plugin::ConnectionContext {
                            connection_id: crate::plugin::utils::generate_connection_id(),
                            client_addr,
                            server_addr: stream.local_addr().unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap()),
                            metadata: HashMap::new(),
                            created_at: std::time::SystemTime::now(),
                        },
                    });
                    
                    self.plugin_manager.send_event_to_plugin(&plugin_name, event).await?;
                    return Ok(());
                }
            }
        }
        
        debug!("No plugin found for unknown protocol from {}", client_addr);
        Ok(())
    }
    
    /// Detect protocol from initial data
    fn detect_protocol(&self, data: &[u8]) -> Result<ProtocolHint> {
        if data.is_empty() {
            return Ok(ProtocolHint::Unknown);
        }
        
        // Check for HTTP
        if data.starts_with(b"GET ") || 
           data.starts_with(b"POST ") ||
           data.starts_with(b"PUT ") ||
           data.starts_with(b"DELETE ") ||
           data.starts_with(b"HEAD ") ||
           data.starts_with(b"OPTIONS ") ||
           data.starts_with(b"PATCH ") {
            return Ok(ProtocolHint::Http);
        }
        
        // Check for TLS
        if data.len() >= 6 && data[0] == 0x16 && data[1] == 0x03 {
            return Ok(ProtocolHint::Http); // Assume HTTPS
        }
        
        // Default to TCP for other protocols
        Ok(ProtocolHint::Tcp)
    }
    
    /// Parse HTTP request from data
    fn parse_http_request(&self, data: &[u8]) -> Result<HttpRequestInfo> {
        let request_str = std::str::from_utf8(data)
            .map_err(|_| RouterError::RequestError("Invalid UTF-8 in request".to_string()))?;
        
        let lines: Vec<&str> = request_str.split("\r\n").collect();
        if lines.is_empty() {
            return Err(RouterError::RequestError("Empty request".to_string()));
        }
        
        // Parse request line
        let request_line_parts: Vec<&str> = lines[0].split_whitespace().collect();
        if request_line_parts.len() < 3 {
            return Err(RouterError::RequestError("Invalid request line".to_string()));
        }
        
        let method = request_line_parts[0].to_string();
        let uri = request_line_parts[1];
        
        // Parse URI
        let (path, query) = if let Some(query_pos) = uri.find('?') {
            (uri[..query_pos].to_string(), uri[query_pos + 1..].to_string())
        } else {
            (uri.to_string(), String::new())
        };
        
        // Parse headers
        let mut headers = HashMap::new();
        let mut domain = String::new();
        
        for line in lines.iter().skip(1) {
            if line.is_empty() {
                break;
            }
            
            if let Some(colon_pos) = line.find(':') {
                let key = line[..colon_pos].trim().to_string();
                let value = line[colon_pos + 1..].trim().to_string();
                
                if key.to_lowercase() == "host" {
                    domain = value.split(':').next().unwrap_or(&value).to_string();
                }
                
                headers.insert(key, value);
            }
        }
        
        Ok(HttpRequestInfo {
            method,
            path,
            query,
            domain,
            headers,
        })
    }
    
    /// Send HTTP error response
    async fn send_http_error(&self, mut stream: tokio::net::TcpStream, status: u16, message: &str) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        
        let response = format!(
            "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\r\n{}",
            status, message, message.len(), message
        );
        
        stream.write_all(response.as_bytes()).await?;
        stream.flush().await?;
        
        Ok(())
    }
    
    /// Load built-in plugins
    async fn load_builtin_plugins(&self) -> Result<()> {
        info!("Loading built-in plugins...");
        
        // Load HTTP proxy plugin
        let http_plugin = Box::new(crate::builtin_plugins::HttpProxyPlugin::new());
        let http_config = crate::plugin::PluginConfig {
            name: "http_proxy".to_string(),
            enabled: true,
            priority: 0,
            bind_ports: vec![],
            bind_addresses: vec![],
            domains: vec![],
            config: serde_json::json!({}),
            middleware_chain: vec![],
            load_balancing: None,
            health_check: None,
        };
        
        self.plugin_manager.registry().register_plugin(
            "http_proxy".to_string(),
            http_plugin,
            http_config,
        ).await?;
        
        // Load TCP proxy plugin
        let tcp_plugin = Box::new(crate::builtin_plugins::TcpProxyPlugin::new());
        let tcp_config = crate::plugin::PluginConfig {
            name: "tcp_proxy".to_string(),
            enabled: true,
            priority: 0,
            bind_ports: vec![],
            bind_addresses: vec![],
            domains: vec![],
            config: serde_json::json!({}),
            middleware_chain: vec![],
            load_balancing: None,
            health_check: None,
        };
        
        self.plugin_manager.registry().register_plugin(
            "tcp_proxy".to_string(),
            tcp_plugin,
            tcp_config,
        ).await?;
        
        info!("Built-in plugins loaded successfully");
        Ok(())
    }
    
    /// Start event processing
    async fn start_event_processing(&self) -> Result<()> {
        let event_router = self.event_router.clone();
        let plugin_manager = self.plugin_manager.clone();
        
        tokio::spawn(async move {
            // Event processing loop would go here
            // This would handle inter-plugin communication
        });
        
        Ok(())
    }
    
    /// Clone for use in connection handlers
    fn clone_for_handler(&self) -> Self {
        Self {
            config: self.config.clone(),
            plugin_manager: self.plugin_manager.clone(),
            request_router: self.request_router.clone(),
            middleware_stack: self.middleware_stack.clone(),
            metrics_collector: self.metrics_collector.clone(),
            health_checker: self.health_checker.clone(),
            management_api: self.management_api.clone(),
            running: self.running.clone(),
            start_time: self.start_time,
            shutdown_sender: self.shutdown_sender.clone(),
            event_router: self.event_router.clone(),
            statistics: self.statistics.clone(),
        }
    }
    
    /// Get plugin manager
    pub fn plugin_manager(&self) -> Arc<PluginManager> {
        self.plugin_manager.clone()
    }
    
    /// Reload configuration
    pub async fn reload_config(&self, new_config: RouterConfig) -> Result<()> {
        info!("Reloading configuration...");
        
        {
            let mut config = self.config.write().await;
            *config = new_config.clone();
        }
        
        // Update router
        self.request_router.update_config(
            new_config.domains.clone(),
            new_config.proxies.clone(),
        ).await;
        
        // Update middleware stack
        self.middleware_stack.update_config(new_config.middleware.clone()).await;
        
        info!("Configuration reloaded successfully");
        Ok(())
    }
    
    /// Enable management API
    pub async fn enable_management_api(&self, port: u16) -> Result<()> {
        if self.management_api.is_some() {
            return Ok(()); // Already enabled
        }
        
        let api = Arc::new(ManagementApi::new(
            port,
            self.plugin_manager.clone(),
            self.config.clone(),
        ));
        
        api.start().await?;
        
        // Store reference (this would need to be mutable in real implementation)
        // self.management_api = Some(api);
        
        Ok(())
    }
    
    /// Check if server is running
    pub async fn is_running(&self) -> bool {
        *self.running.read().await
    }
    
    /// Get server statistics
    pub async fn get_statistics(&self) -> ServerStatistics {
        let mut stats = self.statistics.read().await.clone();
        stats.uptime_seconds = self.start_time.elapsed().as_secs();
        stats.plugin_count = self.plugin_manager.list_plugins().len();
        
        // Get memory usage (simplified)
        #[cfg(target_os = "linux")]
        {
            if let Ok(info) = procfs::process::Process::myself() {
                if let Ok(stat) = info.stat() {
                    stats.memory_usage_bytes = stat.rss * 4096; // Convert pages to bytes
                }
            }
        }
        
        stats
    }
    
    /// Wait for server to finish
    pub async fn wait(&self) -> Result<()> {
        let mut shutdown_rx = self.shutdown_sender.as_ref().unwrap().subscribe();
        let _ = shutdown_rx.recv().await;
        Ok(())
    }
}

#[derive(Debug)]
enum ProtocolHint {
    Http,
    Tcp,
    Unknown,
}

#[derive(Debug)]
struct HttpRequestInfo {
    method: String,
    path: String,
    query: String,
    domain: String,
    headers: HashMap<String, String>,
}

/// Event router for inter-plugin communication
pub struct EventRouter {
    // Event routing implementation would go here
}

impl EventRouter {
    pub fn new() -> Self {
        Self {}
    }
}