// src/config_api.rs
use crate::dynamic_config::{DynamicConfigManager, ConfigEvent};
use crate::config::{ProxyConfig, RouteConfig, TcpProxyConfig};
use std::sync::Arc;
use warp::{Filter, Rejection, Reply};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;

/// API response wrapper
#[derive(Serialize)]
struct ApiResponse<T> {
    success: bool,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<T>,
}

impl<T> ApiResponse<T> {
    fn success(message: &str, data: Option<T>) -> Self {
        Self {
            success: true,
            message: message.to_string(),
            data,
        }
    }

    fn error(message: &str) -> Self {
        Self {
            success: false,
            message: message.to_string(),
            data: None,
        }
    }
}

/// Setup configuration management API routes
pub fn config_api_routes(
    config_manager: Arc<DynamicConfigManager>,
) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
    let with_config_manager = warp::any().map(move || config_manager.clone());

    // GET /api/config - Get current configuration
    let get_config = warp::path!("api" / "config")
        .and(warp::get())
        .and(with_config_manager.clone())
        .and_then(handle_get_config);

    // POST /api/config/reload - Reload config from file
    let reload_config = warp::path!("api" / "config" / "reload")
        .and(warp::post())
        .and(with_config_manager.clone())
        .and_then(handle_reload_config);

    // GET /api/config/routes - List all routes
    let list_routes = warp::path!("api" / "config" / "routes")
        .and(warp::get())
        .and(with_config_manager.clone())
        .and_then(handle_list_routes);

    // GET /api/config/routes/{name} - Get route details
    let get_route = warp::path!("api" / "config" / "routes" / String)
        .and(warp::get())
        .and(with_config_manager.clone())
        .and_then(handle_get_route);

    // PUT /api/config/routes/{name} - Update or create route
    let update_route = warp::path!("api" / "config" / "routes" / String)
        .and(warp::put())
        .and(warp::body::json())
        .and(with_config_manager.clone())
        .and_then(handle_update_route);

    // DELETE /api/config/routes/{name} - Delete route
    let delete_route = warp::path!("api" / "config" / "routes" / String)
        .and(warp::delete())
        .and(with_config_manager.clone())
        .and_then(handle_delete_route);

    // PUT /api/config/tcp - Update TCP config
    let update_tcp_config = warp::path!("api" / "config" / "tcp")
        .and(warp::put())
        .and(warp::body::json())
        .and(with_config_manager.clone())
        .and_then(handle_update_tcp_config);

    // PUT /api/config/global - Update global settings
    let update_global_settings = warp::path!("api" / "config" / "global")
        .and(warp::put())
        .and(warp::body::json())
        .and(with_config_manager.clone())
        .and_then(handle_update_global_settings);

    // Combine all routes
    get_config
        .or(reload_config)
        .or(list_routes)
        .or(get_route)
        .or(update_route)
        .or(delete_route)
        .or(update_tcp_config)
        .or(update_global_settings)
        .with(warp::cors().allow_any_origin())
}

/// Handle GET /api/config
async fn handle_get_config(
    config_manager: Arc<DynamicConfigManager>,
) -> Result<impl Reply, Rejection> {
    let config = config_manager.get_config();
    let config_clone = config.read().await.clone();
    
    Ok(warp::reply::json(&ApiResponse::success(
        "Current configuration retrieved",
        Some(config_clone),
    )))
}

/// Handle POST /api/config/reload
async fn handle_reload_config(
    config_manager: Arc<DynamicConfigManager>,
) -> Result<impl Reply, Rejection> {
    match config_manager.reload_from_file().await {
        Ok(_) => Ok(warp::reply::json(&ApiResponse::<()>::success(
            "Configuration reloaded successfully",
            None,
        ))),
        Err(e) => Ok(warp::reply::json(&ApiResponse::<()>::error(
            &format!("Failed to reload configuration: {}", e),
        ))),
    }
}

