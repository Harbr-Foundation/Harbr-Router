use std::sync::Arc;
use tokio::sync::RwLock;
use hyper::{Body, Request, Response, Server, Uri};
use hyper::service::{make_service_fn, service_fn};
use hyper::header::{HeaderName, HeaderValue, HOST};
use metrics::{counter, histogram};
use std::time::Instant;
use dashmap::DashMap;
use std::convert::Infallible;
use crate::config;
use std::str::FromStr;

type SharedConfig = Arc<RwLock<config::ProxyConfig>>;
type BackendCache = Arc<DashMap<String, hyper::Client<hyper::client::HttpConnector>>>;

pub async fn run_server(config: SharedConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let addr = config.read().await.listen_addr.parse()?;
    let backend_cache: BackendCache = Arc::new(DashMap::new());

    let make_svc = make_service_fn(move |_| {
        let config = config.clone();
        let cache = backend_cache.clone();
        
        async move {
            Ok::<_, Infallible>(service_fn(move |req| {
                handle_request(req, config.clone(), cache.clone())
            }))
        }
    });

    let server = Server::bind(&addr).serve(make_svc);
    let graceful = server.with_graceful_shutdown(shutdown_signal());

    tracing::info!("Reverse proxy listening on {}", addr);
    graceful.await?;
    Ok(())
}

// Helper function to find best matching route
fn find_matching_route(
    path: &str,
    routes: &std::collections::HashMap<String, config::RouteConfig>
) -> Option<(String, config::RouteConfig)> {
    let mut route_paths: Vec<_> = routes.iter().collect();
    route_paths.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    
    for (route_path, route_config) in route_paths {
        if path.starts_with(route_path) {
            return Some((route_path.clone(), route_config.clone()));
        }
    }
    None
}

async fn handle_request(
    req: Request<Body>,
    config: SharedConfig,
    backend_cache: BackendCache,
) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let start = Instant::now();
    let path = req.uri().path().to_string();
    
    tracing::debug!("Processing request for path: {}", path);

    // Get route configuration with priority matching
    let (route_path, route) = {
        let config = config.read().await;
        find_matching_route(&path, &config.routes)
            .ok_or_else(|| std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("No route found for path: {}", path)
            ))?
    };

    // Get or create backend client
    let client = backend_cache
        .entry(route.upstream.clone())
        .or_insert_with(|| {
            let mut connector = hyper::client::HttpConnector::new();
            connector.set_nodelay(true);
            hyper::Client::builder()
                .pool_idle_timeout(std::time::Duration::from_secs(30))
                .build(connector)
        })
        .value()
        .clone();

    // Parse upstream URI
    let upstream_uri = Uri::from_str(&route.upstream)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    
    // Build the new request
    let (parts, body) = req.into_parts();
    let new_path = if route.strip_path_prefix {
        if path.starts_with(&route_path) {
            let stripped = &path[route_path.len()..];
            if stripped.is_empty() { "/" } else { stripped }
        } else {
            &path
        }
    } else {
        &path
    };

    // Clone method and create URI once
    let method = parts.method.clone();
    let version = parts.version;
    let new_uri = if let Some(query) = parts.uri.query() {
        format!("{}{}?{}", upstream_uri, new_path, query)
    } else {
        format!("{}{}", upstream_uri, new_path)
    }.parse::<Uri>()?;

    // Store headers in a way that can be reused
    let headers_vec: Vec<(HeaderName, HeaderValue)> = parts.headers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let host_header = if !route.preserve_host_header.unwrap_or(false) {
        Some(HeaderValue::from_str(
            upstream_uri.authority()
                .expect("upstream URI must have authority")
                .as_str()
        ).ok())
    } else {
        None
    };

    // Aggregate the body into bytes for potential retries
    let body_bytes = hyper::body::to_bytes(body).await?;
    
    // Create a function to build the request
    let build_request = |body: hyper::body::Bytes| -> Result<Request<Body>, Box<dyn std::error::Error + Send + Sync>> {
        let mut builder = Request::builder()
            .method(method.clone())
            .uri(new_uri.clone())
            .version(version);

        let headers = builder.headers_mut().unwrap();
        for (key, value) in &headers_vec {
            headers.insert(key, value.clone());
        }

        if let Some(Some(host)) = &host_header {
            headers.insert(HOST, host.clone());
        }

        Ok(builder.body(Body::from(body))?)
    };

    let timeout = route.timeout_ms.unwrap_or(3000);
    let retry_count = route.retry_count.unwrap_or(2);
    
    let mut final_response = None;
    
    // Try the request with retries
    for attempt in 0..=retry_count {
        let request = build_request(body_bytes.clone())?;
        
        match tokio::time::timeout(
            std::time::Duration::from_millis(timeout),
            client.request(request)
        ).await {
            Ok(Ok(resp)) => {
                final_response = Some(resp);
                counter!("proxy.attempt.success", 1);
                break;
            }
            Ok(Err(e)) => {
                tracing::warn!("Request failed (attempt {}): {}", attempt + 1, e);
                counter!("proxy.attempt.failure", 1);
                
                // Add a small delay between retries
                if attempt < retry_count {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
            Err(_) => {
                tracing::warn!("Request timed out (attempt {})", attempt + 1);
                counter!("proxy.attempt.timeout", 1);
                
                // Add a small delay between retries
                if attempt < retry_count {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        }
    }

    // Record metrics
    let duration = start.elapsed();
    histogram!("proxy.request.duration", duration.as_secs_f64());

    match final_response {
        Some(resp) => {
            counter!("proxy.request.success", 1);
            Ok(resp)
        }
        None => {
            counter!("proxy.request.error", 1);
            Ok(Response::builder()
                .status(503)
                .body(Body::from("Service Unavailable"))?
            )
        }
    }
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to install CTRL+C signal handler");
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::{StatusCode, Method};
    use std::collections::HashMap;
    
    #[tokio::test]
    async fn test_find_matching_route() {
        let mut routes = HashMap::new();
        routes.insert("/api/v1".to_string(), config::RouteConfig {
            upstream: "http://backend1".to_string(),
            strip_path_prefix: true,
            preserve_host_header: Some(false),
            timeout_ms: Some(1000),
            retry_count: Some(1),
        });
        routes.insert("/api".to_string(), config::RouteConfig {
            upstream: "http://backend2".to_string(),
            strip_path_prefix: false,
            preserve_host_header: Some(true),
            timeout_ms: None,
            retry_count: None,
        });

        // Test exact match
        let (path, route) = find_matching_route("/api/v1", &routes).unwrap();
        assert_eq!(path, "/api/v1");
        assert_eq!(route.upstream, "http://backend1");

        // Test partial match
        let (path, route) = find_matching_route("/api/other", &routes).unwrap();
        assert_eq!(path, "/api");
        assert_eq!(route.upstream, "http://backend2");

        // Test no match
        assert!(find_matching_route("/other", &routes).is_none());
    }
}