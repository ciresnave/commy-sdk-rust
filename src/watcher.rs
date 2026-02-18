//! File watcher and change detection engine
//!
//! Monitors temporary variable files for changes and uses SIMD
//! operations to efficiently identify which variables have changed.

use crate::error::{CommyError, Result};
use crate::virtual_file::VirtualVariableFile;
use futures::FutureExt;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::sync::mpsc;

/// Change event for a variable file
#[derive(Debug, Clone)]
pub struct FileChangeEvent {
    /// Path to the changed file
    pub file_path: PathBuf,

    /// Service ID
    pub service_id: String,

    /// Variables that changed
    pub changed_variables: Vec<String>,

    /// Byte ranges that changed
    pub byte_ranges: Vec<(u64, u64)>,
}

/// File watcher for variable file changes
pub struct VariableFileWatcher {
    /// Watch directory path
    watch_dir: PathBuf,

    /// Sender for change events
    tx: mpsc::UnboundedSender<FileChangeEvent>,

    /// Receiver for change events
    rx: Arc<RwLock<mpsc::UnboundedReceiver<FileChangeEvent>>>,

    /// Virtual files being watched (by service ID)
    virtual_files: Arc<RwLock<std::collections::HashMap<String, Arc<VirtualVariableFile>>>>,

    /// Stop signal
    stop_tx: Arc<RwLock<Option<tokio::sync::oneshot::Sender<()>>>>,
}

impl VariableFileWatcher {
    /// Create a new variable file watcher
    pub async fn new(watch_dir: Option<PathBuf>) -> Result<Self> {
        let watch_dir = match watch_dir {
            Some(d) => d,
            None => {
                // Use system temp directory, create commy subdirectory
                let temp = dirs::cache_dir().ok_or_else(|| {
                    CommyError::FileError(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "No cache directory available",
                    ))
                })?;

                let commy_dir = temp.join("commy_virtual_files");
                fs::create_dir_all(&commy_dir)?;
                commy_dir
            }
        };

        let (tx, rx) = mpsc::unbounded_channel();

