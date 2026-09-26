# Changelog ⚛️

All notable changes to **Nucleus** are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Added
- **`nucleus info`** reports the host capabilities Nucleus depends on: cgroup v2 and
  whether the tree is writable, OverlayFS, unprivileged user namespaces, whether a
  seccomp filter can actually be loaded (not merely whether the kernel has seccomp),
  the iptables backend, `ip_forward` writability, effective capabilities, the required
  host tools, the resolved directory layout and the local images. `--json` for
  scripting. Exits non-zero when neither privileged nor rootless mode is usable, so
  it works as a provisioning or health check.
- **`nucleus flush-firewall`** empties the Nucleus iptables chains while leaving them
  hooked, for recovering a host after an unclean shutdown.

### Changed
- **Nucleus no longer writes rules into the host's built-in firewall chains.** It owns
  `NUCLEUS-FORWARD`, `NUCLEUS-PREROUTING` and `NUCLEUS-POSTROUTING`, and inserts a
  single jump at the head of each corresponding built-in chain. This stops Nucleus
  from competing with `firewalld`/`ufw` and gives operators one place to inspect or
  flush everything it installed.

### Fixed
- **iptables rules were built with the table in the wrong position.**
  `iptables -A -t nat CHAIN` is rejected with `Bad argument`, because iptables takes
  the chain name directly after the operation flag, so every rule that specified a
  non-default table failed to apply. Container NAT, masquerading and port forwarding
  have therefore never worked. The rule helpers now take the table and chain
  separately and build `-t <table> <op> <chain> <rule>` themselves, so callers cannot
  get the order wrong. Found by the privileged CI job.
- **The required CI job never pulled a test image**, so the container integration
  tests failed with `Image 'alpine' not found` while the unit tests passed.
- **`nucleus run --detach` no longer reports success for a container that failed to
  start.** It returned 0 as soon as the host side was wired up, so a container that died
  during setup looked healthy and the real reason was only in the log file. The child
  now reports its startup outcome over a dedicated status pipe and the orchestrator
  waits for it, so a bad command is a non-zero exit with the reason inline:
  `Error: Container 'web' failed to start: Command '/etc/hostname' at /etc/hostname is
  not executable`. A short-lived but valid container is still reported as success.
- **Failures that happen before the fork now report their reason too.** Image
  resolution and spec validation run in the supervisor process, so the forked child
  never existed to report them and the orchestrator could only say the container
  "exited during startup". It now reports
  `failed to start: Image 'alpine' not found. Please run 'Nucleus pull alpine' first.`
- **The command is verified to be runnable before the container is declared started.**
  `execvp` replaces the process, so once it is called the child can no longer report
  that the exec failed, and a missing binary was indistinguishable from success. The
  target is now resolved against the container's `PATH` and checked for existence and
  an execute bit first, which catches the two most common startup failures
  deterministically.
- **The detached reaper no longer depends on the state file.** `nucleus list` prunes
  state for containers that have already exited, so a reaper reading it back would find
  nothing and skip cleanup, leaking the container's runtime directory. The teardown spec
  is now serialised into the reaper's command line, with the state file kept only as a
  fallback.
- The status pipe descriptor is now closed by the orchestrator after spawning the child.
  `nix::unistd::pipe` returns raw descriptors, so this needs an explicit `close(2)`;
  without it every failed start blocked for the full handshake timeout.
- **Container path normalisation is now idempotent.** Two bugs let the value the
  orchestrator validated differ from the value the child actually mounted, because the
  child re-normalises the spec it was handed:
  - `str::trim` is Unicode-aware, so a path ending in U+2000 was trimmed on the second
    pass but not the first. Trimming is now ASCII-only.
  - A path component with surrounding whitespace (`/data /sub`) survived the first pass
    and was stripped on the second. Such components are now rejected rather than
    silently transformed, since trimming them would mount somewhere other than what was
    checked.

  Both were found by the new property tests, not by inspection.

### Corrected
- `--tty` no longer claims to allocate a pseudo-terminal. It never did: there is no
  `openpty` call in the codebase, and a comment described behaviour that did not exist.
  The flag now only asserts that an inherited terminal is present, says so in `--help`,
  and explains the limitation when it rejects a run.

### Tests
- 98 tests, up from 67. Added `proptest`-based property tests for every input parser on
  the security boundary, asserting that arbitrary input either errors or satisfies the
  safety invariant — no panics, no silently-accepted unsafe values. Two of the parsers'
  invariants had never been true before.
- Added coverage for the startup handshake: a `--detach` run whose command cannot start
  must fail, must leave no runtime directory, and must say why. A short-lived container
  must still succeed and still be reclaimed after its state is pruned.
- Container logs are dumped into the failure message, so a container that dies during
  startup no longer produces a bare "not listed" assertion.
- Added `nucleus info` and `flush-firewall` integration coverage, including the JSON
  schema and the exit-code contract.
- Failed property cases are recorded in `proptest-regressions/` and re-run on every
  future run.
- Fixed the privileged CI job, where `sudo -E cargo` could not find cargo because- Added a regression test asserting the iptables argument order, since the same mistake
  was present in the pre-0.3.0 code and had never been exercised.
- Fixed the privileged CI job, where `sudo -E cargo` could not find cargo because
  `sudo` resets `PATH`.

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
