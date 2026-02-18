# SIMD Change Detection: Deep Dive

## Overview

The hybrid client uses SIMD (Single Instruction Multiple Data) operations for efficient change detection. This document explains the implementation, trade-offs, and tuning options.

## Why SIMD?

Traditional byte-by-byte comparison for a 4KB service file:

```
4096 bytes × 1 comparison = 4096 CPU instructions
```

With SIMD (AVX-512):

```
4096 bytes ÷ 64 bytes per instruction = 64 CPU instructions
64× faster!
```

### Real Numbers

| Comparison Size | Byte-by-Byte | AVX2     | AVX-512  |
| --------------- | ------------ | -------- | -------- |
| 1 KB            | ~1 μs        | ~0.05 μs | ~0.01 μs |
| 4 KB            | ~4 μs        | ~0.2 μs  | ~0.05 μs |
| 16 KB           | ~16 μs       | ~0.8 μs  | ~0.2 μs  |
| 64 KB           | ~64 μs       | ~3.2 μs  | ~0.8 μs  |

## SIMD Instruction Sets

### AVX-512 (64-byte chunks)

**Available on:** Intel Xeon (Skylake+), Intel Core (Raptor Lake+), AMD EPYC (9004+)

**Instructions:**
```
vpxorq zmm0, zmm1, zmm2    ; Compare two 512-bit (64-byte) registers
```

**Comparison logic:**
```rust
// AVX-512 version (conceptual)
for chunk in data.chunks(64) {
    let current_chunk = load_zmm(chunk);      // Load 64 bytes
    let shadow_chunk = load_zmm(shadow);      // Load 64 bytes
    let xor_result = xor(current_chunk, shadow_chunk);
    if xor_result != 0 {
        // This 64-byte chunk changed
        record_changed_range(offset..offset+64);
    }
}
```

### AVX2 (32-byte chunks)

**Available on:** Most modern CPUs (2013+)

**Instructions:**
```
vpxor ymm0, ymm1, ymm2     ; Compare two 256-bit (32-byte) registers
```

**Comparison logic:**
```rust
// AVX2 version (conceptual)
for chunk in data.chunks(32) {
    let current_chunk = load_ymm(chunk);
    let shadow_chunk = load_ymm(shadow);
    let xor_result = xor(current_chunk, shadow_chunk);
    if xor_result != 0 {
        record_changed_range(offset..offset+32);
    }
}
```

### u64 (8-byte chunks)

**Available on:** All CPUs (portable fallback)

**Operations:**
```rust
// u64 version
for chunk in data.chunks(8) {
    let current: u64 = read_u64(chunk);
    let shadow: u64 = read_u64(shadow);
    if current != shadow {
        record_changed_range(offset..offset+8);
    }
}
```

## Runtime Detection

The SDK automatically selects the best available instruction set:

```rust
#[cfg(target_arch = "x86_64")]
pub fn select_simd_strategy() -> SimdStrategy {
    if is_x86_feature_detected!("avx512f") {
        SimdStrategy::Avx512  // 64-byte chunks, ~20 Gb/s comparison
    } else if is_x86_feature_detected!("avx2") {
        SimdStrategy::Avx2    // 32-byte chunks, ~10 Gb/s comparison
    } else {
        SimdStrategy::U64     // 8-byte chunks, ~5 Gb/s comparison
    }
}

#[cfg(not(target_arch = "x86_64"))]
pub fn select_simd_strategy() -> SimdStrategy {
    SimdStrategy::U64  // Fallback for non-x86_64
}
```

## Implementation Details

### 1. Chunk-based Comparison

```rust
pub async fn compare_ranges(
    current: &[u8],
    shadow: &[u8],
) -> Result<Vec<(u64, u64)>> {
    let strategy = select_simd_strategy();
    
    match strategy {
        SimdStrategy::Avx512 => compare_avx512(current, shadow),
        SimdStrategy::Avx2 => compare_avx2(current, shadow),
        SimdStrategy::U64 => compare_u64(current, shadow),
    }
}
```

### 2. AVX-512 Implementation

