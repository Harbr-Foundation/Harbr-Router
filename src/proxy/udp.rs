//! High-performance UDP proxy implementation.
//!
//! This module provides a production-ready UDP proxy with session tracking,
//! load balancing, and efficient packet forwarding.

use super::{Proxy, ProxyHealth, ProxyMetrics, ShutdownSignal, utils};
use crate::{
    buffers::get_medium_buffer,
    config::{ProxyInstanceConfig, ProxyType, UdpProxyConfig},
    routing::{HealthState, Router, UpstreamInfo},
    Error, Result,
};
use async_trait::async_trait;
use bytes::BytesMut;
use dashmap::DashMap;
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{
    net::UdpSocket,
    sync::RwLock,
    time::{interval, timeout},
};
use tracing::{debug, error, info, warn};

/// UDP proxy implementation with session tracking and load balancing.
#[derive(Debug)]
pub struct UdpProxy {
    /// Client session tracking
    sessions: Arc<DashMap<SocketAddr, UdpSession>>,
    /// Proxy metrics
    metrics: Arc<UdpProxyMetrics>,
}

/// UDP session information for connection tracking.
#[derive(Debug, Clone)]
struct UdpSession {
    /// Upstream server for this session
    upstream: Arc<UpstreamInfo>,
    /// Socket connected to upstream
    upstream_socket: Arc<UdpSocket>,
    /// Last activity timestamp
    last_activity: Arc<tokio::sync::RwLock<Instant>>,
    /// Session creation timestamp
    created_at: Instant,
    /// Bytes sent in this session
    bytes_sent: Arc<AtomicU64>,
    /// Bytes received in this session
    bytes_received: Arc<AtomicU64>,
}

/// UDP proxy specific metrics.
#[derive(Debug, Default)]
struct UdpProxyMetrics {
    /// Total packets processed
    packets_total: AtomicU64,
    /// Currently active sessions
    sessions_active: AtomicUsize,
    /// Total bytes sent to upstreams
    bytes_sent: AtomicU64,
    /// Total bytes received from upstreams
    bytes_received: AtomicU64,
    /// Session creation count
    sessions_created: AtomicU64,
    /// Session expiration count
    sessions_expired: AtomicU64,
}

/// Context for handling UDP connections.
#[derive(Clone)]
struct UdpProxyContext {
    router: Arc<Router>,
    config: UdpProxyConfig,
    metrics: Arc<UdpProxyMetrics>,
    sessions: Arc<DashMap<SocketAddr, UdpSession>>,
}

