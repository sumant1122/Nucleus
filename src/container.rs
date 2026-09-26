use crate::args::RunArgs;
use crate::utils::{
    get_nucleus_data_dir, get_nucleus_runtime_dir, normalize_container_path, parse_env_assignment,
    parse_volume,
};
use anyhow::{Context, Result, bail};
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

/// Capabilities a container process is allowed to keep.
///
/// This mirrors the runc default: enough for everyday workloads while denying
/// namespace, module, raw-device, and tracing primitives.
const ALLOWED_CAPABILITIES: &[Capability] = &[
    Capability::CAP_CHOWN,
    Capability::CAP_DAC_OVERRIDE,
    Capability::CAP_FOWNER,
    Capability::CAP_FSETID,
    Capability::CAP_KILL,
    Capability::CAP_SETGID,
    Capability::CAP_SETUID,
    Capability::CAP_SETPCAP,
    Capability::CAP_NET_BIND_SERVICE,
    Capability::CAP_NET_RAW,
    Capability::CAP_SYS_CHROOT,
    Capability::CAP_MKNOD,
    Capability::CAP_AUDIT_WRITE,
    Capability::CAP_SETFCAP,
];

/// Syscalls denied with EPERM inside the container.
const BLOCKED_SYSCALLS: &[&str] = &[
    // Kernel / system
    "reboot",
    "kexec_load",
    "kexec_file_load",
    "init_module",
    "finit_module",
    "delete_module",
    "swapon",
    "swapoff",
    "syslog",
    // Namespace and mount manipulation: the container is already isolated, and
    // these are the primitives used to escape it.
    "mount",
    "umount",
    "umount2",
    "pivot_root",
    "chroot",
    "setns",
    "unshare",
    // Kernel tracing / eBPF
    "ptrace",
    "bpf",
    "perf_event_open",
    // Privileged process inspection
    "process_vm_readv",
    "process_vm_writev",
    "kcmp",
    "open_by_handle_at",
    "name_to_handle_at",
    // Keyring
    "add_key",
    "request_key",
    "keyctl",
    // Time and clocks (can be used to extend a signed token's lifetime)
    "settimeofday",
    "clock_settime",
    "adjtimex",
    "clock_adjtime",
    // Miscellaneous privilege-sensitive calls
    "personality",
    "quotactl",
    "nfsservctl",
    "create_module",
    "get_kernel_syms",
    "uselib",
    "modify_ldt",
    "ioperm",
    "iopl",
    "acct",
    "move_pages",
    "mbind",
    "set_mempolicy",
];

/// Syscalls denied with ENOSYS so callers fall back to a supported variant.
const UNSUPPORTED_SYSCALLS: &[&str] = &[
    // glibc transparently falls back to clone(2) when clone3 is unavailable.
    "clone3",
];

