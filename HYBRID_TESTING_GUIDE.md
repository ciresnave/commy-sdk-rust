# Hybrid Architecture Testing Guide

## Test Categories

### 1. Virtual File Tests

#### Test: Basic Variable Registration
```rust
#[tokio::test]
async fn test_virtual_file_register_variable() {
    let vf = VirtualVariableFile::new(256);
    
    let meta = VariableMetadata::new("test_var", 0, 8, 1);
    assert!(vf.register_variable(meta).await.is_ok());
    
    let vars = vf.list_variables().await;
    assert!(vars.iter().any(|v| v.name == "test_var"));
}
```

#### Test: Write and Read Variable
```rust
#[tokio::test]
async fn test_virtual_file_write_read() {
    let vf = VirtualVariableFile::new(256);
    
    let meta = VariableMetadata::new("counter", 0, 8, 1);
    vf.register_variable(meta).await.unwrap();
    
    let data = vec![0, 0, 0, 0, 0, 0, 0, 42];
    vf.write_variable("counter", &data).await.unwrap();
    
    let read_data = vf.read_variable_slice("counter").await.unwrap();
    assert_eq!(read_data, data);
}
```

#### Test: Change Tracking
```rust
#[tokio::test]
async fn test_virtual_file_change_tracking() {
    let vf = VirtualVariableFile::new(256);
    
    let meta1 = VariableMetadata::new("var1", 0, 4, 1);
    let meta2 = VariableMetadata::new("var2", 4, 4, 1);
    vf.register_variable(meta1).await.unwrap();
    vf.register_variable(meta2).await.unwrap();
    
    // Write first variable
    vf.write_variable("var1", &[1, 2, 3, 4]).await.unwrap();
    
    let changed = vf.changed_variables().await;
    assert!(changed.contains(&"var1".to_string()));
    assert!(!changed.contains(&"var2".to_string()));
}
```

#### Test: Shadow Synchronization
```rust
#[tokio::test]
async fn test_virtual_file_shadow_sync() {
    let vf = VirtualVariableFile::new(256);
    let meta = VariableMetadata::new("test", 0, 8, 1);
    vf.register_variable(meta).await.unwrap();
    
    // Write and mark changed
    vf.write_variable("test", &[1; 8]).await.unwrap();
    assert!(!vf.changed_variables().await.is_empty());
    
    // Sync shadow
    vf.sync_shadow().await.unwrap();
    assert!(vf.changed_variables().await.is_empty());
    
    // Shadow should match current
    let current = vf.bytes().await;
    let shadow = vf.shadow_bytes().await;
    assert_eq!(current, shadow);
}
```

### 2. FileAccessor Tests

#### Test: LocalFileAccessor Creation
```rust
#[tokio::test]
async fn test_local_file_accessor_new() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.mem");
    
    // Create test file
    fs::write(&file_path, vec![0u8; 1024]).unwrap();
    
    let accessor = LocalFileAccessor::new(&file_path).await.unwrap();
    assert!(accessor.is_local().await);
    assert_eq!(accessor.file_size().await.unwrap(), 1024);
}
```

#### Test: RemoteFileAccessor Write/Read
```rust
#[tokio::test]
async fn test_remote_file_accessor_write_read() {
    let initial = vec![0u8; 256];
    let accessor = RemoteFileAccessor::new(initial.clone());
    
    // Write data
    let test_data = vec![42u8; 8];
    accessor.write_bytes(0, &test_data).await.unwrap();
    
    // Read back
    let read_data = accessor.read_bytes(0, 8).await.unwrap();
    assert_eq!(read_data, test_data);
}
```

#### Test: LocalFileAccessor Slice Access
```rust
#[tokio::test]
async fn test_local_file_accessor_slice() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.mem");
    
    let test_data: Vec<u8> = (0..256).map(|i| i as u8).collect();
    fs::write(&file_path, &test_data).unwrap();
    
    let accessor = LocalFileAccessor::new(&file_path).await.unwrap();
    let slice = accessor.as_slice().unwrap();
    
    assert_eq!(slice.len(), 256);
    assert_eq!(&slice[0..4], &[0, 1, 2, 3]);
}
```

### 3. File Watcher Tests

#### Test: Watcher Creation
```rust
#[tokio::test]
async fn test_watcher_new() {
    let temp_dir = TempDir::new().unwrap();
    let watcher = VariableFileWatcher::new(temp_dir.path())
        .await
        .unwrap();
    
    assert!(watcher.start_watching().await.is_ok());
    watcher.stop_watching().await.unwrap();
}
```

#### Test: Temp File Creation
```rust
#[tokio::test]
async fn test_create_temp_service_file() {
    let path = create_temp_service_file("test_service").await.unwrap();
    
    assert!(path.exists());
    assert!(path.to_string_lossy().contains("service_"));
    
    // On Unix, check permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::metadata(&path).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o600);
    }
}
```

