use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(
    author,
    version,
    about = "Nucleus: High-performance Rust Container Engine"
)]
pub struct OxideArgs {
    /// Unique name for the container instance
    #[arg(short, long)]
    pub name: String,

    /// Static IP address for the container (e.g., 10.0.0.10)
    #[arg(short, long)]
    pub ip: String,

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

    /// Internal flag used for child process orchestration
    #[arg(long, hide = true)]
    pub internal_child: bool,

    /// Internal flag for sync pipe handle
    #[arg(long, hide = true)]
    pub pipe_fd: Option<i32>,
}
