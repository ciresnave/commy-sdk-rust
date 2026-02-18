//! Error types for Commy client SDK

use thiserror::Error;

/// Result type for Commy client operations
pub type Result<T> = std::result::Result<T, CommyError>;

/// Errors that can occur when using the Commy client SDK
#[derive(Error, Debug)]
pub enum CommyError {
    /// WebSocket connection error
    #[error("WebSocket error: {0}")]
    WebSocketError(String),

    /// Connection lost or disconnected
    #[error("Connection lost: {0}")]
    ConnectionLost(String),

    /// Authentication failed
    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),

    /// Unauthorized - invalid credentials
    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    /// Resource not found
    #[error("Not found: {0}")]
    NotFound(String),

    /// Service not found
    #[error("Service not found: {0}")]
    ServiceNotFound(String),

    /// Tenant not found
    #[error("Tenant not found: {0}")]
    TenantNotFound(String),

    /// Resource already exists
    #[error("Already exists: {0}")]
    AlreadyExists(String),

    /// Permission denied
    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    /// Invalid request parameters
    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    /// Invalid message format
    #[error("Invalid message format: {0}")]
    InvalidMessage(String),

    /// Serialization error
    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    /// MessagePack error
    #[error("MessagePack error: {0}")]
    MessagePackError(#[from] rmp_serde::encode::Error),

    /// MessagePack decode error
    #[error("MessagePack decode error: {0}")]
    MessagePackDecodeError(#[from] rmp_serde::decode::Error),

    /// Operation timeout
    #[error("Operation timeout")]
    Timeout,

    /// Channel send error
    #[error("Channel error: {0}")]
    ChannelError(String),

    /// Invalid state
    #[error("Invalid state: {0}")]
    InvalidState(String),

    /// Memory mapping error
    #[error("Memory mapping error: {0}")]
    MemoryMappingError(String),

    /// File I/O error
    #[error("File I/O error: {0}")]
    FileError(#[from] std::io::Error),

    /// File watcher error
    #[error("File watcher error: {0}")]
    WatcherError(String),

    /// Variable not found in file
    #[error("Variable not found: {0}")]
    VariableNotFound(String),

    /// Invalid variable offset
    #[error("Invalid variable offset: {0}")]
    InvalidOffset(String),

    /// SIMD operation error
    #[error("SIMD operation error: {0}")]
    SimdError(String),

    /// Generic error
    #[error("{0}")]
    Other(String),
}

impl From<tokio_tungstenite::tungstenite::Error> for CommyError {
    fn from(err: tokio_tungstenite::tungstenite::Error) -> Self {
        CommyError::WebSocketError(err.to_string())
    }
}