#### Test: File Change Detection
```rust
#[tokio::test]
async fn test_watcher_file_change() {
    let temp_dir = TempDir::new().unwrap();
    let watcher = VariableFileWatcher::new(temp_dir.path())
        .await
        .unwrap();
    
    // Register virtual file
    let vf = VirtualVariableFile::new(256);
    let meta = VariableMetadata::new("test", 0, 8, 1);
    vf.register_variable(meta).await.unwrap();
    
    watcher.register_virtual_file("svc_1", vf.clone()).await.unwrap();
    watcher.start_watching().await.unwrap();
    
    // Modify file in temp directory
    let file_path = temp_dir.path().join("service_svc_1.mem");
    fs::write(&file_path, vec![42u8; 256]).unwrap();
    
    // Should detect change
    tokio::time::sleep(Duration::from_millis(150)).await; // Wait for debounce
    
    let event = tokio::time::timeout(
        Duration::from_secs(2),
        watcher.next_change()
    ).await;
    
    assert!(event.is_ok());
    watcher.stop_watching().await.unwrap();
}
```

### 4. SIMD Comparison Tests

#### Test: SIMD Compare Ranges
```rust
#[tokio::test]
async fn test_simd_compare_ranges() {
    let mut current = vec![0u8; 256];
    let mut shadow = vec![0u8; 256];
    
    // Modify a region in current
    current[32..40].copy_from_slice(&[42u8; 8]);
    
    let ranges = VirtualVariableFile::compare_ranges(&current, &shadow).await.unwrap();
    
    // Should detect change in byte range 32-40
    assert!(ranges.iter().any(|(start, end)| *start <= 32 && *end >= 40));
}
```

#### Test: AVX-512 Path (if available)
```rust
#[test]
fn test_simd_avx512_available() {
    // Check if AVX-512 is available
    if is_x86_feature_detected!("avx512f") {
        let current = vec![0u8; 1024];
        let mut shadow = vec![0u8; 1024];
        shadow[256] = 99; // One byte different
        
        let ranges = compare_with_avx512(&current, &shadow).unwrap();
        assert!(!ranges.is_empty());
    }
}
```

#### Test: AVX2 Fallback
```rust
#[test]
fn test_simd_avx2_fallback() {
    if is_x86_feature_detected!("avx2") && !is_x86_feature_detected!("avx512f") {
        let current = vec![0u8; 512];
        let mut shadow = vec![0u8; 512];
        shadow[256] = 99;
        
        let ranges = compare_with_avx2(&current, &shadow).unwrap();
        assert!(!ranges.is_empty());
    }
}
```

#### Test: u64 Fallback
```rust
#[test]
fn test_simd_u64_fallback() {
    let current = vec![0u8; 128];
    let mut shadow = vec![0u8; 128];
    shadow[64] = 99;
    
    let ranges = compare_with_u64(&current, &shadow);
    assert!(!ranges.is_empty());
}
```

### 5. Client Hybrid Tests

#### Test: Init File Watcher
```rust
#[tokio::test]
async fn test_client_init_file_watcher() {
    let client = Client::new("wss://localhost:9000");
    
    assert!(client.init_file_watcher().await.is_ok());
    assert!(client.stop_file_monitoring().await.is_ok());
}
```

#### Test: Get Virtual Service File
```rust
#[tokio::test]
async fn test_client_get_virtual_service_file() {
    let client = Client::new("wss://localhost:9000");
    client.init_file_watcher().await.unwrap();
    
    let vf = client.get_virtual_service_file("tenant", "svc")
        .await
        .unwrap();
    
    assert!(vf.is_some());
}
```

#### Test: File Monitoring Lifecycle
```rust
#[tokio::test]
async fn test_client_file_monitoring_lifecycle() {
    let client = Client::new("wss://localhost:9000");
    
    // Initialize
    client.init_file_watcher().await.unwrap();
    
    // Start monitoring
    client.start_file_monitoring().await.unwrap();
    
    // Try non-blocking get
    let event = client.try_get_file_change().await.unwrap();
    // Should be None initially (no changes)
    assert!(event.is_none());
    
    // Stop monitoring
    client.stop_file_monitoring().await.unwrap();
}
```

### 6. Integration Tests

#### Test: Local + Remote Transparency
```rust
#[tokio::test]
async fn test_local_remote_transparency() {
    // Create two clients - one local, one remote (simulated)
    let local_client = create_local_client().await;
    let remote_client = create_remote_client().await;
    
    // Both should have same API
    let local_vf = local_client.get_virtual_service_file("t", "s").await.unwrap().unwrap();
    let remote_vf = remote_client.get_virtual_service_file("t", "s").await.unwrap().unwrap();
    
    // Same operations work identically
    let meta = VariableMetadata::new("test", 0, 8, 1);
    assert!(local_vf.register_variable(meta.clone()).await.is_ok());
    assert!(remote_vf.register_variable(meta).await.is_ok());
    
    let data = vec![1, 2, 3, 4, 5, 6, 7, 8];
    assert!(local_vf.write_variable("test", &data).await.is_ok());
    assert!(remote_vf.write_variable("test", &data).await.is_ok());
    
    let local_read = local_vf.read_variable_slice("test").await.unwrap();
    let remote_read = remote_vf.read_variable_slice("test").await.unwrap();
    assert_eq!(local_read, remote_read);
}
```

