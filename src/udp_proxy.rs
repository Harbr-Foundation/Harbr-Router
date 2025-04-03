// src/udp_proxy.rs
use crate::config::{ProxyConfig, RouteConfig};
use dashmap::DashMap;
use metrics::{counter, histogram};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, RwLock};
use tokio::time::timeout;

type SharedConfig = Arc<RwLock<ProxyConfig>>;
type DestinationMap = Arc<DashMap<SocketAddr, String>>;

const MAX_DATAGRAM_SIZE: usize = 65507; // Maximum UDP datagram size

pub struct UdpProxyServer {
    config: SharedConfig,
    destination_map: DestinationMap,
}

impl UdpProxyServer {
    pub fn new(config: SharedConfig) -> Self {
        Self {
            config,
            destination_map: Arc::new(DashMap::new()),
        }
    }

    pub async fn run(&self, addr: &str) -> io::Result<()> {
        // Initialize the UDP socket for listening
        let socket = Arc::new(UdpSocket::bind(addr).await?);
        tracing::info!("UDP proxy listening on {}", addr);

        // Create a channel for response handling
        let (tx, mut rx) = mpsc::channel::<(Vec<u8>, SocketAddr, SocketAddr)>(1000);
        
        // Spawn a task for handling responses
        let response_socket = socket.clone();
        tokio::spawn(async move {
            while let Some((data, client_addr, _)) = rx.recv().await {
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
        });

        // Buffer for receiving datagrams
        let mut buf = vec![0u8; MAX_DATAGRAM_SIZE];

        // Main processing loop
        loop {
            // Set up timeout based on global config
            let timeout_ms = {
                let config_guard = self.config.read().await;
                config_guard.global_timeout_ms
            };
            let timeout_duration = Duration::from_millis(timeout_ms);

            // Try to receive a datagram with timeout
            let receive_result = match timeout(
                timeout_duration, 
                socket.recv_from(&mut buf)
            ).await {
                Ok(result) => result,
                Err(_) => {
                    // Timeout occurred, just continue the loop
                    counter!("udp_proxy.timeout", 1);
                    continue;
                }
            };

            // Process the received datagram
            match receive_result {
                Ok((size, client_addr)) => {
                    tracing::debug!("Received {} bytes from {}", size, client_addr);
                    let start = Instant::now();
                    counter!("udp_proxy.datagram.received", 1);

                    // Clone required resources for the handler task
                    let request_socket = socket.clone();
                    let config_clone = self.config.clone();
                    let destination_map = self.destination_map.clone();
                    let datagram = buf[..size].to_vec();
                    let tx_clone = tx.clone();

                    // Process the datagram in a separate task
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_datagram(
                            request_socket,
                            client_addr,
                            datagram,
                            config_clone,
                            destination_map,
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
    }

    async fn handle_datagram(
        socket: Arc<UdpSocket>,
        client_addr: SocketAddr,
        datagram: Vec<u8>,
        config: SharedConfig,
        destination_map: DestinationMap,
        tx: mpsc::Sender<(Vec<u8>, SocketAddr, SocketAddr)>,
        start_time: Instant,
    ) -> io::Result<()> {
        // Determine the route and upstream destination
        let destination = {
            // Check if we already have a mapping for this client
            if let Some(dest) = destination_map.get(&client_addr) {
                dest.clone()
            } else {

                // ToDo: For UDP, we need to determine the route based on client info
                // or the contents of the first packet
                // This is a simplified implementation
              
                // For now, use the first UDP route in the config
                let config_guard = config.read().await;
                let udp_route = config_guard.routes.iter()
                    .find(|(_, route)| {
                        route.is_udp.unwrap_or(false)
                    });

                if let Some((_, route)) = udp_route {
                    // Extract host and port from the upstream URL
                    let upstream_url = &route.upstream;
                    let dest = extract_host_port(upstream_url);
                    
                    // Store the mapping for future datagrams from this client
                    destination_map.insert(client_addr, dest.clone());
                    
                    dest
                } else {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "No UDP route configured",
                    ));
                }
            }
        };

        // Get timeout from configuration
        let timeout_ms = {
            let config_guard = config.read().await;
            let default_timeout = config_guard.global_timeout_ms;

            // Try to find a specific route config for this destination
            config_guard.routes.iter()
                .find(|(_, route)| {
                    extract_host_port(&route.upstream) == destination
                })
                .and_then(|(_, route)| route.timeout_ms)
                .unwrap_or(default_timeout)
        };

        // Forward the datagram to the destination
        let dest_addr = destination.parse::<SocketAddr>().map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidInput, format!("Invalid destination address: {}", e))
        })?;
        
        // Set up timeout for the send operation
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

        // Create a new socket for receiving responses
        // We need a separate socket because we can't listen on the same socket we're sending from
        // without more complex socket sharing logic
        let bind_addr = if socket.local_addr()?.ip().is_unspecified() {
            format!("0.0.0.0:0")
        } else {
            format!("{}:0", socket.local_addr()?.ip())
        };
        
        let response_socket = UdpSocket::bind(bind_addr).await?;
        
        // Connected UDP sockets can only receive from the specific address they're connected to
        response_socket.connect(dest_addr).await?;
        
        // Set up a task to wait for a response
        let mut response_buf = vec![0u8; MAX_DATAGRAM_SIZE];
        match timeout(
            Duration::from_millis(timeout_ms),
            response_socket.recv(&mut response_buf)
        ).await {
            Ok(Ok(size)) => {
                // Forward the response back to the client through the channel
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

        // Record metrics
        let duration = start_time.elapsed();
        histogram!("udp_proxy.datagram.duration", duration.as_secs_f64());

        Ok(())
    }
}

// Helper function to extract host:port from a URL
fn extract_host_port(url: &str) -> String {
    // Parse out protocol
    let url_without_protocol = url.split("://").nth(1).unwrap_or(url);
    
    // Extract host:port part
    url_without_protocol.split('/').next().unwrap_or(url_without_protocol).to_string()
}
