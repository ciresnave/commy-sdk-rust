# Commy Rust SDK: Quick Start Guide

**TL;DR**: Connect to Commy, authenticate, and use explicit CRUD operations. The SDK handles everything—permission checking and error recovery.

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
commy_client = { path = "../commy/ClientSDKs/rust-sdk" }
tokio = { version = "1.0", features = ["full"] }
```

## 5-Minute Quick Start

### Step 1: Connect and Authenticate

```rust
use commy_client::Client;
use commy_client::auth;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connect to Commy server
    let client = Client::new("wss://localhost:9000");
    client.connect().await?;
    println!("✓ Connected!");
    
    // Authenticate to a tenant
    let auth_ctx = client.authenticate(
        "my_tenant",
        auth::api_key("my_api_key".to_string())
    ).await?;
    println!("✓ Authenticated! Permissions: {:?}", auth_ctx.permissions);
    
    Ok(())
}
```

### Step 2: Explicit CRUD Operations

The SDK provides explicit CRUD methods with clear semantics:

```rust
// CREATE: Explicitly create a service
let service_id = client.create_service("my_tenant", "app_config").await?;
println!("✓ Created service: {}", service_id);

// READ: Safe read (no side effects, won't create)
let service = client.get_service("my_tenant", "app_config").await?;
println!("✓ Read service: {}", service.id);

// DELETE: Explicitly delete a service
client.delete_service("my_tenant", "app_config").await?;
println!("✓ Deleted service");
```

### Step 3: Handle Errors Properly

Each operation returns specific errors:

```rust
use commy_client::CommyError;

match client.create_service("my_tenant", "config").await {
    Ok(id) => println!("✓ Created: {}", id),
    Err(CommyError::AlreadyExists(_)) => {
        // Service already exists, get it instead
        let svc = client.get_service("my_tenant", "config").await?;
        println!("✓ Service exists: {}", svc.id);
    }
    Err(CommyError::PermissionDenied(msg)) => {
        eprintln!("✗ Need permission: {}", msg);
    }
    Err(e) => eprintln!("✗ Error: {}", e),
}
```

## Complete Example: End-to-End CRUD

```rust
use commy_client::{Client, CommyError, auth};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Connect and authenticate
    let client = Client::new("wss://localhost:9000");
    client.connect().await?;
    
    let _auth = client.authenticate(
        "my_org",
        auth::api_key("admin_key".to_string())
    ).await?;
    println!("✓ Connected and authenticated!");
    
    // 2. CREATE: Create a new service
    let service_id = match client.create_service("my_org", "user_config").await {
        Ok(id) => {
            println!("✓ Created service: {}", id);
            id
        }
        Err(CommyError::AlreadyExists(_)) => {
            println!("⚠ Service already exists, getting it...");
            let svc = client.get_service("my_org", "user_config").await?;
            svc.id
        }
        Err(e) => {
            eprintln!("✗ Failed to create: {}", e);
            return Err(e.into());
        }
    };
    
    // 3. READ: Get service metadata
    let service = client.get_service("my_org", "user_config").await?;
    println!("✓ Read service: {} (ID: {})", service.name, service.id);
    
    // 4. DELETE: Remove the service
    match client.delete_service("my_org", "user_config").await {
        Ok(_) => println!("✓ Deleted service"),
        Err(CommyError::NotFound(_)) => println!("⚠ Service not found"),
        Err(e) => {
            eprintln!("✗ Failed to delete: {}", e);
            return Err(e.into());
        }
    }
    
    // 5. Disconnect
    client.disconnect().await?;
    println!("✓ Disconnected!");
    
    Ok(())
}
```

## Key Design Principles

### Explicit Operations

All operations are explicit - no implicit side effects:

- **Create**: Use `create_service()` explicitly
- **Read**: Use `get_service()` for safe reading (no creation)
- **Delete**: Use `delete_service()` explicitly

### Permission Separation

Each operation requires specific permissions:

| Operation          | Permission      | Allows                |
| ------------------ | --------------- | --------------------- |
| `create_service()` | `ServiceCreate` | Create new services   |
| `get_service()`    | `ServiceRead`   | Read service metadata |
| `delete_service()` | `ServiceDelete` | Delete services       |

### Specific Error Handling

```rust
use commy_client::CommyError;

match operation.await {
    Ok(result) => println!("✓ Success"),
    Err(CommyError::NotFound(msg)) => eprintln!("✗ Not found: {}", msg),
    Err(CommyError::PermissionDenied(msg)) => eprintln!("✗ Permission denied: {}", msg),
    Err(CommyError::Unauthorized(msg)) => eprintln!("✗ Unauthorized: {}", msg),
    Err(CommyError::AlreadyExists(msg)) => eprintln!("✗ Already exists: {}", msg),
    Err(CommyError::ConnectionLost(msg)) => eprintln!("✗ Connection lost: {}", msg),
    Err(CommyError::Timeout) => eprintln!("✗ Timeout"),
    Err(e) => eprintln!("✗ Other error: {}", e),
}
```

## Common Patterns

### Idempotent Create

```rust
async fn ensure_service_exists(
    client: &Client,
    tenant: &str,
    service: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    match client.create_service(tenant, service).await {
        Ok(id) => Ok(id),
        Err(CommyError::AlreadyExists(_)) => {
            let svc = client.get_service(tenant, service).await?;
            Ok(svc.id)
        }
        Err(e) => Err(e.into()),
    }
}
```

### Safe Get with Fallback

```rust
async fn get_or_create(
    client: &Client,
    tenant: &str,
    service: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    match client.get_service(tenant, service).await {
        Ok(svc) => Ok(svc.id),
        Err(CommyError::NotFound(_)) => {
            match client.create_service(tenant, service).await {
                Ok(id) => Ok(id),
                Err(CommyError::AlreadyExists(_)) => {
                    // Another client created it, retry read
                    let svc = client.get_service(tenant, service).await?;
                    Ok(svc.id)
                }
                Err(e) => Err(e.into()),
            }
        }
        Err(e) => Err(e.into()),
    }
}
```

## Troubleshooting

### Connection refused

**Error**: Connection refused at wss://localhost:9000

**Solution**: Make sure Commy server is running:

```bash
cargo run --bin commy_server -- --port 9000
```

### Unauthorized

**Error**: Unauthorized

**Solution**: Check authentication credentials:

```rust
let result = client.authenticate("tenant", auth::api_key("correct_key")).await?;
```

### Permission denied

**Error**: PermissionDenied("Missing permission: ServiceCreate")

**Solution**: Request the required permission from your administrator.

### Timeout

**Error**: Timeout

**Solution**: Retry with exponential backoff:

```rust
let mut retries = 0;
loop {
    match client.create_service(tenant, svc).await {
        Ok(id) => return Ok(id),
        Err(CommyError::Timeout) if retries < 3 => {
            retries += 1;
            tokio::time::sleep(Duration::from_millis(100 * (2 ^ retries))).await;
        }
        Err(e) => return Err(e.into()),
    }
}
```

## Next Steps

1. **Full Documentation**: See [CRUD_API_REFERENCE.md](CRUD_API_REFERENCE.md)
2. **Real Examples**: Check `examples/basic_client.rs` and `examples/permissions_example.rs`
3. **Architecture**: Read [../../ARCHITECTURE.md](../../ARCHITECTURE.md)

---

For detailed API documentation, permission model details, and advanced examples, see [CRUD_API_REFERENCE.md](CRUD_API_REFERENCE.md).
