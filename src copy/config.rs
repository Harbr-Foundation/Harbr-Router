// src/config.rs
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProxyConfig {
    pub listen_addr: String,
    pub routes: HashMap<String, RouteConfig>,
    pub global_timeout_ms: u64,
    pub max_connections: usize,
    
    // New TCP proxy specific configuration
    #[serde(default)]
    pub tcp_proxy: TcpProxyConfig,
}

impl ProxyConfig {
    pub fn new(listen_addr: &str, global_timeout_ms: u64, max_connections: usize) -> Self {
        Self {
            listen_addr: listen_addr.to_string(),
            routes: HashMap::new(),
            global_timeout_ms,
            max_connections,
            tcp_proxy: TcpProxyConfig::default(),
        }
    }

    pub fn with_route(mut self, name: &str, route: RouteConfig) -> Self {
        self.routes.insert(name.to_string(), route);
        self
    }

    pub fn with_tcp_proxy(mut self, tcp_proxy: TcpProxyConfig) -> Self {
        self.tcp_proxy = tcp_proxy;
        self
    }

    pub fn enable_tcp_proxy(mut self, enabled: bool) -> Self {
        self.tcp_proxy.enabled = enabled;
        self
    }

    pub fn tcp_listen_addr(mut self, addr: &str) -> Self {
        self.tcp_proxy.listen_addr = addr.to_string();
        self
    }

    pub fn enable_udp_proxy(mut self, enabled: bool) -> Self {
        self.tcp_proxy.udp_enabled = enabled;
        self
    }

