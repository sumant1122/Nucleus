//! End-to-end tests driving the compiled `Nucleus` binary.
//!
//! The container lifecycle test needs root and namespaces, so it self-skips.
//! The CLI and validation tests run unprivileged and therefore execute in CI.

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_Nucleus");

/// Runs the binary with the given arguments.
fn nucleus(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("failed to execute the Nucleus binary")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

/// Combined output, so assertions work regardless of which stream is used.
fn combined(output: &Output) -> String {
    format!("{}{}", stdout_of(output), stderr_of(output))
}

/// Whether this host can actually run a rootless container.
///
/// The container tests need a working unprivileged user namespace with a
/// writable id mapping. Some CI kernels permit `unshare(CLONE_NEWUSER)` but
/// refuse the mapping, which would otherwise surface as a confusing test
/// failure rather than an environmental skip. `nucleus info` performs exactly
/// this probe.
fn rootless_supported() -> bool {
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        let output = nucleus(&["info", "--json"]);
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout_of(&output)) else {
            // If the probe cannot be parsed, assume supported rather than
            // silently skipping everything.
            return true;
        };
        let supported = json["rootless_available"].as_bool().unwrap_or(true);
        if !supported {
            let reason = json["probes"]
                .as_array()
                .map(|probes| {
                    probes
                        .iter()
                        .filter(|p| p["status"] == "FAIL")
                        .map(|p| p["name"].as_str().unwrap_or("?").to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            eprintln!("skipping rootless container tests: host cannot map user ids ({reason})");
        }
        supported
    })
}

/// Skips the calling test when the host cannot run rootless containers.
macro_rules! require_rootless {
    () => {
        if !rootless_supported() {
            return;
        }
    };
}

/// Skips the calling test when not running as root.
macro_rules! require_root {
    () => {
        if !is_root() {
            eprintln!("skipping: requires root");
            return;
        }
    };
}

fn is_root() -> bool {
    // SAFETY: getuid() has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    uid == 0
}

// ---------------------------------------------------------------------------
// Unprivileged tests: these run in CI.
// ---------------------------------------------------------------------------

#[test]
fn test_help_advertises_every_subcommand() {
    let output = nucleus(&["--help"]);
    assert!(
        output.status.success(),
        "--help failed: {}",
        stderr_of(&output)
    );

    let help = stdout_of(&output);
    for command in [
        "run", "exec", "inspect", "list", "ps", "images", "rmi", "rm", "logs", "stop", "stats",
        "pull",
    ] {
        assert!(help.contains(command), "help output missing '{command}'");
    }
}

#[test]
fn test_hidden_internal_subcommands_are_not_advertised() {
    let help = stdout_of(&nucleus(&["--help"]));
    assert!(
        !help.contains("internal-child"),
        "internal-child must stay hidden from users"
    );
    assert!(
        !help.contains("internal-reaper"),
        "internal-reaper must stay hidden from users"
    );
}

#[test]
fn test_version_is_reported() {
    let output = nucleus(&["--version"]);
    assert!(output.status.success());
    // Read the version from the manifest so this cannot go stale.
    assert!(
        stdout_of(&output).contains(env!("CARGO_PKG_VERSION")),
        "--version did not report {}",
        env!("CARGO_PKG_VERSION")
    );
}

#[test]
fn test_list_reports_no_containers() {
    let output = nucleus(&["list"]);
    assert!(
        output.status.success(),
        "list failed: {}",
        stderr_of(&output)
    );
    let text = stdout_of(&output);
    assert!(
        text.contains("No containers running") || text.contains("NAME"),
        "unexpected list output: {text}"
    );
}

#[test]
fn test_images_lists_without_error() {
    let output = nucleus(&["images"]);
    assert!(
        output.status.success(),
        "images failed: {}",
        stderr_of(&output)
    );
}

#[test]
fn test_ps_is_an_alias_for_list() {
    let list = nucleus(&["list"]);
    let ps = nucleus(&["ps"]);
    assert!(ps.status.success());
    assert_eq!(list.status.success(), ps.status.success());
}

#[test]
fn test_container_name_traversal_is_rejected() {
    // A name reaching the state file or cgroup path must not be able to
    // traverse out of them. Validation happens before the root check, so this
    // is observable without privileges.
    for name in [
        "../../etc/passwd",
        "..",
        ".",
        "a/b",
        "/absolute",
        "has space",
    ] {
        let output = nucleus(&["run", "--name", name, "true"]);
        assert!(
            !output.status.success(),
            "name '{name}' should have been rejected"
        );
        let text = combined(&output);
        assert!(
            text.contains("container name") || text.contains("Invalid container name"),
            "unexpected error for name '{name}': {text}"
        );
    }
}

#[test]
fn test_volume_traversal_is_rejected() {
    let output = nucleus(&[
        "run",
        "--name",
        "vol-escape",
        "--rootless",
        "-v",
        "/tmp:/../../escape",
        "true",
    ]);
    assert!(
        !output.status.success(),
        "volume traversal should be rejected"
    );
    let text = combined(&output);
    assert!(
        text.contains("volume") || text.contains("'..'"),
        "unexpected error: {text}"
    );
}

#[test]
fn test_invalid_volume_specs_are_rejected() {
    for spec in ["onlyonepart", ":/data", "/host:/data:bogus"] {
        let output = nucleus(&["run", "--name", "vol-bad", "--rootless", "-v", spec, "true"]);
        assert!(
            !output.status.success(),
            "volume '{spec}' should be rejected"
        );
    }
}

#[test]
fn test_invalid_port_specs_are_rejected() {
    for spec in ["0:80", "70000:80", "abc:80", "80:80/sctp", "1:2:3:4"] {
        let output = nucleus(&[
            "run",
            "--name",
            "port-bad",
            "--rootless",
            "-p",
            spec,
            "true",
        ]);
        assert!(!output.status.success(), "port '{spec}' should be rejected");
    }
}

#[test]
fn test_duplicate_port_mappings_are_rejected() {
    let output = nucleus(&[
        "run",
        "--name",
        "port-dup",
        "--rootless",
        "-p",
        "8080:80",
        "-p",
        "8080:80",
        "true",
    ]);
    assert!(!output.status.success());
    assert!(combined(&output).contains("duplicate"));
}

#[test]
fn test_port_mapping_requires_rootless_opt_out() {
    // Rootless containers have no network namespace, so ports are meaningless.
    let output = nucleus(&[
        "run",
        "--name",
        "port-rootless",
        "--rootless",
        "-p",
        "8080:80",
        "true",
    ]);
    assert!(!output.status.success());
    assert!(combined(&output).contains("rootless"));
}

#[test]
fn test_invalid_resource_limits_are_rejected() {
    // Memory
    let output = nucleus(&[
        "run",
        "--name",
        "bad-mem",
        "--rootless",
        "-m",
        "abc",
        "true",
    ]);
    assert!(
        !output.status.success(),
        "bad memory limit should be rejected"
    );
    assert!(combined(&output).contains("memory"));

    // CPUs (use `=` so clap does not read the leading '-' as a flag)
    for cpus in ["-1", "0", "abc"] {
        let spec = format!("--cpus={cpus}");
        let output = nucleus(&["run", "--name", "bad-cpu", "--rootless", &spec, "true"]);
        assert!(!output.status.success(), "--cpus={cpus} should be rejected");
        let text = combined(&output);
        assert!(
            text.contains("--cpus") || text.contains("invalid value"),
            "unexpected error for --cpus={cpus}: {text}"
        );
    }

    // PIDs
    let output = nucleus(&[
        "run",
        "--name",
        "bad-pid",
        "--rootless",
        "--pids-limit",
        "0",
        "true",
    ]);
    assert!(
        !output.status.success(),
        "--pids-limit 0 should be rejected"
    );
    assert!(combined(&output).contains("--pids-limit"));
}

#[test]
fn test_invalid_subnet_is_rejected() {
    for subnet in ["10.0.0.1", "10.0.0.256/24", "10.0.0.1/33"] {
        let output = nucleus(&[
            "run",
            "--name",
            "bad-subnet",
            "--rootless",
            "--subnet",
            subnet,
            "true",
        ]);
        assert!(
            !output.status.success(),
            "subnet '{subnet}' should be rejected"
        );
    }
}

#[test]
fn test_invalid_workdir_traversal_is_rejected() {
    let output = nucleus(&[
        "run",
        "--name",
        "bad-wd",
        "--rootless",
        "-w",
        "../../etc",
        "true",
    ]);
    assert!(!output.status.success());
    assert!(combined(&output).contains("'..'"));
}

#[test]
fn test_invalid_env_is_rejected() {
    let output = nucleus(&[
        "run",
        "--name",
        "bad-env",
        "--rootless",
        "-e",
        "NOEQUALS",
        "true",
    ]);
    assert!(!output.status.success());
    assert!(combined(&output).contains("KEY=VALUE"));
}

#[test]
fn test_privileged_run_without_root_is_refused() {
    if is_root() {
        return;
    }
    let output = nucleus(&["run", "--name", "needs-root", "true"]);
    assert!(!output.status.success());
    assert!(combined(&output).contains("--rootless"));
}

#[test]
fn test_operations_on_missing_containers_report_cleanly() {
    for args in [
        vec!["stop", "definitely-missing"],
        vec!["stats", "definitely-missing"],
        vec!["inspect", "definitely-missing"],
        vec!["exec", "definitely-missing", "true"],
    ] {
        let output = nucleus(&args);
        assert!(
            !output.status.success(),
            "'{}' should fail for a missing container",
            args.join(" ")
        );
        assert!(
            combined(&output).contains("not found"),
            "unexpected error for '{}': {}",
            args.join(" "),
            combined(&output)
        );
    }
}

#[test]
fn test_pull_rejects_unknown_distro() {
    let output = nucleus(&["pull", "not-a-real-distro"]);
    assert!(!output.status.success());
    assert!(combined(&output).contains("not-a-real-distro"));
}

#[test]
fn test_pull_reports_arch_mismatch_instead_of_substituting() {
    // Every architecture must be reported explicitly rather than silently
    // falling back to an x86_64 rootfs.
    let output = nucleus(&["pull", "plan9"]);
    assert!(!output.status.success());
    assert!(combined(&output).contains("Supported images"));
}

// ---------------------------------------------------------------------------
// Privileged test: requires root and namespaces.
// ---------------------------------------------------------------------------

#[test]
fn test_container_lifecycle() {
    require_root!();
    require_rootless!();

    let name = "test-integration-box";

    // Clean up any leftovers from a previous run.
    let _ = nucleus(&["stop", name]);
    let _ = nucleus(&["rm", name, "--force"]);

    let run = nucleus(&[
        "run",
        "--name",
        name,
        "--ip",
        "10.0.0.99",
        "--detach",
        "--memory",
        "64M",
        "--pids-limit",
        "64",
        "sleep",
        "30",
    ]);
    assert!(
        run.status.success(),
        "Nucleus run failed: {}",
        combined(&run)
    );

    // The container should be visible with its published metadata.
    let list = nucleus(&["list"]);
    let list_text = stdout_of(&list);
    assert!(
        list_text.contains(name),
        "container missing from list output: {list_text}"
    );
    assert!(
        list_text.contains("10.0.0.99"),
        "container IP missing from list output: {list_text}"
    );

    // inspect should emit valid JSON containing the requested limits.
    let inspect = nucleus(&["inspect", name]);
    assert!(inspect.status.success());
    let json = stdout_of(&inspect);
    let parsed: serde_json::Value = serde_json::from_str(&json)
        .unwrap_or_else(|e| panic!("inspect output not JSON ({e}): {json}"));
    assert_eq!(parsed["name"], name);
    assert_eq!(parsed["memory"], "67108864");
    assert_eq!(parsed["pids_limit"], 64);
    assert!(parsed["uptime"].is_string());

    // stats should report the cgroup it created.
    let stats = nucleus(&["stats", name]);
    assert!(stats.status.success(), "stats failed: {}", combined(&stats));
    assert!(stdout_of(&stats).contains("MEM USAGE"));

    // cgroup should live under the Nucleus subtree.
    let cgroup_path = format!("/sys/fs/cgroup/nucleus/{name}");
    assert!(
        std::path::Path::new(&cgroup_path).exists(),
        "cgroup directory {cgroup_path} was not created"
    );

    // Stopping should reap the process and remove host resources.
    let stop = nucleus(&["stop", name, "--timeout", "5"]);
    assert!(stop.status.success(), "stop failed: {}", combined(&stop));

    assert!(
        !std::path::Path::new(&cgroup_path).exists(),
        "cgroup {cgroup_path} was not cleaned up"
    );
    let after = stdout_of(&nucleus(&["list"]));
    assert!(
        !after.contains(name),
        "container still listed after stop: {after}"
    );
}

#[test]
fn test_detached_container_is_reaped_on_exit() {
    require_root!();

    let name = "test-reaper-box";
    let _ = nucleus(&["stop", name]);
    let _ = nucleus(&["rm", name, "--force"]);

    let run = nucleus(&[
        "run",
        "--name",
        name,
        "--ip",
        "10.0.0.98",
        "--detach",
        "true",
    ]);
    assert!(run.status.success(), "run failed: {}", combined(&run));

    // `true` exits immediately; the reaper must clean up behind it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let list = stdout_of(&nucleus(&["list"]));
        if !list.contains(name) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "detached container was never reaped: {list}"
        );
        std::thread::sleep(std::time::Duration::from_millis(250));
    }

    let cgroup_path = format!("/sys/fs/cgroup/nucleus/{name}");
    assert!(
        !std::path::Path::new(&cgroup_path).exists(),
        "cgroup {cgroup_path} leaked after the container exited"
    );
}

