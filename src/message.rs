//! Message types for Commy client protocol

use serde::{Deserialize, Serialize};

/// Messages sent from client to server
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ClientMessage {
    /// Authenticate with a tenant
    Authenticate {
        tenant_id: String,
        client_version: String,
        #[serde(flatten)]
        credentials: AuthCredentials,
    },

    /// Create a new tenant (admin operation)
    CreateTenant {
        tenant_id: String,
        tenant_name: String,
    },

    /// Delete a tenant (admin operation)
    DeleteTenant { tenant_id: String },

    /// Create a new service
    CreateService {
        tenant_id: String,
        service_name: String,
    },

    /// Get existing service (errors if not found)
    GetService {
        tenant_id: String,
        service_name: String,
    },

    /// Delete a service
    DeleteService {
        tenant_id: String,
        service_name: String,
    },

    /// Allocate a variable in a service
    AllocateVariable {
        service_id: String,
        variable_name: String,
        initial_data: Vec<u8>,
    },

    /// Read variable data
    ReadVariable {
        service_id: String,
        variable_name: String,
    },

    /// Write variable data
    WriteVariable {
        service_id: String,
        variable_name: String,
        data: Vec<u8>,
    },

    /// Deallocate a variable
    DeallocateVariable {
        service_id: String,
        variable_name: String,
    },

    /// Subscribe to variable changes
    Subscribe {
        service_id: String,
        variable_name: String,
    },

    /// Unsubscribe from variable changes
    Unsubscribe {
        service_id: String,
        variable_name: String,
    },

    /// Heartbeat/keepalive
    Heartbeat { client_id: String },

    /// Disconnect gracefully
    Disconnect { client_id: String },

    /// Request service file path for memory mapping (local clients)
    GetServiceFilePath {
        tenant_id: String,
        service_name: String,
    },

    /// Notify server of variable changes detected locally
    ReportVariableChanges {
        service_id: String,
        changed_variables: Vec<String>,
        new_values: Vec<(String, Vec<u8>)>,
    },
}

/// Messages received from server
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ServerMessage {
    /// Authentication result
    AuthenticationResult {
        success: bool,
        message: String,
        server_version: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        permissions: Option<Vec<String>>,
    },

    /// Service created or retrieved
    Service {
        service_id: String,
        service_name: String,
        tenant_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        file_path: Option<String>,
    },

    /// Tenant created or retrieved
    Tenant {
        tenant_id: String,
        tenant_name: String,
    },

    /// Tenant operation result (create/delete)
    TenantResult {
        success: bool,
        tenant_id: String,
        message: String,
    },

    /// Variable data response
    VariableData {
        service_id: String,
        variable_name: String,
        data: Vec<u8>,
        version: u64,
    },

    /// Variable change notification
    VariableChanged {
        service_id: String,
        variable_name: String,
        data: Vec<u8>,
        version: u64,
    },

    /// Operation result
    Result {
        request_id: String,
        success: bool,
        message: String,
    },

    /// Error response with explicit error type
    Error { code: ErrorCode, message: String },

    /// Server disconnecting
    Disconnected { reason: String },

    /// Service file path response (for local memory mapping)
    ServiceFilePath {
        service_id: String,
        file_path: String,
        file_size: u64,
    },

    /// Acknowledgment of variable changes
    VariableChangesAcknowledged {
        service_id: String,
        changed_variables: Vec<String>,
    },

    /// Heartbeat response (keep-alive)
    Heartbeat { timestamp: String },
}

/// Explicit error codes for API responses
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// Resource not found (e.g., service doesn't exist)
    NotFound,
    /// Permission denied (unauthorized for this resource)
    PermissionDenied,
    /// Invalid authentication credentials
    Unauthorized,
    /// Resource already exists
    AlreadyExists,
    /// Invalid request parameters
    InvalidRequest,
    /// Internal server error
    InternalError,
    /// Connection lost or disconnected
    ConnectionLost,
    /// Operation timed out
    Timeout,
}

/// Authentication credentials
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method")]
pub enum AuthCredentials {
    /// API key authentication
    #[serde(rename = "api_key")]
    ApiKey { key: String },

    /// JWT token authentication
    #[serde(rename = "jwt")]
    Jwt { token: String },

    /// Username/password authentication
    #[serde(rename = "basic")]
    Basic { username: String, password: String },

    /// Custom authentication
    #[serde(rename = "custom")]
    Custom { data: serde_json::Value },
}

/// Service metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceMetadata {
    pub service_id: String,
    pub service_name: String,
    pub tenant_id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub file_path: Option<String>,
}

