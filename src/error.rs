//! Error types and result handling for Harbr Router.

use std::fmt;

/// Main error type for Harbr Router operations.
#[derive(Debug, Clone)]
pub enum Error {
    /// Configuration related errors
    Config {
        message: String,
        source: Option<String>,
    },
    /// Network related errors
    Network {
        message: String,
        kind: NetworkErrorKind,
    },
    /// Proxy operation errors
    Proxy {
        message: String,
        proxy_type: String,
        instance_name: String,
    },
    /// Resource management errors
    Resource {
        message: String,
        resource_type: String,
    },
    /// Internal system errors
    Internal {
        message: String,
        context: Option<String>,
    },
}

/// Network error categories
#[derive(Debug, Clone)]
pub enum NetworkErrorKind {
    /// Connection failed
    ConnectionFailed,
    /// Timeout occurred
    Timeout,
    /// DNS resolution failed
    DnsResolution,
    /// Certificate/TLS error
    Tls,
    /// Protocol error
    Protocol,
}

impl Error {
    /// Create a configuration error
    pub fn config<S: Into<String>>(message: S) -> Self {
        Self::Config {
            message: message.into(),
            source: None,
        }
    }

    /// Create a configuration error with source
    pub fn config_with_source<S: Into<String>, T: Into<String>>(message: S, source: T) -> Self {
        Self::Config {
            message: message.into(),
            source: Some(source.into()),
        }
    }

    /// Create a network error
    pub fn network<S: Into<String>>(message: S, kind: NetworkErrorKind) -> Self {
        Self::Network {
            message: message.into(),
            kind,
        }
    }

    /// Create a proxy error
    pub fn proxy<S: Into<String>, T: Into<String>, U: Into<String>>(
        message: S,
        proxy_type: T,
        instance_name: U,
    ) -> Self {
        Self::Proxy {
            message: message.into(),
            proxy_type: proxy_type.into(),
            instance_name: instance_name.into(),
        }
    }

    /// Create a resource error
    pub fn resource<S: Into<String>, T: Into<String>>(message: S, resource_type: T) -> Self {
        Self::Resource {
            message: message.into(),
            resource_type: resource_type.into(),
        }
    }

    /// Create an internal error
    pub fn internal<S: Into<String>>(message: S) -> Self {
        Self::Internal {
            message: message.into(),
            context: None,
        }
    }

    /// Create an internal error with context
    pub fn internal_with_context<S: Into<String>, T: Into<String>>(message: S, context: T) -> Self {
        Self::Internal {
            message: message.into(),
            context: Some(context.into()),
        }
    }

    /// Check if this is a configuration error
    pub fn is_config(&self) -> bool {
        matches!(self, Self::Config { .. })
    }

    /// Check if this is a network error
    pub fn is_network(&self) -> bool {
        matches!(self, Self::Network { .. })
    }

    /// Check if this is a proxy error
    pub fn is_proxy(&self) -> bool {
        matches!(self, Self::Proxy { .. })
    }

    /// Check if this is a resource error
    pub fn is_resource(&self) -> bool {
        matches!(self, Self::Resource { .. })
    }

    /// Check if this is an internal error
    pub fn is_internal(&self) -> bool {
        matches!(self, Self::Internal { .. })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config { message, source } => {
                write!(f, "Configuration error: {}", message)?;
                if let Some(source) = source {
                    write!(f, " (source: {})", source)?;
                }
                Ok(())
            }
            Self::Network { message, kind } => {
                write!(f, "Network error ({}): {}", kind, message)
            }
            Self::Proxy {
                message,
                proxy_type,
                instance_name,
            } => {
                write!(
                    f,
                    "Proxy error in {} instance '{}': {}",
                    proxy_type, instance_name, message
                )
            }
            Self::Resource {
                message,
                resource_type,
            } => {
                write!(f, "Resource error ({}): {}", resource_type, message)
            }
            Self::Internal { message, context } => {
                write!(f, "Internal error: {}", message)?;
                if let Some(context) = context {
                    write!(f, " (context: {})", context)?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for NetworkErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConnectionFailed => write!(f, "connection failed"),
            Self::Timeout => write!(f, "timeout"),
            Self::DnsResolution => write!(f, "DNS resolution"),
            Self::Tls => write!(f, "TLS/certificate"),
            Self::Protocol => write!(f, "protocol"),
        }
    }
}

impl std::error::Error for Error {}

// Conversions from common error types
impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        match err.kind() {
            std::io::ErrorKind::TimedOut => {
                Self::network(err.to_string(), NetworkErrorKind::Timeout)
            }
            std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset => {
                Self::network(err.to_string(), NetworkErrorKind::ConnectionFailed)
            }
            _ => Self::internal(err.to_string()),
        }
    }
}

impl From<serde_yaml::Error> for Error {
    fn from(err: serde_yaml::Error) -> Self {
        Self::config_with_source("YAML parsing failed", err.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Self::config_with_source("JSON parsing failed", err.to_string())
    }
}

impl From<toml::de::Error> for Error {
    fn from(err: toml::de::Error) -> Self {
        Self::config_with_source("TOML parsing failed", err.to_string())
    }
}

impl From<std::net::AddrParseError> for Error {
    fn from(err: std::net::AddrParseError) -> Self {
        Self::config_with_source("Invalid address format", err.to_string())
    }
}

/// Result type alias for Harbr Router operations.
pub type Result<T> = std::result::Result<T, Error>;