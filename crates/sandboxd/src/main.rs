//! # sandboxd
//!
//! Nucleus agent sandbox service: a privileged local daemon that manages
//! persistent sandboxes for AI agents. Exposes an HTTP/JSON API over a Unix
//! socket, plus streaming exec, file API, snapshots, and policy enforcement.

use anyhow::Result;
use nucleus_api::{new_sandbox_id, SandboxEvent, SandboxSpec};
use nucleus_core::utils::validate_container_name;
use std::collections::HashMap;
use tracing::{info, warn};

/// In-memory sandbox registry (MVP). Later this will be persisted to disk
/// and backed by a real store.
#[derive(Default)]
pub struct SandboxRegistry {
    sandboxes: HashMap<String, SandboxSpec>,
}

impl SandboxRegistry {
    pub fn create(&mut self, mut spec: SandboxSpec) -> String {
        let id = spec.id.take().unwrap_or_else(new_sandbox_id);
        self.sandboxes.insert(id.clone(), spec);
        id
    }

    pub fn get(&self, id: &str) -> Option<&SandboxSpec> {
        self.sandboxes.get(id)
    }

    pub fn remove(&mut self, id: &str) -> Option<SandboxSpec> {
        self.sandboxes.remove(id)
    }

    pub fn list(&self) -> Vec<&SandboxSpec> {
        self.sandboxes.values().collect()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let socket_path = "/run/nucleus/sandboxd.sock";
    info!("Starting sandboxd on {}", socket_path);

    let _registry = SandboxRegistry::default();

    // Placeholder: the real HTTP server + Unix socket listener will be added
    // in the next phase along with the exec and file APIs.
    warn!("sandboxd is a stub: no API endpoints are served yet");
    info!("sandboxd initialized successfully");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_create_and_get() {
        let mut reg = SandboxRegistry::default();
        let spec = SandboxSpec {
            id: None,
            image: "alpine".to_string(),
            command: vec!["/bin/sh".to_string()],
            env: vec![],
            workdir: None,
            memory: None,
            cpus: None,
            pids_limit: None,
            rootless: false,
            readonly: false,
            network: nucleus_api::NetworkPolicy::None,
            ttl_seconds: None,
            idle_timeout_seconds: None,
            labels: HashMap::new(),
        };
        let id = reg.create(spec);
        validate_container_name(&id).unwrap();
        assert!(reg.get(&id).is_some());
        assert!(reg.remove(&id).is_some());
        assert!(reg.get(&id).is_none());
    }

    #[test]
    fn test_sandbox_event_serialization() {
        let event = SandboxEvent::Created {
            sandbox_id: "sb-1".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("sb-1"));
        assert!(json.contains("Created"));
    }
}
