# Changelog ⚛️

All notable changes to **Nucleus** are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.2.0] - 2026-08-27

### 🚀 Highlights & Major Features
- **Multi-Architecture Image Support**: Added dynamic runtime architecture detection (`x86_64`, `aarch64`, `armhf`) when pulling base rootfs images (`alpine`, `ubuntu`, `debian`), resolving `ENOEXEC` issues on ARM64 hosts.
- **Full `/dev` Pseudofilesystem**: Implemented standard `/dev` subsystem mounting via `tmpfs`, including essential device nodes (`/dev/null`, `/dev/zero`, `/dev/full`, `/dev/random`, `/dev/urandom`, `/dev/tty`), `/dev/pts` (`devpts`), `/dev/shm` (`tmpfs`), and standard symlinks (`/dev/fd`, `/dev/stdin`, `/dev/stdout`, `/dev/stderr`, `/dev/ptmx`).
- **Standardized Directory Layout**: Normalized data, runtime, and log paths according to Linux standards with unprivileged XDG fallbacks:
  - Data directory: `/var/lib/nucleus` (or `$XDG_DATA_HOME/nucleus`)
  - Runtime directory: `/run/nucleus` (or `$XDG_RUNTIME_DIR/nucleus`)
  - Log directory: `/var/log/nucleus` (or `$XDG_STATE_HOME/nucleus/logs`)
- **Collision-Free Interface Generator**: Generates safe, unique network interface pairs (`vh-*`, `vc-*`) using FNV-1a hashing within Linux's 15-character `IFNAMSIZ` limit.
- **Non-Blocking Detached Execution**: Starting containers with `--detach` now fully establishes networking, state, and cgroups, signals the container child, and returns immediately with container metadata.
- **Unified Resource Teardown**: Introduced centralized `teardown_container` orchestration to cleanly remove `veth` interfaces, `iptables` NAT/FORWARD rules, Cgroups v2 directories, OverlayFS layers, and state files on container exit or `nucleus stop`.

### 🛡️ Security Hardening
- Added `PR_SET_NO_NEW_PRIVS` via `libc::prctl` before dropping capabilities and loading Seccomp BPF filters, preventing privilege escalation via setuid/setgid binaries inside containers.
- Enhanced mount namespace hardening with `MS_NOSUID`, `MS_NODEV`, and `MS_NOEXEC` flags on `/proc`, `/sys`, and `/dev` pseudofilesystems.

### 🐛 Bug Fixes & Correctness
- **Container DNS Resolution**: Fixed `/etc/resolv.conf` initialization to provision valid upstream nameservers (`1.1.1.1`, `8.8.8.8`, `8.8.4.4`), restoring networking for package managers (`apk`, `apt`) and utilities (`curl`, `ping`).
- **Idempotent Firewall Rules**: Added `-C` checks before appending `-A` rules to `iptables` (MASQUERADE, FORWARD), preventing host firewall rule leaks across container restarts.
- **Real-Time CPU Usage**: Optimized `nucleus stats` to compute delta CPU calculations continuously without blocking sleep intervals during streaming.
- **CLI Refactoring**: Standardized CLI argument models to `NucleusArgs` with `OxideArgs` backward-compatibility alias.

---

## [0.1.1] - 2026-05-11

### Added
- Real-time container resource monitoring via `nucleus stats` (`--stream` mode).
- Signal forwarding (`SIGINT`, `SIGTERM`) from host orchestrator to container PID 1.
- Integration tests for container lifecycle verification.
- Stop command to terminate running containers cleanly.
- Daemonless logging via `nucleus logs [--follow]`.

---

## [0.1.0] - 2026-04-20

### Added
- Initial release of Nucleus container runtime.
- Process isolation using Linux Namespaces (`user`, `pid`, `mount`, `net`, `uts`, `cgroup`).
- Resource limiting via Cgroups v2 (CPU, Memory, PIDs).
- Layered storage using OverlayFS with `pivot_root`.
- Host-driven container networking with Bridge (`br0`), IPAM, NAT, and port forwarding.
