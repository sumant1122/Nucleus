//! # Nucleus Core
//!
//! `nucleus-core` is the library crate behind the Nucleus container engine.
//! It exposes the sandbox runtime as typed functions so other projects —
//! such as the `nucleusd` agent sandbox service — can consume it without
//! parsing CLI output.
//!
//! # Safety
//!
//! Creating sandboxes requires root or rootless user namespaces. Many
//! functions here mutate host state (namespaces, cgroups, iptables, mount
//! namespaces) and should only be called from a trusted orchestrator.

pub mod args;
pub mod cli;
pub mod container;
pub mod doctor;
pub mod image;
pub mod net;
pub mod orchestrator;
pub mod state;
pub mod stats;
pub mod utils;

// Public re-exports of the types most external callers need.
pub use args::{Commands, RunArgs};
pub use container::{run_container_child, setup_controlling_terminal};
pub use doctor::{Report, collect as collect_host_report};
pub use image::{
    DISTROS, Distro, find_distro, list_images, pull_image_with, resolve_image_path,
    supported_distros,
};
pub use net::{
    DEFAULT_SUBNET, Ipv4Net, PortMapping, parse_port_mapping, publish_port, unpublish_port,
};
pub use orchestrator::{
    ParsedRun, TeardownSpec, create_container, destroy_container, exec_command,
    exec_command_stream, list_dir, read_file, reset_container, snapshot_container, start_container,
    stop_container, validate_run_args, write_file,
};
pub use state::{
    ContainerConfig, ContainerState, NetworkPolicy, cgroup_dir_for, get_config_dir,
    get_container_config, get_container_state, get_state_dir, is_container_alive, list_configs,
    list_containers, prune_stale_state, remove_config, remove_state, save_config, save_state,
};
pub use stats::ContainerStats;
pub use utils::{
    MemoryLimit, Volume, format_bytes, generate_veth_names, get_nucleus_data_dir,
    get_nucleus_log_dir, get_nucleus_runtime_dir, normalize_container_path, parse_env_assignment,
    parse_volume, validate_container_name,
};
