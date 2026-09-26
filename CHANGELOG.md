# Changelog ⚛️

All notable changes to **Nucleus** are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.3.0] - 2026-09-26

This release repairs the build breakage and resource leaks introduced in 0.2.0 and
closes several security gaps. **0.2.0 never compiled**; the `v0.2.0` tag exists but no
binary was published from it.

### 🐛 Build & CLI Correctness
- **Fixed the build.** `main.rs` matched `Commands::Stop` without the new `timeout`
  field and did not handle six subcommands, so `cargo build` failed with `E0027` and
  `E0004`. CI had been red since 2026-04-30.
- **Fixed a `clap` flag collision.** `-i` was claimed by `image`, `ip` *and*
  `interactive`, which panicked at runtime in debug builds and silently mis-bound in
  release builds. `image` and `ip` are now long-only (`--image`, `--ip`).
- **Implemented `exec`, `inspect`, `ps`, `images`, `rmi` and `rm`**, which were
  declared in the CLI but never dispatched, so they appeared in `--help` and did
  nothing.
- **Removed the dead `OxideArgs` alias.**

### 🔧 Resource Leaks
- **Detached containers are now reaped.** `--detach` returned before the child exited
  and nothing ever reclaimed the veth pair, cgroup, iptables rules or overlay layers.
  A hidden `internal-reaper` subcommand now waits for the container and tears it down.
- **Error paths clean up.** A failure during bridge, veth, cgroup or iptables setup
  returned early and left a half-built container behind. A `CleanupGuard` now tears
  down on any exit path.
- **`nucleus stop` now removes published port rules.** State did not record port
  mappings, so DNAT/FORWARD rules leaked on every stop.
- **Cgroups are actually removable.** `remove_dir_all` cannot delete cgroupfs
  pseudo-files, so directories leaked forever. Teardown now uses `cgroup.kill` plus
  `rmdir` with backoff.
- **`stop` reclaims resources on graceful exit**, not only after SIGKILL.

### 🛡️ Security
- **Container names are validated.** A name flows into the state file path and the
  cgroup path; `--name ../../etc/x` escaped both. Names are now restricted to
  `[A-Za-z0-9._-]`, at most 64 characters, and cannot be `.` or `..`.
- **Volume and workdir paths are normalized.** `--volume /tmp/x:../../escape` escaped
  the container rootfs onto the host. `..` components are now rejected outright.
- **Capabilities are restricted across all four sets.** Only the inheritable and
  bounding sets were pruned, leaving effective and permitted intact, so containers ran
  with far more privilege than intended. The process is now reduced to the runc default
  capability set.
- **Seccomp filter expanded** from 6 denied syscalls to ~45, adding namespace
  manipulation (`setns`, `unshare`, `pivot_root`, `chroot`), tracing (`ptrace`, `bpf`,
  `perf_event_open`), keyring, time-setting and module syscalls. `clone3` returns
  `ENOSYS` so glibc falls back to `clone`.
- **Loopback DNS resolvers are no longer copied into containers.**
  systemd-resolved points `/etc/resolv.conf` at `127.0.0.53`, a stub that is
  unreachable from a container. Real upstream resolvers are still inherited.
- **Rootless mode no longer leaks host `/proc`.** `/proc`, `/sys` and
  `/sys/fs/cgroup` are bind-mounted before `pivot_root` rather than after, where the
  bind captured the empty directory inside the image.

### ✅ Previously Ignored Flags
- `--cpus` is honoured. It was parsed and discarded; `cpu.max` was hardcoded to
  `max 100000`, silently capping every container at one core.
- `--pids-limit` is honoured. It was parsed and discarded; `pids.max` was hardcoded to
  `max`.
- `--env` and `--workdir` are now applied inside the container.
- Invalid values for these flags are rejected up front, before any namespace or host
  resource is created.

### 🌐 Networking
- **The subnet is no longer hardcoded.** `10.0.0.0/24` was used in seven places,
  including the default route and MASQUERADE rule, even when `--network` selected a
  different bridge. The subnet is now read from the bridge, with a new `--subnet` flag
  controlling what a newly created bridge gets.
- **Port mappings are parsed strictly** and support `[ip:][host:]container[/proto]`.
  Previously any mapping that was not exactly `host:container` was silently discarded;
  duplicates are now rejected.
- `--ip` is validated against the subnet and checked for conflicts with the gateway,
  the network/broadcast addresses, and running containers.
- `memory.swap.max` is set alongside `memory.max`, which was otherwise trivially
  bypassable via swap.

### 🗄 State
- State now records ports, image, command, volumes, resource limits, subnet, gateway,
  cgroup path, the container's real init PID, and a start timestamp, which is what
  makes `inspect` and correct teardown possible.
- State files are written atomically (temp file plus rename).
- **Liveness checks no longer trust a bare `/proc/<pid>` existence test**, which
  reported dead containers as running forever once a PID was recycled. The process
  command line is now verified as well.
- State written by 0.2.0 still loads; new fields default.
- `nucleus list` reclaims stale state files instead of leaking them silently.

### 🧹 Behaviour
- `nucleus stop` implements real graceful shutdown: SIGTERM, wait for `--timeout`,
  then SIGKILL. The signal is delivered to the container's own init process by its
  host-visible PID. Note that, as with Docker, the kernel discards SIGTERM sent to a
  PID-namespace init that has not installed a handler, so such containers are always
  escalated to SIGKILL.
- `nucleus stop` no longer claims success while the container is still running.
- `image pull` verifies the HTTP status, downloads to a temporary file, and extracts
  atomically. A failed extraction previously left a partial directory that the next
  `pull` reported as "already extracted".
- **The image cache is keyed by architecture.** The cache filename omitted the
  architecture, so pulling on a second architecture reused the wrong rootfs and hit
  the `ENOEXEC` failure that 0.2.0's multi-arch support was meant to fix.
- **An unavailable architecture is now an error** instead of a silent fall back to an
  x86_64 rootfs.
- `stats` reports a readable cgroup path from state, adds swap usage, uses binary
  units, labels CPU as relative to a single core, and explains that rootless
  containers have no cgroup.
- `images` shows sizes; `rmi` refuses to delete an image in use.
- `logs` resolves its path through a single helper instead of duplicating XDG logic.
- Invalid command-line values are validated before the root check, so a typo is
  reported as a typo regardless of the caller's uid.

### 🧪 Tests
- 62 tests, up from 5. The integration suite now exercises the CLI and all input
  validation without root, so CI verifies real behaviour instead of skipping.
- New tests cover name validation, path-traversal rejection, port and volume parsing,
  subnet arithmetic, memory-limit parsing and overflow, veth-name collisions, state
  round-trips, 0.2.0 state compatibility, and the seccomp/capability allowlists.
- `just test` no longer depends on the mutating `fmt` recipe, and `just ci` is
  side-effect free.
- CI now builds with `--locked`, lints `--all-targets --all-features`, caches
  dependencies, and adds a privileged job for the container lifecycle tests.

### 📚 Documentation
- Removed inaccurate claims: Nucleus is dynamically linked, requires `libseccomp-dev`,
  `iptables` and `iproute2`, and depends on kernel namespaces and cgroup v2. The
  `python3` prerequisite and the "statically linked" and "~2MB" claims were wrong.
- Documented every subcommand, the stop/SIGKILL escalation behaviour, rootless
  limitations, and distribution-specific install steps.

## [0.2.0] - 2026-08-27

> **Withdrawn:** this release did not compile. See 0.3.0.

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
