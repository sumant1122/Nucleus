use crate::args::OxideArgs;
use anyhow::{Context, Result};
use caps::{CapSet, Capability};
use nix::mount::{MntFlags, MsFlags, mount, umount2};
use nix::sched::{CloneFlags, unshare};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::{ForkResult, chdir, execvp, fork, pivot_root, read, sethostname};
use std::ffi::CString;
use std::fs;
use std::os::unix::io::RawFd;
use std::path::Path;

/// Child Context: Isolates itself and prepares the container environment.
pub fn run_container_child(args: OxideArgs) -> Result<()> {
    // 1. Isolate BEFORE doing anything else
    unshare(
        CloneFlags::CLONE_NEWNS
            | CloneFlags::CLONE_NEWUTS
            | CloneFlags::CLONE_NEWPID
            | CloneFlags::CLONE_NEWNET
            | CloneFlags::CLONE_NEWCGROUP,
    )
    .context("Failed to isolate child namespaces")?;

    // 2. Fork into the new PID namespace
    // In Linux, the process that calls unshare(CLONE_NEWPID) doesn't enter the namespace,
    // but its next child becomes PID 1.
    match unsafe { fork() }.context("Failed to fork after unshare")? {
        ForkResult::Parent { child } => {
            // Wait for the containerized child (PID 1) to exit
            match waitpid(child, None).context("Failed to wait for child PID 1")? {
                WaitStatus::Exited(_, code) => std::process::exit(code),
                WaitStatus::Signaled(_, sig, _) => std::process::exit(128 + sig as i32),
                _ => std::process::exit(0),
            }
        }
        ForkResult::Child => {
            // We are now PID 1 inside the new namespace.
            setup_container_env(args)?;
        }
    }
    Ok(())
}

fn setup_container_env(args: OxideArgs) -> Result<()> {
    // Fix pivot_root EINVAL: Ensure our mount namespace is private
    mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_REC | MsFlags::MS_PRIVATE,
        None::<&str>,
    )
    .context("Failed to set mount propagation to private")?;

    // Sync with Parent: Wait for host-side networking to be ready
    let pipe_fd = args.pipe_fd.context("Missing pipe handle")?;
    let mut buffer = [0; 4];
    read(pipe_fd as RawFd, &mut buffer).context("Sync read failed")?;

    // Setup Internal Identity
    sethostname(&args.name).ok();

    // Layered Filesystem (OverlayFS)
    let root_base = format!("./temp/{}", args.name);
    let _ = fs::remove_dir_all(&root_base);
    let upper = format!("{}/upper", root_base);
    let work = format!("{}/work", root_base);
    let merged = format!("{}/merged", root_base);

    fs::create_dir_all(&upper).ok();
    fs::create_dir_all(&work).ok();
    fs::create_dir_all(&merged).ok();

    let overlay_opts = format!("lowerdir=./rootfs,upperdir={},workdir={}", upper, work);
    mount(
        Some("overlay"),
        merged.as_str(),
        Some("overlay"),
        MsFlags::empty(),
        Some(overlay_opts.as_str()),
    )
    .context("Failed to mount OverlayFS")?;

    // Pivot Root
    mount(
        Some(merged.as_str()),
        merged.as_str(),
        None::<&str>,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        None::<&str>,
    )
    .context("Failed to bind mount root for pivot_root")?;

    let old_root_name = ".old_root";
    let old_root_path = Path::new(&merged).join(old_root_name);
    fs::create_dir_all(&old_root_path).context("Failed to create old_root dir")?;

    pivot_root(merged.as_str(), old_root_path.as_path()).context("Failed to pivot_root")?;
    chdir("/").context("Failed to chdir to new root")?;

    let old_root_path_in_container = format!("/{}", old_root_name);
    umount2(old_root_path_in_container.as_str(), MntFlags::MNT_DETACH)
        .context("Failed to unmount old root")?;
    fs::remove_dir(old_root_path_in_container.as_str()).ok();

    // System Mounts (Procfs, Sysfs, DNS, Volumes)
    fs::create_dir_all("/proc").ok();
    mount(
        Some("proc"),
        "/proc",
        Some("proc"),
        MsFlags::empty(),
        None::<&str>,
    )
    .context("Failed to mount proc")?;
    fs::create_dir_all("/etc").ok();

    fs::create_dir_all("/sys").ok();
    mount(
        Some("sysfs"),
        "/sys",
        Some("sysfs"),
        MsFlags::empty(),
        None::<&str>,
    )
    .context("Failed to mount sysfs")?;

    fs::create_dir_all("/sys/fs/cgroup").ok();
    mount(
        Some("cgroup2"),
        "/sys/fs/cgroup",
        Some("cgroup2"),
        MsFlags::empty(),
        None::<&str>,
    )
    .context("Failed to mount cgroup2")?;

    let resolv_conf = "/etc/resolv.conf";
    if Path::new(resolv_conf).exists() {
        fs::File::create(resolv_conf).ok();
        mount(
            Some(resolv_conf),
            resolv_conf,
            None::<&str>,
            MsFlags::MS_BIND | MsFlags::MS_RDONLY,
            None::<&str>,
        )
        .context("Failed to bind mount resolv.conf")?;
    }

    // Bind User Volumes: -v /host:/container
    for vol in &args.volumes {
        let parts: Vec<&str> = vol.split(':').collect();
        if parts.len() == 2 {
            let host_path = parts[0];
            let container_path = if parts[1].starts_with('/') {
                parts[1].to_string()
            } else {
                format!("/{}", parts[1])
            };

            fs::create_dir_all(&container_path).ok();
            mount(
                Some(host_path),
                container_path.as_str(),
                None::<&str>,
                MsFlags::MS_BIND | MsFlags::MS_REC,
                None::<&str>,
            )
            .context(format!("Failed to bind mount volume: {}", vol))?;
        }
    }

    // Security: Drop dangerous capabilities
    drop_capabilities()?;

    // Setup Environment Variables
    unsafe {
        std::env::set_var(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        );
        std::env::set_var("HOME", "/root");
        std::env::set_var("USER", "root");
        std::env::remove_var("PS1");
        std::env::remove_var("PROMPT");
    }

    // Execute Target Command
    println!("[Container] Entering {}...", args.command[0]);
    let cmd = CString::new(args.command[0].as_str()).unwrap();
    let c_args: Vec<CString> = args
        .command
        .iter()
        .map(|s| CString::new(s.as_str()).unwrap())
        .collect();

    execvp(&cmd, &c_args).context("Failed to execute inner command")?;

    Ok(())
}

fn drop_capabilities() -> Result<()> {
    println!("[Container] Dropping unnecessary capabilities...");
    let to_drop = [
        Capability::CAP_SYS_RAWIO,
        Capability::CAP_MKNOD,
        Capability::CAP_SYS_TIME,
        Capability::CAP_AUDIT_CONTROL,
        Capability::CAP_MAC_ADMIN,
        Capability::CAP_MAC_OVERRIDE,
        Capability::CAP_SYS_MODULE,
    ];

    for cap in to_drop {
        caps::drop(None, CapSet::Inheritable, cap).context("Failed to drop inheritable cap")?;
        caps::drop(None, CapSet::Bounding, cap).context("Failed to drop bounding cap")?;
    }
    Ok(())
}
