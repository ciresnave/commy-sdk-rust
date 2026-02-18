# Commy Hybrid Client Architecture

## Overview

The Commy Rust SDK now implements a **fully hybrid client model** that provides transparent abstraction over both local and remote variable file access. Application code never needs to know whether it's using:

- **Local Direct Memory Mapping** - Zero-copy access for processes on the same machine
- **Remote WSS Synchronization** - Efficient in-memory buffering for remote clients

## Architecture

### Virtual Variable Files

At the core is the `VirtualVariableFile` abstraction:

```
┌────────────────────────────────────────────┐
│   Application Code (Unchanged)             │
│   Reads/writes variables transparently    │
└────────────────────────────────────────────┘
              ↓
┌────────────────────────────────────────────┐
│   VirtualVariableFile                      │
│   • Zero-copy variable references          │
│   • Raw byte access                        │
│   • Shadow copy for change detection       │
└────────────────────────────────────────────┘
              ↓
┌────────────────────────────────────────────┐
│   FileAccessor Trait                       │
│   ├─ LocalFileAccessor (memory mapping)   │
│   └─ RemoteFileAccessor (in-memory buffer)│
└────────────────────────────────────────────┘
              ↓
┌────────────────────────────────────────────┐
│   Transport Layer                          │
│   ├─ memmap2 (local)                      │
│   └─ WSS + Tokio (remote)                 │
└────────────────────────────────────────────┘
```

### Shadow Copy Pattern

Each virtual file maintains two copies:

1. **Current Bytes** - Latest state from transport
2. **Shadow Bytes** - Last known state sent to server

This enables efficient change detection:

```
Current: [v1_data | v2_data | v3_data]
Shadow:  [v1_old  | v2_old  | v3_old ]
                    ↓
            SIMD Diff Detection
                    ↓
            Identifies which variables
            changed using AVX-512, AVX2,
            or u64-sized comparisons
```

## Supported Modes

### Mode 1: Local Direct Memory Mapping

**When to use:**
- Client and server on same machine
- Highest performance required
- Zero-copy access desired

**How it works:**
1. Client requests service file path from server
2. Server validates permissions and returns path
3. Client memory-maps the `.mem` file directly
4. All reads/writes are direct to mapped memory
5. File watcher detects changes and notifies server

**Performance:** Sub-microsecond variable access

### Mode 2: Remote WSS Synchronization

**When to use:**
- Client on different machine
- Only option for true remote access
- Network conditions variable

**How it works:**
1. Client connects via WSS
2. Client authenticates to one or more tenants
3. Client requests a service from a tenant
4. Server sends current variable file for that service to client (8 bytes at a time)
5. Client stores each 8-byte chunk into both a virtual variable file and a shadow copy
6. Client includes the virtual file in a directory with a file watcher watching it
7. When file watcher detects a change:
   - Client compares the variable file to the shadow file one chunk at a time (8 bytes)
   - Identifies which variables changed
   - Sends all changed variables to server as updates

**Performance:** Microseconds to milliseconds (network-dependent)

### Mode 3: Hybrid (Recommended)

**Automatic mode selection:**
- Try local memory mapping first
- Detect if server is on same machine
- Fall back to WSS if local unavailable
- Transparent to application

**Implemented in:** `Client::init_file_watcher()` + file watching

## API Usage

### Simple Pattern - Consolidated Initialization

```rust
// ONE line: Initialize with everything needed
let client = Client::initialize(
    "wss://localhost:9000",
    "tenant",
    AuthCredentials::api_key("key")
).await?;

// Get virtual file (works for local or remote)
let vf = client.get_virtual_service_file("tenant", "service").await?;

// Register variables
let meta = VariableMetadata::new("counter", 0, 8, 1);
vf.register_variable(meta).await?;

// Read/write transparently
vf.write_variable("counter", &[0, 0, 0, 0, 0, 0, 0, 42]).await?;
let data = vf.read_variable_slice("counter").await?;

// Monitor for changes (already initialized by initialize())
if let Some(change) = client.wait_for_file_change().await? {
    println!("Changed: {:?}", change.changed_variables);
}
```

### What `initialize()` Does (Automatic)

The `Client::initialize()` method is the recommended entry point. It automatically:

1. Creates a new client instance
2. Connects to the server via WSS
3. Authenticates to the specified tenant
4. Initializes the file watcher for hybrid mode
5. Starts file monitoring for change detection

**Before:** You had to call 5 separate methods in the right order
```rust
let client = Client::new("wss://localhost:9000");
client.connect().await?;
client.authenticate("tenant", creds).await?;
client.init_file_watcher().await?;
client.start_file_monitoring().await?;
```

**Now:** One consolidated call that bundles all prerequisites
```rust
let client = Client::initialize("wss://localhost:9000", "tenant", creds).await?;
```

### Advanced Pattern - SIMD Change Detection

```rust
// Get current and shadow bytes
let current = vf.bytes().await;
let shadow = vf.shadow_bytes().await;

// Use SIMD to find changed regions
let byte_ranges = VirtualVariableFile::compare_ranges(&current, &shadow).await?;

// Map byte changes to variables
let changed_vars = vf.find_changed_variables_from_diff(&byte_ranges).await?;

// Sync shadow after sending updates
vf.sync_shadow().await?;
```

