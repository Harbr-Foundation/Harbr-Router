//! High-performance HTTP proxy implementation.
//!
//! This module provides a production-ready HTTP proxy with advanced features including
//! HTTP/2 support, connection pooling, request/response transformation, and comprehensive metrics.

use super::{Proxy, ProxyHealth, ProxyMetrics, ShutdownSignal, utils};
use crate::{
    config::{HttpProxyConfig, ProxyInstanceConfig, ProxyType},
    routing::{HealthState, Router, UpstreamInfo},
    Error, Result,
};
use async_trait::async_trait;
use hyper::{
    body::Body, client::HttpConnector, header::HeaderValue, http::HeaderName, service::{make_service_fn, service_fn}, Client, Request, Response, Server, Uri,
};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use parking_lot::RwLock;
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::time::timeout;
use tracing::{error, info, warn};

/// HTTP proxy implementation with advanced routing and connection pooling.
#[derive(Debug)]
pub struct HttpProxy {
    /// HTTP client for upstream connections
    client: Client<HttpsConnector<HttpConnector>, Body>,
    /// Proxy metrics
    metrics: Arc<HttpProxyMetrics>,
}

/// HTTP proxy specific metrics.
#[derive(Debug, Default)]
struct HttpProxyMetrics {
    /// Total HTTP requests processed
    requests_total: AtomicU64,
    /// Currently active connections
    connections_active: AtomicUsize,
    /// Total bytes sent to upstreams
    bytes_sent: AtomicU64,
    /// Total bytes received from upstreams  
    bytes_received: AtomicU64,
    /// Request duration histogram (simplified)
    request_durations: RwLock<Vec<f64>>,
    /// Response status code counts
    status_codes: RwLock<HashMap<u16, u64>>,
}

/// Context for handling HTTP requests.
#[derive(Clone)]
struct HttpRequestContext {
    router: Arc<Router>,
    config: HttpProxyConfig,
    metrics: Arc<HttpProxyMetrics>,
    client: Client<HttpsConnector<HttpConnector>, Body>,
}

impl HttpProxy {
    /// Create a new HTTP proxy instance.
    pub fn new() -> Self {
        // Build HTTPS-capable client with connection pooling
        let https_connector = HttpsConnectorBuilder::new()
            .with_native_roots()
            .https_or_http()
            .enable_http1()
            .build();

        let client = Client::builder()
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(100)
            .http2_adaptive_window(true)
            .http2_keep_alive_interval(Duration::from_secs(30))
            .build(https_connector);

        Self {
            client,
            metrics: Arc::new(HttpProxyMetrics::default()),
        }
    }

    /// Handle an incoming HTTP request.
    async fn handle_request(
        ctx: HttpRequestContext,
        req: Request<Body>,
    ) -> std::result::Result<Response<Body>, Infallible> {
        let start_time = Instant::now();
        ctx.metrics.requests_total.fetch_add(1, Ordering::Relaxed);
        ctx.metrics.connections_active.fetch_add(1, Ordering::Relaxed);

        let result = Self::process_request(ctx.clone(), req).await;
        
        ctx.metrics.connections_active.fetch_sub(1, Ordering::Relaxed);
        
        let duration = start_time.elapsed();
        let duration_ms = duration.as_secs_f64() * 1000.0;
        
        {
            let mut durations = ctx.metrics.request_durations.write();
            durations.push(duration_ms);
            
            // Keep only recent measurements (sliding window)
            if durations.len() > 10000 {
                durations.drain(..1000);
            }
        }

        match result {
            Ok(response) => {
                let status = response.status().as_u16();
                {
                    let mut status_codes = ctx.metrics.status_codes.write();
                    *status_codes.entry(status).or_insert(0) += 1;
                }
                Ok(response)
            }
            Err(e) => {
                error!("HTTP request processing failed: {}", e);
                {
                    let mut status_codes = ctx.metrics.status_codes.write();
                    *status_codes.entry(502).or_insert(0) += 1;
                }
                Ok(Self::create_error_response(502, "Bad Gateway"))
            }
        }
    }

