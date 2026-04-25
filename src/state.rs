use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Debug)]
pub struct ContainerState {
    pub name: String,
    pub pid: u32,
    pub ip: String,
    pub network: String,
    pub veth_host: String,
    pub status: String,
}

const STATE_DIR: &str = "/tmp/nucleus/state";

fn get_state_dir() -> PathBuf {
    PathBuf::from(STATE_DIR)
}

pub fn save_state(state: &ContainerState) -> Result<()> {
    let state_dir = get_state_dir();
    if !state_dir.exists() {
        fs::create_dir_all(&state_dir).context("Failed to create state directory")?;
    }
    let state_path = state_dir.join(format!("{}.json", state.name));
    let json = serde_json::to_string_pretty(state)
        .context("Failed to serialize container state")?;
    fs::write(state_path, json).context("Failed to write container state file")?;
    Ok(())
}

pub fn remove_state(name: &str) -> Result<()> {
    let state_path = get_state_dir().join(format!("{}.json", name));
    if state_path.exists() {
        fs::remove_file(state_path).context("Failed to remove container state file")?;
    }
    Ok(())
}

pub fn list_containers() -> Result<Vec<ContainerState>> {
    let state_dir = get_state_dir();
    if !state_dir.exists() {
        return Ok(vec![]);
    }

    let mut containers = vec![];
    for entry in fs::read_dir(state_dir).context("Failed to read state directory")? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().map_or(false, |ext| ext == "json") {
            let content = fs::read_to_string(&path).context("Failed to read state file")?;
            let state: ContainerState = serde_json::from_str(&content)
                .context(format!("Failed to parse state file: {:?}", path))?;
            
            // Basic liveness check: check if PID still exists
            if Path::new(&format!("/proc/{}", state.pid)).exists() {
                containers.push(state);
            } else {
                // Cleanup stale state
                let _ = fs::remove_file(path);
            }
        }
    }
    Ok(containers)
}
