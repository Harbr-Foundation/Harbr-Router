// src/error.rs - Comprehensive error handling
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

/// Main router error type
#[derive(Error, Debug, Clone, Serialize, Deserialize)]
pub enum RouterError {
    // Configuration errors
    #[error("Configuration error: {message}")]
    ConfigError { message: String },
    
    #[error("Invalid configuration file: {path}")]
    InvalidConfigFile { path: String },
    
    #[error("Configuration validation failed: {errors:?}")]
    ConfigValidationError { errors: Vec<String> },
    
    // Plugin errors
    #[error("Plugin error in '{plugin}': {message}")]
    PluginError { plugin: String, message: String },
    
    #[error("Plugin '{plugin}' not found")]
    PluginNotFound { plugin: String },
    
    #[error("Failed to load plugin '{plugin}': {reason}")]
    PluginLoadError { plugin: String, reason: String },
    
    #[error("Plugin '{plugin}' initialization failed: {reason}")]
    PluginInitError { plugin: String, reason: String },
    
    #[error("Plugin '{plugin}' is not compatible: {reason}")]
    PluginCompatibilityError { plugin: String, reason: String },
    
    // Server errors
    #[error("Server error: {message}")]
    ServerError { message: String },
    
    #[error("Failed to bind to address '{address}': {reason}")]
    BindError { address: String, reason: String },
    
    #[error("Server is not running")]
    ServerNotRunning,
    
    #[error("Server startup timeout")]
    ServerStartupTimeout,
    
    #[error("Graceful shutdown failed: {reason}")]
    ShutdownError { reason: String },
    
    // Network/Connection errors
    #[error("Connection error from {client}: {message}")]
    ConnectionError { client: String, message: String },
    
    #[error("Network timeout: {operation}")]
    NetworkTimeout { operation: String },
    
    #[error("DNS resolution failed for '{hostname}': {reason}")]
    DnsError { hostname: String, reason: String },
    
    #[error("SSL/TLS error: {message}")]
    SslError { message: String },
    
    // Request/Response errors
    #[error("Request processing error: {message}")]
    RequestError { message: String },
    
    #[error("Invalid request: {reason}")]
    InvalidRequest { reason: String },
    
    #[error("Request timeout after {timeout_ms}ms")]
    RequestTimeout { timeout_ms: u64 },
    
    #[error("Request too large: {size} bytes (max: {max_size})")]
    RequestTooLarge { size: usize, max_size: usize },
    
    #[error("Response processing error: {message}")]
    ResponseError { message: String },
    
    #[error("Upstream error: {message}")]
    UpstreamError { message: String },
    
    #[error("Backend connection failed: {backend}")]
    BackendConnectionError { backend: String },
    
    // Routing errors
    #[error("No route found for {domain}{path}")]
    RouteNotFound { domain: String, path: String },
    
    #[error("Routing error: {message}")]
    RoutingError { message: String },
    
    #[error("Load balancing error: {message}")]
    LoadBalancingError { message: String },
    
    // Security errors
    #[error("Authentication failed: {reason}")]
    AuthenticationError { reason: String },
    
    #[error("Authorization denied: {reason}")]
    AuthorizationError { reason: String },
    
    #[error("Rate limit exceeded for {identifier}")]
    RateLimitExceeded { identifier: String },
    
    #[error("Security violation: {message}")]
    SecurityViolation { message: String },
    
    #[error("CORS error: {message}")]
    CorsError { message: String },
    
    // Circuit breaker errors
    #[error("Circuit breaker is open for {service}")]
    CircuitBreakerOpen { service: String },
    
    #[error("Circuit breaker error: {message}")]
    CircuitBreakerError { message: String },
    
    // Health check errors
    #[error("Health check failed for {service}: {reason}")]
    HealthCheckFailed { service: String, reason: String },
    
    #[error("Service unavailable: {service}")]
    ServiceUnavailable { service: String },
    
    // Resource errors
    #[error("Resource exhausted: {resource}")]
    ResourceExhausted { resource: String },
    
    #[error("Memory limit exceeded: {current} bytes (limit: {limit})")]
    MemoryLimitExceeded { current: usize, limit: usize },
    
