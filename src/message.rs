//! Message types for Commy client protocol

use serde::{Deserialize, Serialize};

/// Messages sent from client to server
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ClientMessage {
    /// Authenticate with a tenant (the commy-wire message: request id, protocol version, credentials)
    Authenticate(commy_wire::Authenticate),

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
        /// File size in bytes (the server rounds up to whole pages and caps it).
        size: u64,
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

    /// Heartbeat/keepalive
    Heartbeat { client_id: String },

    /// Disconnect gracefully
    Disconnect { client_id: String },

    /// Ask whether a service file may be mapped locally (commy-wire local mapping handshake)
    GetServiceFilePath(commy_wire::GetServiceFilePath),

    /// Remote sender: the bytes of one range frame (a WebSocket binary message, never JSON)
    #[serde(skip)]
    Range(commy_wire::RangeFrame),

    /// Ask the server to replace a service's file (resize or revocation)
    RotateRequest(commy_wire::RotateRequest),

    /// Give a client permissions, tenant-wide or on one service
    Grant(commy_wire::Grant),

    /// Take permissions away; with a service and no permissions, remove the client from it
    Revoke(commy_wire::Revoke),

    /// List grants, optionally of one client and/or one service
    ListGrants(commy_wire::ListGrants),

    /// Open or close a service to local memory mapping (needs `service:admin`)
    SetLocalMapping(commy_wire::SetLocalMapping),

    /// This client has flushed everything into the old file (resize drain barrier)
    RotationDrained(commy_wire::RotationDrained),

    /// Requester: the prepared file is filled; commit the rotation
    RotationFilled(commy_wire::RotationFilled),

    /// Requester: give up the rotation; the old file stays live
    RotationAbort(commy_wire::RotationAbort),

    /// Remote sender: the frames for one flush are complete
    FlushEnd(commy_wire::FlushEnd),

    /// Ask for the whole file as range frames followed by `ResyncDone`
    Resync(commy_wire::Resync),

    /// Local sender: these ranges of the shared mapping changed (no data travels)
    RangesChanged(commy_wire::RangesChanged),

    /// Echo the nonce read from the offered side file, proving the same filesystem view
    ConfirmLocal(commy_wire::ConfirmLocal),

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
    /// The answer to `Authenticate`; it names the request it answers.
    AuthenticationResult(commy_wire::AuthenticationResult),

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

    /// Operation result
    Result {
        request_id: String,
        success: bool,
        message: String,
    },

    /// Error response with explicit error type
    Error {
        code: ErrorCode,
        message: String,
        /// The request this answers, when the server names one (every commy-wire request does).
        /// A waiting call ignores an error that names another request.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },

    /// Server disconnecting
    Disconnected { reason: String },

    /// The file id for range frames, and (when offered) the local path and nonce path
    ServiceFilePath(commy_wire::ServiceFilePath),

    /// The rotation was accepted; it ends with a `RotationCommit`
    RotationAccepted(commy_wire::RotationAccepted),

    /// A `Grant` or `Revoke` took effect
    GrantAck(commy_wire::GrantAck),

    /// The answer to `ListGrants`
    Grants(commy_wire::Grants),

    /// A `SetLocalMapping` took effect
    LocalMappingAck(commy_wire::LocalMappingAck),

    /// Resize drain barrier is open: flush and answer `RotationDrained`
    RotationPending(commy_wire::RotationPending),

    /// Requester only: the new file exists, zero filled and not live; fill it
    RotationPrepared(commy_wire::RotationPrepared),

    /// The new file is live
    RotationCommit(commy_wire::RotationCommit),

    /// The rotation was abandoned; the old file stays live
    RotationAborted(commy_wire::RotationAborted),

    /// Writes of this client were lost
    Loss(commy_wire::Loss),

    /// Remote receiver: the bytes of one range frame (a WebSocket binary message)
    #[serde(skip)]
    Range(commy_wire::RangeFrame),

    /// The whole file has been sent as range frames; it is current at `seq`
    ResyncDone(commy_wire::ResyncDone),

    /// The server accepted a flush and assigned it this sequence number
    FlushAck(commy_wire::FlushAck),

    /// Local receiver: these ranges changed in the shared file at `seq` (no data)
    RangesUpdated(commy_wire::RangesUpdated),

    /// The server accepted the nonce; local mapping is confirmed
    ConfirmLocalAck(commy_wire::ConfirmLocalAck),

    /// Acknowledgment of variable changes
    VariableChangesAcknowledged {
        service_id: String,
        changed_variables: Vec<String>,
    },

    /// Heartbeat response (keep-alive)
    Heartbeat { timestamp: String },
}

/// Explicit error codes for API responses. Serialized `SCREAMING_SNAKE_CASE` (the commy-wire spelling);
/// the CamelCase spellings older server handlers still send are accepted when reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// Resource not found (e.g., service doesn't exist)
    #[serde(alias = "NotFound")]
    NotFound,
    /// Permission denied (unauthorized for this resource)
    #[serde(alias = "PermissionDenied")]
    PermissionDenied,
    /// Invalid authentication credentials
    #[serde(alias = "Unauthorized")]
    Unauthorized,
    /// Resource already exists
    #[serde(alias = "AlreadyExists")]
    AlreadyExists,
    /// Invalid request parameters
    #[serde(alias = "InvalidRequest")]
    InvalidRequest,
    /// Internal server error
    #[serde(alias = "InternalError")]
    InternalError,
    /// Connection lost or disconnected
    #[serde(alias = "ConnectionLost")]
    ConnectionLost,
    /// Operation timed out
    #[serde(alias = "Timeout")]
    Timeout,
    /// The request needs authentication first (commy-wire `UNAUTHENTICATED`)
    #[serde(alias = "Unauthenticated")]
    Unauthenticated,
    /// Too many requests (commy-wire `RATE_LIMITED`)
    #[serde(alias = "RateLimited")]
    RateLimited,
    /// The protocol version is not supported (commy-wire `UNSUPPORTED_VERSION`)
    #[serde(alias = "UnsupportedVersion")]
    UnsupportedVersion,
    /// A code this version does not know: the error still reaches the caller instead of the
    /// whole message being dropped.
    #[serde(other)]
    Unknown,
}

