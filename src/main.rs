mod args;
mod container;
mod image;
mod orchestrator;
mod state;
mod utils;

use crate::args::{Commands, OxideArgs};
use anyhow::{Context, Result};
use clap::Parser;
use nix::sys::signal::{self, Signal};
use nix::unistd::{Pid, getuid};

fn main() -> Result<()> {
    let args = OxideArgs::parse();

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
            let log_path = format!("/tmp/nucleus/logs/{}.log", name);
            if !std::path::Path::new(&log_path).exists() {
                println!("[Nucleus] No logs found for container '{}'.", name);
                return Ok(());
            }

            if follow {
                std::process::Command::new("tail")
                    .args(["-f", &log_path])
                    .status()
                    .context("Failed to tail log file")?;
            } else {
                let content = std::fs::read_to_string(&log_path).context("Failed to read log file")?;
                print!("{}", content);
            }
        }
        Some(Commands::Stop { name }) => {
            let containers = state::list_containers()?;
            if let Some(c) = containers.iter().find(|c| c.name == name) {
                println!("[Nucleus] Stopping container '{}' (PID {})...", name, c.pid);
                signal::kill(Pid::from_raw(c.pid as i32), Signal::SIGTERM)
                    .context("Failed to send SIGTERM to container")?;
                // State will be cleaned up by the orchestrator or next 'list' call
            } else {
                println!("[Nucleus] Container '{}' not found.", name);
            }
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
