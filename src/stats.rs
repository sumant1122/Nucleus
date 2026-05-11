use anyhow::Result;
use std::fs;
use std::thread;
use std::time::Duration;

pub fn display_stats(name: &str, stream: bool) -> Result<()> {
    // Verify container exists in state
    let containers = crate::state::list_containers()?;
    if !containers.iter().any(|c| c.name == name) {
        return Err(anyhow::anyhow!(
            "Container '{}' not found or not running.",
            name
        ));
    }

    loop {
        let stats = match get_container_stats(name) {
            Ok(s) => s,
            Err(e) => {
                if stream {
                    println!("\r[Nucleus] Container '{}' stopped.", name);
                    break;
                } else {
                    return Err(e);
                }
            }
        };

        // Clear screen if streaming
        if stream {
            print!("\x1B[2J\x1B[H");
        }

        println!(
            "{:<20} {:<15} {:<15} {:<10}",
            "NAME", "CPU %", "MEM USAGE / LIMIT", "PIDS"
        );
        println!("{:-<65}", "");

        let mem_limit_str = if stats.memory_limit == 0 {
            "unlimited".to_string()
        } else {
            format!("{:.2}MB", stats.memory_limit as f64 / 1024.0 / 1024.0)
        };

        println!(
            "{:<20} {:<15.2} {:<15} {:<10}",
            name,
            stats.cpu_percentage,
            format!(
                "{:.2}MB / {}",
                stats.memory_usage as f64 / 1024.0 / 1024.0,
                mem_limit_str
            ),
            stats.pids_current
        );

        if !stream {
            break;
        }
        thread::sleep(Duration::from_secs(1));
    }
    Ok(())
}

pub struct ContainerStats {
    pub cpu_percentage: f64,
    pub memory_usage: u64,
    pub memory_limit: u64,
    pub pids_current: u64,
}

fn get_container_stats(name: &str) -> Result<ContainerStats> {
    let cgroup_base = format!("/sys/fs/cgroup/{}", name);
    if !std::path::Path::new(&cgroup_base).exists() {
        return Err(anyhow::anyhow!(
            "Container '{}' cgroup not found. Is it running?",
            name
        ));
    }

    // Memory
    let memory_usage: u64 = fs::read_to_string(format!("{}/memory.current", cgroup_base))?
        .trim()
        .parse()?;

    let memory_limit_raw = fs::read_to_string(format!("{}/memory.max", cgroup_base))?
        .trim()
        .to_string();
    let memory_limit: u64 = if memory_limit_raw == "max" {
        0
    } else {
        memory_limit_raw.parse()?
    };

    // PIDs
    let pids_current: u64 = fs::read_to_string(format!("{}/pids.current", cgroup_base))?
        .trim()
        .parse()?;

    // CPU (Simplified: reading usage_usec over an interval would be better for %,
    // but for now let's just show total usage or a placeholder)
    // To get %, we'd need to sample twice.
    let cpu_percentage = calculate_cpu_usage(&cgroup_base)?;

    Ok(ContainerStats {
        cpu_percentage,
        memory_usage,
        memory_limit,
        pids_current,
    })
}

fn calculate_cpu_usage(cgroup_path: &str) -> Result<f64> {
    let read_cpu_usec = || -> Result<u64> {
        let content = fs::read_to_string(format!("{}/cpu.stat", cgroup_path))?;
        for line in content.lines() {
            if line.starts_with("usage_usec") {
                return Ok(line.split_whitespace().nth(1).unwrap().parse()?);
            }
        }
        Ok(0)
    };

    let start_usage = read_cpu_usec()?;
    thread::sleep(Duration::from_millis(100));
    let end_usage = read_cpu_usec()?;

    let diff = end_usage - start_usage;
    // diff is in microseconds over 100ms (100,000 microseconds)
    let percentage = (diff as f64 / 100_000.0) * 100.0;
    Ok(percentage)
}
