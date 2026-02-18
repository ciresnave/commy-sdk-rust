# Hybrid Protocol Implementation Guide for Servers

## Overview

The hybrid client SDK sends new protocol messages to coordinate local memory-mapped access. Servers should handle these messages to enable:

1. **GetServiceFilePath** - Clients request actual file paths for local memory mapping
2. **ReportVariableChanges** - Clients report changes detected locally

This guide shows how to implement server-side support.

## Protocol Messages

### ClientMessage::GetServiceFilePath

Sent by hybrid clients running on the same machine as the server.

```rust
ClientMessage::GetServiceFilePath {
    tenant_id: String,        // Which tenant
    service_name: String,     // Which service
}
```

**When sent:**
- Client detects it's local (same machine as server)
- Client requests direct file path for memory mapping
- OR client fallback when local detection succeeds

**Expected response:**
```rust
ServerMessage::ServiceFilePath {
    service_id: String,       // Unique service identifier
    file_path: String,        // Absolute path to .mem file
    file_size: u64,           // Total file size in bytes
}
```

### ClientMessage::ReportVariableChanges

Sent by hybrid clients after detecting local changes via file watcher.

```rust
ClientMessage::ReportVariableChanges {
    service_id: String,                           // Which service changed
    changed_variables: Vec<String>,               // Variable names
    new_values: Vec<(String, Vec<u8>)>,          // Variable name -> data pairs
}
```

**When sent:**
- Client's file watcher detects changes via SIMD diff
- Client identifies affected variables
- Client reports changes back to server

**Expected response:**
```rust
ServerMessage::VariableChangesAcknowledged {
    service_id: String,           // Which service
    changed_variables: Vec<String>,  // Confirmed variables
}
```

## Server-Side Implementation

### Step 1: Message Routing

```rust
// In your server's message handler
async fn handle_client_message(
    client_session: &mut ClientSession,
    message: ClientMessage,
) -> Result<ServerMessage> {
    match message {
        // Existing message types...
        ClientMessage::ReadVariable { .. } => { /* ... */ }
        ClientMessage::WriteVariable { .. } => { /* ... */ }
        
        // NEW: Hybrid messages
        ClientMessage::GetServiceFilePath { tenant_id, service_name } => {
            handle_get_service_file_path(
                client_session,
                &tenant_id,
                &service_name,
            ).await
        }
        
        ClientMessage::ReportVariableChanges {
            service_id,
            changed_variables,
            new_values,
        } => {
            handle_report_variable_changes(
                client_session,
                &service_id,
                changed_variables,
                new_values,
            ).await
        }
        
        // Catch unknown messages
        _ => Err(CommyError::UnknownMessage),
    }
}
```

### Step 2: Implement GetServiceFilePath Handler

```rust
async fn handle_get_service_file_path(
    client_session: &ClientSession,
    tenant_id: &str,
    service_name: &str,
) -> Result<ServerMessage> {
    // 1. Verify client has permission for this tenant
    let permissions = client_session.get_permissions(tenant_id)?;
    if !permissions.can_read_service(service_name) {
        return Err(CommyError::PermissionDenied);
    }
    
    // 2. Get the actual service
    let tenant = server.get_tenant(tenant_id)?;
    let service = tenant.get_service(service_name)?;
    
    // 3. Get the underlying file path
    //    The server maintains this mapping in its internal state
    let file_path = get_service_file_path(tenant_id, service_name)?;
    
    // 4. Verify file exists and is accessible
    if !Path::new(&file_path).exists() {
        return Err(CommyError::FileNotFound);
    }
    
    // 5. Get file size for validation
    let file_size = std::fs::metadata(&file_path)?
        .len();
    
    // 6. Optionally: Log this request for audit
    audit_log::log_hybrid_access(
        client_session.client_id(),
        tenant_id,
        service_name,
        &file_path,
    ).await.ok();
    
    // 7. Return file path
    Ok(ServerMessage::ServiceFilePath {
        service_id: format!("{}_{}", tenant_id, service_name),
        file_path,
        file_size,
    })
}

fn get_service_file_path(tenant_id: &str, service_name: &str) -> Result<String> {
    // Server stores mapping: (tenant_id, service_name) -> file_path
    // This is typically maintained during service creation
    let server_state = get_server_state();
    
    server_state
        .service_file_paths
        .get(&(tenant_id.to_string(), service_name.to_string()))
        .cloned()
        .ok_or(CommyError::ServiceNotFound)
}
```

