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
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, RwLock};
use tokio::time::timeout;

type DestinationMap = Arc<DashMap<SocketAddr, String>>;
const MAX_DATAGRAM_SIZE: usize = 65507;

pub struct UdpProxy {
    hosts: Arc<RwLock<Vec<HostConfig>>>,
    destination_map: DestinationMap,
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl UdpProxy {
    pub fn new() -> Self {
        Self {
            hosts: Arc::new(RwLock::new(Vec::new())),
            destination_map: Arc::new(DashMap::new()),
            shutdown_tx: None,
        }
    }
    
    async fn select_host(&self, _client_addr: SocketAddr) -> Option<HostConfig> {
        let hosts = self.hosts.read().await;
        
        if let Some(dest) = self.destination_map.get(&_client_addr) {
            hosts.iter().find(|h| self.extract_host_port(&h.upstream) == *dest).cloned()
        } else {
            let mut weighted_hosts: Vec<_> = hosts.iter()
                .map(|host| (host.clone(), host.weight.unwrap_or(1)))
                .collect();
                
            weighted_hosts.sort_by(|a, b| {
                let a_priority = a.0.priority.unwrap_or(0);
                let b_priority = b.0.priority.unwrap_or(0);
                b_priority.cmp(&a_priority)
            });
            
            let selected = weighted_hosts.into_iter().next().map(|(host, _)| host.clone());
            
            if let Some(ref host) = selected {
                let dest = self.extract_host_port(&host.upstream);
                self.destination_map.insert(_client_addr, dest);
            }
            
            selected
        }
    }
    
    async fn handle_datagram(
        &self,
        socket: Arc<UdpSocket>,
        client_addr: SocketAddr,
        datagram: Vec<u8>,
        tx: mpsc::Sender<(Vec<u8>, SocketAddr, SocketAddr)>,
        start_time: Instant,
    ) -> io::Result<()> {
        let host = match self.select_host(client_addr).await {
            Some(host) => host,
            None => {
                tracing::error!("No hosts configured for UDP proxy");
                return Err(io::Error::new(io::ErrorKind::NotFound, "No hosts configured"));
            }
        };
        
        let destination = self.extract_host_port(&host.upstream);
        let timeout_ms = host.timeout_ms.unwrap_or(5000);
        
        let dest_addr = destination.parse::<SocketAddr>().map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidInput, format!("Invalid destination address: {}", e))
        })?;
        
        match timeout(
            Duration::from_millis(timeout_ms),
            socket.send_to(&datagram, dest_addr)
        ).await {
            Ok(Ok(bytes_sent)) => {
                tracing::debug!("Forwarded {} bytes to {}", bytes_sent, dest_addr);
                counter!("udp_proxy.datagram.forwarded", 1);
            }
            Ok(Err(e)) => {
                tracing::error!("Error forwarding UDP datagram: {}", e);
                counter!("udp_proxy.error", 1);
                return Err(e);
            }
            Err(_) => {
                tracing::warn!("Timeout forwarding UDP datagram to {}", dest_addr);
                counter!("udp_proxy.timeout", 1);
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Timed out forwarding UDP datagram",
                ));
            }
        }
        
        let bind_addr = if socket.local_addr()?.ip().is_unspecified() {
            format!("0.0.0.0:0")
        } else {
            format!("{}:0", socket.local_addr()?.ip())
        };
        
        let response_socket = UdpSocket::bind(bind_addr).await?;
        response_socket.connect(dest_addr).await?;
        
        let mut response_buf = vec![0u8; MAX_DATAGRAM_SIZE];
        match timeout(
            Duration::from_millis(timeout_ms),
            response_socket.recv(&mut response_buf)
        ).await {
            Ok(Ok(size)) => {
                if let Err(e) = tx.send((
                    response_buf[..size].to_vec(),
                    client_addr,
                    dest_addr
                )).await {
                    tracing::error!("Failed to send response through channel: {}", e);
                    counter!("udp_proxy.error", 1);
                }
            }
            Ok(Err(e)) => {
                tracing::error!("Error receiving response: {}", e);
                counter!("udp_proxy.error", 1);
            }
            Err(_) => {
                tracing::debug!("No response received within timeout");
                counter!("udp_proxy.timeout", 1);
            }
        }
        
        let duration = start_time.elapsed();
        histogram!("udp_proxy.datagram.duration", duration.as_secs_f64());
        
        Ok(())
    }
    
    fn extract_host_port(&self, url: &str) -> String {
        let url_without_protocol = url.split("://").nth(1).unwrap_or(url);
        url_without_protocol.split('/').next().unwrap_or(url_without_protocol).to_string()
    }
}

#[async_trait]
impl Proxy for UdpProxy {
    fn proxy_type(&self) -> &'static str {
        "udp"
    }
    
    async fn start(&self, instance: &ProxyInstance) -> Result<()> {
        let addr = SocketAddr::from_str(&instance.listen_addr)?;
        let socket = Arc::new(UdpSocket::bind(addr).await?);
        
        *self.hosts.write().await = instance.hosts.clone();
        
        let (tx, mut rx) = mpsc::channel::<(Vec<u8>, SocketAddr, SocketAddr)>(1000);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        
        let response_socket = socket.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some((data, client_addr, _)) = rx.recv() => {
                        match response_socket.send_to(&data, client_addr).await {
                            Ok(sent) => {
                                tracing::debug!("Sent {} bytes response to {}", sent, client_addr);
                                counter!("udp_proxy.datagram.response_sent", 1);
                            }
                            Err(e) => {
                                tracing::error!("Error sending response to {}: {}", client_addr, e);
                                counter!("udp_proxy.error", 1);
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        break;
                    }
                }
            }
        });
        
        let hosts = self.hosts.clone();
        let destination_map = self.destination_map.clone();
        
        tokio::spawn(async move {
            let mut buf = vec![0u8; MAX_DATAGRAM_SIZE];
            
            loop {
                let timeout_duration = Duration::from_millis(5000);
                
                let receive_result = match timeout(
                    timeout_duration,
                    socket.recv_from(&mut buf)
                ).await {
                    Ok(result) => result,
                    Err(_) => {
                        counter!("udp_proxy.timeout", 1);
                        continue;
                    }
                };
                
                match receive_result {
                    Ok((size, client_addr)) => {
                        tracing::debug!("Received {} bytes from {}", size, client_addr);
                        let start = Instant::now();
                        counter!("udp_proxy.datagram.received", 1);
                        
                        let request_socket = socket.clone();
                        let datagram = buf[..size].to_vec();
                        let tx_clone = tx.clone();
                        let udp_proxy = UdpProxy {
                            hosts: hosts.clone(),
                            destination_map: destination_map.clone(),
                            shutdown_tx: None,
                        };
                        
                        tokio::spawn(async move {
                            if let Err(e) = udp_proxy.handle_datagram(
                                request_socket,
                                client_addr,
                                datagram,
                                tx_clone,
                                start
                            ).await {
                                tracing::error!("Error handling UDP datagram: {}", e);
                                counter!("udp_proxy.error", 1);
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("Error receiving UDP datagram: {}", e);
                        counter!("udp_proxy.error", 1);
                    }
                }
            }
        });
        
        tracing::info!("UDP proxy instance '{}' started on {}", instance.name, addr);
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