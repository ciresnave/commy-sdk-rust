//! Virtual variable file abstraction
//!
//! Provides a unified interface for accessing variables from either:
//! - Direct memory-mapped files (local clients)
//! - In-memory buffers synchronized via WSS (remote clients)

use crate::error::{CommyError, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Metadata about a variable in the file
#[derive(Debug, Clone)]
pub struct VariableMetadata {
    /// Variable name
    pub name: String,

    /// Byte offset in the file
    pub offset: u64,

    /// Variable size in bytes
    pub size: u64,

    /// Type ID
    pub type_id: u32,

    /// Whether this variable persists across disconnections
    pub persistent: bool,
}

impl VariableMetadata {
    /// Create new variable metadata
    pub fn new(name: String, offset: u64, size: u64, type_id: u32) -> Self {
        Self {
            name,
            offset,
            size,
            type_id,
            persistent: false,
        }
    }

    /// Set persistence flag
    pub fn with_persistent(mut self, persistent: bool) -> Self {
        self.persistent = persistent;
        self
    }
}

/// Virtual representation of a service's variable file
///
/// This abstraction allows both local memory-mapped files and remote WSS-synced files
/// to be accessed through a unified interface. The file maintains:
/// - Current variable data in memory
/// - Metadata about variable locations and sizes
/// - A shadow copy for change detection
/// - Per-variable change tracking
#[derive(Debug)]
pub struct VirtualVariableFile {
    /// Service ID
    service_id: String,

    /// Service name
    service_name: String,

    /// Tenant ID
    tenant_id: String,

    /// Variable metadata by name
    variables: Arc<RwLock<HashMap<String, VariableMetadata>>>,

    /// Current file bytes
    current_bytes: Arc<RwLock<Vec<u8>>>,

    /// Shadow copy (last known state)
    shadow_bytes: Arc<RwLock<Vec<u8>>>,

    /// Track which variables have changed
    changed_variables: Arc<RwLock<Vec<String>>>,
}

impl VirtualVariableFile {
    /// Create a new virtual variable file
    pub fn new(service_id: String, service_name: String, tenant_id: String) -> Self {
        Self {
            service_id,
            service_name,
            tenant_id,
            variables: Arc::new(RwLock::new(HashMap::new())),
            current_bytes: Arc::new(RwLock::new(Vec::new())),
            shadow_bytes: Arc::new(RwLock::new(Vec::new())),
            changed_variables: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Get service ID
    pub fn service_id(&self) -> &str {
        &self.service_id
    }

    /// Get service name
    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    /// Get tenant ID
    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    /// Register a variable
    pub async fn register_variable(&self, metadata: VariableMetadata) -> Result<()> {
        let mut vars = self.variables.write().await;

        // Ensure the byte buffer is large enough
        let end = metadata.offset as usize + metadata.size as usize;
        let mut current = self.current_bytes.write().await;
        if current.len() < end {
            current.resize(end, 0);
        }

        // Also resize shadow
        let mut shadow = self.shadow_bytes.write().await;
        if shadow.len() < end {
            shadow.resize(end, 0);
        }

        vars.insert(metadata.name.clone(), metadata);
        Ok(())
    }

    /// Get variable metadata by name
    pub async fn get_variable_metadata(&self, name: &str) -> Result<VariableMetadata> {
        let vars = self.variables.read().await;
        vars.get(name)
            .cloned()
            .ok_or_else(|| CommyError::VariableNotFound(name.to_string()))
    }

    /// List all variables
    pub async fn list_variables(&self) -> Result<Vec<VariableMetadata>> {
        let vars = self.variables.read().await;
        Ok(vars.values().cloned().collect())
    }

    /// Read a variable as zero-copy slice
    pub async fn read_variable_slice(&self, name: &str) -> Result<Vec<u8>> {
        let metadata = self.get_variable_metadata(name).await?;
        let current = self.current_bytes.read().await;

        let start = metadata.offset as usize;
        let end = start + metadata.size as usize;

        if end > current.len() {
            return Err(CommyError::InvalidOffset(format!(
                "Variable {} extends beyond file bounds",
                name
            )));
        }

        Ok(current[start..end].to_vec())
    }

    /// Write a variable
    pub async fn write_variable(&self, name: &str, data: &[u8]) -> Result<()> {
        let metadata = self.get_variable_metadata(name).await?;

        if data.len() as u64 != metadata.size {
            return Err(CommyError::InvalidMessage(format!(
                "Data size {} does not match variable size {}",
                data.len(),
                metadata.size
            )));
        }

        let mut current = self.current_bytes.write().await;
        let start = metadata.offset as usize;
        let end = start + data.len();

        if end > current.len() {
            return Err(CommyError::InvalidOffset(format!(
                "Variable {} offset out of bounds",
                name
            )));
        }

        current[start..end].copy_from_slice(data);

        // Mark as changed
        let mut changed = self.changed_variables.write().await;
        if !changed.contains(&name.to_string()) {
            changed.push(name.to_string());
        }

        Ok(())
    }

    /// Get raw bytes (zero-copy reference to internal buffer)
    pub async fn bytes(&self) -> Vec<u8> {
        self.current_bytes.read().await.clone()
    }

    /// Update entire file content
    pub async fn update_bytes(&self, data: Vec<u8>) -> Result<()> {
        let mut current = self.current_bytes.write().await;
        *current = data;
        Ok(())
    }

    /// Get shadow copy
    pub async fn shadow_bytes(&self) -> Vec<u8> {
        self.shadow_bytes.read().await.clone()
    }

    /// Update shadow copy
    pub async fn update_shadow_bytes(&self, data: Vec<u8>) -> Result<()> {
        let mut shadow = self.shadow_bytes.write().await;
        *shadow = data;
        Ok(())
    }

    /// Get list of changed variables since last sync
    pub async fn get_changed_variables(&self) -> Vec<String> {
        self.changed_variables.read().await.clone()
    }

    /// Clear change tracking
    pub async fn clear_changes(&self) {
        self.changed_variables.write().await.clear();
    }

    /// Mark specific variables as changed
    pub async fn mark_variables_changed(&self, names: Vec<String>) {
        let mut changed = self.changed_variables.write().await;
        for name in names {
            if !changed.contains(&name) {
                changed.push(name);
            }
        }
    }

    /// Compare two byte ranges using wide SIMD operations
    ///
    /// Returns byte offsets where differences were found
    pub async fn compare_ranges(current: &[u8], shadow: &[u8]) -> Result<Vec<(u64, u64)>> {
        if current.len() != shadow.len() {
            return Err(CommyError::SimdError(
                "Cannot compare buffers of different sizes".to_string(),
            ));
        }

        let mut differences = Vec::new();
        let mut i = 0;

        // Try to use AVX-512 if available (64-byte chunks)
        #[cfg(target_arch = "x86_64")]
        {
            use std::arch::x86_64::*;

            if is_x86_feature_detected!("avx512f") {
                while i + 64 <= current.len() {
                    unsafe {
                        let a = _mm512_loadu_si512(current[i..].as_ptr() as *const _);
                        let b = _mm512_loadu_si512(shadow[i..].as_ptr() as *const _);
                        let cmp = _mm512_cmpeq_epi8_mask(a, b);

                        // If not all equal (mask != 0xFFFFFFFFFFFFFFFF)
                        if cmp != 0xFFFFFFFFFFFFFFFF {
                            differences.push((i as u64, (i + 64) as u64));
                        }
                    }
                    i += 64;
                }
            }
        }

        // Fall back to AVX2 if available (32-byte chunks)
        #[cfg(target_arch = "x86_64")]
        {
            use std::arch::x86_64::*;

            if i == 0 && is_x86_feature_detected!("avx2") {
                while i + 32 <= current.len() {
                    unsafe {
                        let a = _mm256_loadu_si256(current[i..].as_ptr() as *const _);
                        let b = _mm256_loadu_si256(shadow[i..].as_ptr() as *const _);
                        let cmp = _mm256_cmpeq_epi8(a, b);

                        // Check if any byte differs
                        if _mm256_movemask_epi8(cmp) != -1 {
                            differences.push((i as u64, (i + 32) as u64));
                        }
                    }
                    i += 32;
                }
            }
        }

        // Fall back to 8-byte (u64) comparisons
        while i + 8 <= current.len() {
            let current_u64 = u64::from_ne_bytes([
                current[i],
                current[i + 1],
                current[i + 2],
                current[i + 3],
                current[i + 4],
                current[i + 5],
                current[i + 6],
                current[i + 7],
            ]);
            let shadow_u64 = u64::from_ne_bytes([
                shadow[i],
                shadow[i + 1],
                shadow[i + 2],
                shadow[i + 3],
                shadow[i + 4],
                shadow[i + 5],
                shadow[i + 6],
                shadow[i + 7],
            ]);

            if current_u64 != shadow_u64 {
                differences.push((i as u64, (i + 8) as u64));
            }
            i += 8;
        }

        // Handle remaining bytes
        while i < current.len() {
            if current[i] != shadow[i] {
                differences.push((i as u64, (i + 1) as u64));
            }
            i += 1;
        }

        Ok(differences)
    }

    /// Find which variables changed based on byte differences
    pub async fn find_changed_variables_from_diff(
        &self,
        diff_ranges: &[(u64, u64)],
    ) -> Result<Vec<String>> {
        let vars = self.variables.read().await;
        let mut changed = Vec::new();

        for (diff_start, diff_end) in diff_ranges {
            for (name, metadata) in vars.iter() {
                let var_start = metadata.offset;
                let var_end = metadata.offset + metadata.size;

                // Check if this variable overlaps with the difference
                if *diff_start < var_end && *diff_end > var_start {
                    if !changed.contains(&name.clone()) {
                        changed.push(name.clone());
                    }
                }
            }
        }

        Ok(changed)
    }

    /// Sync shadow with current (after sending updates to server)
    pub async fn sync_shadow(&self) -> Result<()> {
        let current = self.current_bytes.read().await;
        let mut shadow = self.shadow_bytes.write().await;
        *shadow = current.clone();
        self.changed_variables.write().await.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_and_read_variable() {
        let vf = VirtualVariableFile::new(
            "svc_1".to_string(),
            "config".to_string(),
            "tenant_1".to_string(),
        );

        let metadata = VariableMetadata::new("my_var".to_string(), 0, 8, 1);
        vf.register_variable(metadata).await.unwrap();

        // Write data
        vf.write_variable("my_var", &[1, 2, 3, 4, 5, 6, 7, 8])
            .await
            .unwrap();

        // Read it back
        let data = vf.read_variable_slice("my_var").await.unwrap();
        assert_eq!(data, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[tokio::test]
    async fn test_change_tracking() {
        let vf = VirtualVariableFile::new(
            "svc_1".to_string(),
            "config".to_string(),
            "tenant_1".to_string(),
        );

        let metadata = VariableMetadata::new("var1".to_string(), 0, 4, 1);
        vf.register_variable(metadata).await.unwrap();

        vf.write_variable("var1", &[1, 2, 3, 4]).await.unwrap();

        let changed = vf.get_changed_variables().await;
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0], "var1");
    }

    #[tokio::test]
    async fn test_simd_compare() {
        let current = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let mut shadow = current.clone();
        shadow[3] = 99; // Change one byte

        let diffs = VirtualVariableFile::compare_ranges(&current, &shadow)
            .await
            .unwrap();

        assert!(!diffs.is_empty());
    }
}
