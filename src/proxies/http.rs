use super::{Proxy, ProxyInstance, HostConfig};
use async_trait::async_trait;
use anyhow::Result;
use dashmap::DashMap;
use reqwest::Client;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use warp::{Filter, Reply};
use std::net::SocketAddr;
use std::str::FromStr;
use metrics::{counter, histogram};
use std::time::Instant;

pub struct HttpProxy {
    hosts: Arc<RwLock<Vec<HostConfig>>>,
    backend_cache: Arc<DashMap<String, Client>>,
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl HttpProxy {
    pub fn new() -> Self {
        Self {
            hosts: Arc::new(RwLock::new(Vec::new())),
            backend_cache: Arc::new(DashMap::new()),
            shutdown_tx: None,
        }
    }
    
    async fn select_host(&self, path: &str) -> Option<HostConfig> {
        let hosts = self.hosts.read().await;
        
        let mut matching_hosts: Vec<_> = hosts.iter()
            .filter(|host| {
                if host.name == "/" {
                    true
                } else {
                    path.starts_with(&host.name)
                }
            })
            .cloned()
            .collect();
            
        matching_hosts.sort_by(|a, b| {
            let a_priority = a.priority.unwrap_or(0);
            let b_priority = b.priority.unwrap_or(0);
            
            b_priority.cmp(&a_priority)
                .then_with(|| b.name.len().cmp(&a.name.len()))
        });
        
        matching_hosts.into_iter().next()
    }
    
    async fn handle_request(
        &self,
        path: warp::path::FullPath,
        method: warp::http::Method,
        headers: warp::http::header::HeaderMap,
        query: String,
        body: bytes::Bytes,
    ) -> Result<impl Reply, warp::Rejection> {
        let start = Instant::now();
        let path_str = path.as_str();
        
        let host = match self.select_host(path_str).await {
            Some(host) => host,
            None => {
                counter!("http_proxy.route_not_found", 1);
                return Ok(warp::http::Response::builder()
                    .status(404)
                    .body("Route not found".into())
                    .unwrap());
            }
        };
        
        let client = self.backend_cache
            .entry(host.upstream.clone())
            .or_insert_with(|| {
                Client::builder()
                    .timeout(std::time::Duration::from_millis(
                        host.timeout_ms.unwrap_or(30000)
                    ))
                    .build()
                    .expect("Failed to create HTTP client")
            })
            .value()
            .clone();
            
        let upstream_uri = reqwest::Url::from_str(&host.upstream)
            .map_err(|_| warp::reject::custom(InvalidUpstreamUrl))?;
            
        let mut final_url = upstream_uri.clone();
        let upstream_path = upstream_uri.path().trim_end_matches('/');
        
        let request_path = if host.name == "/" {
            path_str
        } else {
            &path_str[host.name.len()..]
        }
        .trim_start_matches('/');
        
        let final_path = if upstream_path.is_empty() || upstream_path == "/" {
            format!("/{}", request_path)
        } else {
            format!("{}/{}", upstream_path, request_path)
        }
        .trim_end_matches('/')
        .to_string();
        
        final_url.set_path(&final_path);
        
        if !query.is_empty() {
            let clean_query = query.trim_start_matches('?');
            if !clean_query.is_empty() {
                final_url.set_query(Some(clean_query));
            }
        }
        
        let timeout_ms = host.timeout_ms.unwrap_or(30000);
        let retry_count = host.retry_count.unwrap_or(0);
        
        let mut response = None;
        for attempt in 0..=retry_count {
            let mut req_builder = client
                .request(self.convert_method(method.clone()), final_url.clone())
                .timeout(std::time::Duration::from_millis(timeout_ms))
                .headers(self.convert_headers(&headers))
                .body(body.clone());
                
            if !host.preserve_host_header.unwrap_or(false) {
                if let Some(host_str) = final_url.host_str() {
                    if let Ok(host_value) = reqwest::header::HeaderValue::from_str(host_str) {
                        req_builder = req_builder.header(reqwest::header::HOST, host_value);
                    }
                }
            }
            
            match req_builder.send().await {
                Ok(resp) => {
                    response = Some(resp);
                    counter!("http_proxy.attempt.success", 1);
                    break;
                }
                Err(e) => {
                    tracing::warn!("Request failed (attempt {}): {}", attempt + 1, e);
                    counter!("http_proxy.attempt.failure", 1);
                }
            }
        }
        
        let duration = start.elapsed();
        histogram!("http_proxy.request.duration", duration.as_secs_f64());
        
        match response {
            Some(resp) => {
                counter!("http_proxy.request.success", 1);
                
                let status = resp.status().as_u16();
                let mut builder = warp::http::Response::builder().status(status);
                
                if let Some(headers) = builder.headers_mut() {
                    for (key, value) in resp.headers() {
                        if let Ok(name) = warp::http::header::HeaderName::from_bytes(key.as_str().as_bytes()) {
                            if let Ok(val) = warp::http::header::HeaderValue::from_bytes(value.as_bytes()) {
                                headers.insert(name, val);
                            }
                        }
                    }
                }
                
                let body_bytes = resp.bytes().await.map_err(|_| warp::reject::custom(UpstreamError))?;
                
                Ok(builder.body(body_bytes).map_err(|_| warp::reject::custom(ResponseError))?)
            }
            None => {
                counter!("http_proxy.request.error", 1);
                Ok(warp::http::Response::builder()
                    .status(503)
                    .body("Service Unavailable".into())
                    .unwrap())
            }
        }
    }
    
    fn convert_method(&self, method: warp::http::Method) -> reqwest::Method {
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
    
    fn convert_headers(&self, headers: &warp::http::header::HeaderMap) -> reqwest::header::HeaderMap {
        let mut reqwest_headers = reqwest::header::HeaderMap::new();
        for (name, value) in headers.iter() {
            if let Ok(reqwest_name) = reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()) {
                if let Ok(reqwest_value) = reqwest::header::HeaderValue::from_bytes(value.as_bytes()) {
                    reqwest_headers.insert(reqwest_name, reqwest_value);
                }
            }
        }
        reqwest_headers
    }
}

#[async_trait]
impl Proxy for HttpProxy {
    fn proxy_type(&self) -> &'static str {
        "http"
    }
    