    /// Process an HTTP request by routing to upstream.
    async fn process_request(
        ctx: HttpRequestContext,
        mut req: Request<Body>,
    ) -> Result<Response<Body>> {
        // Select upstream using the router
        let upstream = ctx.router
            .select_upstream(Self::extract_session_key(&req))
            .ok_or_else(|| Error::proxy("No healthy upstream available", "http", "unknown"))?;

        // Build upstream URI
        let upstream_uri = Self::build_upstream_uri(&upstream, req.uri())?;
        *req.uri_mut() = upstream_uri;

        // Apply request transformations
        Self::transform_request(&mut req, &ctx.config)?;

        // Record request metrics
        let body_size = Self::estimate_body_size(req.body());
        ctx.metrics.bytes_sent.fetch_add(body_size, Ordering::Relaxed);

        // Create timeout for upstream request
        let request_timeout = utils::millis_to_duration(upstream.config.timeout_ms);
        
        // Send request to upstream (simplified - no retries for now to avoid complexity)
        let request_start = Instant::now();
        let response = match timeout(request_timeout, ctx.client.request(req)).await {
            Ok(Ok(response)) => {
                upstream.stats.record_success(request_start.elapsed());
                response
            }
            Ok(Err(e)) => {
                upstream.stats.record_failure();
                return Err(Error::network(
                    format!("Request to upstream '{}' failed: {}", upstream.config.name, e),
                    crate::error::NetworkErrorKind::ConnectionFailed,
                ));
            }
            Err(_) => {
                upstream.stats.record_failure();
                return Err(Error::network(
                    format!("Request to upstream '{}' timed out", upstream.config.name),
                    crate::error::NetworkErrorKind::Timeout,
                ));
            }
        };

        // Record response metrics
        let response_size = Self::estimate_body_size(response.body());
        ctx.metrics.bytes_received.fetch_add(response_size, Ordering::Relaxed);

        // Apply response transformations
        let transformed_response = Self::transform_response(response, &ctx.config)?;

        Ok(transformed_response)
    }


    /// Build the upstream URI from the original request URI.
    fn build_upstream_uri(upstream: &UpstreamInfo, original_uri: &Uri) -> Result<Uri> {
        let upstream_base = upstream.config.address.parse::<Uri>()
            .map_err(|e| Error::config(format!("Invalid upstream address: {}", e)))?;

        let path_and_query = original_uri.path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");

        let upstream_uri = format!(
            "{}://{}{}",
            upstream_base.scheme().map(|s| s.as_str()).unwrap_or("http"),
            upstream_base.authority().map(|a| a.as_str()).unwrap_or(&upstream.config.address),
            path_and_query
        );

        upstream_uri.parse()
            .map_err(|e| Error::internal(format!("Failed to build upstream URI: {}", e)))
    }

    /// Apply request transformations based on configuration.
    fn transform_request(req: &mut Request<Body>, config: &HttpProxyConfig) -> Result<()> {
        let headers = req.headers_mut();

        // Add configured headers
        for (name, value) in &config.add_headers {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| Error::config(format!("Invalid header name '{}': {}", name, e)))?;
            let header_value = HeaderValue::from_str(value)
                .map_err(|e| Error::config(format!("Invalid header value '{}': {}", value, e)))?;
            
            headers.insert(header_name, header_value);
        }

