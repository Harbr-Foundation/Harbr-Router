//! Comprehensive metrics and observability system.
//!
//! This module provides a production-ready metrics collection and export system
//! with Prometheus integration and structured monitoring.

use crate::{
    config::{MetricsConfig, ProxyType},
    proxy::{ProxyHealth, ProxyMetrics},
    routing::{HealthState, UpstreamInfo},
    Error, Result,
};
use hyper::{
    header::CONTENT_TYPE, service::{make_service_fn, service_fn}, Body, Request, Response, Server, StatusCode,
};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    convert::Infallible,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::broadcast, time::interval};
use tracing::{error, info, warn};

/// Global metrics registry
static METRICS_REGISTRY: once_cell::sync::Lazy<Arc<MetricsRegistry>> = 
    once_cell::sync::Lazy::new(|| Arc::new(MetricsRegistry::new()));

/// Central metrics registry for collecting and exporting metrics.
#[derive(Debug)]
pub struct MetricsRegistry {
    /// System-wide metrics
    system_metrics: Arc<SystemMetrics>,
    /// Per-proxy metrics
    proxy_metrics: RwLock<HashMap<String, ProxyMetrics>>,
    /// Per-upstream metrics
    upstream_metrics: RwLock<HashMap<String, Arc<UpstreamInfo>>>,
    /// Metrics collection start time
    start_time: Instant,
}

/// System-wide metrics.
#[derive(Debug, Default)]
pub struct SystemMetrics {
    /// Total number of proxy instances
    proxy_instances_total: AtomicUsize,
    /// Total number of upstream servers
    upstreams_total: AtomicUsize,
    /// Memory usage metrics
    memory_usage: RwLock<MemoryUsage>,
    /// System uptime in seconds
    uptime_seconds: AtomicU64,
    /// Configuration reload count
    config_reloads: AtomicU64,
}

/// Memory usage information.
#[derive(Debug, Default, Clone, Serialize)]
pub struct MemoryUsage {
    /// RSS memory in bytes
    pub rss_bytes: u64,
    /// Virtual memory in bytes
    pub vsize_bytes: u64,
    /// Heap allocated bytes
    pub heap_bytes: u64,
    /// Buffer pool usage
    pub buffer_pool_bytes: u64,
}

/// Metrics server for exposing metrics via HTTP.
#[derive(Debug)]
pub struct MetricsServer {
    config: MetricsConfig,
    registry: Arc<MetricsRegistry>,
    shutdown_sender: broadcast::Sender<()>,
}

/// Health check information for the entire system.
#[derive(Debug, Clone, Serialize)]
pub struct SystemHealth {
    /// Overall system status
    pub status: HealthState,
    /// System uptime in seconds
    pub uptime_seconds: u64,
    /// Number of healthy proxies
    pub healthy_proxies: usize,
    /// Number of unhealthy proxies
    pub unhealthy_proxies: usize,
    /// Number of healthy upstreams
    pub healthy_upstreams: usize,
    /// Number of unhealthy upstreams
    pub unhealthy_upstreams: usize,
    /// Last health check timestamp
    pub timestamp: u64,
    /// Additional status message
    pub message: String,
}

/// Detailed metrics summary for export.
#[derive(Debug, Clone, Serialize)]
pub struct MetricsSummary {
    /// System metrics
    pub system: SystemMetricsSnapshot,
    /// Proxy metrics by instance name
    pub proxies: HashMap<String, ProxyMetrics>,
    /// Upstream metrics by name
    pub upstreams: HashMap<String, UpstreamMetricsSnapshot>,
    /// Collection timestamp
    pub timestamp: u64,
}

/// Snapshot of system metrics.
#[derive(Debug, Clone, Serialize)]
pub struct SystemMetricsSnapshot {
    /// Total proxy instances
    pub proxy_instances_total: usize,
    /// Total upstream servers
    pub upstreams_total: usize,
    /// System uptime in seconds
    pub uptime_seconds: u64,
    /// Configuration reload count
    pub config_reloads: u64,
    /// Memory usage information
    pub memory_usage: MemoryUsage,
}

