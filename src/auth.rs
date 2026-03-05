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

    #[test]
    fn test_api_key_credentials() {
        let creds = api_key("my_key_123".to_string());
        match creds {
            AuthCredentials::ApiKey { key } => assert_eq!(key, "my_key_123"),
            _ => panic!("Expected ApiKey variant"),
        }
    }

    #[test]
    fn test_jwt_credentials() {
        let creds = jwt("my.jwt.token".to_string());
        match creds {
            AuthCredentials::Jwt { token } => assert_eq!(token, "my.jwt.token"),
            _ => panic!("Expected Jwt variant"),
        }
    }

    #[test]
    fn test_basic_credentials() {
        let creds = basic("user".to_string(), "pass".to_string());
        match creds {
            AuthCredentials::Basic { username, password } => {
                assert_eq!(username, "user");
                assert_eq!(password, "pass");
            }
            _ => panic!("Expected Basic variant"),
        }
    }

    #[test]
    fn test_is_admin_true() {
        let ctx = AuthContext::new("t1".to_string(), vec!["admin".to_string()]);
        assert!(ctx.is_admin());
    }

    #[test]
    fn test_is_admin_false() {
        let ctx = AuthContext::new("t1".to_string(), vec!["read".to_string()]);
        assert!(!ctx.is_admin());
    }

    #[test]
    fn test_is_authenticated_to_matching_tenant() {
        let ctx = AuthContext::new("my_tenant".to_string(), vec![]);
        assert!(ctx.is_authenticated_to("my_tenant"));
    }

    #[test]
    fn test_is_authenticated_to_different_tenant() {
        let ctx = AuthContext::new("my_tenant".to_string(), vec![]);
        assert!(!ctx.is_authenticated_to("other_tenant"));
    }

    #[test]
    fn test_validate_token_too_long() {
        let long_token = "x".repeat(10001);
        assert!(validate_token_format(&long_token).is_err());
    }
}