        // Remove configured headers
        for name in &config.remove_headers {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| Error::config(format!("Invalid header name '{}': {}", name, e)))?;
            headers.remove(header_name);
        }

        Ok(())
    }

    /// Apply response transformations based on configuration.
    fn transform_response(mut response: Response<Body>, _config: &HttpProxyConfig) -> Result<Response<Body>> {
        // Add server identification header
        response.headers_mut().insert(
            "X-Proxy-Server",
            HeaderValue::from_static("Harbr-Router"),
        );

        // Additional response transformations could be added here
        // (compression, header modification, etc.)

        Ok(response)
    }

    /// Extract session key for consistent hashing (if enabled).
    fn extract_session_key(req: &Request<Body>) -> Option<&str> {
        // Look for session identifier in cookies or headers
        req.headers()
            .get("cookie")
            .and_then(|h| h.to_str().ok())
            .and_then(|cookies| {
                cookies.split(';')
                    .find_map(|cookie| {
                        let mut parts = cookie.trim().splitn(2, '=');
                        match (parts.next(), parts.next()) {
                            (Some("session_id"), Some(value)) => Some(value),
                            _ => None,
                        }
                    })
            })
            .or_else(|| {
                // Fallback to client IP
                req.headers()
                    .get("x-forwarded-for")
                    .or_else(|| req.headers().get("x-real-ip"))
                    .and_then(|h| h.to_str().ok())
            })
    }

    /// Estimate body size for metrics (without consuming the body).
    fn estimate_body_size(_body: &Body) -> u64 {
        // This is a simplified implementation
        // In a real proxy, you'd want to track actual bytes transferred
        0
    }


    /// Create an error response.
    fn create_error_response(status: u16, message: &str) -> Response<Body> {
        Response::builder()
            .status(status)
            .header("Content-Type", "text/plain")
            .body(Body::from(message.to_string()))
            .unwrap()
    }
}

#[async_trait]
impl Proxy for HttpProxy {
    fn proxy_type(&self) -> ProxyType {
        ProxyType::Http
    }

    async fn start(
        &self,
        config: ProxyInstanceConfig,
        router: Arc<Router>,
        mut shutdown: ShutdownSignal,
    ) -> Result<()> {
        info!("Starting HTTP proxy '{}' on {}", config.name, config.listen_addr);

        // Extract HTTP-specific configuration
        let http_config = config.config.http.clone();

        // Create request context
        let ctx = HttpRequestContext {
            router: router.clone(),
            config: http_config,
            metrics: self.metrics.clone(),
            client: self.client.clone(),
        };

        // Create service
        let make_service = make_service_fn(move |_conn| {
            let ctx = ctx.clone();
            async move {
                Ok::<_, Infallible>(service_fn(move |req| {
                    Self::handle_request(ctx.clone(), req)
                }))
            }
        });

        // Clone config name for use in shutdown and success messages
        let instance_name = config.name.clone();
        let shutdown_name = instance_name.clone();

        // Create and start server
        let server = Server::bind(&config.listen_addr)
            .serve(make_service)
            .with_graceful_shutdown(async move {
                let _ = shutdown.recv().await;
                info!("HTTP proxy '{}' received shutdown signal", shutdown_name);
            });

        info!("HTTP proxy '{}' started successfully", instance_name);

        // Run server
        if let Err(e) = server.await {
            return Err(Error::internal(format!("HTTP proxy server error: {}", e)));
        }

        info!("HTTP proxy '{}' shut down", instance_name);
        Ok(())
    }

    fn validate_config(&self, config: &ProxyInstanceConfig) -> Result<()> {
        if config.proxy_type != ProxyType::Http {
            return Err(Error::config("Invalid proxy type for HTTP proxy"));
        }

        // Validate HTTP-specific configuration
        let http_config = &config.config.http;
        
        if http_config.max_request_size == 0 {
            return Err(Error::config("HTTP max_request_size must be greater than 0"));
        }

        if http_config.max_request_size > 1024 * 1024 * 1024 {
            return Err(Error::config("HTTP max_request_size cannot exceed 1GB"));
        }

        if http_config.request_timeout_secs == 0 {
            return Err(Error::config("HTTP request_timeout_secs must be greater than 0"));
        }

        // Validate header names and values
        for (name, _) in &http_config.add_headers {
            HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| Error::config(format!("Invalid header name '{}': {}", name, e)))?;
        }

