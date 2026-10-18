# Commy SDK Rust Examples

This directory contains fully functional, self-contained examples demonstrating key features of the Commy SDK. Each example automatically manages its own Commy server instance, making them perfect for learning and testing.

## Quick Start

### Try the Basic Client Example

```bash
cargo run --example basic_client
```

This example demonstrates:
- ✅ Automatic Commy server setup and startup
- ✅ Client connection and authentication
- ✅ Service CRUD operations (create, read, delete)
- ✅ Heartbeat management
- ✅ Graceful disconnection and cleanup

**Output:**
```
╔════════════════════════════════════════╗
║    Commy Basic Client Example          ║
║    (Auto-managed Commy Server)         ║
╚════════════════════════════════════════╝

📦 Setting up Commy server
──────────────────────────
  ├─ Preparing server (download binary, generate certs)... ✅
  ├─ Starting server process... ✅
  └─ Server ready at: wss://127.0.0.1:8443

🔌 Connecting client
────────────────────
  ├─ Client ID: client_abc123...
  ├─ Connecting to server... ✅
  ├─ Tenant: my_tenant
  ├─ Authenticating with API key... ✅
  └─ Connected!

📋 Performing service operations
─────────────────────────────────
  ├─ Service name: config
  ├─ Creating service... ✅
  ├─ Reading service info... ✅
  ├─ Sending heartbeat... ✅
  ├─ Deleting service... ✅
  └─ Done!

🔌 Disconnecting
────────────────
  ├─ Disconnecting from server... ✅
  ├─ Stopping server...
  └─ (will happen automatically on exit)

╔════════════════════════════════════════╗
║  ✅ Example completed successfully!    ║
║  Server will be stopped automatically  ║
╚════════════════════════════════════════╝
```

## Available Examples

### 1. **basic_client** - Core Operations
```bash
cargo run --example basic_client
```

Learn the fundamentals:
- Explicit CRUD pattern (create/read/delete)
- Permission-aware operations
- Structured error handling
- Clean async patterns

**Time to run:** ~5 seconds

### 2. **hybrid_client** - Local & Remote Access
```bash
cargo run --example hybrid_client
```

Understand hybrid access patterns:
- Virtual service files for transparent access
- Local memory-mapping (when on same machine)
- Remote WSS synchronization (when remote)
- SIMD-based change detection
- File watching and monitoring

**Key insight:** Application code is identical whether using local or remote access!

**Time to run:** ~5 seconds

### 3. **permissions_example** - Multi-Client Authorization
```bash
cargo run --example permissions_example
```

See granular permission control in action:
- Admin client with create/read/delete
- Read-only client with permission restrictions
- Creator client with create/read (no delete)
- Permission violation detection

**Demonstrates:**
- Principle of least privilege
- Explicit vs implicit operations
- Clear audit trails

**Time to run:** ~7 seconds

## Architecture: Self-Contained Examples

Each example follows this pattern:

```rust
// 1. Start server automatically
let mut server = CommyServer::new(ServerConfig::default());
server.prepare().await?;    // Download binary, generate certs
server.start().await?;      // Start Commy server process

// 2. Create client pointing to auto-started server
let client = Client::new(server.url());
client.connect().await?;

// 3. Use the client
client.authenticate("tenant", auth::api_key("key".to_string())).await?;
// ... perform operations ...

// 4. Cleanup happens automatically
// - Client disconnects when out of scope
// - Server stops when dropped
```

## What's Happening Under the Hood

### Server Preparation
1. **Binary Download** - Looks for `commy` binary in:
   - `target/release/commy` (preferred)
   - `target/debug/commy` (fallback)
2. **Certificate Generation** - Creates self-signed TLS certificates for WSS
3. **Data Directory** - Creates temporary directory for server data

### Server Startup
1. **Process Spawn** - Starts Commy server with environment variables
2. **Ready Detection** - Polls TCP connection until server is ready
3. **Timeout Handling** - 5-second timeout with configurable polling (100ms intervals)

### Client Connection
1. **WebSocket Connection** - Connects to WSS server URL
2. **Authentication** - Sends credentials for tenant access
3. **Operations** - Executes CRUD operations or virtual file access

### Cleanup
1. **Graceful Disconnect** - Client disconnects cleanly
2. **Server Termination** - Server process is killed
3. **Port Release** - Resources released immediately

## Configuration & Customization

### Custom Server Port
```rust
let config = ServerConfig::default()
    .with_port(9000);  // Use custom port
let mut server = CommyServer::new(config);
```

### Custom Data Directory
```rust
let config = ServerConfig {
    port: 8443,
    http_port: 8000,
    data_dir: PathBuf::from("/tmp/commy-data"),
    cert_path: PathBuf::from("/tmp/server.crt"),
    key_path: PathBuf::from("/tmp/server.key"),
};
```

