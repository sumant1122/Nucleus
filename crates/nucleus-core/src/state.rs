use crate::net::PortMapping;
use crate::utils::get_nucleus_runtime_dir;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Persisted description of a running container.
///
/// Fields added after 0.2.0 are `#[serde(default)]` so state files written by
/// older builds still load.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ContainerState {
    pub name: String,
    pub pid: i32,
    pub ip: String,
    /// Bridge gateway address. Absent in 0.2.0 state files.
    #[serde(default)]
    pub gateway: String,
    /// Subnet in CIDR form, e.g. `10.0.0.0/24`. Absent in 0.2.0 state files.
    #[serde(default)]
    pub subnet: String,
    /// Host bridge interface.
    pub network: String,
    pub veth_host: String,
    pub status: String,

    #[serde(default)]
    pub ports: Vec<PortMapping>,
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub volumes: Vec<String>,
    #[serde(default)]
    pub memory: String,
    #[serde(default)]
    pub cpus: Option<f64>,
    #[serde(default)]
    pub pids_limit: Option<u32>,
    #[serde(default)]
    pub rootless: bool,
    #[serde(default)]
    pub readonly: bool,
    /// Host-visible PID of the container's PID 1, when it could be determined.
    /// Needed to signal the container's own init rather than the supervisor.
    #[serde(default)]
    pub init_pid: Option<i32>,
    /// Absolute cgroup directory, so teardown and stats agree on the path.
    #[serde(default)]
    pub cgroup_path: String,
    /// Unix timestamp of container start.
    #[serde(default)]
    pub started_at: u64,
}

impl ContainerState {
    /// The cgroup directory for this container.
    pub fn cgroup_dir(&self) -> String {
        if self.cgroup_path.is_empty() {
            // Fall back to the historical layout for state written by 0.2.0.
            format!("/sys/fs/cgroup/{}", self.name)
        } else {
            self.cgroup_path.clone()
        }
    }

    /// Comma-separated published ports, or `-` when none are published.
    pub fn port_summary(&self) -> String {
        if self.ports.is_empty() {
            "-".to_string()
        } else {
            self.ports
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(",")
        }
    }

    /// The container command as a single shell-like string.
    pub fn command_summary(&self) -> String {
        if self.command.is_empty() {
            "-".to_string()
        } else {
            self.command.join(" ")
        }
    }
}

/// Root of the cgroup subtree owned by Nucleus.
pub const CGROUP_ROOT: &str = "/sys/fs/cgroup/nucleus";

/// Absolute cgroup path for a container.
pub fn cgroup_dir_for(name: &str) -> String {
    format!("{CGROUP_ROOT}/{name}")
}

pub fn get_state_dir() -> PathBuf {
    get_nucleus_runtime_dir().join("state")
}

fn state_path(name: &str) -> PathBuf {
    get_state_dir().join(format!("{name}.json"))
}

/// Writes container state atomically so a crash cannot leave a truncated file.
pub fn save_state(state: &ContainerState) -> Result<()> {
    let state_dir = get_state_dir();
    fs::create_dir_all(&state_dir).context("Failed to create state directory")?;

    let final_path = state_path(&state.name);
    let tmp_path = final_path.with_extension("json.tmp");

    let json =
        serde_json::to_string_pretty(state).context("Failed to serialize container state")?;
    fs::write(&tmp_path, json).context("Failed to write container state file")?;
    fs::rename(&tmp_path, &final_path).context("Failed to commit container state file")?;
    Ok(())
}

pub fn remove_state(name: &str) -> Result<()> {
    // Check primary runtime state dir
    let state_path = state_path(name);
    if state_path.exists() {
        let _ = fs::remove_file(state_path);
    }
    // Also cleanup legacy /tmp/nucleus/state if present
    let legacy_path = PathBuf::from("/tmp/nucleus/state").join(format!("{name}.json"));
    if legacy_path.exists() {
        let _ = fs::remove_file(legacy_path);
    }
    Ok(())
}

pub fn get_container_state(name: &str) -> Result<Option<ContainerState>> {
    Ok(list_containers()?.into_iter().find(|c| c.name == name))
}

/// Returns true when `pid` is still the Nucleus container supervisor for `name`.
///
/// A bare `/proc/<pid>` existence check is not enough: PIDs get recycled, which
/// would make a dead container look permanently alive. We additionally confirm
/// the process is our internal child by inspecting its command line.
pub fn is_container_alive(state: &ContainerState) -> bool {
    if state.pid <= 0 {
        return false;
    }
    let cmdline = PathBuf::from(format!("/proc/{}/cmdline", state.pid));
    let Ok(raw) = fs::read(&cmdline) else {
        return false;
    };
    let cmd = String::from_utf8_lossy(&raw);
    cmd.contains("internal-child") && cmd.contains(&state.name)
}

