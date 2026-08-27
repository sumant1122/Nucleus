use crate::utils::get_nucleus_runtime_dir;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ContainerState {
    pub name: String,
    pub pid: u32,
    pub ip: String,
    pub network: String,
    pub veth_host: String,
    pub status: String,
}

pub fn get_state_dir() -> PathBuf {
    get_nucleus_runtime_dir().join("state")
}

pub fn save_state(state: &ContainerState) -> Result<()> {
    let state_dir = get_state_dir();
    if !state_dir.exists() {
        fs::create_dir_all(&state_dir).context("Failed to create state directory")?;
    }
    let state_path = state_dir.join(format!("{}.json", state.name));
    let json =
        serde_json::to_string_pretty(state).context("Failed to serialize container state")?;
    fs::write(state_path, json).context("Failed to write container state file")?;
    Ok(())
}

pub fn remove_state(name: &str) -> Result<()> {
    // Check primary runtime state dir
    let state_path = get_state_dir().join(format!("{}.json", name));
    if state_path.exists() {
        let _ = fs::remove_file(state_path);
    }
    // Also cleanup legacy /tmp/nucleus/state if present
    let legacy_path = PathBuf::from("/tmp/nucleus/state").join(format!("{}.json", name));
    if legacy_path.exists() {
        let _ = fs::remove_file(legacy_path);
    }
    Ok(())
}

pub fn get_container_state(name: &str) -> Result<Option<ContainerState>> {
    let containers = list_containers()?;
    Ok(containers.into_iter().find(|c| c.name == name))
}

pub fn list_containers() -> Result<Vec<ContainerState>> {
    let state_dirs = vec![
        get_state_dir(),
        PathBuf::from("/tmp/nucleus/state"),
    ];

    let mut containers = vec![];
    let mut seen_names = std::collections::HashSet::new();

    for state_dir in state_dirs {
        if !state_dir.exists() {
            continue;
        }

        if let Ok(entries) = fs::read_dir(&state_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(false, |ext| ext == "json") {
                    if let Ok(content) = fs::read_to_string(&path) {
                        if let Ok(state) = serde_json::from_str::<ContainerState>(&content) {
                            if seen_names.insert(state.name.clone()) {
                                // Liveness check: check if PID still exists
                                if Path::new(&format!("/proc/{}", state.pid)).exists() {
                                    containers.push(state);
                                } else {
                                    // Cleanup stale state
                                    let _ = fs::remove_file(&path);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(containers)
}
