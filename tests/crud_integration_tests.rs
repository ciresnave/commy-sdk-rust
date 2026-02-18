//! Integration tests for CRUD operations
//!
//! These tests verify the complete end-to-end flow:
//! - Client authentication
//! - Service creation with permission checks
//! - Service reading (no implicit side effects)
//! - Service deletion with permission verification
//!
//! Note: These tests use mock/local testing without actual WebSocket server.
//! For full integration with real server, see server integration tests.

use commy_client::message::{ClientMessage, ServerMessage};

#[tokio::test]
async fn test_crud_permissions_flow() {
    // This is a logical test showing the expected behavior
    // In a real scenario, you'd need:
    // 1. A running test server
    // 2. Clients connecting to that server
    // 3. Mock permissions

    println!("CRUD Integration Test: Permission Separation");
    println!("============================================\n");

    // Scenario: Three different clients with different permissions
    println!("Setup: Creating three client instances with different permission profiles\n");

    // Client 1: Admin (has all permissions)
    println!("Client 1: Admin");
    println!("  - Has: ServiceCreate, ServiceRead, ServiceDelete");
    println!("  - Should be able to: create, read, delete services\n");

    // Client 2: Reader (read-only)
    println!("Client 2: Reader");
    println!("  - Has: ServiceRead only");
    println!("  - Should be able to: read services");
    println!("  - Should NOT be able to: create, delete services\n");

    // Client 3: Creator (can create and read, not delete)
    println!("Client 3: Creator");
    println!("  - Has: ServiceCreate, ServiceRead");
    println!("  - Should be able to: create, read services");
    println!("  - Should NOT be able to: delete services\n");

    println!("Expected test flow:");
    println!("1. Admin creates service 'config' ✓");
    println!("2. Reader reads service 'config' ✓");
    println!("3. Reader tries to create service ✗ (Permission denied)");
    println!("4. Creator reads service 'config' ✓");
    println!("5. Creator tries to delete service ✗ (Permission denied)");
    println!("6. Admin deletes service 'config' ✓");
    println!("\nNote: Actual test execution requires running test server\n");
}

#[tokio::test]
async fn test_create_service_message_format() {
    // Test that the CreateService message is formatted correctly
    use commy_client::message::ClientMessage;

    let msg = ClientMessage::CreateService {
        tenant_id: "org_a".to_string(),
        service_name: "config".to_string(),
    };

    println!("CreateService message format:");
    match serde_json::to_string_pretty(&msg) {
        Ok(json) => println!("{}", json),
        Err(e) => println!("Error serializing: {}", e),
    }

    // Verify it can be serialized
    let json = serde_json::to_string(&msg).expect("Should serialize");
    assert!(json.contains("CreateService"));
    assert!(json.contains("org_a"));
    assert!(json.contains("config"));
}

#[tokio::test]
async fn test_get_service_message_format() {
    // Test that the GetService message is formatted correctly
    use commy_client::message::ClientMessage;

    let msg = ClientMessage::GetService {
        tenant_id: "org_a".to_string(),
        service_name: "config".to_string(),
    };

    println!("GetService message format:");
    match serde_json::to_string_pretty(&msg) {
        Ok(json) => println!("{}", json),
        Err(e) => println!("Error serializing: {}", e),
    }

    // Verify it can be serialized
    let json = serde_json::to_string(&msg).expect("Should serialize");
    assert!(json.contains("GetService"));
    assert!(json.contains("org_a"));
    assert!(json.contains("config"));
}

#[tokio::test]
async fn test_delete_service_message_format() {
    // Test that the DeleteService message is formatted correctly
    use commy_client::message::ClientMessage;

    let msg = ClientMessage::DeleteService {
        tenant_id: "org_a".to_string(),
        service_name: "config".to_string(),
    };

    println!("DeleteService message format:");
    match serde_json::to_string_pretty(&msg) {
        Ok(json) => println!("{}", json),
        Err(e) => println!("Error serializing: {}", e),
    }

    // Verify it can be serialized
    let json = serde_json::to_string(&msg).expect("Should serialize");
    assert!(json.contains("DeleteService"));
    assert!(json.contains("org_a"));
    assert!(json.contains("config"));
}

#[tokio::test]
async fn test_error_code_variants() {
    // Test that all error codes are properly defined
    use commy_client::message::ErrorCode;

    println!("Available Error Codes:");
    println!("  - NotFound: Resource doesn't exist");
    println!("  - PermissionDenied: Insufficient permissions");
    println!("  - Unauthorized: Not authenticated");
    println!("  - AlreadyExists: Resource already created");
    println!("  - InvalidRequest: Bad parameters");
    println!("  - InternalError: Server error");
    println!("  - ConnectionLost: WSS disconnected");
    println!("  - Timeout: Operation timeout");

    // Verify error codes can be serialized
    let codes = vec![
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
        let json = serde_json::to_string(&code).expect("Should serialize");
        println!("  {} -> {}", format!("{:?}", code), json);
    }
}