#### Test: Multi-Variable Change Detection
```rust
#[tokio::test]
async fn test_multi_variable_change_detection() {
    let vf = VirtualVariableFile::new(1024);
    
    // Register multiple variables
    for i in 0..10 {
        let meta = VariableMetadata::new(
            format!("var_{}", i),
            (i * 64) as u64,
            64,
            1,
        );
        vf.register_variable(meta).await.unwrap();
    }
    
    // Write to odd-indexed variables
    for i in (1..10).step_by(2) {
        vf.write_variable(&format!("var_{}", i), &vec![42u8; 64])
            .await
            .unwrap();
    }
    
    let changed = vf.changed_variables().await;
    assert_eq!(changed.len(), 5); // 1, 3, 5, 7, 9
    
    for i in (1..10).step_by(2) {
        assert!(changed.contains(&format!("var_{}", i)));
    }
}
```

### 7. Error Handling Tests

#### Test: Invalid Variable Access
```rust
#[tokio::test]
async fn test_invalid_variable_name() {
    let vf = VirtualVariableFile::new(256);
    
    let result = vf.read_variable_slice("nonexistent").await;
    assert!(matches!(result, Err(CommyError::VariableNotFound(_))));
}
```

#### Test: Out of Bounds Write
```rust
#[tokio::test]
async fn test_out_of_bounds_write() {
    let vf = VirtualVariableFile::new(256);
    let meta = VariableMetadata::new("test", 0, 8, 1);
    vf.register_variable(meta).await.unwrap();
    
    // Try to write more data than allocated
    let result = vf.write_variable("test", &vec![0u8; 16]).await;
    assert!(matches!(result, Err(CommyError::InvalidOffset(_))));
}
```

#### Test: File Permission Error
```rust
#[tokio::test]
#[cfg(unix)]
async fn test_file_permission_denied() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("readonly.mem");
    
    fs::write(&file_path, vec![0u8; 256]).unwrap();
    
    // Make file read-only
    let perms = fs::Permissions::from_mode(0o444);
    fs::set_permissions(&file_path, perms).unwrap();
    
    // Should fail to access
    let result = LocalFileAccessor::new(&file_path).await;
    assert!(result.is_err());
}
```

## Performance Benchmarks

### Benchmark: SIMD Comparison Speed

```rust
#[bench]
fn bench_compare_avx512(b: &mut Bencher) {
    let current = vec![0u8; 4096];
    let shadow = vec![0u8; 4096];
    
    b.iter(|| {
        compare_with_avx512(&current, &shadow)
    });
}

#[bench]
fn bench_compare_avx2(b: &mut Bencher) {
    let current = vec![0u8; 4096];
    let shadow = vec![0u8; 4096];
    
    b.iter(|| {
        compare_with_avx2(&current, &shadow)
    });
}

#[bench]
fn bench_compare_u64(b: &mut Bencher) {
    let current = vec![0u8; 4096];
    let shadow = vec![0u8; 4096];
    
    b.iter(|| {
        compare_with_u64(&current, &shadow)
    });
}
```

### Benchmark: Variable Access

```rust
#[bench]
fn bench_read_variable_local(b: &mut Bencher) {
    // Setup local file accessor
    let vf = create_virtual_file_with_variables(100);
    
    b.iter(|| {
        vf.read_variable_slice("var_50")
    });
}

#[bench]
fn bench_write_variable_local(b: &mut Bencher) {
    let vf = create_virtual_file_with_variables(100);
    let data = vec![42u8; 8];
    
    b.iter(|| {
        vf.write_variable("var_50", &data)
    });
}
```

## Continuous Testing

### Recommended CI Pipeline

1. **Unit Tests** - Run all module tests
   ```bash
   cargo test --lib
   ```

2. **SIMD Availability Tests** - On multiple CPU architectures
   ```bash
   cargo test --features simd-avx512
   cargo test --features simd-avx2
   cargo test --features simd-u64
   ```

3. **Integration Tests** - With mock server
   ```bash
   cargo test --test integration_tests
   ```

4. **Benchmarks** - Track performance regression
   ```bash
   cargo bench
   ```

5. **Stress Tests** - Many concurrent clients/variables
   ```bash
   cargo test --test stress_tests -- --nocapture
   ```

## Edge Cases to Test

1. Empty variables (0-byte size)
2. Very large files (> 1GB)
3. Many variables (> 10,000)
4. Rapid change events (100s per second)
5. Network disconnection during remote operations
6. File permission changes during monitoring
7. Concurrent access from multiple threads
8. Shadow synchronization under high load