    async fn start(&self, instance: &ProxyInstance) -> Result<()> {
        let addr = SocketAddr::from_str(&instance.listen_addr)?;
        
        *self.hosts.write().await = instance.hosts.clone();
        
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        
        let hosts = self.hosts.clone();
        let backend_cache = self.backend_cache.clone();
        
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
            .and_then(move |path, method, headers, query, body| {
                let hosts = hosts.clone();
                let backend_cache = backend_cache.clone();
                async move {
                    HttpProxy::new().handle_request(path, method, headers, query, body).await
                }
            });
            
        let (_, server) = warp::serve(routes)
            .bind_with_graceful_shutdown(addr, async move {
                let _ = shutdown_rx.recv().await;
            });
            
        tokio::spawn(server);
        
        tracing::info!("HTTP proxy instance '{}' started on {}", instance.name, addr);
        Ok(())
    }
    
    async fn stop(&self) -> Result<()> {
        if let Some(tx) = &self.shutdown_tx {
            let _ = tx.send(()).await;
        }
        Ok(())
    }
    
    async fn add_host(&self, host: HostConfig) -> Result<()> {
        let mut hosts = self.hosts.write().await;
        if hosts.iter().any(|h| h.name == host.name) {
            return Err(anyhow::anyhow!("Host '{}' already exists", host.name));
        }
        hosts.push(host);
        Ok(())
    }
    
    async fn remove_host(&self, host_name: &str) -> Result<()> {
        let mut hosts = self.hosts.write().await;
        if let Some(pos) = hosts.iter().position(|h| h.name == host_name) {
            hosts.remove(pos);
            Ok(())
        } else {
            Err(anyhow::anyhow!("Host '{}' not found", host_name))
        }
    }
    
    async fn update_host(&self, host: HostConfig) -> Result<()> {
        let mut hosts = self.hosts.write().await;
        if let Some(existing) = hosts.iter_mut().find(|h| h.name == host.name) {
            *existing = host;
            Ok(())
        } else {
            Err(anyhow::anyhow!("Host '{}' not found", host.name))
        }
    }
    
    async fn get_hosts(&self) -> Result<Vec<HostConfig>> {
        Ok(self.hosts.read().await.clone())
    }
    
    async fn health_check(&self) -> Result<bool> {
        Ok(true)
    }
    
    fn validate_config(&self, _config: &serde_yaml::Value) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct InvalidUpstreamUrl;
impl warp::reject::Reject for InvalidUpstreamUrl {}

#[derive(Debug)]
struct UpstreamError;
impl warp::reject::Reject for UpstreamError {}

#[derive(Debug)]
struct ResponseError;
impl warp::reject::Reject for ResponseError {}