/// Handle GET /api/config/routes
async fn handle_list_routes(
    config_manager: Arc<DynamicConfigManager>,
) -> Result<impl Reply, Rejection> {
    let config = config_manager.get_config();
    let routes = config.read().await.routes.clone();
    
    Ok(warp::reply::json(&ApiResponse::success(
        "Routes retrieved successfully",
        Some(routes),
    )))
}

/// Handle GET /api/config/routes/{name}
async fn handle_get_route(
    name: String,
    config_manager: Arc<DynamicConfigManager>,
) -> Result<impl Reply, Rejection> {
    let config = config_manager.get_config();
    let routes = &config.read().await.routes;
    
    match routes.get(&name) {
        Some(route) => Ok(warp::reply::json(&ApiResponse::success(
            &format!("Route '{}' retrieved successfully", name),
            Some(route.clone()),
        ))),
        None => Ok(warp::reply::json(&ApiResponse::<RouteConfig>::error(
            &format!("Route '{}' not found", name),
        ))),
    }
}

/// Handle PUT /api/config/routes/{name}
async fn handle_update_route(
    name: String,
    route_req: crate::dynamic_config::api::RouteUpdateRequest,
    config_manager: Arc<DynamicConfigManager>,
) -> Result<impl Reply, Rejection> {
    let route_config: RouteConfig = route_req.into();
    
    // Check if route exists
    let config = config_manager.get_config();
    let exists = config.read().await.routes.contains_key(&name);
    
    let result = if exists {
        config_manager.update_route(&name, route_config).await
    } else {
        config_manager.add_route(&name, route_config).await
    };
    
    match result {
        Ok(_) => {
            let message = if exists {
                format!("Route '{}' updated successfully", name)
            } else {
                format!("Route '{}' created successfully", name)
            };
            
            Ok(warp::reply::json(&ApiResponse::<()>::success(
                &message,
                None,
            )))
        }
        Err(e) => Ok(warp::reply::json(&ApiResponse::<()>::error(
            &format!("Failed to update route: {}", e),
        ))),
    }
}

/// Handle DELETE /api/config/routes/{name}
async fn handle_delete_route(
    name: String,
    config_manager: Arc<DynamicConfigManager>,
) -> Result<impl Reply, Rejection> {
    match config_manager.remove_route(&name).await {
        Ok(_) => Ok(warp::reply::json(&ApiResponse::<()>::success(
            &format!("Route '{}' deleted successfully", name),
            None,
        ))),
        Err(e) => Ok(warp::reply::json(&ApiResponse::<()>::error(
            &format!("Failed to delete route: {}", e),
        ))),
    }
}

/// Handle PUT /api/config/tcp
async fn handle_update_tcp_config(
    tcp_config_req: crate::dynamic_config::api::TcpConfigUpdateRequest,
    config_manager: Arc<DynamicConfigManager>,
) -> Result<impl Reply, Rejection> {
    // Get current TCP config
    let config = config_manager.get_config();
    let mut tcp_config = config.read().await.tcp_proxy.clone();
    
    // Apply updates
    tcp_config_req.apply_to(&mut tcp_config);
    
    // Update the config
    match config_manager.update_tcp_config(tcp_config).await {
        Ok(_) => Ok(warp::reply::json(&ApiResponse::<()>::success(
            "TCP configuration updated successfully",
            None,
        ))),
        Err(e) => Ok(warp::reply::json(&ApiResponse::<()>::error(
            &format!("Failed to update TCP configuration: {}", e),
        ))),
    }
}

/// Handle PUT /api/config/global
async fn handle_update_global_settings(
    settings: crate::dynamic_config::api::GlobalSettingsUpdateRequest,
    config_manager: Arc<DynamicConfigManager>,
) -> Result<impl Reply, Rejection> {
    match config_manager
        .update_global_settings(
            settings.listen_addr,
            settings.global_timeout_ms,
            settings.max_connections,
        )
        .await
    {
        Ok(_) => Ok(warp::reply::json(&ApiResponse::<()>::success(
            "Global settings updated successfully",
            None,
        ))),
        Err(e) => Ok(warp::reply::json(&ApiResponse::<()>::error(
            &format!("Failed to update global settings: {}", e),
        ))),
    }
}