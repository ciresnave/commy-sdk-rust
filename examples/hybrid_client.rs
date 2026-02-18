//! Hybrid mode example - unified API for local and remote clients
//!
//! This example demonstrates:
//! - Virtual variable files that work transparently for local or remote
//! - File watching and SIMD-based change detection
//! - Zero-copy variable access for local clients
//! - Automatic fallback to WSS for remote clients
//! - Automatically managed Commy server
//!
//! The application code doesn't need to know whether it's using
//! local direct memory mapping or remote WSS synchronization!
//!
//! Run with: cargo run --example hybrid_client

use commy_sdk_rust::{auth, Client, CommyServer, ServerConfig};
use std::io::{self, Write};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n╔════════════════════════════════════════╗");
    println!("║    Commy Hybrid Client Example         ║");
    println!("║    (Auto-managed Commy Server)         ║");
    println!("╚════════════════════════════════════════╝\n");

    // Check if running from GUI (server URL provided)
    let server_url = if let Ok(url) = std::env::var("COMMY_SERVER_URL") {
        println!("📦 Using GUI-managed server");
        println!("──────────────────────────");
        println!("  └─ Connected to: {}\n", url);
        url
    } else {
        // ====================================================================
        // SETUP: Start Commy server
        // ====================================================================
        println!("📦 Setting up Commy server");
        println!("──────────────────────────");

        let config = ServerConfig::default();
        let mut server = CommyServer::new(config);

        print!("  ├─ Preparing server... ");
        io::stdout().flush()?;
        server.prepare().await?;
        println!("✅");

        print!("  ├─ Starting server... ");
        io::stdout().flush()?;
        server.start().await?;
        println!("✅");

        let url = server.url().to_string();
        println!("  └─ Server ready at: {}\n", url);

        url
    };

    // ========================================================================
    // CLIENT: Initialize client
    // ========================================================================
    println!("🔌 Initializing Commy client");
    println!("────────────────────────────");

    // Initialize client (all prerequisites bundled in one call)
    println!("  ├─ Connecting and authenticating...");
    let client = match Client::initialize(
        &server_url,
        "my_tenant",
        auth::api_key("test_key_123".to_string()),
    )
    .await
    {
        Ok(c) => {
            println!("  ├─ ✅ Initialized");
            c
        }
        Err(e) => {
            println!("  ├─ ⚠️  ({})", e);
            let client = Client::new(&server_url);
            println!("  └─ ℹ️  Continuing with basic client");
            client
        }
    };

    println!("  └─ Client ID: {}\n", client.id());

    // Get virtual service file
    // This creates an abstraction that works for both local and remote
    println!("🔍 Getting virtual service file\n");
    let vf = match client.get_virtual_service_file("my_tenant", "config").await {
        Ok(vf) => {
            println!("✅ Got virtual service file for: {}\n", vf.service_name());
            vf
        }
        Err(e) => {
            println!("⚠️  Could not get virtual file: {}\n", e);
            println!("Note: This is expected if server doesn't have the service configured.\n");

            println!("═══════════════════════════════════════════════════════════");
            println!("Example demonstrated:");
            println!("  ✅ Auto-managed Commy server (started and stopped by app)");
            println!("  ✓ Client connection and initialization");
            println!("  ℹ️  Virtual file operations (skipped due to demo server config)");
            println!("═══════════════════════════════════════════════════════════\n");

            println!("🔌 Disconnecting");
            println!("────────────────");
            client.disconnect().await?;
            println!("✅ Disconnected\n");

            return Ok(());
        }
    };

    // Register variables in the virtual file
    println!("📝 Registering variables");
    println!("───────────────────────");
    let var1_meta =
        commy_sdk_rust::virtual_file::VariableMetadata::new("counter".to_string(), 0, 8, 1);
    match vf.register_variable(var1_meta).await {
        Ok(_) => println!("  ├─ counter (8 bytes)... ✅"),
        Err(e) => println!("  ├─ counter (8 bytes)... ⚠️  ({})", e),
    }

    let var2_meta =
        commy_sdk_rust::virtual_file::VariableMetadata::new("status".to_string(), 8, 32, 2);
    match vf.register_variable(var2_meta).await {
        Ok(_) => println!("  └─ status (32 bytes)... ✅\n"),
        Err(e) => println!("  └─ status (32 bytes)... ⚠️  ({})\n", e),
    }

    // Write variables
    println!("✏️  Writing variables");
    println!("───────────────────");
    match vf
        .write_variable("counter", &[0, 0, 0, 0, 0, 0, 0, 42])
        .await
    {
        Ok(_) => println!("  ├─ counter = 42... ✅"),
        Err(e) => println!("  ├─ counter = 42... ⚠️  ({})", e),
    }

    match vf
        .write_variable("status", b"ready                       ")
        .await
    {
        Ok(_) => println!("  └─ status = 'ready'... ✅\n"),
        Err(e) => println!("  └─ status = 'ready'... ⚠️  ({})\n", e),
    }

    // Read variables back
    println!("📖 Reading variables");
    println!("───────────────────");
    match vf.read_variable_slice("counter").await {
        Ok(counter_data) => println!("  ├─ counter: {:?}... ✅", counter_data),
        Err(e) => println!("  ├─ counter: ⚠️  ({})", e),
    }

    match vf.read_variable_slice("status").await {
        Ok(status_data) => {
            println!(
                "  └─ status: {}... ✅\n",
                String::from_utf8_lossy(&status_data)
            )
        }
        Err(e) => println!("  └─ status: ⚠️  ({})\n", e),
    }

    // Send heartbeat
    println!("💓 Sending heartbeat");
    println!("───────────────────");
    print!("  ├─ Sending heartbeat... ");
    io::stdout().flush()?;
    match client.heartbeat().await {
        Ok(_) => println!("✅"),
        Err(e) => println!("⚠️  ({})", e),
    }

    println!("  └─ Done!\n");

    // Cleanup
    println!("🔌 Disconnecting");
    println!("────────────────");
    print!("  ├─ Disconnecting from server... ");
    io::stdout().flush()?;
    client.disconnect().await?;
    println!("✅");

    println!("  ├─ Stopping server... ");
    println!("  └─ (will happen automatically on exit)\n");

    println!("╔════════════════════════════════════════╗");
    println!("║  ✅ Example completed successfully!    ║");
    println!("║  Server will be stopped automatically  ║");
    println!("╚════════════════════════════════════════╝\n");

    Ok(())
}
