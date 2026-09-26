use anyhow::{Context, Result, anyhow, bail};
use nix::unistd::getuid;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Maximum length of a container name. Keeps generated veth names and cgroup
/// paths within kernel limits and leaves room for a `.json` state suffix.
pub const MAX_NAME_LEN: usize = 64;

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

/// Validates a user-supplied container name.
///
/// The name is interpolated into a state file path and a cgroup path, so it must
/// not be able to escape either via path separators or `..` components.
pub fn validate_container_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("container name must not be empty");
    }
    if name.len() > MAX_NAME_LEN {
        bail!(
            "container name '{}' is too long ({} chars, max {})",
            name,
            name.len(),
            MAX_NAME_LEN
        );
    }
    if name == "." || name == ".." {
        bail!("container name '{}' is reserved", name);
    }
    let valid = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.');
    if !valid {
        bail!(
            "invalid container name '{}': only ASCII letters, digits, '_', '-' and '.' are allowed",
            name
        );
    }
    Ok(())
}

/// A memory limit, either a hard byte count or "no limit".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryLimit {
    Max,
    Bytes(u64),
}

impl MemoryLimit {
    /// Parses a human-readable memory string (e.g. "512M", "512MB", "1G", "1GiB").
    pub fn parse(mem: &str) -> Result<Self> {
        let trimmed = mem.trim();
        if trimmed.is_empty() {
            bail!("memory limit must not be empty");
        }
        if trimmed.eq_ignore_ascii_case("max") || trimmed.eq_ignore_ascii_case("unlimited") {
            return Ok(MemoryLimit::Max);
        }

        let upper = trimmed.to_uppercase();
        let (val_str, unit): (&str, u64) =
            if let Some(v) = strip_any_unit(&upper, &["GIB", "GB", "G"]) {
                (v, 1024 * 1024 * 1024)
            } else if let Some(v) = strip_any_unit(&upper, &["MIB", "MB", "M"]) {
                (v, 1024 * 1024)
            } else if let Some(v) = strip_any_unit(&upper, &["KIB", "KB", "K"]) {
                (v, 1024)
            } else if let Some(v) = strip_any_unit(&upper, &["B"]) {
                (v, 1)
            } else {
                (upper.as_str(), 1)
            };

        let val: u64 = val_str
            .trim()
            .parse()
            .with_context(|| format!("Failed to parse numeric memory value from '{mem}'"))?;

        // Reject overflow before it silently wraps.
        val.checked_mul(unit)
            .map(MemoryLimit::Bytes)
            .ok_or_else(|| anyhow!("memory limit '{mem}' is too large"))
    }

    /// The value written to the cgroup `memory.max` file.
    pub fn cgroup_value(&self) -> String {
        match self {
            MemoryLimit::Max => "max".to_string(),
            MemoryLimit::Bytes(b) => b.to_string(),
        }
    }
}

/// Strips the first matching suffix from `s`, returning the remainder.
fn strip_any_unit<'a>(s: &'a str, units: &[&str]) -> Option<&'a str> {
    units
        .iter()
        .find(|u| s.len() > u.len() && s.ends_with(**u))
        .map(|u| &s[..s.len() - u.len()])
}

/// Formats a byte count using binary units (e.g. "1.50GiB").
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes}B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.2}{}", UNITS[unit])
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
    // Use FNV-1a hash to generate a unique 5-character hex suffix (20 bits).
    let mut hash: u32 = 0x811c9dc5;
    for b in container_name.bytes() {
        hash = (hash ^ (b as u32)).wrapping_mul(0x01000193);
    }
    let suffix = format!("{:05x}", hash & 0x000f_ffff);

    let clean_name: String = container_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(6)
        .collect();

    // "vh-" + 6 + "-" + 5 = 15 chars, the kernel maximum.
    let v_host = format!("vh-{}-{}", clean_name, suffix);
    let v_child = format!("vc-{}-{}", clean_name, suffix);

    (v_host, v_child)
}

/// A bind mount or named volume request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    /// Host path, or the name of a volume in the centralized data directory.
    pub source: String,
    /// Normalized, container-relative destination path.
    pub destination: String,
    pub read_only: bool,
    /// True when `source` refers to a named volume rather than a host path.
    pub named: bool,
}