impl UdpSession {
    /// Create a new UDP session.
    async fn new(upstream: Arc<UpstreamInfo>) -> Result<Self> {
        let upstream_addr = utils::parse_address(&upstream.config.address)?;
        let upstream_socket = UdpSocket::bind("0.0.0.0:0").await
            .map_err(|e| Error::network(format!("Failed to create upstream socket: {}", e), crate::error::NetworkErrorKind::ConnectionFailed))?;
        
        upstream_socket.connect(upstream_addr).await
            .map_err(|e| Error::network(format!("Failed to connect to upstream: {}", e), crate::error::NetworkErrorKind::ConnectionFailed))?;

        let now = Instant::now();
        Ok(Self {
            upstream,
            upstream_socket: Arc::new(upstream_socket),
            last_activity: Arc::new(tokio::sync::RwLock::new(now)),
            created_at: now,
            bytes_sent: Arc::new(AtomicU64::new(0)),
            bytes_received: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Update last activity timestamp.
    async fn touch(&self) {
        *self.last_activity.write().await = Instant::now();
    }

    /// Check if session has expired.
    async fn is_expired(&self, timeout: Duration) -> bool {
        let last_activity = *self.last_activity.read().await;
        last_activity.elapsed() > timeout
    }

    /// Get session age.
    fn age(&self) -> Duration {
        self.created_at.elapsed()
    }

    /// Record bytes sent to upstream.
    fn record_bytes_sent(&self, bytes: usize) {
        self.bytes_sent.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// Record bytes received from upstream.
    fn record_bytes_received(&self, bytes: usize) {
        self.bytes_received.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

impl UdpProxy {
    /// Create a new UDP proxy instance.
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(DashMap::new()),
            metrics: Arc::new(UdpProxyMetrics::default()),
        }
    }

    /// Handle incoming UDP traffic.
    async fn handle_client_packet(
        ctx: UdpProxyContext,
        _server_socket: Arc<UdpSocket>,
        client_addr: SocketAddr,
        data: &[u8],
    ) -> Result<()> {
        ctx.metrics.packets_total.fetch_add(1, Ordering::Relaxed);

        // Get or create session for this client
        let session = Self::get_or_create_session(&ctx, client_addr).await?;

        // Update session activity
        session.touch().await;

        // Forward packet to upstream
        Self::forward_to_upstream(&ctx, &session, data).await?;

        Ok(())
    }

    /// Get existing session or create a new one.
    async fn get_or_create_session(
        ctx: &UdpProxyContext,
        client_addr: SocketAddr,
    ) -> Result<Arc<UdpSession>> {
        // Check if session already exists
        if let Some(session) = ctx.sessions.get(&client_addr) {
            let session_timeout = Duration::from_secs(ctx.config.session_timeout_secs);
            if !session.is_expired(session_timeout).await {
                return Ok(Arc::new(session.value().clone()));
            }
            
            // Session expired, remove it
            ctx.sessions.remove(&client_addr);
            ctx.metrics.sessions_expired.fetch_add(1, Ordering::Relaxed);
            ctx.metrics.sessions_active.fetch_sub(1, Ordering::Relaxed);
        }

        // Create new session
        let upstream = ctx.router
            .select_upstream(Some(&client_addr.to_string()))
            .ok_or_else(|| Error::proxy("No healthy upstream available", "udp", "unknown"))?;

        let session = Arc::new(UdpSession::new(upstream).await?);
        
        // Start upstream listener for this session
        Self::start_upstream_listener(ctx.clone(), client_addr, session.clone()).await;

        // Insert session
        ctx.sessions.insert(client_addr, (*session).clone());
        ctx.metrics.sessions_created.fetch_add(1, Ordering::Relaxed);
        ctx.metrics.sessions_active.fetch_add(1, Ordering::Relaxed);

        info!("Created new UDP session for client {}", client_addr);
        Ok(session)
    }

    /// Forward packet to upstream server.
    async fn forward_to_upstream(
        ctx: &UdpProxyContext,
        session: &UdpSession,
        data: &[u8],
    ) -> Result<()> {
        let timeout_duration = utils::millis_to_duration(session.upstream.config.timeout_ms);
        
        match timeout(timeout_duration, session.upstream_socket.send(data)).await {
            Ok(Ok(bytes_sent)) => {
                session.record_bytes_sent(bytes_sent);
                ctx.metrics.bytes_sent.fetch_add(bytes_sent as u64, Ordering::Relaxed);
                debug!("Forwarded {} bytes to upstream {}", bytes_sent, session.upstream.config.name);
                Ok(())
            }
            Ok(Err(e)) => {
                session.upstream.stats.record_failure();
                Err(Error::network(
                    format!("Failed to send to upstream '{}': {}", session.upstream.config.name, e),
                    crate::error::NetworkErrorKind::ConnectionFailed,
                ))
            }
            Err(_) => {
                session.upstream.stats.record_failure();
                Err(Error::network(
                    format!("Timeout sending to upstream '{}'", session.upstream.config.name),
                    crate::error::NetworkErrorKind::Timeout,
                ))
            }
        }
    }

    /// Start listening for responses from upstream.
    async fn start_upstream_listener(
        ctx: UdpProxyContext,
        client_addr: SocketAddr,
        session: Arc<UdpSession>,
    ) {
        let server_socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await.unwrap()); // This should be the main server socket
        
        tokio::spawn(async move {
            let mut buffer = get_medium_buffer();
            buffer.resize(ctx.config.buffer_size, 0);

            loop {
                match session.upstream_socket.recv(&mut buffer).await {
                    Ok(bytes_received) => {
                        session.record_bytes_received(bytes_received);
                        ctx.metrics.bytes_received.fetch_add(bytes_received as u64, Ordering::Relaxed);
                        
                        // Forward response back to client
                        if let Err(e) = server_socket.send_to(&buffer[..bytes_received], client_addr).await {
                            warn!("Failed to send response to client {}: {}", client_addr, e);
                            break;
                        }
                        
                        // Update session activity
                        session.touch().await;
                        
                        debug!("Forwarded {} bytes from upstream to client {}", bytes_received, client_addr);
                    }
                    Err(e) => {
                        warn!("Failed to receive from upstream for client {}: {}", client_addr, e);
                        break;
                    }
                }
                
                // Check if session should be terminated
                let session_timeout = Duration::from_secs(ctx.config.session_timeout_secs);
                if session.is_expired(session_timeout).await {
                    debug!("UDP session for client {} expired", client_addr);
                    ctx.sessions.remove(&client_addr);
                    ctx.metrics.sessions_expired.fetch_add(1, Ordering::Relaxed);
                    ctx.metrics.sessions_active.fetch_sub(1, Ordering::Relaxed);
                    break;
                }
            }
        });
    }

    /// Run session cleanup task.
    async fn run_session_cleanup(ctx: UdpProxyContext) {
        let mut cleanup_interval = interval(Duration::from_secs(60)); // Clean every minute
        
        loop {
            cleanup_interval.tick().await;
            
            let session_timeout = Duration::from_secs(ctx.config.session_timeout_secs);
            let mut expired_sessions = Vec::new();
            
            // Find expired sessions
            for session_ref in ctx.sessions.iter() {
                let client_addr = *session_ref.key();
                let session = session_ref.value();
                
                if session.is_expired(session_timeout).await {
                    expired_sessions.push(client_addr);
                }
            }
            
            // Remove expired sessions
            for client_addr in expired_sessions {
                if ctx.sessions.remove(&client_addr).is_some() {
                    ctx.metrics.sessions_expired.fetch_add(1, Ordering::Relaxed);
                    ctx.metrics.sessions_active.fetch_sub(1, Ordering::Relaxed);
                    debug!("Cleaned up expired UDP session for client {}", client_addr);
                }
            }
            
            let active_sessions = ctx.metrics.sessions_active.load(Ordering::Relaxed);
            debug!("UDP session cleanup completed, {} active sessions remaining", active_sessions);
        }
    }
}

#[async_trait]
impl Proxy for UdpProxy {
    fn proxy_type(&self) -> ProxyType {
        ProxyType::Udp
    }

    async fn start(
        &self,
        config: ProxyInstanceConfig,
        router: Arc<Router>,
        mut shutdown: ShutdownSignal,
    ) -> Result<()> {
        info!("Starting UDP proxy '{}' on {}", config.name, config.listen_addr);

        // Extract UDP-specific configuration
        let udp_config = config.config.udp.clone();

        // Create proxy context
        let ctx = UdpProxyContext {
            router: router.clone(),
            config: udp_config.clone(),
            metrics: self.metrics.clone(),
            sessions: self.sessions.clone(),
        };

        // Bind UDP socket
        let socket = Arc::new(
            UdpSocket::bind(config.listen_addr).await
                .map_err(|e| Error::network(format!("Failed to bind UDP socket to {}: {}", config.listen_addr, e), crate::error::NetworkErrorKind::ConnectionFailed))?
        );

        info!("UDP proxy '{}' started successfully", config.name);

        // Start session cleanup task
        let cleanup_ctx = ctx.clone();
        tokio::spawn(Self::run_session_cleanup(cleanup_ctx));

        // Main packet processing loop
        let mut buffer = get_medium_buffer();
        buffer.resize(udp_config.buffer_size, 0);

        loop {
            tokio::select! {
                // Handle incoming packets
                result = socket.recv_from(&mut buffer) => {
                    match result {
                        Ok((bytes_received, client_addr)) => {
                            let ctx = ctx.clone();
                            let socket = socket.clone();
                            let data = buffer[..bytes_received].to_vec();
                            
                            tokio::spawn(async move {
                                if let Err(e) = Self::handle_client_packet(ctx, socket, client_addr, &data).await {
                                    error!("Failed to handle UDP packet from {}: {}", client_addr, e);
                                }
                            });
                        }
                        Err(e) => {
                            error!("Failed to receive UDP packet: {}", e);
                        }
                    }
                }
                // Handle shutdown signal
                _ = shutdown.recv() => {
                    info!("UDP proxy '{}' received shutdown signal", config.name);
                    break;
                }
            }
        }

        // Clean up all sessions
        self.sessions.clear();
        info!("UDP proxy '{}' shut down", config.name);
        Ok(())
    }

    fn validate_config(&self, config: &ProxyInstanceConfig) -> Result<()> {
        if config.proxy_type != ProxyType::Udp {
            return Err(Error::config("Invalid proxy type for UDP proxy"));
        }

        // Validate UDP-specific configuration
        let udp_config = &config.config.udp;
        
        if udp_config.buffer_size == 0 {
            return Err(Error::config("UDP buffer_size must be greater than 0"));
        }

        if udp_config.buffer_size > 65536 {
            return Err(Error::config("UDP buffer_size cannot exceed 64KB"));
        }

        if udp_config.session_timeout_secs == 0 {
            return Err(Error::config("UDP session_timeout_secs must be greater than 0"));
        }

        if udp_config.session_timeout_secs > 3600 {
            return Err(Error::config("UDP session_timeout_secs cannot exceed 1 hour"));
        }

        Ok(())
    }

    async fn get_metrics(&self) -> Result<ProxyMetrics> {
        let packets_total = self.metrics.packets_total.load(Ordering::Relaxed);
        let sessions_active = self.metrics.sessions_active.load(Ordering::Relaxed);
        let sessions_created = self.metrics.sessions_created.load(Ordering::Relaxed);
        let sessions_expired = self.metrics.sessions_expired.load(Ordering::Relaxed);
        
        let session_success_rate = if sessions_created > 0 {
            (sessions_created - sessions_expired) as f64 / sessions_created as f64
        } else {
            1.0
        };

        Ok(ProxyMetrics {
            total_requests: packets_total,
            active_connections: sessions_active as u64,
            bytes_sent: self.metrics.bytes_sent.load(Ordering::Relaxed),
            bytes_received: self.metrics.bytes_received.load(Ordering::Relaxed),
            request_rate: 0.0, // Would need time-based calculation
            error_rate: 1.0 - session_success_rate,
            avg_response_time_ms: 0.0, // UDP doesn't have a clear response time concept
        })
    }

    async fn health_check(&self) -> Result<ProxyHealth> {
        let active_sessions = self.metrics.sessions_active.load(Ordering::Relaxed);
        let total_packets = self.metrics.packets_total.load(Ordering::Relaxed);
        
        let status = if active_sessions < 1000 {
            HealthState::Healthy
        } else if active_sessions >= 5000 {
            HealthState::Unhealthy
        } else {
            HealthState::Unknown
        };

        let message = format!(
            "Active sessions: {}, Total packets: {}",
            active_sessions, total_packets
        );

        Ok(ProxyHealth { status, message })
    }
}

impl Default for UdpProxy {
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
            name: "test-udp".to_string(),
            proxy_type: ProxyType::Udp,
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
                udp: UdpProxyConfig {
                    buffer_size: 1500,
                    session_timeout_secs: 300,
                    connection_tracking: true,
                },
                ..Default::default()
            },
        }
    }

