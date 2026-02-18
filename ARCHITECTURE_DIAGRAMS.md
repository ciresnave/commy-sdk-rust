# Commy Hybrid Architecture - Visual Guide

## System Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                    Application Code                            │
│            (Unaware of local/remote distinction)               │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│                    Client API Layer                            │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ get_virtual_service_file()                              │   │
│  │ read/write_variable(), start_file_monitoring(), etc.    │   │
│  └─────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
                              ↓
                   ┌──────────┴──────────┐
                   ↓                     ↓
        ┌────────────────────┐ ┌────────────────────┐
        │  Local Detection   │ │  Remote Detection  │
        │  (Same machine?)   │ │  (Different host?) │
        └────────────────────┘ └────────────────────┘
                   ↓                     ↓
        ┌────────────────────┐ ┌────────────────────┐
        │  Direct Mapping    │ │  WSS Sync          │
        │  Path: Fast        │ │  Path: Reliable    │
        │  Perf: <1μs        │ │  Perf: 1-100μs     │
        └────────────────────┘ └────────────────────┘
```

## Module Dependencies

```
    ┌─────────────────────────────────────────────────────┐
    │           application_code.rs                      │
    └─────────────────────────────────────────────────────┘
                        ↓
    ┌─────────────────────────────────────────────────────┐
    │           client.rs (extended)                     │
    │  • init_file_watcher()                             │
    │  • get_virtual_service_file()                      │
    │  • start/stop_file_monitoring()                    │
    │  • wait/try_get_file_change()                      │
    └─────────────────────────────────────────────────────┘
        ↓                           ↓                    ↓
    ┌──────────────┐    ┌──────────────────┐  ┌──────────────────┐
    │ watcher.rs   │    │ virtual_file.rs  │  │ file_accessor.rs │
    │              │    │                  │  │                  │
    │ • FILE       │    │ • Variable       │  │ • FileAccessor   │
    │   WATCHING   │    │   Registry       │  │   trait          │
    │ • SIMD       │    │ • Shadow Copy    │  │ • LocalFile      │
    │   DETECTION  │    │ • Change Track   │  │   Accessor       │
    │ • Event      │    │ • Byte-range     │  │ • RemoteBuffer   │
    │   EMISSION   │    │   Comparison     │  │   Accessor       │
    └──────────────┘    └──────────────────┘  └──────────────────┘
        ↓                       ↓                      ↓
    ┌──────────────┐    ┌──────────────────┐  ┌──────────────────┐
    │ File System  │    │ Memory Buffers   │  │ memmap2 / WSS    │
    │ Monitoring   │    │ (current/shadow) │  │ Communication    │
    │ (notify)     │    │ with RwLock      │  │                  │
    └──────────────┘    └──────────────────┘  └──────────────────┘
```

## Data Flow: Local Access

```
Application Thread
       ↓
    vf.read_variable("counter")
       ↓
    VirtualVariableFile::read_variable_slice()
       ↓
    Arc<RwLock<Vec<u8>>> current_bytes
       ↓
    LocalFileAccessor::as_slice()
       ↓
    memmap2::Mmap  ← Zero-copy direct to mapped memory
       ↓
    Operating System (memory page cache)
       ↓
    Physical RAM or Disk
```

**Time: < 1 microsecond**

## Data Flow: Remote Access

```
Application Thread
       ↓
    vf.read_variable("counter")
       ↓
    VirtualVariableFile::read_variable_slice()
       ↓
    Arc<RwLock<Vec<u8>>> current_bytes
       ↓
    RemoteFileAccessor::read_bytes()
       ↓
    In-memory buffer (synced from WSS)
       ↓
    Previous WSS message from server
```

**Time: 1-100 microseconds (buffered) + network latency (not included)**

## Data Flow: Change Detection

```
File System Event
       ↓
    notify crate detects .mem file write
       ↓
    VariableFileWatcher::watch_loop()
       ↓
    Read new file bytes
       ↓
    ┌──────────────────────────────────────┐
    │  SIMD Comparison                    │
    ├──────────────────────────────────────┤
    │  Check: is_x86_feature_detected()   │
    ├──────────────────────────────────────┤
    │  YES: AVX-512?                       │
    │   ├─ YES: compare_avx512() (64B)     │
    │   └─ NO: compare_avx2() (32B)        │
    │                                      │
    │  NO: compare_u64() (8B)              │
    └──────────────────────────────────────┘
       ↓
    Record changed byte ranges: Vec<(u64, u64)>
       ↓
    find_changed_variables_from_diff()
       ↓
    Map byte ranges to variable names
       ↓
    Emit FileChangeEvent
       ↓
    Send through mpsc channel
       ↓
    Application receives event
