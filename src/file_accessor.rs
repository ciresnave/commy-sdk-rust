// SPDX-License-Identifier: MIT
//! File accessor abstraction for local and remote variable files

use crate::error::Result;
use memmap2::Mmap;
use std::path::PathBuf;

/// Trait for accessing variable file data (local or remote)
#[async_trait::async_trait]
pub trait FileAccessor: Send + Sync {
    /// Read bytes from the file
    async fn read_bytes(&self, offset: u64, size: u64) -> Result<Vec<u8>>;

    /// Write bytes to the file
    async fn write_bytes(&self, offset: u64, data: &[u8]) -> Result<()>;

    /// Get total file size
    async fn file_size(&self) -> Result<u64>;

    /// Check if this is a local file accessor
    fn is_local(&self) -> bool;

    /// Resize file to new size
    async fn resize(&self, new_size: u64) -> Result<()>;
}

/// Local file accessor using memory mapping
pub struct LocalFileAccessor {
    file_path: PathBuf,
    mmap: Mmap,
}

impl LocalFileAccessor {
    /// Create a new local file accessor
    pub async fn new(file_path: PathBuf) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&file_path)?;

        let mmap = unsafe { memmap2::MmapMut::map_mut(&file)? };
        let mmap = mmap.make_read_only()?;
        // Note: mmap keeps the file open via its file descriptor

        Ok(Self { file_path, mmap })
    }

    /// Get file path
    pub fn path(&self) -> &PathBuf {
        &self.file_path
    }
}

#[async_trait::async_trait]
impl FileAccessor for LocalFileAccessor {
    async fn read_bytes(&self, offset: u64, size: u64) -> Result<Vec<u8>> {
        let start = offset as usize;
        let end = (offset + size) as usize;

        if end > self.mmap.len() {
            return Err(crate::error::CommyError::InvalidOffset(
                "Read extends beyond file bounds".to_string(),
            ));
        }

        Ok(self.mmap[start..end].to_vec())
    }

    async fn write_bytes(&self, _offset: u64, _data: &[u8]) -> Result<()> {
        // Local files are read-only after mapping - writes go through the watcher
        Err(crate::error::CommyError::InvalidState(
            "Cannot write directly to local accessor; use file watcher".to_string(),
        ))
    }

    async fn file_size(&self) -> Result<u64> {
        Ok(self.mmap.len() as u64)
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn resize(&self, _new_size: u64) -> Result<()> {
        Err(crate::error::CommyError::InvalidState(
            "Cannot resize local mapped file".to_string(),
        ))
    }
}

/// Remote file accessor for WSS-synced data
pub struct RemoteFileAccessor {
    /// In-memory buffer containing the file data
    buffer: std::sync::Arc<tokio::sync::RwLock<Vec<u8>>>,
}

impl Default for RemoteFileAccessor {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoteFileAccessor {
    /// Create a new remote file accessor
    pub fn new() -> Self {
        Self {
            buffer: std::sync::Arc::new(tokio::sync::RwLock::new(Vec::new())),
        }
    }
}

#[async_trait::async_trait]
impl FileAccessor for RemoteFileAccessor {
    async fn read_bytes(&self, offset: u64, size: u64) -> Result<Vec<u8>> {
        let buf = self.buffer.read().await;
        let start = offset as usize;
        let end = (offset + size) as usize;

        if end > buf.len() {
            return Err(crate::error::CommyError::InvalidOffset(
                "Read extends beyond buffer bounds".to_string(),
            ));
        }

        Ok(buf[start..end].to_vec())
    }

    async fn write_bytes(&self, offset: u64, data: &[u8]) -> Result<()> {
        let mut buf = self.buffer.write().await;
        let start = offset as usize;
        let end = start + data.len();

        if end > buf.len() {
            buf.resize(end, 0);
        }

        buf[start..end].copy_from_slice(data);
        Ok(())
    }

    async fn file_size(&self) -> Result<u64> {
        Ok(self.buffer.read().await.len() as u64)
    }

    fn is_local(&self) -> bool {
        false
    }

