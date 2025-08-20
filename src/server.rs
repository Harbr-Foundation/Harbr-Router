//! Server management and health check endpoints.

use crate::{
    config::HealthCheckConfig,
    metrics::SystemHealth,
    proxy::ProxyHealth,
    routing::HealthState,
    Error, Result,
};
use hyper::{
    header::CONTENT_TYPE, service::{make_service_fn, service_fn}, Body, Request, Response, Server, StatusCode,
};
use serde_json;
use std::{collections::HashMap, convert::Infallible};
use tokio::sync::broadcast;
use tracing::{info, warn};

/// Health check server for system health monitoring.
#[derive(Debug)]
pub struct HealthServer {
    config: HealthCheckConfig,
    shutdown_sender: broadcast::Sender<()>,
}

impl HealthServer {
    /// Create a new health server.
    pub fn new(config: HealthCheckConfig) -> Self {
        let (shutdown_sender, _) = broadcast::channel(16);
        
        Self {
            config,
            shutdown_sender,
        }
    }

    /// Start the health server.
    pub async fn start(&self) -> Result<()> {
        if !self.config.enabled {
            info!("Health check server is disabled");
            return Ok(());
        }

        info!("Starting health server on {}{}", self.config.listen_addr, self.config.path);

        // Create service
        let health_path = self.config.path.clone();
        
        let make_service = make_service_fn(move |_conn| {
            let health_path = health_path.clone();
            
            async move {
                Ok::<_, Infallible>(service_fn(move |req| {
                    Self::handle_request(health_path.clone(), req)
                }))
            }
        });

        // Start server
        let mut shutdown = self.shutdown_sender.subscribe();
        let server = Server::bind(&self.config.listen_addr)
            .serve(make_service)
            .with_graceful_shutdown(async move {
                let _ = shutdown.recv().await;
                info!("Health server received shutdown signal");
            });

        info!("Health server started successfully");

        if let Err(e) = server.await {
            return Err(Error::internal(format!("Health server error: {}", e)));
        }

        info!("Health server shut down");
        Ok(())
    }

    /// Handle HTTP requests to the health server.
    async fn handle_request(
        health_path: String,
        req: Request<Body>,
    ) -> std::result::Result<Response<Body>, Infallible> {
        let path = req.uri().path();

        match path {
            path if path == health_path => {
                // Basic health check - always returns OK for now
                // In a full implementation, this would check actual system health
                let health = SystemHealth {
                    status: HealthState::Healthy,
                    uptime_seconds: 0, // Would get from metrics registry
                    healthy_proxies: 0,
                    unhealthy_proxies: 0,
                    healthy_upstreams: 0,
                    unhealthy_upstreams: 0,
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                    message: "Health check endpoint operational".to_string(),
                };

                let health_json = serde_json::to_string(&health).unwrap();
                
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(health_json))
                    .unwrap())
            }
            _ => {
                Ok(Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::from("Not Found"))
                    .unwrap())
            }
        }
    }

    /// Shutdown the health server.
    pub fn shutdown(&self) -> Result<()> {
        if let Err(e) = self.shutdown_sender.send(()) {
            warn!("Failed to send shutdown signal to health server: {}", e);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_health_server_creation() {
        let config = HealthCheckConfig {
            enabled: true,
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            path: "/health".to_string(),
        };
        
        let server = HealthServer::new(config);
        assert!(server.config.enabled);
    }
}