/// Snapshot of upstream metrics.
#[derive(Debug, Clone, Serialize)]
pub struct UpstreamMetricsSnapshot {
    /// Upstream name
    pub name: String,
    /// Current health state
    pub health_state: HealthState,
    /// Total requests sent
    pub requests_sent: u64,
    /// Successful responses
    pub responses_received: u64,
    /// Failed requests
    pub failures: u64,
    /// Active connections
    pub active_connections: usize,
    /// Average response time in milliseconds
    pub avg_response_time_ms: f64,
    /// Success rate (0.0 to 1.0)
    pub success_rate: f64,
    /// Health score (0.0 to 1.0)
    pub health_score: f64,
}

impl MetricsRegistry {
    /// Create a new metrics registry.
    fn new() -> Self {
        Self {
            system_metrics: Arc::new(SystemMetrics::default()),
            proxy_metrics: RwLock::new(HashMap::new()),
            upstream_metrics: RwLock::new(HashMap::new()),
            start_time: Instant::now(),
        }
    }

    /// Update system uptime.
    pub fn update_uptime(&self) {
        let uptime = self.start_time.elapsed().as_secs();
        self.system_metrics.uptime_seconds.store(uptime, Ordering::Relaxed);
    }

    /// Record configuration reload.
    pub fn record_config_reload(&self) {
        self.system_metrics.config_reloads.fetch_add(1, Ordering::Relaxed);
    }

    /// Update proxy metrics.
    pub fn update_proxy_metrics(&self, instance_name: String, metrics: ProxyMetrics) {
        self.proxy_metrics.write().insert(instance_name, metrics);
    }

