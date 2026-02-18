//! Basic example showing how to connect, authenticate, and use Commy
//!
//! This example is completely self-contained:
//! - Automatically downloads and starts a Commy server
//! - Connects a client to the server
//! - Authenticates to a tenant
//! - Performs service operations
//! - Disconnects and stops the server
//!
//! Run with: cargo run --example basic_client

use commy_sdk_rust::{auth, Client, CommyServer, ServerConfig};
use std::io::{self, Write};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n╔════════════════════════════════════════╗");
    println!("║    Commy Basic Client Example          ║");
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
        // SETUP: Start Commy server automatically (standalone mode)
        // ====================================================================
        println!("📦 Setting up Commy server");
        println!("──────────────────────────");

        let config = ServerConfig::default();
        let mut server = CommyServer::new(config);

        print!("  ├─ Preparing server (download binary, generate certs)... ");
        io::stdout().flush()?;
        server.prepare().await?;
        println!("✅");

        print!("  ├─ Starting server process... ");
        io::stdout().flush()?;
        server.start().await?;
        println!("✅");

        let url = server.url().to_string();
        println!("  └─ Server ready at: {}\n", url);

        url
    };

    // ============================================================================
    // CLIENT: Connect and authenticate
    // ============================================================================
    println!("🔌 Connecting client");
    println!("────────────────────");

    // Create a new client pointing to our server
    let client = Client::new(&server_url);
    println!("  ├─ Client ID: {}", client.id());

    // Connect to server
    print!("  ├─ Connecting to server... ");
    io::stdout().flush()?;
    client.connect().await?;
    println!("✅");

    // Authenticate to a tenant with API key
    println!("  ├─ Tenant: my_tenant");
    print!("  ├─ Authenticating with API key... ");
    io::stdout().flush()?;

    match client
        .authenticate("my_tenant", auth::api_key("test_key_123".to_string()))
        .await
    {
        Ok(auth_ctx) => {
            println!("✅");
            println!("  │  └─ Permissions: {:?}", auth_ctx.permissions);
        }
        Err(e) => {
            println!("⚠️  ({})", e);
            println!("  │  └─ Note: This is normal if server has no tenant config");
        }
    }

    println!("  └─ Connected!\n");

    // ============================================================================
    // OPERATIONS: Use service operations
    // ============================================================================
    println!("📋 Performing service operations");
    println!("─────────────────────────────────");

    // Try to create a service
    println!("  ├─ Service name: config");
    print!("  ├─ Creating service... ");
    io::stdout().flush()?;

    match client.create_service("my_tenant", "config").await {
        Ok(id) => {
            println!("✅");
            println!("  │  └─ ID: {}", id);
        }
        Err(commy_sdk_rust::CommyError::AlreadyExists(_)) => {
            println!("ℹ️  (already exists)");
        }
        Err(e) => {
            println!("⚠️  ({})", e);
        }
    }

    // Read service info
    print!("  ├─ Reading service info... ");
    io::stdout().flush()?;

    match client.get_service("my_tenant", "config").await {
        Ok(service) => {
            println!("✅");
            println!("  │  └─ Service ID: {}", service.id);
        }
        Err(e) => {
            println!("⚠️  ({})", e);
        }
    }

    // Send heartbeat
    print!("  ├─ Sending heartbeat... ");
    io::stdout().flush()?;

    match client.heartbeat().await {
        Ok(_) => println!("✅"),
        Err(e) => println!("⚠️  ({})", e),
    }

    // Try to delete service
    print!("  ├─ Deleting service... ");
    io::stdout().flush()?;

    match client.delete_service("my_tenant", "config").await {
        Ok(_) => println!("✅"),
        Err(commy_sdk_rust::CommyError::NotFound(_)) => println!("ℹ️  (not found)"),
        Err(e) => println!("⚠️  ({})", e),
    }

    println!("  └─ Done!\n");

    // ============================================================================
    // CLEANUP: Disconnect and stop server
    // ============================================================================
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