/// A value that must never appear in logs, panics or `{:?}` output.
///
/// `Debug` prints a fixed placeholder; the value is reachable only through [`Secret::expose`] or
/// [`Secret::into_inner`], so every use is visible in code review. Serialization is transparent
/// (the wire format is unchanged).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// Wrap a sensitive value.
    pub fn new(value: T) -> Self {
        Secret(value)
    }

    /// Borrow the sensitive value.
    pub fn expose(&self) -> &T {
        &self.0
    }

    /// Take the sensitive value out.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> std::fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}

impl<T> From<T> for Secret<T> {
    fn from(value: T) -> Self {
        Secret(value)
    }
}

impl From<&str> for Secret<String> {
    fn from(value: &str) -> Self {
        Secret(value.to_owned())
    }
}

/// Authentication credentials
///
/// Secret fields are wrapped in [`Secret`], so `{:?}` of credentials (or of a
/// [`ClientMessage::Authenticate`] holding them) never reveals them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method")]
pub enum AuthCredentials {
    /// API key authentication
    #[serde(rename = "api_key")]
    ApiKey { key: Secret<String> },

    /// JWT token authentication
    #[serde(rename = "jwt")]
    Jwt { token: Secret<String> },

    /// Username/password authentication
    #[serde(rename = "basic")]
    Basic {
        username: String,
        password: Secret<String>,
    },