    /// Add upstream for tracking.
    pub fn add_upstream(&self, upstream: Arc<UpstreamInfo>) {
        let name = upstream.config.name.clone();
        self.upstream_metrics.write().insert(name, upstream);
        self.system_metrics.upstreams_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Remove upstream from tracking.
    pub fn remove_upstream(&self, name: &str) {
        if self.upstream_metrics.write().remove(name).is_some() {
            self.system_metrics.upstreams_total.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Set proxy instance count.
    pub fn set_proxy_instances_count(&self, count: usize) {
        self.system_metrics.proxy_instances_total.store(count, Ordering::Relaxed);
    }

    /// Update memory usage information.
    pub fn update_memory_usage(&self, usage: MemoryUsage) {
        *self.system_metrics.memory_usage.write() = usage;
    }

    /// Get comprehensive metrics summary.
    pub fn get_metrics_summary(&self) -> MetricsSummary {
        self.update_uptime();

        let system = SystemMetricsSnapshot {
            proxy_instances_total: self.system_metrics.proxy_instances_total.load(Ordering::Relaxed),
            upstreams_total: self.system_metrics.upstreams_total.load(Ordering::Relaxed),
            uptime_seconds: self.system_metrics.uptime_seconds.load(Ordering::Relaxed),
            config_reloads: self.system_metrics.config_reloads.load(Ordering::Relaxed),
            memory_usage: self.system_metrics.memory_usage.read().clone(),
        };

        let proxies = self.proxy_metrics.read().clone();
        
        let upstreams = self.upstream_metrics.read()
            .iter()
            .map(|(name, upstream)| {
                let snapshot = UpstreamMetricsSnapshot {
                    name: name.clone(),
                    health_state: *upstream.health_state.read(),
                    requests_sent: upstream.stats.requests_sent.load(Ordering::Relaxed),
                    responses_received: upstream.stats.responses_received.load(Ordering::Relaxed),
                    failures: upstream.stats.failures.load(Ordering::Relaxed),
                    active_connections: upstream.stats.active_connections.load(Ordering::Relaxed),
                    avg_response_time_ms: upstream.stats.avg_response_time_ms(),
                    success_rate: upstream.stats.success_rate(),
                    health_score: upstream.health_score(),
                };
                (name.clone(), snapshot)
            })
            .collect();

        MetricsSummary {
            system,
            proxies,
            upstreams,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        }
    }

    /// Get system health status.
    pub fn get_system_health(&self, proxy_healths: HashMap<String, ProxyHealth>) -> SystemHealth {
        self.update_uptime();

        let mut healthy_proxies = 0;
        let mut unhealthy_proxies = 0;

        for health in proxy_healths.values() {
            match health.status {
                HealthState::Healthy => healthy_proxies += 1,
                HealthState::Unhealthy => unhealthy_proxies += 1,
                HealthState::Unknown => {} // Don't count unknown
            }
        }

        let mut healthy_upstreams = 0;
        let mut unhealthy_upstreams = 0;

        for upstream in self.upstream_metrics.read().values() {
            match *upstream.health_state.read() {
                HealthState::Healthy => healthy_upstreams += 1,
                HealthState::Unhealthy => unhealthy_upstreams += 1,
                HealthState::Unknown => {} // Don't count unknown
            }
        }

        let overall_status = if unhealthy_proxies > 0 || unhealthy_upstreams > 0 {
            if healthy_proxies == 0 {
                HealthState::Unhealthy
            } else {
                HealthState::Unknown // Degraded
            }
        } else if healthy_proxies > 0 {
            HealthState::Healthy
        } else {
            HealthState::Unknown
        };

        let message = match overall_status {
            HealthState::Healthy => "All systems operational".to_string(),
            HealthState::Unhealthy => format!(
                "System degraded: {} unhealthy proxies, {} unhealthy upstreams",
                unhealthy_proxies, unhealthy_upstreams
            ),
            HealthState::Unknown => "System status unknown or starting up".to_string(),
        };

        SystemHealth {
            status: overall_status,
            uptime_seconds: self.system_metrics.uptime_seconds.load(Ordering::Relaxed),
            healthy_proxies,
            unhealthy_proxies,
            healthy_upstreams,
            unhealthy_upstreams,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            message,
        }
    }

    /// Export metrics in Prometheus format.
    pub fn export_prometheus(&self) -> String {
        let summary = self.get_metrics_summary();
        let mut output = String::new();

        // System metrics
        output.push_str(&format!(
            "# HELP harbr_proxy_instances_total Total number of proxy instances\n\
             # TYPE harbr_proxy_instances_total gauge\n\
             harbr_proxy_instances_total {}\n\n",
            summary.system.proxy_instances_total
        ));

        output.push_str(&format!(
            "# HELP harbr_upstreams_total Total number of upstream servers\n\
             # TYPE harbr_upstreams_total gauge\n\
             harbr_upstreams_total {}\n\n",
            summary.system.upstreams_total
        ));

        output.push_str(&format!(
            "# HELP harbr_uptime_seconds System uptime in seconds\n\
             # TYPE harbr_uptime_seconds gauge\n\
             harbr_uptime_seconds {}\n\n",
            summary.system.uptime_seconds
        ));

        // Memory metrics
        output.push_str(&format!(
            "# HELP harbr_memory_rss_bytes RSS memory usage in bytes\n\
             # TYPE harbr_memory_rss_bytes gauge\n\
             harbr_memory_rss_bytes {}\n\n",
            summary.system.memory_usage.rss_bytes
        ));

        // Proxy metrics
        for (instance_name, proxy_metrics) in &summary.proxies {
            output.push_str(&format!(
                "# HELP harbr_proxy_requests_total Total requests handled by proxy\n\
                 # TYPE harbr_proxy_requests_total counter\n\
                 harbr_proxy_requests_total{{instance=\"{}\"}} {}\n\n",
                instance_name, proxy_metrics.total_requests
            ));

            output.push_str(&format!(
                "# HELP harbr_proxy_connections_active Active connections for proxy\n\
                 # TYPE harbr_proxy_connections_active gauge\n\
                 harbr_proxy_connections_active{{instance=\"{}\"}} {}\n\n",
                instance_name, proxy_metrics.active_connections
            ));

            output.push_str(&format!(
                "# HELP harbr_proxy_bytes_sent_total Total bytes sent by proxy\n\
                 # TYPE harbr_proxy_bytes_sent_total counter\n\
                 harbr_proxy_bytes_sent_total{{instance=\"{}\"}} {}\n\n",
                instance_name, proxy_metrics.bytes_sent
            ));

            output.push_str(&format!(
                "# HELP harbr_proxy_bytes_received_total Total bytes received by proxy\n\
                 # TYPE harbr_proxy_bytes_received_total counter\n\
                 harbr_proxy_bytes_received_total{{instance=\"{}\"}} {}\n\n",
                instance_name, proxy_metrics.bytes_received
            ));
        }

        // Upstream metrics
        for (upstream_name, upstream_metrics) in &summary.upstreams {
            let health_value = match upstream_metrics.health_state {
                HealthState::Healthy => 1.0,
                HealthState::Unknown => 0.5,
                HealthState::Unhealthy => 0.0,
            };

            output.push_str(&format!(
                "# HELP harbr_upstream_health Upstream health status (1=healthy, 0.5=unknown, 0=unhealthy)\n\
                 # TYPE harbr_upstream_health gauge\n\
                 harbr_upstream_health{{upstream=\"{}\"}} {}\n\n",
                upstream_name, health_value
            ));

            output.push_str(&format!(
                "# HELP harbr_upstream_requests_total Total requests sent to upstream\n\
                 # TYPE harbr_upstream_requests_total counter\n\
                 harbr_upstream_requests_total{{upstream=\"{}\"}} {}\n\n",
                upstream_name, upstream_metrics.requests_sent
            ));

            output.push_str(&format!(
                "# HELP harbr_upstream_response_time_ms Average response time in milliseconds\n\
                 # TYPE harbr_upstream_response_time_ms gauge\n\
                 harbr_upstream_response_time_ms{{upstream=\"{}\"}} {}\n\n",
                upstream_name, upstream_metrics.avg_response_time_ms
            ));

            output.push_str(&format!(
                "# HELP harbr_upstream_success_rate Success rate (0.0 to 1.0)\n\
                 # TYPE harbr_upstream_success_rate gauge\n\
                 harbr_upstream_success_rate{{upstream=\"{}\"}} {}\n\n",
                upstream_name, upstream_metrics.success_rate
            ));
        }

        output
    }
}

impl MetricsServer {
    /// Create a new metrics server.
    pub fn new(config: MetricsConfig) -> Self {
        let (shutdown_sender, _) = broadcast::channel(16);
        
        Self {
            config,
            registry: METRICS_REGISTRY.clone(),
            shutdown_sender,
        }
    }

    /// Start the metrics server.
    pub async fn start(&self) -> Result<()> {
        if !self.config.enabled {
            info!("Metrics collection is disabled");
            return Ok(());
        }

        info!("Starting metrics server on {}{}", self.config.listen_addr, self.config.path);

        // Start periodic memory collection
        self.start_memory_collection().await;

        // Create service
        let registry = self.registry.clone();
        let metrics_path = self.config.path.clone();
        
        let make_service = make_service_fn(move |_conn| {
            let registry = registry.clone();
            let metrics_path = metrics_path.clone();
            
            async move {
                Ok::<_, Infallible>(service_fn(move |req| {
                    Self::handle_request(registry.clone(), metrics_path.clone(), req)
                }))
            }
        });

        // Start server
        let mut shutdown = self.shutdown_sender.subscribe();
        let server = Server::bind(&self.config.listen_addr)
            .serve(make_service)
            .with_graceful_shutdown(async move {
                let _ = shutdown.recv().await;
                info!("Metrics server received shutdown signal");
            });

        info!("Metrics server started successfully");

        if let Err(e) = server.await {
            return Err(Error::internal(format!("Metrics server error: {}", e)));
        }

        info!("Metrics server shut down");
        Ok(())
    }

    /// Handle HTTP requests to the metrics server.
    async fn handle_request(
        registry: Arc<MetricsRegistry>,
        metrics_path: String,
        req: Request<Body>,
    ) -> std::result::Result<Response<Body>, Infallible> {
        let path = req.uri().path();

        match path {
            path if path == metrics_path => {
                // Serve Prometheus metrics
                let metrics = registry.export_prometheus();
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header(CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")
                    .body(Body::from(metrics))
                    .unwrap())
            }
            "/health" => {
                // Serve health check (simplified version)
                let health = registry.get_system_health(HashMap::new());
                let health_json = serde_json::to_string(&health).unwrap();
                
                let status = match health.status {
                    HealthState::Healthy => StatusCode::OK,
                    HealthState::Unknown => StatusCode::OK, // Consider as OK for basic health check
                    HealthState::Unhealthy => StatusCode::SERVICE_UNAVAILABLE,
                };
                
                Ok(Response::builder()
                    .status(status)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(health_json))
                    .unwrap())
            }
            _ => {
                // Not found
                Ok(Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::from("Not Found"))
                    .unwrap())
            }
        }
    }

    /// Start periodic memory collection.
    async fn start_memory_collection(&self) {
        let registry = self.registry.clone();
        let interval_secs = self.config.interval_secs;
        
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(interval_secs));
            
            loop {
                ticker.tick().await;
                
                // Collect memory usage (simplified implementation)
                let memory_usage = Self::collect_memory_usage();
                registry.update_memory_usage(memory_usage);
            }
        });
    }

    /// Collect current memory usage.
    fn collect_memory_usage() -> MemoryUsage {
        // This is a simplified implementation
        // In production, you'd want to use system-specific APIs or crates like `sysinfo`
        MemoryUsage {
            rss_bytes: 0, // Would get from /proc/self/status on Linux
            vsize_bytes: 0,
            heap_bytes: 0, // Would get from allocator if available
            buffer_pool_bytes: 0, // Would get from buffer pool stats
        }
    }

    /// Shutdown the metrics server.
    pub fn shutdown(&self) -> Result<()> {
        if let Err(e) = self.shutdown_sender.send(()) {
            warn!("Failed to send shutdown signal to metrics server: {}", e);
        }
        Ok(())
    }
}