    async fn resize(&self, new_size: u64) -> Result<()> {
        let mut buf = self.buffer.write().await;
        buf.resize(new_size as usize, 0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_remote_file_accessor_new_is_empty() {
        let accessor = RemoteFileAccessor::new();
        let size = accessor.file_size().await.unwrap();
        assert_eq!(size, 0);
    }

    #[tokio::test]
    async fn test_remote_resize() {
        let accessor = RemoteFileAccessor::new();
        accessor.resize(64).await.unwrap();
        let size = accessor.file_size().await.unwrap();
        assert_eq!(size, 64);
    }

    #[tokio::test]
    async fn test_remote_is_local_false() {
        let accessor = RemoteFileAccessor::new();
        assert!(!accessor.is_local());
    }

    #[tokio::test]
    async fn test_remote_write_then_read_round_trips() {
        let accessor = RemoteFileAccessor::new();
        accessor.write_bytes(0, &[1, 2, 3, 4]).await.unwrap();
        assert_eq!(accessor.read_bytes(1, 2).await.unwrap(), vec![2, 3]);
        assert_eq!(accessor.file_size().await.unwrap(), 4);
    }

    #[tokio::test]
    async fn test_remote_write_beyond_end_zero_fills_the_gap() {
        let accessor = RemoteFileAccessor::new();
        accessor.write_bytes(4, &[9]).await.unwrap();
        assert_eq!(
            accessor.read_bytes(0, 5).await.unwrap(),
            vec![0, 0, 0, 0, 9]
        );
    }

    #[tokio::test]
    async fn test_remote_read_out_of_bounds_returns_error() {
        let accessor = RemoteFileAccessor::new();
        accessor.write_bytes(0, &[1, 2]).await.unwrap();
        assert!(accessor.read_bytes(1, 5).await.is_err());
    }

    // ─────────────────────────────────────────────────────────────
    // LocalFileAccessor tests
    // ─────────────────────────────────────────────────────────────

    fn make_tmp_file(size: usize) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.mem");
        std::fs::write(&path, vec![0u8; size]).unwrap();
        (dir, path)
    }

    #[tokio::test]
    async fn test_local_accessor_new_and_is_local() {
        let (_tmp, path) = make_tmp_file(64);
        let accessor = LocalFileAccessor::new(path).await.unwrap();
        assert!(accessor.is_local());
    }

    #[tokio::test]
    async fn test_local_accessor_file_size() {
        let (_tmp, path) = make_tmp_file(128);
        let accessor = LocalFileAccessor::new(path).await.unwrap();
        let size = accessor.file_size().await.unwrap();
        assert_eq!(size, 128);
    }

    #[tokio::test]
    async fn test_local_accessor_read_bytes_happy() {
        let content = vec![10u8, 20, 30, 40, 50, 60, 70, 80];
        let (_tmp, path) = make_tmp_file(8);
        std::fs::write(&path, &content).unwrap();
        let accessor = LocalFileAccessor::new(path).await.unwrap();
        let bytes = accessor.read_bytes(2, 4).await.unwrap();
        assert_eq!(bytes, vec![30, 40, 50, 60]);
    }

    #[tokio::test]
    async fn test_local_accessor_read_bytes_out_of_bounds_returns_error() {
        let (_tmp, path) = make_tmp_file(8);
        let accessor = LocalFileAccessor::new(path).await.unwrap();
        let result = accessor.read_bytes(0, 100).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_local_accessor_write_bytes_returns_invalid_state() {
        let (_tmp, path) = make_tmp_file(16);
        let accessor = LocalFileAccessor::new(path).await.unwrap();
        let result = accessor.write_bytes(0, &[1, 2, 3]).await;
        assert!(
            result.is_err(),
            "write_bytes must fail on local read-only accessor"
        );
        // Should be an InvalidState error
        match result.unwrap_err() {
            crate::error::CommyError::InvalidState(_) => {}
            e => panic!("Expected InvalidState, got {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_local_accessor_resize_returns_invalid_state() {
        let (_tmp, path) = make_tmp_file(16);
        let accessor = LocalFileAccessor::new(path).await.unwrap();
        let result = accessor.resize(256).await;
        assert!(
            result.is_err(),
            "resize must fail on local memory-mapped accessor"
        );
        match result.unwrap_err() {
            crate::error::CommyError::InvalidState(_) => {}
            e => panic!("Expected InvalidState, got {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_local_accessor_path() {
        let (_tmp, path) = make_tmp_file(32);
        let accessor = LocalFileAccessor::new(path.clone()).await.unwrap();
        assert_eq!(accessor.path(), &std::path::PathBuf::from(&path));
    }

    #[tokio::test]
    async fn test_local_accessor_read_bytes_exact_boundary_succeeds() {
        // An 8-byte file: reading offset=4, size=4 ends exactly at the last byte \u2014 must succeed
        let content = vec![10u8, 20, 30, 40, 50, 60, 70, 80];
        let (_tmp, path) = make_tmp_file(8);
        std::fs::write(&path, &content).unwrap();
        let accessor = LocalFileAccessor::new(path).await.unwrap();
        let bytes = accessor.read_bytes(4, 4).await.unwrap();
        assert_eq!(
            bytes,
            vec![50, 60, 70, 80],
            "read at exact boundary must return last 4 bytes"
        );
    }
}