    /// Custom authentication
    #[serde(rename = "custom")]
    Custom { data: Secret<serde_json::Value> },
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

impl From<&AuthCredentials> for commy_wire::Credentials {
    /// The wire form of the credentials. The secrets are exposed here, on their way to the server.
    fn from(c: &AuthCredentials) -> Self {
        match c {
            AuthCredentials::ApiKey { key } => commy_wire::Credentials::ApiKey {
                key: key.expose().clone(),
            },
            AuthCredentials::Jwt { token } => commy_wire::Credentials::Jwt {
                token: token.expose().clone(),
            },
            AuthCredentials::Basic { username, password } => commy_wire::Credentials::Basic {
                username: username.clone(),
                password: password.expose().clone(),
            },
            AuthCredentials::Custom { data } => commy_wire::Credentials::Custom {
                data: data.expose().clone(),
            },
        }
    }
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
            ErrorCode::Unauthenticated => {
                crate::error::CommyError::Unauthorized("Not authenticated".to_string())
            }
            ErrorCode::RateLimited => {
                crate::error::CommyError::Other("Rate limited by the server".to_string())
            }
            ErrorCode::UnsupportedVersion => {
                crate::error::CommyError::Other("Unsupported protocol version".to_string())
            }
            ErrorCode::Unknown => {
                crate::error::CommyError::Other("Unknown server error".to_string())
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
        let msg = ClientMessage::Authenticate(commy_wire::Authenticate {
            request_id: "r1".to_string(),
            protocol_version: commy_wire::PROTOCOL_VERSION,
            tenant_id: "tenant1".to_string(),
            client_version: Some("0.1.0".to_string()),
            client_id: Some("c1".to_string()),
            dev_scopes: None,
            credentials: (&AuthCredentials::ApiKey {
                key: "secret".into(),
            })
                .into(),
        });
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, ClientMessage::Authenticate(_)));
    }

    #[test]
    fn test_client_message_heartbeat_serialization() {
        let msg = ClientMessage::Heartbeat {
            client_id: "c1".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, ClientMessage::Heartbeat { .. }));
    }

    #[test]
    fn test_server_message_authentication_result_serialization() {
        let msg = ServerMessage::AuthenticationResult(commy_wire::AuthenticationResult {
            request_id: "r1".to_string(),
            success: true,
            message: "OK".to_string(),
            server_version: "0.1.0".to_string(),
            client_id: None,
            permissions: Some(vec!["read".to_string()]),
            expires_in_seconds: None,
        });
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            decoded,
            ServerMessage::AuthenticationResult(r) if r.success && r.request_id == "r1"
        ));
    }

    #[test]
    fn an_authentication_result_without_request_id_is_refused() {
        // commy-wire 0.4.0 requires the request id; servers older than that are no longer supported.
        let legacy = r#"{"type":"AuthenticationResult","data":{"success":true,"message":"ok","server_version":"0.13.0"}}"#;
        assert!(serde_json::from_str::<ServerMessage>(legacy).is_err());
    }

    #[test]
    fn test_server_message_error_serialization() {
        let msg = ServerMessage::Error {
            code: ErrorCode::NotFound,
            message: "Not found".to_string(),
            request_id: None,
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
        let jwt = AuthCredentials::Jwt {
            token: "tok".into(),
        };
        let json = serde_json::to_string(&jwt).unwrap();
        assert!(json.contains("jwt"));

        let basic = AuthCredentials::Basic {
            username: "user".to_string(),
            password: "pass".into(),
        };
        let json = serde_json::to_string(&basic).unwrap();
        assert!(json.contains("basic"));

        let custom = AuthCredentials::Custom {
            data: serde_json::json!({"key": "val"}).into(),
        };
        let json = serde_json::to_string(&custom).unwrap();
        assert!(json.contains("custom"));
    }

    /// Audit finding 1: `{:?}` of credentials (or of an Authenticate message holding them)
    /// must never reveal the secret.
    #[test]
    fn test_debug_output_never_contains_credential_secrets() {
        let cases = [
            (
                AuthCredentials::Basic {
                    username: "alice".into(),
                    password: "hunter2-SECRET".into(),
                },
                "hunter2-SECRET",
            ),
            (
                AuthCredentials::ApiKey {
                    key: "apikey-SECRET".into(),
                },
                "apikey-SECRET",
            ),
            (
                AuthCredentials::Jwt {
                    token: "jwt-SECRET".into(),
                },
                "jwt-SECRET",
            ),
            (
                AuthCredentials::Custom {
                    data: serde_json::json!({"pw": "custom-SECRET"}).into(),
                },
                "custom-SECRET",
            ),
        ];
        for (creds, secret) in cases {
            let msg = ClientMessage::Authenticate(commy_wire::Authenticate {
                request_id: "r".into(),
                protocol_version: commy_wire::PROTOCOL_VERSION,
                tenant_id: "t".into(),
                client_version: Some("v".into()),
                client_id: Some("c".into()),
                dev_scopes: None,
                credentials: (&creds).into(),
            });
            for shown in [
                format!("{:?}", creds),
                format!("{:#?}", creds),
                format!("{:?}", msg),
            ] {
                assert!(
                    !shown.contains(secret),
                    "Debug output leaked {secret}: {shown}"
                );
                assert!(
                    shown.to_lowercase().contains("redacted"),
                    "expected redaction marker in {shown}"
                );
            }
            // positive control: the wire format still carries the secret
            assert!(serde_json::to_string(&msg).unwrap().contains(secret));
        }
    }

    #[test]
    fn test_secret_expose_and_into_inner() {
        let s: Secret<String> = "abc".into();
        assert_eq!(s.expose(), "abc");
        assert_eq!(s.clone().into_inner(), "abc");
        assert_eq!(format!("{:?}", s), "Secret([REDACTED])");
        assert_eq!(serde_json::to_string(&s).unwrap(), "\"abc\"");
        assert_eq!(
            serde_json::from_str::<Secret<String>>("\"abc\"").unwrap(),
            s
        );
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
            ClientMessage::CreateTenant {
                tenant_id,
                tenant_name,
            } => {
                assert_eq!(tenant_id, "t1");
                assert_eq!(tenant_name, "Tenant One");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_client_message_delete_tenant_round_trip() {
        let msg = ClientMessage::DeleteTenant {
            tenant_id: "t2".to_string(),
        };
        match round_trip_client(msg) {
            ClientMessage::DeleteTenant { tenant_id } => assert_eq!(tenant_id, "t2"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_client_message_disconnect_round_trip() {
        let msg = ClientMessage::Disconnect {
            client_id: "cli_99".to_string(),
        };
        match round_trip_client(msg) {
            ClientMessage::Disconnect { client_id } => assert_eq!(client_id, "cli_99"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_client_message_get_service_file_path_matches_commy_wire() {
        let wire = commy_wire::GetServiceFilePath {
            request_id: "r1".to_string(),
            service_id: "svc".to_string(),
        };
        let sdk = serde_json::to_value(ClientMessage::GetServiceFilePath(wire.clone())).unwrap();
        let expect =
            serde_json::to_value(commy_wire::ControlMessage::GetServiceFilePath(wire)).unwrap();
        assert_eq!(sdk, expect);
    }

    #[test]
    fn test_client_message_confirm_local_matches_commy_wire_and_hides_nonce() {
        let wire = commy_wire::ConfirmLocal {
            request_id: "r2".to_string(),
            file_id: 7,
            nonce: "s3cret-nonce".to_string(),
        };
        let msg = ClientMessage::ConfirmLocal(wire.clone());
        let sdk = serde_json::to_value(&msg).unwrap();
        let expect = serde_json::to_value(commy_wire::ControlMessage::ConfirmLocal(wire)).unwrap();
        assert_eq!(sdk, expect);
        assert!(!format!("{msg:?}").contains("s3cret-nonce"));
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
            ServerMessage::Tenant {
                tenant_id,
                tenant_name,
            } => {
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
            ServerMessage::TenantResult {
                success,
                tenant_id,
                message,
            } => {
                assert!(success);
                assert_eq!(tenant_id, "t2");
                assert_eq!(message, "Tenant created");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_server_message_disconnected_round_trip() {
        let msg = ServerMessage::Disconnected {
            reason: "server shutdown".to_string(),
        };
        match round_trip_server(msg) {
            ServerMessage::Disconnected { reason } => {
                assert_eq!(reason, "server shutdown");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_server_message_service_file_path_parses_commy_wire_offers() {
        let offer = r#"{"type":"ServiceFilePath","data":{"request_id":"r1","service_id":"svc7","file_id":3,"path":"/v/s.mem","nonce_path":"/v/n"}}"#;
        match serde_json::from_str::<ServerMessage>(offer).unwrap() {
            ServerMessage::ServiceFilePath(o) => {
                assert_eq!(o.file_id, 3);
                assert_eq!(o.path.as_deref(), Some("/v/s.mem"));
                assert_eq!(o.nonce_path.as_deref(), Some("/v/n"));
            }
            other => panic!("Wrong variant: {other:?}"),
        }
        let none = r#"{"type":"ServiceFilePath","data":{"request_id":"r1","service_id":"svc7","file_id":3}}"#;
        match serde_json::from_str::<ServerMessage>(none).unwrap() {
            ServerMessage::ServiceFilePath(o) => {
                assert!(o.path.is_none() && o.nonce_path.is_none())
            }
            other => panic!("Wrong variant: {other:?}"),
        }
    }

    #[test]
    fn test_server_message_variable_changes_acknowledged_round_trip() {
        let msg = ServerMessage::VariableChangesAcknowledged {
            service_id: "svc8".to_string(),
            changed_variables: vec!["a".to_string(), "b".to_string()],
        };
        match round_trip_server(msg) {
            ServerMessage::VariableChangesAcknowledged {
                service_id,
                changed_variables,
            } => {
                assert_eq!(service_id, "svc8");
                assert_eq!(changed_variables, vec!["a".to_string(), "b".to_string()]);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_server_message_heartbeat_round_trip() {
        let msg = ServerMessage::Heartbeat {
            timestamp: "2025-01-01T00:00:00Z".to_string(),
        };
        match round_trip_server(msg) {
            ServerMessage::Heartbeat { timestamp } => {
                assert_eq!(timestamp, "2025-01-01T00:00:00Z");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_server_message_result_round_trip() {
        let msg = ServerMessage::Result {
            request_id: "req-42".to_string(),
            success: true,
            message: "Operation completed".to_string(),
        };
        match round_trip_server(msg) {
            ServerMessage::Result {
                request_id,
                success,
                message,
            } => {
                assert_eq!(request_id, "req-42");
                assert!(success);
                assert_eq!(message, "Operation completed");
            }
            _ => panic!("Wrong variant after round-trip"),
        }
    }

    #[test]
    fn test_service_metadata_round_trip_with_and_without_file_path() {
        // With file_path = Some
        let with_path = ServiceMetadata {
            service_id: "svc-1".to_string(),
            service_name: "my_service".to_string(),
            tenant_id: "tenant_a".to_string(),
            created_at: chrono::DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            file_path: Some("/tmp/svc.mem".to_string()),
        };
        let json = serde_json::to_string(&with_path).unwrap();
        let back: ServiceMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(back.service_id, "svc-1");
        assert_eq!(back.file_path, Some("/tmp/svc.mem".to_string()));

        // With file_path = None
        let no_path = ServiceMetadata {
            service_id: "svc-2".to_string(),
            service_name: "remote_service".to_string(),
            tenant_id: "tenant_b".to_string(),
            created_at: chrono::Utc::now(),
            file_path: None,
        };
        let json = serde_json::to_string(&no_path).unwrap();
        let back: ServiceMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(back.service_id, "svc-2");
        assert!(back.file_path.is_none());
    }

    #[test]
    fn test_permission_enum_all_variants_round_trip() {
        let variants = [
            Permission::Read,
            Permission::Write,
            Permission::Admin,
            Permission::Execute,
        ];
        for perm in &variants {
            let json = serde_json::to_string(perm).unwrap();
            let back: Permission = serde_json::from_str(&json).unwrap();
            assert_eq!(&back, perm, "Permission round-trip failed for {:?}", perm);
        }
    }

    #[test]
    fn test_unknown_server_message_type_tag_fails_to_deserialize() {
        let bad_json = r#"{"type":"SomeFutureMessageType","data":{"key":"value"}}"#;
        let result: Result<ServerMessage, _> = serde_json::from_str(bad_json);
        assert!(
            result.is_err(),
            "Deserializing unknown 'type' tag must fail, not silently produce a wrong variant"
        );
    }

    #[test]
    fn every_commy_wire_error_code_deserializes_and_unknown_ones_do_not_drop_the_message() {
        for code in commy_wire::ErrorCode::ALL {
            let json = format!(
                r#"{{"type":"Error","data":{{"code":"{}","message":"m","request_id":"r1"}}}}"#,
                code.as_str()
            );
            let m: ServerMessage = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("{} must deserialize: {e}", code.as_str()));
            assert!(
                matches!(&m, ServerMessage::Error { request_id: Some(r), .. } if r == "r1"),
                "{m:?}"
            );
        }
        let newer = r#"{"type":"Error","data":{"code":"SOMETHING_NEW","message":"m"}}"#;
        assert!(matches!(
            serde_json::from_str::<ServerMessage>(newer).unwrap(),
            ServerMessage::Error {
                code: ErrorCode::Unknown,
                request_id: None,
                ..
            }
        ));
    }

    #[test]
    fn legacy_camel_case_codes_from_older_server_handlers_are_understood() {
        for (spelling, want) in [
            ("NotFound", ErrorCode::NotFound),
            ("PermissionDenied", ErrorCode::PermissionDenied),
            ("Unauthorized", ErrorCode::Unauthorized),
            ("AlreadyExists", ErrorCode::AlreadyExists),
            ("InvalidRequest", ErrorCode::InvalidRequest),
            ("InternalError", ErrorCode::InternalError),
            ("Unauthenticated", ErrorCode::Unauthenticated),
            ("RateLimited", ErrorCode::RateLimited),
        ] {
            let json =
                format!(r#"{{"type":"Error","data":{{"code":"{spelling}","message":"m"}}}}"#);
            let m: ServerMessage = serde_json::from_str(&json).unwrap();
            assert!(
                matches!(&m, ServerMessage::Error { code, .. } if *code == want),
                "{spelling} -> {m:?}"
            );
        }
    }
}