```rust
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
pub fn compare_avx512(current: &[u8], shadow: &[u8]) -> Result<Vec<(u64, u64)>> {
    let mut changed_ranges = Vec::new();
    let chunk_size = 64;
    
    // Process 64-byte chunks
    for (i, chunks) in current.chunks(chunk_size).enumerate() {
        let offset = (i * chunk_size) as u64;
        
        // Safe bounds
        if offset + chunk_size as u64 > current.len() as u64 {
            break;
        }
        
        unsafe {
            use std::arch::x86_64::*;
            
            // Load 64 bytes into each register
            let current_vec = _mm512_loadu_si512(chunks.as_ptr() as *const __m512i);
            let shadow_vec = _mm512_loadu_si512(
                shadow[offset as usize..]
                    .as_ptr() as *const __m512i
            );
            
            // XOR to find differences
            let diff = _mm512_xor_epi64(current_vec, shadow_vec);
            
            // Check if any bytes differ
            let cmp_result = _mm512_test_epi64_mask(diff, diff);
            
            if cmp_result != 0 {
                // This chunk contains changes
                changed_ranges.push((offset, offset + chunk_size as u64));
            }
        }
    }
    
    Ok(changed_ranges)
}
```

### 3. AVX2 Implementation

```rust
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
pub fn compare_avx2(current: &[u8], shadow: &[u8]) -> Result<Vec<(u64, u64)>> {
    let mut changed_ranges = Vec::new();
    let chunk_size = 32;
    
    for (i, chunks) in current.chunks(chunk_size).enumerate() {
        let offset = (i * chunk_size) as u64;
        
        if offset + chunk_size as u64 > current.len() as u64 {
            break;
        }
        
        unsafe {
            use std::arch::x86_64::*;
            
            let current_vec = _mm256_loadu_si256(chunks.as_ptr() as *const __m256i);
            let shadow_vec = _mm256_loadu_si256(
                shadow[offset as usize..]
                    .as_ptr() as *const __m256i
            );
            
            let diff = _mm256_xor_si256(current_vec, shadow_vec);
            
            // AVX2 doesn't have test_epi64_mask, so check manually
            let is_zero = _mm256_testz_si256(diff, diff);
            
            if is_zero == 0 {  // Not zero = differences found
                changed_ranges.push((offset, offset + chunk_size as u64));
            }
        }
    }
    
    Ok(changed_ranges)
}
```

### 4. u64 Fallback

```rust
pub fn compare_u64(current: &[u8], shadow: &[u8]) -> Result<Vec<(u64, u64)>> {
    let mut changed_ranges = Vec::new();
    let chunk_size = 8;
    
    for (i, chunk) in current.chunks(chunk_size).enumerate() {
        let offset = (i * chunk_size) as u64;
        
        if offset + chunk_size as u64 > current.len() as u64 {
            break;
        }
        
        // Read as u64
        let current_u64 = if chunk.len() == 8 {
            u64::from_ne_bytes([
                chunk[0], chunk[1], chunk[2], chunk[3],
                chunk[4], chunk[5], chunk[6], chunk[7],
            ])
        } else {
            // Handle partial chunk at end
            let mut buf = [0u8; 8];
            buf[..chunk.len()].copy_from_slice(chunk);
            u64::from_ne_bytes(buf)
        };
        
        let shadow_start = offset as usize;
        let shadow_u64 = if shadow_start + chunk_size <= shadow.len() {
            u64::from_ne_bytes([
                shadow[shadow_start],
                shadow[shadow_start + 1],
                shadow[shadow_start + 2],
                shadow[shadow_start + 3],
                shadow[shadow_start + 4],
                shadow[shadow_start + 5],
                shadow[shadow_start + 6],
                shadow[shadow_start + 7],
            ])
        } else {
            0
        };
        
        if current_u64 != shadow_u64 {
            changed_ranges.push((offset, offset + chunk_size as u64));
        }
    }
    
    Ok(changed_ranges)
}
```

## Finding Changed Variables

Once we have byte ranges, we need to map them to variables:

```rust
pub async fn find_changed_variables_from_diff(
    &self,
    byte_ranges: &[(u64, u64)],
) -> Result<Vec<String>> {
    let mut changed_vars = HashSet::new();
    
    // Get all registered variables
    let vars = self.variables.read().await;
    
    // For each changed byte range
    for (range_start, range_end) in byte_ranges {
        // Check which variables overlap this range
        for (var_name, metadata) in vars.iter() {
            let var_start = metadata.offset;
            let var_end = metadata.offset + metadata.size;
            
            // Check for overlap
            if var_start < *range_end && var_end > *range_start {
                changed_vars.insert(var_name.clone());
            }
        }
    }
    
    Ok(changed_vars.into_iter().collect())
}
```

