use crate::args::RunArgs;
use crate::net::{
    self, Ipv4Net, PortMapping, detect_bridge_subnet, parse_port_mapping, publish_port, u32_to_ip,
    unpublish_port,
};
use crate::state::{self, ContainerState, cgroup_dir_for};
use crate::utils::{
    MemoryLimit, Volume, generate_veth_names, get_nucleus_log_dir, get_nucleus_runtime_dir,
    normalize_container_path, parse_env_assignment, parse_volume, validate_container_name,
};
use anyhow::{Context, Result, bail};
use nix::sys::signal::{self, SigSet, Signal};
use nix::unistd::{Pid, pipe, write};
use std::collections::HashSet;
use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Everything needed to reclaim a container's host resources.
///
/// Held by [`CleanupGuard`] so that any early return - including error paths -
/// tears the container down.
#[derive(Debug, Clone)]
pub struct TeardownSpec {
    pub name: String,
    pub veth_host: String,
    pub container_ip: String,
    pub ports: Vec<PortMapping>,
    pub cgroup_path: String,
    pub rootless: bool,
}

impl TeardownSpec {
    /// Reconstructs a teardown spec from persisted state, so `nucleus stop` and
    /// `nucleus rm` can clean up exactly what `nucleus run` created.
    pub fn from_state(state: &ContainerState) -> Self {
        Self {
            name: state.name.clone(),
            veth_host: state.veth_host.clone(),
            container_ip: state.ip.clone(),
            ports: state.ports.clone(),
            cgroup_path: state.cgroup_dir(),
            rootless: state.rootless,
        }
    }
}

/// Runs [`teardown_container`] on drop unless disarmed.
///
/// This is what guarantees that a failure during networking or cgroup setup does
/// not leave a half-built container behind.
#[derive(Debug)]
pub struct CleanupGuard {
    spec: Option<TeardownSpec>,
}

impl CleanupGuard {
    pub fn new(spec: TeardownSpec) -> Self {
        Self { spec: Some(spec) }
    }

    /// Prevents the guard from tearing down, e.g. when ownership is transferred
    /// to a reaper process.
    pub fn disarm(&mut self) {
        self.spec = None;
    }
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        if let Some(spec) = self.spec.take() {
            // Teardown must never panic or block during unwinding.
            let _ = teardown_container(&spec, true);
        }
    }
}

/// Fully parsed and validated run specification.
///
/// Validation is separated from execution so that every malformed flag is
/// rejected before any privilege check, namespace, or host resource is touched.
#[derive(Debug, Clone)]
pub struct ParsedRun {
    pub memory: MemoryLimit,
    pub ports: Vec<PortMapping>,
    pub volumes: Vec<Volume>,
    pub cpus: Option<f64>,
    pub pids_limit: Option<u32>,
    pub subnet: Ipv4Net,
}

