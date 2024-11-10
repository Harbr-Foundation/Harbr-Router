use std::sync::Arc;
use tokio::sync::RwLock;
use hyper::{Body, Request, Response, Server, Uri};
use hyper::service::{make_service_fn, service_fn};
use metrics::{counter, histogram};
use std::time::Instant;
use dashmap::DashMap;
use std::convert::Infallible;
use crate::config;
use std::str::FromStr;
use hyper::header::{HeaderValue, HOST};

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

    let server = Server::bind(&addr)
        .serve(make_svc);

    let graceful = server.with_graceful_shutdown(shutdown_signal());

    tracing::info!("Reverse proxy listening on {}", addr);
    graceful.await?;
    Ok(())
}

// Helper function to create a request
fn create_proxied_request(
    original_req: &Request<Body>,
    uri: &Uri,
) -> Result<Request<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let mut builder = Request::builder()
        .method(original_req.method())
        .uri(uri)
        .version(original_req.version());
    
    // Copy headers
    let headers = builder.headers_mut().unwrap();
    for (key, value) in original_req.headers() {
        headers.insert(key, value.clone());
    }
    
    Ok(builder.body(Body::empty())?)
}

async fn handle_request(
    req: Request<Body>,
    config: SharedConfig,
    backend_cache: BackendCache,
) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let start = Instant::now();
    let path = req.uri().path();
    
    tracing::info!("Processing request for path: {}", path);

    // Get route configuration with priority handling
    let route = {
        let config_guard = config.read().await;
        
        // Normalize the request path
        let normalized_path = path.trim_end_matches('/');
        tracing::info!("Normalized path: {}", normalized_path);
        
        // Find all matching routes
        let mut matching_routes: Vec<_> = config_guard.routes.iter()
            .filter_map(|(route_path, route)| {
                let normalized_route = route_path.trim_end_matches('/');
                
                // Root path always matches but with lowest priority
                let is_match = if route_path == "/" {
                    true
                } else {
                    normalized_path == normalized_route || 
                    normalized_path.starts_with(&format!("{}/", normalized_route))
                };

                if is_match {
                    tracing::info!("Route '{}' matches path '{}'", route_path, normalized_path);
                    Some((
                        route_path.to_string(),
                        route.clone(),
                        normalized_route.matches('/').count(),
                        normalized_route.len()
                    ))
                } else {
                    tracing::debug!("Route '{}' does not match path '{}'", route_path, normalized_path);
                    None
                }
            })
            .collect();

        tracing::info!("Found {} matching routes", matching_routes.len());

        // Sort routes by:
        // 1. Priority (highest first)
        // 2. Path segments count (most specific first)
        // 3. Path length (longest first)
        matching_routes.sort_by(|a, b| {
            let a_priority = a.1.priority.unwrap_or(0);
            let b_priority = b.1.priority.unwrap_or(0);
            
            b_priority.cmp(&a_priority)
                .then(b.2.cmp(&a.2))
                .then(b.3.cmp(&a.3))
        });

        // Log all candidate routes
        for (route_path, route_config, segments, length) in &matching_routes {
            tracing::info!(
                "Candidate route: path='{}', priority={}, segments={}, length={}", 
                route_path,
                route_config.priority.unwrap_or(0),
                segments,
                length
            );
        }

        matching_routes.into_iter()
            .next()
            .map(|(_, route, _, _)| route)
            .ok_or_else(|| {
                let err = format!("No route found for path: {}", normalized_path);
                tracing::error!("{}", err);
                std::io::Error::new(std::io::ErrorKind::NotFound, err)
            })?
    };

    // Log selected route
    tracing::info!(
        "Selected route: upstream={}, priority={}", 
        route.upstream,
        route.priority.unwrap_or(0)
    );

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
    
    // Get base components of upstream URL
    let upstream_authority = upstream_uri.authority()
        .expect("upstream URI must have authority")
        .clone();
    let upstream_scheme = upstream_uri.scheme_str().unwrap_or("http");
    let upstream_path = upstream_uri.path().trim_end_matches('/');

    // Get request components
    let request_query = req.uri()
        .query()
        .map(|q| format!("?{}", q))
        .unwrap_or_default();

    // Build the final URL
    let final_url = if upstream_path == "" {
        // If upstream has no path, just use the request path
        format!("{}://{}{}{}", 
            upstream_scheme, 
            upstream_authority, 
            path,
            request_query
        )
    } else {
        // If upstream has a path, append the request path
        format!("{}://{}{}{}{}", 
            upstream_scheme, 
            upstream_authority, 
            upstream_path,
            path,
            request_query
        )
    };

    let new_uri = final_url.parse::<Uri>()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

    tracing::info!("Forwarding to upstream URL: {}", new_uri);

    // Send request with timeout and retry logic
    let timeout = route.timeout_ms.unwrap_or(3000);
    let retry_count = route.retry_count.unwrap_or(2);
    
    let mut response = None;
    for attempt in 0..=retry_count {
        let mut proxied_req = create_proxied_request(&req, &new_uri)?;
        
        if let Ok(host) = HeaderValue::from_str(upstream_authority.as_str()) {
            proxied_req.headers_mut().insert(HOST, host);
        }

        match tokio::time::timeout(
            std::time::Duration::from_millis(timeout),
            client.request(proxied_req)
        ).await {
            Ok(Ok(resp)) => {
                response = Some(resp);
                counter!("proxy.attempt.success", 1);
                break;
            }
            Ok(Err(e)) => {
                tracing::warn!("Request failed (attempt {}): {}", attempt + 1, e);
                counter!("proxy.attempt.failure", 1);
            }
            Err(_) => {
                tracing::warn!("Request timed out (attempt {})", attempt + 1);
                counter!("proxy.attempt.timeout", 1);
            }
        }
    }

    // Record metrics
    let duration = start.elapsed();
    histogram!("proxy.request.duration", duration.as_secs_f64());
    counter!("proxy.request.priority", route.priority.unwrap_or(0) as u64);

    match response {
        Some(resp) => {
            counter!("proxy.request.success", 1);
            tracing::info!("Successfully proxied request for path: {} -> {}", path, new_uri);
            Ok(resp)
        }
        None => {
            counter!("proxy.request.error", 1);
            tracing::error!("Failed to proxy request for path: {}", path);
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