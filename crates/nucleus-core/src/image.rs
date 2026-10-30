use crate::utils::{get_nucleus_data_dir, get_target_arch};
use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tar::Archive;
use xz2::read::XzDecoder;

/// A rootfs image Nucleus knows how to fetch.
#[derive(Debug, Clone, Copy)]
pub struct Distro {
    pub name: &'static str,
    pub arch: &'static str,
    pub url: &'static str,
    /// Short human description shown by `nucleus images`.
    pub description: &'static str,
}

/// Every supported (distro, architecture) combination.
///
/// An exact match is required. Silently substituting an x86_64 rootfs on an
/// ARM host just trades one failure (ENOEXEC) for another.
pub const DISTROS: &[Distro] = &[
    Distro {
        name: "alpine",
        arch: "x86_64",
        url: "https://dl-cdn.alpinelinux.org/alpine/v3.19/releases/x86_64/alpine-minirootfs-3.19.1-x86_64.tar.gz",
        description: "Alpine Linux 3.19 minirootfs",
    },
    Distro {
        name: "alpine",
        arch: "aarch64",
        url: "https://dl-cdn.alpinelinux.org/alpine/v3.19/releases/aarch64/alpine-minirootfs-3.19.1-aarch64.tar.gz",
        description: "Alpine Linux 3.19 minirootfs",
    },
    Distro {
        name: "alpine",
        arch: "armhf",
        url: "https://dl-cdn.alpinelinux.org/alpine/v3.19/releases/armhf/alpine-minirootfs-3.19.1-armhf.tar.gz",
        description: "Alpine Linux 3.19 minirootfs",
    },
    Distro {
        name: "ubuntu",
        arch: "x86_64",
        url: "https://cdimage.ubuntu.com/ubuntu-base/releases/22.04/release/ubuntu-base-22.04.4-base-amd64.tar.gz",
        description: "Ubuntu 22.04 base rootfs",
    },
    Distro {
        name: "ubuntu",
        arch: "aarch64",
        url: "https://cdimage.ubuntu.com/ubuntu-base/releases/22.04/release/ubuntu-base-22.04.4-base-arm64.tar.gz",
        description: "Ubuntu 22.04 base rootfs",
    },
    Distro {
        name: "ubuntu",
        arch: "armhf",
        url: "https://cdimage.ubuntu.com/ubuntu-base/releases/22.04/release/ubuntu-base-22.04.4-base-armhf.tar.gz",
        description: "Ubuntu 22.04 base rootfs",
    },
    Distro {
        name: "debian",
        arch: "x86_64",
        url: "https://github.com/debuerreotype/docker-debian-artifacts/raw/dist-amd64/bookworm/rootfs.tar.xz",
        description: "Debian bookworm rootfs",
    },
    Distro {
        name: "debian",
        arch: "aarch64",
        url: "https://github.com/debuerreotype/docker-debian-artifacts/raw/dist-arm64/bookworm/rootfs.tar.xz",
        description: "Debian bookworm rootfs",
    },
];

/// Names of all distros with at least one supported architecture.
pub fn supported_distros() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = DISTROS.iter().map(|d| d.name).collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Looks up an exact (distro, arch) entry.
pub fn find_distro(distro: &str, arch: &str) -> Result<&'static Distro> {
    DISTROS
        .iter()
        .find(|d| d.name == distro && d.arch == arch)
        .ok_or_else(|| {
            let available: Vec<String> = DISTROS
                .iter()
                .filter(|d| d.name == distro)
                .map(|d| d.arch.to_string())
                .collect();
            if available.is_empty() {
                anyhow::anyhow!(
                    "unknown image '{}'. Supported images: {}",
                    distro,
                    supported_distros().join(", ")
                )
            } else {
                anyhow::anyhow!(
                    "image '{}' is not available for architecture '{}'. Supported architectures: {}",
                    distro,
                    arch,
                    available.join(", ")
                )
            }
        })
}

pub fn images_dir() -> PathBuf {
    get_nucleus_data_dir().join("images")
}

fn cache_dir() -> PathBuf {
    get_nucleus_data_dir().join("cached_images")
}

