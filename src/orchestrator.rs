use crate::args::RunArgs;
use crate::utils::{parse_memory, run_command};
use anyhow::{Context, Result};
use nix::unistd::{pipe, write};
use std::fs;
use std::process::{Command, Stdio};

/// Parent Orchestrator: Sets up host networking, resource limits, and manages the child process.
pub fn run_parent_orchestrator(args: RunArgs) -> Result<()> {
    println!(
        "[Nucleus] Initializing orchestration for '{}'...",
        args.name
    );

    // 1. Setup Host Networking (Bridge)
    if !args.rootless {
        let _ = Command::new("ip")
            .args(["link", "add", "br0", "type", "bridge"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = Command::new("ip")
            .args(["addr", "add", "10.0.0.1/24", "dev", "br0"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = Command::new("ip")
            .args(["link", "set", "br0", "up"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    // 2. Sync Pipe
    let (reader, writer) = pipe().context("Failed to create sync pipe")?;

    // 3. Spawn Child
    let mut child_cmd = Command::new("/proc/self/exe");
    child_cmd
        .arg("--internal-child")
        .arg("--name")
        .arg(&args.name)
        .arg("--ip")
        .arg(&args.ip)
        .arg("--pipe-fd")
        .arg(reader.to_string())
        .arg("--memory")
        .arg(&args.memory);

    if args.rootless {
        child_cmd.arg("--rootless");
    }

    if args.readonly {
        child_cmd.arg("--readonly");
    }

    for vol in &args.volumes {
        child_cmd.arg("--volumes").arg(vol);
    }

    for port in &args.ports {
        child_cmd.arg("--ports").arg(port);
    }

    let mut child = child_cmd
        .args(&args.command)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .context("Failed to spawn child process")?;

    let pid = child.id();
    
    // Save state
    crate::state::save_state(&crate::state::ContainerState {
        name: args.name.clone(),
        pid,
        ip: args.ip.clone(),
        status: "Running".to_string(),
    })?;

    let short_name = if args.name.len() > 12 {
        &args.name[..12]
    } else {
        &args.name
    };
    let v_host = format!("vh-{}", short_name);
    let v_child = format!("vc-{}", short_name);

    // 4. Networking: Connect Host to Container
    if !args.rootless {
        let _ = Command::new("ip")
            .args(["link", "delete", &v_host])
            .stderr(Stdio::null())
            .status();
        run_command(
            "ip",
            &[
                "link", "add", &v_host, "type", "veth", "peer", "name", &v_child,
            ],
        )?;
        run_command("ip", &["link", "set", &v_child, "netns", &pid.to_string()])?;
        run_command("ip", &["link", "set", &v_host, "master", "br0"])?;
        run_command("ip", &["link", "set", &v_host, "up"])?;

        let pid_str = pid.to_string();
        let ns_base = ["-t", &pid_str, "-n", "ip"];
        run_command(
            "nsenter",
            &[
                ns_base[0], ns_base[1], ns_base[2], ns_base[3], "link", "set", &v_child, "name",
                "eth0",
            ],
        )?;
        run_command(
            "nsenter",
            &[
                ns_base[0],
                ns_base[1],
                ns_base[2],
                ns_base[3],
                "addr",
                "add",
                &format!("{}/24", args.ip),
                "dev",
                "eth0",
            ],
        )?;
        run_command(
            "nsenter",
            &[
                ns_base[0], ns_base[1], ns_base[2], ns_base[3], "link", "set", "eth0", "up",
            ],
        )?;
        run_command(
            "nsenter",
            &[
                ns_base[0], ns_base[1], ns_base[2], ns_base[3], "link", "set", "lo", "up",
            ],
        )?;
        run_command(
            "nsenter",
            &[
                ns_base[0], ns_base[1], ns_base[2], ns_base[3], "route", "add", "default", "via",
                "10.0.0.1",
            ],
        )?;
    }

    // 5. Resource Limits (Cgroups v2)
    let cgroup_path = format!("/sys/fs/cgroup/{}", args.name);
    if !args.rootless {
        let _ = fs::write(
            "/sys/fs/cgroup/cgroup.subtree_control",
            "+memory +cpu +pids",
        );
        fs::create_dir_all(&cgroup_path).context("Failed to create cgroup dir")?;
        let mem_bytes = parse_memory(&args.memory)?;
        let _ = fs::write(format!("{}/memory.max", cgroup_path), &mem_bytes);
        let _ = fs::write(format!("{}/cpu.max", cgroup_path), "max 100000");
        let _ = fs::write(format!("{}/pids.max", cgroup_path), "max");
        fs::write(format!("{}/cgroup.procs", cgroup_path), pid.to_string())
            .context("Failed to join cgroup")?;
    }

    // 6. Port Mapping & Forwarding
    if !args.rootless {
        let _ = fs::write("/proc/sys/net/ipv4/ip_forward", "1");
        let _ = Command::new("iptables")
            .args([
                "-t",
                "nat",
                "-A",
                "POSTROUTING",
                "-s",
                "10.0.0.0/24",
                "!",
                "-o",
                "br0",
                "-j",
                "MASQUERADE",
            ])
            .status();
        let _ = Command::new("iptables")
            .args(["-A", "FORWARD", "-i", "br0", "-j", "ACCEPT"])
            .status();
        let _ = Command::new("iptables")
            .args(["-A", "FORWARD", "-o", "br0", "-j", "ACCEPT"])
            .status();

        for port_mapping in &args.ports {
            let parts: Vec<&str> = port_mapping.split(':').collect();
            if parts.len() == 2 {
                let host_port = parts[0];
                let container_port = parts[1];
                let _ = Command::new("iptables")
                    .args([
                        "-A",
                        "FORWARD",
                        "-p",
                        "tcp",
                        "-d",
                        &args.ip,
                        "--dport",
                        container_port,
                        "-m",
                        "state",
                        "--state",
                        "NEW,ESTABLISHED,RELATED",
                        "-j",
                        "ACCEPT",
                    ])
                    .status();
                let _ = Command::new("iptables")
                    .args([
                        "-t",
                        "nat",
                        "-A",
                        "PREROUTING",
                        "-p",
                        "tcp",
                        "--dport",
                        host_port,
                        "-j",
                        "DNAT",
                        "--to-destination",
                        &format!("{}:{}", args.ip, container_port),
                    ])
                    .status();
            }
        }
    }

    // 7. Signal Child
    write(writer, b"done").ok();
    println!("[Nucleus] Network links established. Handing over control.");

    let status = child.wait().context("Container process failed")?;

    // 8. Cleanup
    println!("[Nucleus] Cleaning up resources...");
    let _ = crate::state::remove_state(&args.name);
    if !args.rootless {
        let _ = fs::remove_dir_all(&cgroup_path);
    }
    let _ = fs::remove_dir_all(format!("./temp/{}", args.name));

    if !args.rootless {
        let _ = Command::new("ip")
            .args(["link", "delete", &v_host])
            .status();

        for port_mapping in &args.ports {
            let parts: Vec<&str> = port_mapping.split(':').collect();
            if parts.len() == 2 {
                let host_port = parts[0];
                let container_port = parts[1];
                let _ = Command::new("iptables")
                    .args([
                        "-D",
                        "FORWARD",
                        "-p",
                        "tcp",
                        "-d",
                        &args.ip,
                        "--dport",
                        container_port,
                        "-m",
                        "state",
                        "--state",
                        "NEW,ESTABLISHED,RELATED",
                        "-j",
                        "ACCEPT",
                    ])
                    .status();
                let _ = Command::new("iptables")
                    .args([
                        "-t",
                        "nat",
                        "-D",
                        "PREROUTING",
                        "-p",
                        "tcp",
                        "--dport",
                        host_port,
                        "-j",
                        "DNAT",
                        "--to-destination",
                        &format!("{}:{}", args.ip, container_port),
                    ])
                    .status();
            }
        }
    }

    println!(
        "[Nucleus] Container '{}' terminated (Status: {})",
        args.name, status
    );
    Ok(())
}
