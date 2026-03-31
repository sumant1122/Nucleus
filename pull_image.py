import subprocess
import os
import urllib.request
import shutil
import sys

# Minimal RootFS sources
DISTROS = {
    "alpine": "https://dl-cdn.alpinelinux.org/alpine/v3.19/releases/x86_64/alpine-minirootfs-3.19.1-x86_64.tar.gz",
    "ubuntu": "https://cdimage.ubuntu.com/ubuntu-base/releases/22.04/release/ubuntu-base-22.04.4-base-amd64.tar.gz",
    "debian": "https://github.com/debuerreotype/docker-debian-artifacts/raw/dist-amd64/bookworm/rootfs.tar.xz"
}

CACHE_DIR = "./cached_images"

def setup_rootfs(distro_name, target_dir="./rootfs"):
    if distro_name not in DISTROS:
        print(f"Error: Distro '{distro_name}' not supported. Choose from: {list(DISTROS.keys())}")
        return

    # Create cache dir if it doesn't exist
    if not os.path.exists(CACHE_DIR):
        os.makedirs(CACHE_DIR)

    # 1. Cleanup old rootfs
    if os.path.exists(target_dir):
        print(f"Cleaning up old {target_dir}...")
        shutil.rmtree(target_dir)
    os.makedirs(target_dir)

    # 2. Check Cache or Download
    url = DISTROS[distro_name]
    # Simple extension detection
    ext = ".tar.gz" if ".tar.gz" in url else ".tar.xz"
    cache_path = os.path.join(CACHE_DIR, f"{distro_name}{ext}")

    if os.path.exists(cache_path):
        print(f"Using cached image: {cache_path}")
    else:
        print(f"Downloading {distro_name} from {url}...")
        try:
            urllib.request.urlretrieve(url, cache_path)
            print("Download complete.")
        except Exception as e:
            print(f"Error downloading image: {e}")
            return

    # 3. Extract
    try:
        print(f"Extracting to {target_dir}...")
        subprocess.run(["tar", "-xf", cache_path, "-C", target_dir], check=True)
        print(f"Success! {distro_name} is ready in {target_dir}")
    except Exception as e:
        print(f"Error: {e}")

if __name__ == "__main__":
    name = sys.argv[1] if len(sys.argv) > 1 else "alpine"
    setup_rootfs(name)