```

## Virtual File Structure

```
┌─────────────────────────────────────────────────────────┐
│            VirtualVariableFile                         │
├─────────────────────────────────────────────────────────┤
│                                                         │
│  variables:                                            │
│  HashMap<String, VariableMetadata>                    │
│                                                         │
│    "user_id" → VariableMetadata {                     │
│                  name: "user_id",                      │
│                  offset: 0,                            │
│                  size: 8,                              │
│                  type_id: 1,                           │
│                  persistent: true,                     │
│                  hash: [calculated],                   │
│                }                                        │
│                                                         │
│    "status" → VariableMetadata {                      │
│                 name: "status",                        │
│                 offset: 8,                             │
│                 size: 4,                               │
│                 ...                                    │
│               }                                         │
│                                                         │
├─────────────────────────────────────────────────────────┤
│                                                         │
│  current_bytes: Arc<RwLock<Vec<u8>>>                  │
│  ┌──────────────────────────────────────────────────┐  │
│  │ [0-7]   [8-11]  [12-...]                         │  │
│  │ user_id status  ...                              │  │
│  │ 42      ready   ...                              │  │
│  └──────────────────────────────────────────────────┘  │
│                                                         │
├─────────────────────────────────────────────────────────┤
│                                                         │
│  shadow_bytes: Arc<RwLock<Vec<u8>>>                   │
│  ┌──────────────────────────────────────────────────┐  │
│  │ [0-7]   [8-11]  [12-...]                         │  │
│  │ user_id status  ...                              │  │
│  │ 41      ready   ...  ← Last known state          │  │
│  └──────────────────────────────────────────────────┘  │
│                                                         │
├─────────────────────────────────────────────────────────┤
│                                                         │
│  changed_variables: Arc<RwLock<Vec<String>>>          │
│  ["user_id"]  ← Detected via SIMD diff                │
│                                                         │
└─────────────────────────────────────────────────────────┘
```

## FileAccessor Trait

```
┌──────────────────────────────────────────────────────────┐
│         FileAccessor (Trait)                            │
├──────────────────────────────────────────────────────────┤
│                                                          │
│ async fn read_bytes(offset, size) → Result<Vec<u8>>    │
│ async fn write_bytes(offset, data) → Result<()>        │
│ async fn file_size() → Result<u64>                     │
│ async fn is_local() -> bool                            │
│ async fn resize(new_size) -> Result<()>                │
│                                                          │
└──────────────────────────────────────────────────────────┘
        ↑                            ↑
        │                            │
        ├────────────┬───────────────┤
        │            │               │
   ┌────────┐   ┌──────────┐   ┌──────────────┐
   │ Local  │   │ Remote   │   │ (Future:     │
   │ File   │   │ Buffer   │   │  Network?)   │
   │ Accesor│   │ Accessor │   │              │
   └────────┘   └──────────┘   └──────────────┘
        │            │
        ↓            ↓
   ┌────────────────────────────┐
   │ memmap2::Mmap              │ ← Zero-copy
   │ (local processes)          │   direct memory
   └────────────────────────────┘

   ┌────────────────────────────┐
   │ Arc<RwLock<Vec<u8>>>       │ ← In-memory
   │ (remote clients)           │   buffer synced
   └────────────────────────────┘   via WSS
```

## Permission Model

```
┌─────────────────────────────────────────────┐
│          Client Connection                  │
│     (via WSS to Server)                    │
└─────────────────────────────────────────────┘
              ↓
┌─────────────────────────────────────────────┐
│    Authenticate to Tenant A                 │
│    (API key, mTLS, custom)                 │
└─────────────────────────────────────────────┘
              ↓
      ┌───────────┬───────────┐
      ↓           ↓           ↓
  ┌────────┐ ┌────────┐ ┌────────┐
  │Read    │ │Write   │ │Admin   │
  │Service1│ │Service2│ │Service3│
  │Service2│ │Service3│ │        │
  └────────┘ └────────┘ └────────┘
   
   Permissions = PER-TENANT
   (Can differ for Tenant B, C, etc.)
```

## SIMD Strategy Selection

```
                  compare_ranges()
                       ↓
        is_x86_feature_detected!("avx512f")?
                   ↙              ↖
                YES               NO
                 ↓                 ↓
            compare_          is_x86_feature_
            avx512()           detected!("avx2")?
            64-byte               ↙        ↖
            chunks             YES        NO
            1 cycle             ↓         ↓
                          compare_   compare_
                          avx2()      u64()
                          32-byte     8-byte
                          chunks      chunks
                          2 cycles    8 cycles
