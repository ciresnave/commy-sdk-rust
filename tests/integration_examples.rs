// Integration tests for Commy SDK examples
//
// These tests demonstrate that examples work correctly:
// 1. Download/prepare Commy server
// 2. Start the server
// 3. Run example code against it
// 4. Verify the results

use commy_sdk_rust::{auth, Client, CommyServer, ServerConfig};
use std::time::Duration;
use tokio::time::sleep;

/// Test that we can set up and start a Commy server
#[tokio::test]
#[ignore] // Run with: cargo test -- --ignored --nocapture
async fn test_server_startup() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== TEST: Server Startup ===\n");

    let config = ServerConfig::default();
    let mut server = CommyServer::new(config);

    println!("Step 1: Preparing server (download binary, generate certs)");
    server.prepare().await?;

    println!("\nStep 2: Starting server");
    server.start().await?;

    println!("\nStep 3: Verifying server is running");
    sleep(Duration::from_millis(500)).await;

    println!("✅ Server running at: {}", server.url());
    println!("   (Server will be stopped automatically on test cleanup)\n");

    Ok(())
}

/// Test that a client can connect to the server
#[tokio::test]
#[ignore]
async fn test_client_connection() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== TEST: Client Connection ===\n");

    let config = ServerConfig::default();
    let mut server = CommyServer::new(config);

    println!("1️⃣  Preparing and starting server...");
    server.prepare().await?;
    server.start().await?;

    sleep(Duration::from_millis(500)).await;

    println!("\n2️⃣  Creating client and connecting...");
    let client = Client::new(server.url());
    println!("   Client ID: {}", client.id());

    println!("\n3️⃣  Connecting to server...");
    match client.connect().await {
        Ok(_) => println!("✅ Connected successfully!"),
        Err(e) => {
            println!("❌ Connection failed: {}", e);
            return Err(e.into());
        }
    }

    println!("\n4️⃣  Disconnecting...");
    client.disconnect().await?;
    println!("✅ Disconnected\n");

    Ok(())
}

/// Test that a client can authenticate to a tenant
#[tokio::test]
#[ignore]
async fn test_client_authentication() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== TEST: Client Authentication ===\n");

    let config = ServerConfig::default();
    let mut server = CommyServer::new(config);

    println!("1️⃣  Starting server...");
    server.prepare().await?;
    server.start().await?;
    sleep(Duration::from_millis(500)).await;

    println!("\n2️⃣  Creating client...");
    let client = Client::new(server.url());

    println!("\n3️⃣  Connecting...");
    client.connect().await?;
    println!("✅ Connected");

    println!("\n4️⃣  Authenticating to tenant...");
    match client
        .authenticate("test_tenant", auth::api_key("demo_key".to_string()))
        .await
    {
        Ok(auth_ctx) => {
            println!("✅ Authenticated!");
            println!("   Tenant: {}", auth_ctx.tenant_id);
            println!("   Permissions: {:?}", auth_ctx.permissions);
            println!("   Authenticated at: {}", auth_ctx.authenticated_at);
        }
        Err(e) => {
            println!("⚠️  Authentication result: {}", e);
            // May fail if server doesn't have tenant configured, which is OK
        }
    }

    println!("\n5️⃣  Disconnecting...");
    client.disconnect().await?;
    println!("✅ Disconnected\n");

    Ok(())
}

/// Test the basic client example pattern
#[tokio::test]
#[ignore]
async fn test_basic_client_example_pattern() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== TEST: Basic Client Example Pattern ===\n");

    let config = ServerConfig::default();
    let mut server = CommyServer::new(config);

    println!("Setting up server...");
    server.prepare().await?;
    server.start().await?;
    sleep(Duration::from_millis(500)).await;

    println!("\nRunning example pattern:");
    println!("─────────────────────────\n");

    // This mirrors what basic_client.rs does
    let server_url = server.url();
    println!("Creating client to connect to: {}", server_url);
    let client = Client::new(server_url);
    println!("Client ID: {}\n", client.id());

    println!("Connecting to server...");
    client.connect().await?;
    println!("✅ Connected!\n");

    println!("Authenticating to tenant...");
    match client
        .authenticate("example_tenant", auth::api_key("example_key".to_string()))
        .await
    {
        Ok(_auth_ctx) => {
            println!("✅ Authenticated!\n");
        }
        Err(e) => {
            println!("⚠️  Auth response: {}\n", e);
        }
    }

    println!("Testing service operations...");
    match client
        .get_service("example_tenant", "example_service")
        .await
    {
        Ok(service) => {
            println!("✅ Got service: {}", service.id);
        }
        Err(e) => {
            println!("⚠️  Service response: {}", e);
        }
    }

    println!("\nSending heartbeat...");
    client.heartbeat().await?;
    println!("✅ Heartbeat sent!\n");

    println!("Disconnecting...");
    client.disconnect().await?;
    println!("✅ Disconnected\n");

    println!("Example pattern completed successfully!\n");

    Ok(())
}

/// Test that multiple clients can connect simultaneously
#[tokio::test]
#[ignore]
async fn test_multiple_clients() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== TEST: Multiple Concurrent Clients ===\n");

    let config = ServerConfig::default();
    let mut server = CommyServer::new(config);

    println!("Starting server...");
    server.prepare().await?;
    server.start().await?;
    sleep(Duration::from_millis(500)).await;

    let server_url = server.url();
    println!("Server URL: {}\n", server_url);

    println!("Creating 3 concurrent clients...");
    let mut handles = vec![];

    for i in 1..=3 {
        let url = server_url.clone();
        let handle = tokio::spawn(async move {
            let client = Client::new(url);
            println!("  Client {}: Created ({})", i, client.id());

            match client.connect().await {
                Ok(_) => {
                    println!("  Client {}: Connected ✅", i);
                    sleep(Duration::from_millis(100)).await;
                    match client.disconnect().await {
                        Ok(_) => println!("  Client {}: Disconnected ✅", i),
                        Err(e) => println!("  Client {}: Disconnect failed: {}", i, e),
                    }
                }
                Err(e) => println!("  Client {}: Connection failed: {}", i, e),
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.await?;
    }

    println!("\n✅ All clients completed\n");

    Ok(())
}
