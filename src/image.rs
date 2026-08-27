use crate::utils::{get_nucleus_data_dir, get_target_arch};
use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tar::Archive;
use xz2::read::XzDecoder;

pub const IMAGES_DIR: &str = "images";

/// Resolves the rootfs path for a given image name, checking local CWD and system data paths.
pub fn resolve_image_path(image: &str) -> Option<PathBuf> {
    // 1. Check local CWD ./images/<image>
    let local_path = PathBuf::from("images").join(image);
    if local_path.exists() {
        return Some(local_path);
    }

    // 2. Check central data directory
    let central_path = get_nucleus_data_dir().join("images").join(image);
    if central_path.exists() {
        return Some(central_path);
    }

    // 3. Check /var/lib/nucleus/images/<image>
    let var_lib_path = PathBuf::from("/var/lib/nucleus/images").join(image);
    if var_lib_path.exists() {
        return Some(var_lib_path);
    }

    None
}

pub fn pull_image(distro: &str) -> Result<PathBuf> {
    let arch = get_target_arch();

    // Map (distro, arch) to download URLs
    let mut distros: HashMap<(&str, &str), &str> = HashMap::new();

    // Alpine
    distros.insert(
        ("alpine", "x86_64"),
        "https://dl-cdn.alpinelinux.org/alpine/v3.19/releases/x86_64/alpine-minirootfs-3.19.1-x86_64.tar.gz",
    );
    distros.insert(
        ("alpine", "aarch64"),
        "https://dl-cdn.alpinelinux.org/alpine/v3.19/releases/aarch64/alpine-minirootfs-3.19.1-aarch64.tar.gz",
    );
    distros.insert(
        ("alpine", "armhf"),
        "https://dl-cdn.alpinelinux.org/alpine/v3.19/releases/armhf/alpine-minirootfs-3.19.1-armhf.tar.gz",
    );

    // Ubuntu
    distros.insert(
        ("ubuntu", "x86_64"),
        "https://cdimage.ubuntu.com/ubuntu-base/releases/22.04/release/ubuntu-base-22.04.4-base-amd64.tar.gz",
    );
    distros.insert(
        ("ubuntu", "aarch64"),
        "https://cdimage.ubuntu.com/ubuntu-base/releases/22.04/release/ubuntu-base-22.04.4-base-arm64.tar.gz",
    );
    distros.insert(
        ("ubuntu", "armhf"),
        "https://cdimage.ubuntu.com/ubuntu-base/releases/22.04/release/ubuntu-base-22.04.4-base-armhf.tar.gz",
    );

    // Debian
    distros.insert(
        ("debian", "x86_64"),
        "https://github.com/debuerreotype/docker-debian-artifacts/raw/dist-amd64/bookworm/rootfs.tar.xz",
    );
    distros.insert(
        ("debian", "aarch64"),
        "https://github.com/debuerreotype/docker-debian-artifacts/raw/dist-arm64/bookworm/rootfs.tar.xz",
    );

    let url = distros.get(&(distro, arch)).cloned().or_else(|| {
        // Fallback for x86_64 alias if requested arch doesn't have exact match
        distros.get(&(distro, "x86_64")).cloned()
    }).context(format!(
        "Distro '{}' for architecture '{}' not supported. Supported distros: alpine, ubuntu, debian",
        distro, arch
    ))?;

    let data_dir = get_nucleus_data_dir();
    let cache_dir = data_dir.join("cached_images");
    let images_dir = data_dir.join("images");

    fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;
    fs::create_dir_all(&images_dir).context("Failed to create images directory")?;

    let ext = if url.contains(".tar.gz") {
        ".tar.gz"
    } else {
        ".tar.xz"
    };
    let cache_path = cache_dir.join(format!("{}{}", distro, ext));

    if cache_path.exists() {
        println!("[Nucleus] Using cached archive: {:?}", cache_path);
    } else {
        println!(
            "[Nucleus] Downloading {} ({}) from {}...",
            distro, arch, url
        );
        let mut response = reqwest::blocking::get(url).context("Failed to download image")?;
        let mut file = fs::File::create(&cache_path).context("Failed to create cache file")?;
        io::copy(&mut response, &mut file).context("Failed to save image to cache")?;
        println!("[Nucleus] Download complete.");
    }

    let target_dir = images_dir.join(distro);
    if target_dir.exists() {
        println!("[Nucleus] Image '{}' is already extracted in {:?}.", distro, target_dir);
        return Ok(target_dir);
    }
    fs::create_dir_all(&target_dir).context("Failed to create image target directory")?;

    println!("[Nucleus] Extracting to {:?}...", target_dir);
    let file = fs::File::open(&cache_path).context("Failed to open cached image")?;

    if ext == ".tar.gz" {
        let tar = GzDecoder::new(file);
        let mut archive = Archive::new(tar);
        archive
            .unpack(&target_dir)
            .context("Failed to unpack tar.gz rootfs")?;
    } else if ext == ".tar.xz" {
        let tar = XzDecoder::new(file);
        let mut archive = Archive::new(tar);
        archive
            .unpack(&target_dir)
            .context("Failed to unpack tar.xz rootfs")?;
    }

    println!("[Nucleus] Success! '{}' is ready in {:?}", distro, target_dir);
    Ok(target_dir)
}
