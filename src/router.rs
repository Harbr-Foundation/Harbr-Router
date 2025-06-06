// src/router.rs - Request routing logic
use crate::config::{DomainConfig, ProxyInstanceConfig};
use crate::plugin::PluginCapabilities;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, warn};

/// Request router that determines which plugin should handle a request
pub struct RequestRouter {
    domain_mappings: Arc<RwLock<HashMap<String, DomainConfig>>>,
    proxy_instances: Arc<RwLock<HashMap<String, ProxyInstanceConfig>>>,
    port_mappings: Arc<RwLock<HashMap<u16, String>>>, // port -> proxy instance name
    protocol_mappings: Arc<RwLock<HashMap<String, Vec<String>>>>, // protocol -> plugin names
    routing_cache: Arc<RwLock<HashMap<String, Vec<String>>>>, // cache for performance
}

#[derive(Debug, Clone)]
pub struct RoutingDecision {
    pub plugin_names: Vec<String>,
    pub proxy_instance: String,
    pub domain_config: Option<DomainConfig>,
    pub routing_metadata: HashMap<String, String>,
    pub priority: i32,
    pub load_balancing_config: Option<String>,
}

impl RequestRouter {
    /// Create a new request router
    pub fn new(
        domains: HashMap<String, DomainConfig>,
        proxies: Vec<ProxyInstanceConfig>,
    ) -> Self {
        let mut port_mappings = HashMap::new();
        let mut proxy_instances = HashMap::new();
        
        // Build proxy instance mappings
        for proxy in proxies {
            // Map ports to proxy instances
            for port in &proxy.ports {
                port_mappings.insert(*port, proxy.name.clone());
            }
            
            proxy_instances.insert(proxy.name.clone(), proxy);
        }
        
        Self {
            domain_mappings: Arc::new(RwLock::new(domains)),
            proxy_instances: Arc::new(RwLock::new(proxy_instances)),
            port_mappings: Arc::new(RwLock::new(port_mappings)),
            protocol_mappings: Arc::new(RwLock::new(HashMap::new())),
            routing_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    /// Find plugins that can handle a request for a specific domain and path
    pub async fn find_plugins_for_request(
        &self,
        domain: &str,
        path: &str,
        protocol: &str,
    ) -> Vec<String> {
        // Try cache first
        let cache_key = format!("{}:{}:{}", domain, path, protocol);
        {
            let cache = self.routing_cache.read().await;
            if let Some(cached_plugins) = cache.get(&cache_key) {
                debug!("Cache hit for routing decision: {}", cache_key);
                return cached_plugins.clone();
            }
        }
        
        let mut matching_plugins = Vec::new();
        
        // Find domain configuration
        let domain_config = self.find_domain_config(domain).await;
        
        if let Some(domain_cfg) = &domain_config {
            // Get the proxy instance for this domain
            let proxy_instances = self.proxy_instances.read().await;
            if let Some(proxy) = proxy_instances.get(&domain_cfg.proxy_instance) {
                // Check if proxy is enabled and handles this protocol
                if proxy.enabled {
                    matching_plugins.push(proxy.plugin_type.clone());
                }
            }
        } else {
            // No specific domain config, try to find plugins by protocol
            let protocol_mappings = self.protocol_mappings.read().await;
            if let Some(plugins) = protocol_mappings.get(protocol) {
                matching_plugins.extend(plugins.clone());
            }
        }
        
        // Sort by priority
        matching_plugins.sort();
        matching_plugins.dedup();
        
        // Cache the result
        {
            let mut cache = self.routing_cache.write().await;
            cache.insert(cache_key, matching_plugins.clone());
            
            // Limit cache size
            if cache.len() > 10000 {
                cache.clear();
            }
        }
        
        debug!("Found {} plugins for {}:{} ({})", matching_plugins.len(), domain, path, protocol);
        matching_plugins
    }
    
    /// Find plugins that can handle a specific protocol
    pub async fn find_plugins_for_protocol(&self, protocol: &str) -> Vec<String> {
        let protocol_mappings = self.protocol_mappings.read().await;
        protocol_mappings.get(protocol).cloned().unwrap_or_default()
    }
    
    /// Find plugins that can handle a specific port
    pub async fn find_plugins_for_port(&self, port: u16) -> Vec<String> {
        let port_mappings = self.port_mappings.read().await;
        let proxy_instances = self.proxy_instances.read().await;
        
        if let Some(proxy_name) = port_mappings.get(&port) {
            if let Some(proxy) = proxy_instances.get(proxy_name) {
                if proxy.enabled {
                    return vec![proxy.plugin_type.clone()];
                }
            }
        }
        
        Vec::new()
    }
    
    /// Get routing decision with full context
    pub async fn get_routing_decision(
        &self,
        domain: &str,
        path: &str,
        protocol: &str,
        port: Option<u16>,
    ) -> Option<RoutingDecision> {
        let plugins = self.find_plugins_for_request(domain, path, protocol).await;
        
        if plugins.is_empty() {
            return None;
        }
        
        // Find domain config
        let domain_config = self.find_domain_config(domain).await;
        let proxy_instance_name = domain_config.as_ref()
            .map(|dc| dc.proxy_instance.clone())
            .unwrap_or_else(|| "default".to_string());
        
        // Get proxy instance details
        let proxy_instances = self.proxy_instances.read().await;
        let proxy_instance = proxy_instances.get(&proxy_instance_name);
        
        let mut routing_metadata = HashMap::new();
        routing_metadata.insert("domain".to_string(), domain.to_string());
        routing_metadata.insert("path".to_string(), path.to_string());
        routing_metadata.insert("protocol".to_string(), protocol.to_string());
        
        if let Some(port) = port {
            routing_metadata.insert("port".to_string(), port.to_string());
        }
        
        Some(RoutingDecision {
            plugin_names: plugins,
            proxy_instance: proxy_instance_name,
            domain_config: domain_config.clone(),
            routing_metadata,
            priority: proxy_instance.map(|p| p.priority).unwrap_or(0),
            load_balancing_config: proxy_instance.and_then(|p| p.load_balancing.clone()),
        })
    }
    
    /// Find domain configuration (including wildcard matching)
    async fn find_domain_config(&self, domain: &str) -> Option<DomainConfig> {
        let domain_mappings = self.domain_mappings.read().await;
        
        // Try exact match first
        if let Some(config) = domain_mappings.get(domain) {
            return Some(config.clone());
        }
        
        // Try wildcard matching
        for (pattern, config) in domain_mappings.iter() {
            if self.domain_matches_pattern(pattern, domain) {
                return Some(config.clone());
            }
        }
        
        None
    }
    
    /// Check if domain matches pattern (supports wildcards)
    fn domain_matches_pattern(&self, pattern: &str, domain: &str) -> bool {
        if pattern == "*" {
            return true; // Match all
        }
        
        if pattern.starts_with("*.") {
            let suffix = &pattern[2..];
            return domain.ends_with(suffix);
        }
        
        if pattern.ends_with(".*") {
            let prefix = &pattern[..pattern.len() - 2];
            return domain.starts_with(prefix);
        }
        
        // Support regex patterns
        if pattern.starts_with("~") {
            let regex_pattern = &pattern[1..];
            if let Ok(regex) = regex::Regex::new(regex_pattern) {
                return regex.is_match(domain);
            }
        }
        
        pattern == domain
    }
    
    /// Register plugin capabilities for routing
    pub async fn register_plugin_capabilities(
        &self,
        plugin_name: String,
        capabilities: PluginCapabilities,
    ) {
        let mut protocol_mappings = self.protocol_mappings.write().await;
        
        // Register standard protocol capabilities
        if capabilities.can_handle_tcp {
            protocol_mappings
                .entry("TCP".to_string())
                .or_insert_with(Vec::new)
                .push(plugin_name.clone());
        }
        
        if capabilities.can_handle_udp {
            protocol_mappings
                .entry("UDP".to_string())
                .or_insert_with(Vec::new)
                .push(plugin_name.clone());
        }
        
        // Register custom protocols
        for protocol in capabilities.custom_protocols {
            protocol_mappings
                .entry(protocol.to_uppercase())
                .or_insert_with(Vec::new)
                .push(plugin_name.clone());
        }
        
        debug!("Registered plugin '{}' capabilities", plugin_name);
    }
    
    /// Unregister plugin capabilities
    pub async fn unregister_plugin_capabilities(&self, plugin_name: &str) {
        let mut protocol_mappings = self.protocol_mappings.write().await;
        
        for (_, plugins) in protocol_mappings.iter_mut() {
            plugins.retain(|name| name != plugin_name);
        }
        
        // Clear cache to force re-evaluation
        {
            let mut cache = self.routing_cache.write().await;
            cache.clear();
        }
        
        debug!("Unregistered plugin '{}' capabilities", plugin_name);
    }
    
    /// Update configuration
    pub async fn update_config(
        &self,
        domains: HashMap<String, DomainConfig>,
        proxies: Vec<ProxyInstanceConfig>,
    ) {
        // Update domain mappings
        {
            let mut domain_mappings = self.domain_mappings.write().await;
            *domain_mappings = domains;
        }
        
        // Update proxy instances and port mappings
        {
            let mut proxy_instances = self.proxy_instances.write().await;
            let mut port_mappings = self.port_mappings.write().await;
            
            // Clear existing mappings
            proxy_instances.clear();
            port_mappings.clear();
            
            // Rebuild mappings
            for proxy in proxies {
                // Map ports to proxy instances
                for port in &proxy.ports {
                    port_mappings.insert(*port, proxy.name.clone());
                }
                
                proxy_instances.insert(proxy.name.clone(), proxy);
            }
        }
        
        // Clear cache to force re-evaluation
        {
            let mut cache = self.routing_cache.write().await;
            cache.clear();
        }
        
        debug!("Updated router configuration");
    }
    
    /// Add domain mapping
    pub async fn add_domain(&self, domain: String, config: DomainConfig) {
        let mut domain_mappings = self.domain_mappings.write().await;
        domain_mappings.insert(domain.clone(), config);
        
        // Clear relevant cache entries
        {
            let mut cache = self.routing_cache.write().await;
            cache.retain(|key, _| !key.starts_with(&format!("{}:", domain)));
        }
        
        debug!("Added domain mapping: {}", domain);
    }
    
    /// Remove domain mapping
    pub async fn remove_domain(&self, domain: &str) {
        let mut domain_mappings = self.domain_mappings.write().await;
        domain_mappings.remove(domain);
        
        // Clear relevant cache entries
        {
            let mut cache = self.routing_cache.write().await;
            cache.retain(|key, _| !key.starts_with(&format!("{}:", domain)));
        }
        
        debug!("Removed domain mapping: {}", domain);
    }
    
    /// Add proxy instance
    pub async fn add_proxy_instance(&self, proxy: ProxyInstanceConfig) {
        let proxy_name = proxy.name.clone();
        
        // Update port mappings
        {
            let mut port_mappings = self.port_mappings.write().await;
            for port in &proxy.ports {
                port_mappings.insert(*port, proxy_name.clone());
            }
        }
        
        // Update proxy instances
        {
            let mut proxy_instances = self.proxy_instances.write().await;
            proxy_instances.insert(proxy_name.clone(), proxy);
        }
        
        // Clear cache
        {
            let mut cache = self.routing_cache.write().await;
            cache.clear();
        }
        
        debug!("Added proxy instance: {}", proxy_name);
    }
    
    /// Remove proxy instance
    pub async fn remove_proxy_instance(&self, proxy_name: &str) {
        // Remove from proxy instances
        let removed_proxy = {
            let mut proxy_instances = self.proxy_instances.write().await;
            proxy_instances.remove(proxy_name)
        };
        
        // Remove port mappings
        if let Some(proxy) = removed_proxy {
            let mut port_mappings = self.port_mappings.write().await;
            for port in &proxy.ports {
                port_mappings.remove(port);
            }
        }
        
        // Clear cache
        {
            let mut cache = self.routing_cache.write().await;
            cache.clear();
        }
        
        debug!("Removed proxy instance: {}", proxy_name);
    }
    
    /// Get routing statistics
    pub async fn get_statistics(&self) -> RoutingStatistics {
        let domain_mappings = self.domain_mappings.read().await;
        let proxy_instances = self.proxy_instances.read().await;
        let port_mappings = self.port_mappings.read().await;
        let protocol_mappings = self.protocol_mappings.read().await;
        let cache = self.routing_cache.read().await;
        
        RoutingStatistics {
            total_domains: domain_mappings.len(),
            total_proxy_instances: proxy_instances.len(),
            total_port_mappings: port_mappings.len(),
            total_protocol_mappings: protocol_mappings.len(),
            cache_size: cache.len(),
            enabled_proxies: proxy_instances.values().filter(|p| p.enabled).count(),
            disabled_proxies: proxy_instances.values().filter(|p| !p.enabled).count(),
        }
    }
    
    /// Validate routing configuration
    pub async fn validate_configuration(&self) -> Vec<RoutingValidationError> {
        let mut errors = Vec::new();
        
        let domain_mappings = self.domain_mappings.read().await;
        let proxy_instances = self.proxy_instances.read().await;
        
        // Check that all domain configs reference valid proxy instances
        for (domain, config) in domain_mappings.iter() {
            if !proxy_instances.contains_key(&config.proxy_instance) {
                errors.push(RoutingValidationError {
                    error_type: "missing_proxy_instance".to_string(),
                    domain: Some(domain.clone()),
                    proxy_instance: Some(config.proxy_instance.clone()),
                    message: format!("Domain '{}' references non-existent proxy instance '{}'", 
                                   domain, config.proxy_instance),
                });
            }
        }
        
        // Check for port conflicts
        let mut port_usage = HashMap::new();
        for (name, proxy) in proxy_instances.iter() {
            for port in &proxy.ports {
                if let Some(existing) = port_usage.insert(*port, name.clone()) {
                    errors.push(RoutingValidationError {
                        error_type: "port_conflict".to_string(),
                        domain: None,
                        proxy_instance: Some(name.clone()),
                        message: format!("Port {} is used by both '{}' and '{}'", 
                                       port, existing, name),
                    });
                }
            }
        }
        
        errors
    }
    
    /// Clear routing cache
    pub async fn clear_cache(&self) {
        let mut cache = self.routing_cache.write().await;
        cache.clear();
        debug!("Cleared routing cache");
    }
    
    /// Get cache statistics
    pub async fn get_cache_statistics(&self) -> CacheStatistics {
        let cache = self.routing_cache.read().await;
        
        CacheStatistics {
            total_entries: cache.len(),
            memory_usage_estimate: cache.len() * 100, // Rough estimate
        }
    }
}

/// Routing statistics
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RoutingStatistics {
    pub total_domains: usize,
    pub total_proxy_instances: usize,
    pub total_port_mappings: usize,
    pub total_protocol_mappings: usize,
    pub cache_size: usize,
    pub enabled_proxies: usize,
    pub disabled_proxies: usize,
}

/// Routing validation error
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RoutingValidationError {
    pub error_type: String,
    pub domain: Option<String>,
    pub proxy_instance: Option<String>,
    pub message: String,
}

/// Cache statistics
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CacheStatistics {
    pub total_entries: usize,
    pub memory_usage_estimate: usize,
}

/// Advanced routing features
impl RequestRouter {
    /// Route with load balancing consideration
    pub async fn route_with_load_balancing(
        &self,
        domain: &str,
        path: &str,
        protocol: &str,
        client_info: &ClientInfo,
    ) -> Option<RoutingDecision> {
        let mut decision = self.get_routing_decision(domain, path, protocol, None).await?;
        
        // Apply load balancing logic if configured
        if let Some(lb_config) = &decision.load_balancing_config {
            // Implementation would depend on load balancing strategy
            decision.routing_metadata.insert(
                "load_balancing_strategy".to_string(),
                lb_config.clone(),
            );
            decision.routing_metadata.insert(
                "client_id".to_string(),
                client_info.id.clone(),
            );
        }
        
        Some(decision)
    }
    
    /// Route with session affinity
    pub async fn route_with_session_affinity(
        &self,
        domain: &str,
        path: &str,
        protocol: &str,
        session_id: &str,
    ) -> Option<RoutingDecision> {
        let mut decision = self.get_routing_decision(domain, path, protocol, None).await?;
        
        // Add session affinity metadata
        decision.routing_metadata.insert(
            "session_id".to_string(),
            session_id.to_string(),
        );
        decision.routing_metadata.insert(
            "session_affinity".to_string(),
            "enabled".to_string(),
        );
        
        Some(decision)
    }
    
    /// Route with A/B testing support
    pub async fn route_with_ab_testing(
        &self,
        domain: &str,
        path: &str,
        protocol: &str,
        user_id: &str,
        ab_test_config: &ABTestConfig,
    ) -> Option<RoutingDecision> {
        let mut decision = self.get_routing_decision(domain, path, protocol, None).await?;
        
        // Determine which variant the user should see
        let variant = self.determine_ab_variant(user_id, ab_test_config);
        
        decision.routing_metadata.insert(
            "ab_test_variant".to_string(),
            variant,
        );
        
        Some(decision)
    }
    
    /// Determine A/B test variant
    fn determine_ab_variant(&self, user_id: &str, config: &ABTestConfig) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        
        let mut hasher = DefaultHasher::new();
        user_id.hash(&mut hasher);
        let hash = hasher.finish();
        
        let variant_index = (hash % 100) as u8;
        
        let mut cumulative_percentage = 0;
        for (variant, percentage) in &config.variants {
            cumulative_percentage += percentage;
            if variant_index < cumulative_percentage {
                return variant.clone();
            }
        }
        
        config.variants.first().map(|(v, _)| v.clone()).unwrap_or_else(|| "default".to_string())
    }
}

/// Client information for routing decisions
#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub id: String,
    pub ip_address: std::net::IpAddr,
    pub user_agent: Option<String>,
    pub headers: HashMap<String, String>,
}

