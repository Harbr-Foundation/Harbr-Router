use crate::config;
use dashmap::DashMap;
use metrics::{counter, histogram};
use reqwest::Client;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use warp::http::header::HeaderMap;
use warp::{path::FullPath, Filter, Rejection, Reply};

type SharedConfig = Arc<RwLock<config::ProxyConfig>>;
type BackendCache = Arc<DashMap<String, Client>>;

pub async fn run_server(
    config: SharedConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Parse the socket address
    let addr = SocketAddr::from_str(&config.read().await.listen_addr)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

    let backend_cache: BackendCache = Arc::new(DashMap::new());

    // Create the warp filter
    let routes = warp::any()
        .and(warp::path::full())
        .and(warp::method())
        .and(warp::header::headers_cloned())
        .and(
            warp::filters::query::raw()
                .or(warp::any().map(String::new))
                .unify(),
        )
        .and(warp::body::bytes())
        .and(with_config(config.clone()))
        .and(with_cache(backend_cache.clone()))
        .and_then(handle_request);

    // Start the server with graceful shutdown
    let (_, server) = warp::serve(routes).bind_with_graceful_shutdown(addr, async {
        shutdown_signal().await;
    });

    tracing::info!("Reverse proxy listening on {}", addr);
    server.await;
    Ok(())
}

fn with_config(
    config: SharedConfig,
) -> impl Filter<Extract = (SharedConfig,), Error = Infallible> + Clone {
    warp::any().map(move || config.clone())
}

fn with_cache(
    cache: BackendCache,
) -> impl Filter<Extract = (BackendCache,), Error = Infallible> + Clone {
    warp::any().map(move || cache.clone())
}

// Convert warp/http types to reqwest types
fn convert_method(method: warp::http::Method) -> reqwest::Method {
    match method.as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        "DELETE" => reqwest::Method::DELETE,
        "HEAD" => reqwest::Method::HEAD,
        "OPTIONS" => reqwest::Method::OPTIONS,
        "CONNECT" => reqwest::Method::CONNECT,
        "PATCH" => reqwest::Method::PATCH,
        "TRACE" => reqwest::Method::TRACE,
        _ => {
            reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::GET)
        }
    }
}

fn convert_headers(headers: &HeaderMap) -> reqwest::header::HeaderMap {
    let mut reqwest_headers = reqwest::header::HeaderMap::new();
    for (name, value) in headers.iter() {
        if let Ok(reqwest_name) = reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes())
        {
            if let Ok(reqwest_value) = reqwest::header::HeaderValue::from_bytes(value.as_bytes()) {
                reqwest_headers.insert(reqwest_name, reqwest_value);
            }
        }
    }
    reqwest_headers
}

