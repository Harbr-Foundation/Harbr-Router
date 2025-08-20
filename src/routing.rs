//! Advanced routing and load balancing algorithms.
//!
//! This module provides sophisticated routing strategies including weighted
//! round-robin, consistent hashing, and health-based routing.

use crate::{config::UpstreamConfig, Error, Result};
use ahash::{AHasher, AHashMap};
use parking_lot::RwLock;
use std::hash::{Hash, Hasher};
use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

/// Load balancing strategies supported by the router.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadBalancingStrategy {
    /// Round-robin selection
    RoundRobin,
    /// Weighted round-robin based on upstream weights
    WeightedRoundRobin,
    /// Least connections routing
    LeastConnections,
    /// Consistent hashing for sticky sessions
    ConsistentHashing,
    /// Health-aware routing (combines health scores with other strategies)
    HealthAware,
}

/// Health state of an upstream server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HealthState {
    Healthy,
    Unhealthy,
    Unknown,
}

/// Statistics for an upstream server.
#[derive(Debug)]
pub struct UpstreamStats {
    /// Total number of requests sent to this upstream
    pub requests_sent: AtomicU64,
    /// Total number of successful responses
    pub responses_received: AtomicU64,
    /// Total number of failed requests
    pub failures: AtomicU64,
    /// Active connections to this upstream
    pub active_connections: AtomicUsize,
    /// Average response time in nanoseconds (exponentially weighted)
    pub avg_response_time_ns: AtomicU64,
    /// Last response time update
    pub last_update: RwLock<Instant>,
}

impl Default for UpstreamStats {
    fn default() -> Self {
        Self {
            requests_sent: AtomicU64::new(0),
            responses_received: AtomicU64::new(0),
            failures: AtomicU64::new(0),
            active_connections: AtomicUsize::new(0),
            avg_response_time_ns: AtomicU64::new(0),
            last_update: RwLock::new(Instant::now()),
        }
    }
}

/// Runtime information about an upstream server.
#[derive(Debug)]
pub struct UpstreamInfo {
    /// Static configuration
    pub config: UpstreamConfig,
    /// Current health state
    pub health_state: RwLock<HealthState>,
    /// Performance statistics
    pub stats: UpstreamStats,
    /// Health check failure count
    pub health_failures: AtomicUsize,
    /// Last health check time
    pub last_health_check: RwLock<Instant>,
}

/// Router for selecting upstream servers based on various strategies.
#[derive(Debug)]
pub struct Router {
    /// Available upstream servers
    upstreams: RwLock<Vec<Arc<UpstreamInfo>>>,
    /// Load balancing strategy
    strategy: LoadBalancingStrategy,
    /// Round-robin counter
    rr_counter: AtomicUsize,
    /// Consistent hashing ring (only used with ConsistentHashing strategy)
    hash_ring: RwLock<ConsistentHashRing>,
}

/// Consistent hashing ring for sticky sessions.
#[derive(Debug)]
struct ConsistentHashRing {
    /// Ring with virtual nodes pointing to upstream indices
    ring: AHashMap<u64, usize>,
    /// Sorted hash values for efficient lookup
    sorted_hashes: Vec<u64>,
    /// Number of virtual nodes per upstream
    virtual_nodes_per_upstream: usize,
}

impl UpstreamStats {
    /// Record a successful request with response time.
    pub fn record_success(&self, response_time: Duration) {
        self.requests_sent.fetch_add(1, Ordering::Relaxed);
        self.responses_received.fetch_add(1, Ordering::Relaxed);
        
        let response_time_ns = response_time.as_nanos() as u64;
        let current_avg = self.avg_response_time_ns.load(Ordering::Relaxed);
        
        // Exponentially weighted moving average (alpha = 0.125)
        let new_avg = if current_avg == 0 {
            response_time_ns
        } else {
            (current_avg * 7 + response_time_ns) / 8
        };
        
        self.avg_response_time_ns.store(new_avg, Ordering::Relaxed);
        *self.last_update.write() = Instant::now();
    }

    /// Record a failed request.
    pub fn record_failure(&self) {
        self.requests_sent.fetch_add(1, Ordering::Relaxed);
        self.failures.fetch_add(1, Ordering::Relaxed);
        *self.last_update.write() = Instant::now();
    }

