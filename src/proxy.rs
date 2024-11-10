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

type SharedConfig = Arc<RwLock<config::ProxyConfig>>;
type BackendCache = Arc<DashMap<String, hyper::Client<hyper::client::HttpConnector>>>;

pub async fn run_server(config: SharedConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let addr = config.read().await.listen_addr.parse()?;
    let backend_cache: BackendCache = Arc::new(DashMap::new());

    // Create service
    let make_svc = make_service_fn(move |_| {
        let config = config.clone();
        let cache = backend_cache.clone();
        
        async move {
            Ok::<_, Infallible>(service_fn(move |req| {
                handle_request(req, config.clone(), cache.clone())
            }))
        }
    });

    // Build and run server
    let server = Server::bind(&addr)
        .serve(make_svc);

    let graceful = server.with_graceful_shutdown(shutdown_signal());

    tracing::info!("Reverse proxy listening on {}", addr);
    graceful.await?;
    Ok(())
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
        .or_insert_with(|| hyper::Client::new())
        .value()
        .clone();

    // Parse upstream URI
    let upstream_uri = Uri::from_str(&route.upstream)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    
    // Create new request for each attempt since we can't clone the original
    let create_upstream_request = || -> Result<Request<Body>, Box<dyn std::error::Error + Send + Sync>> {
        let mut builder = Request::builder()
            .method(req.method())
            .uri(&upstream_uri)
            .version(req.version());
        
        // Copy headers
        for (key, value) in req.headers() {
            builder = builder.header(key, value);
        }
        
        Ok(builder.body(Body::empty())?)
    };

    // Send request with timeout and retry logic
    let timeout = route.timeout_ms.unwrap_or(3000);
    let retry_count = route.retry_count.unwrap_or(2);
    
    let mut response = None;
    for attempt in 0..=retry_count {
        let upstream_req = create_upstream_request()?;
        
        match tokio::time::timeout(
            std::time::Duration::from_millis(timeout),
            client.request(upstream_req)
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