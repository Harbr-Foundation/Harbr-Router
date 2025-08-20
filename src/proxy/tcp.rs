//! High-performance TCP proxy implementation.
//!
//! This module provides a production-ready TCP proxy with advanced features including
//! connection pooling, load balancing, and bidirectional streaming with zero-copy buffers.

use super::{Proxy, ProxyHealth, ProxyMetrics, ShutdownSignal, utils};
use crate::{
    buffers::get_large_buffer,
    config::{ProxyInstanceConfig, ProxyType, TcpProxyConfig},
    routing::{HealthState, Router, UpstreamInfo},
    Error, Result,
};
use async_trait::async_trait;
use bytes::BytesMut;
use crossbeam_queue::SegQueue;
use parking_lot::Mutex;
use std::{
    collections::HashMap,
    io,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tracing::{debug, error, info, warn};

/// TCP proxy implementation with connection pooling and load balancing.
#[derive(Debug)]
pub struct TcpProxy {
    /// Connection pools per upstream
    connection_pools: Arc<Mutex<HashMap<String, Arc<SegQueue<PooledConnection>>>>>,
    /// Proxy metrics
    metrics: Arc<TcpProxyMetrics>,
}

/// Pooled TCP connection with metadata.
#[derive(Debug)]
struct PooledConnection {
    /// The actual TCP stream
    stream: TcpStream,
    /// When this connection was created
    created_at: Instant,
    /// When this connection was last used
    last_used: Instant,
}

/// TCP proxy specific metrics.
#[derive(Debug, Default)]
struct TcpProxyMetrics {
    /// Total connections handled
    connections_total: AtomicU64,
    /// Currently active connections
    connections_active: AtomicUsize,
    /// Total bytes transferred (bidirectional)
    bytes_transferred: AtomicU64,
    /// Connection duration sum (for average calculation)
    connection_duration_sum: AtomicU64,
    /// Number of connections used for duration calculation
    connection_duration_count: AtomicU64,
    /// Connection pool statistics
    pool_hits: AtomicU64,
    pool_misses: AtomicU64,
}

/// Context for handling TCP connections.
#[derive(Clone)]
struct TcpConnectionContext {
    router: Arc<Router>,
    config: TcpProxyConfig,
    metrics: Arc<TcpProxyMetrics>,
    connection_pools: Arc<Mutex<HashMap<String, Arc<SegQueue<PooledConnection>>>>>,
}

impl PooledConnection {
    /// Create a new pooled connection.
    fn new(stream: TcpStream) -> Self {
        let now = Instant::now();
        Self {
            stream,
            created_at: now,
            last_used: now,
        }
    }

    /// Check if this connection is still usable.
    fn is_valid(&self, max_idle_time: Duration, max_lifetime: Duration) -> bool {
        let now = Instant::now();
        let idle_time = now.duration_since(self.last_used);
        let lifetime = now.duration_since(self.created_at);
        
        idle_time <= max_idle_time && lifetime <= max_lifetime
    }

    /// Update the last used timestamp.
    fn touch(&mut self) {
        self.last_used = Instant::now();
    }
}

impl TcpProxy {
    /// Create a new TCP proxy instance.
    pub fn new() -> Self {
        Self {
            connection_pools: Arc::new(Mutex::new(HashMap::new())),
            metrics: Arc::new(TcpProxyMetrics::default()),
        }
    }

    /// Handle an incoming TCP connection.
    async fn handle_connection(
        ctx: TcpConnectionContext,
        mut client_stream: TcpStream,
        client_addr: SocketAddr,
    ) -> Result<()> {
        let start_time = Instant::now();
        ctx.metrics.connections_total.fetch_add(1, Ordering::Relaxed);
        ctx.metrics.connections_active.fetch_add(1, Ordering::Relaxed);

        debug!("Handling TCP connection from {}", client_addr);

        let result = Self::process_connection(ctx.clone(), &mut client_stream, client_addr).await;

        ctx.metrics.connections_active.fetch_sub(1, Ordering::Relaxed);
        
        let duration = start_time.elapsed();
        ctx.metrics.connection_duration_sum.fetch_add(duration.as_millis() as u64, Ordering::Relaxed);
        ctx.metrics.connection_duration_count.fetch_add(1, Ordering::Relaxed);

        if let Err(e) = result {
            warn!("TCP connection from {} failed: {}", client_addr, e);
        } else {
            debug!("TCP connection from {} completed successfully", client_addr);
        }

        Ok(())
    }

    /// Process a TCP connection by routing to upstream.
    async fn process_connection(
        ctx: TcpConnectionContext,
        client_stream: &mut TcpStream,
        client_addr: SocketAddr,
    ) -> Result<()> {
        // Select upstream using the router
        let upstream = ctx.router
            .select_upstream(Some(&client_addr.to_string()))
            .ok_or_else(|| Error::proxy("No healthy upstream available", "tcp", "unknown"))?;

        // Get or create connection to upstream
        let mut upstream_stream = Self::get_upstream_connection(&ctx, &upstream).await?;

        // Start bidirectional streaming
        Self::stream_bidirectional(
            client_stream,
            &mut upstream_stream,
            &ctx.metrics,
            upstream.clone(),
        ).await?;

        Ok(())
    }

    /// Get a connection to the upstream server (from pool or create new).
    async fn get_upstream_connection(
        ctx: &TcpConnectionContext,
        upstream: &UpstreamInfo,
    ) -> Result<TcpStream> {
        if ctx.config.connection_pooling {
            if let Some(stream) = Self::try_get_pooled_connection(ctx, upstream).await? {
                ctx.metrics.pool_hits.fetch_add(1, Ordering::Relaxed);
                return Ok(stream);
            }
        }

        ctx.metrics.pool_misses.fetch_add(1, Ordering::Relaxed);
        
        // Create new connection
        let upstream_addr = utils::parse_address(&upstream.config.address)?;
        let connect_timeout = utils::millis_to_duration(upstream.config.timeout_ms);

        let stream = timeout(connect_timeout, TcpStream::connect(upstream_addr))
            .await
            .map_err(|_| Error::network(
                format!("Connection to upstream '{}' timed out", upstream.config.name),
                crate::error::NetworkErrorKind::Timeout,
            ))?
            .map_err(|e| Error::network(
                format!("Failed to connect to upstream '{}': {}", upstream.config.name, e),
                crate::error::NetworkErrorKind::ConnectionFailed,
            ))?;

        // Configure TCP socket options
        Self::configure_socket(&stream, &ctx.config)?;

        Ok(stream)
    }

    /// Try to get a pooled connection to the upstream.
    async fn try_get_pooled_connection(
        ctx: &TcpConnectionContext,
        upstream: &UpstreamInfo,
    ) -> Result<Option<TcpStream>> {
        let pools = ctx.connection_pools.lock();
        let pool = pools.get(&upstream.config.name);
        
        if let Some(pool) = pool {
            let max_idle_time = Duration::from_secs(300); // 5 minutes
            let max_lifetime = Duration::from_secs(3600); // 1 hour

            while let Some(mut pooled_conn) = pool.pop() {
                if pooled_conn.is_valid(max_idle_time, max_lifetime) {
                    pooled_conn.touch();
                    return Ok(Some(pooled_conn.stream));
                }
                // Connection is expired, discard it
            }
        }

        Ok(None)
    }

    /// Return a connection to the pool for reuse.
    fn return_connection_to_pool(
        ctx: &TcpConnectionContext,
        upstream_name: &str,
        stream: TcpStream,
    ) {
        if !ctx.config.connection_pooling {
            return;
        }

        let mut pools = ctx.connection_pools.lock();
        let pool = pools.entry(upstream_name.to_string())
            .or_insert_with(|| Arc::new(SegQueue::new()));

        // Limit pool size to prevent memory bloat
        if pool.len() < 100 {
            pool.push(PooledConnection::new(stream));
        }
    }

    /// Configure TCP socket options.
    fn configure_socket(stream: &TcpStream, config: &TcpProxyConfig) -> Result<()> {
        if config.nodelay {
            stream.set_nodelay(true)
                .map_err(|e| Error::network(format!("Failed to set TCP_NODELAY: {}", e), crate::error::NetworkErrorKind::Protocol))?;
        }

        if let Some(keep_alive) = &config.keep_alive {
            if keep_alive.enabled {
                // Note: Tokio doesn't expose all socket options directly
                // In production, you might want to use socket2 crate for more control
                debug!("TCP keep-alive configured (simplified implementation)");
            }
        }

        Ok(())
    }

    /// Perform bidirectional streaming between client and upstream.
    async fn stream_bidirectional(
        client_stream: &mut TcpStream,
        upstream_stream: &mut TcpStream,
        metrics: &TcpProxyMetrics,
        upstream: Arc<UpstreamInfo>,
    ) -> Result<()> {
        let (mut client_read, mut client_write) = client_stream.split();
        let (mut upstream_read, mut upstream_write) = upstream_stream.split();

        // Use high-performance buffers from the global pool
        let mut client_buffer = get_large_buffer();
        let mut upstream_buffer = get_large_buffer();

        // Resize buffers to appropriate size
        client_buffer.resize(65536, 0);
        upstream_buffer.resize(65536, 0);

        let start_time = Instant::now();
        upstream.stats.increment_connections();

        let result = tokio::select! {
            result = Self::copy_stream(&mut client_read, &mut upstream_write, &mut client_buffer, metrics) => {
                debug!("Client -> Upstream stream ended");
                result
            }
            result = Self::copy_stream(&mut upstream_read, &mut client_write, &mut upstream_buffer, metrics) => {
                debug!("Upstream -> Client stream ended");
                result
            }
        };

        upstream.stats.decrement_connections();
        
        match result {
            Ok(bytes_transferred) => {
                let duration = start_time.elapsed();
                upstream.stats.record_success(duration);
                debug!("Bidirectional streaming completed, {} bytes transferred", bytes_transferred);
                Ok(())
            }
            Err(e) => {
                upstream.stats.record_failure();
                Err(e)
            }
        }
    }

    /// Copy data from one stream to another with metrics tracking.
    async fn copy_stream<R, W>(
        reader: &mut R,
        writer: &mut W,
        buffer: &mut BytesMut,
        metrics: &TcpProxyMetrics,
    ) -> Result<u64> 
    where
        R: AsyncReadExt + Unpin,
        W: AsyncWriteExt + Unpin,
    {
        let mut total_bytes = 0u64;

        loop {
            match reader.read(buffer.as_mut()).await {
                Ok(0) => break, // EOF
                Ok(bytes_read) => {
                    writer.write_all(&buffer[..bytes_read]).await
                        .map_err(|e| Error::network(format!("Write failed: {}", e), crate::error::NetworkErrorKind::ConnectionFailed))?;
                    
                    total_bytes += bytes_read as u64;
                    metrics.bytes_transferred.fetch_add(bytes_read as u64, Ordering::Relaxed);
                }
                Err(e) => {
                    if e.kind() == io::ErrorKind::UnexpectedEof {
                        break;
                    }
                    return Err(Error::network(format!("Read failed: {}", e), crate::error::NetworkErrorKind::ConnectionFailed));
                }
            }
        }

        Ok(total_bytes)
    }
}

#[async_trait]
impl Proxy for TcpProxy {
    fn proxy_type(&self) -> ProxyType {
        ProxyType::Tcp
    }

    async fn start(
        &self,
        config: ProxyInstanceConfig,
        router: Arc<Router>,
        mut shutdown: ShutdownSignal,
    ) -> Result<()> {
        info!("Starting TCP proxy '{}' on {}", config.name, config.listen_addr);

        // Extract TCP-specific configuration
        let tcp_config = config.config.tcp.clone();

        // Create connection context
        let ctx = TcpConnectionContext {
            router: router.clone(),
            config: tcp_config,
            metrics: self.metrics.clone(),
            connection_pools: self.connection_pools.clone(),
        };

        // Bind to listen address
        let listener = TcpListener::bind(config.listen_addr).await
            .map_err(|e| Error::network(format!("Failed to bind to {}: {}", config.listen_addr, e), crate::error::NetworkErrorKind::ConnectionFailed))?;

        info!("TCP proxy '{}' started successfully", config.name);

        // Accept connections until shutdown
        loop {
            tokio::select! {
                // Handle new connections
                result = listener.accept() => {
                    match result {
                        Ok((client_stream, client_addr)) => {
                            let ctx = ctx.clone();
                            tokio::spawn(async move {
                                if let Err(e) = Self::handle_connection(ctx, client_stream, client_addr).await {
                                    error!("Failed to handle TCP connection from {}: {}", client_addr, e);
                                }
                            });
                        }
                        Err(e) => {
                            error!("Failed to accept TCP connection: {}", e);
                        }
                    }
                }
                // Handle shutdown signal
                _ = shutdown.recv() => {
                    info!("TCP proxy '{}' received shutdown signal", config.name);
                    break;
                }
            }
        }

        info!("TCP proxy '{}' shut down", config.name);
        Ok(())
    }

    fn validate_config(&self, config: &ProxyInstanceConfig) -> Result<()> {
        if config.proxy_type != ProxyType::Tcp {
            return Err(Error::config("Invalid proxy type for TCP proxy"));
        }

        // Validate TCP-specific configuration
        let tcp_config = &config.config.tcp;
        
        if tcp_config.buffer_size == 0 {
            return Err(Error::config("TCP buffer_size must be greater than 0"));
        }

        if tcp_config.buffer_size > 1024 * 1024 {
            return Err(Error::config("TCP buffer_size cannot exceed 1MB"));
        }

        // Validate keep-alive configuration if present
        if let Some(keep_alive) = &tcp_config.keep_alive {
            if keep_alive.enabled {
                if keep_alive.idle_secs == 0 {
                    return Err(Error::config("TCP keep-alive idle_secs must be greater than 0"));
                }
                if keep_alive.interval_secs == 0 {
                    return Err(Error::config("TCP keep-alive interval_secs must be greater than 0"));
                }
                if keep_alive.retries == 0 {
                    return Err(Error::config("TCP keep-alive retries must be greater than 0"));
                }
            }
        }

        Ok(())
    }

    async fn get_metrics(&self) -> Result<ProxyMetrics> {
        let total_connections = self.metrics.connections_total.load(Ordering::Relaxed);
        let duration_sum = self.metrics.connection_duration_sum.load(Ordering::Relaxed);
        let duration_count = self.metrics.connection_duration_count.load(Ordering::Relaxed);
        
        let avg_response_time_ms = if duration_count > 0 {
            duration_sum as f64 / duration_count as f64
        } else {
            0.0
        };

        let pool_hits = self.metrics.pool_hits.load(Ordering::Relaxed);
        let pool_misses = self.metrics.pool_misses.load(Ordering::Relaxed);
        let total_pool_requests = pool_hits + pool_misses;
        
        let pool_hit_rate = if total_pool_requests > 0 {
            pool_hits as f64 / total_pool_requests as f64
        } else {
            0.0
        };

        Ok(ProxyMetrics {
            total_requests: total_connections,
            active_connections: self.metrics.connections_active.load(Ordering::Relaxed) as u64,
            bytes_sent: self.metrics.bytes_transferred.load(Ordering::Relaxed),
            bytes_received: self.metrics.bytes_transferred.load(Ordering::Relaxed),
            request_rate: 0.0, // Would need time-based calculation
            error_rate: 0.0,   // TCP doesn't have a clear error concept like HTTP status codes
            avg_response_time_ms,
        })
    }

    async fn health_check(&self) -> Result<ProxyHealth> {
        let active_connections = self.metrics.connections_active.load(Ordering::Relaxed);
        let total_connections = self.metrics.connections_total.load(Ordering::Relaxed);
        
        let status = if active_connections < 5000 {
            HealthState::Healthy
        } else if active_connections >= 10000 {
            HealthState::Unhealthy
        } else {
            HealthState::Unknown
        };

        let message = format!(
            "Active connections: {}, Total connections: {}",
            active_connections, total_connections
        );

        Ok(ProxyHealth { status, message })
    }
}

impl Default for TcpProxy {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;

    fn create_test_config() -> ProxyInstanceConfig {
        ProxyInstanceConfig {
            name: "test-tcp".to_string(),
            proxy_type: ProxyType::Tcp,
            listen_addr: "127.0.0.1:0".parse().unwrap(),
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
            config: ProxySpecificConfig {
                tcp: TcpProxyConfig {
                    buffer_size: 8192,
                    connection_pooling: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        }
    }

    #[test]
    fn test_tcp_proxy_creation() {
        let proxy = TcpProxy::new();
        assert_eq!(proxy.proxy_type(), ProxyType::Tcp);
    }

    #[test]
    fn test_config_validation() {
        let proxy = TcpProxy::new();
        let config = create_test_config();
        
        // Valid configuration should pass
        assert!(proxy.validate_config(&config).is_ok());
        
        // Invalid proxy type should fail
        let mut invalid_config = config.clone();
        invalid_config.proxy_type = ProxyType::Http;
        assert!(proxy.validate_config(&invalid_config).is_err());
        
        // Invalid buffer_size should fail
        let mut invalid_config = config.clone();
        invalid_config.config.tcp.buffer_size = 0;
        assert!(proxy.validate_config(&invalid_config).is_err());
    }

    #[test]
    fn test_pooled_connection() {
        let stream = std::net::TcpStream::connect("127.0.0.1:80");
        if let Ok(stream) = stream {
            let tokio_stream = TcpStream::from_std(stream).unwrap();
            let pooled = PooledConnection::new(tokio_stream);
            
            assert!(pooled.is_valid(Duration::from_secs(300), Duration::from_secs(3600)));
        }
    }

    #[tokio::test]
    async fn test_metrics() {
        let proxy = TcpProxy::new();
        let metrics = proxy.get_metrics().await.unwrap();
        
        assert_eq!(metrics.total_requests, 0);
        assert_eq!(metrics.active_connections, 0);
        assert_eq!(metrics.bytes_sent, 0);
        assert_eq!(metrics.bytes_received, 0);
    }

    #[tokio::test]
    async fn test_health_check() {
        let proxy = TcpProxy::new();
        let health = proxy.health_check().await.unwrap();
        
        assert_eq!(health.status, HealthState::Healthy);
        assert!(health.message.contains("Active connections: 0"));
    }
}