## Performance Optimization

### 1. Early Exit on Identical Files

```rust
pub async fn compare_ranges(
    current: &[u8],
    shadow: &[u8],
) -> Result<Vec<(u64, u64)>> {
    // Quick check: are they the same length?
    if current.len() != shadow.len() {
        return Ok(vec![(0, current.len() as u64)]);
    }
    
    // Quick check: are they identical?
    if current == shadow {
        return Ok(vec![]);  // No changes
    }
    
    // If still potentially identical, do detailed SIMD check
    let mut changed = false;
    for chunk in current.chunks(64) {
        // ... SIMD comparison
    }
    
    // If no changes found, return empty
    if !changed {
        return Ok(vec![]);
    }
    
    // ... otherwise return detailed ranges
}
```

### 2. Lazy Byte Range Expansion

Instead of immediately expanding chunk-level changes to byte-level, defer fine-grained detection:

```rust
// Quick pass: Find changed chunks (64-byte level)
let changed_chunks = find_changed_chunks(current, shadow).await?;

// Only if needed for variable mapping:
let changed_bytes = expand_to_bytes(current, shadow, &changed_chunks).await?;
```

### 3. Caching Strategy

```rust
struct VirtualVariableFile {
    // ... existing fields ...
    last_comparison_result: Arc<RwLock<Option<Vec<(u64, u64)>>>>,
    last_comparison_time: Arc<RwLock<Instant>>,
}

pub async fn compare_ranges_cached(
    &self,
    current: &[u8],
    shadow: &[u8],
) -> Result<Vec<(u64, u64)>> {
    // Check if cache is fresh (< 100ms old)
    let cache = self.last_comparison_result.read().await;
    let cache_time = self.last_comparison_time.read().await;
    
    if let Some(cached) = cache.as_ref() {
        if cache_time.elapsed() < Duration::from_millis(100) {
            return Ok(cached.clone());
        }
    }
    drop(cache);
    drop(cache_time);
    
    // Recompute and cache
    let result = self.compare_ranges_uncached(current, shadow).await?;
    
    let mut cache = self.last_comparison_result.write().await;
    *cache = Some(result.clone());
    
    let mut cache_time = self.last_comparison_time.write().await;
    *cache_time = Instant::now();
    
    Ok(result)
}
```

## Benchmarking

### Benchmark Setup

```rust
#[cfg(test)]
mod benches {
    use criterion::{black_box, criterion_group, criterion_main, Criterion};
    
    fn benchmark_simd_comparison(c: &mut Criterion) {
        let current = black_box(vec![0u8; 65536]);
        let mut shadow = black_box(vec![0u8; 65536]);
        shadow[32768] = 99;  // One byte different
        
        c.bench_function("compare_simd_64kb", |b| {
            b.iter(|| {
                VirtualVariableFile::compare_ranges(
                    &current,
                    &shadow,
                )
            });
        });
    }
    
    criterion_group!(benches, benchmark_simd_comparison);
    criterion_main!(benches);
}
```

### Expected Performance

```
| Input Size | AVX-512 | AVX2    | u64    |
| ---------- | ------- | ------- | ------ |
| 1 KB       | 0.01 μs | 0.02 μs | 0.1 μs |
| 10 KB      | 0.1 μs  | 0.2 μs  | 1 μs   |
| 100 KB     | 1 μs    | 2 μs    | 10 μs  |
| 1 MB       | 10 μs   | 20 μs   | 100 μs |
```

## CPU Detection

### Feature Detection Macros

```rust
// In Cargo.toml
[package]
name = "commy_client"

[dependencies]
...

# At runtime, detect using:
#[cfg(target_arch = "x86_64")]
pub fn has_avx512() -> bool {
    is_x86_feature_detected!("avx512f")
}

#[cfg(target_arch = "x86_64")]
pub fn has_avx2() -> bool {
    is_x86_feature_detected!("avx2")
}
```

### Runtime Detection Example

