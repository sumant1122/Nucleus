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
    if !is_root() {
        eprintln!("skipping: container lifecycle test requires root");
        return;
    }

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
    if !is_root() {
        eprintln!("skipping: reaper test requires root");
        return;
    }

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
    assert!(run.status.success(), "run failed: {}", combined(&run));

    // A detached container must be listed with its metadata.
    let list = stdout_of(&nucleus(&["list"]));
    assert!(list.contains(name), "detached container not listed: {list}");
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
