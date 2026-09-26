# Nucleus v0.2.0 Release Notes ⚛️

> **v0.2.0 was withdrawn.** It did not compile: `main.rs` matched
> `Commands::Stop` without the field added in the same commit, and six declared
> subcommands were never dispatched, so `cargo build` failed with `E0027` and
> `E0004`. The `v0.2.0` tag exists but the release workflow failed, so no binary
> was published. Several items below were also incorrect as written, notably
> multi-architecture support (the download cache was not keyed by architecture)
> and DNS handling. See the [changelog](../CHANGELOG.md) for the 0.3.0 release,
> which supersedes this document.

Nucleus **v0.2.0** introduces critical bug fixes, multi-architecture rootfs support, Linux filesystem standardizations, security hardening, and reliability improvements across the container runtime and orchestrator.

## 🚀 Key Highlights

### 1. Multi-Architecture RootFS Image Support
- Added runtime host architecture detection (`get_target_arch()`) supporting `x86_64`, `aarch64`, and `armhf`.
- Base images (`alpine`, `ubuntu`, `debian`) are now dynamically resolved and pulled for the host's native CPU architecture, resolving `ENOEXEC` (Exec format error) on ARM64 systems.

### 2. Full `/dev` Pseudofilesystem & Standard Device Nodes
- Implemented isolated `/dev` tmpfs provisioning inside containers.
- Mounts and creates standard device nodes: `/dev/null`, `/dev/zero`, `/dev/full`, `/dev/random`, `/dev/urandom`, and `/dev/tty`.
- Configures `/dev/pts` (`devpts`), `/dev/shm` (`tmpfs`), and standard file descriptor symlinks (`/dev/fd`, `/dev/stdin`, `/dev/stdout`, `/dev/stderr`, `/dev/ptmx`).

### 3. Container DNS Resolution Fix (`/etc/resolv.conf`)
- Fixed container DNS initialization by properly writing upstream nameservers (`1.1.1.1`, `8.8.8.8`, `8.8.4.4`), enabling internet and domain name resolution for container processes (`apk`, `apt`, `curl`, `ping`).

### 4. Standardized Linux Directory Layout
- Migrated from relative CWD paths to standard Linux hierarchy with unprivileged XDG fallbacks:
  - **Data directory**: `/var/lib/nucleus` (or `$XDG_DATA_HOME/nucleus`)
  - **Runtime directory**: `/run/nucleus` (or `$XDG_RUNTIME_DIR/nucleus`)
  - **Log directory**: `/var/log/nucleus` (or `$XDG_STATE_HOME/nucleus/logs`)
- Retains backward-compatible lookups for existing workspace setups.

### 5. Collision-Free `veth` Interface Naming
- Implemented deterministic FNV-1a hashing within Linux's 15-character `IFNAMSIZ` limit (`vh-<name>-<hash>`, `vc-<name>-<hash>`), preventing network interface collisions when container names share prefixes.

### 6. Host `iptables` Rule Idempotence
- Added rule existence checks (`-C`) before appending (`-A`) `MASQUERADE` and `FORWARD` rules, eliminating duplicate firewall rule leakage into the host kernel.

### 7. Non-Blocking Detached Mode
- Starting containers with `--detach` sets up networking, state, and cgroups, signals the child, and returns immediately with container PID and metadata without blocking.

### 8. Security Hardening
- Enforces `PR_SET_NO_NEW_PRIVS` before capability dropping and Seccomp BPF filter loading, preventing privilege escalation via setuid/setgid binaries inside unprivileged containers.
- Enhanced mount isolation with `MS_NOSUID`, `MS_NODEV`, and `MS_NOEXEC` flags on pseudofilesystems.

### 9. Unified Resource Teardown
- Centralized cleanup logic in `teardown_container` to ensure `nucleus stop` and exit events cleanly remove veth pairs, iptables rules, Cgroups v2 directories, OverlayFS layers, and state files.

### 10. Performance & CLI Improvements
- Real-time continuous delta CPU calculation in `nucleus stats` without blocking sleep intervals.
- CLI argument model updated to `NucleusArgs` with `OxideArgs` backward compatibility.

---

## 📦 Upgrade Instructions
Download prebuilt binaries from the GitHub Releases page or build from source:
```bash
cargo build --release
```