/// Parses and validates every user-supplied specification in `args`.
pub fn validate_run_args(args: &RunArgs) -> Result<ParsedRun> {
    validate_container_name(&args.name).context("Invalid container name")?;

    let memory = MemoryLimit::parse(&args.memory)
        .with_context(|| format!("Invalid memory limit '{}'", args.memory))?;

    if let Some(cpus) = args.cpus
        && (!cpus.is_finite() || cpus <= 0.0 || cpus > 1024.0)
    {
        bail!("--cpus must be a positive number of CPUs (got {cpus})");
    }
    if args.pids_limit == Some(0) {
        bail!("--pids-limit must be at least 1");
    }

    let ports: Vec<PortMapping> = args
        .ports
        .iter()
        .map(|p| parse_port_mapping(p))
        .collect::<Result<Vec<_>>>()
        .context("Invalid port mapping")?;

    // Reject duplicate host ports, which would silently shadow each other.
    let mut seen_ports: HashSet<(Option<String>, u16, String)> = HashSet::new();
    for mapping in &ports {
        let key = (
            mapping.host_ip.clone(),
            mapping.host_port,
            mapping.protocol.clone(),
        );
        if !seen_ports.insert(key) {
            bail!("duplicate port mapping '{}'", mapping);
        }
    }

    if !ports.is_empty() && args.rootless {
        bail!("Port mapping is not supported in rootless mode: no network namespace is attached");
    }

    let volumes = args
        .volumes
        .iter()
        .map(|v| parse_volume(v))
        .collect::<Result<Vec<_>>>()
        .context("Invalid volume specification")?;

    if let Some(workdir) = &args.workdir {
        normalize_container_path(workdir)
            .with_context(|| format!("Invalid working directory '{workdir}'"))?;
    }

    for assignment in &args.env {
        parse_env_assignment(assignment)
            .with_context(|| format!("Invalid environment variable '{assignment}'"))?;
    }

    let subnet = Ipv4Net::parse_cidr(&args.subnet)
        .with_context(|| format!("Invalid subnet '{}'", args.subnet))?;

    Ok(ParsedRun {
        memory,
        ports,
        volumes,
        cpus: args.cpus,
        pids_limit: args.pids_limit,
        subnet,
    })
}

