// src/client.rs
use crate::config::{ProxyConfig, RouteConfig, TcpProxyConfig};
use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// API response wrapper
#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    success: bool,
    message: String,
    data: Option<T>,
}

/// Client for interacting with the router's configuration API
pub struct ConfigClient {
    client: Client,
    base_url: String,
}

impl ConfigClient {
    /// Create a new client pointing to the given API endpoint
    pub fn new(base_url: &str) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to create HTTP client");
            
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }
    
    /// Get the current configuration
    pub async fn get_config(&self) -> Result<ProxyConfig> {
        let url = format!("{}/api/config", self.base_url);
        let response = self.client.get(&url).send().await?;
        
        let api_response: ApiResponse<ProxyConfig> = response.json().await?;
        
        if api_response.success {
            api_response.data.ok_or_else(|| anyhow::anyhow!("No configuration data returned"))
        } else {
            Err(anyhow::anyhow!("Failed to get configuration: {}", api_response.message))
        }
    }
    
    /// Reload configuration from file
    pub async fn reload_config(&self) -> Result<()> {
        let url = format!("{}/api/config/reload", self.base_url);
        let response = self.client.post(&url).send().await?;
        
        let api_response: ApiResponse<()> = response.json().await?;
        
        if api_response.success {
            Ok(())
        } else {
            Err(anyhow::anyhow!("Failed to reload configuration: {}", api_response.message))
        }
    }
    
    /// Get all routes
    pub async fn get_routes(&self) -> Result<std::collections::HashMap<String, RouteConfig>> {
        let url = format!("{}/api/config/routes", self.base_url);
        let response = self.client.get(&url).send().await?;
        
        let api_response: ApiResponse<std::collections::HashMap<String, RouteConfig>> = response.json().await?;
        
        if api_response.success {
            api_response.data.ok_or_else(|| anyhow::anyhow!("No routes data returned"))
        } else {
            Err(anyhow::anyhow!("Failed to get routes: {}", api_response.message))
        }
    }
    
    /// Get a specific route
    pub async fn get_route(&self, name: &str) -> Result<RouteConfig> {
        let url = format!("{}/api/config/routes/{}", self.base_url, name);
        let response = self.client.get(&url).send().await?;
        
        let api_response: ApiResponse<RouteConfig> = response.json().await?;
        
        if api_response.success {
            api_response.data.ok_or_else(|| anyhow::anyhow!("No route data returned"))
        } else {
            Err(anyhow::anyhow!("Failed to get route: {}", api_response.message))
        }
    }
    
    /// Update or create a route
    pub async fn update_route(&self, name: &str, route: RouteConfig) -> Result<()> {
        let url = format!("{}/api/config/routes/{}", self.base_url, name);
        
        #[derive(Serialize)]
        struct RouteUpdateRequest {
            upstream: String,
            timeout_ms: Option<u64>,
            retry_count: Option<u32>,
            priority: Option<i32>,
            preserve_host_header: Option<bool>,
            is_tcp: Option<bool>,
            tcp_listen_port: Option<u16>,
            is_udp: Option<bool>,
            udp_listen_port: Option<u16>,
            db_type: Option<String>,
        }
        
        let request = RouteUpdateRequest {
            upstream: route.upstream,
            timeout_ms: route.timeout_ms,
            retry_count: route.retry_count,
            priority: route.priority,
            preserve_host_header: route.preserve_host_header,
            is_tcp: Some(route.is_tcp),
            tcp_listen_port: route.tcp_listen_port,
            is_udp: route.is_udp,
            udp_listen_port: route.udp_listen_port,
            db_type: route.db_type,
        };
        
        let response = self.client.put(&url).json(&request).send().await?;
        
        let api_response: ApiResponse<()> = response.json().await?;
        
        if api_response.success {
            Ok(())
        } else {
            Err(anyhow::anyhow!("Failed to update route: {}", api_response.message))
        }
    }
    
    /// Delete a route
    pub async fn delete_route(&self, name: &str) -> Result<()> {
        let url = format!("{}/api/config/routes/{}", self.base_url, name);
        let response = self.client.delete(&url).send().await?;
        
        let api_response: ApiResponse<()> = response.json().await?;
        
        if api_response.success {
            Ok(())
        } else {
            Err(anyhow::anyhow!("Failed to delete route: {}", api_response.message))
        }
    }
    
    /// Update TCP configuration
    pub async fn update_tcp_config(&self, tcp_config: &TcpProxyConfig) -> Result<()> {
        let url = format!("{}/api/config/tcp", self.base_url);
        
        #[derive(Serialize)]
        struct TcpConfigUpdateRequest {
            enabled: Option<bool>,
            listen_addr: Option<String>,
            connection_pooling: Option<bool>,
            max_idle_time_secs: Option<u64>,
            udp_enabled: Option<bool>,
            udp_listen_addr: Option<String>,
        }
        
        let request = TcpConfigUpdateRequest {
            enabled: Some(tcp_config.enabled),
            listen_addr: Some(tcp_config.listen_addr.clone()),
            connection_pooling: Some(tcp_config.connection_pooling),
            max_idle_time_secs: Some(tcp_config.max_idle_time_secs),
            udp_enabled: Some(tcp_config.udp_enabled),
            udp_listen_addr: Some(tcp_config.udp_listen_addr.clone()),
        };
        
        let response = self.client.put(&url).json(&request).send().await?;
        
        let api_response: ApiResponse<()> = response.json().await?;
        
        if api_response.success {
            Ok(())
        } else {
            Err(anyhow::anyhow!("Failed to update TCP configuration: {}", api_response.message))
        }
    }
    
    /// Update global settings
    pub async fn update_global_settings(
        &self,
        listen_addr: Option<String>,
        global_timeout_ms: Option<u64>,
        max_connections: Option<usize>,
    ) -> Result<()> {
        let url = format!("{}/api/config/global", self.base_url);
        
        #[derive(Serialize)]
        struct GlobalSettingsUpdateRequest {
            listen_addr: Option<String>,
            global_timeout_ms: Option<u64>,
            max_connections: Option<usize>,
        }
        
        let request = GlobalSettingsUpdateRequest {
            listen_addr,
            global_timeout_ms,
            max_connections,
        };
        
        let response = self.client.put(&url).json(&request).send().await?;
        
        let api_response: ApiResponse<()> = response.json().await?;
        
        if api_response.success {
            Ok(())
        } else {
            Err(anyhow::anyhow!("Failed to update global settings: {}", api_response.message))
        }
    }
}