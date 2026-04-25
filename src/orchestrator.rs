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

    // 0. IPAM: Determine IP
    let container_ip = if let Some(ip) = &args.ip {
        ip.clone()
    } else {
        allocate_ip().context("Failed to auto-allocate IP")?
    };
    println!("[Nucleus] Assigned IP: {}", container_ip);

    // 1. Setup Host Networking (Bridge)
    if !args.rootless {
        let _ = Command::new("ip")
            .args(["link", "add", &args.network, "type", "bridge"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = Command::new("ip")
            .args(["addr", "add", "10.0.0.1/24", "dev", &args.network])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = Command::new("ip")
            .args(["link", "set", &args.network, "up"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    // 2. Sync Pipe
    let (reader, writer) = pipe().context("Failed to create sync pipe")?;

    // 3. Spawn Child
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

    for vol in &args.volumes {
        child_cmd.arg("--volumes").arg(vol);
    }

    for port in &args.ports {
        child_cmd.arg("--ports").arg(port);
    }

    let (stdout, stderr) = if args.detach {
        let log_dir = "/tmp/nucleus/logs";
        fs::create_dir_all(log_dir).context("Failed to create log directory")?;
        let log_path = format!("{}/{}.log", log_dir, args.name);
        let log_file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .context("Failed to open log file")?;
        let err_file = log_file.try_clone().context("Failed to clone log file handle")?;
        (Stdio::from(log_file), Stdio::from(err_file))
    } else {
        (Stdio::inherit(), Stdio::inherit())
    };

    let mut child = child_cmd
        .args(&args.command)
        .stdin(Stdio::inherit())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .context("Failed to spawn child process")?;

    let pid = child.id();

    let short_name = if args.name.len() > 12 {
        &args.name[..12]
    } else {
        &args.name
    };
    let v_host = format!("vh-{}", short_name);
    let v_child = format!("vc-{}", short_name);
    
    // Save state
    crate::state::save_state(&crate::state::ContainerState {
        name: args.name.clone(),
        pid,
        ip: container_ip.clone(),
        network: args.network.clone(),
        veth_host: v_host.clone(),
        status: "Running".to_string(),
    })?;

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
        run_command("ip", &["link", "set", &v_host, "master", &args.network])?;
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
                &format!("{}/24", container_ip),
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
                &args.network,
                "-j",
                "MASQUERADE",
            ])
            .status();
        let _ = Command::new("iptables")
            .args(["-A", "FORWARD", "-i", &args.network, "-j", "ACCEPT"])
            .status();
        let _ = Command::new("iptables")
            .args(["-A", "FORWARD", "-o", &args.network, "-j", "ACCEPT"])
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
                        &container_ip,
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
                        &format!("{}:{}", container_ip, container_port),
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
                        &container_ip,
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
                        &format!("{}:{}", container_ip, container_port),
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

fn allocate_ip() -> Result<String> {
    let containers = crate::state::list_containers()?;
    let mut used_ips = std::collections::HashSet::new();
    for c in containers {
        used_ips.insert(c.ip);
    }

    for i in 2..254 {
        let ip = format!("10.0.0.{}", i);
        if !used_ips.contains(&ip) {
            return Ok(ip);
        }
    }
    Err(anyhow::anyhow!("No available IPs in subnet 10.0.0.0/24"))
}
