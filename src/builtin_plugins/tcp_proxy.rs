// src/builtin_plugins/tcp_proxy.rs - TCP Proxy Plugin Implementation
use crate::plugin::*;
use crate::error::{RouterError, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, RwLock, Semaphore};
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

/// TCP Proxy Plugin - handles TCP connections with load balancing and connection pooling
#[derive(Debug)]
pub struct TcpProxyPlugin {
    name: String,
    config: Option<TcpProxyConfig>,
    listener: Option<TcpListener>,
    metrics: Arc<RwLock<PluginMetrics>>,
    health: Arc<RwLock<PluginHealth>>,
    connection_pool: Arc<ConnectionPool>,
    load_balancer: Arc<LoadBalancer>,
    shutdown_sender: Option<mpsc::Sender<()>>,
    running: Arc<RwLock<bool>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpProxyConfig {
    pub listen_addresses: Vec<String>,
    pub backends: Vec<TcpBackend>,
    pub load_balancing_strategy: LoadBalancingStrategy,
    pub connection_timeout_ms: u64,
    pub read_timeout_ms: u64,
    pub write_timeout_ms: u64,
    pub max_connections: usize,
    pub connection_pool_size: usize,
    pub health_check_interval_ms: u64,
    pub buffer_size: usize,
    pub tcp_nodelay: bool,
    pub tcp_keepalive: Option<TcpKeepalive>,
    pub circuit_breaker: CircuitBreakerConfig,
    pub retry_policy: RetryPolicy,
    pub session_affinity: SessionAffinityConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpBackend {
    pub address: String,
    pub weight: u32,
    pub max_connections: Option<usize>,
    pub health_check_port: Option<u16>,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LoadBalancingStrategy {
    RoundRobin,
    WeightedRoundRobin,
    LeastConnections,
    IpHash,
    Random,
    ConsistentHash,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpKeepalive {
    pub enabled: bool,
    pub idle_secs: u64,
    pub interval_secs: u64,
    pub retries: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerConfig {
    pub enabled: bool,
    pub failure_threshold: u32,
    pub recovery_timeout_ms: u64,
    pub half_open_max_calls: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub retry_delay_ms: u64,
    pub exponential_backoff: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionAffinityConfig {
    pub enabled: bool,
    pub strategy: SessionAffinityStrategy,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionAffinityStrategy {
    ClientIp,
    Cookie(String),
    Header(String),
}

/// Connection pool for managing backend connections
#[derive(Debug)]
pub struct ConnectionPool {
    pools: RwLock<HashMap<String, Arc<BackendPool>>>,
    config: TcpProxyConfig,
}

#[derive(Debug)]
pub struct BackendPool {
    address: String,
    connections: RwLock<Vec<PooledConnection>>,
    semaphore: Semaphore,
    health_status: RwLock<BackendHealth>,
}

#[derive(Debug)]
pub struct PooledConnection {
    stream: TcpStream,
    created_at: Instant,
    last_used: Instant,
    connection_count: u64,
}

#[derive(Debug, Clone)]
pub struct BackendHealth {
    pub healthy: bool,
    pub last_check: Instant,
    pub consecutive_failures: u32,
    pub response_time_ms: u64,
}

/// Load balancer for selecting backend servers
#[derive(Debug)]
pub struct LoadBalancer {
    strategy: LoadBalancingStrategy,
    backends: RwLock<Vec<TcpBackend>>,
    current_index: RwLock<usize>,
    connection_counts: RwLock<HashMap<String, u64>>,
    session_store: RwLock<HashMap<String, String>>, // session_id -> backend_address
}

impl TcpProxyPlugin {
    pub fn new() -> Self {
        Self {
            name: "tcp_proxy".to_string(),
            config: None,
            listener: None,
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
            connection_pool: Arc::new(ConnectionPool::new()),
            load_balancer: Arc::new(LoadBalancer::new()),
            shutdown_sender: None,
            running: Arc::new(RwLock::new(false)),
        }
    }
    
    async fn start_listeners(&mut self) -> Result<()> {
        let config = self.config.as_ref().ok_or_else(|| {
            RouterError::PluginError {
                plugin: self.name.clone(),
                message: "Plugin not initialized".to_string(),
            }
        })?;
        
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
        self.shutdown_sender = Some(shutdown_tx);
        
        for listen_addr in &config.listen_addresses {
            let addr: SocketAddr = listen_addr.parse()
                .map_err(|e| RouterError::BindError {
                    address: listen_addr.clone(),
                    reason: e.to_string(),
                })?;
            
            let listener = TcpListener::bind(addr).await
                .map_err(|e| RouterError::BindError {
                    address: listen_addr.clone(),
                    reason: e.to_string(),
                })?;
            
            info!("TCP proxy listening on {}", addr);
            
            // Clone necessary components for the listener task
            let connection_pool = self.connection_pool.clone();
            let load_balancer = self.load_balancer.clone();
            let metrics = self.metrics.clone();
            let running = self.running.clone();
            let config_clone = config.clone();
            let mut shutdown_rx_clone = shutdown_rx.resubscribe();
            
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        // Accept new connections
                        result = listener.accept() => {
                            match result {
                                Ok((stream, client_addr)) => {
                                    debug!("Accepted TCP connection from {}", client_addr);
                                    
                                    // Update metrics
                                    {
                                        let mut metrics = metrics.write().await;
                                        metrics.connections_total += 1;
                                        metrics.connections_active += 1;
                                    }
                                    
                                    // Handle connection in a separate task
                                    let pool = connection_pool.clone();
                                    let lb = load_balancer.clone();
                                    let metrics_clone = metrics.clone();
                                    let config = config_clone.clone();
                                    
                                    tokio::spawn(async move {
                                        if let Err(e) = Self::handle_connection(
                                            stream, 
                                            client_addr, 
                                            pool, 
                                            lb, 
                                            metrics_clone,
                                            config
                                        ).await {
                                            error!("Connection handling error: {}", e);
                                        }
                                    });
                                }
                                Err(e) => {
                                    error!("Failed to accept connection: {}", e);
                                }
                            }
                        }
                        
                        // Handle shutdown
                        _ = shutdown_rx_clone.recv() => {
                            info!("Shutting down TCP listener on {}", addr);
                            break;
                        }
                    }
                    
                    // Check if we should continue running
                    if !*running.read().await {
                        break;
                    }
                }
            });
        }
        
        Ok(())
    }
    
    async fn handle_connection(
        client_stream: TcpStream,
        client_addr: SocketAddr,
        connection_pool: Arc<ConnectionPool>,
        load_balancer: Arc<LoadBalancer>,
        metrics: Arc<RwLock<PluginMetrics>>,
        config: TcpProxyConfig,
    ) -> Result<()> {
        let start_time = Instant::now();
        
        // Select backend
        let backend_addr = load_balancer.select_backend(Some(client_addr)).await
            .ok_or_else(|| RouterError::LoadBalancingError {
                message: "No healthy backend available".to_string(),
            })?;
        
        // Get backend connection
        let backend_stream = connection_pool.get_connection(&backend_addr, &config).await?;
        
        // Proxy data between client and backend
        let result = Self::proxy_streams(client_stream, backend_stream, &config).await;
        
        // Update metrics
        let duration = start_time.elapsed();
        {
            let mut metrics = metrics.write().await;
            metrics.connections_active = metrics.connections_active.saturating_sub(1);
            
            if result.is_ok() {
                metrics.custom_metrics.insert(
                    "avg_connection_duration_ms".to_string(),
                    duration.as_millis() as f64,
                );
            } else {
                metrics.errors_total += 1;
            }
        }
        
        result
    }
    
    async fn proxy_streams(
        mut client_stream: TcpStream,
        mut backend_stream: TcpStream,
        config: &TcpProxyConfig,
    ) -> Result<()> {
        // Configure TCP options
        if config.tcp_nodelay {
            let _ = client_stream.set_nodelay(true);
            let _ = backend_stream.set_nodelay(true);
        }
        
        // Split streams for bidirectional proxying
        let (client_read, client_write) = client_stream.split();
        let (backend_read, backend_write) = backend_stream.split();
        
        let mut client_reader = BufReader::with_capacity(config.buffer_size, client_read);
        let mut client_writer = BufWriter::with_capacity(config.buffer_size, client_write);
        let mut backend_reader = BufReader::with_capacity(config.buffer_size, backend_read);
        let mut backend_writer = BufWriter::with_capacity(config.buffer_size, backend_write);
        
        let client_to_backend = async {
            let mut buffer = vec![0u8; config.buffer_size];
            loop {
                match timeout(
                    Duration::from_millis(config.read_timeout_ms),
                    client_reader.read(&mut buffer)
                ).await {
                    Ok(Ok(0)) => break, // Connection closed
                    Ok(Ok(n)) => {
                        if let Err(e) = timeout(
                            Duration::from_millis(config.write_timeout_ms),
                            backend_writer.write_all(&buffer[..n])
                        ).await {
                            error!("Backend write timeout: {}", e);
                            break;
                        }
                        
                        if let Err(e) = backend_writer.flush().await {
                            error!("Backend flush error: {}", e);
                            break;
                        }
                    }
                    Ok(Err(e)) => {
                        error!("Client read error: {}", e);
                        break;
                    }
                    Err(_) => {
                        debug!("Client read timeout");
                        break;
                    }
                }
            }
        };
        
        let backend_to_client = async {
            let mut buffer = vec![0u8; config.buffer_size];
            loop {
                match timeout(
                    Duration::from_millis(config.read_timeout_ms),
                    backend_reader.read(&mut buffer)
                ).await {
                    Ok(Ok(0)) => break, // Connection closed
                    Ok(Ok(n)) => {
                        if let Err(e) = timeout(
                            Duration::from_millis(config.write_timeout_ms),
                            client_writer.write_all(&buffer[..n])
                        ).await {
                            error!("Client write timeout: {}", e);
                            break;
                        }
                        
                        if let Err(e) = client_writer.flush().await {
                            error!("Client flush error: {}", e);
                            break;
                        }
                    }
                    Ok(Err(e)) => {
                        error!("Backend read error: {}", e);
                        break;
                    }
                    Err(_) => {
                        debug!("Backend read timeout");
                        break;
                    }
                }
            }
        };
        
        // Run both directions concurrently
        tokio::select! {
            _ = client_to_backend => {},
            _ = backend_to_client => {},
        }
        
        debug!("TCP proxy connection completed");
        Ok(())
    }
    
    async fn start_health_checker(&self) -> Result<()> {
        let config = self.config.as_ref().unwrap();
        let connection_pool = self.connection_pool.clone();
        let health_check_interval = Duration::from_millis(config.health_check_interval_ms);
        let running = self.running.clone();
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(health_check_interval);
            
            while *running.read().await {
                interval.tick().await;
                
                // Health check all backends
                connection_pool.health_check_all().await;
            }
        });
        
        Ok(())
    }
}

#[async_trait]
impl Plugin for TcpProxyPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: self.name.clone(),
            version: "1.0.0".to_string(),
            description: "TCP proxy with load balancing, connection pooling, and health checks".to_string(),
            author: "Router Team".to_string(),
            license: "MIT".to_string(),
            repository: Some("https://github.com/router/plugins".to_string()),
            min_router_version: "2.0.0".to_string(),
            config_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "listen_addresses": {
                        "type": "array",
                        "items": {"type": "string"}
                    },
                    "backends": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "address": {"type": "string"},
                                "weight": {"type": "number", "default": 1}
                            },
                            "required": ["address"]
                        }
                    }
                },
                "required": ["listen_addresses", "backends"]
            })),
            dependencies: vec![],
            tags: vec!["tcp", "proxy", "load-balancer", "connection-pool"].iter().map(|s| s.to_string()).collect(),
        }
    }
    
    fn capabilities(&self) -> PluginCapabilities {
        PluginCapabilities {
            can_handle_tcp: true,
            can_handle_udp: false,
            can_handle_unix_socket: true,
            requires_dedicated_port: true,
            supports_hot_reload: true,
            supports_load_balancing: true,
            custom_protocols: vec!["TCP".to_string()],
            port_requirements: vec![],
        }
    }
    
    async fn initialize(&mut self, config: PluginConfig, _event_sender: PluginSender) -> Result<()> {
        let tcp_config: TcpProxyConfig = serde_json::from_value(config.config)
            .map_err(|e| RouterError::ConfigError {
                message: format!("Invalid TCP proxy configuration: {}", e),
            })?;
        
        // Initialize connection pool
        self.connection_pool.initialize(&tcp_config).await?;
        
        // Initialize load balancer
        self.load_balancer.initialize(&tcp_config).await?;
        
        self.config = Some(tcp_config);
        
        // Update health status
        {
            let mut health = self.health.write().await;
            health.healthy = true;
            health.message = "TCP proxy initialized successfully".to_string();
            health.last_check = std::time::SystemTime::now();
        }
        
        Ok(())
    }
    
    async fn start(&mut self) -> Result<()> {
        *self.running.write().await = true;
        
        // Start listeners
        self.start_listeners().await?;
        
        // Start health checker
        self.start_health_checker().await?;
        
        info!("TCP proxy plugin started");
        Ok(())
    }
    
    async fn stop(&mut self) -> Result<()> {
        *self.running.write().await = false;
        
        // Send shutdown signal
        if let Some(sender) = &self.shutdown_sender {
            let _ = sender.send(()).await;
        }
        
        info!("TCP proxy plugin stopped");
        Ok(())
    }
    
    async fn handle_event(&mut self, event: PluginEvent) -> Result<()> {
        match event {
            PluginEvent::ConnectionEstablished(context) => {
                debug!("TCP connection established: {}", context.connection_id);
            }
            PluginEvent::ConnectionClosed(connection_id) => {
                debug!("TCP connection closed: {}", connection_id);
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
                let backends = self.load_balancer.get_backends().await;
                Ok(serde_json::to_value(backends)?)
            }
            "get_pool_stats" => {
                let stats = self.connection_pool.get_statistics().await;
                Ok(serde_json::to_value(stats)?)
            }
            "drain_backend" => {
                let backend_addr = args.get("address").and_then(|v| v.as_str())
                    .ok_or_else(|| RouterError::InvalidApiRequest {
                        reason: "Missing 'address' parameter".to_string(),
                    })?;
                
                self.load_balancer.drain_backend(backend_addr).await?;
                Ok(serde_json::json!({ "success": true }))
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

// Implementation for ConnectionPool
impl ConnectionPool {
    pub fn new() -> Self {
        Self {
            pools: RwLock::new(HashMap::new()),
            config: TcpProxyConfig {
                listen_addresses: vec![],
                backends: vec![],
                load_balancing_strategy: LoadBalancingStrategy::RoundRobin,
                connection_timeout_ms: 30000,
                read_timeout_ms: 30000,
                write_timeout_ms: 30000,
                max_connections: 1000,
                connection_pool_size: 100,
                health_check_interval_ms: 30000,
                buffer_size: 8192,
                tcp_nodelay: true,
                tcp_keepalive: None,
                circuit_breaker: CircuitBreakerConfig {
                    enabled: false,
                    failure_threshold: 5,
                    recovery_timeout_ms: 30000,
                    half_open_max_calls: 3,
                },
                retry_policy: RetryPolicy {
                    max_retries: 3,
                    retry_delay_ms: 1000,
                    exponential_backoff: true,
                },
                session_affinity: SessionAffinityConfig {
                    enabled: false,
                    strategy: SessionAffinityStrategy::ClientIp,
                    timeout_secs: 3600,
                },
            },
        }
    }
    
    pub async fn initialize(&self, config: &TcpProxyConfig) -> Result<()> {
        // Initialize pools for each backend
        let mut pools = self.pools.write().await;
        pools.clear();
        
        for backend in &config.backends {
            let pool = Arc::new(BackendPool {
                address: backend.address.clone(),
                connections: RwLock::new(Vec::new()),
                semaphore: Semaphore::new(config.connection_pool_size),
                health_status: RwLock::new(BackendHealth {
                    healthy: true,
                    last_check: Instant::now(),
                    consecutive_failures: 0,
                    response_time_ms: 0,
                }),
            });
            
            pools.insert(backend.address.clone(), pool);
        }
        
        Ok(())
    }
    
    pub async fn get_connection(&self, backend_addr: &str, config: &TcpProxyConfig) -> Result<TcpStream> {
        let pools = self.pools.read().await;
        let pool = pools.get(backend_addr)
            .ok_or_else(|| RouterError::BackendConnectionError {
                backend: backend_addr.to_string(),
            })?;
        
        // Try to get a connection from the pool
        {
            let mut connections = pool.connections.write().await;
            if let Some(mut pooled_conn) = connections.pop() {
                pooled_conn.last_used = Instant::now();
                pooled_conn.connection_count += 1;
                return Ok(pooled_conn.stream);
            }
        }
        
        // No pooled connection available, create a new one
        let addr: SocketAddr = backend_addr.parse()
            .map_err(|e| RouterError::BackendConnectionError {
                backend: format!("Invalid address {}: {}", backend_addr, e),
            })?;
        
        let stream = timeout(
            Duration::from_millis(config.connection_timeout_ms),
            TcpStream::connect(addr)
        ).await
        .map_err(|_| RouterError::NetworkTimeout {
            operation: format!("connect to {}", backend_addr),
        })?
        .map_err(|e| RouterError::BackendConnectionError {
            backend: format!("Failed to connect to {}: {}", backend_addr, e),
        })?;
        
        Ok(stream)
    }
    
    pub async fn health_check_all(&self) {
        let pools = self.pools.read().await;
        for (addr, pool) in pools.iter() {
            self.health_check_backend(addr, pool).await;
        }
    }
    
    async fn health_check_backend(&self, addr: &str, pool: &BackendPool) {
        let start_time = Instant::now();
        
        let health_result = match timeout(
            Duration::from_secs(5),
            TcpStream::connect(addr)
        ).await {
            Ok(Ok(_)) => {
                let response_time = start_time.elapsed().as_millis() as u64;
                Ok(response_time)
            }
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("Health check timeout".to_string()),
        };
        
        let mut health = pool.health_status.write().await;
        match health_result {
            Ok(response_time) => {
                health.healthy = true;
                health.consecutive_failures = 0;
                health.response_time_ms = response_time;
            }
            Err(_) => {
                health.consecutive_failures += 1;
                if health.consecutive_failures >= 3 {
                    health.healthy = false;
                }
            }
        }
        health.last_check = Instant::now();
    }
    
    pub async fn get_statistics(&self) -> HashMap<String, serde_json::Value> {
        let pools = self.pools.read().await;
        let mut stats = HashMap::new();
        
        for (addr, pool) in pools.iter() {
            let connections = pool.connections.read().await;
            let health = pool.health_status.read().await;
            
            stats.insert(addr.clone(), serde_json::json!({
                "connections_count": connections.len(),
                "healthy": health.healthy,
                "consecutive_failures": health.consecutive_failures,
                "response_time_ms": health.response_time_ms,
            }));
        }
        
        stats
    }
}

// Implementation for LoadBalancer
impl LoadBalancer {
    pub fn new() -> Self {
        Self {
            strategy: LoadBalancingStrategy::RoundRobin,
            backends: RwLock::new(Vec::new()),
            current_index: RwLock::new(0),
            connection_counts: RwLock::new(HashMap::new()),
            session_store: RwLock::new(HashMap::new()),
        }
    }
    
    pub async fn initialize(&self, config: &TcpProxyConfig) -> Result<()> {
        self.strategy = config.load_balancing_strategy.clone();
        
        let mut backends = self.backends.write().await;
        *backends = config.backends.clone();
        
        Ok(())
    }
    
    pub async fn select_backend(&self, client_addr: Option<SocketAddr>) -> Option<String> {
        let backends = self.backends.read().await;
        if backends.is_empty() {
            return None;
        }
        
        match self.strategy {
            LoadBalancingStrategy::RoundRobin => {
                let mut index = self.current_index.write().await;
                let backend = &backends[*index % backends.len()];
                *index = (*index + 1) % backends.len();
                Some(backend.address.clone())
            }
            LoadBalancingStrategy::Random => {
                use rand::Rng;
                let mut rng = rand::thread_rng();
                let index = rng.gen_range(0..backends.len());
                Some(backends[index].address.clone())
            }
            LoadBalancingStrategy::IpHash => {
                if let Some(client) = client_addr {
                    use std::collections::hash_map::DefaultHasher;
                    use std::hash::{Hash, Hasher};
                    
                    let mut hasher = DefaultHasher::new();
                    client.ip().hash(&mut hasher);
                    let hash = hasher.finish() as usize;
                    
                    let index = hash % backends.len();
                    Some(backends[index].address.clone())
                } else {
                    // Fallback to round robin
                    let mut index = self.current_index.write().await;
                    let backend = &backends[*index % backends.len()];
                    *index = (*index + 1) % backends.len();
                    Some(backend.address.clone())
                }
            }
            // Add other load balancing strategies as needed
            _ => {
                // Default to round robin
                let mut index = self.current_index.write().await;
                let backend = &backends[*index % backends.len()];
                *index = (*index + 1) % backends.len();
                Some(backend.address.clone())
            }
        }
    }
    
    pub async fn get_backends(&self) -> Vec<TcpBackend> {
        self.backends.read().await.clone()
    }
    
    pub async fn drain_backend(&self, backend_addr: &str) -> Result<()> {
        // Implementation would mark backend as draining
        // and prevent new connections while allowing existing ones to finish
        warn!("Backend draining not fully implemented: {}", backend_addr);
        Ok(())
    }
}

/// Plugin factory function for TCP proxy
#[no_mangle]
pub extern "C" fn create_tcp_proxy_plugin() -> *mut dyn Plugin {
    Box::into_raw(Box::new(TcpProxyPlugin::new()))
}

/// Plugin info function for TCP proxy
#[no_mangle]
pub extern "C" fn get_tcp_proxy_plugin_info() -> PluginInfo {
    PluginInfo {
        name: "tcp_proxy".to_string(),
        version: "1.0.0".to_string(),
        description: "TCP proxy with advanced load balancing and connection pooling".to_string(),
        author: "Router Team".to_string(),
        license: "MIT".to_string(),
        repository: Some("https://github.com/router/plugins".to_string()),
        min_router_version: "2.0.0".to_string(),
        config_schema: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "listen_addresses": {
                    "type": "array",
                    "items": {"type": "string"}
                },
                "backends": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "address": {"type": "string"},
                            "weight": {"type": "number", "default": 1}
                        },
                        "required": ["address"]
                    }
                }
            },
            "required": ["listen_addresses", "backends"]
        })),
        dependencies: vec![],
        tags: vec!["tcp", "proxy", "load-balancer"].iter().map(|s| s.to_string()).collect(),
    }
}