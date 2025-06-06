// src/metrics.rs - Comprehensive metrics collection system
use crate::config::MonitoringConfig;
use crate::error::{RouterError, Result};
use crate::plugin::{PluginMetrics, RouterEvent};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::{mpsc, RwLock};
use tokio::time::interval;
use tracing::{debug, error, info, warn};

/// Main metrics collector
#[derive(Debug)]
pub struct MetricsCollector {
    config: MonitoringConfig,
    registry: Arc<MetricsRegistry>,
    exporters: Vec<Box<dyn MetricsExporter>>,
    shutdown_sender: Option<mpsc::Sender<()>>,
    running: Arc<RwLock<bool>>,
    start_time: Instant,
}

/// Central metrics registry
#[derive(Debug)]
pub struct MetricsRegistry {
    counters: RwLock<HashMap<String, Arc<Counter>>>,
    gauges: RwLock<HashMap<String, Arc<Gauge>>>,
    histograms: RwLock<HashMap<String, Arc<Histogram>>>,
    custom_metrics: RwLock<HashMap<String, serde_json::Value>>,
    labels: RwLock<HashMap<String, HashMap<String, String>>>,
}

/// Counter metric - monotonically increasing value
#[derive(Debug)]
pub struct Counter {
    name: String,
    value: RwLock<u64>,
    labels: HashMap<String, String>,
    created_at: SystemTime,
}

/// Gauge metric - value that can go up and down
#[derive(Debug)]
pub struct Gauge {
    name: String,
    value: RwLock<f64>,
    labels: HashMap<String, String>,
    created_at: SystemTime,
}

/// Histogram metric - distribution of values
#[derive(Debug)]
pub struct Histogram {
    name: String,
    buckets: Vec<f64>,
    counts: RwLock<Vec<u64>>,
    sum: RwLock<f64>,
    count: RwLock<u64>,
    labels: HashMap<String, String>,
    created_at: SystemTime,
}

