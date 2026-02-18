use axum::{
    extract::{Path, State},
    response::Html,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

/// Example metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Example {
    name: String,
    description: String,
    time_estimate: String,
    difficulty: String,
}

/// API response for running an example
#[derive(Debug, Serialize, Deserialize)]
struct RunResponse {
    success: bool,
    message: String,
    process_id: Option<u32>,
    output: Option<String>,
}

/// Shared application state
#[derive(Clone)]
struct AppState {
    running_processes: Arc<Mutex<HashMap<String, Vec<u32>>>>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n╔════════════════════════════════════════╗");
    println!("║    Commy Examples GUI Runner           ║");
    println!("╚════════════════════════════════════════╝\n");

    println!("📦 Starting Commy server...");

    // Try to start Commy server by spawning the binary directly
    let commy_url = "wss://127.0.0.1:8443";

    // Find Commy binary location relative to this executable
    // Path: commy/commy-sdk-rust/target/release/examples_gui.exe
    // So we need to go: up 3 levels to commy/, then into target/release/
    let commy_binary_path = if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            // exe_dir = .../target/release
            if let Some(target_dir) = exe_dir.parent() {
                // target_dir = .../target
                if let Some(sdk_dir) = target_dir.parent() {
                    // sdk_dir = .../commy-sdk-rust
                    if let Some(commy_dir) = sdk_dir.parent() {
                        // commy_dir = .../commy
                        commy_dir.join("target").join("release").join("commy.exe")
                    } else {
                        std::path::PathBuf::from("../../target/release/commy.exe")
                    }
                } else {
                    std::path::PathBuf::from("../../target/release/commy.exe")
                }
            } else {
                std::path::PathBuf::from("../../target/release/commy.exe")
            }
        } else {
            std::path::PathBuf::from("../../target/release/commy.exe")
        }
    } else {
        std::path::PathBuf::from("../../target/release/commy.exe")
    };

    // Get the directory containing the commy.exe (parent of target directory)
    let commy_work_dir = commy_binary_path
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or_else(|| std::path::Path::new("."));

    let cert_path = commy_work_dir.join("dev-cert.pem");
    let key_path = commy_work_dir.join("dev-key.pem");

    if commy_binary_path.exists() {
        match tokio::process::Command::new(&commy_binary_path)
            .env("COMMY_LISTEN_ADDR", "127.0.0.1")
            .env("COMMY_CLUSTER_ENABLED", "false")
            .env("COMMY_LISTEN_PORT", "8443")
            .env(
                "COMMY_TLS_CERT_PATH",
                cert_path.to_string_lossy().to_string(),
            )
            .env("COMMY_TLS_KEY_PATH", key_path.to_string_lossy().to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(_) => {
                // Give server time to start
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                println!("  ├─ Commy server started ✅");
                println!("  └─ Running at {}\n", commy_url);
            }
            Err(e) => {
                println!("  ├─ Warning: Could not start Commy server: {}", e);
                println!("  └─ Examples may not work without it\n");
            }
        }
    } else {
        println!(
            "  ├─ Warning: Commy binary not found at {}",
            commy_binary_path.display()
        );
        println!("  └─ Examples may not work without the server\n");
    }

    // Setup app state
    let app_state = AppState {
        running_processes: Arc::new(Mutex::new(HashMap::new())),
    };

    // Build router
    let app = Router::new()
        // Root path serves index.html
        .route("/", get(serve_index))
        // API endpoints
        .route("/api/examples", get(list_examples))
        .route("/api/examples/:name", get(get_example))
        .route("/api/examples/:name/run", post(run_example))
        .route("/api/examples/:name/stop", post(stop_example))
        .route("/api/running", get(list_running))
        // Static files (HTML/CSS/JS)
        .fallback_service(ServeDir::new("src/bin/gui"))
        .layer(CorsLayer::permissive())
        .with_state(app_state);

    // Start web server
    println!("🌐 Starting GUI server...");
    let gui_port = "8080";
    let gui_addr = "127.0.0.1:8080";

    println!("  ├─ Frontend at: http://{}", gui_addr);
    println!("  ├─ API at: http://{}/api", gui_addr);
    println!("  └─ Open in browser to get started!\n");

    let listener = match tokio::net::TcpListener::bind(gui_addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "❌ Error: Could not bind to {}. Is another instance running?",
                gui_addr
            );
            eprintln!("   Error: {}", e);
            std::process::exit(1);
        }
    };

    axum::serve(listener, app).await?;

    Ok(())
}

