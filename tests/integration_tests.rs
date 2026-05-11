use std::process::Command;
use std::fs;
use std::path::Path;
use std::thread;
use std::time::Duration;

#[test]
fn test_container_lifecycle() {
    // This test requires root/sudo to run fully. 
    // We check for root and skip if not available to avoid failing CI.
    if unsafe { libc::getuid() } != 0 {
        eprintln!("Skipping integration test: must be run as root");
        return;
    }

    let container_name = "test-integration-box";
    let state_path = format!("/tmp/nucleus/state/{}.json", container_name);
    let cgroup_path = format!("/sys/fs/cgroup/{}", container_name);

    // 1. Cleanup previous runs
    let _ = Command::new("./target/debug/Nucleus")
        .args(["stop", container_name])
        .status();
    let _ = fs::remove_file(&state_path);

    // 2. Start a container in detached mode
    // We use a simple sleep command so it stays alive
    let status = Command::new("./target/debug/Nucleus")
        .args([
            "run", 
            "--name", container_name, 
            "--ip", "10.0.0.99", 
            "--detach",
            "sleep", "10"
        ])
        .status()
        .expect("Failed to execute Nucleus run");

    assert!(status.success(), "Nucleus run failed");

    // Give it a moment to initialize
    thread::sleep(Duration::from_millis(500));

    // 3. Verify State exists
    assert!(Path::new(&state_path).exists(), "State file was not created");

    // 4. Verify Cgroup exists
    assert!(Path::new(&cgroup_path).exists(), "Cgroup directory was not created");

    // 5. Verify Stats command works
    let stats_output = Command::new("./target/debug/Nucleus")
        .args(["stats", container_name])
        .output()
        .expect("Failed to run Nucleus stats");
    
    let stdout = String::from_utf8_lossy(&stats_output.stdout);
    assert!(stdout.contains(container_name), "Stats output missing container name");
    assert!(stdout.contains("MEM USAGE"), "Stats output missing header");

    // 6. Stop the container
    let stop_status = Command::new("./target/debug/Nucleus")
        .args(["stop", container_name])
        .status()
        .expect("Failed to run Nucleus stop");

    assert!(stop_status.success(), "Nucleus stop failed");

    // Give it a moment to cleanup
    thread::sleep(Duration::from_millis(500));

    // 7. Verify Cleanup
    assert!(!Path::new(&state_path).exists(), "State file was not cleaned up");
    assert!(!Path::new(&cgroup_path).exists(), "Cgroup was not cleaned up");
}

#[test]
fn test_help_menu() {
    let output = Command::new("./target/debug/Nucleus")
        .arg("--help")
        .output()
        .expect("Failed to run Nucleus --help");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("run"), "Help missing 'run' command");
    assert!(stdout.contains("stats"), "Help missing 'stats' command");
    assert!(stdout.contains("list"), "Help missing 'list' command");
}
