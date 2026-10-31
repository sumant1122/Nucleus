//! # sandboxd
//!
//! Nucleus agent sandbox service: a privileged local daemon that manages
//! persistent sandboxes for AI agents. Exposes an HTTP/JSON API over a Unix
//! socket, plus exec, file, snapshot, and event-stream endpoints.

use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Json, Sse},
    routing::{get, post},
};

use nucleus_api::{
    ExecRequest, ExecResult, ListSandboxesResponse, NetworkPolicy, Sandbox, SandboxEvent,
    SandboxSpec, SandboxStatus, StreamExecRequest, new_sandbox_id,
};
use nucleus_core::{
    ContainerConfig, ContainerState, NetworkPolicy as CoreNetworkPolicy, create_container,
    destroy_container, get_container_config, get_container_state, list_configs, start_container,
    stop_container,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio_stream::Stream;
use tracing::{error, info};

const SOCKET_PATH: &str = "/run/nucleus/sandboxd.sock";

/// Shared application state.
#[derive(Clone)]
struct AppState {
    /// In-flight event channels per sandbox.
    events: Arc<Mutex<HashMap<String, broadcast::Sender<SandboxEvent>>>>,
}

impl AppState {
    fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn event_sender(&self, sandbox_id: &str) -> broadcast::Sender<SandboxEvent> {
        let mut events = self.events.lock().unwrap();
        if let Some(tx) = events.get(sandbox_id) {
            return tx.clone();
        }
        let (tx, _rx) = broadcast::channel(128);
        events.insert(sandbox_id.to_string(), tx.clone());
        tx
    }

    async fn emit_event(&self, sandbox_id: &str, event: SandboxEvent) {
        let events = self.events.lock().unwrap();
        if let Some(tx) = events.get(sandbox_id) {
            let _ = tx.send(event);
        }
    }
}

/// Convert a SandboxSpec into a nucleus-core ContainerConfig.
fn spec_to_config(spec: &SandboxSpec) -> ContainerConfig {
    ContainerConfig {
        name: spec.id.clone().unwrap_or_else(new_sandbox_id),
        image: spec.image.clone(),
        command: spec.command.clone(),
        env: spec.env.clone(),
        workdir: spec.workdir.clone(),
        memory: spec.memory.clone().unwrap_or("1G".to_string()),
        cpus: spec.cpus,
        pids_limit: spec.pids_limit,
        rootless: spec.rootless,
        readonly: spec.readonly,
        network: match spec.network {
            nucleus_api::NetworkPolicy::None => CoreNetworkPolicy::None,
            nucleus_api::NetworkPolicy::Allowlist => CoreNetworkPolicy::Allowlist {
                destinations: vec![],
            },
            nucleus_api::NetworkPolicy::Open => CoreNetworkPolicy::Open,
        },
        ip: None,
        ports: vec![],
        volumes: vec![],
        created_at: 0,
    }
}

/// Convert a nucleus-core ContainerState into an API Sandbox.
fn state_to_sandbox(state: &ContainerState, config: Option<&ContainerConfig>) -> Sandbox {
    let status = match state.status.as_str() {
        "Running" => SandboxStatus::Running,
        "Stopped" | "" => SandboxStatus::Stopped,
        "Error" => SandboxStatus::Error,
        _ => SandboxStatus::Stopped,
    };

    let created_at = config
        .and_then(|c| chrono::DateTime::from_timestamp(c.created_at as i64, 0))
        .unwrap_or_else(chrono::Utc::now);

    Sandbox {
        id: state.name.clone(),
        image: state.image.clone(),
        status,
        pid: Some(state.pid),
        ip: if state.ip.is_empty() {
            None
        } else {
            Some(state.ip.clone())
        },
        memory: config.and_then(|c| {
            if c.memory.is_empty() {
                None
            } else {
                Some(c.memory.clone())
            }
        }),
        cpus: config.and_then(|c| c.cpus),
        pids_limit: config.and_then(|c| c.pids_limit),
        network: config
            .map(|c| match &c.network {
                CoreNetworkPolicy::None => nucleus_api::NetworkPolicy::None,
                CoreNetworkPolicy::Allowlist { .. } => nucleus_api::NetworkPolicy::Allowlist,
                CoreNetworkPolicy::Open => nucleus_api::NetworkPolicy::Open,
            })
            .unwrap_or(nucleus_api::NetworkPolicy::None),
        created_at,
        started_at: if state.started_at == 0 {
            None
        } else {
            Some(
                chrono::DateTime::from_timestamp(state.started_at as i64, 0)
                    .unwrap_or_else(chrono::Utc::now),
            )
        },
        stopped_at: None,
        exit_code: None,
        ttl_seconds: None,
        idle_timeout_seconds: None,
        labels: HashMap::new(),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /sandboxes — create a new sandbox.
async fn create_sandbox(
    State(state): State<AppState>,
    Json(spec): Json<SandboxSpec>,
) -> Result<Json<Sandbox>, StatusCode> {
    let mut config = spec_to_config(&spec);
    config.created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    create_container(&config).map_err(|e| {
        error!("failed to create container {}: {e:#}", config.name);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let sandbox = Sandbox {
        id: config.name.clone(),
        image: config.image.clone(),
        status: SandboxStatus::Stopped,
        pid: None,
        ip: None,
        memory: if config.memory.is_empty() {
            None
        } else {
            Some(config.memory.clone())
        },
        cpus: config.cpus,
        pids_limit: config.pids_limit,
        network: spec.network,
        created_at: chrono::DateTime::from_timestamp(config.created_at as i64, 0)
            .unwrap_or_else(chrono::Utc::now),
        started_at: None,
        stopped_at: None,
        exit_code: None,
        ttl_seconds: spec.ttl_seconds,
        idle_timeout_seconds: spec.idle_timeout_seconds,
        labels: spec.labels,
    };

    state.event_sender(&config.name);
    Ok(Json(sandbox))
}

/// GET /sandboxes — list all sandboxes.
async fn list_sandboxes(
    State(_state): State<AppState>,
) -> Result<Json<ListSandboxesResponse>, StatusCode> {
    let configs = list_configs().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut sandboxes = Vec::new();
    for config in &configs {
        let state = get_container_state(&config.name).unwrap_or(None);
        let sandbox = match state {
            Some(s) => state_to_sandbox(&s, Some(config)),
            None => Sandbox {
                id: config.name.clone(),
                image: config.image.clone(),
                status: SandboxStatus::Stopped,
                pid: None,
                ip: None,
                memory: if config.memory.is_empty() {
                    None
                } else {
                    Some(config.memory.clone())
                },
                cpus: config.cpus,
                pids_limit: config.pids_limit,
                network: NetworkPolicy::None,
                created_at: chrono::DateTime::from_timestamp(config.created_at as i64, 0)
                    .unwrap_or_else(chrono::Utc::now),
                started_at: None,
                stopped_at: None,
                exit_code: None,
                ttl_seconds: None,
                idle_timeout_seconds: None,
                labels: HashMap::new(),
            },
        };
        sandboxes.push(sandbox);
    }

    Ok(Json(ListSandboxesResponse { sandboxes }))
}

/// GET /sandboxes/:id — get a single sandbox.
async fn get_sandbox(
    Path(id): Path<String>,
    State(_state): State<AppState>,
) -> Result<Json<Sandbox>, StatusCode> {
    let config = get_container_config(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let state = get_container_state(&id).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let sandbox = match state {
        Some(s) => state_to_sandbox(&s, Some(&config)),
        None => Sandbox {
            id: config.name.clone(),
            image: config.image.clone(),
            status: SandboxStatus::Stopped,
            pid: None,
            ip: None,
            memory: if config.memory.is_empty() {
                None
            } else {
                Some(config.memory.clone())
            },
            cpus: config.cpus,
            pids_limit: config.pids_limit,
            network: NetworkPolicy::None,
            created_at: chrono::DateTime::from_timestamp(config.created_at as i64, 0)
                .unwrap_or_else(chrono::Utc::now),
            started_at: None,
            stopped_at: None,
            exit_code: None,
            ttl_seconds: None,
            idle_timeout_seconds: None,
            labels: HashMap::new(),
        },
    };

    Ok(Json(sandbox))
}

/// POST /sandboxes/:id/start — start a sandbox.
async fn start_sandbox(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<Sandbox>, StatusCode> {
    let ns_state = start_container(&id).map_err(|e| {
        error!("failed to start container {id}: {e:#}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let config = get_container_config(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    state
        .emit_event(
            &id,
            SandboxEvent::Started {
                sandbox_id: id.clone(),
            },
        )
        .await;

    let sandbox = state_to_sandbox(&ns_state, Some(&config));
    Ok(Json(sandbox))
}

/// POST /sandboxes/:id/stop — stop a sandbox.
async fn stop_sandbox(
    Path(id): Path<String>,
    State(_state): State<AppState>,
) -> Result<Json<Sandbox>, StatusCode> {
    let ns_state = get_container_state(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    stop_container(&ns_state, 10).map_err(|e| {
        error!("failed to stop container {id}: {e:#}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let config = get_container_config(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let mut stopped = ns_state.clone();
    stopped.status = "Stopped".to_string();
    Ok(Json(state_to_sandbox(&stopped, Some(&config))))
}

/// DELETE /sandboxes/:id — destroy a sandbox.
async fn destroy_sandbox(
    Path(id): Path<String>,
    State(_state): State<AppState>,
) -> Result<StatusCode, StatusCode> {
    destroy_container(&id).map_err(|e| {
        error!("failed to destroy container {id}: {e:#}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(StatusCode::NO_CONTENT)
}

/// POST /sandboxes/:id/exec — execute a command inside a sandbox.
async fn exec_in_sandbox(
    Path(id): Path<String>,
    State(_state): State<AppState>,
    Json(req): Json<ExecRequest>,
) -> Result<Json<ExecResult>, StatusCode> {
    // Verify the sandbox exists and is running.
    let _config = get_container_config(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let _state = get_container_state(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Use nucleus-core directly instead of shelling out to the CLI.
    let result = nucleus_core::exec_command(
        &id,
        &req.command,
        &req.env,
        req.workdir.as_deref(),
        req.timeout_seconds,
        req.output_cap_bytes,
    )
    .map_err(|e| {
        error!("failed to exec in {id}: {e:#}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(result))
}

/// POST /sandboxes/:id/exec/stream — stream command output via SSE.
async fn exec_stream_sandbox(
    Path(id): Path<String>,
    State(_state): State<AppState>,
    Json(req): Json<StreamExecRequest>,
) -> Result<Sse<impl Stream<Item = Result<axum::response::sse::Event, axum::Error>>>, StatusCode> {
    // Verify the sandbox exists and is running.
    let _config = get_container_config(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let _state = get_container_state(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Channel for streaming events from the blocking exec thread.
    let (std_tx, std_rx) = std::sync::mpsc::channel::<nucleus_api::ExecEvent>();

    // Bridge channel from std::sync::mpsc to tokio::sync::mpsc for async streaming.
    let (tokio_tx, mut tokio_rx) = tokio::sync::mpsc::channel::<nucleus_api::ExecEvent>(128);

    // Run the blocking exec in a spawn_blocking task.
    let exec_handle = tokio::task::spawn_blocking(move || {
        let error_tx = std_tx.clone();
        let result = nucleus_core::exec_command_stream(
            &id,
            &req.command,
            &req.env,
            req.workdir.as_deref(),
            req.timeout_seconds,
            req.output_cap_bytes,
            std_tx,
        );
        if let Err(e) = result {
            let _ = error_tx.send(nucleus_api::ExecEvent::Exit { code: 1 });
            error!("failed to stream exec in {id}: {e:#}");
        }
    });

    // Bridge task: forward events from std::sync::mpsc receiver to tokio mpsc sender.
    let _bridge_handle = tokio::task::spawn(async move {
        for event in std_rx {
            if tokio_tx.send(event).await.is_err() {
                break;
            }
        }
    });

    let stream = async_stream::stream! {
        // Forward events from the blocking task to the SSE stream.
        while let Some(event) = tokio_rx.recv().await {
            let json = serde_json::to_string(&event).unwrap_or_default();
            yield Ok(axum::response::sse::Event::default().data(json));
            if matches!(event, nucleus_api::ExecEvent::Exit { .. }) {
                break;
            }
        }
        // Wait for the blocking task to complete.
        let _ = exec_handle.await;
    };

    Ok(Sse::new(stream))
}

/// GET /sandboxes/:id/files — read a file or list a directory.
///
/// Query params:
/// - `path` — file or directory path inside the sandbox
/// - `list` — when "true", returns a directory listing instead of file content
async fn get_file(
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    State(_state): State<AppState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let path = params.get("path").cloned().unwrap_or_default();
    let list = params.get("list").map(|v| v == "true").unwrap_or(false);

    // Verify the sandbox exists.
    let _config = get_container_config(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    if list {
        let listing = nucleus_core::list_dir(&id, &path).map_err(|e| {
            error!("failed to list dir {path} in {id}: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        Ok(Json(serde_json::to_value(&listing).unwrap()))
    } else {
        let content = nucleus_core::read_file(&id, &path).map_err(|e| {
            error!("failed to read file {path} in {id}: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        Ok(Json(serde_json::to_value(&content).unwrap()))
    }
}

/// PUT /sandboxes/:id/files — write a file to the sandbox.
///
/// Query params:
/// - `path` — file path inside the sandbox
/// - `mode` — optional file mode (e.g. "0755")
async fn put_file(
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    State(_state): State<AppState>,
    Json(req): Json<nucleus_api::WriteFileRequest>,
) -> Result<StatusCode, StatusCode> {
    // Verify the sandbox exists.
    let _config = get_container_config(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let mode = params
        .get("mode")
        .and_then(|m| u32::from_str_radix(m, 8).ok());

    nucleus_core::write_file(&id, &req.path, &req.content, mode).map_err(|e| {
        error!("failed to write file {} in {id}: {e:#}", req.path);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(StatusCode::NO_CONTENT)
}

/// POST /sandboxes/:id/snapshot — create a snapshot of the sandbox overlay.
///
/// Query params:
/// - `name` — optional snapshot name (defaults to auto-generated ID)
async fn snapshot_sandbox(
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    State(_state): State<AppState>,
    Json(req): Json<nucleus_api::SnapshotRequest>,
) -> Result<Json<nucleus_api::SnapshotResponse>, StatusCode> {
    let snapshot_name = req.name.or_else(|| params.get("name").cloned());

    nucleus_core::snapshot_container(&id, snapshot_name.as_deref())
        .map(Json)
        .map_err(|e| {
            error!("failed to snapshot {id}: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// POST /sandboxes/:id/reset — restore a sandbox from a snapshot.
///
/// Query params:
/// - `name` — snapshot name to restore (required)
async fn reset_sandbox(
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    State(_state): State<AppState>,
) -> Result<Json<nucleus_api::SnapshotResponse>, StatusCode> {
    let snapshot_name = params.get("name").cloned().ok_or_else(|| {
        error!("reset {id} requires ?name=<snapshot>");
        StatusCode::BAD_REQUEST
    })?;

    nucleus_core::reset_container(&id, &snapshot_name)
        .map(Json)
        .map_err(|e| {
            error!("failed to reset {id} from snapshot {snapshot_name}: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// GET /sandboxes/:id/events — Server-Sent Events stream for sandbox events.
async fn sandbox_events(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<axum::response::sse::Event, axum::Error>>> {
    let rx = state.event_sender(&id).subscribe();
    let stream = async_stream::stream! {
        let mut rx = rx;
        while let Ok(event) = rx.recv().await {
            let json = serde_json::to_string(&event).unwrap_or_default();
            yield Ok(axum::response::sse::Event::default().data(json));
        }
    };

    Sse::new(stream)
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let state = AppState::new();

    let app = Router::new()
        .route("/sandboxes", post(create_sandbox).get(list_sandboxes))
        .route("/sandboxes/:id", get(get_sandbox).delete(destroy_sandbox))
        .route("/sandboxes/:id/start", post(start_sandbox))
        .route("/sandboxes/:id/stop", post(stop_sandbox))
        .route("/sandboxes/:id/exec", post(exec_in_sandbox))
        .route("/sandboxes/:id/exec/stream", post(exec_stream_sandbox))
        .route("/sandboxes/:id/files", get(get_file).put(put_file))
        .route("/sandboxes/:id/snapshot", post(snapshot_sandbox))
        .route("/sandboxes/:id/reset", post(reset_sandbox))
        .route("/sandboxes/:id/events", get(sandbox_events))
        .with_state(state);

    // Remove stale socket.
    let _ = std::fs::remove_file(SOCKET_PATH);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    info!("sandboxd listening on {addr} (TCP localhost)");

    axum::serve(listener, app)
        .await
        .map_err(|e| anyhow::anyhow!("server error: {e:#}"))?;
    Ok(())
}
