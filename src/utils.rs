use anyhow::{Context, Result};
use std::process::Command;

/// Executes a shell command and returns an error if it fails.
pub fn run_command(cmd: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(cmd)
        .args(args)
        .status()
        .context(format!("Failed to execute command: {} {:?}", cmd, args))?;
    
    if !status.success() {
        return Err(anyhow::anyhow!("Command {} {:?} failed with status: {}", cmd, args, status));
    }
    Ok(())
}

/// Parses human-readable memory strings (e.g., "512M", "1G") into bytes.
pub fn parse_memory(mem: &str) -> Result<String> {
    if mem == "max" {
        return Ok("max".to_string());
    }

    let mem = mem.to_uppercase();
    let (val_str, unit) = if mem.ends_with('G') {
        (&mem[..mem.len()-1], 1024 * 1024 * 1024)
    } else if mem.ends_with('M') {
        (&mem[..mem.len()-1], 1024 * 1024)
    } else if mem.ends_with('K') {
        (&mem[..mem.len()-1], 1024)
    } else {
        (mem.as_str(), 1)
    };

    let val: u64 = val_str.parse().context("Failed to parse memory value")?;
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
        assert_eq!(parse_memory("512M").unwrap(), (512 * 1024 * 1024).to_string());
        assert_eq!(parse_memory("1G").unwrap(), (1024 * 1024 * 1024).to_string());
        assert_eq!(parse_memory("10k").unwrap(), (10 * 1024).to_string());
        assert_eq!(parse_memory("100").unwrap(), "100");
    }

    #[test]
    fn test_parse_memory_invalid() {
        assert!(parse_memory("abc").is_err());
    }
}