### Step 3: Implement ReportVariableChanges Handler

```rust
async fn handle_report_variable_changes(
    client_session: &ClientSession,
    service_id: &str,
    changed_variables: Vec<String>,
    new_values: Vec<(String, Vec<u8>)>,
) -> Result<ServerMessage> {
    // 1. Parse service_id to extract tenant and service names
    let (tenant_id, service_name) = parse_service_id(service_id)?;
    
    // 2. Verify permissions
    let permissions = client_session.get_permissions(&tenant_id)?;
    if !permissions.can_write_service(&service_name) {
        return Err(CommyError::PermissionDenied);
    }
    
    // 3. Get the service
    let tenant = server.get_tenant(&tenant_id)?;
    let service = tenant.get_service(&service_name)?;
    
    // 4. Apply the variable updates
    for (var_name, var_data) in new_values {
        // Verify variable exists
        service.get_variable(&var_name)?;
        
        // Update variable with new data
        service.update_variable(&var_name, &var_data).await?;
        
        // Mark variable as changed (for watchers)
        service.mark_changed(&var_name).await?;
    }
    
    // 5. Notify other clients about changes
    // (This propagates updates from local client to remote clients)
    broadcast_variable_changes(
        service,
        &changed_variables,
        client_session.client_id(),  // Exclude sender
    ).await?;
    
    // 6. Log the update
    audit_log::log_variable_update(
        client_session.client_id(),
        &tenant_id,
        &service_name,
        &changed_variables,
        new_values.len(),
    ).await.ok();
    
    // 7. Return acknowledgment
    Ok(ServerMessage::VariableChangesAcknowledged {
        service_id: service_id.to_string(),
        changed_variables,
    })
}

fn parse_service_id(service_id: &str) -> Result<(String, String)> {
    let parts: Vec<&str> = service_id.split('_').collect();
    if parts.len() < 2 {
        return Err(CommyError::InvalidServiceId);
    }
    
    let tenant_id = parts[0].to_string();
    let service_name = parts[1..].join("_");
    
    Ok((tenant_id, service_name))
}

async fn broadcast_variable_changes(
    service: &Service,
    changed_variables: &[String],
    exclude_client_id: u64,
) -> Result<()> {
    // Get all clients watching this service
    let watchers = service.get_watchers()?;
    
    for (client_id, client_tx) in watchers {
        if client_id == exclude_client_id {
            continue;  // Don't send back to originating client
        }
        
        // Create update message
        let message = ServerMessage::VariablesUpdated {
            service_id: service.id().to_string(),
            variables: changed_variables.to_vec(),
        };
        
        // Send to client
        let _ = client_tx.send(message).await;
    }
    
    Ok(())
}
```

### Step 4: Update Service File Path Tracking

During service creation, track the file path:

```rust
impl Tenant {
    pub async fn create_service(
        &self,
        service_name: &str,
    ) -> Result<Arc<Service>> {
        // 1. Create service
        let service = Service::new(service_name)?;
        
        // 2. Get file path
        let file_path = service.get_file_path();
        
        // 3. Store mapping in server state
        let mut state = get_server_state_mut();
        state.service_file_paths.insert(
            (self.id().to_string(), service_name.to_string()),
            file_path.clone(),
        );
        
        // 4. Continue normal initialization
        self.register_service(service.clone()).await?;
        
        Ok(service)
    }
}
```

## Security Considerations

### 1. Permission Checks

```rust
// Always verify client has permission before returning file path
fn verify_hybrid_access_allowed(
    client: &ClientSession,
    tenant_id: &str,
    service_name: &str,
) -> Result<()> {
    let perms = client.get_permissions(tenant_id)?;
    
    // Require explicit read permission for local mapping
    if !perms.can_read_service(service_name) {
        return Err(CommyError::PermissionDenied);
    }
    
    // Optional: Require explicit hybrid permission
    if !perms.can_use_hybrid_mode() {
        return Err(CommyError::HybridAccessDenied);
    }
    
    Ok(())
}
```

### 2. Locality Verification

Optionally verify that client connecting to request file path is actually local:

```rust
fn verify_client_is_local(client_addr: &SocketAddr) -> Result<bool> {
    // Check if client is from localhost
    let is_loopback = client_addr.ip().is_loopback();
    
    // OR check if on same network
    let on_same_network = is_same_network(
        get_server_local_ip(),
        client_addr.ip(),
    );
    
    Ok(is_loopback || on_same_network)
}
```

### 3. File Access Logging

```rust
async fn log_hybrid_access(
    client_id: u64,
    tenant_id: &str,
    service_name: &str,
    file_path: &str,
    access_type: &str,  // "GetServiceFilePath" or "ReportChanges"
) {
    println!(
        "[AUDIT] Client {} accessed {} {} via {} (file: {})",
        client_id,
        tenant_id,
        service_name,
        access_type,
        file_path
    );
    
    // Store in audit log
    audit_log::log_event(AuditEvent {
        timestamp: Utc::now(),
        client_id,
        tenant_id: tenant_id.to_string(),
        service_name: service_name.to_string(),
        event_type: access_type.to_string(),
        details: serde_json::json!({
            "file_path": file_path,
        }),
    }).await.ok();
}
```

## Error Handling

Implement proper error responses:

```rust
fn error_to_response(error: CommyError) -> ServerMessage {
    match error {
        CommyError::PermissionDenied => {
            ServerMessage::Error {
                error_code: "PERMISSION_DENIED".to_string(),
                message: "Client does not have permission for hybrid access".to_string(),
            }
        }
        
        CommyError::FileNotFound => {
            ServerMessage::Error {
                error_code: "FILE_NOT_FOUND".to_string(),
                message: "Service file not found on server".to_string(),
            }
        }
        
        CommyError::ServiceNotFound => {
            ServerMessage::Error {
                error_code: "SERVICE_NOT_FOUND".to_string(),
                message: "Requested service does not exist".to_string(),
            }
        }
        
        CommyError::VariableNotFound => {
            ServerMessage::Error {
                error_code: "VARIABLE_NOT_FOUND".to_string(),
                message: "Variable not found in service".to_string(),
            }
        }
        
        _ => {
            ServerMessage::Error {
                error_code: "INTERNAL_ERROR".to_string(),
                message: "Internal server error".to_string(),
            }
        }
    }
}
```

## Integration Examples

### Example 1: Minimal Integration

```rust
// Add to your message handler
match message {
    ClientMessage::GetServiceFilePath { tenant_id, service_name } => {
        // Check permission
        client_session.verify_permission(&tenant_id, &service_name)?;
        
        // Get file path
        let file_path = service_registry.get_file_path(&tenant_id, &service_name)?;
        
        // Return it
        ServerMessage::ServiceFilePath {
            service_id: format!("{}_{}", tenant_id, service_name),
            file_path,
            file_size: std::fs::metadata(&file_path)?.len(),
        }
    }
}
```

### Example 2: With Caching

```rust
struct ServerState {
    // Cache file paths to avoid repeated lookups
    file_path_cache: Arc<RwLock<HashMap<(String, String), String>>>,
}

async fn get_service_file_path_cached(
    state: &ServerState,
    tenant_id: &str,
    service_name: &str,
) -> Result<String> {
    let key = (tenant_id.to_string(), service_name.to_string());
    
    // Check cache first
    {
        let cache = state.file_path_cache.read().await;
        if let Some(path) = cache.get(&key) {
            return Ok(path.clone());
        }
    }
    
    // Not in cache, look up
    let path = service_registry.get_file_path(tenant_id, service_name)?;
    
    // Cache it
    {
        let mut cache = state.file_path_cache.write().await;
        cache.insert(key, path.clone());
    }
    
    Ok(path)
}
```

### Example 3: With Change Propagation

```rust
async fn handle_report_variable_changes(
    state: &ServerState,
    client: &ClientSession,
    service_id: &str,
    changed_variables: Vec<String>,
    new_values: Vec<(String, Vec<u8>)>,
) -> Result<ServerMessage> {
    let (tenant_id, service_name) = parse_service_id(service_id)?;
    
    // Update variables
    for (var_name, data) in new_values {
        let service = state.get_service(&tenant_id, &service_name)?;
        service.update_variable(&var_name, &data).await?;
    }
    
    // Broadcast to all watchers
    let service = state.get_service(&tenant_id, &service_name)?;
    for watcher in service.get_watchers() {
        if watcher.client_id != client.id() {
            watcher.tx.send(ServerMessage::VariablesUpdated {
                service_id: service_id.to_string(),
                variables: changed_variables.clone(),
            }).await.ok();
        }
    }
    
    Ok(ServerMessage::VariableChangesAcknowledged {
        service_id: service_id.to_string(),
        changed_variables,
    })
}
```

