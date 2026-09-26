mod args;
mod container;
mod doctor;
mod image;
mod net;
mod orchestrator;
#[cfg(test)]
mod properties;
mod state;
mod stats;
mod utils;

use crate::args::{Commands, NucleusArgs};
use crate::orchestrator::TeardownSpec;
use crate::state::ContainerState;
use crate::utils::{get_nucleus_log_dir, validate_container_name};
use anyhow::{Context, Result, bail};
use clap::Parser;
use nix::unistd::getuid;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() -> Result<()> {
    let args = NucleusArgs::parse();

    match args.command {
        Some(Commands::InternalChild(run_args)) => {
            // Never returns on success; execs the container process.
            return container::run_container_child(run_args);
        }
        Some(Commands::InternalReaper { name, pid, spec }) => {
            return orchestrator::run_reaper(&name, pid, &spec);
        }
        Some(Commands::Run(run_args)) => {
            // Validate every flag before reporting a privilege problem, so a
            // typo is reported as a typo regardless of the caller's uid.
            let parsed = orchestrator::validate_run_args(&run_args)?;

            if !getuid().is_root() && !run_args.rootless {
                bail!(
                    "Nucleus must be run as root to manage namespaces and networking. Use --rootless for unprivileged isolation."
                );
            }
            orchestrator::run_parent_orchestrator(run_args, parsed)?;
        }
        Some(Commands::Exec {
            name,
            interactive,
            tty,
            env,
            workdir,
            command,
        }) => {
            exec_in_container(
                &name,
                &interactive,
                &tty,
                &env,
                workdir.as_deref(),
                &command,
            )?;
        }
        Some(Commands::Inspect { name }) => {
            inspect_container(&name)?;
        }
        Some(Commands::List) | Some(Commands::Ps) => {
            list_containers()?;
        }
        Some(Commands::Images) => {
            list_images()?;
        }
        Some(Commands::Info { json }) => {
            let report = doctor::collect();
            if json {
                let payload = serde_json::json!({
                    "privileged_available": report.privileged_ok,
                    "rootless_available": report.rootless_ok,
                    "probes": report.probes.iter().map(|p| serde_json::json!({
                        "name": p.name,
                        "status": format!("{:?}", p.status).to_uppercase(),
                        "detail": p.detail,
                    })).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else {
                println!("{report}");
            }
            // Non-zero when neither mode can work, so this is usable in a
            // provisioning script or a container-entrypoint health check.
            if !report.privileged_ok && !report.rootless_ok {
                std::process::exit(1);
            }
        }
        Some(Commands::FlushFirewall) => {
            if !getuid().is_root() {
                bail!("'nucleus flush-firewall' must be run as root");
            }
            net::flush_chains()?;
            println!(
                "[Nucleus] Flushed rules from: {}",
                net::OWNED_CHAINS
                    .iter()
                    .map(|(table, chain)| format!("{table}/{chain}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        Some(Commands::Rmi { image }) => {
            image::remove_image(&image)?;
        }
        Some(Commands::Rm { name, force }) => {
            remove_container(&name, force)?;
        }
        Some(Commands::Logs { name, follow }) => {
            show_logs(&name, follow)?;
        }
        Some(Commands::Stop { name, timeout }) => {
            stop_container(&name, timeout)?;
        }
        Some(Commands::Stats { name, stream }) => {
            stats::display_stats(&name, stream)?;
        }
        Some(Commands::Pull { distro, force }) => {
            image::pull_image_with(&distro, force)?;
        }
        None => {
            println!("Use 'nucleus --help' for usage information.");
        }
    }

    Ok(())
}

/// Runs a command inside a running container's namespaces.
fn exec_in_container(
    name: &str,
    interactive: &bool,
    tty: &bool,
    env: &[String],
    workdir: Option<&str>,
    command: &[String],
) -> Result<()> {
    validate_container_name(name)?;
    let state = require_running_container(name)?;

    // Entering the mount namespace puts the container's rootfs at /, so the
    // command resolves against the container filesystem.
    let mut nsenter_args: Vec<String> = vec![
        "-t".to_string(),
        state.pid.to_string(),
        "--mount".to_string(),
        "--uts".to_string(),
        "--ipc".to_string(),
        "--net".to_string(),
        "--pid".to_string(),
        // Required for --pid to take effect for the exec'd process.
        "--fork".to_string(),
    ];

    if let Some(dir) = workdir {
        let normalized = utils::normalize_container_path(dir)
            .with_context(|| format!("Invalid working directory '{dir}'"))?;
        nsenter_args.push(format!("--wd=/{normalized}"));
    }
    nsenter_args.push("--".to_string());
    nsenter_args.extend_from_slice(command);

    let mut cmd = Command::new("nsenter");
    cmd.args(&nsenter_args);
    // nsenter execs the target, so it inherits the environment we set here.
    for assignment in env {
        let (key, value) = utils::parse_env_assignment(assignment)?;
        cmd.env(key, value);
    }
    if !interactive && !tty {
        cmd.stdin(std::process::Stdio::null());
    }

    let status = cmd.status().with_context(|| {
        format!(
            "Failed to execute '{}' in container '{name}'",
            command.join(" ")
        )
    })?;

    if !status.success() {
        // Propagate the container command's exit code.
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn inspect_container(name: &str) -> Result<()> {
    validate_container_name(name)?;
    let state = require_running_container(name)?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let uptime = state.started_at.checked_sub(0).map(|started| {
        if started == 0 {
            "unknown".to_string()
        } else {
            format!("{}s", now.saturating_sub(started))
        }
    });

    let mut value = serde_json::to_value(&state).context("Failed to render container state")?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "uptime".to_string(),
            serde_json::Value::String(uptime.unwrap_or_else(|| "unknown".to_string())),
        );
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&value).context("Failed to render container state")?
    );
    Ok(())
}

fn list_containers() -> Result<()> {
    // Reclaim state files for containers that are no longer running.
    let pruned = state::prune_stale_state().unwrap_or(0);
    if pruned > 0 {
        println!("[Nucleus] Reclaimed {pruned} stale container record(s).");
    }

    let containers = state::list_containers()?;
    if containers.is_empty() {
        println!("[Nucleus] No containers running.");
        return Ok(());
    }

    println!(
        "{:<20} {:<10} {:<15} {:<10} {:<12} {:<20} COMMAND",
        "NAME", "PID", "IP", "STATUS", "PORTS", "IMAGE"
    );
    println!("{:-<115}", "-");
    for c in containers {
        println!(
            "{:<20} {:<10} {:<15} {:<10} {:<12} {:<20} {}",
            c.name,
            c.pid,
            c.ip,
            c.status,
            truncate(&c.port_summary(), 12),
            truncate(&c.image, 20),
            c.command_summary()
        );
    }
    Ok(())
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let truncated: String = value.chars().take(max.saturating_sub(3)).collect();
    format!("{truncated}...")
}

fn list_images() -> Result<()> {
    let images = image::list_images()?;
    if images.is_empty() {
        println!("[Nucleus] No local images. Run 'nucleus pull <distro>' to add one.");
        return Ok(());
    }

    println!("{:<20} {:<12} PATH", "IMAGE", "SIZE");
    println!("{:-<70}", "-");
    for (name, path) in images {
        let size = dir_size(&path);
        println!("{:<20} {:<12} {}", name, size, path.display());
    }
    Ok(())
}

/// Total size of a directory tree, for display only.
fn dir_size(path: &std::path::Path) -> String {
    let mut total: u64 = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    utils::format_bytes(total)
}

fn remove_container(name: &str, force: bool) -> Result<()> {
    validate_container_name(name)?;
    let state = state::get_container_state(name)?;

    match state {
        Some(state) => {
            if !force {
                bail!("Container '{name}' is running. Stop it first, or use --force.");
            }
            println!("[Nucleus] Removing running container '{name}'...");
            orchestrator::stop_container(&state, 10)?;
        }
        None => {
            // Not running: still clean up any leftover host resources.
            let spec = TeardownSpec {
                name: name.to_string(),
                veth_host: String::new(),
                container_ip: String::new(),
                ports: vec![],
                cgroup_path: state::cgroup_dir_for(name),
                rootless: false,
            };
            orchestrator::teardown_container(&spec, true)?;
        }
    }

    println!("[Nucleus] Removed container '{name}'.");
    Ok(())
}

fn show_logs(name: &str, follow: bool) -> Result<()> {
    validate_container_name(name)?;

    let log_dirs = [
        get_nucleus_log_dir(),
        std::path::PathBuf::from("/tmp/nucleus/logs"),
    ];
    let log_path = log_dirs
        .iter()
        .map(|dir| dir.join(format!("{name}.log")))
        .find(|p| p.exists());

    let Some(log_path) = log_path else {
        println!("[Nucleus] No logs found for container '{name}'.");
        return Ok(());
    };

    if follow {
        let path = log_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Log path is not valid UTF-8"))?;
        Command::new("tail")
            .args(["-f", path])
            .status()
            .context("Failed to tail log file")?;
    } else {
        let content = std::fs::read_to_string(&log_path).context("Failed to read log file")?;
        print!("{content}");
    }
    Ok(())
}

fn stop_container(name: &str, timeout: u64) -> Result<()> {
    validate_container_name(name)?;
    let state = require_running_container(name)?;
    orchestrator::stop_container(&state, timeout)
}

/// Looks up a running container or exits with a clear message.
fn require_running_container(name: &str) -> Result<ContainerState> {
    state::get_container_state(name)?
        .ok_or_else(|| anyhow::anyhow!("Container '{name}' not found or not running."))
}