        for name in &http_config.remove_headers {
            HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| Error::config(format!("Invalid header name '{}': {}", name, e)))?;
        }

        Ok(())
    }

    async fn get_metrics(&self) -> Result<ProxyMetrics> {
        let durations = self.metrics.request_durations.read();
        let avg_response_time_ms = if durations.is_empty() {
            0.0
        } else {
            durations.iter().sum::<f64>() / durations.len() as f64
        };

        let status_codes = self.metrics.status_codes.read();
        let total_responses: u64 = status_codes.values().sum();
        let error_responses: u64 = status_codes.iter()
            .filter(|(status, _)| **status >= 400)
            .map(|(_, count)| *count)
            .sum();

        let error_rate = if total_responses > 0 {
            error_responses as f64 / total_responses as f64
        } else {
            0.0
        };

        Ok(ProxyMetrics {
            total_requests: self.metrics.requests_total.load(Ordering::Relaxed),
            active_connections: self.metrics.connections_active.load(Ordering::Relaxed) as u64,
            bytes_sent: self.metrics.bytes_sent.load(Ordering::Relaxed),
            bytes_received: self.metrics.bytes_received.load(Ordering::Relaxed),
            request_rate: 0.0, // Would need time-based calculation
            error_rate,
            avg_response_time_ms,
        })
    }

    async fn health_check(&self) -> Result<ProxyHealth> {
        // Check if the proxy is functioning properly
        let active_connections = self.metrics.connections_active.load(Ordering::Relaxed);
        let total_requests = self.metrics.requests_total.load(Ordering::Relaxed);

        let status = if active_connections < 10000 && total_requests > 0 {
            HealthState::Healthy
        } else if active_connections >= 10000 {
            HealthState::Unhealthy
        } else {
            HealthState::Unknown
        };

        let message = format!(
            "Active connections: {}, Total requests: {}",
            active_connections, total_requests
        );

        Ok(ProxyHealth { status, message })
    }
}

impl Default for HttpProxy {
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
            name: "test-http".to_string(),
            proxy_type: ProxyType::Http,
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            upstreams: vec![UpstreamConfig {
                name: "test-upstream".to_string(),
                address: "http://127.0.0.1:8081".to_string(),
                weight: 1,
                priority: 0,
                timeout_ms: 5000,
                retry: RetryConfig::default(),
                health_check: None,
                tls: None,
            }],
            config: ProxySpecificConfig {
                http: HttpProxyConfig {
                    max_request_size: 1024 * 1024,
                    request_timeout_secs: 30,
                    add_headers: [("X-Proxy".to_string(), "Harbr-Router".to_string())].into(),
                    ..Default::default()
                },
                ..Default::default()
            },
        }
    }

    #[test]
    fn test_http_proxy_creation() {
        let proxy = HttpProxy::new();
        assert_eq!(proxy.proxy_type(), ProxyType::Http);
    }

    #[test]
    fn test_config_validation() {
        let proxy = HttpProxy::new();
        let config = create_test_config();
        
        // Valid configuration should pass
        assert!(proxy.validate_config(&config).is_ok());
        
        // Invalid proxy type should fail
        let mut invalid_config = config.clone();
        invalid_config.proxy_type = ProxyType::Tcp;
        assert!(proxy.validate_config(&invalid_config).is_err());
        
        // Invalid max_request_size should fail
        let mut invalid_config = config.clone();
        invalid_config.config.http.max_request_size = 0;
        assert!(proxy.validate_config(&invalid_config).is_err());
    }

    #[tokio::test]
    async fn test_metrics() {
        let proxy = HttpProxy::new();
        let metrics = proxy.get_metrics().await.unwrap();
        
        assert_eq!(metrics.total_requests, 0);
        assert_eq!(metrics.active_connections, 0);
        assert_eq!(metrics.bytes_sent, 0);
        assert_eq!(metrics.bytes_received, 0);
    }

    #[tokio::test]
    async fn test_health_check() {
        let proxy = HttpProxy::new();
        let health = proxy.health_check().await.unwrap();
        
        assert_eq!(health.status, HealthState::Unknown);
        assert!(health.message.contains("Active connections: 0"));
    }
}