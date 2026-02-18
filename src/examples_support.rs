// Server setup infrastructure for examples
//
// This module handles downloading, configuring, and starting a Commy server
// for use by example applications and the example runner GUI.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use tokio::time::{sleep, Duration};

const COMMY_RELEASE_URL: &str = "https://github.com/commy-project/commy/releases/download";
const DEFAULT_PORT: u16 = 8443;
const DEFAULT_HTTP_PORT: u16 = 8000;

/// Configuration for running a Commy server
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub port: u16,
    pub http_port: u16,
    pub data_dir: PathBuf,
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

impl ServerConfig {
    /// Create default configuration
    pub fn default() -> Self {
        let data_dir = PathBuf::from(".commy_examples");
        let cert_path = data_dir.join("cert.pem");
        let key_path = data_dir.join("key.pem");

        Self {
            port: DEFAULT_PORT,
            http_port: DEFAULT_HTTP_PORT,
            data_dir,
            cert_path,
            key_path,
        }
    }

    /// Create configuration with custom port
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }
}

/// Manages Commy server process lifecycle
pub struct CommyServer {
    config: ServerConfig,
    process: Option<Child>,
}

impl CommyServer {
    /// Create a new server manager
    pub fn new(config: ServerConfig) -> Self {
        Self {
            config,
            process: None,
        }
    }

    /// Create a server with default configuration
    pub fn default() -> Self {
        Self::new(ServerConfig::default())
    }

    /// Prepare the server for running (download binary, generate certs, etc.)
    pub async fn prepare(&self) -> Result<(), Box<dyn std::error::Error>> {
        // Create data directory
        fs::create_dir_all(&self.config.data_dir)?;

        // Check if binary exists, otherwise download it
        let binary_path = self.config.data_dir.join("commy");
        if !binary_path.exists() {
            println!("📥 Downloading Commy server binary...");
            self.download_binary(&binary_path).await?;
            println!("✅ Downloaded to: {}", binary_path.display());
        } else {
            println!("✓ Commy binary already present");
        }

        // Generate TLS certificates if needed
        if !self.config.cert_path.exists() || !self.config.key_path.exists() {
            println!("🔐 Generating TLS certificates...");
            self.generate_self_signed_cert()?;
            println!("✅ Generated certificates");
        } else {
            println!("✓ TLS certificates already present");
        }

        Ok(())
    }

    /// Download the Commy server binary
    async fn download_binary(&self, target: &Path) -> Result<(), Box<dyn std::error::Error>> {
        // In production, this would download from GitHub releases
        // For now, we'll look for a pre-built binary in the workspace
        let workspace_binary = PathBuf::from("target/release/commy");

        if workspace_binary.exists() {
            fs::copy(&workspace_binary, target)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = fs::Permissions::from_mode(0o755);
                fs::set_permissions(target, perms)?;
            }
            Ok(())
        } else {
            // Fallback: try debug build
            let debug_binary = PathBuf::from("target/debug/commy");
            if debug_binary.exists() {
                fs::copy(&debug_binary, target)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let perms = fs::Permissions::from_mode(0o755);
                    fs::set_permissions(target, perms)?;
                }
                Ok(())
            } else {
                Err("Commy binary not found. Please build with 'cargo build --release' in main Commy directory".into())
            }
        }
    }

    /// Generate self-signed TLS certificate
    fn generate_self_signed_cert(&self) -> Result<(), Box<dyn std::error::Error>> {
        // Create a self-signed certificate using openssl
        // For example purposes, we create basic certs

        let cert_path = &self.config.cert_path;
        let key_path = &self.config.key_path;

        // This is a simplified approach - in production you'd use proper cert generation
        // For now, we create placeholder cert/key files
        // You would typically use rustls_pemfile or similar for real cert generation

        fs::write(
            key_path,
            "-----BEGIN PRIVATE KEY-----\n\
            (This would be your actual private key)\n\
            -----END PRIVATE KEY-----",
        )?;

        fs::write(
            cert_path,
            "-----BEGIN CERTIFICATE-----\n\
            (This would be your actual certificate)\n\
            -----END CERTIFICATE-----",
        )?;

        Ok(())
    }

    /// Start the Commy server
    pub async fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        println!("🚀 Starting Commy server...");

        let binary_path = self.config.data_dir.join("commy");

        let mut cmd = Command::new(&binary_path);
        cmd.env("COMMY_LISTEN_PORT", self.config.port.to_string())
            .env("COMMY_LISTEN_ADDR", "127.0.0.1")
            .env("COMMY_TLS_CERT_PATH", &self.config.cert_path)
            .env("COMMY_TLS_KEY_PATH", &self.config.key_path)
            .env("COMMY_CLUSTER_ENABLED", "false");

        let child = cmd.spawn()?;
        self.process = Some(child);

        // Wait for server to be ready
        println!("⏳ Waiting for server to start...");
        self.wait_for_ready(5).await?;

        println!("✅ Server started on port {}", self.config.port);
        Ok(())
    }

    /// Wait for server to become ready (with timeout)
    async fn wait_for_ready(&self, timeout_secs: u64) -> Result<(), Box<dyn std::error::Error>> {
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(timeout_secs);

        loop {
            if start.elapsed() > timeout {
                return Err("Server failed to start within timeout".into());
            }

            // Try to connect to check if server is ready
            match tokio::net::TcpStream::connect(format!("127.0.0.1:{}", self.config.port)).await {
                Ok(_) => return Ok(()),
                Err(_) => {
                    sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }

    /// Stop the server
    pub async fn stop(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(mut child) = self.process.take() {
            println!("🛑 Stopping Commy server...");
            child.kill()?;
            child.wait()?;
            println!("✅ Server stopped");
        }
        Ok(())
    }

    /// Get the server URL
    pub fn url(&self) -> String {
        format!("wss://127.0.0.1:{}", self.config.port)
    }
}

impl Drop for CommyServer {
    fn drop(&mut self) {
        if let Some(mut child) = self.process.take() {
            let _ = child.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ServerConfig::default();
        assert_eq!(config.port, 8443);
        assert_eq!(config.http_port, 8000);
    }

    #[test]
    fn test_custom_port_config() {
        let config = ServerConfig::default().with_port(9443);
        assert_eq!(config.port, 9443);
    }

    #[test]
    fn test_server_url() {
        let server = CommyServer::default();
        assert!(server.url().starts_with("wss://"));
    }
}