/// Parent Orchestrator: Sets up host networking, resource limits, and manages the child process.
pub fn run_parent_orchestrator(args: RunArgs, parsed: ParsedRun) -> Result<()> {
    let ParsedRun {
        memory,
        ports,
        volumes,
        cpus,
        pids_limit,
        subnet: requested_subnet,
    } = parsed;

    if state::get_container_state(&args.name)?.is_some() {
        bail!(
            "container '{}' is already running; stop it first or choose another name",
            args.name
        );
    }

    println!(
        "[Nucleus] Initializing orchestration for '{}'...",
        args.name
    );

    // 1. Host networking: resolve the bridge and its subnet before IPAM so
    // allocation and routing agree on the same network.
    let mut subnet = requested_subnet;

    if !args.rootless {
        net::ensure_bridge(&args.network, &subnet)
            .with_context(|| format!("Failed to set up bridge '{}'", args.network))?;
        // An existing bridge keeps its own address, which is authoritative.
        if let Some(detected) = detect_bridge_subnet(&args.network) {
            subnet = detected;
        }
    }

    // 2. IPAM: Determine IP
    let container_ip = match &args.ip {
        Some(ip) => {
            let requested = ip.trim();
            let parsed_ip = net::parse_ipv4(requested)
                .with_context(|| format!("Invalid --ip '{requested}'"))?;
            if !subnet.contains(parsed_ip) {
                bail!(
                    "--ip '{requested}' is outside the container subnet {}",
                    subnet.cidr()
                );
            }
            if parsed_ip == subnet.network() || parsed_ip == subnet.broadcast() {
                bail!("--ip '{requested}' is not a valid host address");
            }
            if parsed_ip == subnet.gateway() {
                bail!("--ip '{requested}' conflicts with the bridge gateway address");
            }
            if is_ip_in_use(requested)? {
                bail!("--ip '{requested}' is already assigned to a running container");
            }
            requested.to_string()
        }
        None => allocate_ip(&subnet).context("Failed to auto-allocate IP")?,
    };
    println!(
        "[Nucleus] Assigned IP: {} on {}",
        container_ip,
        subnet.cidr()
    );

    // 3. Sync Pipe
    let (reader, writer) = pipe().context("Failed to create sync pipe")?;

    let (v_host, v_child) = generate_veth_names(&args.name);
    let cgroup_path = cgroup_dir_for(&args.name);

    // From here on, any failure tears down whatever was created.
    let spec = TeardownSpec {
        name: args.name.clone(),
        veth_host: v_host.clone(),
        container_ip: container_ip.clone(),
        ports: ports.clone(),
        cgroup_path: cgroup_path.clone(),
        rootless: args.rootless,
    };
    let mut guard = CleanupGuard::new(spec.clone());

    // 4. Spawn Child
    let mut child_cmd = Command::new("/proc/self/exe");
    child_cmd
        .arg("internal-child")
        .arg("--image")
        .arg(&args.image)
        .arg("--name")
        .arg(&args.name)
        .arg("--ip")
        .arg(&container_ip)
        .arg("--pipe-fd")
        .arg(reader.to_string())
        .arg("--memory")
        .arg(&args.memory)
        .arg("--network")
        .arg(&args.network);

    if args.rootless {
        child_cmd.arg("--rootless");
    }
    if args.readonly {
        child_cmd.arg("--readonly");
    }
    if args.tty {
        child_cmd.arg("--tty");
    }
    for vol in &volumes {
        // Pass canonical, already-validated specs to the child.
        let mode = if vol.read_only { ":ro" } else { "" };
        child_cmd
            .arg("--volumes")
            .arg(format!("{}:{}{}", vol.source, vol.destination, mode));
    }
    for env in &args.env {
        child_cmd.arg("--env").arg(env);
    }
    if let Some(wd) = &args.workdir {
        child_cmd.arg("--workdir").arg(wd);
    }
    for port in &ports {
        child_cmd.arg("--ports").arg(port.to_string());
    }
    child_cmd.args(&args.command);

    let (stdout, stderr) = if args.detach {
        let log_dir = get_nucleus_log_dir();
        fs::create_dir_all(&log_dir).context("Failed to create log directory")?;
        let log_path = log_dir.join(format!("{}.log", args.name));
        let log_file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .context("Failed to open log file")?;
        let err_file = log_file
            .try_clone()
            .context("Failed to clone log file handle")?;
        (Stdio::from(log_file), Stdio::from(err_file))
    } else {
        (Stdio::inherit(), Stdio::inherit())
    };

    let mut child = child_cmd
        .stdin(if args.interactive || !args.detach {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .context("Failed to spawn child process")?;

    let pid = child.id() as i32;

    // 5. Networking: attach the container end of the veth pair.
    if !args.rootless {
        let pid_str = pid.to_string();
        crate::utils::run_command(
            "ip",
            &[
                "link", "add", &v_host, "type", "veth", "peer", "name", &v_child,
            ],
        )?;
        crate::utils::run_command("ip", &["link", "set", &v_child, "netns", &pid_str])?;
        crate::utils::run_command("ip", &["link", "set", &v_host, "master", &args.network])?;
        crate::utils::run_command("ip", &["link", "set", &v_host, "up"])?;

        configure_container_network(&pid_str, &v_child, &container_ip, &subnet)?;
    }

    // 6. Resource limits (Cgroups v2)
    if !args.rootless {
        apply_resource_limits(&cgroup_path, pid, memory, cpus, pids_limit)?;
    }

    // 7. Port publishing
    if !args.rootless {
        net::enable_ip_forwarding()?;
        net::ensure_host_nat(&subnet, &args.network)?;
        for mapping in &ports {
            publish_port(mapping, &container_ip)
                .with_context(|| format!("Failed to publish port mapping '{mapping}'"))?;
        }
    }

    // 8. Release the child, which has been waiting on the sync pipe.
    write(writer, b"done").context("Failed to signal container readiness")?;
    println!("[Nucleus] Network links established. Handing over control.");

    // Record state only once the host side is fully wired up, so a listed
    // container is always a container that works.
    let started_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    state::save_state(&ContainerState {
        name: args.name.clone(),
        pid,
        ip: container_ip.clone(),
        gateway: subnet.gateway_str(),
        subnet: subnet.cidr(),
        network: args.network.clone(),
        veth_host: v_host.clone(),
        status: "Running".to_string(),
        ports: ports.clone(),
        image: args.image.clone(),
        command: args.command.clone(),
        volumes: args.volumes.clone(),
        memory: memory.cgroup_value(),
        cpus: args.cpus,
        pids_limit: args.pids_limit,
        rootless: args.rootless,
        readonly: args.readonly,
        init_pid: discover_init_pid(pid),
        cgroup_path: cgroup_path.clone(),
        started_at,
    })?;

    if args.detach {
        // Hand ownership to a reaper so resources are reclaimed when the
        // container exits, even though this process is about to return.
        spawn_reaper(&args.name, pid)?;
        guard.disarm();

        println!(
            "[Nucleus] Container '{}' started in detached mode (PID: {}).",
            args.name, pid
        );
        println!(
            "[Nucleus] Logs: {}",
            get_nucleus_log_dir()
                .join(format!("{}.log", args.name))
                .display()
        );
        return Ok(());
    }

    // Foreground: forward termination signals to the container.
    let init_pid = state::get_container_state(&args.name)
        .ok()
        .flatten()
        .and_then(|s| s.init_pid);
    forward_signals(pid, init_pid);

    let status = child.wait().context("Container process failed")?;
    guard.disarm();
    teardown_container(&spec, true)?;

    println!(
        "[Nucleus] Container '{}' terminated (Status: {}).",
        args.name, status
    );
    Ok(())
}

/// Configures the loopback and veth interface inside the container's netns.
fn configure_container_network(
    pid: &str,
    v_child: &str,
    container_ip: &str,
    subnet: &Ipv4Net,
) -> Result<()> {
    // Rename the peer to eth0 and assign the address.
    let addr = format!("{container_ip}/{}", subnet.prefix());
    let gateway = subnet.gateway_str();

    for args in [
        vec!["link", "set", v_child, "name", "eth0"],
        vec!["addr", "add", &addr, "dev", "eth0"],
        vec!["link", "set", "eth0", "up"],
        vec!["link", "set", "lo", "up"],
        vec!["route", "add", "default", "via", &gateway],
    ] {
        let mut full = vec!["-t", pid, "-n", "ip"];
        full.extend_from_slice(&args);
        crate::utils::run_command("nsenter", &full).with_context(|| {
            format!(
                "Failed to configure container network: ip {}",
                args.join(" ")
            )
        })?;
    }
    Ok(())
}

/// Creates the Nucleus cgroup subtree and applies the requested limits.
fn apply_resource_limits(
    cgroup_path: &str,
    pid: i32,
    memory: MemoryLimit,
    cpus: Option<f64>,
    pids_limit: Option<u32>,
) -> Result<()> {
    use std::io::Write;

    // Delegate the controllers we manage from the root cgroup down to our
    // subtree, then onward to each container.
    for controller in ["+memory", "+cpu", "+pids"] {
        let _ = fs::OpenOptions::new()
            .write(true)
            .open("/sys/fs/cgroup/cgroup.subtree_control")
            .and_then(|mut f| f.write_all(controller.as_bytes()));
    }

    fs::create_dir_all(state::CGROUP_ROOT).context("Failed to create Nucleus cgroup subtree")?;
    for controller in ["+memory", "+cpu", "+pids"] {
        let path = format!("{}/cgroup.subtree_control", state::CGROUP_ROOT);
        let _ = fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .and_then(|mut f| f.write_all(controller.as_bytes()));
    }

    fs::create_dir_all(cgroup_path).context("Failed to create cgroup dir")?;

    let write_limit = |file: &str, value: &str| -> Result<()> {
        let path = format!("{cgroup_path}/{file}");
        fs::write(&path, value).with_context(|| format!("Failed to write {file} = '{value}'"))
    };

    write_limit("memory.max", &memory.cgroup_value())?;
    // Without a swap cap the memory limit is trivially bypassable.
    write_limit("memory.swap.max", &memory.cgroup_value())?;

    // cpu.max is "<quota> <period>" in microseconds. Honour --cpus; without it
    // fall back to a single CPU rather than the old hardcoded value.
    let (quota, period) = match cpus {
        Some(c) => {
            let quota = (c * period_usecs() as f64).round() as u64;
            (quota.max(1000).to_string(), period_usecs().to_string())
        }
        None => (period_usecs().to_string(), period_usecs().to_string()),
    };
    write_limit("cpu.max", &format!("{quota} {period}"))?;

    match pids_limit {
        Some(limit) => write_limit("pids.max", &limit.to_string())?,
        None => write_limit("pids.max", "max")?,
    }

    fs::write(format!("{cgroup_path}/cgroup.procs"), pid.to_string())
        .context("Failed to move container process into its cgroup")?;
    Ok(())
}

fn period_usecs() -> u64 {
    100_000
}

/// Forwards SIGINT/SIGTERM from the host to the container.
///
/// Both the supervisor and the container's init are signalled: the supervisor
/// so it can relay the exit status, and the init because a PID namespace's init
/// is what the user actually wants to talk to.
fn forward_signals(supervisor_pid: i32, init_pid: Option<i32>) {
    let mut sigset = SigSet::empty();
    sigset.add(Signal::SIGINT);
    sigset.add(Signal::SIGTERM);
    // Blocking happens on this thread; a spawned thread inherits the mask, so
    // sigwait() in that thread receives the signals.
    if sigset.thread_block().is_err() {
        eprintln!("[Nucleus] Warning: could not block signals for forwarding");
        return;
    }

    let supervisor = Pid::from_raw(supervisor_pid);
    std::thread::spawn(move || {
        while let Ok(sig) = sigset.wait() {
            if let Some(init) = init_pid.filter(|pid| *pid > 1) {
                let _ = signal_container_init(init, sig);
            }
            let _ = signal::kill(supervisor, sig);
            if matches!(sig, Signal::SIGTERM | Signal::SIGINT) {
                break;
            }
        }
    });
}

/// Starts a detached reaper that tears the container down when it exits.
fn spawn_reaper(name: &str, pid: i32) -> Result<()> {
    let status = Command::new("/proc/self/exe")
        .arg("internal-reaper")
        .arg("--name")
        .arg(name)
        .arg("--pid")
        .arg(pid.to_string())
        // Detach from the caller's stdio so the reaper outlives this process.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("Failed to spawn container reaper")?;
    // Dropping the handle leaves the reaper running; it is reparented to init
    // when this process exits.
    drop(status);
    Ok(())
}

/// Reaps a detached container and reclaims its host resources.
pub fn run_reaper(name: &str, pid: i32) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(24 * 60 * 60);

    // Poll for the container supervisor to finish. `process_alive` is used
    // rather than a bare `kill(pid, 0)` because a zombie still answers that
    // call, which would leave the reaper running long after the exit.
    while Instant::now() < deadline {
        if !process_alive(pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    if let Some(state) = state::get_container_state(name)? {
        let spec = TeardownSpec::from_state(&state);
        teardown_container(&spec, true)?;
    }
    Ok(())
}

/// Releases every host resource associated with a container.
///
/// `quiet` suppresses progress output, which is desirable when running from a
/// drop guard.
pub fn teardown_container(spec: &TeardownSpec, quiet: bool) -> Result<()> {
    if !quiet {
        println!("[Nucleus] Cleaning up resources for '{}'...", spec.name);
    }

    // State first, so a partially completed teardown is never re-listed.
    let _ = state::remove_state(&spec.name);

    if !spec.rootless {
        // Kill anything still running in the cgroup, then remove the directory.
        // cgroupfs entries cannot be unlinked, so rmdir (not remove_dir_all) is
        // the only way to remove one.
        remove_cgroup(&spec.cgroup_path);

        crate::utils::try_command("ip", &["link", "delete", &spec.veth_host]);

        for mapping in &spec.ports {
            unpublish_port(mapping, &spec.container_ip);
        }
    }

    // OverlayFS upper/work/merged and any legacy temp directory. The helper is
    // used rather than fs::remove_dir_all because OverlayFS leaves an internal
    // work directory with mode 000, which the standard call cannot enumerate.
    let runtime_dir = get_nucleus_runtime_dir();
    let container_dir = runtime_dir.join("containers").join(&spec.name);
    if let Err(e) = crate::utils::force_remove_dir_all(&container_dir) {
        eprintln!(
            "[Nucleus] Warning: could not remove {}: {e}",
            container_dir.display()
        );
    }
    let _ = crate::utils::force_remove_dir_all(
        std::path::Path::new("./temp").join(&spec.name).as_path(),
    );

    if !quiet {
        println!("[Nucleus] Cleanup complete for '{}'.", spec.name);
    }
    Ok(())
}

/// Removes a cgroup directory, killing its processes first.
///
/// cgroup v2 forbids rmdir while tasks remain, and unlike a normal directory
/// its pseudo-files cannot be unlinked, so this must use rmdir on an empty
/// cgroup rather than `remove_dir_all`.
pub fn remove_cgroup(path: &str) {
    if !std::path::Path::new(path).exists() {
        return;
    }

    // cgroup.kill terminates every task recursively (kernel 5.14+). When it is
    // unavailable, fall back to migrating the processes to the parent cgroup.
    if fs::write(format!("{path}/cgroup.kill"), "1").is_err() {
        let parent_procs = std::path::Path::new(path)
            .parent()
            .map(|p| p.join("cgroup.procs"));
        if let (Ok(procs), Some(parent)) = (
            fs::read_to_string(format!("{path}/cgroup.procs")),
            parent_procs,
        ) {
            for pid in procs.lines().filter(|l| !l.trim().is_empty()) {
                let _ = fs::write(&parent, pid.trim());
            }
        }
    }

    let mut delay = Duration::from_millis(10);
    for _ in 0..12 {
        match fs::remove_dir(path) {
            Ok(()) => return,
            Err(_) => std::thread::sleep(delay),
        }
        delay = (delay * 2).min(Duration::from_millis(250));
    }
}

/// Delivers a signal to the container's PID 1 from outside its namespace.
///
/// The kernel ignores signals sent to a PID namespace's init from *within* that
/// namespace unless init has installed a handler for them (see
/// `man 7 pid_namespaces`). Signalling by host-visible PID works because we
/// are in an ancestor namespace, where SIGKILL is forcibly delivered even to an
/// init that ignores it.
///
/// `init_pid` is deliberately not usable for any PID other than the container's
/// init, and the value is validated to be neither 0, 1, nor this process.
fn signal_container_init(init_pid: i32, sig: Signal) -> bool {
    if init_pid <= 1 || init_pid == std::process::id() as i32 {
        return false;
    }
    signal::kill(Pid::from_raw(init_pid), sig).is_ok()
}

/// Discovers the host-visible PID of the container's PID 1.
///
/// The supervisor calls `unshare(CLONE_NEWPID)` and then forks, so the new
/// namespace's init is the supervisor's only child. The kernel exposes it via
/// `/proc/<pid>/task/<tid>/children`.
fn discover_init_pid(supervisor_pid: i32) -> Option<i32> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if let Ok(tasks) = fs::read_dir(format!("/proc/{supervisor_pid}/task")) {
            for task in tasks.flatten() {
                let Ok(children) = fs::read_to_string(task.path().join("children")) else {
                    continue;
                };
                for token in children.split_whitespace() {
                    if let Ok(pid) = token.parse::<i32>()
                        && pid > 1
                        && pid != supervisor_pid
                    {
                        return Some(pid);
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

/// Returns true when `pid` is still running.
///
/// A zombie still answers `kill(pid, 0)`, so the process state is inspected to
/// avoid treating an already-exited container as alive.
fn process_alive(pid: i32) -> bool {
    signal::kill(Pid::from_raw(pid), None).is_ok() && !is_zombie(pid)
}

/// Reads the single-letter process state from `/proc/<pid>/stat`.
fn is_zombie(pid: i32) -> bool {
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        // The process is gone entirely.
        return true;
    };
    // The comm field may contain spaces and parentheses, so parse after the
    // final ')'.
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .is_some_and(|state| state == "Z")
}

/// Polls until `pid` is gone or `timeout` seconds elapse.
fn wait_for_exit(pid: i32, timeout: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        if !process_alive(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Stops a running container: SIGTERM, wait up to `timeout`, then SIGKILL.
///
/// Host resources are always reclaimed, whether the container exits gracefully
/// or has to be killed.
pub fn stop_container(state: &ContainerState, timeout: u64) -> Result<()> {
    let supervisor = Pid::from_raw(state.pid);
    let init_pid = state.init_pid.filter(|pid| *pid > 1);

    println!(
        "[Nucleus] Stopping container '{}' (PID {})...",
        state.name, state.pid
    );

    // The container's own init defines whether the container is still running;
    // the supervisor only relays its exit status. Signalling the supervisor is
    // never sufficient, because that orphans the container.
    let target = init_pid.unwrap_or(state.pid);

    if !process_alive(target) && !process_alive(state.pid) {
        let spec = TeardownSpec::from_state(state);
        teardown_container(&spec, false)?;
        println!("[Nucleus] Container process already exited.");
        return Ok(());
    }

    // A container whose init has already gone counts as having stopped.
    let mut graceful = !process_alive(target);

    if !graceful {
        let _ = signal_container_init(target, Signal::SIGTERM);
        graceful = wait_for_exit(target, timeout);

        if !graceful {
            println!(
                "[Nucleus] Container '{}' did not exit within {}s; sending SIGKILL.",
                state.name, timeout
            );
            let _ = signal_container_init(target, Signal::SIGKILL);
            // Let the kernel tear the process down before the cgroup and network
            // namespace are removed.
            let _ = wait_for_exit(target, 5);
        }
    }

    // The supervisor should follow its child out; give it a moment, then insist
    // so it cannot linger as a zombie.
    if process_alive(state.pid) {
        let _ = wait_for_exit(state.pid, 2);
        if process_alive(state.pid) {
            let _ = signal::kill(supervisor, Signal::SIGKILL);
            let _ = wait_for_exit(state.pid, 5);
        }
    }

    // Final safety net: an init that outlived the supervisor would keep the
    // container's namespaces alive.
    if let Some(init) = init_pid
        && process_alive(init)
    {
        let _ = signal_container_init(init, Signal::SIGKILL);
        let _ = wait_for_exit(init, 5);
    }

    let spec = TeardownSpec::from_state(state);
    teardown_container(&spec, false)?;

    if graceful {
        println!("[Nucleus] Container '{}' stopped.", state.name);
    } else {
        println!("[Nucleus] Container '{}' killed.", state.name);
    }
    Ok(())
}

/// Returns true when the given IP belongs to a running container.
fn is_ip_in_use(ip: &str) -> Result<bool> {
    Ok(state::list_containers()?.iter().any(|c| c.ip == ip))
}

/// Allocates the lowest free host address in the subnet.
fn allocate_ip(subnet: &Ipv4Net) -> Result<String> {
    let used: HashSet<String> = state::list_containers()?
        .into_iter()
        .map(|c| c.ip)
        .collect();

    let gateway = subnet.gateway();
    for candidate in subnet.first_host()..=subnet.last_host() {
        if candidate == gateway {
            continue;
        }
        let ip = u32_to_ip(candidate);
        if !used.contains(&ip) {
            return Ok(ip);
        }
    }
    bail!("No available IP addresses in subnet {}", subnet.cidr())
}