#[test]
fn test_rootless_run_leaves_no_runtime_directory() {
    require_rootless!();
    // Regression guard: OverlayFS creates an internal work directory with mode
    // 000, so a plain recursive delete fails with EACCES and the whole
    // per-container directory leaks. This has to be observable, so assert it.
    let name = "test-cleanup-box";
    let _ = nucleus(&["stop", name]);
    let _ = nucleus(&["rm", name, "--force"]);

    let run = nucleus(&[
        "run",
        "--rootless",
        "--name",
        name,
        "--ip",
        "10.0.0.97",
        "true",
    ]);
    assert!(run.status.success(), "run failed: {}", combined(&run));

    let container_dir = runtime_containers_dir().join(name);
    assert!(
        !container_dir.exists(),
        "per-container runtime directory {} leaked after the container exited",
        container_dir.display()
    );
}

#[test]
fn test_detached_container_is_listed_then_reclaimed() {
    require_rootless!();
    let name = "test-detached-box";
    let _ = nucleus(&["stop", name]);
    let _ = nucleus(&["rm", name, "--force"]);

    let run = nucleus(&[
        "run",
        "--rootless",
        "--name",
        name,
        "--ip",
        "10.0.0.96",
        "--detach",
        "sleep",
        "30",
    ]);
    assert!(
        run.status.success(),
        "run failed: {}\ncontainer log:\n{}",
        combined(&run),
        read_container_log(name)
    );

    // A detached container must be listed with its metadata.
    let list = stdout_of(&nucleus(&["list"]));
    assert!(
        list.contains(name),
        "detached container not listed: {list}\ncontainer log:\n{}",
        read_container_log(name)
    );
    assert!(list.contains("10.0.0.96"), "IP missing from list: {list}");

    // Stopping it must reclaim state and the runtime directory.
    let stop = nucleus(&["stop", name, "--timeout", "2"]);
    assert!(stop.status.success(), "stop failed: {}", combined(&stop));
    assert!(!stdout_of(&nucleus(&["list"])).contains(name));

    let container_dir = runtime_containers_dir().join(name);
    assert!(
        !container_dir.exists(),
        "runtime directory {} leaked after stop",
        container_dir.display()
    );
}

