// src/lib.rs
pub mod metrics;
pub mod proxies;
pub mod new_config;
pub mod modern_router;

// Re-export the main types for easy usage
pub use proxies::{ProxyManager, ProxyInstance, HostConfig, Proxy};
pub use new_config::NewProxyConfig;
pub use modern_router::ModernRouter;