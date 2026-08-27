use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug, Clone)]
#[command(
    author,
    version,
    about = "Nucleus: High-performance Rust Container Engine"
)]
pub struct NucleusArgs {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

pub type OxideArgs = NucleusArgs;

#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
    /// Run a command in a new container
    Run(RunArgs),
    /// Internal subcommand used for child process orchestration
    #[command(name = "internal-child", hide = true)]
    InternalChild(RunArgs),
    /// Execute a command in a running container
    Exec {
        /// Name of the container
        name: String,
        /// Keep STDIN open even if not attached
        #[arg(short = 'i', long)]
        interactive: bool,
        /// Allocate a pseudo-TTY
        #[arg(short = 't', long)]
        tty: bool,
        /// Set environment variables (KEY=VALUE)
        #[arg(short = 'e', long = "env")]
        env: Vec<String>,
        /// Working directory inside the container
        #[arg(short = 'w', long = "workdir")]
        workdir: Option<String>,
        /// Command and arguments to execute
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,
    },
    /// Display detailed information on a container
    Inspect {
        /// Name of the container
        name: String,
    },
    /// List running containers
    List,
    /// List running containers (alias for list)
    Ps,
    /// List local images
    Images,
    /// Remove a local image
    Rmi {
        /// Name of the image to remove
        image: String,
    },
    /// Remove a container
    Rm {
        /// Name of the container
        name: String,
        /// Force removal if running
        #[arg(short, long)]
        force: bool,
    },
    /// Fetch logs for a container
    Logs {
        /// Name of the container
        name: String,
        /// Follow log output
        #[arg(short, long)]
        follow: bool,
    },
    /// Stop a running container
    Stop {
        /// Name of the container to stop
        name: String,
        /// Seconds to wait for stop before killing the container
        #[arg(short, long, default_value = "10")]
        timeout: u64,
    },
    /// Show resource usage statistics for a container
    Stats {
        /// Name of the container
        name: String,
        /// Stream statistics (continuous update)
        #[arg(short, long)]
        stream: bool,
    },
    /// Pull a rootfs image
    Pull {
        /// Distribution name (e.g., alpine, ubuntu, debian)
        #[arg(default_value = "alpine")]
        distro: String,
    },
}

#[derive(Args, Debug, Clone)]
pub struct RunArgs {
    /// Name of the image to use (e.g., alpine, ubuntu)
    #[arg(short, long, default_value = "alpine")]
    pub image: String,

    /// Unique name for the container instance
    #[arg(short, long)]
    pub name: String,

    /// Static IP address for the container (e.g., 10.0.0.10). Auto-assigned if omitted.
    #[arg(short, long)]
    pub ip: Option<String>,

    /// Bridge network to use
    #[arg(long, default_value = "br0")]
    pub network: String,

    /// Memory limit for the container (e.g., 512M, 1G, or "max")
    #[arg(short, long, default_value = "1G")]
    pub memory: String,

    /// Number of CPUs (e.g. 0.5, 1.5, 2.0)
    #[arg(long)]
    pub cpus: Option<f64>,

    /// Maximum number of PIDs in the container
    #[arg(long)]
    pub pids_limit: Option<u32>,

    /// Bind volumes in host:container format
    #[arg(short = 'v', long)]
    pub volumes: Vec<String>,

    /// Map host ports to container ports in host:container format
    #[arg(short = 'p', long)]
    pub ports: Vec<String>,

    /// Set environment variables in KEY=VALUE format
    #[arg(short = 'e', long = "env")]
    pub env: Vec<String>,

    /// Initial working directory inside the container
    #[arg(short = 'w', long = "workdir")]
    pub workdir: Option<String>,

    /// Keep STDIN open even if not attached
    #[arg(short = 'i', long)]
    pub interactive: bool,

    /// Allocate a pseudo-TTY
    #[arg(short = 't', long)]
    pub tty: bool,

    /// The command and its arguments to run inside the container
    #[arg(trailing_var_arg = true, default_value = "/bin/sh")]
    pub command: Vec<String>,

    /// Internal flag for sync pipe handle
    #[arg(long, hide = true)]
    pub pipe_fd: Option<i32>,

    /// Run in rootless mode using User Namespaces
    #[arg(long)]
    pub rootless: bool,

    /// Run container in background and redirect output to logs
    #[arg(short, long)]
    pub detach: bool,

    /// Mount the root filesystem as read-only
    #[arg(long)]
    pub readonly: bool,
}