    /// Increment active connection count.
    pub fn increment_connections(&self) -> usize {
        self.active_connections.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Decrement active connection count.
    pub fn decrement_connections(&self) -> usize {
        self.active_connections.fetch_sub(1, Ordering::Relaxed).saturating_sub(1)
    }

    /// Calculate success rate (0.0 to 1.0).
    pub fn success_rate(&self) -> f64 {
        let requests = self.requests_sent.load(Ordering::Relaxed);
        if requests == 0 {
            1.0 // No requests yet, assume healthy
        } else {
            let successes = self.responses_received.load(Ordering::Relaxed);
            successes as f64 / requests as f64
        }
    }

    /// Get average response time in milliseconds.
    pub fn avg_response_time_ms(&self) -> f64 {
        let ns = self.avg_response_time_ns.load(Ordering::Relaxed);
        ns as f64 / 1_000_000.0
    }
}

impl UpstreamInfo {
    /// Create a new upstream info from configuration.
    pub fn new(config: UpstreamConfig) -> Self {
        let now = Instant::now();
        Self {
            config,
            health_state: RwLock::new(HealthState::Unknown),
            stats: UpstreamStats::default(),
            health_failures: AtomicUsize::new(0),
            last_health_check: RwLock::new(now),
        }
    }

    /// Calculate health score (0.0 to 1.0, higher is better).
    pub fn health_score(&self) -> f64 {
        let health_state = *self.health_state.read();
        let base_score = match health_state {
            HealthState::Healthy => 1.0,
            HealthState::Unknown => 0.5,
            HealthState::Unhealthy => 0.1,
        };

        let success_rate = self.stats.success_rate();
        let response_time_ms = self.stats.avg_response_time_ms();
        
        // Penalize slow response times
        let latency_factor = if response_time_ms > 1000.0 {
            0.5 // Severe penalty for > 1s response time
        } else if response_time_ms > 500.0 {
            0.7 // Moderate penalty for > 500ms
        } else if response_time_ms > 100.0 {
            0.9 // Light penalty for > 100ms
        } else {
            1.0 // No penalty for fast responses
        };

        base_score * success_rate * latency_factor
    }

    /// Update health state.
    pub fn set_health_state(&self, state: HealthState) {
        *self.health_state.write() = state;
        *self.last_health_check.write() = Instant::now();
        
        match state {
            HealthState::Healthy => {
                self.health_failures.store(0, Ordering::Relaxed);
            }
            HealthState::Unhealthy => {
                self.health_failures.fetch_add(1, Ordering::Relaxed);
            }
            HealthState::Unknown => {
                // Don't modify failure count
            }
        }
    }

    /// Check if the upstream is available for routing.
    pub fn is_available(&self) -> bool {
        let health_state = *self.health_state.read();
        matches!(health_state, HealthState::Healthy | HealthState::Unknown)
    }
}

impl ConsistentHashRing {
    /// Create a new consistent hash ring.
    fn new(virtual_nodes_per_upstream: usize) -> Self {
        Self {
            ring: AHashMap::new(),
            sorted_hashes: Vec::new(),
            virtual_nodes_per_upstream,
        }
    }

    /// Update the hash ring with new upstreams.
    fn update(&mut self, upstreams: &[Arc<UpstreamInfo>]) {
        self.ring.clear();
        self.sorted_hashes.clear();

        for (upstream_idx, upstream) in upstreams.iter().enumerate() {
            if !upstream.is_available() {
                continue;
            }

            for virtual_node in 0..self.virtual_nodes_per_upstream {
                let key = format!("{}:{}", upstream.config.name, virtual_node);
                let hash = self.hash_key(&key);
                self.ring.insert(hash, upstream_idx);
                self.sorted_hashes.push(hash);
            }
        }

        self.sorted_hashes.sort_unstable();
    }

    /// Find the upstream for a given key.
    fn get_upstream(&self, key: &str) -> Option<usize> {
        if self.sorted_hashes.is_empty() {
            return None;
        }

        let hash = self.hash_key(key);
        
        // Find the first hash >= target hash
        let idx = match self.sorted_hashes.binary_search(&hash) {
            Ok(idx) => idx,
            Err(idx) => {
                if idx == self.sorted_hashes.len() {
                    0 // Wrap around to the first node
                } else {
                    idx
                }
            }
        };

        let hash_value = self.sorted_hashes[idx];
        self.ring.get(&hash_value).copied()
    }