    #[error("Connection pool exhausted for {backend}")]
    ConnectionPoolExhausted { backend: String },
    
    // I/O errors
    #[error("I/O error: {message}")]
    IoError { message: String },
    
    #[error("File not found: {path}")]
    FileNotFound { path: String },
    
    #[error("Permission denied: {path}")]
    PermissionDenied { path: String },
    
    // Serialization/parsing errors
    #[error("JSON parsing error: {message}")]
    JsonError { message: String },
    
    #[error("YAML parsing error: {message}")]
    YamlError { message: String },
    
    #[error("URL parsing error: {url}")]
    UrlParsingError { url: String },
    
    #[error("Header parsing error: {header}")]
    HeaderParsingError { header: String },
    
    // Middleware errors
    #[error("Middleware '{middleware}' error: {message}")]
    MiddlewareError { middleware: String, message: String },
    
    #[error("Middleware chain error: {message}")]
    MiddlewareChainError { message: String },
    
    // Management API errors
    #[error("Management API error: {message}")]
    ManagementApiError { message: String },
    
    #[error("Invalid API request: {reason}")]
    InvalidApiRequest { reason: String },
    
    #[error("API authentication required")]
    ApiAuthRequired,
    
    // Metrics/monitoring errors
    #[error("Metrics collection error: {message}")]
    MetricsError { message: String },
    
    #[error("Monitoring error: {message}")]
    MonitoringError { message: String },
    
    // Generic/unknown errors
    #[error("Internal error: {message}")]
    InternalError { message: String },
    
    #[error("Unknown error: {message}")]
    UnknownError { message: String },
    
    #[error("Operation not supported: {operation}")]
    NotSupported { operation: String },
    
    #[error("Feature not implemented: {feature}")]
    NotImplemented { feature: String },
}

/// Result type alias for router operations
pub type Result<T> = std::result::Result<T, RouterError>;

impl RouterError {
    /// Get the error category
    pub fn category(&self) -> ErrorCategory {
        match self {
            RouterError::ConfigError { .. } |
            RouterError::InvalidConfigFile { .. } |
            RouterError::ConfigValidationError { .. } => ErrorCategory::Configuration,
            
            RouterError::PluginError { .. } |
            RouterError::PluginNotFound { .. } |
            RouterError::PluginLoadError { .. } |
            RouterError::PluginInitError { .. } |
            RouterError::PluginCompatibilityError { .. } => ErrorCategory::Plugin,
            
            RouterError::ServerError { .. } |
            RouterError::BindError { .. } |
            RouterError::ServerNotRunning |
            RouterError::ServerStartupTimeout |
            RouterError::ShutdownError { .. } => ErrorCategory::Server,
            
            RouterError::ConnectionError { .. } |
            RouterError::NetworkTimeout { .. } |
            RouterError::DnsError { .. } |
            RouterError::SslError { .. } => ErrorCategory::Network,
            
            RouterError::RequestError { .. } |
            RouterError::InvalidRequest { .. } |
            RouterError::RequestTimeout { .. } |
            RouterError::RequestTooLarge { .. } |
            RouterError::ResponseError { .. } |
            RouterError::UpstreamError { .. } |
            RouterError::BackendConnectionError { .. } => ErrorCategory::Request,
            
            RouterError::RouteNotFound { .. } |
            RouterError::RoutingError { .. } |
            RouterError::LoadBalancingError { .. } => ErrorCategory::Routing,
            
            RouterError::AuthenticationError { .. } |
            RouterError::AuthorizationError { .. } |
            RouterError::RateLimitExceeded { .. } |
            RouterError::SecurityViolation { .. } |
            RouterError::CorsError { .. } => ErrorCategory::Security,
            
            RouterError::CircuitBreakerOpen { .. } |
            RouterError::CircuitBreakerError { .. } => ErrorCategory::CircuitBreaker,
            
            RouterError::HealthCheckFailed { .. } |
            RouterError::ServiceUnavailable { .. } => ErrorCategory::Health,
            
            RouterError::ResourceExhausted { .. } |
            RouterError::MemoryLimitExceeded { .. } |
            RouterError::ConnectionPoolExhausted { .. } => ErrorCategory::Resource,
            
            RouterError::IoError { .. } |
            RouterError::FileNotFound { .. } |
            RouterError::PermissionDenied { .. } => ErrorCategory::Io,
            
            RouterError::JsonError { .. } |
            RouterError::YamlError { .. } |
            RouterError::UrlParsingError { .. } |
            RouterError::HeaderParsingError { .. } => ErrorCategory::Parsing,
            
            RouterError::MiddlewareError { .. } |
            RouterError::MiddlewareChainError { .. } => ErrorCategory::Middleware,
            
            RouterError::ManagementApiError { .. } |
            RouterError::InvalidApiRequest { .. } |
            RouterError::ApiAuthRequired => ErrorCategory::Api,
            
            RouterError::MetricsError { .. } |
            RouterError::MonitoringError { .. } => ErrorCategory::Monitoring,
            
            RouterError::InternalError { .. } |
            RouterError::UnknownError { .. } |
            RouterError::NotSupported { .. } |
            RouterError::NotImplemented { .. } => ErrorCategory::Internal,
        }
    }
    
