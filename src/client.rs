//! Main Commy client for connecting to servers

use crate::auth::{AuthContext, AuthCredentials};
use crate::connection::{Connection, ConnectionState};
use crate::error::{CommyError, Result};
use crate::local_map::{LocalMap, LocalMapOutcome, NegotiatedFile};
use crate::message::{ClientMessage, ServerMessage};
use crate::service::Service;
use crate::session::Session;
use crate::shared_file::SharedFile;
use crate::state::{create_shared_state, SharedState};
use crate::virtual_file::VirtualVariableFile;
use crate::watcher::VariableFileWatcher;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Something that happened to a mapped service, as seen by [`Session::poll_shared_events`].
#[derive(Debug)]
pub enum SharedEvent {
    /// Nothing relevant arrived in time.
    None,
    /// Another writer's changes were applied (ranges re-baselined or frames written).
    Updated,
    /// A resize drain barrier opened: call [`Session::drain_for_rotation`].
    RotationPending(commy_wire::RotationPending),
    /// Requester only: the new file is ready to fill (seen outside [`Session::drain_for_rotation`]).
    RotationPrepared(commy_wire::RotationPrepared),
    /// The new file is live: call [`Session::complete_rotation`].
    RotationCommit(commy_wire::RotationCommit),
    /// The rotation was abandoned: the old file stays live and writers may resume on it.
    RotationAborted(commy_wire::RotationAborted),
    /// Writes were lost. For `ROTATION_STRAGGLER`, `REVOKED` and unknown kinds the mapping has
    /// been dropped; `RESYNC_OVERWROTE` keeps it.
    Loss(commy_wire::Loss),
}

/// How a resize rotation ended for this client (or, for the requester, where it stands).
#[derive(Debug)]
pub enum RotationOutcome {
    /// The new file is live and mapped.
    Committed(commy_wire::RotationCommit),
    /// The rotation was abandoned; the old file is still mapped and writable.
    Aborted(commy_wire::RotationAborted),
    /// Requester only: fill the new file ([`Session::begin_fill`]) and finish or abort.
    Prepared(commy_wire::RotationPrepared),
}

/// The prepared file of a resize, being filled by the requester.
pub struct RotationFill {
    prepared: commy_wire::RotationPrepared,
    fill_ms: u64,
    target: FillTarget,
}

enum FillTarget {
    /// The prepared file mapped on this machine.
    Local(memmap2::MmapMut),
    /// A private buffer sent as range frames at the end.
    Remote(Vec<u8>),
}

impl RotationFill {
    /// Size of the new file in bytes.
    pub fn size(&self) -> u64 {
        self.prepared.size
    }

    /// Write `bytes` at `offset` in the new file (zero filled until written).
    pub fn write(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        let out_of_bounds =
            || CommyError::InvalidOffset(format!("write at {offset} is outside the new file"));
        let end = offset
            .checked_add(bytes.len() as u64)
            .filter(|e| *e <= self.prepared.size)
            .ok_or_else(out_of_bounds)?;
        let buf: &mut [u8] = match &mut self.target {
            FillTarget::Local(m) => m,
            FillTarget::Remote(v) => v,
        };
        buf[offset as usize..end as usize].copy_from_slice(bytes);
        Ok(())
    }
}

/// Extra time after `deadline_ms` before a missing `RotationCommit` counts as a failed rotation.
const ROTATION_COMMIT_GRACE: Duration = Duration::from_secs(5);

/// Largest remote mirror accepted from a `Resync` (guards against a hostile or confused size).
const MAX_REMOTE_FILE_BYTES: usize = 1 << 30;

/// Main Commy client for interacting with a Commy server
#[derive(Clone)]
pub struct Client {
    /// Unique client identifier
    client_id: String,

    /// Server URL (WebSocket)
    server_url: String,

    /// Active WebSocket connection
    connection: Arc<RwLock<Option<Connection>>>,

    /// Shared client state
    state: SharedState,

    /// Heartbeat interval
    // The background heartbeat task is disabled (see the TODO in `_connect_impl`).
    #[allow(
        dead_code,
        reason = "read only by the disabled background heartbeat task"
    )]
    heartbeat_interval: Duration,

    /// Maximum reconnection attempts
    max_reconnect_attempts: u32,

    /// PEM certificates to trust instead of the system roots (self-signed development servers)
    tls_root_pem: Option<Vec<u8>>,

    /// Development servers: authenticate with exactly these scopes instead of administrator
    dev_scopes: Option<Vec<String>>,

    /// Current reconnection attempt
    reconnect_attempts: Arc<AtomicU64>,

    /// Virtual variable files by service ID
    virtual_files: Arc<RwLock<std::collections::HashMap<String, Arc<VirtualVariableFile>>>>,

    /// File watcher for change detection
    file_watcher: Arc<RwLock<Option<Arc<VariableFileWatcher>>>>,

    /// Services mapped locally (scan mode), by service id
    shared: Arc<RwLock<std::collections::HashMap<String, SharedFile>>>,

    /// Background heartbeat task handle
    #[allow(
        dead_code,
        reason = "set only by the disabled background heartbeat task"
    )]
    heartbeat_task: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
}

impl Client {
    /// Initialize a fully-configured client in one call
    ///
    /// This is the primary entry point. It:
    /// 1. Creates a new client
    /// 2. Connects to the server
    /// 3. Authenticates to the tenant
    /// 4. Initializes the file watcher
    /// 5. Starts file monitoring
    ///
    /// Returns the [`Session`] for the tenant (its [`Session::client`] is the client). After this
    /// the session is ready to use with `get_virtual_service_file()`.
    pub async fn initialize(
        server_url: impl Into<String>,
        tenant_id: impl Into<String>,
        credentials: AuthCredentials,
    ) -> Result<Session> {
        let client = Self::_new(server_url);
        client._connect_impl().await?;
        let auth = client
            ._authenticate_impl(&tenant_id.into(), credentials)
            .await?;
        client._init_file_watcher_impl().await?;
        client._start_file_monitoring_impl().await?;
        Ok(Session::new(client, auth))
    }

    /// Create a new client (internal)
    #[inline]
    fn _new(server_url: impl Into<String>) -> Self {
        let client_id = Uuid::new_v4().to_string();

        Self {
            client_id: client_id.clone(),
            server_url: server_url.into(),
            connection: Arc::new(RwLock::new(None)),
            state: create_shared_state(client_id),
            heartbeat_interval: Duration::from_secs(30),
            max_reconnect_attempts: 5,
            tls_root_pem: None,
            dev_scopes: None,
            reconnect_attempts: Arc::new(AtomicU64::new(0)),
            virtual_files: Arc::new(RwLock::new(std::collections::HashMap::new())),
            file_watcher: Arc::new(RwLock::new(None)),
            shared: Arc::new(RwLock::new(std::collections::HashMap::new())),
            heartbeat_task: Arc::new(RwLock::new(None)),
        }
    }

    /// Trust exactly the PEM certificate(s) in `pem` instead of the system roots when connecting
    /// (a server with a self-signed certificate). Use it only for servers you control.
    pub fn with_tls_root_pem(mut self, pem: impl Into<Vec<u8>>) -> Self {
        self.tls_root_pem = Some(pem.into());
        self
    }

    /// Development servers only: authenticate with exactly `scopes` (`&[]` for none) instead of
    /// administrator, so this client holds no grant until an administrator gives it one. A
    /// production server ignores or refuses the request.
    pub fn with_dev_scopes(mut self, scopes: &[&str]) -> Self {
        self.dev_scopes = Some(scopes.iter().map(|s| s.to_string()).collect());
        self
    }

    /// Create a new client (public - for testing/special cases)
    pub fn new(server_url: impl Into<String>) -> Self {
        Self::_new(server_url)
    }

    /// Create a new client with custom ID
    pub fn with_id(server_url: impl Into<String>, client_id: impl Into<String>) -> Self {
        let client_id = client_id.into();

        Self {
            client_id: client_id.clone(),
            server_url: server_url.into(),
            connection: Arc::new(RwLock::new(None)),
            state: create_shared_state(client_id),
            heartbeat_interval: Duration::from_secs(30),
            max_reconnect_attempts: 5,
            tls_root_pem: None,
            dev_scopes: None,
            reconnect_attempts: Arc::new(AtomicU64::new(0)),
            virtual_files: Arc::new(RwLock::new(std::collections::HashMap::new())),
            file_watcher: Arc::new(RwLock::new(None)),
            shared: Arc::new(RwLock::new(std::collections::HashMap::new())),
            heartbeat_task: Arc::new(RwLock::new(None)),
        }
    }

    /// Get client ID
    pub fn id(&self) -> &str {
        &self.client_id
    }

    /// Get server URL
    pub fn server_url(&self) -> &str {
        &self.server_url
    }

    /// Connect to server (internal)
    #[inline]
    async fn _connect_impl(&self) -> Result<()> {
        let mut state = self.state.write().await;
        state.connection_state = ConnectionState::Connecting;
        drop(state);

        match Connection::new_with_roots(&self.server_url, self.tls_root_pem.as_deref()).await {
            Ok(conn) => {
                let mut state = self.state.write().await;
                state.connection_state = ConnectionState::Connected;
                state.session_id = Some(Uuid::new_v4().to_string());
                drop(state);

                let mut conn_guard = self.connection.write().await;
                *conn_guard = Some(conn);

                // Reset reconnection attempts on successful connection
                self.reconnect_attempts.store(0, Ordering::SeqCst);

                // TODO: Re-enable background heartbeat task
                // For now, disabled to avoid message ordering issues with concurrent operations
                // self.start_heartbeat_task().await;

                // WebSocket connection established - no Connect message needed
                // Authentication will be the first message sent

                Ok(())
            }
            Err(e) => {
                let mut state = self.state.write().await;
                state.connection_state = ConnectionState::Disconnected;
                Err(e)
            }
        }
    }

    /// Connect to server (public - for testing/special cases)
    pub async fn connect(&self) -> Result<()> {
        self._connect_impl().await
    }

    /// Authenticate to a tenant (internal)
    #[inline]
    async fn _authenticate_impl(
        &self,
        tenant_id: &str,
        credentials: AuthCredentials,
    ) -> Result<AuthContext> {
        let request = commy_wire::Authenticate {
            request_id: Uuid::new_v4().to_string(),
            protocol_version: commy_wire::PROTOCOL_VERSION,
            tenant_id: tenant_id.to_string(),
            client_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            client_id: Some(self.client_id.clone()),
            dev_scopes: self.dev_scopes.clone(),
            credentials: (&credentials).into(),
        };
        request
            .validate()
            .map_err(|e| CommyError::InvalidRequest(e.to_string()))?;
        let request_id = request.request_id.clone();
        self.send_message(ClientMessage::Authenticate(request))
            .await?;

        // Wait for the authentication result that names this request
        let guard = self.connection.read().await;
        let Some(conn) = guard.as_ref() else {
            return Err(CommyError::ConnectionLost(
                "Connection not established".to_string(),
            ));
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            match tokio::time::timeout_at(deadline, conn.recv()).await {
                Ok(Ok(Some(ServerMessage::AuthenticationResult(r))))
                    if r.request_id == request_id =>
                {
                    if !r.success {
                        return Err(CommyError::AuthenticationFailed(r.message));
                    }
                    let auth_context =
                        AuthContext::new(tenant_id.to_string(), r.permissions.unwrap_or_default());
                    let mut state = self.state.write().await;
                    state.connection_state = ConnectionState::Authenticated;
                    state.add_auth_context(tenant_id.to_string(), auth_context.clone());
                    return Ok(auth_context);
                }
                Ok(Ok(Some(ServerMessage::Error {
                    code,
                    request_id: erid,
                    ..
                }))) if erid.as_deref().is_none_or(|e| e == request_id) => {
                    return Err(CommyError::from(code))
                }
                Ok(Ok(Some(_))) => continue,
                Ok(Ok(None)) | Ok(Err(_)) => {
                    return Err(CommyError::ConnectionLost(
                        "Connection closed during authenticate".to_string(),
                    ))
                }
                Err(_) => return Err(CommyError::Timeout),
            }
        }
    }

    /// Authenticate to a tenant and return the [`Session`] for it.
    ///
    /// Every operation that needs an authenticated connection is a method on the returned
    /// session. Authenticating again (another tenant, or the same one) returns another session on
    /// the same connection.
    pub async fn authenticate(
        &self,
        tenant_id: impl Into<String>,
        credentials: AuthCredentials,
    ) -> Result<Session> {
        let auth = self.authenticate_context(tenant_id, credentials).await?;
        Ok(Session::new(self.clone(), auth))
    }

    /// Authenticate and return the raw [`AuthContext`] (no session).
    pub(crate) async fn authenticate_context(
        &self,
        tenant_id: impl Into<String>,
        credentials: AuthCredentials,
    ) -> Result<AuthContext> {
        self._authenticate_impl(&tenant_id.into(), credentials)
            .await
    }

    /// Create a new service in a tenant
    ///
    /// Returns the service ID on success. Returns error if:
    /// - Not authenticated to the tenant
    /// - Service already exists
    /// - Insufficient permissions (need create_service permission)
    pub(crate) async fn create_service(
        &self,
        tenant_id: &str,
        service_name: &str,
        size: u64,
    ) -> Result<String> {
        // Check if authenticated to this tenant
        let state = self.state.read().await;
        if !state.is_authenticated_to(tenant_id) {
            return Err(CommyError::PermissionDenied(format!(
                "Not authenticated to tenant: {}",
                tenant_id
            )));
        }
        drop(state);

        // Request service creation
        self.send_message(ClientMessage::CreateService {
            tenant_id: tenant_id.to_string(),
            service_name: service_name.to_string(),
            size,
        })
        .await?;

        // Wait for service response
        if let Some(conn) = &*self.connection.read().await {
            match tokio::time::timeout(Duration::from_secs(10), conn.recv()).await {
                Ok(Ok(Some(ServerMessage::Service { service_id, .. }))) => Ok(service_id),
                Ok(Ok(Some(ServerMessage::Error { code, .. }))) => Err(CommyError::from(code)),
                Err(_) => Err(CommyError::Timeout),
                _ => Err(CommyError::Timeout),
            }
        } else {
            Err(CommyError::ConnectionLost(
                "Connection lost during create_service".to_string(),
            ))
        }
    }

    /// Get an existing service from a tenant (read-only, no side effects)
    ///
    /// Returns error if:
    /// - Not authenticated to the tenant
    /// - Service does not exist (NotFound error)
    /// - Insufficient permissions (need read_service permission)
    pub(crate) async fn get_service(&self, tenant_id: &str, service_name: &str) -> Result<Service> {
        // Check if authenticated to this tenant
        let state = self.state.read().await;
        if !state.is_authenticated_to(tenant_id) {
            return Err(CommyError::PermissionDenied(format!(
                "Not authenticated to tenant: {}",
                tenant_id
            )));
        }
        drop(state);

        // Request service
        self.send_message(ClientMessage::GetService {
            tenant_id: tenant_id.to_string(),
            service_name: service_name.to_string(),
        })
        .await?;

        // Wait for service response
        if let Some(conn) = &*self.connection.read().await {
            match tokio::time::timeout(Duration::from_secs(10), conn.recv()).await {
                Ok(Ok(Some(ServerMessage::Service {
                    service_id,
                    service_name,
                    tenant_id: resp_tenant,
                    file_path,
                }))) => {
                    let service = Service::new(service_id, service_name, resp_tenant, file_path);
                    Ok(service)
                }
                Ok(Ok(Some(ServerMessage::Error { code, .. }))) => Err(CommyError::from(code)),
                Err(_) => Err(CommyError::Timeout),
                _ => Err(CommyError::Timeout),
            }
        } else {
            Err(CommyError::ConnectionLost(
                "Connection lost during get_service".to_string(),
            ))
        }
    }

    /// Delete a service from a tenant
    ///
    /// Returns error if:
    /// - Not authenticated to the tenant
    /// - Service does not exist
    /// - Insufficient permissions (need delete_service permission, typically admin)
    pub(crate) async fn delete_service(&self, tenant_id: &str, service_name: &str) -> Result<()> {
        // Check if authenticated to this tenant
        let state = self.state.read().await;
        if !state.is_authenticated_to(tenant_id) {
            return Err(CommyError::PermissionDenied(format!(
                "Not authenticated to tenant: {}",
                tenant_id
            )));
        }
        drop(state);

        // Request service deletion
        self.send_message(ClientMessage::DeleteService {
            tenant_id: tenant_id.to_string(),
            service_name: service_name.to_string(),
        })
        .await?;

        // Wait for result acknowledgment
        if let Some(conn) = &*self.connection.read().await {
            match tokio::time::timeout(Duration::from_secs(10), conn.recv()).await {
                Ok(Ok(Some(ServerMessage::Result { success: true, .. }))) => Ok(()),
                Ok(Ok(Some(ServerMessage::Result {
                    success: false,
                    message,
                    ..
                }))) => Err(CommyError::PermissionDenied(message)),
                Ok(Ok(Some(ServerMessage::Error { code, .. }))) => Err(CommyError::from(code)),
                Err(_) => Err(CommyError::Timeout),
                _ => Err(CommyError::Timeout),
            }
        } else {
            Err(CommyError::ConnectionLost(
                "Connection lost during delete_service".to_string(),
            ))
        }
    }