/// Child Context: Isolates itself and prepares the container environment.
pub fn run_container_child(args: RunArgs) -> Result<()> {
    let host_uid = getuid();
    let host_gid = getgid();

    // Resolve all host paths *before* unsharing. Inside a user namespace the
    // process is uid 0, so the XDG/root-aware path helpers would otherwise
    // resolve to root-only locations like /var/lib and /run, making the image
    // and overlay directories unreachable for unprivileged users.
    let data_dir = get_nucleus_data_dir();
    let runtime_dir = get_nucleus_runtime_dir();
    let rootfs_path = crate::image::resolve_image_path(&args.image).ok_or_else(|| {
        anyhow::anyhow!(
            "Image '{}' not found. Please run 'Nucleus pull {}' first.",
            args.image,
            args.image
        )
    })?;
    if !crate::image::is_valid_rootfs(&rootfs_path) {
        bail!(
            "Image '{}' at {} is not a usable rootfs (missing bin/, lib/ or etc/). Try 'Nucleus pull {} --force'.",
            args.image,
            rootfs_path.display(),
            args.image
        );
    }

    // Validate everything that influences filesystem layout before unsharing,
    // so a bad spec fails before any namespace exists.
    let volumes = args
        .volumes
        .iter()
        .map(|v| parse_volume(v))
        .collect::<Result<Vec<_>>>()
        .context("Invalid volume specification")?;

    // 1. Isolate User Namespace FIRST if rootless
    if args.rootless {
        unshare(CloneFlags::CLONE_NEWUSER).context("Failed to unshare user namespace")?;

        println!("[Container] Setting up User Namespace ID mapping...");
        // Map only the invoking uid/gid; without setgroups=deny the write is
        // rejected on modern kernels.
        let uid_map = format!("0 {host_uid} 1");
        fs::write("/proc/self/setgroups", "deny").context("Failed to write to setgroups")?;
        fs::write("/proc/self/uid_map", uid_map).context("Failed to write to uid_map")?;
        let gid_map = format!("0 {host_gid} 1");
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
            // Reap PID 1 and mirror its exit status so the host orchestrator
            // observes the container's real result.
            match waitpid(child, None).context("Failed to wait for child PID 1")? {
                WaitStatus::Exited(_, code) => std::process::exit(code),
                WaitStatus::Signaled(_, sig, _) => std::process::exit(128 + sig as i32),
                other => {
                    eprintln!("[Container] PID 1 ended unexpectedly: {other:?}");
                    std::process::exit(0);
                }
            }
        }
        ForkResult::Child => {
            setup_container_env(args, &volumes, &rootfs_path, &runtime_dir, &data_dir)?;
        }
    }
    Ok(())
}

/// Applies a seccomp filter that denies dangerous syscalls.
fn apply_seccomp_filter() -> Result<()> {
    println!("[Container] Applying Seccomp syscall filter...");
    let mut filter = ScmpFilterContext::new_filter(ScmpAction::Allow)
        .context("Failed to create Seccomp context")?;

    for syscall_name in BLOCKED_SYSCALLS {
        // Syscalls absent from this kernel's ABI table are simply skipped.
        let Ok(syscall) = ScmpSyscall::from_name(syscall_name) else {
            continue;
        };
        filter
            .add_rule(ScmpAction::Errno(libc::EPERM), syscall)
            .with_context(|| format!("Failed to block syscall: {syscall_name}"))?;
    }

    for syscall_name in UNSUPPORTED_SYSCALLS {
        let Ok(syscall) = ScmpSyscall::from_name(syscall_name) else {
            continue;
        };
        filter
            .add_rule(ScmpAction::Errno(libc::ENOSYS), syscall)
            .with_context(|| format!("Failed to restrict syscall: {syscall_name}"))?;
    }

    filter.load().context("Failed to load Seccomp filter")?;
    Ok(())
}

/// Restricts the process to [`ALLOWED_CAPABILITIES`] across every capability set.
///
/// The previous implementation only pruned the inheritable and bounding sets,
/// leaving the effective and permitted sets intact, so the container process
/// still ran with far more privilege than intended.
fn restrict_capabilities() -> Result<()> {
    println!("[Container] Restricting capabilities...");
    let allowed: caps::CapsHashSet = ALLOWED_CAPABILITIES.iter().copied().collect();
    let empty: caps::CapsHashSet = caps::CapsHashSet::new();

    for set in [CapSet::Effective, CapSet::Permitted] {
        caps::set(None, set, &allowed)
            .with_context(|| format!("Failed to set {set:?} capability set"))?;
    }
    caps::set(None, CapSet::Inheritable, &empty).context("Failed to clear inheritable set")?;
    caps::set(None, CapSet::Ambient, &empty).context("Failed to clear ambient set")?;

    // The bounding set is a one-way door: explicitly drop everything not allowed.
    for cap in caps::all() {
        if !allowed.contains(&cap) {
            let _ = caps::drop(None, CapSet::Bounding, cap);
        }
    }
    Ok(())
}