    /// Get the HTTP status code that should be returned for this error
    pub fn http_status_code(&self) -> u16 {
        match self {
            RouterError::InvalidRequest { .. } |
            RouterError::HeaderParsingError { .. } |
            RouterError::UrlParsingError { .. } => 400, // Bad Request
            
            RouterError::AuthenticationError { .. } |
            RouterError::ApiAuthRequired => 401, // Unauthorized
            
            RouterError::AuthorizationError { .. } => 403, // Forbidden
            
            RouterError::RouteNotFound { .. } |
            RouterError::PluginNotFound { .. } |
            RouterError::FileNotFound { .. } => 404, // Not Found
            
            RouterError::RequestTooLarge { .. } => 413, // Payload Too Large
            
            RouterError::RateLimitExceeded { .. } => 429, // Too Many Requests
            
            RouterError::InternalError { .. } |
            RouterError::ServerError { .. } |
            RouterError::PluginError { .. } |
            RouterError::ConfigError { .. } => 500, // Internal Server Error
            
            RouterError::NotImplemented { .. } |
            RouterError::NotSupported { .. } => 501, // Not Implemented
            
            RouterError::BackendConnectionError { .. } |
            RouterError::UpstreamError { .. } => 502, // Bad Gateway
            
            RouterError::ServiceUnavailable { .. } |
            RouterError::CircuitBreakerOpen { .. } |
            RouterError::ServerNotRunning => 503, // Service Unavailable
            
            RouterError::RequestTimeout { .. } |
            RouterError::NetworkTimeout { .. } => 504, // Gateway Timeout
            
            _ => 500, // Default to Internal Server Error
        }
    }
    
    /// Check if the error is recoverable
    pub fn is_recoverable(&self) -> bool {
        match self {
            RouterError::NetworkTimeout { .. } |
            RouterError::RequestTimeout { .. } |
            RouterError::BackendConnectionError { .. } |
            RouterError::CircuitBreakerOpen { .. } |
            RouterError::ResourceExhausted { .. } |
            RouterError::ConnectionPoolExhausted { .. } => true,
            
            RouterError::ConfigError { .. } |
            RouterError::PluginLoadError { .. } |
            RouterError::PluginInitError { .. } |
            RouterError::ServerNotRunning |
            RouterError::InvalidRequest { .. } |
            RouterError::AuthenticationError { .. } |
            RouterError::AuthorizationError { .. } |
            RouterError::RouteNotFound { .. } => false,
            
            _ => false, // Conservative approach - assume not recoverable
        }
    }
    
    /// Check if the error should be logged
    pub fn should_log(&self) -> bool {
        match self {
            RouterError::RouteNotFound { .. } |
            RouterError::AuthenticationError { .. } |
            RouterError::RateLimitExceeded { .. } => false, // These are expected and shouldn't clutter logs
            
            _ => true,
        }
    }
    
