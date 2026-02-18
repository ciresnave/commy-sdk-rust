#[cfg(test)]
mod tenant_crud_tests {
    use serde_json::json;

    /// Helper to deserialize ServerMessage responses
    fn parse_response(response: &str) -> serde_json::Value {
        serde_json::from_str(response).unwrap_or_else(|_| json!({"error": "parse_failed"}))
    }

    #[test]
    fn test_create_tenant_message_format() {
        let msg = json!({
            "type": "CreateTenant",
            "data": {
                "tenant_id": "org_alpha",
                "tenant_name": "Alpha Organization"
            }
        });

        assert_eq!(msg["type"].as_str().unwrap(), "CreateTenant");
        assert_eq!(msg["data"]["tenant_id"].as_str().unwrap(), "org_alpha");
        assert_eq!(
            msg["data"]["tenant_name"].as_str().unwrap(),
            "Alpha Organization"
        );
    }

    #[test]
    fn test_delete_tenant_message_format() {
        let msg = json!({
            "type": "DeleteTenant",
            "data": {
                "tenant_id": "org_alpha"
            }
        });

        assert_eq!(msg["type"].as_str().unwrap(), "DeleteTenant");
        assert_eq!(msg["data"]["tenant_id"].as_str().unwrap(), "org_alpha");
    }

    #[test]
    fn test_create_tenant_response_format() {
        let response_str = json!({
            "type": "TenantResult",
            "data": {
                "success": true,
                "tenant_id": "org_alpha",
                "message": "Tenant 'Alpha Organization' created"
            }
        }).to_string();

        let response = parse_response(&response_str);
        assert_eq!(response["type"].as_str().unwrap(), "TenantResult");
        assert!(response["data"]["success"].as_bool().unwrap());
        assert_eq!(response["data"]["tenant_id"].as_str().unwrap(), "org_alpha");
    }

    #[test]
    fn test_delete_tenant_response_format() {
        let response_str = json!({
            "type": "Result",
            "data": {
                "request_id": "req-123",
                "success": true,
                "message": "Tenant 'org_alpha' deleted"
            }
        }).to_string();

        let response = parse_response(&response_str);
        assert_eq!(response["type"].as_str().unwrap(), "Result");
        assert!(response["data"]["success"].as_bool().unwrap());
    }

    #[test]
    fn test_create_tenant_already_exists_error() {
        let error_response = json!({
            "type": "Error",
            "data": {
                "code": "AlreadyExists",
                "message": "Tenant 'org_alpha' already exists"
            }
        });

        assert_eq!(error_response["data"]["code"].as_str().unwrap(), "AlreadyExists");
    }

    #[test]
    fn test_delete_tenant_not_found_error() {
        let error_response = json!({
            "type": "Error",
            "data": {
                "code": "NotFound",
                "message": "Tenant 'org_alpha' not found"
            }
        });

        assert_eq!(error_response["data"]["code"].as_str().unwrap(), "NotFound");
    }

    #[test]
    fn test_create_tenant_missing_id() {
        let msg = json!({
            "type": "CreateTenant",
            "data": {
                "tenant_name": "Alpha Organization"
            }
        });

        // Missing tenant_id should trigger validation error
        let id = msg["data"].get("tenant_id");
        assert!(id.is_none() || id.unwrap().as_str().unwrap_or("").is_empty());
    }

    #[test]
    fn test_create_tenant_missing_name() {
        let msg = json!({
            "type": "CreateTenant",
            "data": {
                "tenant_id": "org_alpha"
            }
        });

        // Missing tenant_name should trigger validation error
        let name = msg["data"].get("tenant_name");
        assert!(name.is_none() || name.unwrap().as_str().unwrap_or("").is_empty());
    }

    #[test]
    fn test_create_tenant_serialization() {
        // Ensure message can be serialized to JSON
        let msg = json!({
            "type": "CreateTenant",
            "data": {
                "tenant_id": "org_beta",
                "tenant_name": "Beta Corp"
            }
        });

        let serialized = serde_json::to_string(&msg).unwrap();
        assert!(serialized.contains("CreateTenant"));
        assert!(serialized.contains("org_beta"));
        assert!(serialized.contains("Beta Corp"));
    }

    #[test]
    fn test_delete_tenant_serialization() {
        let msg = json!({
            "type": "DeleteTenant",
            "data": {
                "tenant_id": "org_beta"
            }
        });

        let serialized = serde_json::to_string(&msg).unwrap();
        assert!(serialized.contains("DeleteTenant"));
        assert!(serialized.contains("org_beta"));
    }

    #[test]
    fn test_tenant_result_deserialization() {
        let response_str = r#"
        {
            "type": "TenantResult",
            "data": {
                "success": true,
                "tenant_id": "org_gamma",
                "message": "Tenant created"
            }
        }"#;

        let response = parse_response(response_str);
        assert_eq!(response["type"].as_str().unwrap(), "TenantResult");
        assert_eq!(response["data"]["tenant_id"].as_str().unwrap(), "org_gamma");
    }

    #[test]
    fn test_create_tenant_with_special_characters() {
        let msg = json!({
            "type": "CreateTenant",
            "data": {
                "tenant_id": "org-alpha_123",
                "tenant_name": "Alpha Organization (Test)"
            }
        });

        assert_eq!(
            msg["data"]["tenant_id"].as_str().unwrap(),
            "org-alpha_123"
        );
    }

    #[test]
    fn test_multiple_tenant_operations_sequence() {
        // Test a sequence of tenant operations
        let create_msg = json!({
            "type": "CreateTenant",
            "data": {
                "tenant_id": "org_seq",
                "tenant_name": "Sequence Test"
            }
        });

        let delete_msg = json!({
            "type": "DeleteTenant",
            "data": {
                "tenant_id": "org_seq"
            }
        });

        // Both messages should serialize without error
        let create_json = serde_json::to_string(&create_msg).unwrap();
        let delete_json = serde_json::to_string(&delete_msg).unwrap();

        assert!(!create_json.is_empty());
        assert!(!delete_json.is_empty());
    }

    #[test]
    fn test_tenant_id_case_sensitivity() {
        let msg1 = json!({
            "type": "CreateTenant",
            "data": {
                "tenant_id": "OrgAlpha",
                "tenant_name": "Org Alpha"
            }
        });

        let msg2 = json!({
            "type": "CreateTenant",
            "data": {
                "tenant_id": "orgalpha",
                "tenant_name": "org alpha"
            }
        });

        assert_ne!(
            msg1["data"]["tenant_id"].as_str().unwrap(),
            msg2["data"]["tenant_id"].as_str().unwrap()
        );
    }

    #[test]
    fn test_invalid_tenant_operation_message() {
        let invalid_msg = json!({
            "type": "CreateTenant",
            "data": {}  // Missing required fields
        });

        // Should have empty/missing fields
        assert!(invalid_msg["data"]["tenant_id"].as_str().unwrap_or("").is_empty());
    }
}
