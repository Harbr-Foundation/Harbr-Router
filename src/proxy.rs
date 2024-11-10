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
    
    // Get route configuration
    let route = {
        let config = config.read().await;
        config.routes.get(path)
            .or_else(|| config.routes.get("/"))
            .cloned()
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
    
    // Construct the new URI for the upstream request
    let upstream_authority = upstream_uri.authority().expect("upstream URI must have authority").clone();
    let upstream_scheme = upstream_uri.scheme_str().unwrap_or("http");
    
    // Keep the original path and query
    let path_and_query = req.uri().path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    
    let new_uri = format!("{}://{}{}", upstream_scheme, upstream_authority, path_and_query)
        .parse::<Uri>()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

    // Send request with timeout and retry logic
    let timeout = route.timeout_ms.unwrap_or(3000);
    let retry_count = route.retry_count.unwrap_or(2);
    
    let mut response = None;
    for attempt in 0..=retry_count {
        // Create a new request for each attempt
        let mut proxied_req = create_proxied_request(&req, &new_uri)?;
        
        // Update host header
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

    match response {
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