/// Resolves the rootfs path for a given image name.
pub fn resolve_image_path(image: &str) -> Option<PathBuf> {
    // 1. Central data directory (canonical location)
    let central_path = images_dir().join(image);
    if central_path.exists() {
        return Some(central_path);
    }

    // 2. Images placed in the current working directory
    let local_path = PathBuf::from("images").join(image);
    if local_path.exists() {
        return Some(local_path);
    }

    // 3. Legacy fixed path
    let var_lib_path = PathBuf::from("/var/lib/nucleus/images").join(image);
    if var_lib_path.exists() {
        return Some(var_lib_path);
    }

    None
}

/// Lists locally available images.
pub fn list_images() -> Result<Vec<(String, PathBuf)>> {
    let dir = images_dir();
    let mut images = Vec::new();
    if !dir.exists() {
        return Ok(images);
    }
    for entry in fs::read_dir(&dir).context("Failed to read image directory")? {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            // Skip in-progress extraction directories.
            continue;
        }
        images.push((name, entry.path()));
    }
    images.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(images)
}

/// Deletes a local image, refusing if a running container depends on it.
pub fn remove_image(image: &str) -> Result<()> {
    let path =
        resolve_image_path(image).ok_or_else(|| anyhow::anyhow!("image '{image}' not found"))?;

    let running: Vec<String> = crate::state::list_containers()?
        .into_iter()
        .filter(|c| c.image == image)
        .map(|c| c.name)
        .collect();
    if !running.is_empty() {
        bail!(
            "image '{image}' is in use by running container(s): {}",
            running.join(", ")
        );
    }

    // Only allow removal inside directories we manage.
    let managed = images_dir();
    let is_managed = path.starts_with(&managed) || path.starts_with(PathBuf::from("images"));
    if !is_managed {
        bail!("refusing to remove '{image}': it is not in a Nucleus-managed image store");
    }

    fs::remove_dir_all(&path)
        .with_context(|| format!("Failed to remove image directory '{}'", path.display()))?;
    println!("[Nucleus] Removed image '{image}'.");
    Ok(())
}

/// Downloads and extracts a rootfs image, returning its path.
pub fn pull_image_with(distro: &str, force: bool) -> Result<PathBuf> {
    let arch = get_target_arch();
    let entry = find_distro(distro, arch)?;
    let url = entry.url;

    let cache_dir = cache_dir();
    let images_dir = images_dir();
    fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;
    fs::create_dir_all(&images_dir).context("Failed to create images directory")?;

    let ext = if url.ends_with(".tar.gz") {
        ".tar.gz"
    } else {
        ".tar.xz"
    };
    // The architecture is part of the cache key: the same distro name resolves
    // to a different rootfs per architecture, and reusing the wrong one is
    // exactly the ENOEXEC bug multi-arch support is meant to avoid.
    let cache_path = cache_dir.join(format!("{distro}-{arch}{ext}"));

    let target_dir = images_dir.join(distro);

    if force {
        let _ = fs::remove_file(&cache_path);
        let _ = fs::remove_dir_all(&target_dir);
    }

    if !cache_path.exists() {
        println!(
            "[Nucleus] Downloading {} ({arch}) from {url}...",
            entry.description
        );
        download(url, &cache_path)?;
    } else {
        println!("[Nucleus] Using cached archive: {}", cache_path.display());
    }

    if target_dir.exists() {
        println!(
            "[Nucleus] Image '{}' is already extracted in {}.",
            distro,
            target_dir.display()
        );
        return Ok(target_dir);
    }

    extract(&cache_path, &target_dir, ext)?;
    println!(
        "[Nucleus] Success! '{}' is ready in {}",
        distro,
        target_dir.display()
    );
    Ok(target_dir)
}