/// Metrics snapshot for export
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub timestamp: SystemTime,
    pub counters: HashMap<String, CounterValue>,
    pub gauges: HashMap<String, GaugeValue>,
    pub histograms: HashMap<String, HistogramValue>,
    pub custom_metrics: HashMap<String, serde_json::Value>,
    pub system_metrics: SystemMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CounterValue {
    pub value: u64,
    pub labels: HashMap<String, String>,
    pub created_at: SystemTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GaugeValue {
    pub value: f64,
    pub labels: HashMap<String, String>,
    pub created_at: SystemTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistogramValue {
    pub buckets: Vec<f64>,
    pub counts: Vec<u64>,
    pub sum: f64,
    pub count: u64,
    pub labels: HashMap<String, String>,
    pub created_at: SystemTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMetrics {
    pub uptime_seconds: u64,
    pub memory_usage_bytes: u64,
    pub cpu_usage_percent: f64,
    pub goroutines: u64,
    pub open_file_descriptors: u64,
    pub network_connections: u64,
}

/// Trait for metrics exporters
pub trait MetricsExporter: Send + Sync + std::fmt::Debug {
    fn export(&self, snapshot: &MetricsSnapshot) -> Result<()>;
    fn name(&self) -> &str;
    fn shutdown(&self) -> Result<()>;
}

/// Prometheus metrics exporter
#[derive(Debug)]
pub struct PrometheusExporter {
    endpoint: String,
    push_gateway: Option<String>,
    job_name: String,
    client: reqwest::Client,
}

/// JSON metrics exporter
#[derive(Debug)]
pub struct JsonExporter {
    output_path: std::path::PathBuf,
    pretty_print: bool,
}

/// StatsD metrics exporter
#[derive(Debug)]
pub struct StatsdExporter {
    address: String,
    prefix: String,
    client: Option<tokio::net::UdpSocket>,
}

impl MetricsCollector {
    pub fn new(config: MonitoringConfig) -> Self {
        Self {
            config,
            registry: Arc::new(MetricsRegistry::new()),
            exporters: Vec::new(),
            shutdown_sender: None,
            running: Arc::new(RwLock::new(false)),
            start_time: Instant::now(),
        }
    }
    
    pub fn with_prometheus_exporter(mut self, endpoint: String) -> Self {
        let exporter = PrometheusExporter::new(endpoint);
        self.exporters.push(Box::new(exporter));
        self
    }
    
    pub fn with_json_exporter(mut self, output_path: std::path::PathBuf) -> Self {
        let exporter = JsonExporter::new(output_path);
        self.exporters.push(Box::new(exporter));
        self
    }
    
    pub fn with_statsd_exporter(mut self, address: String, prefix: String) -> Self {
        let exporter = StatsdExporter::new(address, prefix);
        self.exporters.push(Box::new(exporter));
        self
    }
    
    pub async fn start(&self) -> Result<()> {
        *self.running.write().await = true;
        
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
        // self.shutdown_sender = Some(shutdown_tx); // This would need to be mutable
        
        // Register default metrics
        self.register_default_metrics().await?;
        
        // Start collection loop
        let registry = self.registry.clone();
        let exporters = self.exporters.iter().map(|e| e.name().to_string()).collect::<Vec<_>>();
        let running = self.running.clone();
        let collection_interval = Duration::from_secs(self.config.metrics_collection_interval_seconds);
        
        tokio::spawn(async move {
            let mut interval = interval(collection_interval);
            
            while *running.read().await {
                tokio::select! {
                    _ = interval.tick() => {
                        if let Err(e) = Self::collect_and_export(&registry, &exporters).await {
                            error!("Metrics collection error: {}", e);
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        info!("Shutting down metrics collector");
                        break;
                    }
                }
            }
        });
        
        info!("Metrics collector started with {} exporters", self.exporters.len());
        Ok(())
    }
    
    pub async fn stop(&self) -> Result<()> {
        *self.running.write().await = false;
        
        if let Some(sender) = &self.shutdown_sender {
            let _ = sender.send(()).await;
        }
        
        // Shutdown exporters
        for exporter in &self.exporters {
            if let Err(e) = exporter.shutdown() {
                error!("Error shutting down exporter {}: {}", exporter.name(), e);
            }
        }
        
        info!("Metrics collector stopped");
        Ok(())
    }
    
    pub fn registry(&self) -> Arc<MetricsRegistry> {
        self.registry.clone()
    }
    
    pub async fn get_snapshot(&self) -> MetricsSnapshot {
        self.registry.snapshot().await
    }
    
    pub async fn handle_router_event(&self, event: RouterEvent) {
        match event {
            RouterEvent::PluginStarted(name) => {
                self.registry.increment_counter(&format!("plugin.{}.started", name), None).await;
            }
            RouterEvent::PluginStopped(name) => {
                self.registry.increment_counter(&format!("plugin.{}.stopped", name), None).await;
            }
            RouterEvent::PluginError { plugin_name, .. } => {
                self.registry.increment_counter(&format!("plugin.{}.errors", plugin_name), None).await;
            }
            RouterEvent::MetricsUpdated { plugin_name, metrics } => {
                self.update_plugin_metrics(&plugin_name, &metrics).await;
            }
            _ => {}
        }
    }
    
    async fn register_default_metrics(&self) -> Result<()> {
        // System metrics
        self.registry.create_gauge("system.uptime_seconds", None).await?;
        self.registry.create_gauge("system.memory_usage_bytes", None).await?;
        self.registry.create_gauge("system.cpu_usage_percent", None).await?;
        self.registry.create_gauge("system.open_file_descriptors", None).await?;
        
        // Router metrics
        self.registry.create_counter("router.requests_total", None).await?;
        self.registry.create_counter("router.responses_total", None).await?;
        self.registry.create_counter("router.errors_total", None).await?;
        self.registry.create_gauge("router.active_connections", None).await?;
        
        // Create histogram for request duration
        let buckets = vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0];
        self.registry.create_histogram("router.request_duration_seconds", buckets, None).await?;
        
        info!("Default metrics registered");
        Ok(())
    }
    
    async fn update_plugin_metrics(&self, plugin_name: &str, metrics: &PluginMetrics) {
        let labels = Some(vec![("plugin".to_string(), plugin_name.to_string())].into_iter().collect());
        
        // Update plugin-specific metrics
        self.registry.set_gauge(&format!("plugin.connections_total"), metrics.connections_total as f64, labels.clone()).await;
        self.registry.set_gauge(&format!("plugin.connections_active"), metrics.connections_active as f64, labels.clone()).await;
        self.registry.set_gauge(&format!("plugin.bytes_sent"), metrics.bytes_sent as f64, labels.clone()).await;
        self.registry.set_gauge(&format!("plugin.bytes_received"), metrics.bytes_received as f64, labels.clone()).await;
        self.registry.set_gauge(&format!("plugin.errors_total"), metrics.errors_total as f64, labels.clone()).await;
        
        // Update custom metrics
        for (key, value) in &metrics.custom_metrics {
            self.registry.set_gauge(&format!("plugin.custom.{}", key), *value, labels.clone()).await;
        }
    }
    
    async fn collect_and_export(
        registry: &MetricsRegistry,
        _exporters: &[String],
    ) -> Result<()> {
        // Update system metrics
        registry.set_gauge("system.uptime_seconds", get_uptime_seconds(), None).await;
        registry.set_gauge("system.memory_usage_bytes", get_memory_usage_bytes(), None).await;
        registry.set_gauge("system.cpu_usage_percent", get_cpu_usage_percent(), None).await;
        registry.set_gauge("system.open_file_descriptors", get_open_file_descriptors(), None).await;
        
        // Create snapshot
        let snapshot = registry.snapshot().await;
        
        // Export to all configured exporters
        // Note: In the real implementation, you'd iterate over actual exporter instances
        debug!("Collected metrics: {} counters, {} gauges, {} histograms", 
               snapshot.counters.len(), 
               snapshot.gauges.len(),
               snapshot.histograms.len());
        
        Ok(())
    }
}

impl MetricsRegistry {
    pub fn new() -> Self {
        Self {
            counters: RwLock::new(HashMap::new()),
            gauges: RwLock::new(HashMap::new()),
            histograms: RwLock::new(HashMap::new()),
            custom_metrics: RwLock::new(HashMap::new()),
            labels: RwLock::new(HashMap::new()),
        }
    }
    
    pub async fn create_counter(&self, name: &str, labels: Option<HashMap<String, String>>) -> Result<()> {
        let counter = Arc::new(Counter::new(name, labels.unwrap_or_default()));
        let mut counters = self.counters.write().await;
        counters.insert(name.to_string(), counter);
        Ok(())
    }
    
    pub async fn create_gauge(&self, name: &str, labels: Option<HashMap<String, String>>) -> Result<()> {
        let gauge = Arc::new(Gauge::new(name, labels.unwrap_or_default()));
        let mut gauges = self.gauges.write().await;
        gauges.insert(name.to_string(), gauge);
        Ok(())
    }
    
    pub async fn create_histogram(&self, name: &str, buckets: Vec<f64>, labels: Option<HashMap<String, String>>) -> Result<()> {
        let histogram = Arc::new(Histogram::new(name, buckets, labels.unwrap_or_default()));
        let mut histograms = self.histograms.write().await;
        histograms.insert(name.to_string(), histogram);
        Ok(())
    }
    
    pub async fn increment_counter(&self, name: &str, labels: Option<HashMap<String, String>>) {
        let counters = self.counters.read().await;
        if let Some(counter) = counters.get(name) {
            counter.increment().await;
        } else {
            // Auto-create counter if it doesn't exist
            drop(counters);
            let _ = self.create_counter(name, labels).await;
            let counters = self.counters.read().await;
            if let Some(counter) = counters.get(name) {
                counter.increment().await;
            }
        }
    }
    
    pub async fn add_to_counter(&self, name: &str, value: u64, _labels: Option<HashMap<String, String>>) {
        let counters = self.counters.read().await;
        if let Some(counter) = counters.get(name) {
            counter.add(value).await;
        }
    }
    
    pub async fn set_gauge(&self, name: &str, value: f64, labels: Option<HashMap<String, String>>) {
        let gauges = self.gauges.read().await;
        if let Some(gauge) = gauges.get(name) {
            gauge.set(value).await;
        } else {
            // Auto-create gauge if it doesn't exist
            drop(gauges);
            let _ = self.create_gauge(name, labels).await;
            let gauges = self.gauges.read().await;
            if let Some(gauge) = gauges.get(name) {
                gauge.set(value).await;
            }
        }
    }
    
    pub async fn observe_histogram(&self, name: &str, value: f64, _labels: Option<HashMap<String, String>>) {
        let histograms = self.histograms.read().await;
        if let Some(histogram) = histograms.get(name) {
            histogram.observe(value).await;
        }
    }
    
    pub async fn set_custom_metric(&self, name: &str, value: serde_json::Value) {
        let mut custom_metrics = self.custom_metrics.write().await;
        custom_metrics.insert(name.to_string(), value);
    }
    
    pub async fn snapshot(&self) -> MetricsSnapshot {
        let counters = self.counters.read().await;
        let gauges = self.gauges.read().await;
        let histograms = self.histograms.read().await;
        let custom_metrics = self.custom_metrics.read().await;
        
        let mut counter_values = HashMap::new();
        for (name, counter) in counters.iter() {
            counter_values.insert(name.clone(), counter.snapshot().await);
        }
        
        let mut gauge_values = HashMap::new();
        for (name, gauge) in gauges.iter() {
            gauge_values.insert(name.clone(), gauge.snapshot().await);
        }
        
        let mut histogram_values = HashMap::new();
        for (name, histogram) in histograms.iter() {
            histogram_values.insert(name.clone(), histogram.snapshot().await);
        }
        
        MetricsSnapshot {
            timestamp: SystemTime::now(),
            counters: counter_values,
            gauges: gauge_values,
            histograms: histogram_values,
            custom_metrics: custom_metrics.clone(),
            system_metrics: SystemMetrics {
                uptime_seconds: get_uptime_seconds() as u64,
                memory_usage_bytes: get_memory_usage_bytes() as u64,
                cpu_usage_percent: get_cpu_usage_percent(),
                goroutines: 0, // Not applicable to Rust
                open_file_descriptors: get_open_file_descriptors() as u64,
                network_connections: get_network_connections(),
            },
        }
    }
}

impl Counter {
    fn new(name: &str, labels: HashMap<String, String>) -> Self {
        Self {
            name: name.to_string(),
            value: RwLock::new(0),
            labels,
            created_at: SystemTime::now(),
        }
    }
    
    pub async fn increment(&self) {
        let mut value = self.value.write().await;
        *value += 1;
    }
    
    pub async fn add(&self, amount: u64) {
        let mut value = self.value.write().await;
        *value += amount;
    }
    
    pub async fn get(&self) -> u64 {
        *self.value.read().await
    }
    
    pub async fn snapshot(&self) -> CounterValue {
        CounterValue {
            value: self.get().await,
            labels: self.labels.clone(),
            created_at: self.created_at,
        }
    }
}

impl Gauge {
    fn new(name: &str, labels: HashMap<String, String>) -> Self {
        Self {
            name: name.to_string(),
            value: RwLock::new(0.0),
            labels,
            created_at: SystemTime::now(),
        }
    }
    
    pub async fn set(&self, value: f64) {
        let mut val = self.value.write().await;
        *val = value;
    }
    
    pub async fn add(&self, amount: f64) {
        let mut val = self.value.write().await;
        *val += amount;
    }
    
    pub async fn get(&self) -> f64 {
        *self.value.read().await
    }
    
    pub async fn snapshot(&self) -> GaugeValue {
        GaugeValue {
            value: self.get().await,
            labels: self.labels.clone(),
            created_at: self.created_at,
        }
    }
}

impl Histogram {
    fn new(name: &str, buckets: Vec<f64>, labels: HashMap<String, String>) -> Self {
        let counts = vec![0u64; buckets.len() + 1]; // +1 for +Inf bucket
        Self {
            name: name.to_string(),
            buckets,
            counts: RwLock::new(counts),
            sum: RwLock::new(0.0),
            count: RwLock::new(0),
            labels,
            created_at: SystemTime::now(),
        }
    }
    
    pub async fn observe(&self, value: f64) {
        let mut counts = self.counts.write().await;
        let mut sum = self.sum.write().await;
        let mut count = self.count.write().await;
        
        *sum += value;
        *count += 1;
        
        // Find the appropriate bucket
        for (i, &bucket) in self.buckets.iter().enumerate() {
            if value <= bucket {
                counts[i] += 1;
                break;
            }
        }
        // Also increment the +Inf bucket
        if let Some(last) = counts.last_mut() {
            *last += 1;
        }
    }
    
    pub async fn snapshot(&self) -> HistogramValue {
        HistogramValue {
            buckets: self.buckets.clone(),
            counts: self.counts.read().await.clone(),
            sum: *self.sum.read().await,
            count: *self.count.read().await,
            labels: self.labels.clone(),
            created_at: self.created_at,
        }
    }
}

// Exporter implementations
impl PrometheusExporter {
    pub fn new(endpoint: String) -> Self {
        Self {
            endpoint,
            push_gateway: None,
            job_name: "router".to_string(),
            client: reqwest::Client::new(),
        }
    }
    
    pub fn with_push_gateway(mut self, gateway: String, job_name: String) -> Self {
        self.push_gateway = Some(gateway);
        self.job_name = job_name;
        self
    }
}

impl MetricsExporter for PrometheusExporter {
    fn export(&self, snapshot: &MetricsSnapshot) -> Result<()> {
        let mut output = String::new();
        
        // Export counters
        for (name, counter) in &snapshot.counters {
            output.push_str(&format!("# TYPE {} counter\n", name));
            let labels = format_labels(&counter.labels);
            output.push_str(&format!("{}{} {}\n", name, labels, counter.value));
        }
        
        // Export gauges
        for (name, gauge) in &snapshot.gauges {
            output.push_str(&format!("# TYPE {} gauge\n", name));
            let labels = format_labels(&gauge.labels);
            output.push_str(&format!("{}{} {}\n", name, labels, gauge.value));
        }
        
        // Export histograms
        for (name, histogram) in &snapshot.histograms {
            output.push_str(&format!("# TYPE {} histogram\n", name));
            let labels = format_labels(&histogram.labels);
            
            // Bucket counts
            for (i, &bucket) in histogram.buckets.iter().enumerate() {
                let mut bucket_labels = histogram.labels.clone();
                bucket_labels.insert("le".to_string(), bucket.to_string());
                let bucket_labels_str = format_labels(&bucket_labels);
                output.push_str(&format!("{}_bucket{} {}\n", name, bucket_labels_str, histogram.counts[i]));
            }
            
            // +Inf bucket
            let mut inf_labels = histogram.labels.clone();
            inf_labels.insert("le".to_string(), "+Inf".to_string());
            let inf_labels_str = format_labels(&inf_labels);
            if let Some(&inf_count) = histogram.counts.last() {
                output.push_str(&format!("{}_bucket{} {}\n", name, inf_labels_str, inf_count));
            }
            
            // Sum and count
            output.push_str(&format!("{}_sum{} {}\n", name, labels, histogram.sum));
            output.push_str(&format!("{}_count{} {}\n", name, labels, histogram.count));
        }
        
        debug!("Generated Prometheus metrics: {} bytes", output.len());
        
        // If push gateway is configured, push metrics
        if let Some(_gateway) = &self.push_gateway {
            // Implementation would push to Prometheus Push Gateway
            debug!("Would push metrics to push gateway");
        }
        
        Ok(())
    }
    
    fn name(&self) -> &str {
        "prometheus"
    }
    
    fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

impl JsonExporter {
    pub fn new(output_path: std::path::PathBuf) -> Self {
        Self {
            output_path,
            pretty_print: true,
        }
    }
}

impl MetricsExporter for JsonExporter {
    fn export(&self, snapshot: &MetricsSnapshot) -> Result<()> {
        let json = if self.pretty_print {
            serde_json::to_string_pretty(snapshot)?
        } else {
            serde_json::to_string(snapshot)?
        };
        
        std::fs::write(&self.output_path, json)?;
        debug!("Exported metrics to {}", self.output_path.display());
        
        Ok(())
    }
    
    fn name(&self) -> &str {
        "json"
    }
    
    fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

impl StatsdExporter {
    pub fn new(address: String, prefix: String) -> Self {
        Self {
            address,
            prefix,
            client: None,
        }
    }
}

impl MetricsExporter for StatsdExporter {
    fn export(&self, snapshot: &MetricsSnapshot) -> Result<()> {
        // Implementation would send metrics to StatsD
        debug!("Would export {} metrics to StatsD at {}", 
               snapshot.counters.len() + snapshot.gauges.len(), 
               self.address);
        Ok(())
    }
    
    fn name(&self) -> &str {
        "statsd"
    }
    
    fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

// Utility functions
fn format_labels(labels: &HashMap<String, String>) -> String {
    if labels.is_empty() {
        return String::new();
    }
    
    let mut parts = Vec::new();
    for (key, value) in labels {
        parts.push(format!("{}=\"{}\"", key, value));
    }
    
    format!("{{{}}}", parts.join(","))
}

// System metrics collection
fn get_uptime_seconds() -> f64 {
    // Implementation would get actual system uptime
    0.0
}

fn get_memory_usage_bytes() -> f64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(process) = procfs::process::Process::myself() {
            if let Ok(stat) = process.stat() {
                return (stat.rss * 4096) as f64; // Convert pages to bytes
            }
        }
    }
    0.0
}

fn get_cpu_usage_percent() -> f64 {
    // Implementation would calculate actual CPU usage
    0.0
}

fn get_open_file_descriptors() -> f64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(process) = procfs::process::Process::myself() {
            if let Ok(fd_count) = process.fd_count() {
                return fd_count as f64;
            }
        }
    }
    0.0
}

fn get_network_connections() -> u64 {
    // Implementation would count network connections
    0
}