    /// Get the log level for this error
    pub fn log_level(&self) -> LogLevel {
        match self {
            RouterError::ConfigError { .. } |
            RouterError::PluginLoadError { .. } |
            RouterError::ServerError { .. } |
            RouterError::InternalError { .. } => LogLevel::Error,
            
            RouterError::BackendConnectionError { .. } |
            RouterError::UpstreamError { .. } |
            RouterError::CircuitBreakerOpen { .. } |
            RouterError::HealthCheckFailed { .. } => LogLevel::Warn,
            
            RouterError::RequestTimeout { .. } |
            RouterError::NetworkTimeout { .. } |
            RouterError::RateLimitExceeded { .. } => LogLevel::Info,
            
            RouterError::RouteNotFound { .. } |
            RouterError::InvalidRequest { .. } => LogLevel::Debug,
            
            _ => LogLevel::Warn,
        }
    }
    
    /// Convert to a user-friendly error message
    pub fn user_message(&self) -> String {
        match self {
            RouterError::RouteNotFound { .. } => "The requested resource was not found".to_string(),
            RouterError::ServiceUnavailable { .. } => "Service temporarily unavailable".to_string(),
            RouterError::RequestTimeout { .. } => "Request timed out".to_string(),
            RouterError::RateLimitExceeded { .. } => "Too many requests. Please try again later".to_string(),
            RouterError::AuthenticationError { .. } => "Authentication required".to_string(),
            RouterError::AuthorizationError { .. } => "Access denied".to_string(),
            RouterError::RequestTooLarge { .. } => "Request entity too large".to_string(),
            _ => "An error occurred while processing your request".to_string(),
        }
    }
}

/// Error categories for classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCategory {
    Configuration,
    Plugin,
    Server,
    Network,
    Request,
    Routing,
    Security,
    CircuitBreaker,
    Health,
    Resource,
    Io,
    Parsing,
    Middleware,
    Api,
    Monitoring,
    Internal,
}

/// Log levels
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

/// Error context for debugging
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorContext {
    pub error: RouterError,
    pub timestamp: std::time::SystemTime,
    pub request_id: Option<String>,
    pub plugin_name: Option<String>,
    pub client_ip: Option<String>,
    pub user_agent: Option<String>,
    pub additional_context: std::collections::HashMap<String, String>,
}

impl ErrorContext {
    pub fn new(error: RouterError) -> Self {
        Self {
            error,
            timestamp: std::time::SystemTime::now(),
            request_id: None,
            plugin_name: None,
            client_ip: None,
            user_agent: None,
            additional_context: std::collections::HashMap::new(),
        }
    }
    
    pub fn with_request_id(mut self, request_id: String) -> Self {
        self.request_id = Some(request_id);
        self
    }
    
    pub fn with_plugin(mut self, plugin_name: String) -> Self {
        self.plugin_name = Some(plugin_name);
        self
    }
    
    pub fn with_client_info(mut self, client_ip: String, user_agent: Option<String>) -> Self {
        self.client_ip = Some(client_ip);
        self.user_agent = user_agent;
        self
    }
    
    pub fn with_context(mut self, key: String, value: String) -> Self {
        self.additional_context.insert(key, value);
        self
    }
}

/// Error handler trait for customizing error handling
pub trait ErrorHandler: Send + Sync {
    fn handle_error(&self, context: &ErrorContext) -> Result<()>;
}

/// Default error handler implementation
pub struct DefaultErrorHandler;

impl ErrorHandler for DefaultErrorHandler {
    fn handle_error(&self, context: &ErrorContext) -> Result<()> {
        let level = context.error.log_level();
        let should_log = context.error.should_log();
        
        if should_log {
            let message = format!(
                "Error in {}: {} [Category: {:?}]",
                context.plugin_name.as_deref().unwrap_or("system"),
                context.error,
                context.error.category()
            );
            
            match level {
                LogLevel::Error => tracing::error!("{}", message),
                LogLevel::Warn => tracing::warn!("{}", message),
                LogLevel::Info => tracing::info!("{}", message),
                LogLevel::Debug => tracing::debug!("{}", message),
            }
        }
        
        Ok(())
    }
}

/// Conversion implementations from common error types
impl From<std::io::Error> for RouterError {
    fn from(err: std::io::Error) -> Self {
        match err.kind() {
            std::io::ErrorKind::NotFound => RouterError::FileNotFound {
                path: "unknown".to_string(),
            },
            std::io::ErrorKind::PermissionDenied => RouterError::PermissionDenied {
                path: "unknown".to_string(),
            },
            std::io::ErrorKind::TimedOut => RouterError::NetworkTimeout {
                operation: "I/O operation".to_string(),
            },
            _ => RouterError::IoError {
                message: err.to_string(),
            },
        }
    }
}

