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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display_messages() {
        let cases: &[(&str, CommyError)] = &[
            ("WebSocket error", CommyError::WebSocketError("refused".to_string())),
            ("Connection lost", CommyError::ConnectionLost("drop".to_string())),
            ("Authentication failed", CommyError::AuthenticationFailed("bad token".to_string())),
            ("Unauthorized", CommyError::Unauthorized("no perms".to_string())),
            ("Not found", CommyError::NotFound("item".to_string())),
            ("Service not found", CommyError::ServiceNotFound("svc".to_string())),
            ("Tenant not found", CommyError::TenantNotFound("t1".to_string())),
            ("Already exists", CommyError::AlreadyExists("x".to_string())),
            ("Permission denied", CommyError::PermissionDenied("read".to_string())),
            ("Invalid request", CommyError::InvalidRequest("missing field".to_string())),
            ("Invalid message format", CommyError::InvalidMessage("bad fmt".to_string())),
            ("timeout", CommyError::Timeout),
            ("Channel error", CommyError::ChannelError("closed".to_string())),
            ("Invalid state", CommyError::InvalidState("disconnected".to_string())),
            ("Memory mapping error", CommyError::MemoryMappingError("mmap fail".to_string())),
            ("File watcher error", CommyError::WatcherError("watch fail".to_string())),
            ("Variable not found", CommyError::VariableNotFound("var_x".to_string())),
            ("Invalid variable offset", CommyError::InvalidOffset("bad offset".to_string())),
            ("SIMD operation error", CommyError::SimdError("simd fail".to_string())),
            ("just misc", CommyError::Other("just misc".to_string())),
        ];

        for (fragment, err) in cases {
            let s = err.to_string();
            assert!(
                s.to_lowercase().contains(&fragment.to_lowercase()),
                "Display for {:?} should contain '{}', got: '{}'",
                err,
                fragment,
                s
            );
        }
    }

    #[test]
    fn test_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let commy_err = CommyError::from(io_err);
        assert!(matches!(commy_err, CommyError::FileError(_)));
    }

    #[test]
    fn test_from_serde_json_error() {
        let json_err = serde_json::from_str::<i32>("not_a_number").unwrap_err();
        let commy_err = CommyError::from(json_err);
        assert!(matches!(commy_err, CommyError::SerializationError(_)));
    }

    #[test]
    fn test_from_tungstenite_error() {
        use tokio_tungstenite::tungstenite::Error as WsError;
        // ConnectionClosed is a unit variant available in all tungstenite versions
        let ws_err = WsError::ConnectionClosed;
        let commy_err = CommyError::from(ws_err);
        assert!(matches!(commy_err, CommyError::WebSocketError(_)));
        assert!(commy_err.to_string().contains("WebSocket error"));
    }
}
