# Commy Rust SDK: CRUD API Reference

## Overview

The Commy SDK implements explicit CRUD (Create, Read, Update, Delete) operations with proper permission separation. This document covers all CRUD operations available in the SDK.

## Philosophy: Explicit Over Implicit

The SDK follows the principle of explicit operations rather than implicit side effects:

- ❌ **OLD (Implicit)**: `get_service()` creates if missing
- ✅ **NEW (Explicit)**: `create_service()` for creation, `get_service()` for reading

This ensures:
- ✅ No surprising side effects from read operations
- ✅ Clear permission boundaries (create ≠ read)
- ✅ Better auditability (who created what)
- ✅ Proper error semantics

## Table of Contents

1. [Authentication](#authentication)
2. [Service CRUD](#service-crud)
3. [Permission Model](#permission-model)
4. [Error Handling](#error-handling)
5. [Examples](#examples)
6. [Best Practices](#best-practices)

---

## Authentication

Before performing any CRUD operations, authenticate to a tenant:

```rust
use commy_client::{Client, auth};

let client = Client::new("wss://localhost:9000");
client.connect().await?;

let auth_ctx = client.authenticate(
    "my_tenant",
    auth::api_key("my_api_key".to_string())
).await?;

println!("Authenticated! Permissions: {:?}", auth_ctx.permissions);
```

---

## Service CRUD

### Create Service

Create a new service explicitly.

```rust
// Create a new service
let service_id = client.create_service("my_tenant", "app_config").await?;
println!("Created service: {}", service_id);
```

**Requirements:**
- Must be authenticated to the tenant
- Must have `ServiceCreate` permission
- Service must not already exist

**Returns:** `Result<String>` - Service ID if successful

**Errors:**
- `Unauthorized` - Not authenticated to tenant
- `PermissionDenied` - Lacks `ServiceCreate` permission
- `AlreadyExists` - Service already exists
- `ConnectionLost` - WebSocket disconnected
- `Timeout` - Operation timed out

**Example with error handling:**

```rust
match client.create_service("org_a", "config").await {
    Ok(id) => println!("Created: {}", id),
    Err(CommyError::AlreadyExists(_)) => {
        println!("Service exists, getting it...");
        let svc = client.get_service("org_a", "config").await?;
        println!("Service ID: {}", svc.id);
    }
    Err(CommyError::PermissionDenied(msg)) => {
        eprintln!("Permission denied: {}", msg);
        eprintln!("Request admin to grant create_service permission");
    }
    Err(e) => return Err(e.into()),
}
```

### Read Service

Get an existing service. This operation is **read-only** with **no side effects**.

```rust
// Read service metadata
let service = client.get_service("my_tenant", "app_config").await?;
println!("Service ID: {}", service.id);
```

**Requirements:**
- Must be authenticated to the tenant
- Must have `ServiceRead` permission
- Service must exist

**Returns:** `Result<Service>` - Service metadata if found

**Errors:**
- `Unauthorized` - Not authenticated to tenant
- `PermissionDenied` - Lacks `ServiceRead` permission
- `NotFound` - Service doesn't exist
- `ConnectionLost` - WebSocket disconnected
- `Timeout` - Operation timed out

**Important:** `get_service()` will **NOT** create the service if it doesn't exist. Use `create_service()` for creation.

**Example with error handling:**

```rust
match client.get_service("org_a", "config").await {
    Ok(service) => {
        println!("Found service: {} (ID: {})", service.name, service.id);
    }
    Err(CommyError::NotFound(msg)) => {
        eprintln!("Service not found: {}", msg);
        eprintln!("Create it with create_service()");
    }
    Err(CommyError::PermissionDenied(_)) => {
        eprintln!("Permission denied: need read_service permission");
    }
    Err(e) => return Err(e.into()),
}
```

### Delete Service

Delete an existing service.

```rust
// Delete a service
client.delete_service("my_tenant", "app_config").await?;
println!("Service deleted!");
```

**Requirements:**
- Must be authenticated to the tenant
- Must have `ServiceDelete` permission
- Service must exist

**Returns:** `Result<()>` - Success with no return value

**Errors:**
- `Unauthorized` - Not authenticated to tenant
- `PermissionDenied` - Lacks `ServiceDelete` permission
- `NotFound` - Service doesn't exist
- `ConnectionLost` - WebSocket disconnected
- `Timeout` - Operation timed out

**Example with error handling:**

```rust
match client.delete_service("org_a", "config").await {
    Ok(_) => println!("Service deleted successfully"),
    Err(CommyError::NotFound(_)) => {
        eprintln!("Service not found");
    }
    Err(CommyError::PermissionDenied(_)) => {
        eprintln!("Permission denied: need delete_service permission");
    }
    Err(e) => return Err(e.into()),
}
```

---

## Permission Model

The SDK enforces granular permissions for CRUD operations:

| Permission      | Operation          | Allows                |
| --------------- | ------------------ | --------------------- |
| `ServiceCreate` | `create_service()` | Create new services   |
| `ServiceRead`   | `get_service()`    | Read service metadata |
| `ServiceDelete` | `delete_service()` | Delete services       |

### Permission Separation Benefits

**Example: Read-Only Client**

```rust
// This client has ServiceRead permission only
let client = Client::new("wss://localhost:9000");
client.connect().await?;
client.authenticate("org_a", api_key("read_only_key")).await?;

// ✅ This works (has ServiceRead)
let service = client.get_service("org_a", "config").await?;

// ❌ This fails with PermissionDenied (no ServiceCreate)
client.create_service("org_a", "new_svc").await?;  // Error!

// ❌ This fails with PermissionDenied (no ServiceDelete)
client.delete_service("org_a", "config").await?;  // Error!
```

**Example: Creator Client**

```rust
// This client has ServiceCreate + ServiceRead permissions
let client = Client::new("wss://localhost:9000");
client.connect().await?;
client.authenticate("org_a", api_key("creator_key")).await?;

// ✅ This works (has ServiceCreate)
let id = client.create_service("org_a", "new_svc").await?;

// ✅ This works (has ServiceRead)
let service = client.get_service("org_a", "new_svc").await?;

// ❌ This fails with PermissionDenied (no ServiceDelete)
client.delete_service("org_a", "new_svc").await?;  // Error!
```

---

## Error Handling

The SDK provides specific error types for different failure scenarios:

```rust
use commy_client::CommyError;

match result {
    Err(CommyError::NotFound(msg)) => {
        // Resource doesn't exist
        eprintln!("Resource not found: {}", msg);
    }
    Err(CommyError::PermissionDenied(msg)) => {
        // Insufficient permissions for operation
        eprintln!("Permission denied: {}", msg);
        eprintln!("Request admin to grant required permission");
    }
    Err(CommyError::Unauthorized(msg)) => {
        // Not authenticated or authentication invalid
        eprintln!("Unauthorized: {}", msg);
        // Retry with authentication
        client.authenticate(tenant, credentials).await?;
    }
    Err(CommyError::AlreadyExists(msg)) => {
        // Resource already exists (for create operations)
        eprintln!("Already exists: {}", msg);
        // Retrieve existing resource
        let existing = client.get_service(tenant, service).await?;
    }
    Err(CommyError::InvalidRequest(msg)) => {
        // Bad request parameters
        eprintln!("Invalid request: {}", msg);
        // Verify parameter format and values
    }
    Err(CommyError::ConnectionLost(msg)) => {
        // WebSocket connection lost
        eprintln!("Connection lost: {}", msg);
        // Reconnect and retry
        client.connect().await?;
        // Retry operation...
    }
    Err(CommyError::Timeout) => {
        // Operation timed out
        eprintln!("Operation timed out");
        // Retry with exponential backoff
    }
    Err(e) => {
        // Other errors
        eprintln!("Error: {}", e);
    }
}
```

---

## Examples

### Example 1: Complete CRUD Workflow

```rust
use commy_client::{Client, auth};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Create and connect client
    let client = Client::new("wss://localhost:9000");
    client.connect().await?;
    println!("Connected!");

    // 2. Authenticate to tenant
    let auth_ctx = client.authenticate(
        "my_tenant",
        auth::api_key("admin_key".to_string())
    ).await?;
    println!("Authenticated! Permissions: {:?}", auth_ctx.permissions);

    // 3. CREATE: Create a new service
    match client.create_service("my_tenant", "app_config").await {
        Ok(id) => println!("✓ Created service: {}", id),
        Err(e) => println!("✗ Failed to create: {}", e),
    }

    // 4. READ: Get service metadata
    match client.get_service("my_tenant", "app_config").await {
        Ok(service) => println!("✓ Read service: {}", service.id),
        Err(e) => println!("✗ Failed to read: {}", e),
    }

    // 5. DELETE: Delete the service
    match client.delete_service("my_tenant", "app_config").await {
        Ok(_) => println!("✓ Deleted service"),
        Err(e) => println!("✗ Failed to delete: {}", e),
    }

    // 6. Disconnect
    client.disconnect().await?;
    println!("Disconnected!");

    Ok(())
}
```

### Example 2: Handling AlreadyExists

```rust
// Idempotent service creation
async fn ensure_service_exists(
    client: &Client,
    tenant: &str,
    service: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    match client.create_service(tenant, service).await {
        Ok(id) => {
            println!("Created new service: {}", id);
            Ok(id)
        }
        Err(CommyError::AlreadyExists(_)) => {
            println!("Service already exists, retrieving it...");
            let svc = client.get_service(tenant, service).await?;
            Ok(svc.id)
        }
        Err(e) => Err(e.into()),
    }
}
```

### Example 3: Safe Read with Error Recovery

```rust
// Read service with automatic recovery
async fn read_service_safe(
    client: &Client,
    tenant: &str,
    service: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    loop {
        match client.get_service(tenant, service).await {
            Ok(svc) => {
                println!("Service found: {}", svc.id);
                return Ok(svc.id);
            }
            Err(CommyError::NotFound(_)) => {
                println!("Service not found, creating it...");
                client.create_service(tenant, service).await?;
                // Retry read
                continue;
            }
            Err(CommyError::ConnectionLost(_)) => {
                println!("Connection lost, reconnecting...");
                client.connect().await?;
                // Retry read
                continue;
            }
            Err(e) => {
                eprintln!("Failed to read service: {}", e);
                return Err(e.into());
            }
        }
    }
}
```

---

## Best Practices

### 1. Always Authenticate First

```rust
// ✅ Correct: Authenticate before CRUD
client.connect().await?;
client.authenticate("tenant", creds).await?;
let svc = client.get_service("tenant", "service").await?;

// ❌ Wrong: No authentication
let svc = client.get_service("tenant", "service").await?;  // Fails with Unauthorized
```

### 2. Use Explicit Create for Idempotency

```rust
// ✅ Correct: Explicit creation with AlreadyExists handling
match client.create_service(tenant, service).await {
    Ok(id) => println!("Created: {}", id),
    Err(CommyError::AlreadyExists(_)) => {
        let svc = client.get_service(tenant, service).await?;
    }
    Err(e) => return Err(e.into()),
}

// ❌ Wrong: Implicit creation (old pattern)
// SDK no longer does implicit creation
```

### 3. Check Permissions Before Errors

```rust
// ✅ Correct: Distinguish between PermissionDenied and NotFound
match client.create_service(tenant, service).await {
    Err(CommyError::PermissionDenied(_)) => {
        // Different handling than NotFound
        eprintln!("Request admin to grant permission");
    }
    Err(CommyError::AlreadyExists(_)) => {
        // Service exists, get it
    }
    Err(e) => return Err(e.into()),
}
```

### 4. Implement Retry Logic

```rust
// ✅ Correct: Retry transient errors
async fn create_with_retry(
    client: &Client,
    tenant: &str,
    service: &str,
    max_retries: u32,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut retries = 0;
    loop {
        match client.create_service(tenant, service).await {
            Ok(id) => return Ok(id),
            Err(CommyError::ConnectionLost(_)) | Err(CommyError::Timeout) => {
                retries += 1;
                if retries > max_retries {
                    return Err("Max retries exceeded".into());
                }
                tokio::time::sleep(
                    std::time::Duration::from_millis(100 * 2_u64.pow(retries))
                ).await;
                continue;
            }
            Err(e) => return Err(e.into()),
        }
    }
}
```

### 5. Handle Permission Errors Gracefully

```rust
// ✅ Correct: Inform user about missing permissions
async fn safe_delete(
    client: &Client,
    tenant: &str,
    service: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    match client.delete_service(tenant, service).await {
        Ok(_) => {
            println!("Service deleted successfully");
            Ok(())
        }
        Err(CommyError::PermissionDenied(_)) => {
            eprintln!("Permission denied!");
            eprintln!("You need 'delete_service' permission");
            eprintln!("Contact your administrator to request this permission");
            Err("Permission denied".into())
        }
        Err(CommyError::NotFound(_)) => {
            eprintln!("Service doesn't exist");
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}
```

---

## Migration Guide

### From Implicit to Explicit CRUD

**Old Code (Implicit):**
```rust
// Old: get_service could create implicitly
let service = client.get_service("tenant", "svc").await?;
```

**New Code (Explicit):**
```rust
// New: explicit creation + safe reading
let id = client.create_service("tenant", "svc").await
    .or_else(|e| match e {
        CommyError::AlreadyExists(_) => {
            client.get_service("tenant", "svc").await.map(|s| s.id)
        }
        other => Err(other),
    })?;

// Later: safe read without side effects
let service = client.get_service("tenant", "svc").await?;
```

---

## FAQ

**Q: Why can't `get_service()` create the service if missing?**  
A: Explicit operations prevent unexpected side effects and enable proper permission separation. Creation requires explicit `create_service()` call.

**Q: What if I need to create if missing?**  
A: Handle the `AlreadyExists` error from `create_service()`, then call `get_service()`. See Example 2 above.

**Q: Can I create multiple services in parallel?**  
A: Yes, but handle potential race conditions. If two clients create the same service simultaneously, one gets `AlreadyExists`.

**Q: What permissions do I need for each operation?**  
A: Create needs `ServiceCreate`, read needs `ServiceRead`, delete needs `ServiceDelete`. Contact your admin if you lack permissions.

---

## Related Documentation

- [Commy Architecture Guide](../../ARCHITECTURE.md)
- [SDK Examples](examples/)
- [Permission Model](PERMISSION_MODEL.md)
- [Error Reference](ERROR_REFERENCE.md)