/// The directory Nucleus uses for per-container runtime state.
fn runtime_containers_dir() -> std::path::PathBuf {
    let base = std::env::var("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            // SAFETY: getuid() has no preconditions and cannot fail.
            std::path::PathBuf::from(format!("/tmp/nucleus-{}", unsafe { libc::getuid() }))
        });
    base.join("nucleus").join("containers")
}

// ---------------------------------------------------------------------------
// `nucleus info`
// ---------------------------------------------------------------------------

#[test]
fn test_info_reports_host_capabilities() {
    let output = nucleus(&["info"]);
    let text = combined(&output);

    // The report must cover the kernel features Nucleus depends on.
    for probe in [
        "kernel",
        "identity",
        "cgroup v2",
        "OverlayFS",
        "user namespaces",
        "seccomp",
        "iptables",
        "iproute2",
    ] {
        assert!(
            text.contains(probe),
            "info report missing '{probe}':\n{text}"
        );
    }

    // A verdict for each mode must always be present.
    assert!(
        text.contains("Privileged mode:"),
        "no privileged verdict:\n{text}"
    );
    assert!(
        text.contains("Rootless mode:"),
        "no rootless verdict:\n{text}"
    );
}

#[test]
fn test_info_json_is_valid_and_consistent() {
    let output = nucleus(&["info", "--json"]);
    let json = stdout_of(&output);

    let parsed: serde_json::Value = serde_json::from_str(&json)
        .unwrap_or_else(|e| panic!("info --json not valid JSON ({e}): {json}"));

    assert!(parsed["privileged_available"].is_boolean());
    assert!(parsed["rootless_available"].is_boolean());

    let probes = parsed["probes"]
        .as_array()
        .expect("probes must be an array");
    assert!(
        probes.len() >= 10,
        "expected a substantial report, got {probes:?}"
    );

    for probe in probes {
        for field in ["name", "status", "detail"] {
            assert!(
                probe.get(field).is_some(),
                "probe is missing '{field}': {probe:?}"
            );
        }
        let status = probe["status"].as_str().unwrap_or_default();
        assert!(
            ["INFO", "WARN", "FAIL"].contains(&status),
            "unexpected status {status:?}"
        );
    }
}

