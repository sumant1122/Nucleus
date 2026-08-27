use crate::args::RunArgs;
use crate::utils::{get_nucleus_data_dir, get_nucleus_runtime_dir};
use anyhow::{Context, Result};
use caps::{CapSet, Capability};
use libseccomp::*;
use nix::mount::{MntFlags, MsFlags, mount, umount2};
use nix::sched::{CloneFlags, unshare};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::{ForkResult, chdir, execvp, fork, getgid, getuid, pivot_root, read, sethostname};
use std::ffi::CString;
use std::fs;
use std::os::unix::fs::symlink;
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};

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

    // 3. Fork into the new PID namespace so target process runs as PID 1
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

    // Resolve base image rootfs path
    let rootfs_path = crate::image::resolve_image_path(&args.image).ok_or_else(|| {
        anyhow::anyhow!(
            "Image '{}' not found. Please run 'Nucleus pull {}' first.",
            args.image,
            args.image
        )
    })?;

    let runtime_dir = get_nucleus_runtime_dir();
    let root_base = runtime_dir.join("containers").join(&args.name);
    let _ = fs::remove_dir_all(&root_base);
    let upper = root_base.join("upper");
    let work = root_base.join("work");
    let merged = root_base.join("merged");

    fs::create_dir_all(&upper).context("Failed to create upper dir")?;
    fs::create_dir_all(&work).context("Failed to create work dir")?;
    fs::create_dir_all(&merged).context("Failed to create merged dir")?;

    let mut overlay_opts = format!(
        "lowerdir={},upperdir={},workdir={}",
        rootfs_path.to_str().context("Invalid rootfs path")?,
        upper.to_str().context("Invalid upper path")?,
        work.to_str().context("Invalid work path")?
    );

    if args.rootless {
        overlay_opts.push_str(",userxattr");
    }

    mount(
        Some("overlay"),
        &merged,
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
                // Named volume in centralized data dir
                let vol_dir = get_nucleus_data_dir().join("volumes").join(host_part);
                fs::create_dir_all(&vol_dir).context("Failed to create named volume dir")?;
                vol_dir
            };

            fs::create_dir_all(&target_path).context("Failed to create volume target dir")?;
            mount(
                Some(&host_path),
                &target_path,
                None::<&str>,
                MsFlags::MS_BIND | MsFlags::MS_REC,
                None::<&str>,
            )
            .context(format!("Failed to bind mount volume: {}", vol))?;
        }
    }

    mount(
        Some(&merged),
        &merged,
        None::<&str>,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        None::<&str>,
    )
    .context("Failed to bind mount root for pivot_root")?;

    let old_root_name = ".old_root";
    let old_root_path = merged.join(old_root_name);
    fs::create_dir_all(&old_root_path).context("Failed to create old_root dir")?;

    pivot_root(&merged, &old_root_path).context("Failed to pivot_root")?;
    chdir("/").context("Failed to chdir to new root")?;

    let old_root_path_in_container = format!("/{}", old_root_name);
    umount2(old_root_path_in_container.as_str(), MntFlags::MNT_DETACH)
        .context("Failed to unmount old root")?;
    fs::remove_dir(old_root_path_in_container.as_str()).ok();

    // 1. Mount /proc
    fs::create_dir_all("/proc").ok();
    let _ = mount(
        Some("proc"),
        "/proc",
        Some("proc"),
        MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC | MsFlags::MS_NODEV,
        None::<&str>,
    );

    // 2. Mount /sys
    fs::create_dir_all("/sys").ok();
    let _ = mount(
        Some("sysfs"),
        "/sys",
        Some("sysfs"),
        MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC | MsFlags::MS_NODEV | MsFlags::MS_RDONLY,
        None::<&str>,
    );

    // 3. Mount /sys/fs/cgroup
    fs::create_dir_all("/sys/fs/cgroup").ok();
    let _ = mount(
        Some("cgroup2"),
        "/sys/fs/cgroup",
        Some("cgroup2"),
        MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC | MsFlags::MS_NODEV,
        None::<&str>,
    );

    // 4. Mount /dev tmpfs and setup standard device nodes
    fs::create_dir_all("/dev").ok();
    let _ = mount(
        Some("tmpfs"),
        "/dev",
        Some("tmpfs"),
        MsFlags::MS_NOSUID | MsFlags::MS_STRICTATIME,
        Some("mode=755"),
    );

    let dev_nodes = ["null", "zero", "full", "random", "urandom", "tty"];
    for node in &dev_nodes {
        let target = format!("/dev/{}", node);
        let _ = fs::File::create(&target);
        let host_node = format!("/dev/{}", node);
        if Path::new(&host_node).exists() {
            let _ = mount(
                Some(host_node.as_str()),
                target.as_str(),
                None::<&str>,
                MsFlags::MS_BIND,
                None::<&str>,
            );
        }
    }

    fs::create_dir_all("/dev/pts").ok();
    let _ = mount(
        Some("devpts"),
        "/dev/pts",
        Some("devpts"),
        MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC,
        Some("newinstance,ptmxmode=0666,mode=0620"),
    );

    fs::create_dir_all("/dev/shm").ok();
    let _ = mount(
        Some("shm"),
        "/dev/shm",
        Some("tmpfs"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC,
        Some("mode=1777,size=65536k"),
    );

    let _ = symlink("/proc/self/fd", "/dev/fd");
    let _ = symlink("/proc/self/fd/0", "/dev/stdin");
    let _ = symlink("/proc/self/fd/1", "/dev/stdout");
    let _ = symlink("/proc/self/fd/2", "/dev/stderr");
    let _ = symlink("/dev/pts/ptmx", "/dev/ptmx");

    // 5. Setup DNS (/etc/resolv.conf)
    fs::create_dir_all("/etc").ok();
    let resolv_conf = "/etc/resolv.conf";
    let _ = fs::remove_file(resolv_conf);
    let dns_content = "nameserver 1.1.1.1\nnameserver 8.8.8.8\nnameserver 8.8.4.4\n";
    let _ = fs::write(resolv_conf, dns_content);

    // 6. Read-only RootFS
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

    // 7. Security Hardening: PR_SET_NO_NEW_PRIVS, Capabilities & Seccomp
    unsafe {
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
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
    let cmd = CString::new(args.command[0].as_str()).context("Invalid command")?;
    let c_args: Vec<CString> = args
        .command
        .iter()
        .map(|s| CString::new(s.as_str()).context("Invalid argument"))
        .collect::<Result<Vec<_>>>()?;

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
