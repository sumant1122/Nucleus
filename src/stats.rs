use crate::utils::format_bytes;
use anyhow::{Result, bail};
use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

/// Point-in-time resource usage for a container.
pub struct ContainerStats {
    /// CPU utilisation as a percentage of a single core.
    pub cpu_percentage: f64,
    pub memory_usage: u64,
    pub memory_limit: u64,
    pub swap_usage: u64,
    pub pids_current: u64,
}

pub fn display_stats(name: &str, stream: bool) -> Result<()> {
    let containers = crate::state::list_containers()?;
    let state = containers
        .iter()
        .find(|c| c.name == name)
        .ok_or_else(|| anyhow::anyhow!("Container '{name}' not found or not running."))?;

    if state.rootless {
        bail!(
            "stats is unavailable for rootless container '{name}': no cgroup is attached in rootless mode"
        );
    }

    let cgroup_base = state.cgroup_dir();
    if !Path::new(&cgroup_base).exists() {
        bail!(
            "Container '{name}' has no cgroup at {}. Was it created before resource limits were supported?",
            cgroup_base
        );
    }

    let mut prev_cpu_usec: Option<u64> = None;
    let mut prev_instant = Instant::now();

    loop {
        let stats =
            match get_container_stats(name, &cgroup_base, &mut prev_cpu_usec, &mut prev_instant) {
                Ok(s) => s,
                Err(e) => {
                    if stream {
                        println!("\r[Nucleus] Container '{name}' stopped.");
                        break;
                    }
                    return Err(e);
                }
            };

        if stream {
            // Clear screen and home the cursor for a stable one-line refresh.
            print!("\x1B[2J\x1B[H");
        }

        let mem_limit_str = if stats.memory_limit == 0 {
            "unlimited".to_string()
        } else {
            format_bytes(stats.memory_limit)
        };

        println!(
            "{:<20} {:<15} {:<25} {:<15} {:<10}",
            "NAME", "CPU %", "MEM USAGE / LIMIT", "SWAP USAGE", "PIDS"
        );
        println!("{:-<90}", "");
        println!(
            "{:<20} {:<15.2} {:<25} {:<15} {:<10}",
            name,
            stats.cpu_percentage,
            format!("{} / {}", format_bytes(stats.memory_usage), mem_limit_str),
            format_bytes(stats.swap_usage),
            stats.pids_current
        );
        println!("(CPU % is relative to a single core; 100% = 1 core saturated)");

        if !stream {
            break;
        }
        thread::sleep(Duration::from_secs(1));
    }
    Ok(())
}

/// Reads `usage_usec` from a cgroup's `cpu.stat`.
fn read_cpu_usec(cgroup_path: &str) -> Result<u64> {
    let content = fs::read_to_string(format!("{cgroup_path}/cpu.stat"))?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("usage_usec")
            && let Some(value) = rest.split_whitespace().next()
        {
            return Ok(value.parse().unwrap_or(0));
        }
    }
    Ok(0)
}

/// Reads a single-value cgroup file, tolerating absence.
fn read_counter(cgroup_path: &str, file: &str) -> u64 {
    fs::read_to_string(format!("{cgroup_path}/{file}"))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

fn get_container_stats(
    name: &str,
    cgroup_base: &str,
    prev_cpu: &mut Option<u64>,
    prev_instant: &mut Instant,
) -> Result<ContainerStats> {
    if !Path::new(cgroup_base).exists() {
        bail!("Container '{name}' cgroup not found. Is it running?");
    }

    let memory_usage = read_counter(cgroup_base, "memory.current");

    // memory.max reads "max" when unlimited, which is not a number.
    let memory_limit_raw = fs::read_to_string(format!("{cgroup_base}/memory.max"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "max".to_string());
    let memory_limit = if memory_limit_raw == "max" {
        0
    } else {
        memory_limit_raw.parse().unwrap_or(0)
    };

    let swap_usage = read_counter(cgroup_base, "memory.swap.current");
    let pids_current = read_counter(cgroup_base, "pids.current");

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
        // First sample in one-shot mode: take a short second reading so a
        // single `nucleus stats` call still reports a real value.
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
        swap_usage,
        pids_current,
    })
}
