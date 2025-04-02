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

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct RouteConfig {
    pub upstream: String,
    pub timeout_ms: Option<u64>,
    pub retry_count: Option<u32>,
    #[serde(default)] 
    pub priority: Option<i32>,
    pub preserve_host_header: Option<bool>,
    
    // New TCP-specific configuration
    #[serde(default = "default_is_tcp")]
    pub is_tcp: bool,
    #[serde(default = "default_tcp_port")]
    pub tcp_listen_port: Option<u16>,
    #[serde(default = "default_db_type")]
    pub db_type: Option<String>,
}

fn default_is_tcp() -> bool {
    false
}

fn default_tcp_port() -> Option<u16> {
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