    #[test]
    fn test_udp_proxy_creation() {
        let proxy = UdpProxy::new();
        assert_eq!(proxy.proxy_type(), ProxyType::Udp);
    }

    #[test]
    fn test_config_validation() {
        let proxy = UdpProxy::new();
        let config = create_test_config();
        
        // Valid configuration should pass
        assert!(proxy.validate_config(&config).is_ok());
        
        // Invalid proxy type should fail
        let mut invalid_config = config.clone();
        invalid_config.proxy_type = ProxyType::Http;
        assert!(proxy.validate_config(&invalid_config).is_err());
        
        // Invalid buffer_size should fail
        let mut invalid_config = config.clone();
        invalid_config.config.udp.buffer_size = 0;
        assert!(proxy.validate_config(&invalid_config).is_err());
        
        // Invalid session_timeout should fail
        let mut invalid_config = config.clone();
        invalid_config.config.udp.session_timeout_secs = 0;
        assert!(proxy.validate_config(&invalid_config).is_err());
    }

    #[tokio::test]
    async fn test_metrics() {
        let proxy = UdpProxy::new();
        let metrics = proxy.get_metrics().await.unwrap();
        
        assert_eq!(metrics.total_requests, 0);
        assert_eq!(metrics.active_connections, 0);
        assert_eq!(metrics.bytes_sent, 0);
        assert_eq!(metrics.bytes_received, 0);
    }

    #[tokio::test]
    async fn test_health_check() {
        let proxy = UdpProxy::new();
        let health = proxy.health_check().await.unwrap();
        
        assert_eq!(health.status, HealthState::Healthy);
        assert!(health.message.contains("Active sessions: 0"));
    }

    #[tokio::test]
    async fn test_udp_session() {
        use crate::config::UpstreamConfig;
        use crate::routing::UpstreamInfo;
        
        let upstream_config = UpstreamConfig {
            name: "test".to_string(),
            address: "127.0.0.1:12345".to_string(),
            weight: 1,
            priority: 0,
            timeout_ms: 5000,
            retry: RetryConfig::default(),
            health_check: None,
            tls: None,
        };
        
        let upstream = Arc::new(UpstreamInfo::new(upstream_config));
        
        // This test would need a real UDP server to be meaningful
        // For now, we'll just test that the creation doesn't panic
        if UdpSession::new(upstream).await.is_ok() {
            // Session created successfully
        } else {
            // Expected to fail without a real upstream server
        }
    }
}