/// A/B testing configuration
#[derive(Debug, Clone)]
pub struct ABTestConfig {
    pub test_name: String,
    pub variants: Vec<(String, u8)>, // (variant_name, percentage)
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DomainConfig, ProxyInstanceConfig};
    
    #[tokio::test]
    async fn test_domain_matching() {
        let router = RequestRouter::new(HashMap::new(), Vec::new());
        
        // Test exact match
        assert!(router.domain_matches_pattern("example.com", "example.com"));
        assert!(!router.domain_matches_pattern("example.com", "test.com"));
        
        // Test wildcard match
        assert!(router.domain_matches_pattern("*.example.com", "api.example.com"));
        assert!(router.domain_matches_pattern("*.example.com", "www.example.com"));
        assert!(!router.domain_matches_pattern("*.example.com", "example.com"));
        
        // Test catch-all
        assert!(router.domain_matches_pattern("*", "anything.com"));
    }
    
    #[tokio::test]
    async fn test_routing_decision() {
        let mut domains = HashMap::new();
        domains.insert("example.com".to_string(), DomainConfig {
            proxy_instance: "http_proxy".to_string(),
            backend_service: Some("http://backend:8080".to_string()),
            ssl_config: None,
            cors_config: None,
            cache_config: None,
            custom_headers: HashMap::new(),
            rewrite_rules: vec![],
            access_control: None,
        });
        
        let proxies = vec![ProxyInstanceConfig {
            name: "http_proxy".to_string(),
            plugin_type: "http_proxy".to_string(),
            enabled: true,
            priority: 0,
            ports: vec![8080],
            bind_addresses: vec!["0.0.0.0".to_string()],
            domains: vec!["example.com".to_string()],
            plugin_config: serde_json::json!({}),
            middleware: vec![],
            load_balancing: None,
            health_check: None,
            circuit_breaker: None,
            rate_limiting: None,
            ssl_config: None,
        }];
        
        let router = RequestRouter::new(domains, proxies);
        
        let plugins = router.find_plugins_for_request("example.com", "/api", "HTTP").await;
        assert_eq!(plugins, vec!["http_proxy"]);
        
        let decision = router.get_routing_decision("example.com", "/api", "HTTP", None).await;
        assert!(decision.is_some());
        
        let decision = decision.unwrap();
        assert_eq!(decision.proxy_instance, "http_proxy");
        assert_eq!(decision.plugin_names, vec!["http_proxy"]);
    }
}