#[test]
fn test_info_exit_code_signals_usability() {
    let output = nucleus(&["info"]);

    // On a host where neither mode works the command must fail, so it can be
    // used as a provisioning or health check.
    let usable = combined(&output).contains("Rootless mode:   available")
        || combined(&output).contains("Privileged mode: available");
    if usable {
        assert!(output.status.success(), "info should exit 0 when usable");
    } else {
        assert_eq!(
            output.status.code(),
            Some(1),
            "info should exit 1 when neither mode is usable"
        );
    }
}

#[test]
fn test_flush_firewall_requires_root() {
    if is_root() {
        return;
    }
    let output = nucleus(&["flush-firewall"]);
    assert!(!output.status.success());
    assert!(combined(&output).contains("root"));
}

/// Reads a detached container's log, so test failures are diagnosable.
///
/// Without this a container that dies during startup produces a bare
/// "not listed" assertion with no indication of why.
fn read_container_log(name: &str) -> String {
    let base = std::env::var("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|_| {
            std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join(".local/state"))
        })
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"));
    let log = base.join("nucleus/logs").join(format!("{name}.log"));
    std::fs::read_to_string(&log).unwrap_or_else(|e| format!("(no log at {}: {e})", log.display()))
}

#[test]
fn test_detached_run_fails_when_the_command_cannot_start() {
    require_rootless!();
    // `run --detach` must not claim success for a container that cannot start.
    // This is what made a CI failure undiagnosable: the error was buried in
    // the log while the command exited 0.
    let cases: [(&str, &str); 4] = [
        ("missing", "/definitely-not-a-real-binary-xyz"),
        ("directory", "/bin"),
        ("non-executable", "/etc/hostname"),
        ("root", "/"),
    ];

    for (label, command) in cases {
        let name = format!("test-badcmd-{label}");
        let _ = nucleus(&["rm", &name, "--force"]);

        let run = nucleus(&["run", "--rootless", "--name", &name, "--detach", command]);
        assert!(
            !run.status.success(),
            "`--detach {command}` should have failed, but exited 0"
        );

        let text = combined(&run);
        assert!(
            text.contains("failed to start"),
            "expected a startup failure message for {label}, got: {text}"
        );
    }
}