impl From<serde_json::Error> for RouterError {
    fn from(err: serde_json::Error) -> Self {
        RouterError::JsonError {
            message: err.to_string(),
        }
    }
}

impl From<url::ParseError> for RouterError {
    fn from(err: url::ParseError) -> Self {
        RouterError::UrlParsingError {
            url: err.to_string(),
        }
    }
}

impl From<tokio::time::error::Elapsed> for RouterError {
    fn from(_: tokio::time::error::Elapsed) -> Self {
        RouterError::NetworkTimeout {
            operation: "async operation".to_string(),
        }
    }
}

/// Utility functions for error handling
pub mod utils {
    use super::*;
    
    /// Create a configuration error
    pub fn config_error<S: Into<String>>(message: S) -> RouterError {
        RouterError::ConfigError {
            message: message.into(),
        }
    }
    
    /// Create a plugin error
    pub fn plugin_error<S: Into<String>>(plugin: S, message: S) -> RouterError {
        RouterError::PluginError {
            plugin: plugin.into(),
            message: message.into(),
        }
    }
    
    /// Create a server error
    pub fn server_error<S: Into<String>>(message: S) -> RouterError {
        RouterError::ServerError {
            message: message.into(),
        }
    }
    
    /// Create a request error
    pub fn request_error<S: Into<String>>(message: S) -> RouterError {
        RouterError::RequestError {
            message: message.into(),
        }
    }
    
    /// Create a routing error
    pub fn routing_error<S: Into<String>>(message: S) -> RouterError {
        RouterError::RoutingError {
            message: message.into(),
        }
    }
    
    /// Create an internal error
    pub fn internal_error<S: Into<String>>(message: S) -> RouterError {
        RouterError::InternalError {
            message: message.into(),
        }
    }
    
    /// Chain errors with context
    pub fn chain_error<E: std::error::Error>(error: E, context: &str) -> RouterError {
        RouterError::InternalError {
            message: format!("{}: {}", context, error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_error_categorization() {
        let config_error = RouterError::ConfigError {
            message: "test".to_string(),
        };
        assert_eq!(config_error.category(), ErrorCategory::Configuration);
        
        let plugin_error = RouterError::PluginError {
            plugin: "test".to_string(),
            message: "test".to_string(),
        };
        assert_eq!(plugin_error.category(), ErrorCategory::Plugin);
    }
    
    #[test]
    fn test_http_status_codes() {
        let not_found = RouterError::RouteNotFound {
            domain: "example.com".to_string(),
            path: "/test".to_string(),
        };
        assert_eq!(not_found.http_status_code(), 404);
        
        let auth_error = RouterError::AuthenticationError {
            reason: "invalid token".to_string(),
        };
        assert_eq!(auth_error.http_status_code(), 401);
        
        let rate_limit = RouterError::RateLimitExceeded {
            identifier: "127.0.0.1".to_string(),
        };
        assert_eq!(rate_limit.http_status_code(), 429);
    }
    
    #[test]
    fn test_error_recoverability() {
        let timeout_error = RouterError::RequestTimeout { timeout_ms: 5000 };
        assert!(timeout_error.is_recoverable());
        
        let config_error = RouterError::ConfigError {
            message: "test".to_string(),
        };
        assert!(!config_error.is_recoverable());
    }
    
    #[test]
    fn test_error_context() {
        let error = RouterError::RequestError {
            message: "test error".to_string(),
        };
        
        let context = ErrorContext::new(error)
            .with_request_id("req-123".to_string())
            .with_plugin("http_proxy".to_string())
            .with_client_info("127.0.0.1".to_string(), Some("curl/7.68.0".to_string()))
            .with_context("additional".to_string(), "info".to_string());
        
        assert_eq!(context.request_id, Some("req-123".to_string()));
        assert_eq!(context.plugin_name, Some("http_proxy".to_string()));
        assert_eq!(context.client_ip, Some("127.0.0.1".to_string()));
        assert_eq!(context.additional_context.get("additional"), Some(&"info".to_string()));
    }
}