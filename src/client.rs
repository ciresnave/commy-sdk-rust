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
            if let Ok(Ok(Some(ServerMessage::AuthenticationResult {
                success,
                permissions,
                ..
            }))) = tokio::time::timeout(Duration::from_secs(10), conn.recv()).await
            {
                if success {
                    let auth_context =
                        AuthContext::new(tenant_id.to_string(), permissions.unwrap_or_default());

                    let mut state = self.state.write().await;
                    state.connection_state = ConnectionState::Authenticated;
                    state.add_auth_context(tenant_id.to_string(), auth_context.clone());

                    Ok(auth_context)
                } else {
                    Err(CommyError::AuthenticationFailed(
                        "Authentication denied by server".to_string(),
                    ))
                }
            } else {
                Err(CommyError::Timeout)
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
            if let Ok(Ok(Some(ServerMessage::Service { service_id, .. }))) =
                tokio::time::timeout(Duration::from_secs(10), conn.recv()).await
            {
                Ok(service_id)
            } else {
                Err(CommyError::Timeout)
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
            if let Ok(Ok(Some(ServerMessage::Service {
                service_id,
                service_name,
                tenant_id: resp_tenant,
                file_path,
            }))) = tokio::time::timeout(Duration::from_secs(10), conn.recv()).await
            {
                let service = Service::new(service_id, service_name, resp_tenant, file_path);
                Ok(service)
            } else {
                Err(CommyError::Timeout)
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
            if let Ok(Ok(Some(ServerMessage::Result { success: true, .. }))) =
                tokio::time::timeout(Duration::from_secs(10), conn.recv()).await
            {
                Ok(())
            } else {
                Err(CommyError::Timeout)
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
            if let Ok(Ok(Some(ServerMessage::TenantResult {
                success: true,
                tenant_id: returned_id,
                ..
            }))) = tokio::time::timeout(Duration::from_secs(10), conn.recv()).await
            {
                Ok(returned_id)
            } else {
                Err(CommyError::Timeout)
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
            if let Ok(Ok(Some(ServerMessage::Result { success: true, .. }))) =
                tokio::time::timeout(Duration::from_secs(10), conn.recv()).await
            {
                Ok(())
            } else {
                Err(CommyError::Timeout)
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
            if let Ok(Ok(Some(ServerMessage::VariableData { data, .. }))) =
                tokio::time::timeout(Duration::from_secs(10), conn.recv()).await
            {
                Ok(data)
            } else {
                Err(CommyError::Timeout)
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
}
