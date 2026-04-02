mod args;
mod container;
mod orchestrator;
mod utils;

use crate::args::OxideArgs;
use anyhow::Result;
use clap::Parser;
use nix::unistd::getuid;

fn main() -> Result<()> {
    // 2. Parse CLI Arguments
    let args = OxideArgs::parse();

    // 1. Startup Verification: Check for root
    // Nucleus needs root for namespaces, mounts, and networking
    // If --rootless is provided, we warn instead of erroring.
    if !getuid().is_root() {
        if !args.rootless {
            return Err(anyhow::anyhow!(
                "Nucleus must be run as root to manage namespaces and networking. Use --rootless for unprivileged isolation."
            ));
        } else {
            println!("[Nucleus] Running in rootless mode...");
        }
    }

    // 3. Execution Branch
    // If internal_child is set, we are running INSIDE the isolated namespaces
    if args.internal_child {
        container::run_container_child(args)?;
    } else {
        // Otherwise, we are the host-side orchestrator
        orchestrator::run_parent_orchestrator(args)?;
    }

    Ok(())
}
