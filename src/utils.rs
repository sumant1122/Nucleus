use anyhow::{Context, Result};
use nix::unistd::getuid;
use std::path::PathBuf;
use std::process::Command;

/// Executes a shell command and returns an error if it fails.
pub fn run_command(cmd: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(cmd)
        .args(args)
        .status()
        .context(format!("Failed to execute command: {} {:?}", cmd, args))?;

    if !status.success() {
        return Err(anyhow::anyhow!(
            "Command {} {:?} failed with status: {}",
            cmd,
            args,
            status
        ));
    }
    Ok(())
}

/// Returns the primary data directory for images, volumes, and caches.
pub fn get_nucleus_data_dir() -> PathBuf {
    if getuid().is_root() {
        PathBuf::from("/var/lib/nucleus")
    } else if let Ok(data_home) = std::env::var("XDG_DATA_HOME") {
        PathBuf::from(data_home).join("nucleus")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/share/nucleus")
    } else {
        PathBuf::from("/tmp/nucleus")
    }
}

/// Returns the runtime directory for active container state and mount points.
pub fn get_nucleus_runtime_dir() -> PathBuf {
    if getuid().is_root() {
        PathBuf::from("/run/nucleus")
    } else if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime_dir).join("nucleus")
    } else {
        PathBuf::from(format!("/tmp/nucleus-{}", getuid()))
    }
}

/// Returns the log directory for detached container outputs.
pub fn get_nucleus_log_dir() -> PathBuf {
    if getuid().is_root() {
        PathBuf::from("/var/log/nucleus")
    } else if let Ok(state_home) = std::env::var("XDG_STATE_HOME") {
        PathBuf::from(state_home).join("nucleus/logs")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/state/nucleus/logs")
    } else {
        PathBuf::from(format!("/tmp/nucleus-{}/logs", getuid()))
    }
}

/// Returns the normalized system architecture (e.g., "x86_64", "aarch64").
pub fn get_target_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "arm" => "armhf",
        other => other,
    }
}

/// Generates unique and valid (max 15 chars) veth pair names for a container.
pub fn generate_veth_names(container_name: &str) -> (String, String) {
    // Linux IFNAMSIZ is 16 bytes (15 chars + null terminator).
    // Use FNV-1a hash to generate a unique 4-character hex suffix.
    let mut hash: u32 = 0x811c9dc5;
    for b in container_name.bytes() {
        hash = (hash ^ (b as u32)).wrapping_mul(0x01000193);
    }
    let suffix = format!("{:04x}", hash & 0xffff);

    let clean_name: String = container_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(7)
        .collect();

    let v_host = format!("vh-{}-{}", clean_name, suffix);
    let v_child = format!("vc-{}-{}", clean_name, suffix);

    (v_host, v_child)
}

/// Parses human-readable memory strings (e.g., "512M", "512MB", "1G", "1GiB") into bytes.
pub fn parse_memory(mem: &str) -> Result<String> {
    let trimmed = mem.trim();
    if trimmed.eq_ignore_ascii_case("max") || trimmed.eq_ignore_ascii_case("unlimited") {
        return Ok("max".to_string());
    }

    let upper = trimmed.to_uppercase();
    let (val_str, unit): (&str, u64) = if upper.ends_with("GIB") || upper.ends_with("GB") {
        let len = if upper.ends_with("GIB") { 3 } else { 2 };
        (&upper[..upper.len() - len], 1024 * 1024 * 1024)
    } else if upper.ends_with('G') {
        (&upper[..upper.len() - 1], 1024 * 1024 * 1024)
    } else if upper.ends_with("MIB") || upper.ends_with("MB") {
        let len = if upper.ends_with("MIB") { 3 } else { 2 };
        (&upper[..upper.len() - len], 1024 * 1024)
    } else if upper.ends_with('M') {
        (&upper[..upper.len() - 1], 1024 * 1024)
    } else if upper.ends_with("KIB") || upper.ends_with("KB") {
        let len = if upper.ends_with("KIB") { 3 } else { 2 };
        (&upper[..upper.len() - len], 1024)
    } else if upper.ends_with('K') {
        (&upper[..upper.len() - 1], 1024)
    } else if upper.ends_with('B') {
        (&upper[..upper.len() - 1], 1)
    } else {
        (upper.as_str(), 1)
    };

    let val: u64 = val_str
        .trim()
        .parse()
        .context(format!("Failed to parse numeric memory value from '{}'", mem))?;
    if val == 0 {
        return Ok("0".to_string());
    }
    Ok((val * unit).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_memory() {
        assert_eq!(parse_memory("max").unwrap(), "max");
        assert_eq!(parse_memory("MAX").unwrap(), "max");
        assert_eq!(
            parse_memory("512M").unwrap(),
            (512 * 1024 * 1024).to_string()
        );
        assert_eq!(
            parse_memory("512mb").unwrap(),
            (512 * 1024 * 1024).to_string()
        );
        assert_eq!(
            parse_memory("1G").unwrap(),
            (1024 * 1024 * 1024).to_string()
        );
        assert_eq!(
            parse_memory("1GiB").unwrap(),
            (1024 * 1024 * 1024).to_string()
        );
        assert_eq!(parse_memory("10k").unwrap(), (10 * 1024).to_string());
        assert_eq!(parse_memory("100").unwrap(), "100");
    }

    #[test]
    fn test_generate_veth_names() {
        let (vh1, vc1) = generate_veth_names("my-very-long-container-name-1");
        let (vh2, vc2) = generate_veth_names("my-very-long-container-name-2");
        assert!(vh1.len() <= 15, "vh1 length {} > 15", vh1.len());
        assert!(vc1.len() <= 15, "vc1 length {} > 15", vc1.len());
        assert_ne!(vh1, vh2, "Veth names collided for different container names");
    }

    #[test]
    fn test_parse_memory_invalid() {
        assert!(parse_memory("abc").is_err());
    }
}