/// Lists containers whose supervisor process is still alive.
///
/// State files are not modified here; use [`prune_stale_state`] to reclaim
/// them. If a name appears in both the current and legacy state directories,
/// the current one wins.
pub fn list_containers() -> Result<Vec<ContainerState>> {
    let state_dirs = vec![get_state_dir(), PathBuf::from("/tmp/nucleus/state")];

    let mut containers = vec![];
    let mut seen_names = std::collections::HashSet::new();

    for state_dir in state_dirs {
        let Ok(entries) = fs::read_dir(&state_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(state) = serde_json::from_str::<ContainerState>(&content) else {
                continue;
            };
            if seen_names.insert(state.name.clone()) && is_container_alive(&state) {
                containers.push(state);
            }
        }
    }
    Ok(containers)
}

/// Deletes state files whose container process is no longer running.
pub fn prune_stale_state() -> Result<usize> {
    let state_dirs = vec![get_state_dir(), PathBuf::from("/tmp/nucleus/state")];
    let mut removed = 0;

    for state_dir in state_dirs {
        let Ok(entries) = fs::read_dir(&state_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(state) = serde_json::from_str::<ContainerState>(&content) else {
                // Unparseable state is stale by definition.
                if fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
                continue;
            };
            if !is_container_alive(&state) && fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ContainerState {
        ContainerState {
            name: "web".to_string(),
            pid: 1234,
            ip: "10.0.0.5".to_string(),
            gateway: "10.0.0.1".to_string(),
            subnet: "10.0.0.0/24".to_string(),
            network: "br0".to_string(),
            veth_host: "vh-web-abcde".to_string(),
            status: "Running".to_string(),
            ports: vec![PortMapping {
                host_ip: None,
                host_port: 8080,
                container_port: 80,
                protocol: "tcp".to_string(),
            }],
            image: "alpine".to_string(),
            command: vec!["/bin/sh".to_string()],
            volumes: vec![],
            memory: "1G".to_string(),
            cpus: Some(1.0),
            pids_limit: Some(100),
            rootless: false,
            readonly: false,
            init_pid: Some(1235),
            cgroup_path: cgroup_dir_for("web"),
            started_at: 1_700_000_000,
        }
    }

    #[test]
    fn test_state_serde_roundtrip() {
        let state = sample();
        let json = serde_json::to_string(&state).unwrap();
        let parsed: ContainerState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, state.name);
        assert_eq!(parsed.ports, state.ports);
        assert_eq!(parsed.cgroup_dir(), state.cgroup_dir());
    }

    #[test]
    fn test_state_deserializes_legacy_v020_files() {
        // A 0.2.0 state file: only the original six fields were present.
        let legacy = r#"{
            "name": "old",
            "pid": 42,
            "ip": "10.0.0.9",
            "network": "br0",
            "veth_host": "vh-old-1234",
            "status": "Running"
        }"#;
        let parsed: ContainerState = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.name, "old");
        assert!(parsed.ports.is_empty());
        assert!(parsed.command.is_empty());
        assert!(!parsed.rootless);
        // New fields fall back to the historical cgroup layout.
        assert_eq!(parsed.cgroup_dir(), "/sys/fs/cgroup/old");
        assert_eq!(parsed.gateway, "");
        assert_eq!(parsed.subnet, "");
    }

    #[test]
    fn test_cgroup_dir_for_is_namespaced() {
        assert_eq!(cgroup_dir_for("web"), "/sys/fs/cgroup/nucleus/web");
    }

    #[test]
    fn test_port_and_command_summaries() {
        let state = sample();
        assert_eq!(state.port_summary(), "8080:80/tcp");

        let mut none = state.clone();
        none.ports.clear();
        assert_eq!(none.port_summary(), "-");

        assert_eq!(state.command_summary(), "/bin/sh");
        let mut nocmd = state;
        nocmd.command.clear();
        assert_eq!(nocmd.command_summary(), "-");
    }

    #[test]
    fn test_is_container_alive_rejects_impossible_pid() {
        let mut state = sample();
        state.pid = 0;
        assert!(!is_container_alive(&state));
        state.pid = -1;
        assert!(!is_container_alive(&state));
        // PID 1 is init, never a Nucleus container.
        state.pid = 1;
        assert!(!is_container_alive(&state));
    }

    #[test]
    fn test_is_container_alive_detects_recycled_pid() {
        // Use this test binary's own PID, which is not an internal-child.
        let mut state = sample();
        state.pid = std::process::id() as i32;
        assert!(!is_container_alive(&state));
    }
}