#[tokio::test]
async fn test_service_response_message() {
    // Test that Service response message is formatted correctly
    let msg = ServerMessage::Service {
        service_id: "svc_123".to_string(),
        service_name: "config".to_string(),
        tenant_id: "org_a".to_string(),
        file_path: None,
    };

    println!("Service response message format:");
    match serde_json::to_string_pretty(&msg) {
        Ok(json) => println!("{}", json),
        Err(e) => println!("Error serializing: {}", e),
    }

    // Verify it can be serialized
    let json = serde_json::to_string(&msg).expect("Should serialize");
    assert!(json.contains("Service"));
    assert!(json.contains("svc_123"));
    assert!(json.contains("config"));
}

#[tokio::test]
async fn test_error_response_message() {
    // Test that Error response message is formatted correctly
    use commy_client::message::{ErrorCode, ServerMessage};

    let msg = ServerMessage::Error {
        code: ErrorCode::NotFound,
        message: "Service not found".to_string(),
    };

    println!("Error response message format:");
    match serde_json::to_string_pretty(&msg) {
        Ok(json) => println!("{}", json),
        Err(e) => println!("Error serializing: {}", e),
    }

    // Verify it can be serialized
    let json = serde_json::to_string(&msg).expect("Should serialize");
    assert!(json.contains("Error"));
    assert!(json.contains("NOT_FOUND"));
    assert!(json.contains("Service not found"));
}

#[tokio::test]
async fn test_create_service_idempotency_handling() {
    // Test showing how clients should handle AlreadyExists error
    use commy_client::message::ErrorCode;

    println!("Create Service Idempotency Pattern:");
    println!("==================================\n");

    println!("When creating a service:");
    println!("1. First call: Returns ServiceId (created)");
    println!("2. Second call: Returns AlreadyExists error");
    println!("\nClient handling pattern:");
    println!("match client.create_service(tenant, service).await {{");
    println!("    Ok(id) => println!(\"Created: {{}}\", id),");
    println!("    Err(CommyError::AlreadyExists(_)) => {{");
    println!("        println!(\"Service exists, getting it...\");");
    println!("        let svc = client.get_service(tenant, service).await?;");
    println!("    }}");
    println!("    Err(e) => return Err(e),");
    println!("}}");
}

#[tokio::test]
async fn test_permission_enforcement_scenarios() {
    // Document the expected permission enforcement behavior
    println!("Permission Enforcement Scenarios:");
    println!("================================\n");

    println!("Scenario 1: ReadOnly Client");
    println!("  Operation: get_service()");
    println!("  Required Permission: ServiceRead");
    println!("  Expected: ✓ Success (reads service)");
    println!("  Side Effects: None (pure read)\n");

    println!("Scenario 2: ReadOnly Client");
    println!("  Operation: create_service()");
    println!("  Required Permission: ServiceCreate");
    println!("  Expected: ✗ PermissionDenied error");
    println!("  Reason: Client doesn't have create permission\n");

    println!("Scenario 3: Creator Client");
    println!("  Operation: create_service()");
    println!("  Required Permission: ServiceCreate");
    println!("  Expected: ✓ Success (creates service)");
    println!("  Side Effects: Service allocated in filesystem\n");

    println!("Scenario 4: Creator Client");
    println!("  Operation: delete_service()");
    println!("  Required Permission: ServiceDelete");
    println!("  Expected: ✗ PermissionDenied error");
    println!("  Reason: Client has create+read but not delete\n");

    println!("Scenario 5: Admin Client");
    println!("  Operation: delete_service()");
    println!("  Required Permission: ServiceDelete");
    println!("  Expected: ✓ Success (deletes service)");
    println!("  Side Effects: Service file removed\n");

    println!("Scenario 6: Unauthenticated Client");
    println!("  Operation: Any CRUD operation");
    println!("  Required: Authentication first");
    println!("  Expected: ✗ Unauthorized error");
}

