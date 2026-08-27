use anyhow::Result;
use std::fs;
use std::thread;
use std::time::{Duration, Instant};

pub fn display_stats(name: &str, stream: bool) -> Result<()> {
    // Verify container exists in state
    let containers = crate::state::list_containers()?;
    if !containers.iter().any(|c| c.name == name) {
        return Err(anyhow::anyhow!(
            "Container '{}' not found or not running.",
            name
        ));
    }

    let cgroup_base = format!("/sys/fs/cgroup/{}", name);
    let mut prev_cpu_usec: Option<u64> = None;
    let mut prev_instant = Instant::now();

    loop {
        let stats = match get_container_stats(name, &cgroup_base, &mut prev_cpu_usec, &mut prev_instant) {
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
            "{:<20} {:<15} {:<25} {:<10}",
            "NAME", "CPU %", "MEM USAGE / LIMIT", "PIDS"
        );
        println!("{:-<75}", "");

        let mem_limit_str = if stats.memory_limit == 0 {
            "unlimited".to_string()
        } else {
            format!("{:.2}MB", stats.memory_limit as f64 / 1024.0 / 1024.0)
        };

        println!(
            "{:<20} {:<15.2} {:<25} {:<10}",
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

fn read_cpu_usec(cgroup_path: &str) -> Result<u64> {
    let content = fs::read_to_string(format!("{}/cpu.stat", cgroup_path))?;
    for line in content.lines() {
        if line.starts_with("usage_usec") {
            if let Some(val_str) = line.split_whitespace().nth(1) {
                return Ok(val_str.parse().unwrap_or(0));
            }
        }
    }
    Ok(0)
}

fn get_container_stats(
    name: &str,
    cgroup_base: &str,
    prev_cpu: &mut Option<u64>,
    prev_instant: &mut Instant,
) -> Result<ContainerStats> {
    if !std::path::Path::new(cgroup_base).exists() {
        return Err(anyhow::anyhow!(
            "Container '{}' cgroup not found. Is it running?",
            name
        ));
    }

    // Memory
    let memory_usage: u64 = fs::read_to_string(format!("{}/memory.current", cgroup_base))
        .map(|s| s.trim().parse().unwrap_or(0))
        .unwrap_or(0);

    let memory_limit_raw = fs::read_to_string(format!("{}/memory.max", cgroup_base))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "max".to_string());

    let memory_limit: u64 = if memory_limit_raw == "max" {
        0
    } else {
        memory_limit_raw.parse().unwrap_or(0)
    };

    // PIDs
    let pids_current: u64 = fs::read_to_string(format!("{}/pids.current", cgroup_base))
        .map(|s| s.trim().parse().unwrap_or(0))
        .unwrap_or(0);

    // CPU Calculation
    let current_cpu = read_cpu_usec(cgroup_base)?;
    let now = Instant::now();

    let cpu_percentage = if let Some(last_cpu) = *prev_cpu {
        let elapsed_micros = now.duration_since(*prev_instant).as_micros() as f64;
        let delta_cpu = current_cpu.saturating_sub(last_cpu) as f64;
        if elapsed_micros > 0.0 {
            (delta_cpu / elapsed_micros) * 100.0
        } else {
            0.0
        }
    } else {
        // First sample in one-shot mode: do a quick 80ms sample
        thread::sleep(Duration::from_millis(80));
        let next_cpu = read_cpu_usec(cgroup_base)?;
        let delta = next_cpu.saturating_sub(current_cpu) as f64;
        (delta / 80_000.0) * 100.0
    };

    *prev_cpu = Some(current_cpu);
    *prev_instant = now;

    Ok(ContainerStats {
        cpu_percentage,
        memory_usage,
        memory_limit,
        pids_current,
    })
}