/// Reads the host's DNS configuration, keeping only usable `nameserver` lines.
///
/// Copying the host resolver preserves corporate split-DNS setups, which the
/// previous hardcoded public list silently bypassed. Loopback resolvers are
/// dropped: distributions using systemd-resolved point /etc/resolv.conf at
/// 127.0.0.53, a stub that only exists on the host's own loopback and is
/// therefore unreachable from inside a container.
fn host_resolv_conf() -> String {
    let mut servers: Vec<String> = Vec::new();
    if let Ok(content) = fs::read_to_string("/etc/resolv.conf") {
        for line in content.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if let Some(rest) = line.strip_prefix("nameserver") {
                let addr = rest.trim();
                if addr.is_empty() || is_loopback_resolver(addr) {
                    continue;
                }
                if !servers.iter().any(|s| s == addr) {
                    servers.push(addr.to_string());
                }
            }
        }
    }

    if servers.is_empty() {
        servers.push("1.1.1.1".to_string());
        servers.push("8.8.8.8".to_string());
    }
    servers
        .iter()
        .map(|s| format!("nameserver {s}\n"))
        .collect()
}

/// True for addresses that only resolve on the host's loopback interface.
fn is_loopback_resolver(addr: &str) -> bool {
    if addr == "::1" {
        return true;
    }
    match addr.parse::<std::net::Ipv4Addr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => false,
    }
}

