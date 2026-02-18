//! Client state management

use crate::auth::AuthContext;
use crate::connection::ConnectionState;
use crate::service::ServiceManager;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Represents the complete client state
#[derive(Debug)]
pub struct ClientState {
    /// Current connection state
    pub connection_state: ConnectionState,

    /// Active authentication contexts per tenant
    pub auth_contexts: HashMap<String, AuthContext>,

    /// Service manager
    pub services: ServiceManager,

    /// Session ID
    pub session_id: Option<String>,

    /// Client ID
    pub client_id: String,

    /// Server version
    pub server_version: Option<String>,

    /// Last activity timestamp
    pub last_activity: chrono::DateTime<chrono::Utc>,
}

impl ClientState {
    /// Create a new client state
    pub fn new(client_id: String) -> Self {
        Self {
            connection_state: ConnectionState::Disconnected,
            auth_contexts: HashMap::new(),
            services: ServiceManager::new(),
            session_id: None,
            client_id,
            server_version: None,
            last_activity: chrono::Utc::now(),
        }
    }

    /// Update last activity time
    pub fn touch(&mut self) {
        self.last_activity = chrono::Utc::now();
    }

    /// Get idle duration in seconds
    pub fn idle_seconds(&self) -> u64 {
        (chrono::Utc::now() - self.last_activity)
            .num_seconds()
            .max(0) as u64
    }

    /// Add authentication context
    pub fn add_auth_context(&mut self, tenant_id: String, context: AuthContext) {
        self.auth_contexts.insert(tenant_id, context);
    }

    /// Get authentication context for a tenant
    pub fn get_auth_context(&self, tenant_id: &str) -> Option<&AuthContext> {
        self.auth_contexts.get(tenant_id)
    }

    /// Check if authenticated to a tenant
    pub fn is_authenticated_to(&self, tenant_id: &str) -> bool {
        self.auth_contexts.contains_key(tenant_id)
    }

    /// List authenticated tenants
    pub fn authenticated_tenants(&self) -> Vec<&str> {
        self.auth_contexts.keys().map(|s| s.as_str()).collect()
    }

    /// Clear authentication for a tenant
    pub fn clear_auth(&mut self, tenant_id: &str) {
        self.auth_contexts.remove(tenant_id);
    }

    /// Clear all authentication
    pub fn clear_all_auth(&mut self) {
        self.auth_contexts.clear();
    }

    /// Reset to disconnected state
    pub fn reset(&mut self) {
        self.connection_state = ConnectionState::Disconnected;
        self.session_id = None;
        self.auth_contexts.clear();
        self.services.clear();
        self.last_activity = chrono::Utc::now();
    }
}

/// Thread-safe shared client state
pub type SharedState = Arc<RwLock<ClientState>>;

/// Create shared state
pub fn create_shared_state(client_id: String) -> SharedState {
    Arc::new(RwLock::new(ClientState::new(client_id)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_state_creation() {
        let state = ClientState::new("client_1".to_string());
        assert_eq!(state.client_id, "client_1");
        assert!(state.session_id.is_none());
    }

    #[test]
    fn test_idle_calculation() {
        let mut state = ClientState::new("client_1".to_string());
        state.last_activity = chrono::Utc::now() - chrono::Duration::seconds(10);
        assert!(state.idle_seconds() >= 10);
    }

    #[test]
    fn test_auth_context_management() {
        let mut state = ClientState::new("client_1".to_string());
        let ctx = AuthContext::new("tenant_1".to_string(), vec!["read".to_string()]);

        state.add_auth_context("tenant_1".to_string(), ctx);
        assert!(state.is_authenticated_to("tenant_1"));
        assert_eq!(state.authenticated_tenants().len(), 1);
    }
}