    /// Create a new tenant (admin operation)
    ///
    /// Requires admin credentials or special permissions to create tenants.
    /// Returns the tenant ID on success.
    ///
    /// Returns error if:
    /// - Not connected to server
    /// - Tenant already exists
    /// - Insufficient permissions (need admin role)
    pub(crate) async fn create_tenant(&self, tenant_id: &str, tenant_name: &str) -> Result<String> {
        self.require_session().await?;

        // Request tenant creation
        self.send_message(ClientMessage::CreateTenant {
            tenant_id: tenant_id.to_string(),
            tenant_name: tenant_name.to_string(),
        })
        .await?;

        // Wait for result
        if let Some(conn) = &*self.connection.read().await {
            match tokio::time::timeout(Duration::from_secs(10), conn.recv()).await {
                Ok(Ok(Some(ServerMessage::TenantResult {
                    success: true,
                    tenant_id: returned_id,
                    ..
                }))) => Ok(returned_id),
                Ok(Ok(Some(ServerMessage::TenantResult {
                    success: false,
                    message,
                    ..
                }))) => Err(CommyError::PermissionDenied(message)),
                Ok(Ok(Some(ServerMessage::Error { code, .. }))) => Err(CommyError::from(code)),
                Err(_) => Err(CommyError::Timeout),
                _ => Err(CommyError::Timeout),
            }
        } else {
            Err(CommyError::ConnectionLost(
                "Connection lost during create_tenant".to_string(),
            ))
        }
    }

    /// Delete a tenant (admin operation)
    ///
    /// Removes all services and data associated with the tenant.
    /// Requires admin credentials or special permissions.
    ///
    /// Returns error if:
    /// - Not connected to server
    /// - Tenant does not exist
    /// - Insufficient permissions (need admin role)
    /// - Tenant has active clients
    pub(crate) async fn delete_tenant(&self, tenant_id: &str) -> Result<()> {
        self.require_session().await?;

        // Request tenant deletion
        self.send_message(ClientMessage::DeleteTenant {
            tenant_id: tenant_id.to_string(),
        })
        .await?;

        // Wait for result acknowledgment
        if let Some(conn) = &*self.connection.read().await {
            match tokio::time::timeout(Duration::from_secs(10), conn.recv()).await {
                Ok(Ok(Some(ServerMessage::Result { success: true, .. }))) => Ok(()),
                Ok(Ok(Some(ServerMessage::Result {
                    success: false,
                    message,
                    ..
                }))) => Err(CommyError::PermissionDenied(message)),
                Ok(Ok(Some(ServerMessage::Error { code, .. }))) => Err(CommyError::from(code)),
                Err(_) => Err(CommyError::Timeout),
                _ => Err(CommyError::Timeout),
            }
        } else {
            Err(CommyError::ConnectionLost(
                "Connection lost during delete_tenant".to_string(),
            ))
        }
    }

    /// Send heartbeat to server
    pub async fn heartbeat(&self) -> Result<()> {
        self.send_message(ClientMessage::Heartbeat {
            client_id: self.client_id.clone(),
        })
        .await?;

        // Wait for heartbeat response from server
        if let Some(conn) = &*self.connection.read().await {
            match tokio::time::timeout(Duration::from_secs(10), conn.recv()).await {
                Ok(Ok(Some(ServerMessage::Heartbeat { .. }))) => {
                    // Heartbeat response received successfully
                }
                _ => {
                    // Heartbeat response not received or wrong type, but don't fail
                }
            }
        }

        let mut state = self.state.write().await;
        state.touch();

        Ok(())
    }

    /// Disconnect from server
    pub async fn disconnect(&self) -> Result<()> {
        self.send_message(ClientMessage::Disconnect {
            client_id: self.client_id.clone(),
        })
        .await?;

        let mut conn_guard = self.connection.write().await;
        *conn_guard = None;

        let mut state = self.state.write().await;
        state.reset();

        Ok(())
    }

    /// Check if connected
    pub async fn is_connected(&self) -> bool {
        self.connection.read().await.is_some()
    }

    /// Get current connection state
    pub async fn connection_state(&self) -> ConnectionState {
        let state = self.state.read().await;
        state.connection_state
    }

    /// Refuse an operation that needs a session when no tenant is authenticated, before anything
    /// is sent. (The server answers `UNAUTHENTICATED` to these too; this saves the round trip and
    /// the reply-queue slot.) `heartbeat` is exempt, as it is on the server.
    async fn require_session(&self) -> Result<()> {
        if self.state.read().await.authenticated_tenants().is_empty() {
            return Err(CommyError::PermissionDenied(
                "Not authenticated: call authenticate first".to_string(),
            ));
        }
        Ok(())
    }

    /// Get authenticated tenants
    pub async fn authenticated_tenants(&self) -> Vec<String> {
        let state = self.state.read().await;
        state
            .authenticated_tenants()
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    }

    /// Check if authenticated to tenant
    pub async fn is_authenticated_to(&self, tenant_id: &str) -> bool {
        let state = self.state.read().await;
        state.is_authenticated_to(tenant_id)
    }

    /// Get idle time in seconds
    pub async fn idle_seconds(&self) -> u64 {
        let state = self.state.read().await;
        state.idle_seconds()
    }

    /// Send a message to server with automatic reconnection
    async fn send_message(&self, msg: ClientMessage) -> Result<()> {
        // Try sending the message
        let result = self.send_message_once(msg.clone()).await;

        // If connection was lost, attempt reconnection
        if let Err(CommyError::ConnectionLost(_)) = result {
            let current_attempts = self.reconnect_attempts.fetch_add(1, Ordering::SeqCst);

            if current_attempts < self.max_reconnect_attempts as u64 {
                // Exponential backoff: 1s, 2s, 4s, 8s, 16s
                let delay = Duration::from_secs(2_u64.pow(current_attempts as u32).min(16));
                tokio::time::sleep(delay).await;

                // Attempt to reconnect
                if let Ok(()) = self._connect_impl().await {
                    // Retry the message after reconnection
                    return self.send_message_once(msg).await;
                }
            }

            return Err(CommyError::ConnectionLost(format!(
                "Connection lost after {} reconnection attempts",
                current_attempts + 1
            )));
        }

        result
    }

    /// Send a message without reconnection logic (internal)
    async fn send_message_once(&self, msg: ClientMessage) -> Result<()> {
        let conn_guard = self.connection.read().await;
        if let Some(conn) = conn_guard.as_ref() {
            conn.send(msg).await?;

            let mut state = self.state.write().await;
            state.touch();

            Ok(())
        } else {
            Err(CommyError::ConnectionLost(
                "Connection not established".to_string(),
            ))
        }
    }

    /// Ask the server whether `service_id` may be mapped locally and, if it is offered and the
    /// nonce checks out, return the confirmed mapping. Any failure after the offer (unopenable
    /// file, bad nonce file, no ack) returns `local: None`: the caller stays on WSS ranges.
    ///
    /// While waiting, messages that do not carry this call's `request_id` are discarded.
    pub(crate) async fn negotiate_local(&self, service_id: &str) -> Result<NegotiatedFile> {
        self.negotiate_local_with_timeout(service_id, Duration::from_secs(10), true)
            .await
    }

    /// `confirm`: take up a local mapping if one is offered. A caller that only wants the file id
    /// (a remote mirror) passes `false`: confirming would make the server treat it as a local
    /// mapper, which sends `RangesChanged` and has its range frames refused.
    async fn negotiate_local_with_timeout(
        &self,
        service_id: &str,
        wait: Duration,
        confirm: bool,
    ) -> Result<NegotiatedFile> {
        let guard = self.connection.read().await;
        let Some(conn) = guard.as_ref() else {
            return Err(CommyError::ConnectionLost(
                "Connection lost during negotiate_local".to_string(),
            ));
        };

        let request_id = Uuid::new_v4().to_string();
        conn.send(ClientMessage::GetServiceFilePath(
            commy_wire::GetServiceFilePath {
                request_id: request_id.clone(),
                service_id: service_id.to_string(),
            },
        ))
        .await?;

        let deadline = tokio::time::Instant::now() + wait;
        let offer = loop {
            match tokio::time::timeout_at(deadline, conn.recv()).await {
                Ok(Ok(Some(ServerMessage::ServiceFilePath(o)))) if o.request_id == request_id => {
                    break o
                }
                Ok(Ok(Some(ServerMessage::Error {
                    code,
                    request_id: erid,
                    ..
                }))) if erid.as_deref().is_none_or(|e| e == request_id) => {
                    return Err(CommyError::from(code))
                }
                Ok(Ok(Some(_))) => continue,
                Ok(Ok(None)) | Ok(Err(_)) => {
                    return Err(CommyError::ConnectionLost(
                        "Connection closed during negotiate_local".to_string(),
                    ))
                }
                Err(_) => return Err(CommyError::Timeout),
            }
        };

        let stay_on_wss = NegotiatedFile {
            file_id: offer.file_id,
            local: None,
        };
        if !confirm {
            return Ok(stay_on_wss);
        }
        let map = match LocalMap::from_offer(
            offer.path.as_deref().map(std::path::Path::new),
            offer.nonce_path.as_deref().map(std::path::Path::new),
        ) {
            LocalMapOutcome::Local(m) => *m,
            LocalMapOutcome::Fallback(_) => return Ok(stay_on_wss),
        };

        let confirm_id = Uuid::new_v4().to_string();
        conn.send(ClientMessage::ConfirmLocal(commy_wire::ConfirmLocal {
            request_id: confirm_id.clone(),
            file_id: offer.file_id,
            nonce: map.nonce().expose().clone(),
        }))
        .await?;

        loop {
            match tokio::time::timeout_at(deadline, conn.recv()).await {
                Ok(Ok(Some(ServerMessage::ConfirmLocalAck(a)))) if a.request_id == confirm_id => {
                    return Ok(NegotiatedFile {
                        file_id: offer.file_id,
                        local: Some(map),
                    })
                }
                // A refusal (wrong nonce, rate limit) or silence: stay on WSS ranges.
                Ok(Ok(Some(ServerMessage::Error {
                    request_id: erid, ..
                }))) if erid.as_deref().is_none_or(|e| e == confirm_id) => return Ok(stay_on_wss),
                Err(_) => return Ok(stay_on_wss),
                Ok(Ok(Some(_))) => continue,
                Ok(Ok(None)) | Ok(Err(_)) => {
                    return Err(CommyError::ConnectionLost(
                        "Connection closed during negotiate_local".to_string(),
                    ))
                }
            }
        }
    }

    /// Map `service_id` locally when the server offers it (see [`Client::negotiate_local`]) and
    /// start tracking it in scan mode. Returns the file id when local mode is active, `None` when
    /// the service stays on WSS ranges.
    pub(crate) async fn open_shared(&self, service_id: &str) -> Result<Option<u32>> {
        let negotiated = self.negotiate_local(service_id).await?;
        let Some(map) = negotiated.local else {
            return Ok(None);
        };
        let file_id = negotiated.file_id;
        self.shared
            .write()
            .await
            .insert(service_id.to_string(), SharedFile::new(map, file_id));
        Ok(Some(file_id))
    }

    /// Trap writes to a locally mapped service instead of scanning every page at flush
    /// ([`SharedFile::arm_trap`]); `pool_pages / 2` pages are trapped. Opt-in.
    ///
    /// # Safety
    /// While armed, no thread may block `SIGSEGV`/`SIGBUS` while writing the mapping (Unix), and
    /// nothing may read from the OS into it.
    pub(crate) async unsafe fn arm_shared_trap(
        &self,
        service_id: &str,
        pool_pages: usize,
    ) -> Result<()> {
        let mut shared = self.shared.write().await;
        let file = shared.get_mut(service_id).ok_or_else(|| {
            CommyError::NotFound(format!("service {service_id} is not mapped locally"))
        })?;
        // SAFETY: the caller upholds the contract above.
        unsafe { file.arm_trap(pool_pages) }
            .map_err(|e| CommyError::InvalidState(format!("write trap not armed: {e:?}")))
    }