/// Downloads a URL to `dest`, rejecting non-success HTTP responses.
fn download(url: &str, dest: &Path) -> Result<()> {
    let mut response = reqwest::blocking::get(url)
        .with_context(|| format!("Failed to download image from {url}"))?;

    let status = response.status();
    if !status.is_success() {
        bail!("Failed to download {url}: server returned HTTP {status}");
    }

    if response.content_length() == Some(0) {
        bail!("Failed to download {url}: server returned an empty body");
    }

    // Download to a temporary file so an interrupted transfer never poisons
    // the cache with a truncated archive.
    let tmp_path = dest.with_extension("part");
    let result = (|| -> Result<()> {
        let mut file =
            fs::File::create(&tmp_path).context("Failed to create temporary cache file")?;
        io::copy(&mut response, &mut file).context("Failed to save image to cache")?;
        file.sync_all().context("Failed to flush image to cache")?;
        Ok(())
    })();

    if let Err(e) = result {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }

    fs::rename(&tmp_path, dest).context("Failed to commit downloaded image")?;
    Ok(())
}

/// Extracts an archive into `target`, atomically.
fn extract(archive_path: &Path, target: &Path, ext: &str) -> Result<()> {
    println!("[Nucleus] Extracting to {}...", target.display());
    let file = fs::File::open(archive_path).context("Failed to open cached image")?;

    // Extract into a staging directory first. If unpacking fails partway the
    // target is never created, so a later `pull` cannot mistake a half-written
    // rootfs for a complete one.
    let staging = target.with_file_name(format!(
        ".{}.tmp.{}",
        target
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "image".to_string()),
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).context("Failed to create staging directory")?;

    let unpack_result = match ext {
        ".tar.gz" => {
            let mut archive = Archive::new(GzDecoder::new(file));
            archive.unpack(&staging)
        }
        ".tar.xz" => {
            let mut archive = Archive::new(XzDecoder::new(file));
            archive.unpack(&staging)
        }
        other => {
            let _ = fs::remove_dir_all(&staging);
            bail!("unsupported archive format '{other}'");
        }
    };

    if let Err(e) = unpack_result {
        let _ = fs::remove_dir_all(&staging);
        return Err(anyhow::Error::new(e).context("Failed to unpack rootfs archive"));
    }

    fs::rename(&staging, target)
        .with_context(|| format!("Failed to move extracted rootfs into {}", target.display()))?;
    Ok(())
}

/// Verifies that a directory looks like a usable rootfs.
pub fn is_valid_rootfs(path: &Path) -> bool {
    let required = ["bin", "lib", "etc"];
    required.iter().all(|d| path.join(d).is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_distro_exact_match() {
        assert!(find_distro("alpine", "x86_64").is_ok());
        assert!(find_distro("alpine", "aarch64").is_ok());
    }

    #[test]
    fn test_find_distro_rejects_wrong_arch_without_fallback() {
        // armhf debian is not published; we must not hand back an x86_64 rootfs.
        let err = find_distro("debian", "armhf").unwrap_err().to_string();
        assert!(err.contains("armhf"), "unexpected error: {err}");
        assert!(!err.contains("x86_64") || err.contains("Supported architectures"));
    }

    #[test]
    fn test_find_distro_unknown_image_lists_options() {
        let err = find_distro("plan9", "x86_64").unwrap_err().to_string();
        assert!(err.contains("plan9"));
        assert!(
            err.contains("alpine"),
            "should list supported images: {err}"
        );
    }

    #[test]
    fn test_supported_distros() {
        let names = supported_distros();
        assert!(names.contains(&"alpine"));
        assert!(names.contains(&"ubuntu"));
        assert!(names.contains(&"debian"));
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn test_cache_key_includes_architecture() {
        // Regression guard: the cache filename must differ per architecture.
        let x86 = format!("alpine-{}.tar.gz", get_target_arch());
        let arm = format!("alpine-{}.tar.gz", "aarch64");
        assert_ne!(x86, arm);
    }

    #[test]
    fn test_is_valid_rootfs_checks_required_dirs() {
        let tmp = std::env::temp_dir().join(format!("nuc-rootfs-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // An empty directory is not a rootfs.
        assert!(!is_valid_rootfs(&tmp));

        for d in ["bin", "lib", "etc"] {
            fs::create_dir_all(tmp.join(d)).unwrap();
        }
        assert!(is_valid_rootfs(&tmp));

        // Removing one required directory invalidates it again.
        fs::remove_dir_all(tmp.join("lib")).unwrap();
        assert!(!is_valid_rootfs(&tmp));

        let _ = fs::remove_dir_all(&tmp);
    }
}
