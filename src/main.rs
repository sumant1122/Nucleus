mod args;
mod container;
mod image;
mod orchestrator;
mod state;
mod stats;
mod utils;

use crate::args::{Commands, NucleusArgs};
use crate::utils::get_nucleus_log_dir;
use anyhow::{Context, Result};
use clap::Parser;
use nix::sys::signal::{self, Signal};
use nix::unistd::{Pid, getuid};

fn main() -> Result<()> {
    let args = NucleusArgs::parse();

    // Command Dispatch
    match args.command {
        Some(Commands::InternalChild(run_args)) => {
            return container::run_container_child(run_args);
        }
        Some(Commands::Run(run_args)) => {
            if !getuid().is_root() && !run_args.rootless {
                return Err(anyhow::anyhow!(
                    "Nucleus must be run as root to manage namespaces and networking. Use --rootless for unprivileged isolation."
                ));
            }
            orchestrator::run_parent_orchestrator(run_args)?;
        }
        Some(Commands::List) => {
            let containers = state::list_containers()?;
            if containers.is_empty() {
                println!("[Nucleus] No containers running.");
            } else {
                println!("{:<20} {:<10} {:<15} {:<10}", "NAME", "PID", "IP", "STATUS");
                println!("{:-<55}", "");
                for c in containers {
                    println!("{:<20} {:<10} {:<15} {:<10}", c.name, c.pid, c.ip, c.status);
                }
            }
        }
        Some(Commands::Logs { name, follow }) => {
            let log_dirs = vec![get_nucleus_log_dir(), std::path::PathBuf::from("/tmp/nucleus/logs")];
            let mut resolved_log_path = None;
            for dir in log_dirs {
                let p = dir.join(format!("{}.log", name));
                if p.exists() {
                    resolved_log_path = Some(p);
                    break;
                }
            }

            let log_path = match resolved_log_path {
                Some(p) => p,
                None => {
                    println!("[Nucleus] No logs found for container '{}'.", name);
                    return Ok(());
                }
            };

            if follow {
                std::process::Command::new("tail")
                    .args(["-f", log_path.to_str().unwrap_or("")])
                    .status()
                    .context("Failed to tail log file")?;
            } else {
                let content =
                    std::fs::read_to_string(&log_path).context("Failed to read log file")?;
                print!("{}", content);
            }
        }
        Some(Commands::Stop { name }) => {
            let containers = state::list_containers()?;
            if let Some(c) = containers.iter().find(|c| c.name == name) {
                println!("[Nucleus] Stopping container '{}' (PID {})...", name, c.pid);
                let _ = signal::kill(Pid::from_raw(c.pid as i32), Signal::SIGTERM);
                let _ = orchestrator::teardown_container(&c.name, &c.veth_host, &[], &c.ip, false);
                println!("[Nucleus] Container '{}' stopped.", name);
            } else {
                println!("[Nucleus] Container '{}' not found.", name);
            }
        }
        Some(Commands::Stats { name, stream }) => {
            stats::display_stats(&name, stream)?;
        }
        Some(Commands::Pull { distro }) => {
            image::pull_image(&distro)?;
        }
        None => {
            println!("Use 'nucleus --help' for usage information.");
        }
    }

    Ok(())
}
