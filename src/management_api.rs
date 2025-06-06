// src/management_api.rs - Management REST API for the router
use crate::config::RouterConfig;
use crate::plugin::manager::PluginManager;
use crate::plugin::{PluginConfig, PluginEvent, PluginInfo, PluginHealth, PluginMetrics};
use crate::error::{RouterError, Result};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use warp::{Filter, Rejection, Reply};
use warp::http::StatusCode;
use tracing;

/// Management API server
pub struct ManagementApi {
    port: u16,
    plugin_manager: Arc<PluginManager>,
    config: Arc<RwLock<RouterConfig>>,
    shutdown_sender: Option<mpsc::Sender<()>>,
    authentication: Option<ApiAuthentication>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiAuthentication {
    pub api_keys: Vec<String>,
    pub basic_auth: Option<BasicAuth>,
    pub jwt_secret: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasicAuth {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiResponse<T> {
    pub success: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub timestamp: String,
}

impl<T> ApiResponse<T> {
    pub fn success(message: &str, data: Option<T>) -> Self {
        Self {
            success: true,
            message: message.to_string(),
            data,
            error: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }
    
    pub fn error(message: &str, error_detail: Option<String>) -> Self {
        Self {
            success: false,
            message: message.to_string(),
            data: None,
            error: error_detail,
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PluginStatusRequest {
    pub enabled: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PluginConfigUpdateRequest {
    pub config: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RouteCreateRequest {
    pub domain: String,
    pub proxy_instance: String,
    pub config: Option<crate::config::DomainConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SystemStatus {
    pub uptime_seconds: u64,
    pub version: String,
    pub total_plugins: usize,
    pub running_plugins: usize,
    pub total_connections: u64,
    pub memory_usage_mb: f64,
    pub cpu_usage_percent: f64,
}

impl ManagementApi {
    pub fn new(
        port: u16,
        plugin_manager: Arc<PluginManager>,
        config: Arc<RwLock<RouterConfig>>,
    ) -> Self {
        Self {
            port,
            plugin_manager,
            config,
            shutdown_sender: None,
            authentication: None,
        }
    }
    
    pub fn with_authentication(mut self, auth: ApiAuthentication) -> Self {
        self.authentication = Some(auth);
        self
    }
    
    pub async fn start(&mut self) -> Result<()> {
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
        self.shutdown_sender = Some(shutdown_tx);
        
        let routes = self.create_routes();
        let addr = SocketAddr::from(([0, 0, 0, 0], self.port));
        
        tracing::info!("Starting management API on port {}", self.port);
        
        let server = warp::serve(routes)
            .bind_with_graceful_shutdown(addr, async move {
                let _ = shutdown_rx.recv().await;
                tracing::info!("Shutting down management API");
            });
        
        tokio::spawn(server);
        
        Ok(())
    }
    
    pub async fn stop(&self) -> Result<()> {
        if let Some(sender) = &self.shutdown_sender {
            let _ = sender.send(()).await;
        }
        Ok(())
    }
    
    fn create_routes(&self) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
        let cors = warp::cors()
            .allow_any_origin()
            .allow_headers(vec!["content-type", "authorization", "x-api-key"])
            .allow_methods(vec!["GET", "POST", "PUT", "DELETE", "PATCH"]);
        
        // Create route filters
        let system_routes = self.system_routes();
        let plugin_routes = self.plugin_routes();
        let config_routes = self.config_routes();
        let health_routes = self.health_routes();
        let metrics_routes = self.metrics_routes();
        
        system_routes
            .or(plugin_routes)
            .or(config_routes)
            .or(health_routes)
            .or(metrics_routes)
            .with(cors)
            .with(warp::log("management_api"))
            .recover(handle_rejection)
    }
    
    fn system_routes(&self) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
        let plugin_manager = self.plugin_manager.clone();
        let config = self.config.clone();
        
        // GET /api/system/status
        let status = warp::path!("api" / "system" / "status")
            .and(warp::get())
            .and_then(move || {
                let pm = plugin_manager.clone();
                let cfg = config.clone();
                async move {
                    handle_get_system_status(pm, cfg).await
                }
            });
        
        // POST /api/system/shutdown
        let shutdown = warp::path!("api" / "system" / "shutdown")
            .and(warp::post())
            .and_then(|| async {
                handle_system_shutdown().await
            });
        
        // GET /api/system/version
        let version = warp::path!("api" / "system" / "version")
            .and(warp::get())
            .and_then(|| async {
                Ok::<_, Rejection>(warp::reply::json(&ApiResponse::success(
                    "Version information",
                    Some(serde_json::json!({
                        "version": env!("CARGO_PKG_VERSION"),
                        "build_date": env!("BUILD_DATE").unwrap_or("unknown"),
                        "git_hash": env!("GIT_HASH").unwrap_or("unknown"),
                    }))
                )))
            });
        
        status.or(shutdown).or(version)
    }
    
    fn plugin_routes(&self) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
        let plugin_manager = self.plugin_manager.clone();
        
        // GET /api/plugins
        let list_plugins = warp::path!("api" / "plugins")
            .and(warp::get())
            .and_then(move || {
                let pm = plugin_manager.clone();
                async move {
                    handle_list_plugins(pm).await
                }
            });
        
        let plugin_manager2 = self.plugin_manager.clone();
        
        // GET /api/plugins/{name}
        let get_plugin = warp::path!("api" / "plugins" / String)
            .and(warp::get())
            .and_then(move |name: String| {
                let pm = plugin_manager2.clone();
                async move {
                    handle_get_plugin(pm, name).await
                }
            });
        
        let plugin_manager3 = self.plugin_manager.clone();
        
        // POST /api/plugins/{name}/reload
        let reload_plugin = warp::path!("api" / "plugins" / String / "reload")
            .and(warp::post())
            .and_then(move |name: String| {
                let pm = plugin_manager3.clone();
                async move {
                    handle_reload_plugin(pm, name).await
                }
            });
        
        let plugin_manager4 = self.plugin_manager.clone();
        
        // PUT /api/plugins/{name}/config
        let update_plugin_config = warp::path!("api" / "plugins" / String / "config")
            .and(warp::put())
            .and(warp::body::json())
            .and_then(move |name: String, req: PluginConfigUpdateRequest| {
                let pm = plugin_manager4.clone();
                async move {
                    handle_update_plugin_config(pm, name, req).await
                }
            });
        
        let plugin_manager5 = self.plugin_manager.clone();
        
        // POST /api/plugins/{name}/command
        let plugin_command = warp::path!("api" / "plugins" / String / "command")
            .and(warp::post())
            .and(warp::body::json())
            .and_then(move |name: String, body: serde_json::Value| {
                let pm = plugin_manager5.clone();
                async move {
                    handle_plugin_command(pm, name, body).await
                }
            });
        
        list_plugins
            .or(get_plugin)
            .or(reload_plugin)
            .or(update_plugin_config)
            .or(plugin_command)
    }
    
    fn config_routes(&self) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
        let config = self.config.clone();
        let plugin_manager = self.plugin_manager.clone();
        
        // GET /api/config
        let get_config = warp::path!("api" / "config")
            .and(warp::get())
            .and_then(move || {
                let cfg = config.clone();
                async move {
                    handle_get_config(cfg).await
                }
            });
        
        let config2 = self.config.clone();
        
        // PUT /api/config
        let update_config = warp::path!("api" / "config")
            .and(warp::put())
            .and(warp::body::json())
            .and_then(move |new_config: RouterConfig| {
                let cfg = config2.clone();
                async move {
                    handle_update_config(cfg, new_config).await
                }
            });
        
        let config3 = self.config.clone();
        
        // POST /api/config/reload
        let reload_config = warp::path!("api" / "config" / "reload")
            .and(warp::post())
            .and_then(move || {
                let cfg = config3.clone();
                async move {
                    handle_reload_config(cfg).await
                }
            });
        
        let config4 = self.config.clone();
        
        // POST /api/routes
        let create_route = warp::path!("api" / "routes")
            .and(warp::post())
            .and(warp::body::json())
            .and_then(move |req: RouteCreateRequest| {
                let cfg = config4.clone();
                async move {
                    handle_create_route(cfg, req).await
                }
            });
        
        let config5 = self.config.clone();
        
        // DELETE /api/routes/{domain}
        let delete_route = warp::path!("api" / "routes" / String)
            .and(warp::delete())
            .and_then(move |domain: String| {
                let cfg = config5.clone();
                async move {
                    handle_delete_route(cfg, domain).await
                }
            });
        
        get_config
            .or(update_config)
            .or(reload_config)
            .or(create_route)
            .or(delete_route)
    }
    
    fn health_routes(&self) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
        let plugin_manager = self.plugin_manager.clone();
        
        // GET /api/health
        let overall_health = warp::path!("api" / "health")
            .and(warp::get())
            .and_then(move || {
                let pm = plugin_manager.clone();
                async move {
                    handle_get_overall_health(pm).await
                }
            });
        
        let plugin_manager2 = self.plugin_manager.clone();
        
        // GET /api/health/plugins
        let plugin_health = warp::path!("api" / "health" / "plugins")
            .and(warp::get())
            .and_then(move || {
                let pm = plugin_manager2.clone();
                async move {
                    handle_get_plugin_health(pm).await
                }
            });
        
        overall_health.or(plugin_health)
    }
    
    fn metrics_routes(&self) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
        let plugin_manager = self.plugin_manager.clone();
        
        // GET /api/metrics
        let get_metrics = warp::path!("api" / "metrics")
            .and(warp::get())
            .and_then(move || {
                let pm = plugin_manager.clone();
                async move {
                    handle_get_metrics(pm).await
                }
            });
        
        let plugin_manager2 = self.plugin_manager.clone();
        
        // GET /api/metrics/prometheus
        let prometheus_metrics = warp::path!("api" / "metrics" / "prometheus")
            .and(warp::get())
            .and_then(move || {
                let pm = plugin_manager2.clone();
                async move {
                    handle_get_prometheus_metrics(pm).await
                }
            });
        
        get_metrics.or(prometheus_metrics)
    }
}

// Handler implementations
async fn handle_get_system_status(
    plugin_manager: Arc<PluginManager>,
    config: Arc<RwLock<RouterConfig>>,
) -> Result<impl Reply, Rejection> {
    let plugins = plugin_manager.list_plugins();
    let total_plugins = plugins.len();
    let running_plugins = total_plugins; // Simplified - in practice you'd check each plugin status
    
    let status = SystemStatus {
        uptime_seconds: 0, // Would track actual uptime
        version: env!("CARGO_PKG_VERSION").to_string(),
        total_plugins,
        running_plugins,
        total_connections: 0, // Would get from metrics
        memory_usage_mb: get_memory_usage(),
        cpu_usage_percent: get_cpu_usage(),
    };
    
    Ok(warp::reply::json(&ApiResponse::success(
        "System status retrieved",
        Some(status),
    )))
}

async fn handle_system_shutdown() -> Result<impl Reply, Rejection> {
    // In a real implementation, this would trigger graceful shutdown
    tracing::warn!("System shutdown requested via API");
    
    Ok(warp::reply::json(&ApiResponse::<()>::success(
        "Shutdown initiated",
        None,
    )))
}

async fn handle_list_plugins(
    plugin_manager: Arc<PluginManager>,
) -> Result<impl Reply, Rejection> {
    let plugins = plugin_manager.list_plugins();
    let mut plugin_infos = Vec::new();
    
    for plugin_name in plugins {
        if let Some(info) = plugin_manager.get_plugin_info(&plugin_name).await {
            plugin_infos.push(info);
        }
    }
    
    Ok(warp::reply::json(&ApiResponse::success(
        "Plugins listed successfully",
        Some(plugin_infos),
    )))
}

async fn handle_get_plugin(
    plugin_manager: Arc<PluginManager>,
    name: String,
) -> Result<impl Reply, Rejection> {
    match plugin_manager.get_plugin_info(&name).await {
        Some(info) => {
            let health = plugin_manager.get_plugin_health(&name).await;
            let metrics = plugin_manager.get_plugin_metrics(&name).await;
            
            let plugin_details = serde_json::json!({
                "info": info,
                "health": health,
                "metrics": metrics,
            });
            
            Ok(warp::reply::json(&ApiResponse::success(
                &format!("Plugin '{}' details", name),
                Some(plugin_details),
            )))
        }
        None => Ok(warp::reply::json(&ApiResponse::<()>::error(
            &format!("Plugin '{}' not found", name),
            None,
        ))),
    }
}

async fn handle_reload_plugin(
    plugin_manager: Arc<PluginManager>,
    name: String,
) -> Result<impl Reply, Rejection> {
    match plugin_manager.reload_plugin(&name).await {
        Ok(_) => Ok(warp::reply::json(&ApiResponse::<()>::success(
            &format!("Plugin '{}' reloaded successfully", name),
            None,
        ))),
        Err(e) => Ok(warp::reply::json(&ApiResponse::<()>::error(
            &format!("Failed to reload plugin '{}'", name),
            Some(e.to_string()),
        ))),
    }
}

async fn handle_update_plugin_config(
    plugin_manager: Arc<PluginManager>,
    name: String,
    req: PluginConfigUpdateRequest,
) -> Result<impl Reply, Rejection> {
    // Get current plugin config and update it
    if let Some(mut current_config) = plugin_manager.registry().get_plugin_config(&name).await {
        current_config.config = req.config;
        
        match plugin_manager.update_plugin_config(&name, current_config).await {
            Ok(_) => Ok(warp::reply::json(&ApiResponse::<()>::success(
                &format!("Plugin '{}' configuration updated", name),
                None,
            ))),
            Err(e) => Ok(warp::reply::json(&ApiResponse::<()>::error(
                &format!("Failed to update plugin '{}' configuration", name),
                Some(e.to_string()),
            ))),
        }
    } else {
        Ok(warp::reply::json(&ApiResponse::<()>::error(
            &format!("Plugin '{}' not found", name),
            None,
        )))
    }
}

async fn handle_plugin_command(
    plugin_manager: Arc<PluginManager>,
    name: String,
    body: serde_json::Value,
) -> Result<impl Reply, Rejection> {
    let command = body.get("command").and_then(|v| v.as_str()).unwrap_or("status");
    let args = body.get("args").cloned().unwrap_or(serde_json::json!({}));
    
    match plugin_manager.execute_plugin_command(&name, command, args).await {
        Ok(result) => Ok(warp::reply::json(&ApiResponse::success(
            &format!("Command '{}' executed on plugin '{}'", command, name),
            Some(result),
        ))),
        Err(e) => Ok(warp::reply::json(&ApiResponse::<()>::error(
            &format!("Failed to execute command '{}' on plugin '{}'", command, name),
            Some(e.to_string()),
        ))),
    }
}

async fn handle_get_config(config: Arc<RwLock<RouterConfig>>) -> Result<impl Reply, Rejection> {
    let cfg = config.read().await.clone();
    Ok(warp::reply::json(&ApiResponse::success(
        "Configuration retrieved",
        Some(cfg),
    )))
}

async fn handle_update_config(
    config: Arc<RwLock<RouterConfig>>,
    new_config: RouterConfig,
) -> Result<impl Reply, Rejection> {
    {
        let mut cfg = config.write().await;
        *cfg = new_config;
    }
    
    Ok(warp::reply::json(&ApiResponse::<()>::success(
        "Configuration updated successfully",
        None,
    )))
}

async fn handle_reload_config(config: Arc<RwLock<RouterConfig>>) -> Result<impl Reply, Rejection> {
    // In a real implementation, this would reload from file
    Ok(warp::reply::json(&ApiResponse::<()>::success(
        "Configuration reloaded successfully",
        None,
    )))
}

async fn handle_create_route(
    config: Arc<RwLock<RouterConfig>>,
    req: RouteCreateRequest,
) -> Result<impl Reply, Rejection> {
    let mut cfg = config.write().await;
    
    let domain_config = req.config.unwrap_or(crate::config::DomainConfig {
        proxy_instance: req.proxy_instance,
        backend_service: None,
        ssl_config: None,
        cors_config: None,
        cache_config: None,
        custom_headers: HashMap::new(),
        rewrite_rules: vec![],
        access_control: None,
    });
    
    cfg.domains.insert(req.domain.clone(), domain_config);
    
    Ok(warp::reply::json(&ApiResponse::<()>::success(
        &format!("Route created for domain '{}'", req.domain),
        None,
    )))
}

async fn handle_delete_route(
    config: Arc<RwLock<RouterConfig>>,
    domain: String,
) -> Result<impl Reply, Rejection> {
    let mut cfg = config.write().await;
    
    match cfg.domains.remove(&domain) {
        Some(_) => Ok(warp::reply::json(&ApiResponse::<()>::success(
            &format!("Route deleted for domain '{}'", domain),
            None,
        ))),
        None => Ok(warp::reply::json(&ApiResponse::<()>::error(
            &format!("Route not found for domain '{}'", domain),
            None,
        ))),
    }
}

async fn handle_get_overall_health(
    plugin_manager: Arc<PluginManager>,
) -> Result<impl Reply, Rejection> {
    let plugins = plugin_manager.list_plugins();
    let mut all_healthy = true;
    let mut health_details = HashMap::new();
    
    for plugin_name in plugins {
        if let Some(health) = plugin_manager.get_plugin_health(&plugin_name).await {
            health_details.insert(plugin_name.clone(), health.healthy);
            if !health.healthy {
                all_healthy = false;
            }
        }
    }
    
    let overall_health = serde_json::json!({
        "healthy": all_healthy,
        "plugins": health_details,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });
    
    Ok(warp::reply::json(&ApiResponse::success(
        "Overall health status",
        Some(overall_health),
    )))
}

async fn handle_get_plugin_health(
    plugin_manager: Arc<PluginManager>,
) -> Result<impl Reply, Rejection> {
    let plugins = plugin_manager.list_plugins();
    let mut health_status = HashMap::new();
    
    for plugin_name in plugins {
        if let Some(health) = plugin_manager.get_plugin_health(&plugin_name).await {
            health_status.insert(plugin_name, health);
        }
    }
    
    Ok(warp::reply::json(&ApiResponse::success(
        "Plugin health status",
        Some(health_status),
    )))
}

async fn handle_get_metrics(
    plugin_manager: Arc<PluginManager>,
) -> Result<impl Reply, Rejection> {
    let metrics = plugin_manager.get_all_metrics().await;
    
    Ok(warp::reply::json(&ApiResponse::success(
        "Metrics retrieved",
        Some(metrics),
    )))
}

async fn handle_get_prometheus_metrics(
    plugin_manager: Arc<PluginManager>,
) -> Result<impl Reply, Rejection> {
    let metrics = plugin_manager.get_all_metrics().await;
    
    // Convert to Prometheus format
    let mut prometheus_output = String::new();
    
    for (plugin_name, plugin_metrics) in metrics {
        prometheus_output.push_str(&format!(
            "# HELP router_plugin_connections_total Total connections for plugin\n"
        ));
        prometheus_output.push_str(&format!(
            "# TYPE router_plugin_connections_total counter\n"
        ));
        prometheus_output.push_str(&format!(
            "router_plugin_connections_total{{plugin=\"{}\"}} {}\n",
            plugin_name, plugin_metrics.connections_total
        ));
        
        prometheus_output.push_str(&format!(
            "router_plugin_connections_active{{plugin=\"{}\"}} {}\n",
            plugin_name, plugin_metrics.connections_active
        ));
        
        prometheus_output.push_str(&format!(
            "router_plugin_bytes_sent_total{{plugin=\"{}\"}} {}\n",
            plugin_name, plugin_metrics.bytes_sent
        ));
        
        prometheus_output.push_str(&format!(
            "router_plugin_bytes_received_total{{plugin=\"{}\"}} {}\n",
            plugin_name, plugin_metrics.bytes_received
        ));
        
        prometheus_output.push_str(&format!(
            "router_plugin_errors_total{{plugin=\"{}\"}} {}\n",
            plugin_name, plugin_metrics.errors_total
        ));
    }
    
    Ok(warp::reply::with_header(
        prometheus_output,
        "content-type",
        "text/plain; version=0.0.4",
    ))
}

// Error handling
async fn handle_rejection(err: Rejection) -> Result<impl Reply, Infallible> {
    let (code, message) = if err.is_not_found() {
        (StatusCode::NOT_FOUND, "Not Found")
    } else if let Some(_) = err.find::<warp::filters::body::BodyDeserializeError>() {
        (StatusCode::BAD_REQUEST, "Invalid JSON body")
    } else if let Some(_) = err.find::<warp::reject::MethodNotAllowed>() {
        (StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed")
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
    };
    
    let json = warp::reply::json(&ApiResponse::<()>::error(message, None));
    Ok(warp::reply::with_status(json, code))
}

// System utilities
fn get_memory_usage() -> f64 {
    // Simplified memory usage - in practice use proper system monitoring
    #[cfg(target_os = "linux")]
    {
        if let Ok(process) = procfs::process::Process::myself() {
            if let Ok(stat) = process.stat() {
                return (stat.rss * 4096) as f64 / 1024.0 / 1024.0; // Convert to MB
            }
        }
    }
    0.0
}

fn get_cpu_usage() -> f64 {
    // Simplified CPU usage - in practice use proper system monitoring
    0.0
}