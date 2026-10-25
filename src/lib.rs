// SPDX-License-Identifier: MIT
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
//!     // Authenticate with a tenant; everything that needs a login is a method on the session
//!     let session = client
//!         .authenticate("my_tenant", auth::api_key("api_key_value".to_string()))
//!         .await?;
//!     
//!     // Get a service of that tenant
//!     let _service = session.get_service("config").await?;
//!     
//!     Ok(())
//! }
//! ```
//!
//! # Removed in 0.17.0
//!
//! The virtual-file layer (`VirtualVariableFile`, the file watcher, `FileAccessor`, the `Session` wrappers around
//! them and the two `*VariableChanges*` messages) never reached the server: `commy-wire` has no such messages and the
//! server answers them with an "unknown message type" error. Share bytes through `Session::open_shared`,
//! `shared_write`, `flush_shared` and `open_remote`. Each removed item is pinned by a `compile_fail` test below:
//!
//! ```compile_fail,E0432
//! use commy_sdk_rust::virtual_file::VirtualVariableFile;
//! ```
//!
//! ```compile_fail,E0432
//! use commy_sdk_rust::watcher::VariableFileWatcher;
//! ```
//!
//! ```compile_fail,E0432
//! use commy_sdk_rust::file_accessor::FileAccessor;
//! ```
//!
//! ```compile_fail,E0432
//! use commy_sdk_rust::file_accessor::LocalFileAccessor;
//! ```
//!
//! ```compile_fail,E0432
//! use commy_sdk_rust::file_accessor::RemoteFileAccessor;
//! ```
//!
//! ```compile_fail,E0599
//! fn f(s: &commy_sdk_rust::Session) { let _ = s.init_file_watcher(); }
//! ```
//!
//! ```compile_fail,E0599
//! fn f(s: &commy_sdk_rust::Session) { let _ = s.get_virtual_service_file("svc"); }
//! ```
//!
//! ```compile_fail,E0599
//! fn f(s: &commy_sdk_rust::Session) { let _ = s.start_file_monitoring(); }
//! ```
//!
//! ```compile_fail,E0599
//! fn f(s: &commy_sdk_rust::Session) { let _ = s.wait_for_file_change(); }
//! ```
//!
//! ```compile_fail,E0599
//! fn f(s: &commy_sdk_rust::Session) { let _ = s.try_get_file_change(); }
//! ```
//!
//! ```compile_fail,E0599
//! fn f(s: &commy_sdk_rust::Session) { let _ = s.stop_file_monitoring(); }
//! ```
//!
//! ```compile_fail,E0599
//! let _ = commy_sdk_rust::ClientMessage::ReportVariableChanges { service_id: String::new(), changed_variables: vec![], new_values: vec![] };
//! ```
//!
//! ```compile_fail,E0599
//! let _ = commy_sdk_rust::ServerMessage::VariableChangesAcknowledged { service_id: String::new(), changed_variables: vec![] };
//! ```
//!

pub mod auth;
pub mod client;
pub mod connection;
pub mod error;
pub mod examples_support;
pub mod local_map;
pub mod message;
pub mod service;
pub mod session;
pub mod shared_file;
pub mod state;

pub use client::Client;
pub use error::{CommyError, Result};
pub use examples_support::{CommyServer, ServerConfig};
pub use message::{ClientMessage, ServerMessage};
pub use service::Service;
pub use session::Session;

/// Library version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Compiles and runs the code blocks of `README.md` as doctests, so the front page cannot drift from the API.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
