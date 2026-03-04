//! Error types for the streaming architecture

use crate::streaming::ConnectionId;
use std::time::Duration;
use thiserror::Error;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Result type alias for streaming operations
pub type StreamResult<T> = std::result::Result<T, StreamError>;

/// Comprehensive error types for the streaming system
#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum StreamError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Stream broken for connection {connection_id}: {reason}")]
    StreamBroken {
        connection_id: ConnectionId,
        reason: String,
    },

    #[error("Protocol error: {0}")]
    ProtocolError(String),

    #[error("Resource exhausted: {resource_type}")]
    ResourceExhausted { resource_type: String },

    #[error("Operation timed out after {duration:?}")]
    Timeout { duration: Duration },

    #[error("Connection {connection_id} not found")]
    ConnectionNotFound { connection_id: ConnectionId },

    #[error("Invalid connection state: expected {expected}, found {actual}")]
    InvalidConnectionState { expected: String, actual: String },

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Channel send error: {0}")]
    ChannelSend(String),

    #[error("Channel receive error: {0}")]
    ChannelReceive(String),

    #[error("TLS error: {0}")]
    Tls(#[from] rustls::Error),

    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("Authentication failed: {0}")]
    Authentication(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Service unavailable: {0}")]
    ServiceUnavailable(String),

    #[error("Rate limit exceeded: {0}")]
    RateLimitExceeded(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

#[allow(dead_code)]
impl StreamError {
    /// Create a connection failed error
    pub fn connection_failed<S: Into<String>>(reason: S) -> Self {
        Self::ConnectionFailed(reason.into())
    }

    /// Create a stream broken error
    pub fn stream_broken<S: Into<String>>(connection_id: ConnectionId, reason: S) -> Self {
        Self::StreamBroken {
            connection_id,
            reason: reason.into(),
        }
    }

    /// Create a protocol error
    pub fn protocol_error<S: Into<String>>(message: S) -> Self {
        Self::ProtocolError(message.into())
    }

    /// Create a resource exhausted error
    pub fn resource_exhausted<S: Into<String>>(resource_type: S) -> Self {
        Self::ResourceExhausted {
            resource_type: resource_type.into(),
        }
    }

    /// Create a timeout error
    pub fn timeout(duration: Duration) -> Self {
        Self::Timeout { duration }
    }

    /// Create a connection not found error
    pub fn connection_not_found(connection_id: ConnectionId) -> Self {
        Self::ConnectionNotFound { connection_id }
    }

    /// Create an invalid connection state error
    pub fn invalid_connection_state<S: Into<String>>(expected: S, actual: S) -> Self {
        Self::InvalidConnectionState {
            expected: expected.into(),
            actual: actual.into(),
        }
    }

    /// Create a channel send error
    pub fn channel_send<S: Into<String>>(message: S) -> Self {
        Self::ChannelSend(message.into())
    }

    /// Create a channel receive error
    pub fn channel_receive<S: Into<String>>(message: S) -> Self {
        Self::ChannelReceive(message.into())
    }

    /// Create a configuration error
    pub fn configuration<S: Into<String>>(message: S) -> Self {
        Self::Configuration(message.into())
    }

    /// Create an authentication error
    pub fn authentication<S: Into<String>>(message: S) -> Self {
        Self::Authentication(message.into())
    }

    /// Create a permission denied error
    pub fn permission_denied<S: Into<String>>(message: S) -> Self {
        Self::PermissionDenied(message.into())
    }

    /// Create a service unavailable error
    pub fn service_unavailable<S: Into<String>>(message: S) -> Self {
        Self::ServiceUnavailable(message.into())
    }

    /// Create a rate limit exceeded error
    pub fn rate_limit_exceeded<S: Into<String>>(message: S) -> Self {
        Self::RateLimitExceeded(message.into())
    }

    /// Create an internal error
    pub fn internal<S: Into<String>>(message: S) -> Self {
        Self::Internal(message.into())
    }

    /// Check if the error is recoverable
    pub fn is_recoverable(&self) -> bool {
        match self {
            Self::ConnectionFailed(_) => true,
            Self::StreamBroken { .. } => false,
            Self::ProtocolError(_) => false,
            Self::ResourceExhausted { .. } => true,
            Self::Timeout { .. } => true,
            Self::ConnectionNotFound { .. } => false,
            Self::InvalidConnectionState { .. } => false,
            Self::Serialization(_) => false,
            Self::Io(_) => true,
            Self::ChannelSend(_) => true,
            Self::ChannelReceive(_) => true,
            Self::Tls(_) => false,
            Self::Configuration(_) => false,
            Self::Authentication(_) => false,
            Self::PermissionDenied(_) => false,
            Self::ServiceUnavailable(_) => true,
            Self::RateLimitExceeded(_) => true,
            Self::Internal(_) => false,
        }
    }

    /// Get error category for metrics and logging
    pub fn category(&self) -> &'static str {
        match self {
            Self::ConnectionFailed(_) => "connection",
            Self::StreamBroken { .. } => "stream",
            Self::ProtocolError(_) => "protocol",
            Self::ResourceExhausted { .. } => "resource",
            Self::Timeout { .. } => "timeout",
            Self::ConnectionNotFound { .. } => "connection",
            Self::InvalidConnectionState { .. } => "state",
            Self::Serialization(_) => "serialization",
            Self::Io(_) => "io",
            Self::ChannelSend(_) => "channel",
            Self::ChannelReceive(_) => "channel",
            Self::Tls(_) => "tls",
            Self::Configuration(_) => "configuration",
            Self::Authentication(_) => "authentication",
            Self::PermissionDenied(_) => "permission",
            Self::ServiceUnavailable(_) => "service",
            Self::RateLimitExceeded(_) => "rate_limit",
            Self::Internal(_) => "internal",
        }
    }

    /// Get error severity level for logging and alerting
    pub fn severity(&self) -> ErrorSeverity {
        match self {
            Self::ConnectionFailed(_) => ErrorSeverity::Warning,
            Self::StreamBroken { .. } => ErrorSeverity::Error,
            Self::ProtocolError(_) => ErrorSeverity::Error,
            Self::ResourceExhausted { .. } => ErrorSeverity::Critical,
            Self::Timeout { .. } => ErrorSeverity::Warning,
            Self::ConnectionNotFound { .. } => ErrorSeverity::Warning,
            Self::InvalidConnectionState { .. } => ErrorSeverity::Error,
            Self::Serialization(_) => ErrorSeverity::Error,
            Self::Io(_) => ErrorSeverity::Warning,
            Self::ChannelSend(_) => ErrorSeverity::Warning,
            Self::ChannelReceive(_) => ErrorSeverity::Warning,
            Self::Tls(_) => ErrorSeverity::Error,
            Self::Configuration(_) => ErrorSeverity::Error,
            Self::Authentication(_) => ErrorSeverity::Error,
            Self::PermissionDenied(_) => ErrorSeverity::Error,
            Self::ServiceUnavailable(_) => ErrorSeverity::Warning,
            Self::RateLimitExceeded(_) => ErrorSeverity::Warning,
            Self::Internal(_) => ErrorSeverity::Critical,
        }
    }

    /// Get suggested retry strategy for recoverable errors
    pub fn retry_strategy(&self) -> Option<RetryStrategy> {
        if !self.is_recoverable() {
            return None;
        }

        match self {
            Self::ConnectionFailed(_) => Some(RetryStrategy::ExponentialBackoff {
                initial_delay: Duration::from_millis(100),
                max_delay: Duration::from_secs(30),
                max_attempts: 5,
            }),
            Self::ResourceExhausted { .. } => Some(RetryStrategy::LinearBackoff {
                delay: Duration::from_secs(1),
                max_attempts: 3,
            }),
            Self::Timeout { .. } => Some(RetryStrategy::ExponentialBackoff {
                initial_delay: Duration::from_millis(500),
                max_delay: Duration::from_secs(10),
                max_attempts: 3,
            }),
            Self::Io(_) => Some(RetryStrategy::ExponentialBackoff {
                initial_delay: Duration::from_millis(50),
                max_delay: Duration::from_secs(5),
                max_attempts: 3,
            }),
            Self::ChannelSend(_) | Self::ChannelReceive(_) => Some(RetryStrategy::LinearBackoff {
                delay: Duration::from_millis(100),
                max_attempts: 2,
            }),
            Self::ServiceUnavailable(_) => Some(RetryStrategy::ExponentialBackoff {
                initial_delay: Duration::from_secs(1),
                max_delay: Duration::from_secs(60),
                max_attempts: 10,
            }),
            Self::RateLimitExceeded(_) => Some(RetryStrategy::LinearBackoff {
                delay: Duration::from_secs(5),
                max_attempts: 5,
            }),
            _ => None,
        }
    }

    /// Add context information to the error
    pub fn with_context<S: Into<String>>(self, context: S) -> ErrorWithContext {
        ErrorWithContext {
            error: self,
            context: context.into(),
            timestamp: std::time::SystemTime::now(),
            additional_data: HashMap::new(),
        }
    }

    /// Add connection context to the error
    pub fn with_connection_context(self, connection_id: ConnectionId, operation: &str) -> ErrorWithContext {
        let mut context = ErrorWithContext {
            error: self,
            context: format!("Connection {} during {}", connection_id, operation),
            timestamp: std::time::SystemTime::now(),
            additional_data: HashMap::new(),
        };
        context.additional_data.insert("connection_id".to_string(), connection_id.to_string());
        context.additional_data.insert("operation".to_string(), operation.to_string());
        context
    }
}

/// Error severity levels for logging and alerting
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

/// Retry strategies for recoverable errors
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum RetryStrategy {
    /// Exponential backoff with jitter
    ExponentialBackoff {
        initial_delay: Duration,
        max_delay: Duration,
        max_attempts: u32,
    },
    /// Linear backoff with fixed delay
    LinearBackoff {
        delay: Duration,
        max_attempts: u32,
    },
    /// No retry
    None,
}

impl RetryStrategy {
    /// Calculate the delay for the given attempt number
    pub fn calculate_delay(&self, attempt: u32) -> Option<Duration> {
        match self {
            Self::ExponentialBackoff { initial_delay, max_delay, max_attempts } => {
                if attempt >= *max_attempts {
                    return None;
                }
                
                let delay = initial_delay.as_millis() as u64 * 2_u64.pow(attempt);
                let delay = Duration::from_millis(delay);
                
                // Add jitter (±25%)
                let jitter_range = (delay.as_millis() / 4) as u64;
                let jitter = fastrand::u64(0..=jitter_range * 2) as i64 - jitter_range as i64;
                let final_delay = Duration::from_millis((delay.as_millis() as i64 + jitter).max(0) as u64);
                
                Some(final_delay.min(*max_delay))
            }
            Self::LinearBackoff { delay, max_attempts } => {
                if attempt >= *max_attempts {
                    None
                } else {
                    Some(*delay)
                }
            }
            Self::None => None,
        }
    }

    /// Check if more attempts are allowed
    pub fn should_retry(&self, attempt: u32) -> bool {
        match self {
            Self::ExponentialBackoff { max_attempts, .. } => attempt < *max_attempts,
            Self::LinearBackoff { max_attempts, .. } => attempt < *max_attempts,
            Self::None => false,
        }
    }
}

/// Error with additional context information
#[derive(Debug)]
#[allow(dead_code)]
pub struct ErrorWithContext {
    pub error: StreamError,
    pub context: String,
    pub timestamp: std::time::SystemTime,
    pub additional_data: HashMap<String, String>,
}

#[allow(dead_code)]
impl ErrorWithContext {
    /// Add additional data to the error context
    pub fn with_data<K: Into<String>, V: Into<String>>(mut self, key: K, value: V) -> Self {
        self.additional_data.insert(key.into(), value.into());
        self
    }

    /// Get the underlying error
    pub fn error(&self) -> &StreamError {
        &self.error
    }

    /// Get the context string
    pub fn context(&self) -> &str {
        &self.context
    }

    /// Get additional data
    pub fn additional_data(&self) -> &HashMap<String, String> {
        &self.additional_data
    }

    /// Convert to a structured log entry
    pub fn to_log_entry(&self) -> serde_json::Value {
        serde_json::json!({
            "error": self.error.to_string(),
            "error_category": self.error.category(),
            "error_severity": self.error.severity(),
            "context": self.context,
            "timestamp": self.timestamp.duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_secs(),
            "recoverable": self.error.is_recoverable(),
            "additional_data": self.additional_data
        })
    }
}

impl std::fmt::Display for ErrorWithContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.context, self.error)
    }
}

impl std::error::Error for ErrorWithContext {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}