/// Variable metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableMetadata {
    pub name: String,
    pub service_id: String,
    pub offset: u64,
    pub size: u64,
    pub version: u64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Permission set
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Permission {
    Read,
    Write,
    Admin,
    Execute,
}

/// Convert ErrorCode to CommyError
impl From<ErrorCode> for crate::error::CommyError {
    fn from(code: ErrorCode) -> Self {
        match code {
            ErrorCode::NotFound => {
                crate::error::CommyError::NotFound("Resource not found".to_string())
            }
            ErrorCode::PermissionDenied => {
                crate::error::CommyError::PermissionDenied("Permission denied".to_string())
            }
            ErrorCode::Unauthorized => {
                crate::error::CommyError::Unauthorized("Unauthorized".to_string())
            }
            ErrorCode::AlreadyExists => {
                crate::error::CommyError::AlreadyExists("Resource already exists".to_string())
            }
            ErrorCode::InvalidRequest => {
                crate::error::CommyError::InvalidRequest("Invalid request".to_string())
            }
            ErrorCode::InternalError => {
                crate::error::CommyError::Other("Internal server error".to_string())
            }
            ErrorCode::ConnectionLost => {
                crate::error::CommyError::ConnectionLost("Connection lost".to_string())
            }
            ErrorCode::Timeout => crate::error::CommyError::Timeout,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CommyError;

    #[test]
    fn test_error_code_all_variants_convert_correctly() {
        // NotFound
        let e: CommyError = ErrorCode::NotFound.into();
        assert!(matches!(e, CommyError::NotFound(_)));

        // PermissionDenied
        let e: CommyError = ErrorCode::PermissionDenied.into();
        assert!(matches!(e, CommyError::PermissionDenied(_)));

        // Unauthorized
        let e: CommyError = ErrorCode::Unauthorized.into();
        assert!(matches!(e, CommyError::Unauthorized(_)));

        // AlreadyExists
        let e: CommyError = ErrorCode::AlreadyExists.into();
        assert!(matches!(e, CommyError::AlreadyExists(_)));

        // InvalidRequest
        let e: CommyError = ErrorCode::InvalidRequest.into();
        assert!(matches!(e, CommyError::InvalidRequest(_)));

        // InternalError -> Other
        let e: CommyError = ErrorCode::InternalError.into();
        assert!(matches!(e, CommyError::Other(_)));

        // ConnectionLost
        let e: CommyError = ErrorCode::ConnectionLost.into();
        assert!(matches!(e, CommyError::ConnectionLost(_)));

        // Timeout
        let e: CommyError = ErrorCode::Timeout.into();
        assert!(matches!(e, CommyError::Timeout));
    }

    #[test]
    fn test_error_code_serialization_round_trip() {
        let codes = [
            ErrorCode::NotFound,
            ErrorCode::PermissionDenied,
            ErrorCode::Unauthorized,
            ErrorCode::AlreadyExists,
            ErrorCode::InvalidRequest,
            ErrorCode::InternalError,
            ErrorCode::ConnectionLost,
            ErrorCode::Timeout,
        ];
        for code in codes {
            let json = serde_json::to_string(&code).unwrap();
            let decoded: ErrorCode = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, code);
        }
    }

    #[test]
    fn test_client_message_authenticate_serialization() {
        let msg = ClientMessage::Authenticate {
            tenant_id: "tenant1".to_string(),
            client_version: "0.1.0".to_string(),
            credentials: AuthCredentials::ApiKey { key: "secret".to_string() },
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, ClientMessage::Authenticate { .. }));
    }

