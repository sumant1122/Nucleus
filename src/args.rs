use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug, Clone)]
#[command(
    author,
    version,
    about = "Nucleus: High-performance Rust Container Engine"
)]
pub struct OxideArgs {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
    /// Run a command in a new container
    Run(RunArgs),
    /// Internal subcommand used for child process orchestration
    #[command(name = "internal-child", hide = true)]
    InternalChild(RunArgs),
    /// List running containers
    List,
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

    /// Bind volumes in host:container format
    #[arg(short = 'v', long)]
    pub volumes: Vec<String>,

    /// Map host ports to container ports in host:container format
    #[arg(short = 'p', long)]
    pub ports: Vec<String>,

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