async fn serve_index() -> Html<&'static str> {
    const INDEX_HTML: &str = include_str!("gui/index.html");
    Html(INDEX_HTML)
}

async fn list_examples() -> Json<Vec<Example>> {
    let examples = vec![
        Example {
            name: "basic_client".to_string(),
            description: "Learn core CRUD operations with Commy services. Connect, authenticate, create/read/delete services, and manage heartbeats.".to_string(),
            time_estimate: "~5 seconds".to_string(),
            difficulty: "Beginner".to_string(),
        },
        Example {
            name: "hybrid_client".to_string(),
            description: "Understand hybrid local/remote access patterns using virtual files. See how the same code works transparently whether accessing local mapped files or remote services.".to_string(),
            time_estimate: "~5 seconds".to_string(),
            difficulty: "Intermediate".to_string(),
        },
        Example {
            name: "permissions_example".to_string(),
            description: "See granular permission control in action. Watch admin, read-only, and creator clients interact with the same service under different authorization constraints.".to_string(),
            time_estimate: "~7 seconds".to_string(),
            difficulty: "Intermediate".to_string(),
        },
    ];
    Json(examples)
}

async fn get_example(Path(name): Path<String>) -> Result<Json<Example>, String> {
    let examples = list_examples().await.0;
    examples
        .into_iter()
        .find(|e| e.name == name)
        .map(Json)
        .ok_or_else(|| format!("Example '{}' not found", name))
}

async fn run_example(Path(name): Path<String>, State(state): State<AppState>) -> Json<RunResponse> {
    // Run the pre-built example binary directly
    let exe_path = format!("target/release/examples/{}.exe", name);
    match tokio::process::Command::new(&exe_path)
        .env("COMMY_SERVER_URL", "wss://127.0.0.1:8443")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(mut child) => {
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();

            // Capture stdout
            let stdout_output = if let Some(stdout) = stdout {
                let reader = BufReader::new(stdout);
                let mut lines = Vec::new();
                let mut lines_reader = reader.lines();
                while let Ok(Some(line)) = lines_reader.next_line().await {
                    lines.push(line);
                }
                lines.join("\n")
            } else {
                String::new()
            };

            // Capture stderr
            let stderr_output = if let Some(stderr) = stderr {
                let reader = BufReader::new(stderr);
                let mut lines = Vec::new();
                let mut lines_reader = reader.lines();
                while let Ok(Some(line)) = lines_reader.next_line().await {
                    lines.push(line);
                }
                lines.join("\n")
            } else {
                String::new()
            };

            // Wait for process to complete
            let output = match child.wait().await {
                Ok(status) => {
                    let combined = format!(
                        "{}{}\n\n[Exit code: {}]",
                        if stdout_output.is_empty() {
                            String::new()
                        } else {
                            format!("{}\n", stdout_output)
                        },
                        if stderr_output.is_empty() {
                            String::new()
                        } else {
                            format!("STDERR:\n{}\n", stderr_output)
                        },
                        status.code().unwrap_or(-1)
                    );
                    combined
                }
                Err(e) => format!("Error running example: {}", e),
            };

            Json(RunResponse {
                success: true,
                message: format!("Ran example '{}'", name),
                process_id: None,
                output: Some(output),
            })
        }
        Err(e) => Json(RunResponse {
            success: false,
            message: format!("Failed to start example: {}", e),
            process_id: None,
            output: None,
        }),
    }
}

async fn stop_example(
    Path(name): Path<String>,
    State(state): State<AppState>,
) -> Json<RunResponse> {
    if let Ok(mut processes) = state.running_processes.lock() {
        if let Some(pids) = processes.get_mut(&name) {
            pids.clear();
            return Json(RunResponse {
                success: true,
                message: format!("Stopped all instances of '{}'", name),
                process_id: None,
                output: None,
            });
        }
    }

    Json(RunResponse {
        success: false,
        message: format!("No running instances of '{}'", name),
        process_id: None,
        output: None,
    })
}

async fn list_running(State(state): State<AppState>) -> Json<HashMap<String, Vec<u32>>> {
    state
        .running_processes
        .lock()
        .map(|p| p.clone())
        .unwrap_or_default()
        .into()
}