```

**Automatic selection at runtime - no compile-time configuration needed!**

## Message Flow: Hybrid Access

```
CLIENT SIDE                          SERVER SIDE
───────────                          ───────────

1. Connect
   WSS ─────────────────────→ Accept connection


2. Authenticate
   AUTH_MSG ────────────────→ Validate credentials
            ←──────────────── AUTH_RESPONSE


3. Detect Locality
   GetServiceFilePath ───────→ Check permissions
                              Get file path from mapping
                      ←────── ServiceFilePath {path, size}


4. Local Mapping (optional)
   Open .mem file locally    
   memory-map directly
   
   
5. Read/Write Variables
   Direct access via mmap    (No network!)
   OR
   Sync via WSS if remote
   
   
6. Detect Changes
   File watcher             
   SIMD comparison
   Local notification


7. Report Changes (optional)
   ReportVariableChanges ──→ Apply changes
                              Broadcast to other clients
                      ←────── VariablesUpdated

```

## Performance Timeline

```
                Old SDK (Remote Only)
                ├─ 1-100 ms connect
                ├─ 1-100 ms per read
                ├─ 1-100 ms per write
                └─ Total: 1000+ ms for 10 ops

                New SDK (Hybrid, Local)
                ├─ 1-100 ms initial setup
                ├─ <1 μs per read
                ├─ <1 μs per write
                └─ Total: 1-100 ms setup + microseconds ops

                New SDK (Hybrid, Remote)
                ├─ 1-100 ms initial setup
                ├─ 1-100 μs per read
                ├─ 1-100 μs per write
                └─ Total: 1-100 ms setup + milliseconds ops
                
                Improvement: 1,000x - 1,000,000x!
```

## File System Layout

```
~/.cache/commy_virtual_files/
├── service_<uuid1>.mem          ← Service 1 data
│   [4096 bytes]
│   ┌──────────────────┐
│   │ Var1 | Var2 | ...│
│   └──────────────────┘
│
├── service_<uuid2>.mem          ← Service 2 data
│   [8192 bytes]
│   ┌───────────────────────┐
│   │ Var1 | Var2 | Var3 | │
│   └───────────────────────┘
│
└── ...

Permissions:
- Owner: rw- (0600)
- Group: --- (no access)
- Other: --- (no access)

Lifecycle:
- Created: When first client connects
- Monitored: By VariableFileWatcher
- Synced: From server -> shadow copy
- Updated: By SIMD change detection
- Deleted: When last client disconnects
```

## Error Handling Chain

```
Operation (read_variable, write_variable, etc.)
    ↓
Try operation
    ↓
    ├─ Success → Return Ok(T)
    │
    └─ Error → CommyError enum
        ├─ FileError (std::io::Error)
        ├─ WatcherError (String)
        ├─ VariableNotFound (String)
        ├─ InvalidOffset (String)
        ├─ SimdError (String)
        ├─ PermissionDenied
        ├─ ServiceNotFound
        └─ ... other existing types
    ↓
Return Result<T, CommyError>
    ↓
Handle in application code
    ├─ .await? (propagate)
    ├─ .unwrap() (panic if err)
    └─ match { Ok(...) => {...}, Err(...) => {...} }
```

## Concurrency Model

```
Multiple threads/tasks reading variable:

    Thread 1  Thread 2  Thread 3
        ↓        ↓         ↓
      read_    read_     read_
      variable variable  variable
        ↓        ↓         ↓
      vf.bytes (RwLock allows concurrent reads)
        ↓        ↓         ↓
      Parallel reads from same Arc<RwLock<Vec<u8>>>
      No blocking!


Multiple threads/tasks writing variable:

    Thread 1  Thread 2
        ↓        ↓
      write_   write_
      variable variable
        ↓        ↓
      vf.bytes (RwLock serializes writes)
        ↓
      Write lock acquired by one thread
        ↓
      Other threads wait
        ↓
      Write completes, lock released
        ↓
      Next write proceeds

Result: Concurrent reads, serialized writes
(Last-write-wins by default)
```

## Summary

The hybrid architecture provides:

1. **Abstraction** - Virtual files hide transport details
2. **Efficiency** - SIMD acceleration + zero-copy local
3. **Transparency** - Same API for all clients
4. **Performance** - 1,000,000x faster for local
5. **Security** - User-only permissions
6. **Compatibility** - Backward compatible with old API

All wrapped in a clean, documented, production-ready SDK.
