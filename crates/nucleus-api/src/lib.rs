//! Shared API types for the Nucleus agent sandbox.
//!
//! This crate defines the request/response types, `SandboxSpec`, and event
//! shapes used by the `sandboxd` service, SDKs, and MCP server. It has no
//! dependency on `nucleus-core` so it can be imported by unprivileged client
//! code without pulling in the privileged engine.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A sandbox specification describing how a sandbox should be created.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxSpec {
    pub id: Option<String>,
    pub image: String,
    pub command: Vec<String>,
    pub env: Vec<String>,
    pub workdir: Option<String>,
    pub memory: Option<String>,
    pub cpus: Option<f64>,
    pub pids_limit: Option<u32>,
    pub rootless: bool,
    pub readonly: bool,
    pub network: NetworkPolicy,
    pub ttl_seconds: Option<u64>,
    pub idle_timeout_seconds: Option<u64>,
    pub labels: std::collections::HashMap<String, String>,
}

/// Network access policy for a sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkPolicy {
    /// No network access except loopback.
    None,
    /// Allowlisted outbound destinations only.
    Allowlist,
    /// Full NAT/egress (dangerous, not recommended for untrusted code).
    Open,
}

/// Current status of a sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SandboxStatus {
    Creating,
    Running,
    Paused,
    Stopped,
    Error,
    Destroyed,
}

/// Persistent sandbox record returned by the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sandbox {
    pub id: String,
    pub image: String,
    pub status: SandboxStatus,
    pub pid: Option<i32>,
    pub ip: Option<String>,
    pub memory: Option<String>,
    pub cpus: Option<f64>,
    pub pids_limit: Option<u32>,
    pub network: NetworkPolicy,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub ttl_seconds: Option<u64>,
    pub idle_timeout_seconds: Option<u64>,
    pub labels: std::collections::HashMap<String, String>,
}

/// Request to execute a command inside a sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRequest {
    pub command: Vec<String>,
    pub env: Vec<String>,
    pub workdir: Option<String>,
    /// Per-command timeout in seconds.
    pub timeout_seconds: Option<u64>,
    /// Maximum output bytes before truncation.
    pub output_cap_bytes: Option<usize>,
    /// Optional stdin to feed to the process.
    pub stdin: Option<String>,
}

/// Result of an exec call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub truncated: bool,
}

/// Events emitted during streaming exec.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ExecEvent {
    /// A chunk of stdout output.
    Stdout(String),
    /// A chunk of stderr output.
    Stderr(String),
    /// The command has finished with the given exit code.
    Exit { code: i32 },
}

/// Request to stream-exec a command inside a sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamExecRequest {
    pub command: Vec<String>,
    pub env: Vec<String>,
    pub workdir: Option<String>,
    /// Per-command timeout in seconds.
    pub timeout_seconds: Option<u64>,
    /// Maximum total output bytes before truncation.
    pub output_cap_bytes: Option<usize>,
}

/// File read request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadFileRequest {
    pub path: String,
}

/// File write request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteFileRequest {
    pub path: String,
    pub content: Vec<u8>,
    pub mode: Option<u32>,
}

/// File list entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
}

/// Sandbox event emitted to the event stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum SandboxEvent {
    Created { sandbox_id: String },
    Started { sandbox_id: String },
    Stopped { sandbox_id: String },
    Destroyed { sandbox_id: String },
    Paused { sandbox_id: String },
    Resumed { sandbox_id: String },
    Error { sandbox_id: String, message: String },
    CommandStarted { sandbox_id: String, command: Vec<String> },
    CommandFinished {
        sandbox_id: String,
        exit_code: i32,
        duration_ms: u64,
    },
    NetworkDenied { sandbox_id: String, destination: String },
}

/// Response to listing sandboxes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListSandboxesResponse {
    pub sandboxes: Vec<Sandbox>,
}

/// Create a new sandbox ID.
pub fn new_sandbox_id() -> String {
    Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_sandbox_id_is_uuid() {
        let id = new_sandbox_id();
        assert!(Uuid::parse_str(&id).is_ok());
    }

    #[test]
    fn test_sandbox_spec_serialization() {
        let spec = SandboxSpec {
            id: None,
            image: "alpine".to_string(),
            command: vec!["/bin/sh".to_string()],
            env: vec![],
            workdir: None,
            memory: Some("512M".to_string()),
            cpus: Some(1.0),
            pids_limit: Some(100),
            rootless: false,
            readonly: false,
            network: NetworkPolicy::None,
            ttl_seconds: Some(3600),
            idle_timeout_seconds: Some(300),
            labels: std::collections::HashMap::new(),
        };
        let json = serde_json::to_string(&spec).unwrap();
        let parsed: SandboxSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.image, spec.image);
        assert_eq!(parsed.memory, spec.memory);
    }
}