### Environment Variables (for server process)
Set these before calling `server.start()`:
```rust
std::env::set_var("COMMY_PORT", "9000");
std::env::set_var("COMMY_DATA_DIR", "/tmp/commy");
```

## Real-World Patterns

### Standalone Application
```rust
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Server starts on app launch
    let mut server = CommyServer::new(ServerConfig::default());
    server.prepare().await?;
    server.start().await?;

    // Your app code here
    let client = Client::new(server.url());
    // ... use client ...

    // Server automatically cleaned up when dropped
    Ok(())
}
```

### Testing with Real Server
```rust
#[tokio::test]
async fn test_my_feature() -> Result<(), Box<dyn std::error::Error>> {
    // Test with real Commy server
    let mut server = CommyServer::new(ServerConfig::default());
    server.prepare().await?;
    server.start().await?;

    let client = Client::new(server.url());
    client.connect().await?;

    // Assert your feature works
    assert!(client.authenticate(...).await.is_ok());

    Ok(())
}
```

## Troubleshooting

### `error: Commy binary not found`
**Solution:** Build the Commy server first
```bash
cd ..
cargo build --release --bin commy
cd commy-sdk-rust
cargo run --example basic_client
```

### `Connection refused` or `Network error`
**Possible causes:**
1. Server didn't start (check stdout messages)
2. Port already in use (customize with `.with_port()`)
3. Firewall blocking WSS connection

**Solution:** Try custom port
```rust
let config = ServerConfig::default().with_port(9000);
let mut server = CommyServer::new(config);
```

### `Permission denied` errors
**Expected behavior:**
- Read-only clients can't create/delete
- Unauthorized tenants fail authentication
- See `permissions_example` for examples

### TLS certificate warnings
**Note:** Examples use self-signed certificates. This is fine for development/testing. For production, provide real certificates:
```rust
let config = ServerConfig {
    cert_path: PathBuf::from("/path/to/real/cert.pem"),
    key_path: PathBuf::from("/path/to/real/key.pem"),
    ..Default::default()
};
```

## Performance Characteristics

| Metric                 | Value        |
| ---------------------- | ------------ |
| Server startup         | ~500ms       |
| Client connection      | ~50-100ms    |
| Authentication         | ~20-50ms     |
| CRUD operation         | ~10-30ms     |
| Heartbeat              | ~5ms         |
| **Total example time** | ~5-7 seconds |

## Learning Path

**Beginner:**
1. Start with `basic_client` - understand core patterns
2. Read the example code (well-commented)
3. Modify the example: try different tenants, service names, etc.

**Intermediate:**
1. Study `permissions_example` - learn authorization model
2. Create multiple clients in one app
3. Try error handling: what happens if you authenticate twice?

**Advanced:**
1. Explore `hybrid_client` - understand virtual files
2. Integrate with your own application
3. Build custom concurrency patterns (versioning, CAS, etc.)

## Integration Testing

Run integration tests that use the server infrastructure:

```bash
cargo test --test integration_examples -- --ignored --nocapture
```

Tests available:
- ✅ `test_server_startup` - Server setup and teardown
- ✅ `test_client_connection` - Basic connectivity
- ✅ `test_client_authentication` - Auth flows
- ✅ `test_basic_client_example_pattern` - Full workflow
- ✅ `test_multiple_clients` - Concurrent clients

## Next Steps

### Use Examples in Your Project

Copy the pattern from any example into your own code:
```bash
cp examples/basic_client.rs my_app/src/main.rs
# Modify to your needs
```

### Extend the Examples

Add new examples for your use cases:
```bash
# Create new example
echo 'use commy_sdk_rust::*;

#[tokio::main]
async fn main() {
    let mut server = CommyServer::new(ServerConfig::default());
    // ... your code ...
}' > examples/my_example.rs

# Run it
cargo run --example my_example
```

### Production Deployment

When deploying to production:
1. ✅ Use pre-compiled binary (not downloading)
2. ✅ Use real TLS certificates
3. ✅ Configure proper data directories
4. ✅ Set environment variables as needed
5. ✅ Implement error recovery and logging

## Resources

- **API Reference:** Check module documentation with `cargo doc --open`
- **Architecture Guide:** See [../../ARCHITECTURE.md](../../ARCHITECTURE.md)
- **Copilot Instructions:** See [../../.github/copilot-instructions.md](../../.github/copilot-instructions.md)

## Support

For issues specific to examples:
1. Check this README's Troubleshooting section
2. Review example source code (heavily commented)
3. Check integration tests for working patterns
4. Review SDK documentation for API details

---

**Happy learning! 🚀**

All examples are designed to be:
- ✅ Self-contained (no external setup)
- ✅ Well-documented (inline comments)
- Proper error handling
- ✅ Educational (demonstrate key patterns)
- ✅ Easy to modify (copy and customize)