/// Parses a `source:destination[:ro|rw]` volume specification.
pub fn parse_volume(spec: &str) -> Result<Volume> {
    let parts: Vec<&str> = spec.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        bail!("invalid volume '{spec}': expected 'source:destination' or 'source:destination:ro'");
    }

    let source = trim_ascii(parts[0]);
    if source.is_empty() {
        bail!("invalid volume '{spec}': source must not be empty");
    }

    let read_only = match parts.get(2) {
        None => false,
        Some(&"ro") => true,
        Some(&"rw") => false,
        Some(other) => {
            bail!("invalid volume '{spec}': unknown mode '{other}' (expected 'ro' or 'rw')")
        }
    };

    let destination = normalize_container_path(parts[1])
        .with_context(|| format!("invalid volume '{spec}': invalid destination path"))?;

    // A source is a host path if it is absolute or explicitly relative.
    let named = !(source.starts_with('/') || source.starts_with('.') || source.starts_with('~'));

    Ok(Volume {
        source: source.to_string(),
        destination,
        read_only,
        named,
    })
}

/// Trims ASCII whitespace only.
///
/// `str::trim` also strips Unicode whitespace such as U+2000, which makes
/// normalisation non-idempotent: a path ending in an ideographic space is
/// transformed differently the second time it is passed through, so the value
/// that was validated and the value that gets used can disagree. ASCII
/// trimming is stable.
fn trim_ascii(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_ascii_whitespace())
}

/// Normalizes a user-supplied container path into a safe relative path.
///
/// Rejects `..` traversal, NUL bytes, and paths that resolve to the container
/// root, so the result can never escape the container's merged rootfs when
/// joined onto it.
pub fn normalize_container_path(raw: &str) -> Result<String> {
    let trimmed = trim_ascii(raw);
    if trimmed.is_empty() {
        bail!("container path must not be empty");
    }
    if trimmed.contains('\0') {
        bail!("container path must not contain NUL bytes");
    }

    let mut components: Vec<&str> = Vec::new();
    for component in trimmed.split('/') {
        match component {
            "" | "." => continue,
            ".." => bail!("container path '{raw}' must not contain '..' components"),
            c => {
                // A component with surrounding whitespace is rejected rather
                // than trimmed. Trimming it here would make normalisation
                // non-idempotent - the orchestrator validates a path, passes the
                // normalised form to the child, and the child normalises it
                // again - so the destination actually mounted could differ from
                // the one that was checked.
                if trim_ascii(c) != c {
                    bail!(
                        "container path '{raw}' has a component with leading or trailing whitespace: '{c}'"
                    );
                }
                components.push(c)
            }
        }
    }

    if components.is_empty() {
        bail!("container path '{raw}' resolves to the container root");
    }

    Ok(components.join("/"))
}

/// Parses a `KEY=VALUE` environment assignment.
pub fn parse_env_assignment(spec: &str) -> Result<(String, String)> {
    let (key, value) = spec
        .split_once('=')
        .ok_or_else(|| anyhow!("invalid environment variable '{spec}': expected 'KEY=VALUE'"))?;
    let key = trim_ascii(key);
    if key.is_empty() {
        bail!("invalid environment variable '{spec}': key must not be empty");
    }
    if key.contains('\0') || value.contains('\0') {
        bail!("invalid environment variable '{spec}': must not contain NUL bytes");
    }
    Ok((key.to_string(), value.to_string()))
}

/// Recursively removes a directory tree, restoring access as it goes.
///
/// `fs::remove_dir_all` enumerates each directory before deleting it, so it
/// fails with `EACCES` on any directory it cannot read. OverlayFS creates an
/// internal work directory with mode `000`, which makes the standard call fail
/// and leak the whole container directory. Permissions are therefore widened on
/// the way down before the directory is opened.
pub fn force_remove_dir_all(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    // symlink_metadata does not follow symlinks, so a symlink is removed rather
    // than recursed into.
    let meta = match path.symlink_metadata() {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    if meta.is_dir() {
        let mut perms = meta.permissions();
        if perms.mode() & 0o700 != 0o700 {
            perms.set_mode(perms.mode() | 0o700);
            // Best effort: without it the delete below may still fail, but the
            // error from the delete is the more useful one to report.
            let _ = fs::set_permissions(path, perms);
        }
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            force_remove_dir_all(&entry.path())?;
        }
        return fs::remove_dir(path);
    }

    fs::remove_file(path)
}

/// Executes a command, surfacing its exit status and stderr on failure.
pub fn run_command(cmd: &str, args: &[&str]) -> Result<()> {
    let rendered = format!("{cmd} {}", args.join(" "));
    let output = Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("Failed to execute `{rendered}`"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        if stderr.is_empty() {
            bail!("`{rendered}` failed with status {}", output.status);
        }
        bail!(
            "`{rendered}` failed with status {}: {stderr}",
            output.status
        );
    }
    Ok(())
}

