use super::{Proxy, ProxyInstance, HostConfig};
use async_trait::async_trait;
use anyhow::Result;
use dashmap::DashMap;
use metrics::{counter, histogram};
use std::io;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, RwLock};
use tokio::time::timeout;

type ConnectionCache = Arc<DashMap<String, Vec<(TcpStream, Instant)>>>;

pub struct TcpProxy {
    hosts: Arc<RwLock<Vec<HostConfig>>>,
    connection_cache: ConnectionCache,
    shutdown_tx: Option<mpsc::Sender<()>>,
    max_idle_time: Duration,
    pooled_connections: bool,
}

impl TcpProxy {
    pub fn new() -> Self {
        Self {
            hosts: Arc::new(RwLock::new(Vec::new())),
            connection_cache: Arc::new(DashMap::new()),
            shutdown_tx: None,
            max_idle_time: Duration::from_secs(60),
            pooled_connections: true,
        }
    }
    
    async fn select_host(&self) -> Option<HostConfig> {
        let hosts = self.hosts.read().await;
        
        let mut weighted_hosts: Vec<_> = hosts.iter()
            .map(|host| (host.clone(), host.weight.unwrap_or(1)))
            .collect();
            
        weighted_hosts.sort_by(|a, b| {
            let a_priority = a.0.priority.unwrap_or(0);
            let b_priority = b.0.priority.unwrap_or(0);
            b_priority.cmp(&a_priority)
        });
        
        weighted_hosts.into_iter().next().map(|(host, _)| host)
    }
    
    async fn handle_connection(
        &self,
        mut client_stream: TcpStream,
        client_addr: SocketAddr,
    ) -> io::Result<()> {
        let host = match self.select_host().await {
            Some(host) => host,
            None => {
                tracing::error!("No hosts configured for TCP proxy");
                return Err(io::Error::new(io::ErrorKind::NotFound, "No hosts configured"));
            }
        };
        
        let upstream_url = &host.upstream;
        let parts: Vec<&str> = upstream_url.split("://").collect();
        let host_port = if parts.len() > 1 {
            parts[1].split('/').next().unwrap_or(parts[1])
        } else {
            upstream_url.split('/').next().unwrap_or(upstream_url)
        };
        
        let start = Instant::now();
        
        let mut server_stream = if self.pooled_connections {
            match self.get_or_create_connection(host_port).await {
                Ok(stream) => stream,
                Err(e) => {
                    tracing::error!("Failed to connect to upstream {}: {}", host_port, e);
                    return Err(e);
                }
            }
        } else {
            match TcpStream::connect(host_port).await {
                Ok(stream) => stream,
                Err(e) => {
                    tracing::error!("Failed to connect to upstream {}: {}", host_port, e);
                    return Err(e);
                }
            }
        };
        
        let timeout_duration = Duration::from_millis(host.timeout_ms.unwrap_or(30000));
        
        let (mut client_read, mut client_write) = client_stream.split();
        let (mut server_read, mut server_write) = server_stream.split();
        
        let mut client_buffer = vec![0u8; 65536];
        let mut server_buffer = vec![0u8; 65536];
        
        loop {
            tokio::select! {
                read_result = timeout(timeout_duration, client_read.read(&mut client_buffer)) => {
                    match read_result {
                        Ok(Ok(0)) => break,
                        Ok(Ok(n)) => {
                            if let Err(e) = server_write.write_all(&client_buffer[..n]).await {
                                tracing::error!("Failed to write to server: {}", e);
                                break;
                            }
                        }
                        Ok(Err(e)) => {
                            tracing::error!("Failed to read from client: {}", e);
                            break;
                        }
                        Err(_) => {
                            tracing::warn!("Client read timeout from {}", client_addr);
                            counter!("tcp_proxy.timeout", 1);
                            break;
                        }
                    }
                }
                read_result = timeout(timeout_duration, server_read.read(&mut server_buffer)) => {
                    match read_result {
                        Ok(Ok(0)) => break,
                        Ok(Ok(n)) => {
                            if let Err(e) = client_write.write_all(&server_buffer[..n]).await {
                                tracing::error!("Failed to write to client: {}", e);
                                break;
                            }
                        }
                        Ok(Err(e)) => {
                            tracing::error!("Failed to read from server: {}", e);
                            break;
                        }
                        Err(_) => {
                            tracing::warn!("Server read timeout");
                            counter!("tcp_proxy.timeout", 1);
                            break;
                        }
                    }
                }
            }
        }
        
        let duration = start.elapsed();
        histogram!("tcp_proxy.connection.duration", duration.as_secs_f64());
        counter!("tcp_proxy.connection.completed", 1);
        
        Ok(())
    }
    