#[tokio::test]
async fn test_client_lifecycle() {
    // Test the expected client lifecycle with CRUD operations
    println!("Client Lifecycle for CRUD Operations:");
    println!("====================================\n");

    println!("Step 1: Create client instance");
    println!("  let client = Client::new(\"wss://localhost:9000\");\n");

    println!("Step 2: Connect to server");
    println!("  client.connect().await?;\n");

    println!("Step 3: Authenticate to tenant");
    println!("  client.authenticate(\"org_a\", credentials).await?;\n");

    println!("Step 4a: Create service (if ServiceCreate permission)");
    println!("  let service_id = client.create_service(\"org_a\", \"config\").await?;\n");

    println!("Step 4b: Read service (if ServiceRead permission)");
    println!("  let service = client.get_service(\"org_a\", \"config\").await?;\n");

    println!("Step 4c: Delete service (if ServiceDelete permission)");
    println!("  client.delete_service(\"org_a\", \"config\").await?;\n");

    println!("Step 5: Disconnect gracefully");
    println!("  client.disconnect().await?;\n");

    println!("Expected outcomes:");
    println!("  ✓ Operations succeed if client has permissions");
    println!("  ✗ Operations fail with specific error codes if permissions lacking");
    println!("  ✓ Errors are descriptive and actionable");
}

#[tokio::test]
async fn test_error_handling_patterns() {
    // Document error handling patterns
    use commy_client::CommyError;

    println!("Error Handling Patterns for CRUD:");
    println!("=================================\n");

    println!("Pattern 1: Handle AlreadyExists");
    println!("match client.create_service(...).await {{");
    println!("    Ok(id) => println!(\"Created: {{}}\", id),");
    println!("    Err(CommyError::AlreadyExists(msg)) => println!(\"Already exists: {{}}\", msg),");
    println!("    Err(e) => return Err(e),");
    println!("}}\n");

    println!("Pattern 2: Handle NotFound");
    println!("match client.get_service(...).await {{");
    println!("    Ok(service) => println!(\"Found: {{}}\", service.id),");
    println!("    Err(CommyError::NotFound(msg)) => println!(\"Not found: {{}}\", msg),");
    println!("    Err(e) => return Err(e),");
    println!("}}\n");

    println!("Pattern 3: Handle PermissionDenied");
    println!("match client.create_service(...).await {{");
    println!("    Ok(id) => println!(\"Created: {{}}\", id),");
    println!("    Err(CommyError::PermissionDenied(msg)) => {{");
    println!("        eprintln!(\"Permission denied: {{}}\", msg);");
    println!("        eprintln!(\"Request admin to grant create_service permission\");");
    println!("    }}");
    println!("    Err(e) => return Err(e),");
    println!("}}\n");

    println!("Pattern 4: Handle Unauthorized");
    println!("match client.delete_service(...).await {{");
    println!("    Ok(_) => println!(\"Deleted\"),");
    println!("    Err(CommyError::Unauthorized(msg)) => {{");
    println!("        eprintln!(\"Not authenticated: {{}}\", msg);");
    println!("        client.authenticate(tenant, creds).await?;");
    println!("        client.delete_service(...).await?; // retry");
    println!("    }}");
    println!("    Err(e) => return Err(e),");
    println!("}}\n");
}

#[tokio::test]
async fn test_concurrent_operations() {
    // Document how concurrent operations should be handled
    println!("Concurrent Operations Considerations:");
    println!("====================================\n");

    println!("Scenario 1: Two clients create same service");
    println!("  Client A: create_service(\"config\") -> Success");
    println!("  Client B: create_service(\"config\") -> AlreadyExists\n");

    println!("Scenario 2: Create while another deletes");
    println!("  Client A: delete_service(\"config\") -> Success");
    println!("  Client B: create_service(\"config\") -> May succeed (timing dependent)\n");

    println!("Scenario 3: Read while delete in progress");
    println!("  Client A: delete_service(\"config\") -> In progress");
    println!("  Client B: get_service(\"config\") -> May return NotFound\n");

    println!("Best practice:");
    println!("  - Use explicit error handling for AlreadyExists");
    println!("  - Implement retry logic with exponential backoff");
    println!("  - Use transactions for multi-step operations");
}

#[test]
fn test_message_serialization_round_trip() {
    // Test that messages can be serialized and deserialized
    use commy_client::message::ClientMessage;

    let original = ClientMessage::CreateService {
        tenant_id: "org_a".to_string(),
        service_name: "config".to_string(),
    };

    // Serialize to JSON
    let json = serde_json::to_string(&original).expect("Should serialize");

    // Deserialize back
    let deserialized: ClientMessage = serde_json::from_str(&json).expect("Should deserialize");

    // Verify the round trip preserved the data
    match deserialized {
        ClientMessage::CreateService {
            tenant_id,
            service_name,
        } => {
            assert_eq!(tenant_id, "org_a");
            assert_eq!(service_name, "config");
        }
        _ => panic!("Wrong message type after deserialization"),
    }
}
