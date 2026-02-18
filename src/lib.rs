//! Commy Rust Client SDK
//!
//! A high-level Rust client for interacting with the Commy shared memory coordination system.
//!
//! # Features
//!
//! - WebSocket Secure (WSS) client for remote connections
//! - Direct memory-mapping support for local processes
//! - Automatic connection management with reconnection
//! - Full async/await support with Tokio
//! - Multiple authentication methods
//!
//! # Example
//!
//! ```no_run
//! use commy_sdk_rust::{Client, auth};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create a new client
//!     let client = Client::new("wss://localhost:9000");
//!     
//!     // Connect to server
//!     client.connect().await?;
//!     
//!     // Authenticate with a tenant
//!     client.authenticate("my_tenant", auth::api_key("api_key_value".to_string())).await?;
//!     
//!     // Create or get a service
//!     let _service = client.get_service("my_tenant", "config").await?;
//!     
//!     Ok(())
//! }
//! ```

pub mod auth;
pub mod client;
pub mod connection;
pub mod error;
pub mod examples_support;
pub mod file_accessor;
pub mod message;
pub mod service;
pub mod state;
pub mod virtual_file;
pub mod watcher;

pub use client::Client;
pub use error::{CommyError, Result};
pub use examples_support::{CommyServer, ServerConfig};
pub use message::{ClientMessage, ServerMessage};
pub use service::Service;

/// Library version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