    /// Hash a key using AHash.
    fn hash_key(&self, key: &str) -> u64 {
        let mut hasher = AHasher::default();
        key.hash(&mut hasher);
        hasher.finish()
    }
}

impl Router {
    /// Create a new router with the specified strategy.
    pub fn new(strategy: LoadBalancingStrategy) -> Self {
        Self {
            upstreams: RwLock::new(Vec::new()),
            strategy,
            rr_counter: AtomicUsize::new(0),
            hash_ring: RwLock::new(ConsistentHashRing::new(150)), // 150 virtual nodes per upstream
        }
    }

    /// Update the list of upstream servers.
    pub fn update_upstreams(&self, configs: Vec<UpstreamConfig>) -> Result<()> {
        let upstreams: Vec<Arc<UpstreamInfo>> = configs
            .into_iter()
            .map(|config| Arc::new(UpstreamInfo::new(config)))
            .collect();

        *self.upstreams.write() = upstreams;

        // Update consistent hash ring if using consistent hashing
        if matches!(self.strategy, LoadBalancingStrategy::ConsistentHashing) {
            let upstreams = self.upstreams.read();
            self.hash_ring.write().update(&upstreams);
        }

        Ok(())
    }

    /// Select an upstream server for a request.
    pub fn select_upstream(&self, session_key: Option<&str>) -> Option<Arc<UpstreamInfo>> {
        let upstreams = self.upstreams.read();
        if upstreams.is_empty() {
            return None;
        }

        match self.strategy {
            LoadBalancingStrategy::RoundRobin => self.select_round_robin(&upstreams),
            LoadBalancingStrategy::WeightedRoundRobin => self.select_weighted_round_robin(&upstreams),
            LoadBalancingStrategy::LeastConnections => self.select_least_connections(&upstreams),
            LoadBalancingStrategy::ConsistentHashing => {
                if let Some(key) = session_key {
                    self.select_consistent_hash(&upstreams, key)
                } else {
                    self.select_round_robin(&upstreams) // Fallback
                }
            }
            LoadBalancingStrategy::HealthAware => self.select_health_aware(&upstreams),
        }
    }

    /// Round-robin selection.
    fn select_round_robin(&self, upstreams: &[Arc<UpstreamInfo>]) -> Option<Arc<UpstreamInfo>> {
        let available_upstreams: Vec<_> = upstreams.iter()
            .filter(|u| u.is_available())
            .collect();

        if available_upstreams.is_empty() {
            return None;
        }

        let index = self.rr_counter.fetch_add(1, Ordering::Relaxed) % available_upstreams.len();
        Some(available_upstreams[index].clone())
    }

    /// Weighted round-robin selection.
    fn select_weighted_round_robin(&self, upstreams: &[Arc<UpstreamInfo>]) -> Option<Arc<UpstreamInfo>> {
        let mut weighted_upstreams = Vec::new();
        
        for upstream in upstreams.iter().filter(|u| u.is_available()) {
            let weight = upstream.config.weight.max(1);
            for _ in 0..weight {
                weighted_upstreams.push(upstream.clone());
            }
        }

        if weighted_upstreams.is_empty() {
            return None;
        }

        let index = self.rr_counter.fetch_add(1, Ordering::Relaxed) % weighted_upstreams.len();
        Some(weighted_upstreams[index].clone())
    }

    /// Least connections selection.
    fn select_least_connections(&self, upstreams: &[Arc<UpstreamInfo>]) -> Option<Arc<UpstreamInfo>> {
        upstreams.iter()
            .filter(|u| u.is_available())
            .min_by_key(|u| u.stats.active_connections.load(Ordering::Relaxed))
            .cloned()
    }

    /// Consistent hashing selection.
    fn select_consistent_hash(&self, upstreams: &[Arc<UpstreamInfo>], key: &str) -> Option<Arc<UpstreamInfo>> {
        let hash_ring = self.hash_ring.read();
        if let Some(upstream_idx) = hash_ring.get_upstream(key) {
            upstreams.get(upstream_idx).cloned()
        } else {
            None
        }
    }

    /// Health-aware selection (weighted by health scores).
    fn select_health_aware(&self, upstreams: &[Arc<UpstreamInfo>]) -> Option<Arc<UpstreamInfo>> {
        let mut candidates: Vec<_> = upstreams.iter()
            .filter(|u| u.is_available())
            .map(|u| {
                let health_score = u.health_score();
                let weight = u.config.weight as f64;
                let combined_score = health_score * weight;
                (u.clone(), combined_score)
            })
            .filter(|(_, score)| *score > 0.0)
            .collect();

        if candidates.is_empty() {
            return None;
        }

        // Sort by combined score (descending)
        candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        // Use weighted random selection based on scores
        let total_score: f64 = candidates.iter().map(|(_, score)| *score).sum();
        let mut random_value = (self.rr_counter.fetch_add(1, Ordering::Relaxed) as f64 / u64::MAX as f64) * total_score;

        for (upstream, score) in &candidates {
            random_value -= score;
            if random_value <= 0.0 {
                return Some(upstream.clone());
            }
        }

        // Fallback to the first candidate
        candidates.into_iter().next().map(|(upstream, _)| upstream)
    }