```rust
pub struct SimdCapabilities {
    pub avx512: bool,
    pub avx2: bool,
    pub sse2: bool,
}

pub fn detect_simd_capabilities() -> SimdCapabilities {
    #[cfg(target_arch = "x86_64")]
    {
        SimdCapabilities {
            avx512: is_x86_feature_detected!("avx512f"),
            avx2: is_x86_feature_detected!("avx2"),
            sse2: is_x86_feature_detected!("sse2"),
        }
    }
    
    #[cfg(not(target_arch = "x86_64"))]
    {
        SimdCapabilities {
            avx512: false,
            avx2: false,
            sse2: false,
        }
    }
}
```

## Troubleshooting

### Issue: "Illegal instruction" crash

**Cause:** Code compiled for AVX-512 but CPU doesn't support it

**Solution:**
```rust
// Don't force feature flags at compile time
// Let runtime detection choose

// NOT this:
// cargo build --target-cpu=skylake-avx512

// Instead use:
// cargo build  (runtime detection)
```

### Issue: Incorrect change detection

**Cause:** Misaligned memory access or buffer overflow

**Solution:**
```rust
// Always check bounds
let safe_chunk_size = std::cmp::min(
    64,
    current.len().saturating_sub(offset)
);

// Use safe memory copy
let mut aligned_buffer = [0u8; 64];
aligned_buffer[..safe_chunk_size]
    .copy_from_slice(&current[offset..offset+safe_chunk_size]);
```

### Issue: Performance not improved

**Cause:** Not using SIMD version or small files

**Solution:**
```rust
// Verify SIMD is being used
println!("SIMD Strategy: {:?}", select_simd_strategy());

// Only worth SIMD for files > 1KB
// For smaller files, byte-by-byte is faster due to setup overhead

if current.len() > 1024 {
    use_simd_comparison()
} else {
    use_simple_comparison()
}
```

## Future Optimizations

### 1. NEON (ARM Support)

```rust
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
pub fn compare_neon(current: &[u8], shadow: &[u8]) -> Vec<(u64, u64)> {
    // Similar to AVX-512 but with NEON intrinsics
}
```

### 2. Accelerated Memory Compare

```rust
// Instead of reimplementing SIMD, use standard library
// (if performance is comparable)
pub fn compare_optimized(current: &[u8], shadow: &[u8]) -> Vec<(u64, u64)> {
    // Leverage libc memcmp for comparison
    unsafe {
        if libc::memcmp(
            current.as_ptr() as *const _,
            shadow.as_ptr() as *const _,
            current.len(),
        ) == 0 {
            return vec![];
        }
    }
    
    // Then do SIMD for detail
    compare_ranges(current, shadow)
}
```

### 3. Parallel Comparison

```rust
pub async fn compare_ranges_parallel(
    current: &[u8],
    shadow: &[u8],
) -> Result<Vec<(u64, u64)>> {
    // Split into chunks, compare in parallel
    let num_threads = num_cpus::get();
    let chunk_per_thread = current.len() / num_threads;
    
    let handles: Vec<_> = (0..num_threads)
        .map(|i| {
            let start = i * chunk_per_thread;
            let end = if i == num_threads - 1 {
                current.len()
            } else {
                (i + 1) * chunk_per_thread
            };
            
            let current = current[start..end].to_vec();
            let shadow = shadow[start..end].to_vec();
            
            tokio::spawn(async move {
                compare_ranges(&current, &shadow)
                    .await
                    .map(|ranges| {
                        ranges.into_iter()
                            .map(|(a, b)| (a + start as u64, b + start as u64))
                            .collect::<Vec<_>>()
                    })
            })
        })
        .collect();
    
    let mut all_ranges = Vec::new();
    for handle in handles {
        if let Ok(Ok(ranges)) = handle.await {
            all_ranges.extend(ranges);
        }
    }
    
    Ok(all_ranges)
}
```

## References

- [Intel SIMD Intrinsics Guide](https://www.intel.com/content/www/us/en/docs/intrinsics-guide/index.html)
- [AVX-512 Specification](https://software.intel.com/sites/default/files/managed/9f/3c/325462-sdm-vol-2a-2b.pdf)
- [Rust portable-SIMD RFC](https://github.com/rust-lang/rfcs/pull/2948)
- [x86-64 ABI](https://gitlab.com/x86-psABIs/x86-64-ABI)
