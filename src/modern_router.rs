use anyhow::Result;
use tokio::sync::mpsc;
use warp::Filter;
use crate::proxies::{ProxyManager, HttpProxy, TcpProxy, UdpProxy};
use crate::new_config::NewProxyConfig;
use crate::metrics;

pub struct ModernRouter {
    config: NewProxyConfig,
    proxy_manager: ProxyManager,
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl ModernRouter {
    pub fn new(config: NewProxyConfig) -> Self {
        Self {
            config,
            proxy_manager: ProxyManager::new(),
            shutdown_tx: None,
        }
    }
    
    pub async fn from_file(config_path: &str) -> Result<Self> {
        let config = NewProxyConfig::load_from_file(config_path)?;
        config.validate()?;
        Ok(Self::new(config))
    }
    
    pub async fn start(&mut self) -> Result<()> {
        tracing::info!("Starting ModernRouter with {} proxy instances", self.config.proxy_instances.len());
        
        if self.config.global.metrics.enabled {
            metrics::init_metrics()?;
            tracing::info!("Metrics enabled on {}", self.config.global.metrics.listen_addr);
        }
        
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);
        
        self.register_proxy_types().await;
        
        for instance in &self.config.proxy_instances {
            tracing::info!("Creating proxy instance '{}' of type '{}'", instance.name, instance.proxy_type);
            
            match self.proxy_manager.create_instance(instance.clone()).await {
                Ok(()) => {
                    tracing::info!("Successfully created proxy instance '{}'", instance.name);
                }
                Err(e) => {
                    tracing::error!("Failed to create proxy instance '{}': {}", instance.name, e);
                    return Err(e);
                }
            }
        }
        
        if self.config.global.health_check.enabled {
            self.start_health_check_server().await?;
        }
        
        if self.config.global.metrics.enabled {
            self.start_metrics_server().await?;
        }
        
        tracing::info!("All proxy instances started successfully");
        
        tokio::signal::ctrl_c().await?;
        tracing::info!("Received shutdown signal");
        
        self.shutdown().await?;
        Ok(())
    }
    
    async fn register_proxy_types(&self) {
        self.proxy_manager.register_proxy_type(Box::new(HttpProxy::new())).await;
        self.proxy_manager.register_proxy_type(Box::new(TcpProxy::new())).await;
        self.proxy_manager.register_proxy_type(Box::new(UdpProxy::new())).await;
    }
    
    async fn start_health_check_server(&self) -> Result<()> {
        let listen_addr = self.config.global.health_check.listen_addr.clone();
        let path = self.config.global.health_check.path.clone();
        
        let path_trimmed = path.trim_start_matches('/').to_string();
        let health_routes = warp::path(path_trimmed)
            .and(warp::get())
            .map(|| {
                warp::reply::json(&serde_json::json!({
                    "status": "healthy",
                    "timestamp": chrono::Utc::now().to_rfc3339()
                }))
            });
        
        tokio::spawn(async move {
            tracing::info!("Health check server starting on {}{}", listen_addr, path);
            warp::serve(health_routes)
                .run(listen_addr.parse::<std::net::SocketAddr>().unwrap())
                .await;
        });
        
        Ok(())
    }
    
    async fn start_metrics_server(&self) -> Result<()> {
        let listen_addr = self.config.global.metrics.listen_addr.clone();
        let path = self.config.global.metrics.path.clone();
        
        let path_trimmed = path.trim_start_matches('/').to_string();
        let metrics_routes = warp::path(path_trimmed)
            .and(warp::get())
            .and_then(|| async move {
                use metrics_exporter_prometheus::PrometheusHandle;
                use std::sync::OnceLock;
                
                static PROMETHEUS_HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();
                
                let handle = PROMETHEUS_HANDLE.get_or_init(|| {
                    metrics_exporter_prometheus::PrometheusBuilder::new()
                        .install_recorder()
                        .expect("Failed to install Prometheus recorder")
                });
                
                Ok::<_, warp::Rejection>(handle.render())
            });
        
        tokio::spawn(async move {
            tracing::info!("Metrics server starting on {}{}", listen_addr, path);
            warp::serve(metrics_routes)
                .run(listen_addr.parse::<std::net::SocketAddr>().unwrap())
                .await;
        });
        
        Ok(())
    }
    
    pub async fn add_host_to_instance(&self, instance_name: &str, host: crate::proxies::HostConfig) -> Result<()> {
        self.proxy_manager.add_host_to_instance(instance_name, host).await
    }
    
    pub async fn remove_host_from_instance(&self, instance_name: &str, host_name: &str) -> Result<()> {
        self.proxy_manager.remove_host_from_instance(instance_name, host_name).await
    }
    
    pub async fn list_instances(&self) -> Vec<crate::proxies::ProxyInstance> {
        self.proxy_manager.list_instances().await
    }
    
    pub async fn get_instance(&self, instance_name: &str) -> Option<crate::proxies::ProxyInstance> {
        self.proxy_manager.get_instance(instance_name).await
    }
    
    async fn shutdown(&mut self) -> Result<()> {
        tracing::info!("Shutting down proxy instances");
        
        for instance in &self.config.proxy_instances {
            if let Err(e) = self.proxy_manager.remove_instance(&instance.name).await {
                tracing::warn!("Error shutting down instance '{}': {}", instance.name, e);
            } else {
                tracing::info!("Successfully shut down instance '{}'", instance.name);
            }
        }
        
        if let Some(tx) = &self.shutdown_tx {
            let _ = tx.send(()).await;
        }
        
        tracing::info!("ModernRouter shutdown complete");
        Ok(())
    }
}