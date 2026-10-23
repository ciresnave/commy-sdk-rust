// SPDX-License-Identifier: MIT
//! Permission-aware CRUD example
//!
//! This example demonstrates how different permissions affect CRUD operations:
//! - Admin clients with 'create_service', 'read_service', 'delete_service' permissions
//! - Read-only clients with only 'read_service' permission
//! - Creator clients with 'create_service' and 'read_service' permissions
//!
//! The protocol supports granular permission separation at each CRUD operation.
//! Automatically managed Commy server for complete self-contained demo.
//!
//! Run with: cargo run --example permissions_example

use commy_sdk_rust::{auth, Client, CommyError, CommyServer, ServerConfig};
use std::io::{self, Write};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n╔════════════════════════════════════════╗");
    println!("║  Commy Permission-Aware CRUD Example   ║");
    println!("║  (Auto-managed Commy Server)           ║");
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

    // === SCENARIO 1: Admin Client with all permissions ===
    println!("🔐 SCENARIO 1: Admin Client");
    println!("────────────────────────────────────────");
    println!("  Permissions: create, read, delete\n");

    let admin = Client::new(&server_url);
    print!("  ├─ Connecting... ");
    io::stdout().flush()?;
    admin.connect().await?;
    println!("✅");

    print!("  ├─ Authenticating with admin key... ");
    io::stdout().flush()?;

    let admin_session = admin
        .authenticate(
            "my_tenant",
            auth::api_key("admin_key_with_all_perms".to_string()),
        )
        .await?;
    println!("✅");

    println!("  ├─ Can create services");
    println!("  ├─ Can read services");
    println!("  └─ Can delete services\n");

    // Admin creates a service
    print!("  Creating service... ");
    io::stdout().flush()?;

    match admin_session.create_service("app_state", 65536).await {
        Ok(id) => println!("✅ (ID: {})", id),
        Err(CommyError::AlreadyExists(_)) => println!("ℹ️  (already exists)"),
        Err(e) => println!("⚠️  ({})", e),
    }

    print!("  Disconnecting... ");
    io::stdout().flush()?;
    admin.disconnect().await?;
    println!("✅\n");

    // === SCENARIO 2: Read-Only Client ===
    println!("🔐 SCENARIO 2: Read-Only Client");
    println!("──────────────────────────────────────");
    println!("  Permissions: read only\n");

    let reader = Client::new(&server_url);
    print!("  ├─ Connecting... ");
    io::stdout().flush()?;
    reader.connect().await?;
    println!("✅");

    print!("  ├─ Authenticating with read-only key... ");
    io::stdout().flush()?;

    let reader_session = reader
        .authenticate("my_tenant", auth::api_key("read_only_key".to_string()))
        .await?;
    println!("✅");

    println!("  ├─ Cannot create services");
    println!("  ├─ Can read services");
    println!("  └─ Cannot delete services\n");

    // Read-only client tries to read (should succeed)
    print!("  Reading service... ");
    io::stdout().flush()?;

    match reader_session.get_service("app_state").await {
        Ok(service) => println!("✅ (ID: {})", service.id),
        Err(CommyError::NotFound(_)) => println!("ℹ️  (not found)"),
        Err(e) => println!("⚠️  ({})", e),
    }

    // Read-only client tries to create (should fail with PermissionDenied)
    print!("  Attempting to create service... ");
    io::stdout().flush()?;

    match reader_session.create_service("new_service", 65536).await {
        Ok(_) => println!("❌ Unexpectedly succeeded!"),
        Err(CommyError::Unauthorized(_)) => println!("✅ Permission denied"),
        Err(CommyError::PermissionDenied(_)) => println!("✅ Permission denied"),
        Err(e) => println!("ℹ️  ({})", e),
    }

    // Read-only client tries to delete (should fail with PermissionDenied)
    print!("  Attempting to delete service... ");
    io::stdout().flush()?;

    match reader_session.delete_service("app_state").await {
        Ok(_) => println!("❌ Unexpectedly succeeded!"),
        Err(CommyError::Unauthorized(_)) => println!("✅ Permission denied"),
        Err(CommyError::PermissionDenied(_)) => println!("✅ Permission denied"),
        Err(e) => println!("ℹ️  ({})", e),
    }

    print!("  Disconnecting... ");
    io::stdout().flush()?;
    reader.disconnect().await?;
    println!("✅\n");

    // === SCENARIO 3: Service Creator ===
    println!("🔐 SCENARIO 3: Service Creator");
    println!("──────────────────────────────────────");
    println!("  Permissions: create + read\n");

    let creator = Client::new(&server_url);
    print!("  ├─ Connecting... ");
    io::stdout().flush()?;
    creator.connect().await?;
    println!("✅");

    print!("  ├─ Authenticating with creator key... ");
    io::stdout().flush()?;

    let creator_session = creator
        .authenticate("my_tenant", auth::api_key("creator_key".to_string()))
        .await?;
    println!("✅");

    println!("  ├─ Can create services");
    println!("  ├─ Can read services");
    println!("  └─ Cannot delete services\n");

    // Creator creates a new service
    print!("  Creating service... ");
    io::stdout().flush()?;

    match creator_session.create_service("user_cache", 65536).await {
        Ok(id) => println!("✅ (ID: {})", id),
        Err(CommyError::AlreadyExists(_)) => println!("ℹ️  (already exists)"),
        Err(e) => println!("⚠️  ({})", e),
    }

    // Creator reads the service
    print!("  Reading service... ");
    io::stdout().flush()?;

    match creator_session.get_service("user_cache").await {
        Ok(service) => println!("✅ (ID: {})", service.id),
        Err(e) => println!("⚠️  ({})", e),
    }

    // Creator tries to delete (should fail)
    print!("  Attempting to delete service... ");
    io::stdout().flush()?;

    match creator_session.delete_service("user_cache").await {
        Ok(_) => println!("❌ Unexpectedly succeeded!"),
        Err(CommyError::PermissionDenied(_)) => println!("✅ Permission denied"),
        Err(CommyError::Unauthorized(_)) => println!("✅ Permission denied"),
        Err(e) => println!("ℹ️  ({})", e),
    }

    print!("  Disconnecting... ");
    io::stdout().flush()?;
    creator.disconnect().await?;
    println!("✅\n");

    // ========================================================================
    // SUMMARY
    // ========================================================================
    println!("═══════════════════════════════════════════════════════════");
    println!("Permission Model Summary");
    println!("═══════════════════════════════════════════════════════════\n");

    println!("┌──────────────────┬──────────────────────────────────────┐");
    println!("│ Permission       │ Operation                            │");
    println!("├──────────────────┼──────────────────────────────────────┤");
    println!("│ create_service   │ create_service()                     │");
    println!("│ read_service     │ get_service()                        │");
    println!("│ delete_service   │ delete_service()                     │");
    println!("└──────────────────┴──────────────────────────────────────┘\n");

    println!("Benefits of granular permissions:");
    println!("  ✅ Principle of least privilege");
    println!("  ✅ Explicit vs implicit operations");
    println!("  ✅ Clear permission boundaries");
    println!("  ✅ Better security auditing\n");

    println!("╔════════════════════════════════════════╗");
    println!("║  ✅ Example completed successfully!    ║");
    println!("║  Server will be stopped automatically  ║");
    println!("╚════════════════════════════════════════╝\n");

    Ok(())
}
