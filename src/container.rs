use crate::args::RunArgs;
use anyhow::{Context, Result};
use caps::{CapSet, Capability};
use libseccomp::*;
use nix::mount::{MntFlags, MsFlags, mount, umount2};
use nix::sched::{CloneFlags, unshare};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::{ForkResult, chdir, execvp, fork, getgid, getuid, pivot_root, read, sethostname};
use std::ffi::CString;
use std::fs;
use std::os::unix::io::RawFd;
use std::path::Path;

/// Child Context: Isolates itself and prepares the container environment.
pub fn run_container_child(args: RunArgs) -> Result<()> {
    let host_uid = getuid();
    let host_gid = getgid();

    // 1. Isolate User Namespace FIRST if rootless
    if args.rootless {
        unshare(CloneFlags::CLONE_NEWUSER).context("Failed to unshare user namespace")?;

        println!("[Container] Setting up User Namespace ID mapping...");
        let uid_map = format!("0 {} 1", host_uid);
        fs::write("/proc/self/uid_map", uid_map).context("Failed to write to uid_map")?;
        fs::write("/proc/self/setgroups", "deny").context("Failed to write to setgroups")?;
        let gid_map = format!("0 {} 1", host_gid);
        fs::write("/proc/self/gid_map", gid_map).context("Failed to write to gid_map")?;
    }

    // 2. Isolate other namespaces
    let clone_flags = CloneFlags::CLONE_NEWNS
        | CloneFlags::CLONE_NEWUTS
        | CloneFlags::CLONE_NEWPID
        | CloneFlags::CLONE_NEWNET
        | CloneFlags::CLONE_NEWCGROUP;

    unshare(clone_flags).context("Failed to isolate other namespaces")?;

    // 3. Fork into the new PID namespace
    match unsafe { fork() }.context("Failed to fork after unshare")? {
        ForkResult::Parent { child } => {
            match waitpid(child, None).context("Failed to wait for child PID 1")? {
                WaitStatus::Exited(_, code) => std::process::exit(code),
                WaitStatus::Signaled(_, sig, _) => std::process::exit(128 + sig as i32),
                _ => std::process::exit(0),
            }
        }
        ForkResult::Child => {
            setup_container_env(args)?;
        }
    }
    Ok(())
}

fn apply_seccomp_filter() -> Result<()> {
    println!("[Container] Applying Seccomp syscall filter...");
    let mut filter = ScmpFilterContext::new_filter(ScmpAction::Allow)
        .context("Failed to create Seccomp context")?;

    let syscalls_to_block = [
        "reboot",
        "sethostname",
        "swapon",
        "swapoff",
        "mount",
        "umount2",
    ];

    for syscall_name in syscalls_to_block {
        let syscall = ScmpSyscall::from_name(syscall_name)
            .context(format!("Invalid syscall name: {}", syscall_name))?;
        filter
            .add_rule(ScmpAction::Errno(libc::EPERM), syscall)
            .context(format!("Failed to block syscall: {}", syscall_name))?;
    }

    filter.load().context("Failed to load Seccomp filter")?;
    Ok(())
}

