//! Main Commy client for connecting to servers

use crate::auth::{AuthContext, AuthCredentials};
use crate::connection::{Connection, ConnectionState};
use crate::error::{CommyError, Result};
use crate::message::{ClientMessage, ServerMessage};
use crate::service::Service;
use crate::state::{create_shared_state, SharedState};
use crate::virtual_file::VirtualVariableFile;
use crate::watcher::VariableFileWatcher;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Main Commy client for interacting with a Commy server
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
    heartbeat_interval: Duration,

    /// Maximum reconnection attempts
    max_reconnect_attempts: u32,

    /// Current reconnection attempt
    reconnect_attempts: Arc<AtomicU64>,

    /// Virtual variable files by service ID
    virtual_files: Arc<RwLock<std::collections::HashMap<String, Arc<VirtualVariableFile>>>>,

    /// File watcher for change detection
    file_watcher: Arc<RwLock<Option<Arc<VariableFileWatcher>>>>,

    /// Background heartbeat task handle
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
    /// After this, the client is ready to use with `get_virtual_service_file()`.
    pub async fn initialize(
        server_url: impl Into<String>,
        tenant_id: impl Into<String>,
        credentials: AuthCredentials,
    ) -> Result<Self> {
        let client = Self::_new(server_url);
        client._connect_impl().await?;
        client
            ._authenticate_impl(&tenant_id.into(), credentials)
            .await?;
        client._init_file_watcher_impl().await?;
        client._start_file_monitoring_impl().await?;
        Ok(client)
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
            reconnect_attempts: Arc::new(AtomicU64::new(0)),
            virtual_files: Arc::new(RwLock::new(std::collections::HashMap::new())),
            file_watcher: Arc::new(RwLock::new(None)),
            heartbeat_task: Arc::new(RwLock::new(None)),
        }
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
            reconnect_attempts: Arc::new(AtomicU64::new(0)),
            virtual_files: Arc::new(RwLock::new(std::collections::HashMap::new())),
            file_watcher: Arc::new(RwLock::new(None)),
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

        match Connection::new(&self.server_url).await {
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
        self.send_message(ClientMessage::Authenticate {
            tenant_id: tenant_id.to_string(),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            credentials: credentials.clone(),
        })
        .await?;

        // Wait for authentication result
        if let Some(conn) = &*self.connection.read().await {
            match tokio::time::timeout(Duration::from_secs(10), conn.recv()).await {
                Ok(Ok(Some(ServerMessage::AuthenticationResult {
                    success: true,
                    permissions,
                    ..
                }))) => {
                    let auth_context =
                        AuthContext::new(tenant_id.to_string(), permissions.unwrap_or_default());

                    let mut state = self.state.write().await;
                    state.connection_state = ConnectionState::Authenticated;
                    state.add_auth_context(tenant_id.to_string(), auth_context.clone());

                    Ok(auth_context)
                }
                Ok(Ok(Some(ServerMessage::AuthenticationResult { success: false, message, .. }))) => {
                    Err(CommyError::AuthenticationFailed(message))
                }
                Ok(Ok(Some(ServerMessage::Error { code, .. }))) => {
                    Err(CommyError::from(code))
                }
                Err(_) => Err(CommyError::Timeout),
                _ => Err(CommyError::Timeout),
            }
        } else {
            Err(CommyError::ConnectionLost(
                "Connection not established".to_string(),
            ))
        }
    }

    /// Authenticate to a tenant (public - for testing/special cases)
    pub async fn authenticate(
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
    pub async fn create_service(&self, tenant_id: &str, service_name: &str) -> Result<String> {
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
    pub async fn get_service(&self, tenant_id: &str, service_name: &str) -> Result<Service> {
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
    pub async fn delete_service(&self, tenant_id: &str, service_name: &str) -> Result<()> {
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
                Ok(Ok(Some(ServerMessage::Result { success: false, message, .. }))) => {
                    Err(CommyError::PermissionDenied(message))
                }
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
    pub async fn create_tenant(&self, tenant_id: &str, tenant_name: &str) -> Result<String> {
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
                Ok(Ok(Some(ServerMessage::TenantResult { success: false, message, .. }))) => {
                    Err(CommyError::PermissionDenied(message))
                }
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
    pub async fn delete_tenant(&self, tenant_id: &str) -> Result<()> {
        // Request tenant deletion
        self.send_message(ClientMessage::DeleteTenant {
            tenant_id: tenant_id.to_string(),
        })
        .await?;

        // Wait for result acknowledgment
        if let Some(conn) = &*self.connection.read().await {
            match tokio::time::timeout(Duration::from_secs(10), conn.recv()).await {
                Ok(Ok(Some(ServerMessage::Result { success: true, .. }))) => Ok(()),
                Ok(Ok(Some(ServerMessage::Result { success: false, message, .. }))) => {
                    Err(CommyError::PermissionDenied(message))
                }
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

    /// Read a variable value
    pub async fn read_variable(&self, service_id: &str, variable_name: &str) -> Result<Vec<u8>> {
        self.send_message(ClientMessage::ReadVariable {
            service_id: service_id.to_string(),
            variable_name: variable_name.to_string(),
        })
        .await?;

        // Wait for variable data
        if let Some(conn) = &*self.connection.read().await {
            match tokio::time::timeout(Duration::from_secs(10), conn.recv()).await {
                Ok(Ok(Some(ServerMessage::VariableData { data, .. }))) => Ok(data),
                Ok(Ok(Some(ServerMessage::Error { code, .. }))) => Err(CommyError::from(code)),
                Err(_) => Err(CommyError::Timeout),
                _ => Err(CommyError::Timeout),
            }
        } else {
            Err(CommyError::ConnectionLost(
                "Connection lost during read_variable".to_string(),
            ))
        }
    }

    /// Write a variable value
    pub async fn write_variable(
        &self,
        service_id: &str,
        variable_name: &str,
        data: Vec<u8>,
    ) -> Result<()> {
        self.send_message(ClientMessage::WriteVariable {
            service_id: service_id.to_string(),
            variable_name: variable_name.to_string(),
            data,
        })
        .await?;

        Ok(())
    }

    /// Subscribe to variable changes
    pub async fn subscribe(&self, service_id: &str, variable_name: &str) -> Result<()> {
        self.send_message(ClientMessage::Subscribe {
            service_id: service_id.to_string(),
            variable_name: variable_name.to_string(),
        })
        .await?;

        Ok(())
    }

    /// Unsubscribe from variable changes
    pub async fn unsubscribe(&self, service_id: &str, variable_name: &str) -> Result<()> {
        self.send_message(ClientMessage::Unsubscribe {
            service_id: service_id.to_string(),
            variable_name: variable_name.to_string(),
        })
        .await?;

        Ok(())
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

    /// Start background heartbeat task
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
    pub async fn init_file_watcher(&self) -> Result<()> {
        self._init_file_watcher_impl().await
    }

    /// Get or create a virtual variable file for a service
    ///
    /// This creates a virtual representation that works seamlessly for both:
    /// - Local clients: Memory-mapped to actual service file
    /// - Remote clients: In-memory buffer synced via WSS
    pub async fn get_virtual_service_file(
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
    pub async fn start_file_monitoring(&self) -> Result<()> {
        self._start_file_monitoring_impl().await
    }

    /// Get next file change event (blocks until a file changes)
    pub async fn wait_for_file_change(&self) -> Result<Option<crate::watcher::FileChangeEvent>> {
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
    pub async fn try_get_file_change(&self) -> Result<Option<crate::watcher::FileChangeEvent>> {
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
    pub async fn stop_file_monitoring(&self) -> Result<()> {
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
        assert_eq!(client.connection_state().await, ConnectionState::Disconnected);
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
        assert!(result.is_ok(), "init_file_watcher should succeed: {:?}", result);
    }

    #[tokio::test]
    async fn test_start_file_monitoring_succeeds_and_is_idempotent() {
        let client = Client::new("wss://localhost:9000");
        // First call initialises the watcher
        let r1 = client.start_file_monitoring().await;
        assert!(r1.is_ok(), "first start_file_monitoring should succeed: {:?}", r1);
        // Second call is a no-op (watcher already exists)
        let r2 = client.start_file_monitoring().await;
        assert!(r2.is_ok(), "second start_file_monitoring should also succeed: {:?}", r2);
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
        assert!(stop_result.is_ok(), "stop_file_monitoring should succeed: {:?}", stop_result);

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

        assert!(Arc::ptr_eq(&vf1, &vf2), "second call should return the cached VirtualVariableFile");

        // Different service name → different file
        let vf3 = client
            .get_virtual_service_file("tenant_a", "other_service")
            .await
            .expect("should create a separate virtual service file");

        assert!(!Arc::ptr_eq(&vf1, &vf3), "different service should be a separate file");
    }

    // ─────────────────────────────────────────────────────────────
    // Auth pre-check guard tests
    // ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_create_service_not_authenticated_returns_permission_denied() {
        let client = Client::new("wss://localhost:9999");
        // No authentication → must fail immediately with PermissionDenied
        let result = client.create_service("tenant_a", "my_service").await;
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
    ) -> (Client, tokio::sync::mpsc::UnboundedReceiver<crate::message::ClientMessage>) {
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
        };
        let (client, _client_rx) = setup_client_with_mock_response("tenant_a", err_response).await;
        let result = client.create_service("tenant_a", "missing_svc").await;
        assert!(result.is_err(), "create_service must fail on Error response");
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
        let (conn, server_tx, _client_rx) = crate::connection::Connection::new_for_test();
        server_tx
            .send(crate::message::ServerMessage::AuthenticationResult {
                success: false,
                message: "invalid credentials".to_string(),
                server_version: "0.1.0".to_string(),
                permissions: None,
            })
            .expect("pre-send failed");
        client.inject_connection_for_test(conn).await;

        let creds = crate::message::AuthCredentials::ApiKey {
            key: "bad_key".to_string(),
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
            CommyError::Timeout => panic!(
                "authenticate success=false was silently converted to Timeout"
            ),
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
            })
            .expect("pre-send failed");
        client.inject_connection_for_test(conn).await;

        let creds = crate::message::AuthCredentials::Jwt {
            token: "expired-token".to_string(),
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
        assert!(client.is_connected().await, "must be connected before disconnect");
        assert!(
            client.is_authenticated_to("tenant_a").await,
            "must be authenticated before disconnect"
        );

        let result = client.disconnect().await;
        assert!(result.is_ok(), "disconnect should succeed: {:?}", result);

        // Post-conditions
        assert!(!client.is_connected().await, "connection must be cleared after disconnect");
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
            tokio::spawn(async move { c1.create_service("t1", "svc_a").await }),
            tokio::spawn(async move { c2.create_service("t1", "svc_b").await }),
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
}
