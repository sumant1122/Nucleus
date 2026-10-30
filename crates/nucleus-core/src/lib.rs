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
pub use doctor::{collect as collect_host_report, Report};
pub use image::{Distro, DISTROS, find_distro, list_images, pull_image_with, resolve_image_path, supported_distros};
pub use net::{Ipv4Net, PortMapping, parse_port_mapping, publish_port, unpublish_port, DEFAULT_SUBNET};
pub use orchestrator::{ParsedRun, TeardownSpec, validate_run_args};
pub use state::{
    ContainerState, cgroup_dir_for, get_container_state, get_state_dir,
    is_container_alive, list_containers, prune_stale_state, remove_state,
    save_state,
};
pub use stats::ContainerStats;
pub use utils::{
    format_bytes, generate_veth_names, get_nucleus_data_dir,
    get_nucleus_log_dir, get_nucleus_runtime_dir, MemoryLimit, Volume,
    normalize_container_path, parse_env_assignment, parse_volume,
    validate_container_name,
};