/// Executes a command, discarding its status. Used for idempotent cleanup where
/// failure is expected and not actionable.
pub fn try_command(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_limit_parsing() {
        assert_eq!(MemoryLimit::parse("max").unwrap(), MemoryLimit::Max);
        assert_eq!(MemoryLimit::parse("MAX").unwrap(), MemoryLimit::Max);
        assert_eq!(MemoryLimit::parse("unlimited").unwrap(), MemoryLimit::Max);
        assert_eq!(
            MemoryLimit::parse("512M").unwrap(),
            MemoryLimit::Bytes(512 * 1024 * 1024)
        );
        assert_eq!(
            MemoryLimit::parse("512mb").unwrap(),
            MemoryLimit::Bytes(512 * 1024 * 1024)
        );
        assert_eq!(
            MemoryLimit::parse("512MiB").unwrap(),
            MemoryLimit::Bytes(512 * 1024 * 1024)
        );
        assert_eq!(
            MemoryLimit::parse("1G").unwrap(),
            MemoryLimit::Bytes(1024 * 1024 * 1024)
        );
        assert_eq!(
            MemoryLimit::parse("1GiB").unwrap(),
            MemoryLimit::Bytes(1024 * 1024 * 1024)
        );
        assert_eq!(
            MemoryLimit::parse("10k").unwrap(),
            MemoryLimit::Bytes(10 * 1024)
        );
        assert_eq!(MemoryLimit::parse("100").unwrap(), MemoryLimit::Bytes(100));
    }

    #[test]
    fn test_memory_limit_rejects_garbage_and_overflow() {
        assert!(MemoryLimit::parse("abc").is_err());
        assert!(MemoryLimit::parse("").is_err());
        assert!(MemoryLimit::parse("-5M").is_err());
        // 2^64 would overflow when multiplied by the unit.
        assert!(MemoryLimit::parse("99999999999999999999G").is_err());
    }

    #[test]
    fn test_memory_cgroup_value() {
        assert_eq!(MemoryLimit::Max.cgroup_value(), "max");
        assert_eq!(MemoryLimit::Bytes(1024).cgroup_value(), "1024");
    }

    #[test]
    fn test_validate_container_name() {
        assert!(validate_container_name("web").is_ok());
        assert!(validate_container_name("web-01.prod_2").is_ok());
        assert!(validate_container_name("").is_err());
        assert!(validate_container_name(".").is_err());
        assert!(validate_container_name("..").is_err());
        // Path traversal must never be accepted.
        assert!(validate_container_name("../../etc/passwd").is_err());
        assert!(validate_container_name("a/b").is_err());
        assert!(validate_container_name("/abs").is_err());
        assert!(validate_container_name("has space").is_err());
        assert!(validate_container_name(&"x".repeat(MAX_NAME_LEN + 1)).is_err());
    }

    #[test]
    fn test_normalize_container_path() {
        assert_eq!(normalize_container_path("/data").unwrap(), "data");
        assert_eq!(normalize_container_path("data/logs").unwrap(), "data/logs");
        assert_eq!(
            normalize_container_path("/data//logs/").unwrap(),
            "data/logs"
        );
        assert_eq!(normalize_container_path("./data").unwrap(), "data");
    }

    #[test]
    fn test_normalize_container_path_rejects_traversal() {
        assert!(normalize_container_path("../etc").is_err());
        assert!(normalize_container_path("/data/../../etc").is_err());
        assert!(normalize_container_path("..").is_err());
        assert!(normalize_container_path("/").is_err());
        assert!(normalize_container_path("").is_err());
    }

    #[test]
    fn test_normalize_container_path_rejects_padded_components() {
        // Trimming these would make normalisation non-idempotent, so the value
        // the child mounts could differ from the one that was validated.
        for path in ["! /", "/data /sub", "data\u{2000}", "  /data", "/data  "] {
            let result = normalize_container_path(path);
            if let Ok(normalized) = &result {
                // If it was accepted, re-normalising must be a no-op.
                let again = normalize_container_path(normalized)
                    .unwrap_or_else(|e| panic!("{path:?} was accepted as {normalized:?} but rejected on re-normalisation: {e}"));
                assert_eq!(again, *normalized, "{path:?} normalised non-idempotently");
            }
        }
        // Unambiguously padded components are rejected outright.
        assert!(normalize_container_path("/data /sub").is_err());
        assert!(normalize_container_path("data /x").is_err());
    }

    #[test]
    fn test_normalize_container_path_is_idempotent_for_accepted_input() {
        for path in ["/data", "data/logs", "a/b/c", "-dash", "sp ace/inner"] {
            let once = normalize_container_path(path).unwrap();
            let twice = normalize_container_path(&once).unwrap();
            assert_eq!(once, twice, "{path:?} was not idempotent");
        }
    }

    #[test]
    fn test_parse_volume() {
        let v = parse_volume("/host/data:/data").unwrap();
        assert_eq!(v.source, "/host/data");
        assert_eq!(v.destination, "data");
        assert!(!v.named);
        assert!(!v.read_only);

        let v = parse_volume("mydata:/var/lib/data:ro").unwrap();
        assert_eq!(v.source, "mydata");
        assert_eq!(v.destination, "var/lib/data");
        assert!(v.named);
        assert!(v.read_only);

        let v = parse_volume("./rel:/x:rw").unwrap();
        assert!(!v.named);
    }

    #[test]
    fn test_parse_volume_rejects_invalid() {
        assert!(parse_volume("onlyonepart").is_err());
        assert!(parse_volume("").is_err());
        assert!(parse_volume(":/data").is_err());
        assert!(parse_volume("/host:../../escape").is_err());
        assert!(parse_volume("/host:/data:zz").is_err());
        assert!(parse_volume("/a:/b:c:d").is_err());
    }

    #[test]
    fn test_parse_env_assignment() {
        assert_eq!(
            parse_env_assignment("FOO=bar").unwrap(),
            ("FOO".to_string(), "bar".to_string())
        );
        assert_eq!(
            parse_env_assignment("EMPTY=").unwrap(),
            ("EMPTY".to_string(), "".to_string())
        );
        assert!(parse_env_assignment("NOEQUALS").is_err());
        assert!(parse_env_assignment("=novalue").is_err());
    }

    #[test]
    fn test_generate_veth_names() {
        let (vh1, vc1) = generate_veth_names("my-very-long-container-name-1");
        let (vh2, _) = generate_veth_names("my-very-long-container-name-2");
        assert!(vh1.len() <= 15, "vh1 length {} > 15", vh1.len());
        assert!(vc1.len() <= 15, "vc1 length {} > 15", vc1.len());
        assert!(vh1.starts_with("vh-") && vc1.starts_with("vc-"));
        assert_ne!(
            vh1, vh2,
            "Veth names collided for different container names"
        );
    }

    #[test]
    fn test_generate_veth_names_unique_for_many() {
        // Distinct names sharing a long prefix must not collide once truncated.
        let mut seen = std::collections::HashSet::new();
        for i in 0..500 {
            let (vh, _) = generate_veth_names(&format!("prefix-that-is-long-{i}"));
            assert!(seen.insert(vh), "collision at index {i}");
        }
    }

    #[test]
    fn test_force_remove_dir_all_handles_unreadable_dirs() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = std::env::temp_dir().join(format!("nuc-frda-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);

        // Reproduce the OverlayFS layout: an internal directory with mode 000.
        let locked = tmp.join("work").join("work");
        fs::create_dir_all(&locked).unwrap();
        fs::create_dir_all(tmp.join("upper/etc")).unwrap();
        fs::write(tmp.join("upper/etc/resolv.conf"), "nameserver 1.1.1.1\n").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        // The standard call cannot enumerate it.
        assert!(fs::remove_dir_all(&tmp).is_err());
        assert!(tmp.exists());

        // The helper can.
        force_remove_dir_all(&tmp).unwrap();
        assert!(!tmp.exists());
    }

    #[test]
    fn test_force_remove_dir_all_is_idempotent_and_handles_missing() {
        let missing = std::env::temp_dir().join("nuc-definitely-not-here-12345");
        // Must not error when the path does not exist.
        force_remove_dir_all(&missing).unwrap();
    }

    #[test]
    fn test_force_remove_dir_all_does_not_follow_symlinks() {
        // The symlink target lives outside the tree being removed, so that
        // "survived" proves the link was unlinked rather than recursed into.
        let base = std::env::temp_dir().join(format!("nuc-frda-link-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let tmp = base.join("tree");
        let victim = base.join("victim");
        fs::create_dir_all(&tmp).unwrap();
        fs::create_dir_all(&victim).unwrap();
        fs::write(victim.join("keep.txt"), "keep").unwrap();

        let link = tmp.join("link");
        std::os::unix::fs::symlink(&victim, &link).unwrap();

        force_remove_dir_all(&tmp).unwrap();

        assert!(!tmp.exists(), "the tree should be gone");
        assert!(
            victim.join("keep.txt").exists(),
            "the symlink target must not be deleted"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0B");
        assert_eq!(format_bytes(512), "512B");
        assert_eq!(format_bytes(1024), "1.00KiB");
        assert_eq!(format_bytes(1024 * 1024 * 3 / 2), "1.50MiB");
    }
}
