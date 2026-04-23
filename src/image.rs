use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;
use tar::Archive;
use xz2::read::XzDecoder;

const CACHE_DIR: &str = "./cached_images";
const TARGET_DIR: &str = "./rootfs";

pub fn pull_image(distro: &str) -> Result<()> {
    let mut distros = HashMap::new();
    distros.insert(
        "alpine",
        "https://dl-cdn.alpinelinux.org/alpine/v3.19/releases/x86_64/alpine-minirootfs-3.19.1-x86_64.tar.gz",
    );
    distros.insert(
        "ubuntu",
        "https://cdimage.ubuntu.com/ubuntu-base/releases/22.04/release/ubuntu-base-22.04.4-base-amd64.tar.gz",
    );
    distros.insert(
        "debian",
        "https://github.com/debuerreotype/docker-debian-artifacts/raw/dist-amd64/bookworm/rootfs.tar.xz",
    );

    let url = distros.get(distro).context(format!(
        "Distro '{}' not supported. Supported: {:?}",
        distro,
        distros.keys()
    ))?;

    if !Path::new(CACHE_DIR).exists() {
        fs::create_dir_all(CACHE_DIR).context("Failed to create cache directory")?;
    }

    let ext = if url.contains(".tar.gz") {
        ".tar.gz"
    } else {
        ".tar.xz"
    };
    let cache_path = format!("{}/{}{}", CACHE_DIR, distro, ext);

    if Path::new(&cache_path).exists() {
        println!("[Nucleus] Using cached image: {}", cache_path);
    } else {
        println!("[Nucleus] Downloading {} from {}...", distro, url);
        let mut response = reqwest::blocking::get(*url).context("Failed to download image")?;
        let mut file = fs::File::create(&cache_path).context("Failed to create cache file")?;
        io::copy(&mut response, &mut file).context("Failed to save image to cache")?;
        println!("[Nucleus] Download complete.");
    }

    if Path::new(TARGET_DIR).exists() {
        println!("[Nucleus] Cleaning up old {}...", TARGET_DIR);
        fs::remove_dir_all(TARGET_DIR).ok();
    }
    fs::create_dir_all(TARGET_DIR).context("Failed to create rootfs directory")?;

    println!("[Nucleus] Extracting to {}...", TARGET_DIR);
    let file = fs::File::open(&cache_path).context("Failed to open cached image")?;
    
    if cache_path.ends_with(".tar.gz") {
        let tar = GzDecoder::new(file);
        let mut archive = Archive::new(tar);
        archive.unpack(TARGET_DIR).context("Failed to unpack tar.gz")?;
    } else if cache_path.ends_with(".tar.xz") {
        let tar = XzDecoder::new(file);
        let mut archive = Archive::new(tar);
        archive.unpack(TARGET_DIR).context("Failed to unpack tar.xz")?;
    }

    println!("[Nucleus] Success! {} is ready in {}", distro, TARGET_DIR);
    Ok(())
}