/// Get the global metrics registry.
pub fn global_registry() -> Arc<MetricsRegistry> {
    METRICS_REGISTRY.clone()
}

/// Record configuration reload.
pub fn record_config_reload() {
    global_registry().record_config_reload();
}

/// Update proxy metrics.
pub fn update_proxy_metrics(instance_name: String, metrics: ProxyMetrics) {
    global_registry().update_proxy_metrics(instance_name, metrics);
}

/// Add upstream for tracking.
pub fn add_upstream(upstream: Arc<UpstreamInfo>) {
    global_registry().add_upstream(upstream);
}

/// Remove upstream from tracking.
pub fn remove_upstream(name: &str) {
    global_registry().remove_upstream(name);
}

/// Set proxy instances count.
pub fn set_proxy_instances_count(count: usize) {
    global_registry().set_proxy_instances_count(count);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_registry() {
        let registry = MetricsRegistry::new();
        
        // Test initial state
        let summary = registry.get_metrics_summary();
        assert_eq!(summary.system.proxy_instances_total, 0);
        assert_eq!(summary.system.upstreams_total, 0);
        assert!(summary.proxies.is_empty());
        assert!(summary.upstreams.is_empty());
    }

    #[test]
    fn test_prometheus_export() {
        let registry = MetricsRegistry::new();
        let prometheus_output = registry.export_prometheus();
        
        // Check that basic metrics are present
        assert!(prometheus_output.contains("harbr_proxy_instances_total"));
        assert!(prometheus_output.contains("harbr_upstreams_total"));
        assert!(prometheus_output.contains("harbr_uptime_seconds"));
    }

    #[tokio::test]
    async fn test_metrics_server_creation() {
        let config = MetricsConfig {
            enabled: true,
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            path: "/metrics".to_string(),
            interval_secs: 60,
        };
        
        let server = MetricsServer::new(config);
        assert!(server.config.enabled);
    }
}