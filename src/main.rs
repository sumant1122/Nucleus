mod args;
mod container;
mod image;
mod orchestrator;
mod utils;

use crate::args::{Commands, OxideArgs};
use anyhow::Result;
use clap::Parser;
use nix::unistd::getuid;

fn main() -> Result<()> {
    let args = OxideArgs::parse();

    // Internal child execution branch (re-exec)
    if args.internal_child {
        return container::run_container_child(args.run_args);
    }

    // Command Dispatch
    match args.command {
        Some(Commands::Run(run_args)) => {
            if !getuid().is_root() && !run_args.rootless {
                return Err(anyhow::anyhow!(
                    "Nucleus must be run as root to manage namespaces and networking. Use --rootless for unprivileged isolation."
                ));
            }
            orchestrator::run_parent_orchestrator(run_args)?;
        }
        Some(Commands::List) => {
            println!("[Nucleus] Listing containers (to be implemented)...");
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
