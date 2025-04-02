// src/tcp_proxy.rs
use crate::config::{ProxyConfig, RouteConfig};
use dashmap::DashMap;
use metrics::{counter, histogram};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio::time::timeout;

type SharedConfig = Arc<RwLock<ProxyConfig>>;
type ConnectionCache = Arc<DashMap<String, Vec<(TcpStream, Instant)>>>;

pub struct TcpProxyServer {
    config: SharedConfig,
    connection_cache: ConnectionCache,
    max_idle_time: Duration,
    pooled_connections: bool,
}

impl TcpProxyServer {
    pub fn new(config: SharedConfig) -> Self {
        let max_idle_time_secs = {
            let cfg = config.read().unwrap();
            cfg.tcp_proxy.max_idle_time_secs.unwrap_or(60)
        };
        Self {
            config,
            connection_cache: Arc::new(DashMap::new()),
            max_idle_time: Duration::from_secs(max_idle_time_secs),
            pooled_connections: true,
        }
    }
    pub async fn run(&self, addr: &str) -> io::Result<()> {
        let listener = TcpListener::bind(addr).await?;
        tracing::info!("TCP proxy listening on {}", addr);

        // Spawn a task to periodically clean idle connections
        let cache = self.connection_cache.clone();
        let max_idle = self.max_idle_time;
        tokio::spawn(async move {
            loop {
                TcpProxyServer::clean_idle_connections(cache.clone(), max_idle).await;
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });

        loop {
            let (client_stream, client_addr) = match listener.accept().await {
                Ok(connection) => connection,
                Err(e) => {
                    tracing::error!("Failed to accept connection: {}", e);
                    continue;
                }
            };

            tracing::info!("Accepted connection from {}", client_addr);

            // Clone required resources for the handler task
            let config = self.config.clone();
            let cache = self.connection_cache.clone();
            let max_idle = self.max_idle_time;
            let pooled = self.pooled_connections;

            // Spawn a task to handle the connection
            tokio::spawn(async move {
                if let Err(e) = Self::handle_connection(client_stream, client_addr, config, cache, max_idle, pooled).await {
                    tracing::error!("Connection error: {}", e);
                }
            });
        }
    }

    async fn handle_connection(
        mut client_stream: TcpStream,
        client_addr: SocketAddr,
        config: SharedConfig,
        connection_cache: ConnectionCache,
        max_idle_time: Duration,
        use_pooling: bool,
    ) -> io::Result<()> {
        // For simplicity, we'll use the first route as the default TCP route
        // In a real implementation, you'd need some way to determine the appropriate route
        // possibly by examining the first packet or using separate listeners for different services
        let route_config = {
            let config_guard = config.read().await;
            let first_route = config_guard.routes.iter().next();
            
            if let Some((_, route)) = first_route {
                route.clone()
            } else {
                return Err(io::Error::new(io::ErrorKind::NotFound, "No routes configured"));
            }
        };

        // Extract host and port from upstream URL
        let upstream_url = &route_config.upstream;
        let parts: Vec<&str> = upstream_url.split("://").collect();
        let host_port = if parts.len() > 1 {
            parts[1].split('/').next().unwrap_or(parts[1])
        } else {
            upstream_url.split('/').next().unwrap_or(upstream_url)
        };

        // Start timing
        let start = Instant::now();

        // Try to get a cached connection or create a new one
        let mut server_stream = if use_pooling {
            match Self::get_or_create_connection(host_port, &connection_cache, max_idle_time).await {
                Ok(stream) => stream,
                Err(e) => {
                    tracing::error!("Failed to connect to upstream {}: {}", host_port, e);
                    return Err(e);
                }
            }
        } else {
            match TcpStream::connect(host_port).await {
                Ok(stream) => stream,
                Err(e) => {
                    tracing::error!("Failed to connect to upstream {}: {}", host_port, e);
                    return Err(e);
                }
            }
        };

        // Set up timeout based on route configuration
        let timeout_ms = route_config.timeout_ms.unwrap_or(5000);
        let timeout_duration = Duration::from_millis(timeout_ms);

        // Bidirectional copy with timeout
        let (mut client_read, mut client_write) = client_stream.split();
        let (mut server_read, mut server_write) = server_stream.split();

        // Buffer for data transfer
        let mut buffer = vec![0u8; 65536]; // 64K buffer

        // Process data in both directions
        loop {
            // Read from client with timeout
            let read_result = match timeout(timeout_duration, client_read.read(&mut buffer)).await {
                Ok(result) => result,
                Err(_) => {
                    tracing::warn!("Client read timeout from {}", client_addr);
                    counter!("tcp_proxy.timeout", 1);
                    break;
                }
            };

            // Process read result
            match read_result {
                Ok(0) => {
                    // Client closed the connection
                    break;
                }
                Ok(n) => {
                    // Write data to server
                    if let Err(e) = server_write.write_all(&buffer[..n]).await {
                        tracing::error!("Failed to write to server: {}", e);
                        break;
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to read from client: {}", e);
                    break;
                }
            }

            // Read from server with timeout
            let read_result = match timeout(timeout_duration, server_read.read(&mut buffer)).await {
                Ok(result) => result,
                Err(_) => {
                    tracing::warn!("Server read timeout");
                    counter!("tcp_proxy.timeout", 1);
                    break;
                }
            };

            // Process read result
            match read_result {
                Ok(0) => {
                    // Server closed the connection
                    break;
                }
                Ok(n) => {
                    // Write data to client
                    if let Err(e) = client_write.write_all(&buffer[..n]).await {
                        tracing::error!("Failed to write to client: {}", e);
                        break;
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to read from server: {}", e);
                    break;
                }
            }
        }

        // Record metrics
        let duration = start.elapsed();
        histogram!("tcp_proxy.connection.duration", duration.as_secs_f64());
        counter!("tcp_proxy.connection.completed", 1);
        
        // If we're using connection pooling and the connection is still good, return it to the pool
        // This would be a more complex check in a real implementation
        if use_pooling {
            // Recombine the split parts (we can't actually do this easily, so this is simplified)
            // In a real implementation, you'd need a different approach to reuse the connection
            tracing::info!("Connection completed, not returning to pool in this simplified example");
        }

        Ok(())
    }

    async fn get_or_create_connection(
        target: &str, 
        cache: &ConnectionCache,
        max_idle_time: Duration
    ) -> io::Result<TcpStream> {
        // Try to get an existing connection from the pool
        if let Some(mut connections) = cache.get_mut(target) {
            while let Some((conn, timestamp)) = connections.pop() {
                if timestamp.elapsed() < max_idle_time {
                    tracing::debug!("Reusing cached connection to {}", target);
                    return Ok(conn);
                }
                // Connection too old, discard it
                tracing::debug!("Discarding expired connection to {}", target);
            }
        }

        // No valid connection found, create a new one
        tracing::debug!("Creating new connection to {}", target);
        let stream = TcpStream::connect(target).await?;
        counter!("tcp_proxy.connection.new", 1);
        Ok(stream)
    }

    async fn clean_idle_connections(cache: ConnectionCache, max_idle_time: Duration) {
        let now = Instant::now();
        
        for mut entry in cache.iter_mut() {
            let connections = entry.value_mut();
            
            // Remove expired connections
            connections.retain(|(_, timestamp)| {
                now.duration_since(*timestamp) < max_idle_time
            });
            
            tracing::debug!("{} connections remaining for {}", connections.len(), entry.key());
        }
    }
}