fn setup_container_env(
    args: RunArgs,
    volumes: &[crate::utils::Volume],
    rootfs_path: &Path,
    runtime_dir: &Path,
    data_dir: &Path,
) -> Result<()> {
    mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_REC | MsFlags::MS_PRIVATE,
        None::<&str>,
    )
    .context("Failed to set mount propagation to private")?;

    // Block until the orchestrator has finished wiring up networking.
    let pipe_fd = args.pipe_fd.context("Missing pipe handle")?;
    let mut buffer = [0u8; 4];
    let read_bytes = read(pipe_fd as RawFd, &mut buffer).context("Sync read failed")?;
    if read_bytes != buffer.len() {
        // The orchestrator died before signalling readiness. Continuing would
        // produce a container with no network and no supervisor.
        bail!("orchestrator closed the sync pipe before the container was ready");
    }

    sethostname(&args.name).ok();

    let root_base = runtime_dir.join("containers").join(&args.name);
    // Clear any leftovers from a previous run. A crashed container can leave an
    // OverlayFS work directory with mode 000 behind, so the removal has to be
    // able to restore access.
    crate::utils::force_remove_dir_all(&root_base)
        .with_context(|| format!("Failed to clear stale runtime dir {}", root_base.display()))?;
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

    // Bind mount host volumes into the merged rootfs BEFORE pivoting. Targets
    // are normalized and `..`-free, so they cannot escape the container root.
    for vol in volumes {
        let container_rel_path = &vol.destination;
        let target_path = merged.join(container_rel_path);

        let host_path = if vol.named {
            let vol_dir = data_dir.join("volumes").join(&vol.source);
            fs::create_dir_all(&vol_dir)
                .with_context(|| format!("Failed to create named volume '{}'", vol.source))?;
            vol_dir
        } else {
            let raw = vol
                .source
                .replace('~', &std::env::var("HOME").unwrap_or_default());
            let candidate = PathBuf::from(&raw);
            if !candidate.is_absolute() {
                // Relative host paths are resolved against the caller's CWD.
                std::env::current_dir()
                    .context("Failed to resolve current directory")?
                    .join(candidate)
            } else {
                candidate
            }
        };

        if !host_path.exists() {
            fs::create_dir_all(&host_path).with_context(|| {
                format!("Failed to create volume source '{}'", host_path.display())
            })?;
        }

        fs::create_dir_all(&target_path).with_context(|| {
            format!(
                "Failed to create volume mount point '{}' inside the container",
                container_rel_path
            )
        })?;

        mount(
            Some(&host_path),
            &target_path,
            None::<&str>,
            MsFlags::MS_BIND | MsFlags::MS_REC,
            None::<&str>,
        )
        .with_context(|| {
            format!(
                "Failed to bind mount volume source '{}'",
                host_path.display()
            )
        })?;

        if vol.read_only {
            mount(
                None::<&str>,
                &target_path,
                None::<&str>,
                MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY,
                None::<&str>,
            )
            .with_context(|| {
                format!(
                    "Failed to remount volume '{}' read-only",
                    host_path.display()
                )
            })?;
        }
    }

    if args.rootless {
        // In rootless mode these must be bind-mounted from the host *before*
        // pivot_root. Afterwards the host root is gone, so a bind of "/proc"
        // would capture the empty directory inside the image instead.
        for dir in ["/proc", "/sys", "/sys/fs/cgroup"] {
            let target = merged.join(dir.trim_start_matches('/'));
            fs::create_dir_all(&target)
                .with_context(|| format!("Failed to create {dir} in rootfs"))?;
            mount(
                Some(dir),
                &target,
                None::<&str>,
                MsFlags::MS_BIND | MsFlags::MS_REC,
                None::<&str>,
            )
            .with_context(|| format!("Failed to bind mount host {dir} into the container"))?;
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

    let old_root_path_in_container = format!("/{old_root_name}");
    umount2(old_root_path_in_container.as_str(), MntFlags::MNT_DETACH)
        .context("Failed to unmount old root")?;
    fs::remove_dir(old_root_path_in_container.as_str()).ok();

    setup_pseudo_filesystems(args.rootless)?;

    // Hostname resolution helpers, so `ping <name>` works inside the container.
    fs::write("/etc/hostname", format!("{}\n", args.name)).ok();
    fs::write(
        "/etc/hosts",
        format!(
            "127.0.0.1\tlocalhost\n127.0.0.1\t{}\n::1\tlocalhost ip6-localhost ip6-loopback\n",
            args.name
        ),
    )
    .ok();

    // DNS configuration, inherited from the host where possible.
    let resolv_conf = "/etc/resolv.conf";
    let _ = fs::remove_file(resolv_conf);
    fs::write(resolv_conf, host_resolv_conf()).ok();

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

    // 7. Security hardening, in the order the kernel requires:
    // no_new_privs must precede the seccomp filter load, and capabilities are
    // dropped before the container process is exec'd.
    //
    // SAFETY: prctl with PR_SET_NO_NEW_PRIVS takes (int, unsigned long, ...)
    // and has no preconditions beyond a valid option value.
    unsafe {
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
    }
    restrict_capabilities()?;
    apply_seccomp_filter()?;

    setup_controlling_terminal(&args)?;

    // Environment: start from a known-good PATH, then apply user overrides.
    // SAFETY: single-threaded at this point; no other thread can observe a
    // torn environment.
    unsafe {
        std::env::set_var(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        );
        std::env::set_var("HOME", "/root");
        std::env::set_var("USER", "root");
        std::env::set_var("TERM", if args.tty { "xterm" } else { "dumb" });
        std::env::remove_var("PS1");
        std::env::remove_var("PROMPT");
    }

    for assignment in &args.env {
        let (key, value) = parse_env_assignment(assignment)
            .with_context(|| format!("Invalid environment variable '{assignment}'"))?;
        // SAFETY: as above, this process is single-threaded.
        unsafe {
            std::env::set_var(key, value);
        }
    }

    // Working directory, validated the same way as volume destinations.
    if let Some(workdir) = &args.workdir {
        let normalized = normalize_container_path(workdir)
            .with_context(|| format!("Invalid working directory '{workdir}'"))?;
        chdir(normalized.as_str())
            .with_context(|| format!("Failed to change directory to '{normalized}'"))?;
    }

    if args.command.is_empty() {
        bail!("No command specified for the container");
    }

    println!("[Container] Entering {}...", args.command[0]);
    let cmd = CString::new(args.command[0].as_str()).context("Invalid command")?;
    let c_args: Vec<CString> = args
        .command
        .iter()
        .map(|s| CString::new(s.as_str()).context("Invalid argument"))
        .collect::<Result<Vec<_>>>()?;

    execvp(&cmd, &c_args).context("Failed to execute inner command")?;

    // execvp only returns on failure.
    Ok(())
}

/// Mounts the standard container pseudo-filesystems.
///
/// Privileged mode gets fresh kernel instances. Rootless mode cannot: the
/// kernel refuses a new procfs inside a user namespace unless the host's mount
/// is "fully visible", which modern distributions break by masking paths under
/// /proc. Those are bind-mounted from the host before `pivot_root` instead,
/// which is what bubblewrap and podman do; the trade-off is that the host's
/// /proc view is visible inside a rootless container.
fn setup_pseudo_filesystems(rootless: bool) -> Result<()> {
    if !rootless {
        // 1. Mount /proc
        fs::create_dir_all("/proc").ok();
        mount(
            Some("proc"),
            "/proc",
            Some("proc"),
            MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC | MsFlags::MS_NODEV,
            None::<&str>,
        )
        .context("Failed to mount /proc")?;

        // 2. Mount /sys read-only
        fs::create_dir_all("/sys").ok();
        mount(
            Some("sysfs"),
            "/sys",
            Some("sysfs"),
            MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC | MsFlags::MS_NODEV | MsFlags::MS_RDONLY,
            None::<&str>,
        )
        .context("Failed to mount /sys")?;

        // 3. Mount /sys/fs/cgroup (cgroup v2, read-only to the container)
        fs::create_dir_all("/sys/fs/cgroup").ok();
        mount(
            Some("cgroup2"),
            "/sys/fs/cgroup",
            Some("cgroup2"),
            MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC | MsFlags::MS_NODEV | MsFlags::MS_RDONLY,
            None::<&str>,
        )
        .context("Failed to mount /sys/fs/cgroup")?;
    }

    // 4. Mount /dev tmpfs and setup standard device nodes
    fs::create_dir_all("/dev").ok();
    mount(
        Some("tmpfs"),
        "/dev",
        Some("tmpfs"),
        MsFlags::MS_NOSUID | MsFlags::MS_STRICTATIME,
        Some("mode=755"),
    )
    .context("Failed to mount /dev")?;

    // Each node is created as an empty placeholder so it has a mount target,
    // then the host device is bind-mounted over it. mknod(2) is blocked by the
    // seccomp policy and unavailable in rootless mode, so bind mounts are the
    // portable choice.
    for node in ["null", "zero", "full", "random", "urandom", "tty"] {
        let target = format!("/dev/{node}");
        let _ = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&target);
        let host_node = format!("/dev/{node}");
        if Path::new(&host_node).exists() {
            mount(
                Some(host_node.as_str()),
                target.as_str(),
                None::<&str>,
                MsFlags::MS_BIND,
                None::<&str>,
            )
            .with_context(|| format!("Failed to bind mount {host_node}"))?;
        }
    }

    fs::create_dir_all("/dev/pts").ok();
    mount(
        Some("devpts"),
        "/dev/pts",
        Some("devpts"),
        MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC,
        Some("newinstance,ptmxmode=0666,mode=0620"),
    )
    .context("Failed to mount /dev/pts")?;

    fs::create_dir_all("/dev/shm").ok();
    mount(
        Some("shm"),
        "/dev/shm",
        Some("tmpfs"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC,
        Some("mode=1777,size=65536k"),
    )
    .context("Failed to mount /dev/shm")?;

    let _ = symlink("/proc/self/fd", "/dev/fd");
    let _ = symlink("/proc/self/fd/0", "/dev/stdin");
    let _ = symlink("/proc/self/fd/1", "/dev/stdout");
    let _ = symlink("/proc/self/fd/2", "/dev/stderr");
    let _ = symlink("/dev/pts/ptmx", "/dev/ptmx");
    Ok(())
}

/// Establishes a controlling terminal for the container process.
///
/// Nucleus does not allocate a pseudo-terminal: there is no `openpty` call
/// anywhere in the codebase. `--tty` therefore only asserts that an inherited
/// terminal exists and asks for `TERM=xterm`; when the container is started
/// without a controlling terminal the flag is rejected rather than silently
/// ignored. Allocating a pty pair and proxying it from the orchestrator is not
/// implemented yet.
fn setup_controlling_terminal(args: &RunArgs) -> Result<()> {
    // SAFETY: setsid() takes no arguments and only fails if the process is
    // already a session leader, in which case the controlling terminal is
    // acquired by the ioctl below regardless.
    unsafe {
        libc::setsid();
    }

    let is_tty = nix::unistd::isatty(libc::STDIN_FILENO).unwrap_or(false);
    if is_tty {
        // SAFETY: TIOCSCTTY takes a single int argument and acts on the
        // calling process's controlling terminal.
        unsafe {
            libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY, 0);
        }
    }

    if args.tty && !is_tty {
        bail!(
            "--tty requires an inherited pseudo-terminal, and Nucleus does not allocate one yet; \
             run from an interactive terminal or drop the flag"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_seccomp_blocklist_targets_escape_primitives() {
        for critical in [
            "mount",
            "pivot_root",
            "setns",
            "unshare",
            "ptrace",
            "bpf",
            "perf_event_open",
            "reboot",
        ] {
            assert!(
                BLOCKED_SYSCALLS.contains(&critical),
                "'{critical}' must be blocked to prevent container escape"
            );
        }
    }

    #[test]
    fn test_clone3_uses_enosys_for_fallback() {
        // EPERM on clone3 would break glibc rather than trigger its fallback.
        assert!(UNSUPPORTED_SYSCALLS.contains(&"clone3"));
        assert!(!BLOCKED_SYSCALLS.contains(&"clone3"));
    }

    #[test]
    fn test_allowed_capabilities_exclude_dangerous_ones() {
        for dangerous in [
            Capability::CAP_SYS_ADMIN,
            Capability::CAP_SYS_MODULE,
            Capability::CAP_SYS_RAWIO,
            Capability::CAP_SYS_PTRACE,
            Capability::CAP_NET_ADMIN,
            Capability::CAP_MAC_ADMIN,
            Capability::CAP_SYS_BOOT,
            Capability::CAP_DAC_READ_SEARCH,
        ] {
            assert!(
                !ALLOWED_CAPABILITIES.contains(&dangerous),
                "{dangerous:?} must not be retained by the container"
            );
        }
    }

    #[test]
    fn test_allowed_capabilities_include_workload_basics() {
        for needed in [
            Capability::CAP_CHOWN,
            Capability::CAP_SETUID,
            Capability::CAP_SETGID,
            Capability::CAP_NET_RAW,
        ] {
            assert!(
                ALLOWED_CAPABILITIES.contains(&needed),
                "{needed:?} is required by ordinary container workloads"
            );
        }
    }

    #[test]
    fn test_capability_allowlist_is_disjoint_from_full_set() {
        assert!(ALLOWED_CAPABILITIES.len() < caps::all().len());
    }

    #[test]
    fn test_loopback_resolvers_are_detected() {
        // systemd-resolved's stub is unreachable from inside a container.
        assert!(is_loopback_resolver("127.0.0.53"));
        assert!(is_loopback_resolver("127.0.0.1"));
        assert!(is_loopback_resolver("::1"));
        // Real upstream resolvers must be preserved.
        assert!(!is_loopback_resolver("1.1.1.1"));
        assert!(!is_loopback_resolver("8.8.8.8"));
        assert!(!is_loopback_resolver("10.0.0.53"));
        assert!(!is_loopback_resolver("192.168.1.1"));
    }

    #[test]
    fn test_resolv_conf_never_uses_loopback() {
        let content = host_resolv_conf();
        assert!(!content.is_empty());
        for line in content.lines() {
            let addr = line
                .strip_prefix("nameserver ")
                .expect("only nameserver lines");
            assert!(
                !is_loopback_resolver(addr),
                "container resolv.conf must not reference {addr}"
            );
        }
    }
}