## SIMD Change Detection

### Strategy

The SDK uses the widest available CPU operations for fastest comparison:

1. **AVX-512** (64-byte chunks) - Newest x86_64 CPUs
2. **AVX2** (32-byte chunks) - Most modern CPUs
3. **u64** (8-byte chunks) - Universal fallback

### Example

For a 256-byte service file with one changed variable:

```
Without SIMD: 256 byte-by-byte comparisons
With AVX-512: 4 × 64-byte comparisons = 256 bytes checked
With AVX2:    8 × 32-byte comparisons = 256 bytes checked
With u64:    32 ×  8-byte comparisons = 256 bytes checked
```

Detection identifies **exact byte ranges** changed, enabling precise variable identification.

## Temporary File Management

### Directory Structure

```
~/.cache/commy_virtual_files/          (Linux/macOS)
C:\Users\<user>\AppData\Local\commy_virtual_files\ (Windows)
                    ↓
            service_<uuid>.mem
            service_<uuid>.mem
            ...
```

### Security

- **User-only permissions** (0o600 on Unix)
- **Isolated to current user** - No cross-user access
- **Automatic cleanup** - Deleted when client disconnects
- **Temp directory** - OS provides secure location

## File Watcher

### Behavior

```rust
// Create and start watcher
client.init_file_watcher().await?;

// Get events (blocks until change)
let event = client.wait_for_file_change().await?;

// Or non-blocking
if let Some(event) = client.try_get_file_change().await? {
    // Handle change
}

// Stop when done
client.stop_file_monitoring().await?;
```

### Change Event

```rust
FileChangeEvent {
    file_path: PathBuf,                    // Path to changed file
    service_id: String,                   // Which service changed
    changed_variables: Vec<String>,       // Variable names
    byte_ranges: Vec<(u64, u64)>,        // Exact byte ranges
}
```

## Protocol Extensions

### New Client Messages

```rust
// Request service file path (local mode)
ClientMessage::GetServiceFilePath {
    tenant_id: String,
    service_name: String,
}

// Report changes detected locally
ClientMessage::ReportVariableChanges {
    service_id: String,
    changed_variables: Vec<String>,
    new_values: Vec<(String, Vec<u8>)>,
}
```

### New Server Messages

```rust
// Provide file path for memory mapping
ServerMessage::ServiceFilePath {
    service_id: String,
    file_path: String,
    file_size: u64,
}

// Acknowledge received changes
ServerMessage::VariableChangesAcknowledged {
    service_id: String,
    changed_variables: Vec<String>,
}
```

## Performance Characteristics

| Operation        | Local     | Remote    |
| ---------------- | --------- | --------- |
| Variable read    | <1 μs     | 1-100 μs  |
| Variable write   | <1 μs     | 1-100 μs  |
| Change detection | 10-100 μs | 10-100 μs |
| SIMD compare     | 1-10 μs   | N/A       |

*Note: Remote performance depends on network latency*

## Example: Hybrid Application

```rust
use commy_client::Client;
use commy_client::auth::AuthCredentials;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize client with all prerequisites
    let client = Client::initialize(
        "wss://commy-server:9000",
        "my_tenant",
        AuthCredentials::api_key("key")
    ).await?;
    
    // Get virtual file (handles local/remote automatically)
    let vf = client.get_virtual_service_file("my_tenant", "cache").await?;
    
    // Register variables
    let meta = VariableMetadata::new("user_id".to_string(), 0, 8, 1);
    vf.register_variable(meta).await?;
    
    // Application code is transport-agnostic!
    vf.write_variable("user_id", &[0, 0, 0, 0, 0, 0, 0, 123]).await?;
    let data = vf.read_variable_slice("user_id").await?;
    
    // Monitor for changes from other processes/clients
    while let Some(event) = client.wait_for_file_change().await? {
        for var_name in event.changed_variables {
            let updated = vf.read_variable_slice(&var_name).await?;
            println!("Variable {} changed to {:?}", var_name, updated);
        }
    }
    
    Ok(())
}
```

## Key Benefits

1. **Unified API** - Same code works for local and remote
2. **Zero-copy for local** - Direct memory access when possible
3. **Efficient remote** - SIMD change detection, minimal bandwidth
4. **Transparent fallback** - Local fails gracefully to remote
5. **Shadow-based sync** - Efficient incremental updates
6. **SIMD acceleration** - Automatic use of CPU capabilities
7. **User isolation** - Secure temporary file handling
8. **No codec overhead** - Raw bytes when local

## Migration Path

Existing code using the original API:

```rust
// Old way (still works)
client.read_variable("tenant", "svc", "var").await?;
client.write_variable("tenant", "svc", "var", data).await?;
```

Can gradually adopt hybrid:

```rust
// New way (with hybrid benefits)
let vf = client.get_virtual_service_file("tenant", "svc").await?;
vf.read_variable_slice("var").await?;
vf.write_variable("var", data).await?;
```

Both patterns coexist in the same client!
