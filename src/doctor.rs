//! Host capability probing for `nucleus info`.
//!
//! Nucleus is built directly on kernel primitives, so whether it can run at all
//! depends on the host: cgroup v2, OverlayFS, user namespaces, seccomp and
//! `CAP_SYS_ADMIN` are all required for some subset of its features. This module
//! reports what is actually available rather than failing later with an opaque
//! error.

use crate::utils::{get_nucleus_data_dir, get_nucleus_log_dir, get_nucleus_runtime_dir};
use std::fmt;
use std::fs;
use std::path::Path;
use std::process::Command;

/// Severity of a single probe result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    /// Informational, nothing to act on.
    Info,
    /// Works, but a capability is missing or degraded.
    Warn,
    /// Broken; the corresponding feature cannot work.
    Fail,
}

impl Status {
    fn label(&self) -> &'static str {
        match self {
            Status::Info => "INFO",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
        }
    }
}

/// One probe result.
#[derive(Debug, Clone)]
pub struct Probe {
    pub name: String,
    pub status: Status,
    pub detail: String,
}

impl Probe {
    fn info(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            status: Status::Info,
            detail: detail.into(),
        }
    }
    fn warn(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            status: Status::Warn,
            detail: detail.into(),
        }
    }
    fn fail(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            status: Status::Fail,
            detail: detail.into(),
        }
    }
    fn ok(name: &str, detail: impl Into<String>) -> Self {
        // A satisfied requirement is informational, not a warning.
        Self {
            name: name.to_string(),
            status: Status::Info,
            detail: detail.into(),
        }
    }
}

/// Capability flags gathered by the probes.
///
/// Availability is computed from these rather than by looking probes up by
/// name, which silently reported rootless as unavailable when a probe label
/// was renamed.
#[derive(Debug, Default, Clone, Copy)]
struct Findings {
    cgroup_v2: bool,
    cgroup_writable: bool,
    overlay: bool,
    userns: bool,
    seccomp: bool,
    iptables: bool,
    iproute2: bool,
    nsenter: bool,
    ip_forward: bool,
}

/// The full report.
pub struct Report {
    pub probes: Vec<Probe>,
    /// Whether privileged mode (root, networking, cgroups) looks usable.
    pub privileged_ok: bool,
    /// Whether `--rootless` looks usable.
    pub rootless_ok: bool,
}

impl Report {
    fn push(&mut self, probe: Probe) {
        self.probes.push(probe);
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let width = self
            .probes
            .iter()
            .map(|p| p.name.len())
            .max()
            .unwrap_or(4)
            .clamp(4, 28);

        writeln!(f, "Nucleus host report")?;
        writeln!(f, "{}", "-".repeat(width + 30))?;
        for probe in &self.probes {
            writeln!(
                f,
                "{:<width$}  {:<4}  {}",
                probe.name,
                probe.status.label(),
                probe.detail,
                width = width
            )?;
        }
        writeln!(f, "{}", "-".repeat(width + 30))?;
        writeln!(f, "Privileged mode: {}", verdict(self.privileged_ok))?;
        writeln!(f, "Rootless mode:   {}", verdict(self.rootless_ok))?;
        Ok(())
    }
}

fn verdict(ok: bool) -> &'static str {
    if ok { "available" } else { "unavailable" }
}

/// Runs every probe and builds the report.
pub fn collect() -> Report {
    let mut report = Report {
        probes: Vec::new(),
        privileged_ok: false,
        rootless_ok: false,
    };
    let mut found = Findings::default();

    probe_kernel(&mut report);
    probe_identity(&mut report);
    probe_cgroups(&mut report, &mut found);
    probe_overlayfs(&mut report, &mut found);
    probe_userns(&mut report, &mut found);
    probe_seccomp(&mut report, &mut found);
    probe_netfilter(&mut report, &mut found);
    probe_tools(&mut report);
    probe_sysctls(&mut report, &mut found);
    probe_paths(&mut report);
    probe_images(&mut report);

    // Rootless needs namespaces, a way to build the rootfs and a way to sandbox
    // the result. Networking and cgroup limits are not available in that mode.
    report.rootless_ok = found.userns && found.overlay && found.seccomp;

    // Privileged mode additionally needs cgroup v2, a writable cgroup tree and
    // netfilter for bridge networking.
    report.privileged_ok = is_root()
        && found.cgroup_v2
        && found.cgroup_writable
        && found.overlay
        && found.seccomp
        && found.iptables
        && found.iproute2
        && found.nsenter
        && found.ip_forward;

    report
}

fn is_root() -> bool {
    nix::unistd::getuid().is_root()
}