fn setup_container_env(args: RunArgs) -> Result<()> {
    mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_REC | MsFlags::MS_PRIVATE,
        None::<&str>,
    )
    .context("Failed to set mount propagation to private")?;

    let pipe_fd = args.pipe_fd.context("Missing pipe handle")?;
    let mut buffer = [0; 4];
    read(pipe_fd as RawFd, &mut buffer).context("Sync read failed")?;

    sethostname(&args.name).ok();

    let cwd = std::env::current_dir().context("Failed to get current dir")?;
    let rootfs_path = cwd.join(crate::image::IMAGES_DIR).join(&args.image);
    if !rootfs_path.exists() {
        return Err(anyhow::anyhow!(
            "Image '{}' not found in {}. Please run 'Nucleus pull {}' first.",
            args.image,
            crate::image::IMAGES_DIR,
            args.image
        ));
    }
    let root_base = cwd.join("temp").join(&args.name);
    let _ = fs::remove_dir_all(&root_base);
    let upper = root_base.join("upper");
    let work = root_base.join("work");
    let merged = root_base.join("merged");

    fs::create_dir_all(&upper).context("Failed to create upper dir")?;
    fs::create_dir_all(&work).context("Failed to create work dir")?;
    fs::create_dir_all(&merged).context("Failed to create merged dir")?;

    let overlay_opts = format!(
        "lowerdir={},upperdir={},workdir={}",
        rootfs_path.to_str().unwrap(),
        upper.to_str().unwrap(),
        work.to_str().unwrap()
    );
    mount(
        Some("overlay"),
        merged.to_str().unwrap(),
        Some("overlay"),
        MsFlags::empty(),
        Some(overlay_opts.as_str()),
    )
    .context("Failed to mount OverlayFS")?;

    // Bind mount host volumes into the merged rootfs BEFORE pivoting
    for vol in &args.volumes {
        let parts: Vec<&str> = vol.split(':').collect();
        if parts.len() == 2 {
            let host_part = parts[0];
            let container_rel_path = if parts[1].starts_with('/') {
                &parts[1][1..]
            } else {
                parts[1]
            };
            let target_path = merged.join(container_rel_path);

            let host_path = if host_part.starts_with('/') || host_part.starts_with('.') {
                Path::new(host_part).to_path_buf()
            } else {
                // Named volume
                let vol_dir = Path::new("/tmp/nucleus/volumes").join(host_part);
                fs::create_dir_all(&vol_dir).context("Failed to create named volume dir")?;
                vol_dir
            };

            fs::create_dir_all(&target_path).context("Failed to create volume target dir")?;
            mount(
                Some(host_path.to_str().unwrap()),
                target_path.to_str().unwrap(),
                None::<&str>,
                MsFlags::MS_BIND | MsFlags::MS_REC,
                None::<&str>,
            )
            .context(format!("Failed to bind mount volume: {}", vol))?;
        }
    }

    mount(
        Some(merged.to_str().unwrap()),
        merged.to_str().unwrap(),
        None::<&str>,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        None::<&str>,
    )
    .context("Failed to bind mount root for pivot_root")?;

    let old_root_name = ".old_root";
    let old_root_path = merged.join(old_root_name);
    fs::create_dir_all(&old_root_path).context("Failed to create old_root dir")?;

    pivot_root(merged.to_str().unwrap(), old_root_path.as_path())
        .context("Failed to pivot_root")?;
    chdir("/").context("Failed to chdir to new root")?;

    let old_root_path_in_container = format!("/{}", old_root_name);
    umount2(old_root_path_in_container.as_str(), MntFlags::MNT_DETACH)
        .context("Failed to unmount old root")?;
    fs::remove_dir(old_root_path_in_container.as_str()).ok();

    fs::create_dir_all("/proc").ok();
    let _ = mount(
        Some("proc"),
        "/proc",
        Some("proc"),
        MsFlags::empty(),
        None::<&str>,
    );
    fs::create_dir_all("/etc").ok();

    fs::create_dir_all("/sys").ok();
    let _ = mount(
        Some("sysfs"),
        "/sys",
        Some("sysfs"),
        MsFlags::empty(),
        None::<&str>,
    );

    fs::create_dir_all("/sys/fs/cgroup").ok();
    let _ = mount(
        Some("cgroup2"),
        "/sys/fs/cgroup",
        Some("cgroup2"),
        MsFlags::empty(),
        None::<&str>,
    );

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

    if args.readonly {
        println!("[Container] Remounting root filesystem as read-only...");
        mount(
            None::<&str>,
            "/",
            None::<&str>,
            MsFlags::MS_REMOUNT | MsFlags::MS_BIND | MsFlags::MS_RDONLY,
            None::<&str>,
        )
        .context("Failed to remount / as read-only")?;
    }

    drop_capabilities()?;
    apply_seccomp_filter()?;

    // TTY Support: Create a new session and set controlling terminal
    if nix::unistd::isatty(libc::STDIN_FILENO).unwrap_or(false) {
        let _ = nix::unistd::setsid();
        unsafe {
            libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY, 1);
        }
    }

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
        Capability::CAP_SYS_PTRACE,
        Capability::CAP_SYS_PACCT,
        Capability::CAP_SYS_TTY_CONFIG,
    ];

    for cap in to_drop {
        let _ = caps::drop(None, CapSet::Inheritable, cap);
        let _ = caps::drop(None, CapSet::Bounding, cap);
    }
    Ok(())
}