## Testing Server Implementation

### Test 1: Permission Check

```rust
#[tokio::test]
async fn test_get_service_file_path_permission_denied() {
    let server = setup_test_server().await;
    let client = create_client_without_permission().await;
    
    let result = server.handle_message(
        &client,
        ClientMessage::GetServiceFilePath {
            tenant_id: "tenant".to_string(),
            service_name: "service".to_string(),
        }
    ).await;
    
    assert!(matches!(result, Err(CommyError::PermissionDenied)));
}
```

### Test 2: File Path Validity

```rust
#[tokio::test]
async fn test_get_service_file_path_valid() {
    let server = setup_test_server().await;
    let client = create_authorized_client().await;
    
    let response = server.handle_message(
        &client,
        ClientMessage::GetServiceFilePath {
            tenant_id: "tenant".to_string(),
            service_name: "service".to_string(),
        }
    ).await.unwrap();
    
    match response {
        ServerMessage::ServiceFilePath { file_path, file_size, .. } => {
            // Path should be absolute
            assert!(Path::new(&file_path).is_absolute());
            
            // File should exist
            assert!(Path::new(&file_path).exists());
            
            // File size should match
            let actual_size = std::fs::metadata(&file_path).unwrap().len();
            assert_eq!(file_size, actual_size);
        }
        _ => panic!("Wrong response type"),
    }
}
```

### Test 3: Change Reporting

```rust
#[tokio::test]
async fn test_report_variable_changes() {
    let server = setup_test_server().await;
    let client = create_authorized_client().await;
    
    let response = server.handle_message(
        &client,
        ClientMessage::ReportVariableChanges {
            service_id: "tenant_service".to_string(),
            changed_variables: vec!["var1".to_string()],
            new_values: vec![
                ("var1".to_string(), vec![42u8]),
            ],
        }
    ).await.unwrap();
    
    match response {
        ServerMessage::VariableChangesAcknowledged { changed_variables, .. } => {
            assert!(changed_variables.contains(&"var1".to_string()));
        }
        _ => panic!("Wrong response type"),
    }
}
```

## Performance Tips

1. **Cache file paths** - Server-side mapping lookups are frequent
2. **Batch broadcasts** - Group multiple variable changes before broadcasting
3. **Async I/O** - Use async file operations for file size checks
4. **Permission cache** - Cache client permissions with expiration
5. **Connection pooling** - For database-backed service registries

## Troubleshooting

### Issue: Client gets "PermissionDenied" for valid service

**Cause:** Permission check too strict or cache stale

**Solution:**
```rust
// Verify permissions are properly set
let perms = client.get_permissions(tenant_id)?;
println!("Permissions: {:?}", perms);

// Clear permission cache if stale
permission_cache.invalidate(client_id).await;
```

### Issue: Client can't memory-map returned file path

**Cause:** File permissions wrong or path invalid

**Solution:**
```rust
// Verify file is readable by all users
let metadata = fs::metadata(&file_path)?;
let perms = metadata.permissions();

// Make sure it's readable
#[cfg(unix)]
{
    use std::os::unix::fs::PermissionsExt;
    let mode = perms.mode();
    if (mode & 0o444) != 0o444 {
        // Fix permissions
        let new_perms = Permissions::from_mode(0o644);
        fs::set_permissions(&file_path, new_perms)?;
    }
}
```

### Issue: Changes from local client not reaching other clients

**Cause:** Broadcast not implemented or watcher list empty

**Solution:**
```rust
// Ensure watchers are registered
let watchers = service.get_watchers();
println!("Watchers for service: {}", watchers.len());

// Verify broadcast is running
for (client_id, tx) in watchers {
    let result = tx.send(update_message).await;
    if result.is_err() {
        println!("Failed to send to client {}", client_id);
    }
}
```

## Next Steps

1. Implement message handlers in your server
2. Add tests for all error paths
3. Deploy with monitoring
4. Track performance metrics
5. Gather feedback from hybrid clients
6. Iterate on optimization

