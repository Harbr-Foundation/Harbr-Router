//! # Harbr Router
//!
//! A high-performance, modular reverse proxy built with Rust.
//! 
//! ## Architecture
//! 
//! This crate follows Clean Architecture principles with clear separation of concerns:
//! 
//! - **Domain**: Core business logic and entities (`config`, `routing`)
//! - **Application**: Use cases and orchestration (`proxy`, `router`) 
//! - **Infrastructure**: External concerns (`metrics`, `server`)
//! - **Performance**: Optimizations and resource management (`buffers`, `affinity`)

pub mod config;
pub mod routing;
pub mod proxy;
pub mod router;
pub mod metrics;
pub mod server;
pub mod buffers;
pub mod affinity;
pub mod error;

// Re-export main public types
pub use config::Config;
pub use router::Router;
pub use error::{Error, Result};

/// Current version of the Harbr Router
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Maximum supported proxy instances per router
pub const MAX_PROXY_INSTANCES: usize = 1000;

/// Default configuration file name
pub const DEFAULT_CONFIG_FILE: &str = "config.yml";