    /// Write into a locally mapped service. Nothing is sent until [`Client::flush_shared`].
    pub(crate) async fn shared_write(
        &self,
        service_id: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<()> {
        let mut shared = self.shared.write().await;
        let file = shared.get_mut(service_id).ok_or_else(|| {
            CommyError::NotFound(format!("service {service_id} is not mapped locally"))
        })?;
        file.write_range(offset, bytes).map_err(|e| match e {
            crate::shared_file::WriteError::Draining => {
                CommyError::InvalidState("a rotation is draining this service".to_string())
            }
            crate::shared_file::WriteError::OutOfBounds => {
                CommyError::InvalidOffset(format!("write at {offset} is outside the file"))
            }
        })
    }

    /// Copy `len` bytes at `offset` out of a mapped or remote-mirrored service.
    pub(crate) async fn shared_read(
        &self,
        service_id: &str,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>> {
        let shared = self.shared.read().await;
        let file = shared
            .get(service_id)
            .ok_or_else(|| CommyError::NotFound(format!("service {service_id} is not open")))?;
        let out_of_bounds =
            || CommyError::InvalidOffset(format!("read at {offset}+{len} is outside the file"));
        let end = offset.checked_add(len).ok_or_else(out_of_bounds)?;
        file.view()
            .get(offset as usize..end as usize)
            .map(<[u8]>::to_vec)
            .ok_or_else(out_of_bounds)
    }

    /// Report what changed in a locally mapped service to the server, one `RangesChanged` per
    /// batch of up to 1024 ranges, each answered by a `FlushAck`. Returns the highest `seq` seen.
    pub(crate) async fn flush_shared(&self, service_id: &str) -> Result<u64> {
        self.flush_shared_with_timeout(service_id, Duration::from_secs(10))
            .await
    }

    async fn flush_shared_with_timeout(&self, service_id: &str, wait: Duration) -> Result<u64> {
        // Local mode sends ranges only; remote mode sends the changed bytes as range frames
        // followed by a FlushEnd.
        let (file_id, batches, frames) = {
            let mut shared = self.shared.write().await;
            let file = shared.get_mut(service_id).ok_or_else(|| {
                CommyError::NotFound(format!("service {service_id} is not mapped locally"))
            })?;
            if file.is_local() {
                (file.file_id(), file.commit_local(), Vec::new())
            } else {
                (file.file_id(), Vec::new(), file.commit())
            }
        };
        let guard = self.connection.read().await;
        let Some(conn) = guard.as_ref() else {
            return Err(CommyError::ConnectionLost(
                "Connection lost during flush_shared".to_string(),
            ));
        };
        let mut seq = 0;
        for ranges in batches {
            let request_id = Uuid::new_v4().to_string();
            conn.send(ClientMessage::RangesChanged(commy_wire::RangesChanged {
                request_id: request_id.clone(),
                file_id,
                ranges,
            }))
            .await?;
            seq = seq.max(
                self.await_flush_ack(conn, service_id, &request_id, wait)
                    .await?,
            );
        }
        if !frames.is_empty() {
            for frame in frames {
                conn.send(ClientMessage::Range(frame)).await?;
            }
            let request_id = Uuid::new_v4().to_string();
            conn.send(ClientMessage::FlushEnd(commy_wire::FlushEnd {
                request_id: request_id.clone(),
                file_id,
            }))
            .await?;
            seq = seq.max(
                self.await_flush_ack(conn, service_id, &request_id, wait)
                    .await?,
            );
        }
        Ok(seq)
    }

    /// Wait for the `FlushAck` of `request_id`, applying any update that arrives meanwhile.
    async fn await_flush_ack(
        &self,
        conn: &Connection,
        service_id: &str,
        request_id: &str,
        wait: Duration,
    ) -> Result<u64> {
        let deadline = tokio::time::Instant::now() + wait;
        let acked = loop {
            match tokio::time::timeout_at(deadline, conn.recv()).await {
                Ok(Ok(Some(ServerMessage::FlushAck(a)))) if a.request_id == request_id => {
                    break a.seq
                }
                Ok(Ok(Some(ServerMessage::Error {
                    code,
                    request_id: erid,
                    ..
                }))) if erid.as_deref().is_none_or(|e| e == request_id) => {
                    return Err(CommyError::from(code))
                }
                Ok(Ok(Some(ServerMessage::RangesUpdated(u)))) => {
                    if let Some(f) = self.shared.write().await.get_mut(service_id) {
                        f.on_ranges_updated(&u);
                    }
                }
                Ok(Ok(Some(ServerMessage::Range(frame)))) => {
                    if let Some(f) = self.shared.write().await.get_mut(service_id) {
                        let _ = f.apply_inbound(&frame);
                    }
                }
                Ok(Ok(Some(_))) => continue,
                Ok(Ok(None)) | Ok(Err(_)) => {
                    return Err(CommyError::ConnectionLost(
                        "Connection closed during flush_shared".to_string(),
                    ))
                }
                Err(_) => return Err(CommyError::Timeout),
            }
        };
        if let Some(f) = self.shared.write().await.get_mut(service_id) {
            f.on_flush_ack(acked);
        }
        Ok(acked)
    }

    /// Remote mode: fetch the whole file with `Resync` and keep a private copy that range frames
    /// keep current. Use when [`Client::open_shared`] returns `None` (no local mapping offered).
    /// Returns the file id.
    pub(crate) async fn open_remote(&self, service_id: &str) -> Result<u32> {
        self.open_remote_with_timeout(service_id, Duration::from_secs(10))
            .await
    }

    async fn open_remote_with_timeout(&self, service_id: &str, wait: Duration) -> Result<u32> {
        // The offer carries the file id; a local mapping offered is neither opened nor confirmed.
        let file_id = self
            .negotiate_local_with_timeout(service_id, wait, false)
            .await?
            .file_id;
        let guard = self.connection.read().await;
        let Some(conn) = guard.as_ref() else {
            return Err(CommyError::ConnectionLost(
                "Connection lost during open_remote".to_string(),
            ));
        };
        let request_id = Uuid::new_v4().to_string();
        conn.send(ClientMessage::Resync(commy_wire::Resync {
            request_id: request_id.clone(),
            file_id,
        }))
        .await?;
        let deadline = tokio::time::Instant::now() + wait;
        let mut frames = Vec::new();
        let done = loop {
            match tokio::time::timeout_at(deadline, conn.recv()).await {
                Ok(Ok(Some(ServerMessage::Range(f)))) if f.file_id == file_id => frames.push(f),
                Ok(Ok(Some(ServerMessage::ResyncDone(d)))) if d.request_id == request_id => {
                    break d
                }
                Ok(Ok(Some(ServerMessage::Error {
                    code,
                    request_id: erid,
                    ..
                }))) if erid.as_deref().is_none_or(|e| e == request_id) => {
                    return Err(CommyError::from(code))
                }
                Ok(Ok(Some(_))) => continue,
                Ok(Ok(None)) | Ok(Err(_)) => {
                    return Err(CommyError::ConnectionLost(
                        "Connection closed during open_remote".to_string(),
                    ))
                }
                Err(_) => return Err(CommyError::Timeout),
            }
        };
        let size = usize::try_from(done.size)
            .ok()
            .filter(|n| *n <= MAX_REMOTE_FILE_BYTES)
            .ok_or_else(|| {
                CommyError::InvalidMessage(format!("resync size {} is not acceptable", done.size))
            })?;
        let mut bytes = vec![0u8; size];
        for f in &frames {
            let end = usize::try_from(f.offset)
                .ok()
                .and_then(|o| o.checked_add(f.data.len()).map(|e| (o, e)))
                .filter(|(_, e)| *e <= size)
                .ok_or_else(|| {
                    CommyError::InvalidMessage("resync frame outside the file".to_string())
                })?;
            bytes[end.0..end.1].copy_from_slice(&f.data);
        }
        self.shared.write().await.insert(
            service_id.to_string(),
            SharedFile::from_snapshot(bytes, file_id, done.seq),
        );
        Ok(file_id)
    }

    /// Wait up to `wait` for one server message and, if it is a `RangesUpdated` or range frame for
    /// a mapped service, apply it so it is not reported as a local change. Returns `true` when an
    /// update was applied. Other messages are discarded; use [`Client::poll_shared_events`] to see
    /// rotation and loss events.
    pub(crate) async fn poll_shared_updates(&self, wait: Duration) -> Result<bool> {
        Ok(matches!(
            self.poll_shared_events(wait).await?,
            SharedEvent::Updated
        ))
    }

    /// Like [`Client::poll_shared_updates`], but also reports rotation and loss events.
    pub(crate) async fn poll_shared_events(&self, wait: Duration) -> Result<SharedEvent> {
        let guard = self.connection.read().await;
        let Some(conn) = guard.as_ref() else {
            return Err(CommyError::ConnectionLost(
                "Connection lost during poll_shared_events".to_string(),
            ));
        };
        let msg = match tokio::time::timeout(wait, conn.recv()).await {
            Ok(Ok(Some(m))) => m,
            Ok(Ok(None)) | Ok(Err(_)) => {
                return Err(CommyError::ConnectionLost(
                    "Connection closed during poll_shared_events".to_string(),
                ))
            }
            Err(_) => return Ok(SharedEvent::None),
        };
        drop(guard);
        Ok(self.apply_event(msg).await)
    }

    async fn apply_event(&self, msg: ServerMessage) -> SharedEvent {
        match msg {
            ServerMessage::RangesUpdated(u) => {
                let mut shared = self.shared.write().await;
                let n: usize = shared.values_mut().map(|f| f.on_ranges_updated(&u)).sum();
                if n > 0 {
                    SharedEvent::Updated
                } else {
                    SharedEvent::None
                }
            }
            ServerMessage::Range(frame) => {
                let mut shared = self.shared.write().await;
                let applied = shared
                    .values_mut()
                    .any(|f| f.apply_inbound(&frame) == Ok(crate::shared_file::Applied::Applied));
                if applied {
                    SharedEvent::Updated
                } else {
                    SharedEvent::None
                }
            }
            ServerMessage::RotationPending(p) => SharedEvent::RotationPending(p),
            ServerMessage::RotationPrepared(p) => SharedEvent::RotationPrepared(p),
            ServerMessage::RotationCommit(c) => SharedEvent::RotationCommit(c),
            ServerMessage::RotationAborted(a) => SharedEvent::RotationAborted(a),
            ServerMessage::Loss(l) => {
                if l.kind != commy_wire::LossKind::ResyncOverwrote {
                    let mut shared = self.shared.write().await;
                    if shared.get(&l.service_id).map(SharedFile::file_id) == Some(l.file_id) {
                        shared.remove(&l.service_id);
                    }
                }
                SharedEvent::Loss(l)
            }
            _ => SharedEvent::None,
        }
    }

    /// Give `client_id` permissions (`tenant:read|write|admin`, `service:read|write|admin|resize`),
    /// tenant-wide or, with `service_id`, on that service. The caller needs admin rights there.
    pub(crate) async fn grant(
        &self,
        client_id: &str,
        service_id: Option<&str>,
        permissions: &[&str],
    ) -> Result<()> {
        let req = commy_wire::Grant {
            request_id: Uuid::new_v4().to_string(),
            client_id: client_id.to_string(),
            service_id: service_id.map(str::to_string),
            permissions: permissions.iter().map(|p| p.to_string()).collect(),
        };
        req.validate()
            .map_err(|e| CommyError::InvalidRequest(e.to_string()))?;
        let id = req.request_id.clone();
        self.grant_round_trip(ClientMessage::Grant(req), &id)
            .await
            .map(|_| ())
    }

    /// Take `permissions` from `client_id`. With `service_id` and no permissions, the client is
    /// removed from that service: it gets `Loss{REVOKED}` and the file rotates (see
    /// [`Client::complete_rotation`]).
    pub(crate) async fn revoke(
        &self,
        client_id: &str,
        service_id: Option<&str>,
        permissions: &[&str],
    ) -> Result<()> {
        let req = commy_wire::Revoke {
            request_id: Uuid::new_v4().to_string(),
            client_id: client_id.to_string(),
            service_id: service_id.map(str::to_string),
            permissions: permissions.iter().map(|p| p.to_string()).collect(),
        };
        req.validate()
            .map_err(|e| CommyError::InvalidRequest(e.to_string()))?;
        let id = req.request_id.clone();
        self.grant_round_trip(ClientMessage::Revoke(req), &id)
            .await
            .map(|_| ())
    }

    /// List grants, optionally only those of one client and/or one service.
    pub(crate) async fn list_grants(
        &self,
        client_id: Option<&str>,
        service_id: Option<&str>,
    ) -> Result<Vec<commy_wire::GrantEntry>> {
        let id = Uuid::new_v4().to_string();
        let msg = ClientMessage::ListGrants(commy_wire::ListGrants {
            request_id: id.clone(),
            client_id: client_id.map(str::to_string),
            service_id: service_id.map(str::to_string),
        });
        match self.grant_round_trip(msg, &id).await? {
            ServerMessage::Grants(g) => Ok(g.entries),
            _ => unreachable!("grant_round_trip returns only GrantAck or Grants"),
        }
    }

    /// Open (`true`) or close (`false`) a service to local memory mapping. Closed by default: until
    /// an administrator opens it, [`Client::open_shared`] gets no path and stays on WSS ranges.
    /// Needs `service:admin` on the service. Closing stops future offers; existing mappings end at
    /// the next rotation. Returns the state the server confirms.
    pub(crate) async fn set_local_mapping(&self, service_id: &str, enabled: bool) -> Result<bool> {
        let id = Uuid::new_v4().to_string();
        let msg = ClientMessage::SetLocalMapping(commy_wire::SetLocalMapping {
            request_id: id.clone(),
            service_id: service_id.to_string(),
            enabled,
        });
        match self.grant_round_trip(msg, &id).await? {
            ServerMessage::LocalMappingAck(a) => Ok(a.enabled),
            _ => unreachable!("grant_round_trip returns only the acks it waits for"),
        }
    }

    /// Send a grant-family request and wait for the reply carrying its `request_id`.
    async fn grant_round_trip(
        &self,
        msg: ClientMessage,
        request_id: &str,
    ) -> Result<ServerMessage> {
        let guard = self.connection.read().await;
        let Some(conn) = guard.as_ref() else {
            return Err(CommyError::ConnectionLost(
                "Connection lost during a grant request".to_string(),
            ));
        };
        conn.send(msg).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            match tokio::time::timeout_at(deadline, conn.recv()).await {
                Ok(Ok(Some(reply @ ServerMessage::GrantAck(_)))) if matches!(&reply, ServerMessage::GrantAck(a) if a.request_id == request_id) => {
                    return Ok(reply)
                }
                Ok(Ok(Some(reply @ ServerMessage::Grants(_)))) if matches!(&reply, ServerMessage::Grants(g) if g.request_id == request_id) => {
                    return Ok(reply)
                }
                Ok(Ok(Some(reply @ ServerMessage::LocalMappingAck(_)))) if matches!(&reply, ServerMessage::LocalMappingAck(a) if a.request_id == request_id) => {
                    return Ok(reply)
                }
                Ok(Ok(Some(ServerMessage::Error {
                    code,
                    request_id: erid,
                    ..
                }))) if erid.as_deref().is_none_or(|e| e == request_id) => {
                    return Err(CommyError::from(code))
                }
                Ok(Ok(Some(_))) => continue,
                Ok(Ok(None)) | Ok(Err(_)) => {
                    return Err(CommyError::ConnectionLost(
                        "Connection closed during a grant request".to_string(),
                    ))
                }
                Err(_) => return Err(CommyError::Timeout),
            }
        }
    }

    /// Ask the server to replace a service's file. Returns the rotation id. A resize also needs
    /// the other SDKs to drain; this client then sees `RotationPending` / `RotationCommit` like the
    /// others.
    pub(crate) async fn request_rotation(
        &self,
        service_id: &str,
        reason: commy_wire::RotateReason,
        new_size: Option<u64>,
        revoke_client_id: Option<String>,
    ) -> Result<String> {
        let req = commy_wire::RotateRequest {
            request_id: Uuid::new_v4().to_string(),
            service_id: service_id.to_string(),
            reason,
            new_size,
            revoke_client_id,
        };
        req.validate()
            .map_err(|e| CommyError::InvalidRequest(e.to_string()))?;
        let request_id = req.request_id.clone();
        let guard = self.connection.read().await;
        let Some(conn) = guard.as_ref() else {
            return Err(CommyError::ConnectionLost(
                "Connection lost during request_rotation".to_string(),
            ));
        };
        conn.send(ClientMessage::RotateRequest(req)).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            match tokio::time::timeout_at(deadline, conn.recv()).await {
                Ok(Ok(Some(ServerMessage::RotationAccepted(a)))) if a.request_id == request_id => {
                    return Ok(a.rotation_id)
                }
                Ok(Ok(Some(ServerMessage::Error {
                    code,
                    request_id: erid,
                    ..
                }))) if erid.as_deref().is_none_or(|e| e == request_id) => {
                    return Err(CommyError::from(code))
                }
                Ok(Ok(Some(_))) => continue,
                Ok(Ok(None)) | Ok(Err(_)) => {
                    return Err(CommyError::ConnectionLost(
                        "Connection closed during request_rotation".to_string(),
                    ))
                }
                Err(_) => return Err(CommyError::Timeout),
            }
        }
    }

    /// Answer a resize `RotationPending`: refuse new writes, flush into the old file, send
    /// `RotationDrained`, then wait for the end of the rotation (at most `deadline_ms + fill_ms`
    /// plus a grace period). Returns `Committed` (switched to the new file), `Aborted` (writes
    /// resume on the old file), or, for the requester only, `Prepared`: fill the new file with
    /// [`Client::begin_fill`] and finish with [`Client::finish_fill`]. Writers get an error until the
    /// end; old-offset writes are never replayed into the new layout.
    pub(crate) async fn drain_for_rotation(
        &self,
        p: &commy_wire::RotationPending,
    ) -> Result<RotationOutcome> {
        let start = tokio::time::Instant::now();
        let flush_wait = Duration::from_millis(p.deadline_ms);
        let bound = start
            + Duration::from_millis(p.deadline_ms)
            + Duration::from_millis(p.fill_ms)
            + ROTATION_COMMIT_GRACE;
        {
            let mut shared = self.shared.write().await;
            let file = shared.get_mut(&p.service_id).ok_or_else(|| {
                CommyError::NotFound(format!("service {} is not mapped", p.service_id))
            })?;
            if file.file_id() != p.file_id {
                return Err(CommyError::InvalidState(format!(
                    "pending names file {} but {} is mapped",
                    p.file_id,
                    file.file_id()
                )));
            }
            file.set_draining(true);
        }
        let result = self.drain_inner(p, flush_wait, bound).await;
        if result.is_err() {
            if let Some(f) = self.shared.write().await.get_mut(&p.service_id) {
                f.set_draining(false);
            }
        }
        result
    }

    async fn drain_inner(
        &self,
        p: &commy_wire::RotationPending,
        flush_wait: Duration,
        bound: tokio::time::Instant,
    ) -> Result<RotationOutcome> {
        self.flush_shared_with_timeout(&p.service_id, flush_wait)
            .await?;
        let seq = self
            .shared
            .read()
            .await
            .get(&p.service_id)
            .map(SharedFile::last_seq)
            .unwrap_or(0);
        {
            let guard = self.connection.read().await;
            let Some(conn) = guard.as_ref() else {
                return Err(CommyError::ConnectionLost(
                    "Connection lost during drain_for_rotation".to_string(),
                ));
            };
            conn.send(ClientMessage::RotationDrained(
                commy_wire::RotationDrained {
                    request_id: Uuid::new_v4().to_string(),
                    rotation_id: p.rotation_id.clone(),
                    file_id: p.file_id,
                    seq,
                },
            ))
            .await?;
        }
        self.await_rotation_end(&p.service_id, &p.rotation_id, bound, true)
            .await
    }

    /// Wait for the first of `RotationCommit` / `RotationAborted` for `rotation_id` (and, when
    /// `stop_on_prepared`, `RotationPrepared`). The first terminal message wins; later ones for the
    /// same id are never read as this rotation's end. A commit switches the mapping; an abort
    /// reopens writes on the old file.
    async fn await_rotation_end(
        &self,
        service_id: &str,
        rotation_id: &str,
        bound: tokio::time::Instant,
        stop_on_prepared: bool,
    ) -> Result<RotationOutcome> {
        enum End {
            Commit(commy_wire::RotationCommit),
            Aborted(commy_wire::RotationAborted),
            Prepared(commy_wire::RotationPrepared),
        }
        let end = {
            let guard = self.connection.read().await;
            let Some(conn) = guard.as_ref() else {
                return Err(CommyError::ConnectionLost(
                    "Connection lost during rotation".to_string(),
                ));
            };
            loop {
                match tokio::time::timeout_at(bound, conn.recv()).await {
                    Ok(Ok(Some(ServerMessage::RotationCommit(c))))
                        if c.rotation_id == rotation_id =>
                    {
                        break End::Commit(c)
                    }
                    Ok(Ok(Some(ServerMessage::RotationAborted(a))))
                        if a.rotation_id == rotation_id =>
                    {
                        break End::Aborted(a)
                    }
                    Ok(Ok(Some(ServerMessage::RotationPrepared(r))))
                        if stop_on_prepared && r.rotation_id == rotation_id =>
                    {
                        break End::Prepared(r)
                    }
                    Ok(Ok(Some(ServerMessage::Loss(l)))) if l.service_id == service_id => {
                        self.shared.write().await.remove(service_id);
                        return Err(CommyError::InvalidState(format!(
                            "dropped from the service: {:?}",
                            l.kind
                        )));
                    }
                    Ok(Ok(Some(other))) => {
                        self.apply_event(other).await;
                    }
                    Ok(Ok(None)) | Ok(Err(_)) => {
                        return Err(CommyError::ConnectionLost(
                            "Connection closed during rotation".to_string(),
                        ))
                    }
                    Err(_) => return Err(CommyError::Timeout),
                }
            }
        };
        match end {
            End::Commit(c) => {
                self.complete_rotation(&c).await?;
                Ok(RotationOutcome::Committed(c))
            }
            End::Aborted(a) => {
                if let Some(f) = self.shared.write().await.get_mut(service_id) {
                    f.set_draining(false);
                }
                Ok(RotationOutcome::Aborted(a))
            }
            End::Prepared(r) => Ok(RotationOutcome::Prepared(r)),
        }
    }