        Ok(Self {
            watch_dir,
            tx,
            rx: Arc::new(RwLock::new(rx)),
            virtual_files: Arc::new(RwLock::new(std::collections::HashMap::new())),
            stop_tx: Arc::new(RwLock::new(None)),
        })
    }

    /// Get watch directory
    pub fn watch_dir(&self) -> &Path {
        &self.watch_dir
    }

    /// Register a virtual file for watching
    pub async fn register_virtual_file(
        &self,
        service_id: String,
        vf: Arc<VirtualVariableFile>,
    ) -> Result<()> {
        let mut files = self.virtual_files.write().await;
        files.insert(service_id, vf);
        Ok(())
    }

    /// Start watching for changes (spawns background task)
    pub async fn start_watching(&self) -> Result<()> {
        let watch_dir = self.watch_dir.clone();
        let tx = self.tx.clone();
        let virtual_files = Arc::clone(&self.virtual_files);

        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel();
        *self.stop_tx.write().await = Some(stop_tx);

        tokio::spawn(async move {
            if let Err(e) = Self::watch_loop(watch_dir, tx, virtual_files, &mut stop_rx).await {
                eprintln!("Watch loop error: {}", e);
            }
        });

        Ok(())
    }

    /// Background watch loop
    async fn watch_loop(
        watch_dir: PathBuf,
        tx: mpsc::UnboundedSender<FileChangeEvent>,
        virtual_files: Arc<RwLock<std::collections::HashMap<String, Arc<VirtualVariableFile>>>>,
        stop_rx: &mut tokio::sync::oneshot::Receiver<()>,
    ) -> Result<()> {
        let (file_tx, mut file_rx) = mpsc::unbounded_channel();

        let mut watcher = RecommendedWatcher::new(
            move |event: std::result::Result<Event, notify::Error>| {
                if let Ok(evt) = event {
                    let _ = file_tx.send(evt);
                }
            },
            Config::default().with_poll_interval(Duration::from_millis(100)),
        )
        .map_err(|e: notify::Error| CommyError::WatcherError(e.to_string()))?;

        watcher
            .watch(&watch_dir, RecursiveMode::NonRecursive)
            .map_err(|e: notify::Error| CommyError::WatcherError(e.to_string()))?;

        loop {
            tokio::select! {
                Some(event) = file_rx.recv() => {
                    match event.kind {
                        EventKind::Modify(_) => {
                            for path in event.paths {
                                if let Err(e) = Self::handle_file_change(
                                    &path,
                                    &tx,
                                    &virtual_files,
                                ).await {
                                    eprintln!("Error handling file change: {}", e);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ = &mut *stop_rx => {
                    break;
                }
            }
        }

        Ok(())
    }

    /// Handle a file change event
    async fn handle_file_change(
        file_path: &Path,
        tx: &mpsc::UnboundedSender<FileChangeEvent>,
        virtual_files: &Arc<RwLock<std::collections::HashMap<String, Arc<VirtualVariableFile>>>>,
    ) -> Result<()> {
        // Extract service ID from filename (format: service_<id>.mem)
        let filename = file_path
            .file_name()
            .ok_or_else(|| {
                CommyError::FileError(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Invalid filename",
                ))
            })?
            .to_string_lossy();

        if !filename.ends_with(".mem") {
            return Ok(());
        }

        let service_id = filename
            .strip_prefix("service_")
            .and_then(|s| s.strip_suffix(".mem"))
            .ok_or_else(|| {
                CommyError::FileError(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Invalid service filename",
                ))
            })?;

        // Read the file
        let new_bytes = tokio::fs::read(file_path).await?;

        // Find the virtual file
        let vfiles = virtual_files.read().await;
        if let Some(vf) = vfiles.get(service_id) {
            // Compare using SIMD
            let _current = vf.bytes().await;
            let shadow = vf.shadow_bytes().await;

            if new_bytes == shadow {
                // No changes detected
                return Ok(());
            }

            // Use SIMD diff detection
            let byte_ranges = VirtualVariableFile::compare_ranges(&new_bytes, &shadow).await?;

            // Identify which variables changed
            let changed_vars = vf.find_changed_variables_from_diff(&byte_ranges).await?;

            // Update virtual file
            vf.update_bytes(new_bytes.clone()).await?;
            vf.mark_variables_changed(changed_vars.clone()).await;

            // Send change event
            let event = FileChangeEvent {
                file_path: file_path.to_path_buf(),
                service_id: service_id.to_string(),
                changed_variables: changed_vars,
                byte_ranges,
            };

            let _ = tx.send(event);
        }

        Ok(())
    }

    /// Stop watching
    pub async fn stop_watching(&self) -> Result<()> {
        if let Some(stop_tx) = self.stop_tx.write().await.take() {
            let _ = stop_tx.send(());
        }
        Ok(())
    }

    /// Receive next change event (blocking)
    pub async fn next_change(&self) -> Option<FileChangeEvent> {
        let mut rx = self.rx.write().await;
        rx.recv().await
    }

    /// Try to receive next change event (non-blocking)
    pub async fn try_next_change(&self) -> Option<FileChangeEvent> {
        let mut rx = self.rx.write().await;
        rx.recv().now_or_never().flatten()
    }
}

/// Create temporary file for a service
pub async fn create_temp_service_file(service_id: &str) -> Result<PathBuf> {
    let temp_dir = dirs::cache_dir().ok_or_else(|| {
        CommyError::FileError(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "No cache directory",
        ))
    })?;

    let commy_dir = temp_dir.join("commy_virtual_files");
    fs::create_dir_all(&commy_dir)?;

    let file_path = commy_dir.join(format!("service_{}.mem", service_id));

    // Ensure only current user can read/write
    #[cfg(unix)]
    {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file_path, Permissions::from_mode(0o600))?;
    }

    Ok(file_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_watcher_creation() {
        let watcher = VariableFileWatcher::new(None).await.unwrap();
        assert!(watcher.watch_dir().exists());
    }

    #[tokio::test]
    async fn test_temp_file_creation() {
        let path = create_temp_service_file("test_service").await.unwrap();
        assert!(path.to_string_lossy().contains("service_test_service.mem"));
    }
}