    /// Get all upstream information.
    pub fn get_upstreams(&self) -> Vec<Arc<UpstreamInfo>> {
        self.upstreams.read().clone()
    }

    /// Get upstream by name.
    pub fn get_upstream_by_name(&self, name: &str) -> Option<Arc<UpstreamInfo>> {
        self.upstreams.read()
            .iter()
            .find(|u| u.config.name == name)
            .cloned()
    }

    /// Update health state for an upstream.
    pub fn update_upstream_health(&self, name: &str, state: HealthState) {
        if let Some(upstream) = self.get_upstream_by_name(name) {
            upstream.set_health_state(state);
            
            // Update consistent hash ring if needed
            if matches!(self.strategy, LoadBalancingStrategy::ConsistentHashing) {
                let upstreams = self.upstreams.read();
                self.hash_ring.write().update(&upstreams);
            }
        }
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new(LoadBalancingStrategy::HealthAware)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_upstream(name: &str, weight: u32) -> UpstreamConfig {
        UpstreamConfig {
            name: name.to_string(),
            address: "127.0.0.1:8080".to_string(),
            weight,
            priority: 0,
            timeout_ms: 5000,
            retry: Default::default(),
            health_check: None,
            tls: None,
        }
    }

    #[test]
    fn test_round_robin_selection() {
        let router = Router::new(LoadBalancingStrategy::RoundRobin);
        
        let configs = vec![
            create_test_upstream("upstream1", 1),
            create_test_upstream("upstream2", 1),
            create_test_upstream("upstream3", 1),
        ];
        
        router.update_upstreams(configs).unwrap();
        
        // Mark all upstreams as healthy
        router.update_upstream_health("upstream1", HealthState::Healthy);
        router.update_upstream_health("upstream2", HealthState::Healthy);
        router.update_upstream_health("upstream3", HealthState::Healthy);
        
        // Test round-robin behavior
        let selections: Vec<_> = (0..6)
            .map(|_| router.select_upstream(None).unwrap().config.name.clone())
            .collect();
        
        // Should cycle through upstreams in order
        assert_eq!(selections[0], "upstream1");
        assert_eq!(selections[1], "upstream2");
        assert_eq!(selections[2], "upstream3");
        assert_eq!(selections[3], "upstream1");
        assert_eq!(selections[4], "upstream2");
        assert_eq!(selections[5], "upstream3");
    }

    #[test]
    fn test_health_aware_selection() {
        let router = Router::new(LoadBalancingStrategy::HealthAware);
        
        let configs = vec![
            create_test_upstream("healthy", 1),
            create_test_upstream("unhealthy", 1),
        ];
        
        router.update_upstreams(configs).unwrap();
        
        // Mark one healthy and one unhealthy
        router.update_upstream_health("healthy", HealthState::Healthy);
        router.update_upstream_health("unhealthy", HealthState::Unhealthy);
        
        // Should always select the healthy upstream
        for _ in 0..10 {
            let selected = router.select_upstream(None).unwrap();
            assert_eq!(selected.config.name, "healthy");
        }
    }

    #[test]
    fn test_upstream_stats() {
        let config = create_test_upstream("test", 1);
        let upstream = UpstreamInfo::new(config);
        
        // Test success recording
        upstream.stats.record_success(Duration::from_millis(100));
        assert_eq!(upstream.stats.requests_sent.load(Ordering::Relaxed), 1);
        assert_eq!(upstream.stats.responses_received.load(Ordering::Relaxed), 1);
        assert_eq!(upstream.stats.success_rate(), 1.0);
        
        // Test failure recording
        upstream.stats.record_failure();
        assert_eq!(upstream.stats.requests_sent.load(Ordering::Relaxed), 2);
        assert_eq!(upstream.stats.responses_received.load(Ordering::Relaxed), 1);
        assert_eq!(upstream.stats.success_rate(), 0.5);
    }
}