    /// Requester: get the prepared file ready to fill. With a `path` (same machine) the file is
    /// mapped directly; without one the new bytes are buffered and sent as range frames by
    /// [`Client::finish_fill`]. Read the old layout with [`Client::shared_read`].
    pub(crate) fn begin_fill(
        &self,
        pending: &commy_wire::RotationPending,
        prepared: &commy_wire::RotationPrepared,
    ) -> Result<RotationFill> {
        let target = match &prepared.path {
            Some(path) => FillTarget::Local(
                crate::local_map::open_fill_target(std::path::Path::new(path), prepared.size)
                    .map_err(|r| CommyError::InvalidState(format!("cannot fill {path}: {r:?}")))?,
            ),
            None => {
                let size = usize::try_from(prepared.size)
                    .ok()
                    .filter(|n| *n > 0 && *n <= MAX_REMOTE_FILE_BYTES)
                    .ok_or_else(|| {
                        CommyError::InvalidMessage(format!(
                            "prepared size {} is not acceptable",
                            prepared.size
                        ))
                    })?;
                FillTarget::Remote(vec![0; size])
            }
        };
        Ok(RotationFill {
            prepared: prepared.clone(),
            fill_ms: pending.fill_ms,
            target,
        })
    }

    /// Requester: the new file is filled. Send it (remote), say `RotationFilled`, and wait for the
    /// end of the rotation: `Committed` (switched) or `Aborted` (the old file stays).
    pub(crate) async fn finish_fill(&self, fill: RotationFill) -> Result<RotationOutcome> {
        let fill_wait = Duration::from_millis(fill.fill_ms);
        let bound = tokio::time::Instant::now() + fill_wait + ROTATION_COMMIT_GRACE;
        let RotationFill {
            prepared, target, ..
        } = fill;
        {
            let guard = self.connection.read().await;
            let Some(conn) = guard.as_ref() else {
                return Err(CommyError::ConnectionLost(
                    "Connection lost during finish_fill".to_string(),
                ));
            };
            match target {
                FillTarget::Local(map) => map
                    .flush()
                    .map_err(|e| CommyError::InvalidState(format!("flushing the new file: {e}")))?,
                FillTarget::Remote(data) => {
                    // The server's file is zero filled: only non-zero runs need to travel.
                    let changed = dirty_ranges::Scanner::new(&vec![0u8; data.len()])
                        .scan(&data)
                        .expect("same length");
                    for r in changed {
                        for frame in commy_wire::RangeFrame::split(
                            prepared.new_file_id,
                            0,
                            r.offset as u64,
                            &data[r.offset..r.offset + r.len],
                        ) {
                            conn.send(ClientMessage::Range(frame)).await?;
                        }
                    }
                    let request_id = Uuid::new_v4().to_string();
                    conn.send(ClientMessage::FlushEnd(commy_wire::FlushEnd {
                        request_id: request_id.clone(),
                        file_id: prepared.new_file_id,
                    }))
                    .await?;
                    self.await_flush_ack(conn, &prepared.service_id, &request_id, fill_wait)
                        .await?;
                }
            }
            conn.send(ClientMessage::RotationFilled(commy_wire::RotationFilled {
                request_id: Uuid::new_v4().to_string(),
                rotation_id: prepared.rotation_id.clone(),
            }))
            .await?;
        }
        self.await_rotation_end(&prepared.service_id, &prepared.rotation_id, bound, false)
            .await
    }

    /// Requester: give up. The server keeps the old file and sends `RotationAborted` to everyone.
    pub(crate) async fn abort_fill(&self, fill: RotationFill) -> Result<RotationOutcome> {
        let bound = tokio::time::Instant::now()
            + Duration::from_millis(fill.fill_ms)
            + ROTATION_COMMIT_GRACE;
        {
            let guard = self.connection.read().await;
            let Some(conn) = guard.as_ref() else {
                return Err(CommyError::ConnectionLost(
                    "Connection lost during abort_fill".to_string(),
                ));
            };
            conn.send(ClientMessage::RotationAbort(commy_wire::RotationAbort {
                request_id: Uuid::new_v4().to_string(),
                rotation_id: fill.prepared.rotation_id.clone(),
            }))
            .await?;
        }
        self.await_rotation_end(
            &fill.prepared.service_id,
            &fill.prepared.rotation_id,
            bound,
            false,
        )
        .await
    }

    /// Switch a service to the file named by a `RotationCommit`: re-run the local handshake (or
    /// `Resync` when no mapping is offered). After a revocation, unflushed local writes are
    /// written into the new file and flushed; after a resize they are never replayed. A commit
    /// that names the already-mapped file as its new file is a duplicate and ignored.
    pub(crate) async fn complete_rotation(&self, c: &commy_wire::RotationCommit) -> Result<()> {
        let (was_local, pending) = {
            let mut shared = self.shared.write().await;
            let Some(file) = shared.get_mut(&c.service_id) else {
                return Err(CommyError::NotFound(format!(
                    "service {} is not mapped",
                    c.service_id
                )));
            };
            if file.file_id() == c.new_file_id {
                return Ok(());
            }
            if file.file_id() != c.old_file_id {
                return Err(CommyError::InvalidState(format!(
                    "commit replaces file {} but {} is mapped",
                    c.old_file_id,
                    file.file_id()
                )));
            }
            let pending = if c.reason == commy_wire::RotateReason::Revocation {
                file.take_pending_writes()
            } else {
                Vec::new()
            };
            let was_local = file.is_local();
            shared.remove(&c.service_id);
            (was_local, pending)
        };
        let new_id = if was_local {
            match self.open_shared(&c.service_id).await? {
                Some(id) => id,
                None => self.open_remote(&c.service_id).await?,
            }
        } else {
            self.open_remote(&c.service_id).await?
        };
        if new_id != c.new_file_id {
            self.shared.write().await.remove(&c.service_id);
            return Err(CommyError::InvalidMessage(format!(
                "server offered file {new_id}, commit named {}",
                c.new_file_id
            )));
        }
        if !pending.is_empty() {
            for (offset, bytes) in &pending {
                // A write that no longer fits the new layout is dropped, never truncated.
                let _ = self.shared_write(&c.service_id, *offset, bytes).await;
            }
            self.flush_shared(&c.service_id).await?;
        }
        Ok(())
    }