    #[test]
    fn test_client_message_heartbeat_serialization() {
        let msg = ClientMessage::Heartbeat { client_id: "c1".to_string() };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, ClientMessage::Heartbeat { .. }));
    }

    #[test]
    fn test_server_message_authentication_result_serialization() {
        let msg = ServerMessage::AuthenticationResult {
            success: true,
            message: "OK".to_string(),
            server_version: "0.1.0".to_string(),
            permissions: Some(vec!["read".to_string()]),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, ServerMessage::AuthenticationResult { success: true, .. }));
    }

    #[test]
    fn test_server_message_error_serialization() {
        let msg = ServerMessage::Error {
            code: ErrorCode::NotFound,
            message: "Not found".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ServerMessage::Error { code, .. } => assert_eq!(code, ErrorCode::NotFound),
            _ => panic!("Expected ServerMessage::Error"),
        }
    }

    #[test]
    fn test_auth_credentials_variants_serialization() {
        let jwt = AuthCredentials::Jwt { token: "tok".to_string() };
        let json = serde_json::to_string(&jwt).unwrap();
        assert!(json.contains("jwt"));

        let basic = AuthCredentials::Basic {
            username: "user".to_string(),
            password: "pass".to_string(),
        };
        let json = serde_json::to_string(&basic).unwrap();
        assert!(json.contains("basic"));

        let custom = AuthCredentials::Custom {
            data: serde_json::json!({"key": "val"}),
        };
        let json = serde_json::to_string(&custom).unwrap();
        assert!(json.contains("custom"));
    }

    // ─────────────────────────────────────────────────────────────
    // ClientMessage round-trip tests (untested variants)
    // ─────────────────────────────────────────────────────────────

    fn round_trip_client(msg: ClientMessage) -> ClientMessage {
        let json = serde_json::to_string(&msg).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    fn round_trip_server(msg: ServerMessage) -> ServerMessage {
        let json = serde_json::to_string(&msg).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn test_client_message_create_tenant_round_trip() {
        let msg = ClientMessage::CreateTenant {
            tenant_id: "t1".to_string(),
            tenant_name: "Tenant One".to_string(),
        };
        match round_trip_client(msg) {
            ClientMessage::CreateTenant { tenant_id, tenant_name } => {
                assert_eq!(tenant_id, "t1");
                assert_eq!(tenant_name, "Tenant One");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_client_message_delete_tenant_round_trip() {
        let msg = ClientMessage::DeleteTenant { tenant_id: "t2".to_string() };
        match round_trip_client(msg) {
            ClientMessage::DeleteTenant { tenant_id } => assert_eq!(tenant_id, "t2"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_client_message_allocate_variable_round_trip() {
        let initial = vec![1u8, 2, 3, 4];
        let msg = ClientMessage::AllocateVariable {
            service_id: "svc1".to_string(),
            variable_name: "counter".to_string(),
            initial_data: initial.clone(),
        };
        match round_trip_client(msg) {
            ClientMessage::AllocateVariable { service_id, variable_name, initial_data } => {
                assert_eq!(service_id, "svc1");
                assert_eq!(variable_name, "counter");
                assert_eq!(initial_data, initial);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_client_message_read_variable_round_trip() {
        let msg = ClientMessage::ReadVariable {
            service_id: "svc1".to_string(),
            variable_name: "counter".to_string(),
        };
        match round_trip_client(msg) {
            ClientMessage::ReadVariable { service_id, variable_name } => {
                assert_eq!(service_id, "svc1");
                assert_eq!(variable_name, "counter");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_client_message_write_variable_round_trip() {
        let data = vec![9u8, 8, 7];
        let msg = ClientMessage::WriteVariable {
            service_id: "svc2".to_string(),
            variable_name: "temp".to_string(),
            data: data.clone(),
        };
        match round_trip_client(msg) {
            ClientMessage::WriteVariable { service_id, variable_name, data: d } => {
                assert_eq!(service_id, "svc2");
                assert_eq!(variable_name, "temp");
                assert_eq!(d, data);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_client_message_deallocate_variable_round_trip() {
        let msg = ClientMessage::DeallocateVariable {
            service_id: "svc3".to_string(),
            variable_name: "old_var".to_string(),
        };
        match round_trip_client(msg) {
            ClientMessage::DeallocateVariable { service_id, variable_name } => {
                assert_eq!(service_id, "svc3");
                assert_eq!(variable_name, "old_var");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_client_message_subscribe_round_trip() {
        let msg = ClientMessage::Subscribe {
            service_id: "svc4".to_string(),
            variable_name: "level".to_string(),
        };
        match round_trip_client(msg) {
            ClientMessage::Subscribe { service_id, variable_name } => {
                assert_eq!(service_id, "svc4");
                assert_eq!(variable_name, "level");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_client_message_unsubscribe_round_trip() {
        let msg = ClientMessage::Unsubscribe {
            service_id: "svc5".to_string(),
            variable_name: "level".to_string(),
        };
        match round_trip_client(msg) {
            ClientMessage::Unsubscribe { service_id, variable_name } => {
                assert_eq!(service_id, "svc5");
                assert_eq!(variable_name, "level");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_client_message_disconnect_round_trip() {
        let msg = ClientMessage::Disconnect { client_id: "cli_99".to_string() };
        match round_trip_client(msg) {
            ClientMessage::Disconnect { client_id } => assert_eq!(client_id, "cli_99"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_client_message_get_service_file_path_round_trip() {
        let msg = ClientMessage::GetServiceFilePath {
            tenant_id: "t1".to_string(),
            service_name: "cache".to_string(),
        };
        match round_trip_client(msg) {
            ClientMessage::GetServiceFilePath { tenant_id, service_name } => {
                assert_eq!(tenant_id, "t1");
                assert_eq!(service_name, "cache");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_client_message_report_variable_changes_round_trip() {
        let new_values = vec![("v1".to_string(), vec![1u8, 2, 3])];
        let msg = ClientMessage::ReportVariableChanges {
            service_id: "svc6".to_string(),
            changed_variables: vec!["v1".to_string()],
            new_values: new_values.clone(),
        };
        match round_trip_client(msg) {
            ClientMessage::ReportVariableChanges {
                service_id,
                changed_variables,
                new_values: nv,
            } => {
                assert_eq!(service_id, "svc6");
                assert_eq!(changed_variables, vec!["v1".to_string()]);
                assert_eq!(nv, new_values);
            }
            _ => panic!("Wrong variant"),
        }
    }

    // ─────────────────────────────────────────────────────────────
    // ServerMessage round-trip tests (untested variants)
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn test_server_message_tenant_round_trip() {
        let msg = ServerMessage::Tenant {
            tenant_id: "t1".to_string(),
            tenant_name: "Alpha Corp".to_string(),
        };
        match round_trip_server(msg) {
            ServerMessage::Tenant { tenant_id, tenant_name } => {
                assert_eq!(tenant_id, "t1");
                assert_eq!(tenant_name, "Alpha Corp");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_server_message_tenant_result_round_trip() {
        let msg = ServerMessage::TenantResult {
            success: true,
            tenant_id: "t2".to_string(),
            message: "Tenant created".to_string(),
        };
        match round_trip_server(msg) {
            ServerMessage::TenantResult { success, tenant_id, message } => {
                assert!(success);
                assert_eq!(tenant_id, "t2");
                assert_eq!(message, "Tenant created");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_server_message_variable_data_round_trip() {
        let data = vec![5u8, 6, 7, 8];
        let msg = ServerMessage::VariableData {
            service_id: "svc1".to_string(),
            variable_name: "x".to_string(),
            data: data.clone(),
            version: 42,
        };
        match round_trip_server(msg) {
            ServerMessage::VariableData { service_id, variable_name, data: d, version } => {
                assert_eq!(service_id, "svc1");
                assert_eq!(variable_name, "x");
                assert_eq!(d, data);
                assert_eq!(version, 42);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_server_message_variable_changed_round_trip() {
        let data = vec![11u8, 22];
        let msg = ServerMessage::VariableChanged {
            service_id: "svc2".to_string(),
            variable_name: "y".to_string(),
            data: data.clone(),
            version: 7,
        };
        match round_trip_server(msg) {
            ServerMessage::VariableChanged { service_id, variable_name, data: d, version } => {
                assert_eq!(service_id, "svc2");
                assert_eq!(variable_name, "y");
                assert_eq!(d, data);
                assert_eq!(version, 7);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_server_message_disconnected_round_trip() {
        let msg = ServerMessage::Disconnected { reason: "server shutdown".to_string() };
        match round_trip_server(msg) {
            ServerMessage::Disconnected { reason } => {
                assert_eq!(reason, "server shutdown");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_server_message_service_file_path_round_trip() {
        let msg = ServerMessage::ServiceFilePath {
            service_id: "svc7".to_string(),
            file_path: "/var/commy/service.mem".to_string(),
            file_size: 1_048_576,
        };
        match round_trip_server(msg) {
            ServerMessage::ServiceFilePath { service_id, file_path, file_size } => {
                assert_eq!(service_id, "svc7");
                assert_eq!(file_path, "/var/commy/service.mem");
                assert_eq!(file_size, 1_048_576);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_server_message_variable_changes_acknowledged_round_trip() {
        let msg = ServerMessage::VariableChangesAcknowledged {
            service_id: "svc8".to_string(),
            changed_variables: vec!["a".to_string(), "b".to_string()],
        };
        match round_trip_server(msg) {
            ServerMessage::VariableChangesAcknowledged { service_id, changed_variables } => {
                assert_eq!(service_id, "svc8");
                assert_eq!(changed_variables, vec!["a".to_string(), "b".to_string()]);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_server_message_heartbeat_round_trip() {
        let msg = ServerMessage::Heartbeat { timestamp: "2025-01-01T00:00:00Z".to_string() };
        match round_trip_server(msg) {
            ServerMessage::Heartbeat { timestamp } => {
                assert_eq!(timestamp, "2025-01-01T00:00:00Z");
            }
            _ => panic!("Wrong variant"),
        }
    }
}