async fn handle_request(
    path: FullPath,
    method: warp::http::Method,
    headers: HeaderMap,
    query: String,
    body: bytes::Bytes,
    config: SharedConfig,
    backend_cache: BackendCache,
) -> Result<impl Reply, Rejection> {
    let start = Instant::now();
    let path_str = path.as_str();

    tracing::info!("Processing request for path: {}", path_str);

    // Get route configuration with priority handling
    let route = {
        let config_guard = config.read().await;

        // Normalize the request path
        let normalized_path = path_str.trim_end_matches('/');
        tracing::info!("Normalized path: {}", normalized_path);

        // Find all matching routes
        let mut matching_routes: Vec<_> = config_guard
            .routes
            .iter()
            .filter_map(|(route_path, route)| {
                let normalized_route = route_path.trim_end_matches('/');

                let is_match = if route_path == "/" {
                    true
                } else {
                    normalized_path == normalized_route
                        || normalized_path.starts_with(&format!("{}/", normalized_route))
                };

                if is_match {
                    Some((
                        route_path.to_string(),
                        route.clone(),
                        normalized_route.matches('/').count(),
                        normalized_route.len(),
                    ))
                } else {
                    None
                }
            })
            .collect();

        matching_routes.sort_by(|a, b| {
            let a_priority = a.1.priority.unwrap_or(0);
            let b_priority = b.1.priority.unwrap_or(0);

            b_priority
                .cmp(&a_priority)
                .then(b.2.cmp(&a.2))
                .then(b.3.cmp(&a.3))
        });

        matching_routes
            .into_iter()
            .next()
            .ok_or_else(|| {
                let err = format!("No route found for path: {}", normalized_path);
                tracing::error!("{}", err);
                warp::reject::custom(RouteNotFound)
            })
            .map(|(route_path, route_config, _, _)| (route_path, route_config))?
    };

    // Get or create backend client
    let client = backend_cache
        .entry(route.1.upstream.clone())
        .or_insert_with(|| {
            Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Failed to create HTTP client")
        })
        .value()
        .clone();

    // Parse upstream URI
    let upstream_uri = reqwest::Url::from_str(&route.1.upstream)
        .map_err(|_| warp::reject::custom(InvalidUpstreamUrl))?;

    // Build the final URL
    let mut final_url = upstream_uri.clone();

    // Handle path joining properly
    let upstream_path = upstream_uri.path().trim_end_matches('/');

    // Strip the matching route prefix from the request path
    let request_path = if route.0 == "/" {
        path_str
    } else {
        &path_str[route.0.len()..]
    }
    .trim_start_matches('/');

    // If upstream path is not empty and not just "/", combine it with request path
    let final_path = if upstream_path.is_empty() || upstream_path == "/" {
        format!("/{}", request_path)
    } else {
        format!("{}/{}", upstream_path, request_path)
    }
    .trim_end_matches('/')
    .to_string();

    final_url.set_path(&final_path);

    // Handle query string properly - strip any leading '?' if present
    if !query.is_empty() {
        let clean_query = query.trim_start_matches('?');
        if !clean_query.is_empty() {
            final_url.set_query(Some(clean_query));
        }
    }

    tracing::info!("Forwarding to upstream URL: {}", final_url);

    // Send request with timeout and retry logic
    let timeout = route.1.timeout_ms.unwrap_or(3000);
    let retry_count = route.1.retry_count.unwrap_or(2);

    let mut response = None;
    for attempt in 0..=retry_count {
        let mut req_builder = client
            .request(convert_method(method.clone()), final_url.clone())
            .timeout(std::time::Duration::from_millis(timeout))
            .headers(convert_headers(&headers))
            .body(body.clone());

        // Handle Host header based on preserve_host_header setting
        if !route.1.preserve_host_header.unwrap_or(false) {
            // Only set the host header to upstream host if we're not preserving the original
            if let Some(host) = final_url.host_str() {
                if let Ok(host_value) = reqwest::header::HeaderValue::from_str(host) {
                    req_builder = req_builder.header(reqwest::header::HOST, host_value);
                }
            }
        }

        match req_builder.send().await {
            Ok(resp) => {
                response = Some(resp);
                counter!("proxy.attempt.success", 1);
                break;
            }
            Err(e) => {
                tracing::warn!("Request failed (attempt {}): {}", attempt + 1, e);
                counter!("proxy.attempt.failure", 1);
            }
        }
    }

    // Record metrics
    let duration = start.elapsed();
    histogram!("proxy.request.duration", duration.as_secs_f64());
    counter!(
        "proxy.request.priority",
        route.1.priority.unwrap_or(0) as u64
    );

    match response {
        Some(resp) => {
            counter!("proxy.request.success", 1);
            tracing::info!(
                "Successfully proxied request for path: {} -> {}",
                path_str,
                final_url
            );

            // Convert reqwest response to warp response
            let status = resp.status().as_u16();
            let mut builder = warp::http::Response::builder().status(status);

            // Convert response headers
            if let Some(headers) = builder.headers_mut() {
                for (key, value) in resp.headers() {
                    if let Ok(name) =
                        warp::http::header::HeaderName::from_bytes(key.as_str().as_bytes())
                    {
                        if let Ok(val) =
                            warp::http::header::HeaderValue::from_bytes(value.as_bytes())
                        {
                            headers.insert(name, val);
                        }
                    }
                }
            }

            let body_bytes = resp
                .bytes()
                .await
                .map_err(|_| warp::reject::custom(UpstreamError))?;

            Ok(builder
                .body(body_bytes)
                .map_err(|_| warp::reject::custom(ResponseError))?)
        }
        None => {
            counter!("proxy.request.error", 1);
            tracing::error!("Failed to proxy request for path: {}", path_str);
            Ok(warp::http::Response::builder()
                .status(503)
                .body("Service Unavailable".into())
                .unwrap())
        }
    }
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to install CTRL+C signal handler");
}

// Custom rejection types
#[derive(Debug)]
struct RouteNotFound;
impl warp::reject::Reject for RouteNotFound {}

#[derive(Debug)]
struct InvalidUpstreamUrl;
impl warp::reject::Reject for InvalidUpstreamUrl {}

#[derive(Debug)]
struct UpstreamError;
impl warp::reject::Reject for UpstreamError {}

#[derive(Debug)]
struct ResponseError;
impl warp::reject::Reject for ResponseError {}