fn read_trim(path: &str) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn probe_kernel(report: &mut Report) {
    let release = read_trim("/proc/sys/kernel/osrelease").unwrap_or_else(|| "unknown".into());
    let version = read_trim("/proc/version").unwrap_or_default();

    // OverlayFS and cgroup v2 both want a reasonably modern kernel.
    let major_minor: Vec<u32> = release
        .split(['.', '-'])
        .take(2)
        .filter_map(|p| p.parse().ok())
        .collect();
    let modern = major_minor.first().is_some_and(|maj| {
        *maj > 4 || (*maj == 4 && major_minor.get(1).is_some_and(|min| *min >= 18))
    });

    if modern {
        report.push(Probe::ok("kernel", release.clone()));
    } else {
        report.push(Probe::warn(
            "kernel",
            format!("{release} is older than the recommended 4.18 for cgroup v2"),
        ));
    }
    if !version.is_empty() {
        report.push(Probe::info("kernel build", version));
    }
}

fn probe_identity(report: &mut Report) {
    if is_root() {
        report.push(Probe::ok("identity", "running as root (uid 0)"));
    } else {
        report.push(Probe::info(
            "identity",
            format!(
                "running as uid {} (privileged mode needs root; use --rootless)",
                nix::unistd::getuid()
            ),
        ));
    }

    let cap_eff = read_trim("/proc/self/status")
        .and_then(|status| {
            status
                .lines()
                .find(|l| l.starts_with("CapEff:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "0".into());

    if let Ok(bits) = u64::from_str_radix(&cap_eff, 16) {
        let mut names = Vec::new();
        // Bit 21 is CAP_SYS_ADMIN, the one that gates namespace and mount work.
        if bits & (1 << 21) != 0 {
            names.push("CAP_SYS_ADMIN");
        }
        if bits & (1 << 12) != 0 {
            names.push("CAP_NET_ADMIN");
        }
        let detail = if names.is_empty() {
            format!("effective capabilities 0x{cap_eff} (no CAP_SYS_ADMIN)")
        } else {
            format!("effective capabilities 0x{cap_eff} ({})", names.join(", "))
        };
        report.push(Probe::info("capabilities", detail));
    }
}

fn probe_cgroups(report: &mut Report, found: &mut Findings) {
    let controllers_file = Path::new("/sys/fs/cgroup/cgroup.controllers");
    if !controllers_file.exists() {
        report.push(Probe::fail(
            "cgroup v2",
            "not mounted at /sys/fs/cgroup (expected cgroup2); memory/cpu/pid limits are unavailable",
        ));
        return;
    }

    let controllers = read_trim("/sys/fs/cgroup/cgroup.controllers").unwrap_or_default();
    let available: Vec<&str> = controllers.split_whitespace().collect();

    // Nucleus needs these three delegated from the root cgroup.
    let required = ["memory", "cpu", "pids"];
    let missing: Vec<&&str> = required.iter().filter(|c| !available.contains(c)).collect();

    found.cgroup_v2 = missing.is_empty();

    if missing.is_empty() {
        report.push(Probe::ok(
            "cgroup v2",
            format!("mounted; controllers: {}", available.join(", ")),
        ));
    } else {
        report.push(Probe::warn(
            "cgroup v2",
            format!(
                "mounted but missing controller(s): {}; the matching limits will fail",
                missing.iter().map(|c| **c).collect::<Vec<_>>().join(", ")
            ),
        ));
    }

    // Can we actually create a cgroup? This needs a writable cgroup2 mount.
    let probe_dir = Path::new("/sys/fs/cgroup").join("nucleus-info-probe");
    match fs::create_dir(&probe_dir) {
        Ok(()) => {
            let _ = fs::remove_dir(&probe_dir);
            found.cgroup_writable = true;
            report.push(Probe::ok("cgroup writable", "yes"));
        }
        Err(e) => report.push(Probe::warn(
            "cgroup writable",
            format!("no ({e}); container limits cannot be applied"),
        )),
    }

    if Path::new(crate::state::CGROUP_ROOT).exists() {
        report.push(Probe::info(
            "cgroup subtree",
            format!("{} exists", crate::state::CGROUP_ROOT),
        ));
    }
}

fn probe_overlayfs(report: &mut Report, found: &mut Findings) {
    let filesystems = read_trim("/proc/filesystems").unwrap_or_default();
    // Entries look like: "nodev\toverlay".
    let supported = filesystems
        .lines()
        .any(|line| line.split_whitespace().last() == Some("overlay"));

    found.overlay = supported;

    if supported {
        report.push(Probe::ok("OverlayFS", "available in /proc/filesystems"));
    } else {
        report.push(Probe::fail(
            "OverlayFS",
            "not listed in /proc/filesystems; the root filesystem cannot be assembled",
        ));
    }

    // OverlayFS on top of some network filesystems is unsupported, and the
    // upper and work directories must share a filesystem.
    let data = get_nucleus_data_dir();
    report.push(Probe::info(
        "overlay storage",
        format!("upper/work layers under {}", data.display()),
    ));
}

fn probe_userns(report: &mut Report, found: &mut Findings) {
    // Debian/Ubuntu gate unprivileged namespaces behind this knob.
    let clone_sysctl = read_trim("/proc/sys/kernel/unprivileged_userns_clone");
    let max_ns = read_trim("/proc/sys/user/max_user_namespaces");

    let mut blocked = false;
    if let Some(value) = &clone_sysctl
        && value == "0"
    {
        blocked = true;
    }
    if let Some(value) = &max_ns
        && value == "0"
    {
        blocked = true;
    }

    // A cheap empirical check: can this process create a user namespace at all?
    let can_unshare = probe_unshare_user();

    found.userns = can_unshare;

    if can_unshare {
        report.push(Probe::ok(
            "user namespaces",
            "unprivileged user namespaces can be created",
        ));
    } else if blocked {
        report.push(Probe::fail(
            "user namespaces",
            "disabled by sysctl (unprivileged_userns_clone=0 or max_user_namespaces=0); \
             rootless mode is unavailable",
        ));
    } else {
        report.push(Probe::warn(
            "user namespaces",
            "could not create a user namespace; rootless mode may be unavailable \
             (a seccomp or AppArmor profile may block unshare)",
        ));
    }

    if let Some(value) = clone_sysctl {
        report.push(Probe::info("unprivileged_userns_clone", value));
    }
    if let Some(value) = max_ns {
        report.push(Probe::info("max_user_namespaces", value));
    }
}

/// Attempts `unshare(CLONE_NEWUSER)` in a throwaway child process.
///
/// The call is irreversible for the calling process, so it must happen in a
/// child that exits immediately either way.
fn probe_unshare_user() -> bool {
    use nix::sched::{CloneFlags, unshare};
    use nix::sys::wait::{WaitStatus, waitpid};
    use nix::unistd::{ForkResult, fork};

    // SAFETY: fork() is safe here because `nucleus info` is single-threaded.
    match unsafe { fork() } {
        Ok(ForkResult::Parent { child }) => {
            matches!(waitpid(child, None), Ok(WaitStatus::Exited(_, 0)))
        }
        Ok(ForkResult::Child) => {
            let ok = unshare(CloneFlags::CLONE_NEWUSER).is_ok();
            std::process::exit(if ok { 0 } else { 1 });
        }
        Err(_) => false,
    }
}

fn probe_seccomp(report: &mut Report, found: &mut Findings) {
    let actions = read_trim("/proc/sys/kernel/seccomp/actions_avail");
    match actions {
        Some(actions) => report.push(Probe::info("seccomp support", actions)),
        None => report.push(Probe::fail(
            "seccomp support",
            "/proc/sys/kernel/seccomp/actions_avail is missing; the kernel lacks seccomp",
        )),
    }

    // The real question is whether a filter can actually be loaded, which is
    // only answerable by trying. Again in a child, because it cannot be undone.
    found.seccomp = probe_seccomp_load();

    if found.seccomp {
        report.push(Probe::ok(
            "seccomp filter",
            "a filter can be loaded into this process",
        ));
    } else {
        report.push(Probe::fail(
            "seccomp filter",
            "loading a filter was refused; container sandboxing is unavailable",
        ));
    }
}

/// Tries to load a trivial seccomp filter in a child process.
fn probe_seccomp_load() -> bool {
    use nix::unistd::{ForkResult, fork};

    // SAFETY: fork() is safe here because `nucleus info` is single-threaded.
    match unsafe { fork() } {
        Ok(ForkResult::Parent { child }) => {
            let status = nix::sys::wait::waitpid(child, None).ok();
            matches!(status, Some(nix::sys::wait::WaitStatus::Exited(_, 0)))
        }
        Ok(ForkResult::Child) => {
            use libseccomp::{ScmpAction, ScmpFilterContext};
            let ok = ScmpFilterContext::new_filter(ScmpAction::Allow)
                .and_then(|f| f.load())
                .is_ok();
            std::process::exit(if ok { 0 } else { 1 });
        }
        Err(_) => false,
    }
}

fn probe_netfilter(report: &mut Report, found: &mut Findings) {
    let iptables = which("iptables");
    match iptables {
        None => {
            report.push(Probe::fail(
                "iptables",
                "not found in PATH; port mapping and NAT are unavailable",
            ));
        }
        Some(path) => {
            found.iptables = true;
            let version = Command::new(&path)
                .arg("--version")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();

            // Report the backend, because nft and legacy behave differently.
            let backend = if version.contains("nf_tables") {
                "nftables backend"
            } else if version.contains("legacy") {
                "legacy backend"
            } else {
                "unknown backend"
            };
            report.push(Probe::ok("iptables", format!("{version} ({backend})")));

            // Our own chains, so an operator can see what Nucleus installed.
            for (table, chain) in crate::net::OWNED_CHAINS {
                if crate::net::chain_exists(table, chain) {
                    report.push(Probe::info(
                        "nucleus chains",
                        format!("{table}/{chain} exists"),
                    ));
                } else {
                    report.push(Probe::info(
                        "nucleus chains",
                        format!("{table}/{chain} not created yet (created on first run)"),
                    ));
                }
            }
        }
    }

    if let Some(path) = which("ip") {
        found.iproute2 = true;
        report.push(Probe::ok("iproute2", path));
    } else {
        report.push(Probe::fail(
            "iproute2",
            "'ip' not found in PATH; bridge and veth setup will fail",
        ));
    }

    if let Some(path) = which("nsenter") {
        found.nsenter = true;
        report.push(Probe::ok("nsenter", path));
    } else {
        report.push(Probe::warn(
            "nsenter",
            "'nsenter' not found in PATH; `nucleus exec` will not work",
        ));
    }
}

fn probe_tools(report: &mut Report) {
    for (label, binary, required) in [("tail", "tail", false), ("sh", "sh", true)] {
        match which(binary) {
            Some(path) => report.push(Probe::ok(label, path)),
            None if required => {
                report.push(Probe::fail(label, format!("'{binary}' not found in PATH")))
            }
            None => report.push(Probe::warn(
                label,
                format!("'{binary}' not found in PATH; `nucleus logs --follow` will not work"),
            )),
        }
    }
}

fn probe_sysctls(report: &mut Report, found: &mut Findings) {
    // Needed for container egress. Read-only /proc/sys is common under hardening.
    match fs::OpenOptions::new()
        .write(true)
        .open("/proc/sys/net/ipv4/ip_forward")
    {
        Ok(_) => {
            found.ip_forward = true;
            report.push(Probe::ok(
                "ip_forward",
                "writable; container egress can be enabled",
            ));
        }
        Err(e) => report.push(Probe::warn(
            "ip_forward",
            format!("not writable ({e}); outbound networking may fail"),
        )),
    }
}

fn probe_paths(report: &mut Report) {
    for (label, path) in [
        ("data dir", get_nucleus_data_dir()),
        ("runtime dir", get_nucleus_runtime_dir()),
        ("log dir", get_nucleus_log_dir()),
    ] {
        let exists = path.exists();
        let writable = !exists || fs::metadata(&path).is_ok_and(|m| !m.permissions().readonly());

        if exists && writable {
            report.push(Probe::ok(label, path.display().to_string()));
        } else if exists {
            report.push(Probe::warn(
                label,
                format!("{} exists but is not writable", path.display()),
            ));
        } else {
            report.push(Probe::info(
                label,
                format!("{} (will be created)", path.display()),
            ));
        }
    }
}

fn probe_images(report: &mut Report) {
    match crate::image::list_images() {
        Ok(images) if images.is_empty() => report.push(Probe::info(
            "images",
            "none; run 'nucleus pull alpine' to add one",
        )),
        Ok(images) => {
            let names: Vec<String> = images.iter().map(|(n, _)| n.clone()).collect();
            report.push(Probe::ok("images", names.join(", ")));
        }
        Err(e) => report.push(Probe::warn("images", format!("could not list: {e}"))),
    }
}

/// Locates a binary in `PATH`.
fn which(binary: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(binary);
        candidate.is_file().then(|| candidate.display().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_report_renders_all_probes() {
        let report = collect();
        assert!(!report.probes.is_empty(), "collect() produced no probes");
        let rendered = report.to_string();
        for probe in &report.probes {
            assert!(
                rendered.contains(&probe.name),
                "rendered report is missing probe {}",
                probe.name
            );
        }
        assert!(rendered.contains("Privileged mode:"));
        assert!(rendered.contains("Rootless mode:"));
    }

    #[test]
    fn test_status_ordering_puts_fail_first() {
        // Higher severity must sort first so reports can be filtered.
        assert!(Status::Fail > Status::Warn);
        assert!(Status::Warn > Status::Info);
    }

    #[test]
    fn test_which_finds_a_known_binary() {
        // sh is required for any container to run.
        assert!(which("sh").is_some(), "'sh' should be in PATH");
        assert!(which("definitely-not-a-real-binary-xyz").is_none());
    }

    #[test]
    fn test_read_trim_missing_file_is_none() {
        assert!(read_trim("/proc/definitely/not/here").is_none());
    }

    #[test]
    fn test_unshare_probe_returns_a_bool() {
        // Must not hang or panic regardless of kernel policy.
        let _ = probe_unshare_user();
    }

    #[test]
    fn test_seccomp_probe_returns_a_bool() {
        let _ = probe_seccomp_load();
    }
}