    async fn get_or_create_connection(&self, target: &str) -> io::Result<TcpStream> {
        if let Some(mut connections) = self.connection_cache.get_mut(target) {
            while let Some((conn, timestamp)) = connections.pop() {
                if timestamp.elapsed() < self.max_idle_time {
                    tracing::debug!("Reusing cached connection to {}", target);
                    return Ok(conn);
                }
                tracing::debug!("Discarding expired connection to {}", target);
            }
        }
        
        tracing::debug!("Creating new connection to {}", target);
        let stream = TcpStream::connect(target).await?;
        counter!("tcp_proxy.connection.new", 1);
        Ok(stream)
    }
    
    async fn clean_idle_connections(&self) {
        let now = Instant::now();
        
        for mut entry in self.connection_cache.iter_mut() {
            let connections = entry.value_mut();
            connections.retain(|(_, timestamp)| {
                now.duration_since(*timestamp) < self.max_idle_time
            });
            
            tracing::debug!("{} connections remaining for {}", connections.len(), entry.key());
        }
    }
}

#[async_trait]
impl Proxy for TcpProxy {
    fn proxy_type(&self) -> &'static str {
        "tcp"
    }
    
    async fn start(&self, instance: &ProxyInstance) -> Result<()> {
        let addr = SocketAddr::from_str(&instance.listen_addr)?;
        let listener = TcpListener::bind(addr).await?;
        
        *self.hosts.write().await = instance.hosts.clone();
        
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        
        let cache = self.connection_cache.clone();
        let max_idle = self.max_idle_time;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        // Clean connections periodically
                    }
                    _ = shutdown_rx.recv() => {
                        break;
                    }
                }
            }
        });
        
        let hosts = self.hosts.clone();
        let connection_cache = self.connection_cache.clone();
        let max_idle_time = self.max_idle_time;
        let pooled = self.pooled_connections;
        
        tokio::spawn(async move {
            loop {
                let (client_stream, client_addr) = match listener.accept().await {
                    Ok(connection) => connection,
                    Err(e) => {
                        tracing::error!("Failed to accept connection: {}", e);
                        continue;
                    }
                };
                
                tracing::info!("Accepted TCP connection from {}", client_addr);
                
                let tcp_proxy = TcpProxy {
                    hosts: hosts.clone(),
                    connection_cache: connection_cache.clone(),
                    shutdown_tx: None,
                    max_idle_time,
                    pooled_connections: pooled,
                };
                
                tokio::spawn(async move {
                    if let Err(e) = tcp_proxy.handle_connection(client_stream, client_addr).await {
                        tracing::error!("TCP connection error: {}", e);
                    }
                });
            }
        });
        
        tracing::info!("TCP proxy instance '{}' started on {}", instance.name, addr);
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
    
    fn validate_config(&self, config: &serde_yaml::Value) -> Result<()> {
        if let Some(max_idle_secs) = config.get("max_idle_time_secs") {
            if let Some(secs) = max_idle_secs.as_u64() {
                if secs > 3600 {
                    return Err(anyhow::anyhow!("max_idle_time_secs cannot exceed 3600 seconds"));
                }
            }
        }
        Ok(())
    }
}