#[test]
fn test_failed_start_leaves_no_runtime_directory() {
    require_rootless!();
    let name = "test-badcmd-cleanup";
    let _ = nucleus(&["rm", name, "--force"]);

    let run = nucleus(&[
        "run",
        "--rootless",
        "--name",
        name,
        "--detach",
        "/definitely-not-a-real-binary-xyz",
    ]);
    assert!(!run.status.success(), "expected the run to fail");

    // A container that never started must not leave anything behind.
    let container_dir = runtime_containers_dir().join(name);
    assert!(
        !container_dir.exists(),
        "failed start left {} behind",
        container_dir.display()
    );
    assert!(
        !stdout_of(&nucleus(&["list"])).contains(name),
        "failed start left the container listed"
    );
}

#[test]
fn test_short_lived_detached_container_is_reclaimed() {
    require_rootless!();
    // A container that exits immediately must still be reaped, and its runtime
    // directory removed even after `list` prunes its state file.
    let name = "test-shortlived";
    let _ = nucleus(&["rm", name, "--force"]);

    let run = nucleus(&[
        "run",
        "--rootless",
        "--name",
        name,
        "--detach",
        "/bin/sh",
        "-c",
        "true",
    ]);
    assert!(
        run.status.success(),
        "a fast but valid exit is not a failure: {}",
        combined(&run)
    );

    // Prune state immediately, which is what used to defeat the reaper.
    let _ = nucleus(&["list"]);

    let container_dir = runtime_containers_dir().join(name);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while container_dir.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert!(
        !container_dir.exists(),
        "short-lived container leaked {} even after its state was pruned",
        container_dir.display()
    );
}