    pub fn udp_listen_addr(mut self, addr: &str) -> Self {
        self.tcp_proxy.udp_listen_addr = addr.to_string();
        self
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct TcpProxyConfig {
    #[serde(default = "default_tcp_enabled")]
    pub enabled: bool,
    #[serde(default = "default_tcp_listen_addr")]
    pub listen_addr: String,
    #[serde(default = "default_tcp_connection_pooling")]
    pub connection_pooling: bool,
    #[serde(default = "default_tcp_max_idle_time_secs")]
    pub max_idle_time_secs: u64,
    #[serde(default = "default_udp_enabled")]
    pub udp_enabled: bool,
    #[serde(default = "default_udp_listen_addr")]
    pub udp_listen_addr: String,
}

impl TcpProxyConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn with_listen_addr(mut self, addr: &str) -> Self {
        self.listen_addr = addr.to_string();
        self
    }

    pub fn with_connection_pooling(mut self, enabled: bool) -> Self {
        self.connection_pooling = enabled;
        self
    }

    pub fn with_max_idle_time(mut self, secs: u64) -> Self {
        self.max_idle_time_secs = secs;
        self
    }

    pub fn with_udp_enabled(mut self, enabled: bool) -> Self {
        self.udp_enabled = enabled;
        self
    }

    pub fn with_udp_listen_addr(mut self, addr: &str) -> Self {
        self.udp_listen_addr = addr.to_string();
        self
    }
}

fn default_tcp_enabled() -> bool {
    false
}

fn default_tcp_listen_addr() -> String {
    "0.0.0.0:9090".to_string()
}

fn default_tcp_connection_pooling() -> bool {
    true
}

fn default_tcp_max_idle_time_secs() -> u64 {
    60
}

fn default_udp_enabled() -> bool {
    false
}

fn default_udp_listen_addr() -> String {
    "0.0.0.0:9090".to_string() // Same port as TCP by default
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct RouteConfig {
    pub upstream: String,
    pub timeout_ms: Option<u64>,
    pub retry_count: Option<u32>,
    #[serde(default)] 
    pub priority: Option<i32>,
    pub preserve_host_header: Option<bool>,
    
    // TCP-specific configuration
    #[serde(default = "default_is_tcp")]
    pub is_tcp: bool,
    #[serde(default = "default_tcp_port")]
    pub tcp_listen_port: Option<u16>,
    
    // UDP-specific configuration
    #[serde(default = "default_is_udp")]
    pub is_udp: Option<bool>,
    #[serde(default = "default_udp_port")]
    pub udp_listen_port: Option<u16>,
    
    // Database-specific configuration
    #[serde(default = "default_db_type")]
    pub db_type: Option<String>,
}

impl RouteConfig {
    pub fn new(upstream: &str) -> Self {
        Self {
            upstream: upstream.to_string(),
            timeout_ms: None,
            retry_count: None,
            priority: None,
            preserve_host_header: None,
            is_tcp: false,
            tcp_listen_port: None,
            is_udp: None,
            udp_listen_port: None,
            db_type: None,
        }
    }

    pub fn with_timeout(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = Some(timeout_ms);
        self
    }

    pub fn with_retry_count(mut self, count: u32) -> Self {
        self.retry_count = Some(count);
        self
    }

    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = Some(priority);
        self
    }

    pub fn preserve_host_header(mut self, preserve: bool) -> Self {
        self.preserve_host_header = Some(preserve);
        self
    }

    pub fn as_tcp(mut self, is_tcp: bool) -> Self {
        self.is_tcp = is_tcp;
        self
    }

    pub fn with_tcp_listen_port(mut self, port: u16) -> Self {
        self.tcp_listen_port = Some(port);
        self
    }

    pub fn as_udp(mut self, is_udp: bool) -> Self {
        self.is_udp = Some(is_udp);
        self
    }

    pub fn with_udp_listen_port(mut self, port: u16) -> Self {
        self.udp_listen_port = Some(port);
        self
    }

    pub fn with_db_type(mut self, db_type: &str) -> Self {
        self.db_type = Some(db_type.to_string());
        self
    }
}

fn default_is_tcp() -> bool {
    false
}

fn default_tcp_port() -> Option<u16> {
    None
}

fn default_is_udp() -> Option<bool> {
    Some(false)
}

fn default_udp_port() -> Option<u16> {
    None
}

fn default_db_type() -> Option<String> {
    None
}

pub fn load_config(path: &str) -> Result<ProxyConfig> {
    let content = fs::read_to_string(path)?;
    let config: ProxyConfig = serde_yaml::from_str(&content)?;
    Ok(config)
}

// Helper function to detect if a route is likely a database
pub fn is_likely_database(route: &RouteConfig) -> bool {
    // Check if explicitly marked as TCP
    if route.is_tcp {
        return true;
    }
    
    // Check if db_type is specified
    if route.db_type.is_some() {
        return true;
    }
    
    // Basic heuristics for common database port detection
    if let Some(port) = extract_port(&route.upstream) {
        match port {
            3306 | 33060 => true, // MySQL
            5432 => true,         // PostgreSQL
            27017 | 27018 | 27019 => true, // MongoDB
            6379 => true,         // Redis
            1521 => true,         // Oracle
            1433 => true,         // SQL Server
            9042 => true,         // Cassandra
            5984 => true,         // CouchDB
            8086 => true,         // InfluxDB
            9200 | 9300 => true,  // Elasticsearch
            _ => false,
        }
    } else {
        // Check for database prefixes in the upstream URL
        let upstream = route.upstream.to_lowercase();
        upstream.starts_with("mysql://") 
            || upstream.starts_with("postgresql://") 
            || upstream.starts_with("mongodb://")
            || upstream.starts_with("redis://")
            || upstream.starts_with("oracle://")
            || upstream.starts_with("sqlserver://")
            || upstream.starts_with("cassandra://")
            || upstream.starts_with("couchdb://")
            || upstream.starts_with("influxdb://")
            || upstream.starts_with("elasticsearch://")
    }
}

// Helper function to extract port from a URL
fn extract_port(url: &str) -> Option<u16> {
    // Parse out protocol
    let url_without_protocol = url.split("://").nth(1).unwrap_or(url);
    
    // Extract host:port part
    let host_port = url_without_protocol.split('/').next()?;
    
    // Extract port
    let port_str = host_port.split(':').nth(1)?;
    port_str.parse::<u16>().ok()
}