    /// Start background heartbeat task
    #[allow(
        dead_code,
        reason = "the heartbeat task is disabled (see the TODO in `_connect_impl`)"
    )]
    async fn start_heartbeat_task(&self) {
        // Stop existing heartbeat task if any
        let mut task_guard = self.heartbeat_task.write().await;
        if let Some(handle) = task_guard.take() {
            handle.abort();
        }

        let interval = self.heartbeat_interval;
        let client_id = self.client_id.clone();
        let connection = Arc::clone(&self.connection);
        let state = Arc::clone(&self.state);

        let handle = tokio::spawn(async move {
            let mut interval_timer = tokio::time::interval(interval);
            interval_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                interval_timer.tick().await;

                // Check if still connected
                let conn_guard = connection.read().await;
                if let Some(conn) = conn_guard.as_ref() {
                    // Send heartbeat
                    let heartbeat_msg = ClientMessage::Heartbeat {
                        client_id: client_id.clone(),
                    };

                    if conn.send(heartbeat_msg).await.is_ok() {
                        // Update last activity timestamp
                        let mut state_guard = state.write().await;
                        state_guard.touch();
                    } else {
                        // Heartbeat failed, connection likely lost
                        break;
                    }
                } else {
                    // No connection, stop heartbeat task
                    break;
                }
            }
        });

        *task_guard = Some(handle);
    }

    /// Initialize file watcher for hybrid mode (internal)
    #[inline]
    async fn _init_file_watcher_impl(&self) -> Result<()> {
        let watcher = VariableFileWatcher::new(None).await?;
        let watcher = Arc::new(watcher);
        watcher.start_watching().await?;
        *self.file_watcher.write().await = Some(watcher);
        Ok(())
    }

    /// Initialize file watcher for hybrid mode (public - for testing/special cases)
    pub(crate) async fn init_file_watcher(&self) -> Result<()> {
        self._init_file_watcher_impl().await
    }

    /// Get or create a virtual variable file for a service
    ///
    /// This creates a virtual representation that works seamlessly for both:
    /// - Local clients: Memory-mapped to actual service file
    /// - Remote clients: In-memory buffer synced via WSS
    pub(crate) async fn get_virtual_service_file(
        &self,
        tenant_id: &str,
        service_name: &str,
    ) -> Result<Arc<VirtualVariableFile>> {
        let service_id = format!("{}_{}", tenant_id, service_name);

        // Check if already loaded
        {
            let vfiles = self.virtual_files.read().await;
            if let Some(vf) = vfiles.get(&service_id) {
                return Ok(Arc::clone(vf));
            }
        }

        // Create new virtual file
        let vf = Arc::new(VirtualVariableFile::new(
            service_id.clone(),
            service_name.to_string(),
            tenant_id.to_string(),
        ));

        // Register with watcher if available
        if let Some(watcher_guard) = self.file_watcher.read().await.as_ref() {
            watcher_guard
                .register_virtual_file(service_id.clone(), Arc::clone(&vf))
                .await?;
        }

        // Store in cache
        let mut vfiles = self.virtual_files.write().await;
        vfiles.insert(service_id, Arc::clone(&vf));

        Ok(vf)
    }

    /// Start monitoring virtual files for changes (internal)
    ///
    /// This spawns a background task that watches for file changes
    /// and automatically detects which variables have changed using SIMD
    #[inline]
    async fn _start_file_monitoring_impl(&self) -> Result<()> {
        let watcher = self.file_watcher.read().await;
        if watcher.is_some() {
            // Already started in init_file_watcher
            Ok(())
        } else {
            drop(watcher);
            self._init_file_watcher_impl().await
        }
    }

    /// Start monitoring virtual files for changes (public - for testing/special cases)
    ///
    /// This spawns a background task that watches for file changes
    /// and automatically detects which variables have changed using SIMD
    pub(crate) async fn start_file_monitoring(&self) -> Result<()> {
        self._start_file_monitoring_impl().await
    }

    /// Get next file change event (blocks until a file changes)
    pub(crate) async fn wait_for_file_change(
        &self,
    ) -> Result<Option<crate::watcher::FileChangeEvent>> {
        let watcher = self.file_watcher.read().await;
        if let Some(w) = watcher.as_ref() {
            Ok(w.next_change().await)
        } else {
            Err(CommyError::InvalidState(
                "File watcher not initialized. Call start_file_monitoring() first".to_string(),
            ))
        }
    }

    /// Try to get next file change event (non-blocking)
    pub(crate) async fn try_get_file_change(
        &self,
    ) -> Result<Option<crate::watcher::FileChangeEvent>> {
        let watcher = self.file_watcher.read().await;
        if let Some(w) = watcher.as_ref() {
            Ok(w.try_next_change().await)
        } else {
            Err(CommyError::InvalidState(
                "File watcher not initialized. Call start_file_monitoring() first".to_string(),
            ))
        }
    }

    /// Stop file monitoring
    pub(crate) async fn stop_file_monitoring(&self) -> Result<()> {
        if let Some(watcher) = self.file_watcher.write().await.take() {
            watcher.stop_watching().await?;
        }
        Ok(())
    }

    /// Inject a pre-built `Connection` into the client (for unit testing without a real server).
    ///
    /// Sets the connection field **and** advances `connection_state` to `Connected` so
    /// subsequent permission guards behave correctly.
    #[cfg(test)]
    pub async fn inject_connection_for_test(&self, conn: Connection) {
        let mut c = self.connection.write().await;
        *c = Some(conn);
        let mut state = self.state.write().await;
        state.connection_state = ConnectionState::Connected;
    }

    /// Inject an authenticated tenant context (no real server auth required).
    #[cfg(test)]
    pub async fn inject_auth_for_test(&self, tenant_id: &str) {
        let mut state = self.state.write().await;
        state.add_auth_context(
            tenant_id.to_string(),
            AuthContext::new(
                tenant_id.to_string(),
                vec!["read".to_string(), "write".to_string()],
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = Client::new("wss://localhost:9000");
        assert!(!client.id().is_empty());
        assert_eq!(client.server_url(), "wss://localhost:9000");
    }

    #[test]
    fn test_client_with_custom_id() {
        let client = Client::with_id("wss://localhost:9000", "my_client");
        assert_eq!(client.id(), "my_client");
    }

    #[tokio::test]
    async fn test_is_connected_initially_false() {
        let client = Client::new("wss://localhost:9000");
        assert!(!client.is_connected().await);
    }

    #[tokio::test]
    async fn test_idle_seconds() {
        let client = Client::new("wss://localhost:9000");
        let idle = client.idle_seconds().await;
        assert!(idle < 2);
    }

    #[tokio::test]
    async fn test_connection_state_initially_disconnected() {
        let client = Client::new("wss://localhost:9000");
        assert_eq!(
            client.connection_state().await,
            ConnectionState::Disconnected
        );
    }

    #[tokio::test]
    async fn test_authenticated_tenants_initially_empty() {
        let client = Client::new("wss://localhost:9000");
        let tenants = client.authenticated_tenants().await;
        assert!(tenants.is_empty());
    }

    #[tokio::test]
    async fn test_is_authenticated_to_false() {
        let client = Client::new("wss://localhost:9000");
        assert!(!client.is_authenticated_to("any_tenant").await);
    }

    #[test]
    fn test_client_server_url() {
        let client = Client::new("wss://example.com:9000");
        assert_eq!(client.server_url(), "wss://example.com:9000");
    }

    // ─── File watcher methods ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_init_file_watcher_succeeds() {
        let client = Client::new("wss://localhost:9000");
        let result = client.init_file_watcher().await;
        assert!(
            result.is_ok(),
            "init_file_watcher should succeed: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_start_file_monitoring_succeeds_and_is_idempotent() {
        let client = Client::new("wss://localhost:9000");
        // First call initialises the watcher
        let r1 = client.start_file_monitoring().await;
        assert!(
            r1.is_ok(),
            "first start_file_monitoring should succeed: {:?}",
            r1
        );
        // Second call is a no-op (watcher already exists)
        let r2 = client.start_file_monitoring().await;
        assert!(
            r2.is_ok(),
            "second start_file_monitoring should also succeed: {:?}",
            r2
        );
        let _ = client.stop_file_monitoring().await;
    }

    #[tokio::test]
    async fn test_wait_for_file_change_uninitialized_returns_err() {
        let client = Client::new("wss://localhost:9000");
        // Watcher not initialised — should return Err(InvalidState)
        let result = client.wait_for_file_change().await;
        assert!(
            matches!(result, Err(CommyError::InvalidState(_))),
            "expected InvalidState error, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_try_get_file_change_uninitialized_returns_err() {
        let client = Client::new("wss://localhost:9000");
        let result = client.try_get_file_change().await;
        assert!(
            matches!(result, Err(CommyError::InvalidState(_))),
            "expected InvalidState error when watcher not initialised, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_try_get_file_change_initialized_returns_ok_none() {
        let client = Client::new("wss://localhost:9000");
        client.init_file_watcher().await.unwrap();
        // No file changes have occurred, so the channel should be empty
        let result = client.try_get_file_change().await;
        assert!(
            matches!(result, Ok(None)),
            "expected Ok(None) with no pending events, got {:?}",
            result
        );
        let _ = client.stop_file_monitoring().await;
    }

    #[tokio::test]
    async fn test_stop_file_monitoring_clears_watcher() {
        let client = Client::new("wss://localhost:9000");
        client.init_file_watcher().await.unwrap();

        // Watcher is live — non-blocking peek should be Ok(None)
        assert!(matches!(client.try_get_file_change().await, Ok(None)));

        // Stop the watcher
        let stop_result = client.stop_file_monitoring().await;
        assert!(
            stop_result.is_ok(),
            "stop_file_monitoring should succeed: {:?}",
            stop_result
        );

        // After stopping, the watcher slot is None ⇒ try_get returns InvalidState
        let after = client.try_get_file_change().await;
        assert!(
            matches!(after, Err(CommyError::InvalidState(_))),
            "expected InvalidState after stopping, got {:?}",
            after
        );
    }

    #[tokio::test]
    async fn test_get_virtual_service_file_creates_and_caches() {
        let client = Client::new("wss://localhost:9000");

        // First call creates the virtual file
        let vf1 = client
            .get_virtual_service_file("tenant_a", "my_service")
            .await
            .expect("should create virtual service file");

        // Second call with same args returns the cached instance (same Arc pointer)
        let vf2 = client
            .get_virtual_service_file("tenant_a", "my_service")
            .await
            .expect("should return cached virtual service file");

        assert!(
            Arc::ptr_eq(&vf1, &vf2),
            "second call should return the cached VirtualVariableFile"
        );

        // Different service name → different file
        let vf3 = client
            .get_virtual_service_file("tenant_a", "other_service")
            .await
            .expect("should create a separate virtual service file");

        assert!(
            !Arc::ptr_eq(&vf1, &vf3),
            "different service should be a separate file"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // Auth pre-check guard tests
    // ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_create_service_not_authenticated_returns_permission_denied() {
        let client = Client::new("wss://localhost:9999");
        // No authentication → must fail immediately with PermissionDenied
        let result = client.create_service("tenant_a", "my_service", 65536).await;
        assert!(
            result.is_err(),
            "create_service must fail when not authenticated"
        );
        match result.unwrap_err() {
            crate::error::CommyError::PermissionDenied(_) => {}
            e => panic!("Expected PermissionDenied, got {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_get_service_not_authenticated_returns_permission_denied() {
        let client = Client::new("wss://localhost:9999");
        let result = client.get_service("tenant_a", "my_service").await;
        assert!(
            result.is_err(),
            "get_service must fail when not authenticated"
        );
        match result.unwrap_err() {
            crate::error::CommyError::PermissionDenied(_) => {}
            e => panic!("Expected PermissionDenied, got {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_delete_service_not_authenticated_returns_permission_denied() {
        let client = Client::new("wss://localhost:9999");
        let result = client.delete_service("tenant_a", "my_service").await;
        assert!(
            result.is_err(),
            "delete_service must fail when not authenticated"
        );
        match result.unwrap_err() {
            crate::error::CommyError::PermissionDenied(_) => {}
            e => panic!("Expected PermissionDenied, got {:?}", e),
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // audit-2: every operation that needs a session refuses before sending
    // ─────────────────────────────────────────────────────────────────────────

    /// An unauthenticated client with a live connection must not put the request on the wire.
    async fn unauthenticated_client_with_connection() -> (
        Client,
        tokio::sync::mpsc::UnboundedReceiver<crate::message::ClientMessage>,
        tokio::sync::mpsc::UnboundedSender<crate::message::ServerMessage>,
    ) {
        let client = Client::new("wss://test");
        let (conn, server_tx, client_rx) = crate::connection::Connection::new_for_test();
        client.inject_connection_for_test(conn).await;
        (client, client_rx, server_tx)
    }

    #[tokio::test]
    async fn test_unauthenticated_tenant_operations_are_refused_and_send_nothing() {
        let (client, mut sent, _server_tx) = unauthenticated_client_with_connection().await;

        let results: Vec<(&str, Result<()>)> = vec![
            (
                "create_tenant",
                client.create_tenant("t", "T").await.map(|_| ()),
            ),
            ("delete_tenant", client.delete_tenant("t").await),
        ];
        for (name, result) in results {
            match result {
                Err(CommyError::PermissionDenied(_)) => {}
                other => panic!(
                    "{name} without authentication: expected PermissionDenied, got {other:?}"
                ),
            }
        }
        assert!(
            sent.try_recv().is_err(),
            "an unauthenticated client put a message on the wire"
        );
    }

    /// Positive control for the test above: once a tenant is authenticated the call is sent.
    #[tokio::test]
    async fn test_authenticated_create_tenant_is_sent() {
        let (client, mut sent, _server_tx) = unauthenticated_client_with_connection().await;
        client.inject_auth_for_test("t").await;

        // No reply is queued, so the call times out waiting; what matters is that it was sent.
        let _ =
            tokio::time::timeout(Duration::from_millis(200), client.create_tenant("t", "T")).await;
        assert!(
            matches!(
                sent.try_recv(),
                Ok(crate::message::ClientMessage::CreateTenant { .. })
            ),
            "create_tenant was not sent"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Bug #14 regression tests: server Error response must NOT be silently
    // converted to Timeout.  These verify the match-based response handlers.
    // ─────────────────────────────────────────────────────────────────────────

    /// Helper: creates a Client with injected mock connection that immediately
    /// returns `response` when the next recv() is called.
    /// Returns `(client, _client_rx)` — the caller MUST keep `_client_rx` alive
    /// for the duration of the test, otherwise the sender channel closes and
    /// `send_message` will fail with `ChannelError`.
    async fn setup_client_with_mock_response(
        tenant_id: &str,
        response: crate::message::ServerMessage,
    ) -> (
        Client,
        tokio::sync::mpsc::UnboundedReceiver<crate::message::ClientMessage>,
    ) {
        let client = Client::new("wss://test");
        let (conn, server_tx, client_rx) = crate::connection::Connection::new_for_test();
        // Pre-load the server-side response channel BEFORE injecting
        server_tx.send(response).expect("pre-send failed");
        client.inject_auth_for_test(tenant_id).await;
        client.inject_connection_for_test(conn).await;
        (client, client_rx)
    }

    /// #14: create_service receiving Error{NotFound} must return Err(NotFound),
    /// NOT Err(Timeout) as was the bug.
    #[tokio::test]
    async fn test_create_service_server_error_returns_not_found_not_timeout() {
        let err_response = crate::message::ServerMessage::Error {
            code: crate::message::ErrorCode::NotFound,
            message: "service creation failed: not found".to_string(),
            request_id: None,
        };
        let (client, _client_rx) = setup_client_with_mock_response("tenant_a", err_response).await;
        let result = client
            .create_service("tenant_a", "missing_svc", 65536)
            .await;
        assert!(
            result.is_err(),
            "create_service must fail on Error response"
        );
        match result.unwrap_err() {
            CommyError::NotFound(_) => {} // Correct: server error is properly translated
            CommyError::Timeout => panic!(
                "Bug #14 regressed: server Error{{NotFound}} was silently converted to Timeout"
            ),
            e => panic!("Unexpected error: {:?}", e),
        }
    }

    /// #14: get_service receiving Error{PermissionDenied} must return Err(PermissionDenied).
    #[tokio::test]
    async fn test_get_service_server_error_returns_permission_denied_not_timeout() {
        let err_response = crate::message::ServerMessage::Error {
            code: crate::message::ErrorCode::PermissionDenied,
            message: "insufficient permissions".to_string(),
            request_id: None,
        };
        let (client, _client_rx) = setup_client_with_mock_response("tenant_b", err_response).await;
        let result = client.get_service("tenant_b", "svc").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            CommyError::PermissionDenied(_) => {}
            CommyError::Timeout => panic!(
                "Bug #14 regressed: server Error{{PermissionDenied}} was silently converted to Timeout"
            ),
            e => panic!("Unexpected error variant: {:?}", e),
        }
    }

    /// #14: delete_service receiving Error{AlreadyExists} (a non-success error)
    /// must return Err(AlreadyExists), NOT Err(Timeout).
    #[tokio::test]
    async fn test_delete_service_server_error_returns_proper_error_not_timeout() {
        let err_response = crate::message::ServerMessage::Error {
            code: crate::message::ErrorCode::AlreadyExists,
            message: "unexpected".to_string(),
            request_id: None,
        };
        let (client, _client_rx) = setup_client_with_mock_response("tenant_c", err_response).await;
        let result = client.delete_service("tenant_c", "svc").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            CommyError::AlreadyExists(_) => {}
            CommyError::Timeout => panic!(
                "Bug #14 regressed: server Error{{AlreadyExists}} was silently converted to Timeout"
            ),
            e => panic!("Unexpected error variant: {:?}", e),
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Authentication edge-case tests
    // ─────────────────────────────────────────────────────────────────────────

    /// #2 (SDK): authenticate where server responds with success=false must
    /// return Err(AuthenticationFailed), not Err(Timeout).
    #[tokio::test]
    async fn test_authenticate_success_false_returns_auth_failed() {
        let client = Client::new("wss://test");
        let (conn, server_tx, mut client_rx) = crate::connection::Connection::new_for_test();
        // Answer the Authenticate that arrives, naming its request.
        tokio::spawn(async move {
            let Some(crate::message::ClientMessage::Authenticate(a)) = client_rx.recv().await
            else {
                panic!("expected Authenticate");
            };
            server_tx
                .send(crate::message::ServerMessage::AuthenticationResult(
                    commy_wire::AuthenticationResult {
                        request_id: a.request_id,
                        success: false,
                        message: "invalid credentials".to_string(),
                        server_version: "0.1.0".to_string(),
                        client_id: None,
                        permissions: None,
                        expires_in_seconds: None,
                    },
                ))
                .expect("reply");
        });
        client.inject_connection_for_test(conn).await;

        let creds = crate::message::AuthCredentials::ApiKey {
            key: "bad_key".into(),
        };
        let result = client.authenticate("tenant_a", creds).await;

        assert!(result.is_err(), "authenticate must fail when success=false");
        match result.unwrap_err() {
            CommyError::AuthenticationFailed(msg) => {
                assert!(
                    msg.contains("invalid credentials"),
                    "Error message should propagate: {}",
                    msg
                );
            }
            CommyError::Timeout => {
                panic!("authenticate success=false was silently converted to Timeout")
            }
            e => panic!("Unexpected error: {:?}", e),
        }
    }

    /// authenticate receiving Error{Unauthorized} returns Err(Unauthorized).
    #[tokio::test]
    async fn test_authenticate_error_response_returns_unauthorized() {
        let client = Client::new("wss://test");
        let (conn, server_tx, _client_rx) = crate::connection::Connection::new_for_test();
        server_tx
            .send(crate::message::ServerMessage::Error {
                code: crate::message::ErrorCode::Unauthorized,
                message: "bad token".to_string(),
                request_id: None,
            })
            .expect("pre-send failed");
        client.inject_connection_for_test(conn).await;

        let creds = crate::message::AuthCredentials::Jwt {
            token: "expired-token".into(),
        };
        let result = client.authenticate("tenant_a", creds).await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), CommyError::Unauthorized(_)),
            "Expected Unauthorized"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Connection lifecycle tests
    // ─────────────────────────────────────────────────────────────────────────

    /// #5: disconnect() must clear both the connection and all auth contexts.
    #[tokio::test]
    async fn test_disconnect_clears_connection_and_auth() {
        let client = Client::new("wss://test");
        let (conn, _server_tx, _client_rx) = crate::connection::Connection::new_for_test();
        client.inject_auth_for_test("tenant_a").await;
        client.inject_connection_for_test(conn).await;

        // Preconditions
        assert!(
            client.is_connected().await,
            "must be connected before disconnect"
        );
        assert!(
            client.is_authenticated_to("tenant_a").await,
            "must be authenticated before disconnect"
        );

        let result = client.disconnect().await;
        assert!(result.is_ok(), "disconnect should succeed: {:?}", result);

        // Post-conditions
        assert!(
            !client.is_connected().await,
            "connection must be cleared after disconnect"
        );
        assert!(
            !client.is_authenticated_to("tenant_a").await,
            "auth must be cleared after disconnect"
        );
        assert!(
            client.authenticated_tenants().await.is_empty(),
            "all auth contexts must be removed after disconnect"
        );
        assert_eq!(
            client.connection_state().await,
            crate::connection::ConnectionState::Disconnected,
            "state must be Disconnected after disconnect"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Bug #15 documentation test: concurrent CRUD methods share a single
    // receiver, so interleaved responses can be mismatched.
    // ─────────────────────────────────────────────────────────────────────────

    /// #15: Demonstrates the concurrent receiver race: if two tasks call
    /// create_service simultaneously on the SAME client instance, both share
    /// the same Arc<RwLock<Receiver>>.  The first recv() winner gets the only
    /// response; the loser sees Timeout or a mis-attributed response.
    ///
    /// This test documents the KNOWN limitation — it is NOT asserting correct
    /// behaviour but that the race DOES produce observable mis-routing.
    #[tokio::test]
    async fn test_concurrent_create_service_receiver_race_documented() {
        use std::sync::Arc;

        let client = Arc::new(Client::new("wss://test"));
        let (conn, server_tx, _client_rx) = crate::connection::Connection::new_for_test();

        // Only one Service response in the channel — two tasks will race for it
        server_tx
            .send(crate::message::ServerMessage::Service {
                service_id: "svc-1".to_string(),
                service_name: "svc_name".to_string(),
                tenant_id: "t1".to_string(),
                file_path: None,
            })
            .expect("pre-send one response");

        client.inject_auth_for_test("t1").await;
        client.inject_connection_for_test(conn).await;

        let c1 = Arc::clone(&client);
        let c2 = Arc::clone(&client);

        // Spawn both concurrently — only one can win the recv()
        let (r1, r2) = tokio::join!(
            tokio::spawn(async move { c1.create_service("t1", "svc_a", 65536).await }),
            tokio::spawn(async move { c2.create_service("t1", "svc_b", 65536).await }),
        );

        let r1 = r1.expect("task 1 panicked");
        let r2 = r2.expect("task 2 panicked");

        // Exactly ONE task should get the single response; the other times out
        // or gets an error.  Both succeeding simultaneously would indicate a
        // bug (response duplication).
        let successes = [r1.is_ok(), r2.is_ok()].iter().filter(|&&b| b).count();
        assert!(
            successes <= 1,
            "Both concurrent create_service calls succeeded with a single server response — \
             this indicates response duplication which is a serious protocol bug. \
             Expected at most 1 success."
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // "Not connected" early-return tests
    // Uses start_paused = true so the 1-second reconnect delay is instant.
    // ─────────────────────────────────────────────────────────────────────────

    #[tokio::test(start_paused = true)]
    async fn test_heartbeat_when_disconnected_returns_connection_lost() {
        let client = Client::new("wss://localhost:9999");
        let result = client.heartbeat().await;
        assert!(result.is_err(), "heartbeat with no connection must fail");
        assert!(
            matches!(result.unwrap_err(), CommyError::ConnectionLost(_)),
            "Expected ConnectionLost"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // create_tenant / delete_tenant error response paths
    // ─────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_create_tenant_success_false_returns_permission_denied() {
        let (client, _client_rx) = setup_client_with_mock_response(
            "tenant_x",
            crate::message::ServerMessage::TenantResult {
                success: false,
                tenant_id: "t1".to_string(),
                message: "not allowed".to_string(),
            },
        )
        .await;
        let result = client.create_tenant("t1", "Tenant One").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            CommyError::PermissionDenied(_) => {}
            e => panic!("Expected PermissionDenied, got {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_create_tenant_error_response_returns_unauthorized() {
        let (client, _client_rx) = setup_client_with_mock_response(
            "tenant_x",
            crate::message::ServerMessage::Error {
                code: crate::message::ErrorCode::Unauthorized,
                message: "invalid credentials".to_string(),
                request_id: None,
            },
        )
        .await;
        let result = client.create_tenant("t1", "Tenant One").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            CommyError::Unauthorized(_) => {}
            CommyError::Timeout => {
                panic!("Bug: Error{{Unauthorized}} was silently converted to Timeout")
            }
            e => panic!("Expected Unauthorized, got {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_delete_tenant_success_false_returns_permission_denied() {
        let (client, _client_rx) = setup_client_with_mock_response(
            "tenant_x",
            crate::message::ServerMessage::Result {
                request_id: "r1".to_string(),
                success: false,
                message: "not allowed to delete".to_string(),
            },
        )
        .await;
        let result = client.delete_tenant("t1").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            CommyError::PermissionDenied(_) => {}
            e => panic!("Expected PermissionDenied, got {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_delete_tenant_error_not_found_returns_not_found() {
        let (client, _client_rx) = setup_client_with_mock_response(
            "tenant_x",
            crate::message::ServerMessage::Error {
                code: crate::message::ErrorCode::NotFound,
                message: "tenant does not exist".to_string(),
                request_id: None,
            },
        )
        .await;
        let result = client.delete_tenant("t1").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            CommyError::NotFound(_) => {}
            CommyError::Timeout => panic!("Bug: Error{{NotFound}} converted to Timeout"),
            e => panic!("Expected NotFound, got {:?}", e),
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // authenticate success \u2192 state transitions
    // ─────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_authenticate_success_transitions_to_authenticated_state() {
        let client = Client::new("wss://test");
        let (conn, server_tx, mut client_rx) = crate::connection::Connection::new_for_test();
        // Pre-load a successful AuthenticationResult
        // Answer the Authenticate that arrives, naming its request.
        tokio::spawn(async move {
            let Some(crate::message::ClientMessage::Authenticate(a)) = client_rx.recv().await
            else {
                panic!("expected Authenticate");
            };
            server_tx
                .send(crate::message::ServerMessage::AuthenticationResult(
                    commy_wire::AuthenticationResult {
                        request_id: a.request_id,
                        success: true,
                        message: "Welcome".to_string(),
                        server_version: "1.0.0".to_string(),
                        client_id: None,
                        permissions: Some(vec!["read".to_string(), "write".to_string()]),
                        expires_in_seconds: None,
                    },
                ))
                .expect("reply");
        });
        client.inject_connection_for_test(conn).await;

        let creds = crate::message::AuthCredentials::ApiKey {
            key: "test-key".into(),
        };
        let result = client.authenticate("tenant_a", creds).await;
        assert!(result.is_ok(), "authenticate must succeed: {:?}", result);

        // State must be Authenticated
        assert_eq!(
            client.connection_state().await,
            crate::connection::ConnectionState::Authenticated,
            "connection_state must be Authenticated after successful authenticate()"
        );

        // Tenant must be in the list
        assert!(
            client.is_authenticated_to("tenant_a").await,
            "is_authenticated_to must return true for the authenticated tenant"
        );
        assert!(
            client
                .authenticated_tenants()
                .await
                .contains(&"tenant_a".to_string()),
            "authenticated_tenants() must list the tenant"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Helper: inject a connection with no pre-loaded response.
    // Useful for write_variable / subscribe / unsubscribe / heartbeat, which
    // either don't wait for a response or handle a missing response gracefully.
    // ─────────────────────────────────────────────────────────────────────────
    async fn client_with_injected_connection() -> (
        Client,
        tokio::sync::mpsc::UnboundedReceiver<crate::message::ClientMessage>,
    ) {
        let client = Client::new("wss://test");
        let (conn, _server_tx, client_rx) = crate::connection::Connection::new_for_test();
        // _server_tx dropped intentionally: these tests never pre-load a response.
        // conn.recv() returns None (channel closed), which heartbeat handles gracefully.
        client.inject_auth_for_test("tenant_a").await;
        client.inject_connection_for_test(conn).await;
        (client, client_rx)
    }

    // ── write_variable / subscribe / unsubscribe success ─────────────────────

    // ── heartbeat success ─────────────────────────────────────────────────────

    #[tokio::test(start_paused = true)]
    async fn test_heartbeat_with_connection_returns_ok() {
        // heartbeat() calls send_message then waits up to 10 s for a reply.
        // The mock channel has no response pre-loaded, so conn.recv() returns
        // None (closed channel). heartbeat() handles this gracefully and still
        // returns Ok(()).  start_paused makes the 10-s timeout instant.
        let (client, _rx) = client_with_injected_connection().await;
        let result = client.heartbeat().await;
        assert!(
            result.is_ok(),
            "heartbeat with connection must return Ok: {:?}",
            result
        );
    }

    // ── success paths for methods that wait for a server response ─────────────

    #[tokio::test]
    async fn test_create_service_success_returns_id() {
        let (client, _rx) = setup_client_with_mock_response(
            "tenant_x",
            crate::message::ServerMessage::Service {
                service_id: "svc-abc-123".to_string(),
                service_name: "my_service".to_string(),
                tenant_id: "tenant_x".to_string(),
                file_path: None,
            },
        )
        .await;
        let result = client.create_service("tenant_x", "my_service", 65536).await;
        assert!(result.is_ok(), "create_service must succeed: {:?}", result);
        assert_eq!(result.unwrap(), "svc-abc-123");
    }

    #[tokio::test]
    async fn test_get_service_success_returns_service() {
        let (client, _rx) = setup_client_with_mock_response(
            "tenant_x",
            crate::message::ServerMessage::Service {
                service_id: "svc-xyz".to_string(),
                service_name: "config".to_string(),
                tenant_id: "tenant_x".to_string(),
                file_path: Some("/data/svc.mem".to_string()),
            },
        )
        .await;
        let result = client.get_service("tenant_x", "config").await;
        assert!(result.is_ok(), "get_service must succeed: {:?}", result);
        let svc = result.unwrap();
        assert_eq!(svc.id, "svc-xyz");
        assert_eq!(svc.name, "config");
        assert_eq!(svc.tenant_id, "tenant_x");
        assert_eq!(svc.file_path(), Some("/data/svc.mem"));
    }

    #[tokio::test]
    async fn test_delete_service_success_returns_ok() {
        let (client, _rx) = setup_client_with_mock_response(
            "tenant_x",
            crate::message::ServerMessage::Result {
                request_id: "r1".to_string(),
                success: true,
                message: "deleted".to_string(),
            },
        )
        .await;
        let result = client.delete_service("tenant_x", "old_svc").await;
        assert!(result.is_ok(), "delete_service must succeed: {:?}", result);
    }

    #[tokio::test]
    async fn test_create_tenant_success_returns_tenant_id() {
        let (client, _rx) = setup_client_with_mock_response(
            "tenant_x",
            crate::message::ServerMessage::TenantResult {
                success: true,
                tenant_id: "new_tenant".to_string(),
                message: "created".to_string(),
            },
        )
        .await;
        let result = client.create_tenant("new_tenant", "New Tenant").await;
        assert!(result.is_ok(), "create_tenant must succeed: {:?}", result);
        assert_eq!(result.unwrap(), "new_tenant");
    }

    #[tokio::test]
    async fn test_delete_tenant_success_returns_ok() {
        let (client, _rx) = setup_client_with_mock_response(
            "tenant_x",
            crate::message::ServerMessage::Result {
                request_id: "r1".to_string(),
                success: true,
                message: "tenant deleted".to_string(),
            },
        )
        .await;
        let result = client.delete_tenant("old_tenant").await;
        assert!(result.is_ok(), "delete_tenant must succeed: {:?}", result);
    }

    // ── outgoing message field verification ───────────────────────────────────
}

#[cfg(test)]
mod negotiate_tests {
    use super::*;
    use crate::message::{ClientMessage, ServerMessage};
    use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

    struct Rig {
        client: Client,
        to_client: UnboundedSender<ServerMessage>,
        from_client: UnboundedReceiver<ClientMessage>,
    }

    async fn rig() -> Rig {
        let (conn, to_client, from_client) = Connection::new_for_test();
        let client = Client::new("wss://localhost:9000");
        client.inject_connection_for_test(conn).await;
        Rig {
            client,
            to_client,
            from_client,
        }
    }

    fn offer(rid: &str, path: Option<&str>, nonce_path: Option<&str>) -> ServerMessage {
        ServerMessage::ServiceFilePath(commy_wire::ServiceFilePath {
            request_id: rid.to_string(),
            service_id: "svc".to_string(),
            file_id: 9,
            path: path.map(str::to_string),
            nonce_path: nonce_path.map(str::to_string),
        })
    }

    fn files(dir: &std::path::Path) -> (String, String) {
        let svc = dir.join("svc.bin");
        let non = dir.join("nonce");
        std::fs::write(&svc, [0u8; 4096]).unwrap();
        std::fs::write(&non, "n0nce-value\n").unwrap();
        (
            svc.to_string_lossy().into_owned(),
            non.to_string_lossy().into_owned(),
        )
    }

    async fn first_request_id(rx: &mut UnboundedReceiver<ClientMessage>) -> String {
        match rx.recv().await.unwrap() {
            ClientMessage::GetServiceFilePath(g) => {
                assert_eq!(g.service_id, "svc");
                g.request_id
            }
            other => panic!("expected GetServiceFilePath, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn no_offer_means_wss_with_file_id_and_no_confirm_sent() {
        let Rig {
            client,
            to_client,
            mut from_client,
        } = rig().await;
        let fut = client.negotiate_local("svc");
        let drive = async {
            let rid = first_request_id(&mut from_client).await;
            to_client.send(offer(&rid, None, None)).unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        let n = res.unwrap();
        assert_eq!(n.file_id, 9);
        assert!(n.local.is_none());
        assert!(from_client.try_recv().is_err(), "no ConfirmLocal expected");
    }

    #[tokio::test]
    async fn open_remote_never_confirms_a_local_mapping_it_was_offered() {
        // Confirming would make the server treat this client as a local mapper and refuse its range
        // frames, so a remote mirror must go straight to Resync.
        let d = tempfile::tempdir().unwrap();
        let (svc, non) = files(d.path());
        let Rig {
            client,
            to_client,
            mut from_client,
        } = rig().await;
        let fut = client.open_remote_with_timeout("svc", Duration::from_millis(300));
        let drive = async {
            let rid = first_request_id(&mut from_client).await;
            to_client.send(offer(&rid, Some(&svc), Some(&non))).unwrap();
            assert!(
                matches!(from_client.recv().await.unwrap(), ClientMessage::Resync(_)),
                "open_remote must not send ConfirmLocal"
            );
        };
        let (res, ()) = tokio::join!(fut, drive);
        assert!(res.is_err(), "no ResyncDone was served");
    }

    #[tokio::test]
    async fn offer_with_matching_nonce_and_ack_enters_local_mode() {
        let d = tempfile::tempdir().unwrap();
        let (svc, non) = files(d.path());
        let Rig {
            client,
            to_client,
            mut from_client,
        } = rig().await;
        let fut = client.negotiate_local("svc");
        let drive = async {
            let rid = first_request_id(&mut from_client).await;
            to_client.send(offer(&rid, Some(&svc), Some(&non))).unwrap();
            match from_client.recv().await.unwrap() {
                ClientMessage::ConfirmLocal(c) => {
                    assert_eq!(c.file_id, 9);
                    assert_eq!(c.nonce, "n0nce-value");
                    to_client
                        .send(ServerMessage::ConfirmLocalAck(
                            commy_wire::ConfirmLocalAck {
                                request_id: c.request_id,
                                file_id: 9,
                            },
                        ))
                        .unwrap();
                }
                other => panic!("expected ConfirmLocal, got {other:?}"),
            }
        };
        let (res, ()) = tokio::join!(fut, drive);
        let n = res.unwrap();
        assert_eq!(n.file_id, 9);
        assert_eq!(n.local.expect("local mode").len(), 4096);
    }

    #[tokio::test]
    async fn error_reply_to_confirm_falls_back_to_wss() {
        let d = tempfile::tempdir().unwrap();
        let (svc, non) = files(d.path());
        let Rig {
            client,
            to_client,
            mut from_client,
        } = rig().await;
        let fut = client.negotiate_local("svc");
        let drive = async {
            let rid = first_request_id(&mut from_client).await;
            to_client.send(offer(&rid, Some(&svc), Some(&non))).unwrap();
            let _confirm = from_client.recv().await.unwrap();
            to_client
                .send(ServerMessage::Error {
                    code: crate::message::ErrorCode::PermissionDenied,
                    message: "bad nonce".to_string(),
                    request_id: None,
                })
                .unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        let n = res.unwrap();
        assert_eq!(n.file_id, 9);
        assert!(n.local.is_none());
    }

    #[tokio::test]
    async fn unopenable_file_falls_back_without_sending_confirm() {
        let d = tempfile::tempdir().unwrap();
        let (_svc, non) = files(d.path());
        let absent = d.path().join("absent.bin").to_string_lossy().into_owned();
        let Rig {
            client,
            to_client,
            mut from_client,
        } = rig().await;
        let fut = client.negotiate_local("svc");
        let drive = async {
            let rid = first_request_id(&mut from_client).await;
            to_client
                .send(offer(&rid, Some(&absent), Some(&non)))
                .unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        assert!(res.unwrap().local.is_none());
        assert!(from_client.try_recv().is_err(), "no ConfirmLocal expected");
    }

    #[tokio::test]
    async fn path_without_nonce_path_is_wss() {
        let d = tempfile::tempdir().unwrap();
        let (svc, _non) = files(d.path());
        let Rig {
            client,
            to_client,
            mut from_client,
        } = rig().await;
        let fut = client.negotiate_local("svc");
        let drive = async {
            let rid = first_request_id(&mut from_client).await;
            to_client.send(offer(&rid, Some(&svc), None)).unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        assert!(res.unwrap().local.is_none());
        assert!(from_client.try_recv().is_err());
    }

    #[tokio::test]
    async fn reply_with_another_request_id_is_not_accepted() {
        let Rig {
            client,
            to_client,
            mut from_client,
        } = rig().await;
        let fut = client.negotiate_local_with_timeout("svc", Duration::from_millis(200), true);
        let drive = async {
            let _rid = first_request_id(&mut from_client).await;
            to_client.send(offer("someone-else", None, None)).unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        assert!(matches!(res, Err(CommyError::Timeout)), "{res:?}");
    }

    #[tokio::test]
    async fn error_reply_to_the_request_is_an_error() {
        let Rig {
            client,
            to_client,
            mut from_client,
        } = rig().await;
        let fut = client.negotiate_local("svc");
        let drive = async {
            let _rid = first_request_id(&mut from_client).await;
            to_client
                .send(ServerMessage::Error {
                    code: crate::message::ErrorCode::NotFound,
                    message: "no such service".to_string(),
                    request_id: None,
                })
                .unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        assert!(matches!(res, Err(CommyError::NotFound(_))), "{res:?}");
    }

    #[tokio::test]
    async fn not_connected_is_connection_lost() {
        let client = Client::new("wss://localhost:9000");
        let res = client.negotiate_local("svc").await;
        assert!(matches!(res, Err(CommyError::ConnectionLost(_))), "{res:?}");
    }
}

#[cfg(test)]
mod shared_tests {
    use super::*;
    use crate::local_map::LocalMap;
    use crate::message::{ClientMessage, ServerMessage};
    use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

    async fn rig() -> (
        Client,
        UnboundedSender<ServerMessage>,
        UnboundedReceiver<ClientMessage>,
        tempfile::TempDir,
    ) {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("svc.bin");
        let n = d.path().join("n");
        std::fs::write(&f, vec![0u8; 8192]).unwrap();
        std::fs::write(&n, "x").unwrap();
        let LocalMapOutcome::Local(m) = LocalMap::open(&f, &n) else {
            panic!("local mode expected");
        };
        let (conn, to_client, from_client) = Connection::new_for_test();
        let client = Client::new("wss://localhost:9000");
        client.inject_connection_for_test(conn).await;
        client
            .shared
            .write()
            .await
            .insert("svc".to_string(), SharedFile::new(*m, 5));
        (client, to_client, from_client, d)
    }

    fn ack(rid: &str, seq: u64) -> ServerMessage {
        ServerMessage::FlushAck(commy_wire::FlushAck {
            request_id: rid.to_string(),
            file_id: 5,
            seq,
        })
    }

    #[tokio::test]
    async fn shared_read_returns_what_was_written_and_refuses_outside_the_file() {
        let (client, _to, _from, _d) = rig().await;
        client.shared_write("svc", 10, b"abc").await.unwrap();
        assert_eq!(
            client.shared_read("svc", 9, 5).await.unwrap(),
            [0, b'a', b'b', b'c', 0]
        );
        assert!(matches!(
            client.shared_read("svc", 8190, 4).await,
            Err(CommyError::InvalidOffset(_))
        ));
        assert!(matches!(
            client.shared_read("svc", u64::MAX, 2).await,
            Err(CommyError::InvalidOffset(_))
        ));
        assert!(matches!(
            client.shared_read("nope", 0, 1).await,
            Err(CommyError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn create_service_sends_the_requested_size() {
        let (client, _to, mut from, _d) = rig().await;
        client.inject_auth_for_test("t").await;
        // No reply is queued, so the call times out; the request is what matters.
        let _ = tokio::time::timeout(
            Duration::from_millis(200),
            client.create_service("t", "svc2", 12345),
        )
        .await;
        match from.recv().await {
            Some(ClientMessage::CreateService { size, .. }) => assert_eq!(size, 12345),
            other => panic!("expected CreateService, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn flush_sends_ranges_only_and_adopts_the_ack_seq() {
        let (client, to_client, mut from_client, _d) = rig().await;
        client.shared_write("svc", 100, &[1, 2, 3]).await.unwrap();
        let fut = client.flush_shared("svc");
        let drive = async {
            let ClientMessage::RangesChanged(r) = from_client.recv().await.unwrap() else {
                panic!("expected RangesChanged");
            };
            assert_eq!(r.file_id, 5);
            assert_eq!(
                r.ranges,
                vec![commy_wire::ByteRange {
                    offset: 100,
                    len: 3
                }]
            );
            to_client.send(ack(&r.request_id, 12)).unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        assert_eq!(res.unwrap(), 12);
        // Nothing changed since: a second flush sends nothing and does not wait.
        assert_eq!(client.flush_shared("svc").await.unwrap(), 0);
        assert!(from_client.try_recv().is_err());
    }

    #[tokio::test]
    async fn flush_without_an_ack_times_out() {
        let (client, _to_client, mut from_client, _d) = rig().await;
        client.shared_write("svc", 0, &[1]).await.unwrap();
        let res = client
            .flush_shared_with_timeout("svc", Duration::from_millis(100))
            .await;
        assert!(matches!(res, Err(CommyError::Timeout)), "{res:?}");
        assert!(from_client.try_recv().is_ok(), "RangesChanged was sent");
    }

    #[tokio::test]
    async fn ack_for_another_request_is_not_accepted() {
        let (client, to_client, mut from_client, _d) = rig().await;
        client.shared_write("svc", 0, &[1]).await.unwrap();
        let fut = client.flush_shared_with_timeout("svc", Duration::from_millis(150));
        let drive = async {
            let _ = from_client.recv().await.unwrap();
            to_client.send(ack("someone-else", 9)).unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        assert!(matches!(res, Err(CommyError::Timeout)), "{res:?}");
    }

    #[tokio::test]
    async fn ranges_updated_during_poll_is_rebaselined_and_not_flushed() {
        let (client, to_client, mut from_client, _d) = rig().await;
        // A foreign process wrote bytes 50..52 of the shared file.
        {
            let mut shared = client.shared.write().await;
            assert!(shared
                .get_mut("svc")
                .unwrap()
                .foreign_write_for_test(50, &[8, 8]));
        }
        to_client
            .send(ServerMessage::RangesUpdated(commy_wire::RangesUpdated {
                file_id: 5,
                seq: 3,
                ranges: vec![commy_wire::ByteRange { offset: 50, len: 2 }],
            }))
            .unwrap();
        assert!(client
            .poll_shared_updates(Duration::from_millis(200))
            .await
            .unwrap());
        assert_eq!(client.flush_shared("svc").await.unwrap(), 0);
        assert!(from_client.try_recv().is_err(), "foreign write echoed back");
    }

    #[tokio::test]
    async fn poll_with_nothing_pending_returns_false() {
        let (client, _to_client, _from_client, _d) = rig().await;
        assert!(!client
            .poll_shared_updates(Duration::from_millis(50))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn unknown_service_is_not_found_and_out_of_bounds_is_invalid_offset() {
        let (client, _t, _f, _d) = rig().await;
        assert!(matches!(
            client.shared_write("nope", 0, &[1]).await,
            Err(CommyError::NotFound(_))
        ));
        assert!(matches!(
            client.flush_shared("nope").await,
            Err(CommyError::NotFound(_))
        ));
        assert!(matches!(
            client.shared_write("svc", 8191, &[1, 2]).await,
            Err(CommyError::InvalidOffset(_))
        ));
    }
}

#[cfg(test)]
mod remote_tests {
    use super::*;
    use crate::message::{ClientMessage, ServerMessage};
    use commy_wire::{RangeFrame, ResyncDone};
    use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

    async fn rig() -> (
        Client,
        UnboundedSender<ServerMessage>,
        UnboundedReceiver<ClientMessage>,
    ) {
        let (conn, to_client, from_client) = Connection::new_for_test();
        let client = Client::new("wss://localhost:9000");
        client.inject_connection_for_test(conn).await;
        (client, to_client, from_client)
    }

    fn offer(rid: &str) -> ServerMessage {
        ServerMessage::ServiceFilePath(commy_wire::ServiceFilePath {
            request_id: rid.to_string(),
            service_id: "svc".to_string(),
            file_id: 4,
            path: None,
            nonce_path: None,
        })
    }

    fn frame(offset: u64, data: &[u8]) -> ServerMessage {
        ServerMessage::Range(RangeFrame {
            file_id: 4,
            seq: 6,
            offset,
            data: data.to_vec(),
        })
    }

    async fn open(
        client: &Client,
        to_client: &UnboundedSender<ServerMessage>,
        from_client: &mut UnboundedReceiver<ClientMessage>,
    ) -> Result<u32> {
        let fut = client.open_remote("svc");
        let drive = async {
            let ClientMessage::GetServiceFilePath(g) = from_client.recv().await.unwrap() else {
                panic!("expected GetServiceFilePath");
            };
            to_client.send(offer(&g.request_id)).unwrap();
            let ClientMessage::Resync(r) = from_client.recv().await.unwrap() else {
                panic!("expected Resync");
            };
            assert_eq!(r.file_id, 4);
            // The snapshot arrives in two frames, then ResyncDone.
            to_client.send(frame(0, &[1, 2, 3, 4])).unwrap();
            to_client.send(frame(4, &[5, 6, 7, 8])).unwrap();
            to_client
                .send(ServerMessage::ResyncDone(ResyncDone {
                    request_id: r.request_id,
                    file_id: 4,
                    seq: 6,
                    size: 8,
                }))
                .unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        res
    }

    #[tokio::test]
    async fn open_remote_builds_the_mirror_from_the_resync_frames() {
        let (client, to_client, mut from_client) = rig().await;
        assert_eq!(
            open(&client, &to_client, &mut from_client).await.unwrap(),
            4
        );
        let shared = client.shared.read().await;
        let f = shared.get("svc").unwrap();
        assert!(!f.is_local());
        assert_eq!(f.view(), &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(f.seq(), 6);
    }

    #[tokio::test]
    async fn flush_remote_sends_frames_then_flush_end_with_base_seq_and_adopts_ack() {
        let (client, to_client, mut from_client) = rig().await;
        open(&client, &to_client, &mut from_client).await.unwrap();
        client.shared_write("svc", 2, &[9, 9]).await.unwrap();
        let fut = client.flush_shared("svc");
        let drive = async {
            let ClientMessage::Range(f) = from_client.recv().await.unwrap() else {
                panic!("expected a range frame first");
            };
            assert_eq!((f.file_id, f.seq, f.offset), (4, 6, 2));
            assert_eq!(f.data, vec![9, 9]);
            let ClientMessage::FlushEnd(e) = from_client.recv().await.unwrap() else {
                panic!("expected FlushEnd after the frames");
            };
            to_client
                .send(ServerMessage::FlushAck(commy_wire::FlushAck {
                    request_id: e.request_id,
                    file_id: 4,
                    seq: 7,
                }))
                .unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        assert_eq!(res.unwrap(), 7);
        assert_eq!(client.flush_shared("svc").await.unwrap(), 0);
        assert!(from_client.try_recv().is_err(), "nothing left to flush");
    }

    #[tokio::test]
    async fn inbound_frame_updates_the_mirror_without_echoing() {
        let (client, to_client, mut from_client) = rig().await;
        open(&client, &to_client, &mut from_client).await.unwrap();
        to_client.send(frame(0, &[42])).unwrap();
        assert!(client
            .poll_shared_updates(Duration::from_millis(200))
            .await
            .unwrap());
        assert_eq!(client.shared.read().await.get("svc").unwrap().view()[0], 42);
        assert_eq!(client.flush_shared("svc").await.unwrap(), 0);
        assert!(from_client.try_recv().is_err(), "inbound bytes echoed back");
    }

    #[tokio::test]
    async fn resync_frame_outside_the_file_is_rejected() {
        let (client, to_client, mut from_client) = rig().await;
        let fut = client.open_remote("svc");
        let drive = async {
            let ClientMessage::GetServiceFilePath(g) = from_client.recv().await.unwrap() else {
                panic!("expected GetServiceFilePath");
            };
            to_client.send(offer(&g.request_id)).unwrap();
            let ClientMessage::Resync(r) = from_client.recv().await.unwrap() else {
                panic!("expected Resync");
            };
            to_client.send(frame(6, &[1, 2, 3, 4])).unwrap();
            to_client
                .send(ServerMessage::ResyncDone(ResyncDone {
                    request_id: r.request_id,
                    file_id: 4,
                    seq: 1,
                    size: 8,
                }))
                .unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        assert!(matches!(res, Err(CommyError::InvalidMessage(_))), "{res:?}");
        assert!(client.shared.read().await.get("svc").is_none());
    }

    #[tokio::test]
    async fn resync_without_done_times_out() {
        let (client, to_client, mut from_client) = rig().await;
        let fut = client.open_remote_with_timeout("svc", Duration::from_millis(150));
        let drive = async {
            let ClientMessage::GetServiceFilePath(g) = from_client.recv().await.unwrap() else {
                panic!("expected GetServiceFilePath");
            };
            to_client.send(offer(&g.request_id)).unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        assert!(matches!(res, Err(CommyError::Timeout)), "{res:?}");
    }
}

#[cfg(test)]
mod rotation_tests {
    use super::*;
    use crate::local_map::LocalMap;
    use crate::message::{ClientMessage, ServerMessage};
    use commy_wire::{
        ByteRange, FlushAck, Loss, LossKind, RotateReason, RotationAccepted, RotationCommit,
        RotationPending,
    };
    use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

    struct Rig {
        client: Client,
        to_client: UnboundedSender<ServerMessage>,
        from_client: UnboundedReceiver<ClientMessage>,
        dir: tempfile::TempDir,
    }

    /// A client with `svc` mapped locally (file id 5, 8 KiB).
    async fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("old.bin");
        let n = dir.path().join("n");
        std::fs::write(&f, vec![0u8; 8192]).unwrap();
        std::fs::write(&n, "x").unwrap();
        let LocalMapOutcome::Local(m) = LocalMap::open(&f, &n) else {
            panic!("local mode expected");
        };
        let (conn, to_client, from_client) = Connection::new_for_test();
        let client = Client::new("wss://localhost:9000");
        client.inject_connection_for_test(conn).await;
        client
            .shared
            .write()
            .await
            .insert("svc".to_string(), SharedFile::new(*m, 5));
        Rig {
            client,
            to_client,
            from_client,
            dir,
        }
    }

    fn pending(rid: &str) -> RotationPending {
        RotationPending {
            rotation_id: rid.to_string(),
            service_id: "svc".to_string(),
            file_id: 5,
            deadline_ms: 400,
            fill_ms: 400,
        }
    }

    fn commit(rid: &str, reason: RotateReason) -> RotationCommit {
        RotationCommit {
            rotation_id: rid.to_string(),
            service_id: "svc".to_string(),
            reason,
            old_file_id: 5,
            new_file_id: 6,
            size: 16384,
        }
    }

    /// Server side of the local handshake for the new file (id 6).
    async fn serve_new_local_file(
        to_client: &UnboundedSender<ServerMessage>,
        from_client: &mut UnboundedReceiver<ClientMessage>,
        dir: &std::path::Path,
        size: usize,
    ) {
        let svc = dir.join("new.bin");
        let non = dir.join("new.nonce");
        std::fs::write(&svc, vec![0u8; size]).unwrap();
        std::fs::write(&non, "fresh-nonce").unwrap();
        let ClientMessage::GetServiceFilePath(g) = from_client.recv().await.unwrap() else {
            panic!("expected GetServiceFilePath");
        };
        to_client
            .send(ServerMessage::ServiceFilePath(
                commy_wire::ServiceFilePath {
                    request_id: g.request_id,
                    service_id: "svc".to_string(),
                    file_id: 6,
                    path: Some(svc.to_string_lossy().into_owned()),
                    nonce_path: Some(non.to_string_lossy().into_owned()),
                },
            ))
            .unwrap();
        let ClientMessage::ConfirmLocal(c) = from_client.recv().await.unwrap() else {
            panic!("expected ConfirmLocal");
        };
        assert_eq!(c.file_id, 6);
        assert_eq!(c.nonce, "fresh-nonce", "a fresh nonce, not the old one");
        to_client
            .send(ServerMessage::ConfirmLocalAck(
                commy_wire::ConfirmLocalAck {
                    request_id: c.request_id,
                    file_id: 6,
                },
            ))
            .unwrap();
    }

    #[tokio::test]
    async fn resize_drain_flushes_then_drains_then_switches_to_the_new_file() {
        let Rig {
            client,
            to_client,
            mut from_client,
            dir,
        } = rig().await;
        client.shared_write("svc", 100, &[1, 2, 3]).await.unwrap();
        let p = pending("rot1");
        let fut = client.drain_for_rotation(&p);
        let drive = async {
            // 1. flush into the OLD file
            let ClientMessage::RangesChanged(r) = from_client.recv().await.unwrap() else {
                panic!("expected the flush first");
            };
            assert_eq!(r.file_id, 5);
            to_client
                .send(ServerMessage::FlushAck(FlushAck {
                    request_id: r.request_id,
                    file_id: 5,
                    seq: 9,
                }))
                .unwrap();
            // 2. drained, with the last flushed seq
            let ClientMessage::RotationDrained(d) = from_client.recv().await.unwrap() else {
                panic!("expected RotationDrained");
            };
            assert_eq!((d.rotation_id.as_str(), d.file_id, d.seq), ("rot1", 5, 9));
            // 3. commit, then the handshake for the new file
            to_client
                .send(ServerMessage::RotationCommit(commit(
                    "rot1",
                    RotateReason::Resize,
                )))
                .unwrap();
            serve_new_local_file(&to_client, &mut from_client, dir.path(), 16384).await;
        };
        let (res, ()) = tokio::join!(fut, drive);
        let RotationOutcome::Committed(c) = res.unwrap() else {
            panic!("expected Committed");
        };
        assert_eq!(c.new_file_id, 6);
        let shared = client.shared.read().await;
        let f = shared.get("svc").unwrap();
        assert_eq!(f.file_id(), 6);
        assert!(!f.is_draining());
        assert!(
            f.view().iter().all(|b| *b == 0),
            "old-offset writes must not be replayed after a resize"
        );
        assert_eq!(f.view().len(), 16384);
    }

    async fn expect_drained(
        to_client: &UnboundedSender<ServerMessage>,
        from_client: &mut UnboundedReceiver<ClientMessage>,
    ) {
        // Nothing unflushed: straight to RotationDrained.
        let ClientMessage::RotationDrained(d) = from_client.recv().await.unwrap() else {
            panic!("expected RotationDrained");
        };
        assert_eq!(d.file_id, 5);
        let _ = to_client;
    }

    fn aborted(rid: &str) -> commy_wire::RotationAborted {
        commy_wire::RotationAborted {
            rotation_id: rid.to_string(),
            service_id: "svc".to_string(),
            file_id: 5,
            reason: commy_wire::AbortReason::FillTimeout,
        }
    }

    fn prepared(rid: &str, path: Option<String>, size: u64) -> commy_wire::RotationPrepared {
        commy_wire::RotationPrepared {
            rotation_id: rid.to_string(),
            service_id: "svc".to_string(),
            old_file_id: 5,
            new_file_id: 6,
            size,
            path,
        }
    }

    #[tokio::test]
    async fn abort_after_drain_reopens_writes_on_the_old_file_and_ignores_other_rotations() {
        let Rig {
            client,
            to_client,
            mut from_client,
            ..
        } = rig().await;
        let p = pending("rotA");
        let fut = client.drain_for_rotation(&p);
        let drive = async {
            expect_drained(&to_client, &mut from_client).await;
            // Another rotation's abort is not ours.
            to_client
                .send(ServerMessage::RotationAborted(aborted("other")))
                .unwrap();
            assert!(client.shared_write("svc", 0, &[1]).await.is_err());
            to_client
                .send(ServerMessage::RotationAborted(aborted("rotA")))
                .unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        assert!(matches!(res.unwrap(), RotationOutcome::Aborted(a) if a.rotation_id == "rotA"));
        let shared = client.shared.read().await;
        let f = shared.get("svc").unwrap();
        assert_eq!(f.file_id(), 5, "still on the old file");
        assert!(!f.is_draining());
        drop(shared);
        client.shared_write("svc", 0, &[1]).await.unwrap();
    }

    #[tokio::test]
    async fn first_terminal_message_wins_a_later_commit_is_not_read() {
        let Rig {
            client,
            to_client,
            mut from_client,
            ..
        } = rig().await;
        let p = pending("rotB");
        let fut = client.drain_for_rotation(&p);
        let drive = async {
            expect_drained(&to_client, &mut from_client).await;
            to_client
                .send(ServerMessage::RotationAborted(aborted("rotB")))
                .unwrap();
            to_client
                .send(ServerMessage::RotationCommit(commit(
                    "rotB",
                    RotateReason::Resize,
                )))
                .unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        assert!(matches!(res.unwrap(), RotationOutcome::Aborted(_)));
        assert_eq!(
            client.shared.read().await.get("svc").unwrap().file_id(),
            5,
            "the late commit must not switch the mapping"
        );
    }

    #[tokio::test]
    async fn requester_fills_a_local_prepared_file_then_commits() {
        let Rig {
            client,
            to_client,
            mut from_client,
            dir,
        } = rig().await;
        let prep_path = dir.path().join("prep.bin");
        std::fs::write(&prep_path, vec![0u8; 16384]).unwrap();
        let p = pending("rotC");
        let fut = async {
            let RotationOutcome::Prepared(r) = client.drain_for_rotation(&p).await.unwrap() else {
                panic!("requester must get Prepared");
            };
            let mut fill = client.begin_fill(&p, &r).unwrap();
            assert_eq!(fill.size(), 16384);
            assert!(fill.write(16383, &[1, 2]).is_err(), "past the end");
            fill.write(100, b"new shape").unwrap();
            client.finish_fill(fill).await.unwrap()
        };
        let drive = async {
            expect_drained(&to_client, &mut from_client).await;
            to_client
                .send(ServerMessage::RotationPrepared(prepared(
                    "rotC",
                    Some(prep_path.to_string_lossy().into_owned()),
                    16384,
                )))
                .unwrap();
            let ClientMessage::RotationFilled(f) = from_client.recv().await.unwrap() else {
                panic!("expected RotationFilled");
            };
            assert_eq!(f.rotation_id, "rotC");
            assert_eq!(&std::fs::read(&prep_path).unwrap()[100..109], b"new shape");
            to_client
                .send(ServerMessage::RotationCommit(commit(
                    "rotC",
                    RotateReason::Resize,
                )))
                .unwrap();
            serve_new_local_file(&to_client, &mut from_client, dir.path(), 16384).await;
        };
        let (res, ()) = tokio::join!(fut, drive);
        assert!(matches!(res, RotationOutcome::Committed(_)));
        assert_eq!(client.shared.read().await.get("svc").unwrap().file_id(), 6);
    }

    #[tokio::test]
    async fn requester_fills_a_remote_prepared_file_as_frames_then_sees_the_abort() {
        let Rig {
            client,
            to_client,
            mut from_client,
            ..
        } = rig().await;
        let p = pending("rotD");
        let fut = async {
            let RotationOutcome::Prepared(r) = client.drain_for_rotation(&p).await.unwrap() else {
                panic!("requester must get Prepared");
            };
            let mut fill = client.begin_fill(&p, &r).unwrap();
            fill.write(8, b"remote").unwrap();
            client.finish_fill(fill).await.unwrap()
        };
        let drive = async {
            expect_drained(&to_client, &mut from_client).await;
            to_client
                .send(ServerMessage::RotationPrepared(prepared(
                    "rotD", None, 4096,
                )))
                .unwrap();
            // Only the non-zero run travels, for the NEW file id, seq 0, absolute offsets.
            let ClientMessage::Range(frame) = from_client.recv().await.unwrap() else {
                panic!("expected a range frame");
            };
            assert_eq!(
                (
                    frame.file_id,
                    frame.seq,
                    frame.offset,
                    frame.data.as_slice()
                ),
                (6, 0, 8, &b"remote"[..])
            );
            let ClientMessage::FlushEnd(e) = from_client.recv().await.unwrap() else {
                panic!("expected FlushEnd");
            };
            assert_eq!(e.file_id, 6);
            to_client
                .send(ServerMessage::FlushAck(FlushAck {
                    request_id: e.request_id,
                    file_id: 6,
                    seq: 0,
                }))
                .unwrap();
            let ClientMessage::RotationFilled(_) = from_client.recv().await.unwrap() else {
                panic!("expected RotationFilled");
            };
            to_client
                .send(ServerMessage::RotationAborted(aborted("rotD")))
                .unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        assert!(matches!(res, RotationOutcome::Aborted(_)));
        assert!(!client.shared.read().await.get("svc").unwrap().is_draining());
    }

    #[tokio::test]
    async fn requester_can_abort_the_fill() {
        let Rig {
            client,
            to_client,
            mut from_client,
            ..
        } = rig().await;
        let p = pending("rotE");
        let fut = async {
            let RotationOutcome::Prepared(r) = client.drain_for_rotation(&p).await.unwrap() else {
                panic!("requester must get Prepared");
            };
            let fill = client.begin_fill(&p, &r).unwrap();
            client.abort_fill(fill).await.unwrap()
        };
        let drive = async {
            expect_drained(&to_client, &mut from_client).await;
            to_client
                .send(ServerMessage::RotationPrepared(prepared(
                    "rotE", None, 4096,
                )))
                .unwrap();
            let ClientMessage::RotationAbort(a) = from_client.recv().await.unwrap() else {
                panic!("expected RotationAbort");
            };
            assert_eq!(a.rotation_id, "rotE");
            to_client
                .send(ServerMessage::RotationAborted(
                    commy_wire::RotationAborted {
                        reason: commy_wire::AbortReason::RequesterAbort,
                        ..aborted("rotE")
                    },
                ))
                .unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        assert!(matches!(res, RotationOutcome::Aborted(a)
            if a.reason == commy_wire::AbortReason::RequesterAbort));
    }

    #[test]
    fn begin_fill_refuses_a_bad_size_or_path() {
        let c = Client::new("wss://localhost:9000");
        let p = pending("x");
        assert!(c.begin_fill(&p, &prepared("x", None, 0)).is_err());
        assert!(c
            .begin_fill(&p, &prepared("x", Some("/no/such/file".into()), 4096))
            .is_err());
    }

    #[tokio::test]
    async fn writes_are_refused_while_draining() {
        let Rig { client, .. } = rig().await;
        client
            .shared
            .write()
            .await
            .get_mut("svc")
            .unwrap()
            .set_draining(true);
        assert!(matches!(
            client.shared_write("svc", 0, &[1]).await,
            Err(CommyError::InvalidState(_))
        ));
    }

    #[tokio::test]
    async fn drain_sends_drained_then_keeps_waiting_for_the_commit() {
        let Rig {
            client,
            to_client: _keep,
            mut from_client,
            ..
        } = rig().await;
        let mut p = pending("rot2");
        p.deadline_ms = 10;
        // No data to flush, so the drain goes straight to RotationDrained; the commit never comes.
        // The grace period is 5 s, so shorten the wait by racing a timeout around the call.
        let res =
            tokio::time::timeout(Duration::from_millis(300), client.drain_for_rotation(&p)).await;
        assert!(res.is_err(), "still waiting for the commit");
        assert!(matches!(
            from_client.recv().await.unwrap(),
            ClientMessage::RotationDrained(_)
        ));
    }

    #[tokio::test]
    async fn revocation_carries_unflushed_writes_into_the_new_file() {
        let Rig {
            client,
            to_client,
            mut from_client,
            dir,
        } = rig().await;
        client.shared_write("svc", 100, &[7, 7]).await.unwrap();
        let c = commit("rot3", RotateReason::Revocation);
        let fut = client.complete_rotation(&c);
        let drive = async {
            serve_new_local_file(&to_client, &mut from_client, dir.path(), 16384).await;
            // The carried write is flushed to the NEW file id.
            let ClientMessage::RangesChanged(r) = from_client.recv().await.unwrap() else {
                panic!("expected the carried write to be flushed");
            };
            assert_eq!(r.file_id, 6);
            assert_eq!(
                r.ranges,
                vec![ByteRange {
                    offset: 100,
                    len: 2
                }]
            );
            to_client
                .send(ServerMessage::FlushAck(FlushAck {
                    request_id: r.request_id,
                    file_id: 6,
                    seq: 1,
                }))
                .unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        res.unwrap();
        let shared = client.shared.read().await;
        assert_eq!(&shared.get("svc").unwrap().view()[100..102], &[7, 7]);
    }

    #[tokio::test]
    async fn duplicate_commit_is_ignored() {
        let Rig {
            client,
            mut from_client,
            ..
        } = rig().await;
        let mut c = commit("old", RotateReason::Resize);
        c.new_file_id = 5;
        client.complete_rotation(&c).await.unwrap();
        assert!(
            from_client.try_recv().is_err(),
            "no handshake for a duplicate"
        );
        assert_eq!(client.shared.read().await.get("svc").unwrap().file_id(), 5);
    }

    #[tokio::test]
    async fn commit_naming_the_wrong_old_file_is_refused() {
        let Rig { client, .. } = rig().await;
        let mut c = commit("r", RotateReason::Resize);
        c.old_file_id = 99;
        assert!(matches!(
            client.complete_rotation(&c).await,
            Err(CommyError::InvalidState(_))
        ));
        assert_eq!(client.shared.read().await.get("svc").unwrap().file_id(), 5);
    }

    #[tokio::test]
    async fn loss_revoked_drops_the_mapping_but_resync_overwrote_keeps_it() {
        let Rig {
            client, to_client, ..
        } = rig().await;
        let loss = |kind| {
            ServerMessage::Loss(Loss {
                service_id: "svc".to_string(),
                file_id: 5,
                kind,
                ranges: vec![],
            })
        };
        to_client.send(loss(LossKind::ResyncOverwrote)).unwrap();
        assert!(matches!(
            client.poll_shared_events(Duration::from_millis(200)).await,
            Ok(SharedEvent::Loss(_))
        ));
        assert!(client.shared.read().await.contains_key("svc"));
        to_client.send(loss(LossKind::Revoked)).unwrap();
        assert!(matches!(
            client.poll_shared_events(Duration::from_millis(200)).await,
            Ok(SharedEvent::Loss(_))
        ));
        assert!(!client.shared.read().await.contains_key("svc"));
    }

    #[tokio::test]
    async fn pending_and_commit_are_reported_as_events() {
        let Rig {
            client, to_client, ..
        } = rig().await;
        to_client
            .send(ServerMessage::RotationPending(pending("a")))
            .unwrap();
        assert!(matches!(
            client.poll_shared_events(Duration::from_millis(200)).await,
            Ok(SharedEvent::RotationPending(_))
        ));
        to_client
            .send(ServerMessage::RotationCommit(commit(
                "a",
                RotateReason::Resize,
            )))
            .unwrap();
        assert!(matches!(
            client.poll_shared_events(Duration::from_millis(200)).await,
            Ok(SharedEvent::RotationCommit(_))
        ));
    }

    #[tokio::test]
    async fn request_rotation_validates_then_returns_the_rotation_id() {
        let Rig {
            client,
            to_client,
            mut from_client,
            ..
        } = rig().await;
        // RESIZE without new_size never leaves the client.
        let bad = client
            .request_rotation("svc", RotateReason::Resize, None, None)
            .await;
        assert!(matches!(bad, Err(CommyError::InvalidRequest(_))), "{bad:?}");
        assert!(from_client.try_recv().is_err());
        let fut = client.request_rotation("svc", RotateReason::Resize, Some(16384), None);
        let drive = async {
            let ClientMessage::RotateRequest(r) = from_client.recv().await.unwrap() else {
                panic!("expected RotateRequest");
            };
            assert_eq!(r.new_size, Some(16384));
            to_client
                .send(ServerMessage::RotationAccepted(RotationAccepted {
                    request_id: r.request_id,
                    rotation_id: "rot-9".to_string(),
                }))
                .unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        assert_eq!(res.unwrap(), "rot-9");
    }
}

#[cfg(test)]
mod grant_tests {
    use super::*;
    use crate::connection::Connection;
    use crate::message::{ClientMessage, ServerMessage};

    async fn client() -> (
        Client,
        tokio::sync::mpsc::UnboundedSender<ServerMessage>,
        tokio::sync::mpsc::UnboundedReceiver<ClientMessage>,
    ) {
        let (conn, to_client, from_client) = Connection::new_for_test();
        let client = Client::new("wss://localhost:9000");
        client.inject_connection_for_test(conn).await;
        (client, to_client, from_client)
    }

    #[tokio::test]
    async fn grant_sends_the_request_and_waits_for_its_ack() {
        let (c, to, mut from) = client().await;
        let fut = c.grant("bob", Some("svc"), &["service:read", "service:write"]);
        let drive = async {
            let ClientMessage::Grant(g) = from.recv().await.unwrap() else {
                panic!("expected Grant");
            };
            assert_eq!(g.client_id, "bob");
            assert_eq!(g.service_id.as_deref(), Some("svc"));
            assert_eq!(g.permissions, ["service:read", "service:write"]);
            // An ack for another request is not ours.
            to.send(ServerMessage::GrantAck(commy_wire::GrantAck {
                request_id: "other".into(),
            }))
            .unwrap();
            to.send(ServerMessage::GrantAck(commy_wire::GrantAck {
                request_id: g.request_id,
            }))
            .unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        res.unwrap();
    }

    #[tokio::test]
    async fn revoke_and_list_grants_round_trip() {
        let (c, to, mut from) = client().await;
        let fut = c.revoke("bob", Some("svc"), &[]);
        let drive = async {
            let ClientMessage::Revoke(r) = from.recv().await.unwrap() else {
                panic!("expected Revoke");
            };
            assert!(r.permissions.is_empty());
            to.send(ServerMessage::GrantAck(commy_wire::GrantAck {
                request_id: r.request_id,
            }))
            .unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        res.unwrap();

        let fut = c.list_grants(Some("bob"), None);
        let drive = async {
            let ClientMessage::ListGrants(l) = from.recv().await.unwrap() else {
                panic!("expected ListGrants");
            };
            assert_eq!((l.client_id.as_deref(), l.service_id), (Some("bob"), None));
            to.send(ServerMessage::Grants(commy_wire::Grants {
                request_id: l.request_id,
                entries: vec![commy_wire::GrantEntry {
                    client_id: "bob".into(),
                    service_id: Some("svc".into()),
                    permissions: vec!["service:read".into()],
                }],
            }))
            .unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        let entries = res.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].permissions, ["service:read"]);
    }

    #[tokio::test]
    async fn set_local_mapping_returns_the_confirmed_state() {
        let (c, to, mut from) = client().await;
        let fut = c.set_local_mapping("svc", true);
        let drive = async {
            let ClientMessage::SetLocalMapping(m) = from.recv().await.unwrap() else {
                panic!("expected SetLocalMapping");
            };
            assert_eq!((m.service_id.as_str(), m.enabled), ("svc", true));
            to.send(ServerMessage::LocalMappingAck(
                commy_wire::LocalMappingAck {
                    request_id: "other".into(),
                    service_id: "svc".into(),
                    enabled: false,
                },
            ))
            .unwrap();
            to.send(ServerMessage::LocalMappingAck(
                commy_wire::LocalMappingAck {
                    request_id: m.request_id,
                    service_id: "svc".into(),
                    enabled: true,
                },
            ))
            .unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        assert!(res.unwrap(), "the ack for another request must be ignored");
    }

    #[tokio::test]
    async fn set_local_mapping_surfaces_a_refusal() {
        let (c, to, _from) = client().await;
        to.send(ServerMessage::Error {
            code: crate::message::ErrorCode::PermissionDenied,
            message: "not an admin".into(),
            request_id: None,
        })
        .unwrap();
        assert!(matches!(
            c.set_local_mapping("svc", true).await,
            Err(CommyError::PermissionDenied(_))
        ));
    }

    #[tokio::test]
    async fn an_error_naming_another_request_is_not_this_requests_reply() {
        let (c, to, mut from) = client().await;
        let fut = c.grant("bob", None, &["tenant:read"]);
        let drive = async {
            let ClientMessage::Grant(g) = from.recv().await.unwrap() else {
                panic!("expected Grant");
            };
            // Someone else's refusal arrives first; it must not become this grant's result.
            to.send(ServerMessage::Error {
                code: crate::message::ErrorCode::PermissionDenied,
                message: "not yours".into(),
                request_id: Some("another-request".into()),
            })
            .unwrap();
            to.send(ServerMessage::GrantAck(commy_wire::GrantAck {
                request_id: g.request_id,
            }))
            .unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        res.unwrap();
    }

    #[tokio::test]
    async fn an_error_naming_this_request_and_new_wire_codes_reach_the_caller() {
        let (c, to, mut from) = client().await;
        let fut = c.grant("bob", None, &["tenant:read"]);
        let drive = async {
            let ClientMessage::Grant(g) = from.recv().await.unwrap() else {
                panic!("expected Grant");
            };
            to.send(ServerMessage::Error {
                code: crate::message::ErrorCode::RateLimited,
                message: "slow down".into(),
                request_id: Some(g.request_id),
            })
            .unwrap();
        };
        let (res, ()) = tokio::join!(fut, drive);
        assert!(matches!(res, Err(CommyError::Other(m)) if m.contains("Rate limited")));
        assert!(matches!(
            CommyError::from(crate::message::ErrorCode::Unauthenticated),
            CommyError::Unauthorized(_)
        ));
    }

    #[tokio::test]
    async fn invalid_grants_are_refused_before_sending_and_server_errors_surface() {
        let (c, to, mut from) = client().await;
        assert!(matches!(
            c.grant("", None, &["tenant:read"]).await,
            Err(CommyError::InvalidRequest(_))
        ));
        assert!(matches!(
            c.grant("bob", None, &[]).await,
            Err(CommyError::InvalidRequest(_))
        ));
        assert!(from.try_recv().is_err(), "nothing may have been sent");
        to.send(ServerMessage::Error {
            code: crate::message::ErrorCode::PermissionDenied,
            message: "no".into(),
            request_id: None,
        })
        .unwrap();
        assert!(matches!(
            c.grant("bob", None, &["tenant:read"]).await,
            Err(CommyError::PermissionDenied(_))
        ));
    }
}
