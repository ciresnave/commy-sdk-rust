//! Authentication utilities

use crate::Result;
pub use crate::message::AuthCredentials;

/// Authentication context for a session
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub tenant_id: String,
    pub permissions: Vec<String>,
    pub authenticated_at: chrono::DateTime<chrono::Utc>,
}

impl AuthContext {
    /// Create a new authentication context
    pub fn new(tenant_id: String, permissions: Vec<String>) -> Self {
        Self {
            tenant_id,
            permissions,
            authenticated_at: chrono::Utc::now(),
        }
    }

    /// Check if the context has a specific permission
    pub fn has_permission(&self, permission: &str) -> bool {
        self.permissions.iter().any(|p| p == permission)
    }

    /// Check if the context has admin permission
    pub fn is_admin(&self) -> bool {
        self.has_permission("admin")
    }

    /// Check if authenticated to a specific tenant
    pub fn is_authenticated_to(&self, tenant_id: &str) -> bool {
        self.tenant_id == tenant_id
    }
}

/// Build API key authentication
pub fn api_key(key: String) -> AuthCredentials {
    AuthCredentials::ApiKey { key }
}

/// Build JWT authentication
pub fn jwt(token: String) -> AuthCredentials {
    AuthCredentials::Jwt { token }
}

/// Build basic authentication
pub fn basic(username: String, password: String) -> AuthCredentials {
    AuthCredentials::Basic { username, password }
}

/// Validate a token format (basic validation)
pub fn validate_token_format(token: &str) -> Result<()> {
    if token.is_empty() {
        return Err(crate::error::CommyError::AuthenticationFailed(
            "Token cannot be empty".to_string(),
        ));
    }

    if token.len() > 10000 {
        return Err(crate::error::CommyError::AuthenticationFailed(
            "Token is too long".to_string(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_context_creation() {
        let ctx = AuthContext::new("tenant_1".to_string(), vec!["read".to_string()]);
        assert_eq!(ctx.tenant_id, "tenant_1");
        assert!(ctx.has_permission("read"));
    }

    #[test]
    fn test_has_permission() {
        let ctx = AuthContext::new(
            "tenant_1".to_string(),
            vec!["read".to_string(), "write".to_string()],
        );
        assert!(ctx.has_permission("read"));
        assert!(ctx.has_permission("write"));
        assert!(!ctx.has_permission("admin"));
    }

    #[test]
    fn test_validate_token_format() {
        assert!(validate_token_format("valid_token").is_ok());
        assert!(validate_token_format("").is_err());
    }
}
