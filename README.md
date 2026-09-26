# Nucleus ⚛️

**Nucleus** is a minimalist container engine written in Rust. It is a compact
demonstration of how modern Linux containerisation works, built directly on kernel
primitives: namespaces, cgroups v2, OverlayFS and `pivot_root`.

> **Status: 0.3.0.** The 0.2.0 release did not compile and was withdrawn; see the
> [changelog](CHANGELOG.md) for what was broken and what has been fixed.

## Key Features

- **True PID isolation** — the container process runs as **PID 1** in its own PID
  namespace.
- **Secure filesystem** — `pivot_root` (not `chroot`) with private mount propagation.
- **Layered storage** — OverlayFS with a read-only image layer and a per-container
  writable upper layer.
- **Host-driven networking** — a Linux bridge, `veth` pairs, IPAM, NAT and port
  forwarding, configured from the host via `nsenter`.
- **Resource limits** — cgroups v2 memory, CPU and PID limits, with a swap cap so the
  memory limit cannot be bypassed.
- **Volumes** — host bind mounts and named volumes, optionally read-only.
- **Observability** — `stats` for CPU, memory, swap and PIDs, and `logs` for detached
  containers.
- **Rootless mode** — runs as an unprivileged user through user namespaces.
- **Hardening** — a runc-equivalent default capability set, a seccomp filter, a
  read-only `/sys`, `no_new_privs`, and path-traversal-proof name and mount handling.
- **No daemon** — a single binary; `--detach` spawns a small reaper that cleans up when
  the container exits.

## Requirements

| | |
|---|---|
| **OS** | Linux, kernel 4.18+ (cgroup v2 and OverlayFS required) |
| **Build** | `rustc`, `cargo`, and `libseccomp-dev` |
| **Runtime** (privileged mode) | `iptables`, `iproute2` (`ip`), `nsenter` |
| **Privileges** | root for namespaces/networking/cgroups, or `--rootless` |

`libseccomp-dev` is a **link-time** dependency:

```bash
# Debian / Ubuntu
sudo apt-get install -y libseccomp-dev
# Fedora / RHEL
sudo dnf install -y libseccomp-devel
# Arch
sudo pacman -S libseccomp
```

Nucleus is **dynamically linked** against `libseccomp`; it is not a static binary.

## Getting Started

```bash
git clone https://github.com/sumant1122/Nucleus.git
cd Nucleus
cargo build --release          # or: just build

sudo ./target/release/Nucleus pull alpine
sudo ./target/release/Nucleus run --name my-shell --image alpine /bin/sh
```

Pre-built binaries for `x86_64` and `aarch64` are published on the
[Releases page](https://github.com/sumant1122/Nucleus/releases).

## Usage

### Run a container

```bash
# IP is auto-assigned if omitted
sudo Nucleus run --name my-shell --image alpine /bin/sh
```

### Environment, working directory and volumes

```bash
sudo Nucleus run \
  --name dev-box \
  --env LOG_LEVEL=debug \
  --workdir /srv/app \
  --volumes /home/user/data:/mnt/data \
  --volumes my-db-vol:/var/lib/db \
  --volumes /etc/config:/etc/config:ro \
  /bin/sh
```

`--volumes` accepts `source:destination` and `source:destination:ro`. A source
starting with `/`, `.` or `~` is a host path; anything else names a volume in the data
directory.

### Resource limits

```bash
sudo Nucleus run \
  --name limited-box \
  --memory 512M \
  --cpus 1.5 \
  --pids-limit 128 \
  /bin/sh
```

Without `--cpus`, a container is limited to a single CPU. `--memory` accepts `512M`,
`1G`, `1GiB`, or `max` for no limit.

### Port mapping

```bash
# host:container
sudo Nucleus run --name web-app --ports 8080:80 /bin/sh

# bind address, and UDP
sudo Nucleus run --name dns --ports 127.0.0.1:5353:53/udp /bin/sh
```

Port mapping requires privileged mode; rootless containers have no network namespace.

### Networking

```bash
# Use a specific bridge, and set the subnet used when creating it
sudo Nucleus run --name web --network br1 --subnet 192.168.50.1/24 /bin/sh
```

The subnet of an existing bridge is detected and used, so `--subnet` only applies when
Nucleus creates the bridge.

### Detached execution and logs

```bash
sudo Nucleus run --name worker --detach /bin/sh -c 'while true; do echo working; sleep 5; done'
sudo Nucleus logs worker --follow
```

A detached container is reaped automatically when it exits, and its veth pair, cgroup,
port rules and overlay layers are removed.

### Inspecting and interacting

```bash
sudo Nucleus list                     # or: ps
sudo Nucleus inspect my-shell         # full state as JSON
sudo Nucleus exec my-shell -- /bin/sh  # run a command inside a running container
sudo Nucleus stats my-shell --stream  # live CPU / memory / swap / PIDs
```

### Checking host support

Nucleus is built directly on kernel primitives, so what it can do depends on the
host. `nucleus info` reports that up front:

```bash
Nucleus info            # human-readable report
Nucleus info --json     # for scripting
```

It probes cgroup v2 (and whether the tree is writable), OverlayFS, unprivileged
user namespaces, whether a seccomp filter can actually be loaded, the iptables
backend, `ip_forward` writability, effective capabilities, required host tools and
the resolved directory layout. It exits non-zero when neither privileged nor
rootless mode is usable, so it works as a provisioning check or a health check.

### Images

```bash
sudo Nucleus pull alpine
sudo Nucleus images
sudo Nucleus rmi alpine
```

Supported images are `alpine`, `ubuntu` and `debian` for `x86_64`, `aarch64` and
`armhf` (`debian` is not published for `armhf`). Pulling for an unsupported
architecture is an error rather than a silent fall back to an x86_64 rootfs. Cached
archives are keyed by architecture.

### Stopping and removing

```bash
sudo Nucleus stop my-shell            # SIGTERM, then SIGKILL after --timeout (default 10s)
sudo Nucleus stop my-shell --timeout 30
sudo Nucleus rm my-shell --force
```

`stop` delivers SIGTERM to the container's init process and escalates to SIGKILL after
the timeout. Note the kernel behaviour this inherits from Docker: a PID-namespace init
that has not installed a SIGTERM handler discards the signal, so such containers are
always escalated to SIGKILL. Install a handler (or wrap your command in `sh -c` with a
`trap`) for a genuinely graceful shutdown.

## Rootless Mode

```bash
Nucleus run --rootless --name rootless-box --image alpine /bin/sh
```

Rootless mode uses user namespaces and requires no privileges. It has real
limitations:

- **No networking.** The container gets an isolated, unconfigured network namespace.
  Port mapping is rejected.
- **No resource limits.** No cgroup is attached, so `--memory`, `--cpus` and
  `--pids-limit` are not enforced and `stats` is unavailable.
- **`/proc`, `/sys` and `/sys/fs/cgroup` are bind-mounted from the host** rather than
  fresh kernel instances, because the kernel forbids mounting a new procfs inside a
  user namespace when the host's `/proc` is not fully visible. Some host `/proc` detail
  is therefore visible inside the container.

## Networking and the Host Firewall

Nucleus never writes rules directly into the host's `FORWARD`, `PREROUTING` or
`POSTROUTING` chains. Instead it owns three chains and inserts a single jump into
each built-in chain, at the head so container traffic is seen first:

| Table | Chain |
| :--- | :--- |
| `filter` | `NUCLEUS-FORWARD` |
| `nat` | `NUCLEUS-PREROUTING` |
| `nat` | `NUCLEUS-POSTROUTING` |

This keeps Nucleus from competing with `firewalld` or `ufw`, and means everything
it installed can be inspected in one place:

```bash
sudo iptables -t nat -S NUCLEUS-PREROUTING
```

To recover a host after an unclean shutdown:

```bash
sudo Nucleus flush-firewall    # empties the three chains, leaves them hooked
```

## Security Notes

- Container names are restricted to `[A-Za-z0-9._-]` and cannot traverse paths, because
  they are used to build state file and cgroup paths.
- Volume and working-directory destinations are normalised; `..` components are
  rejected so a mount cannot escape the container root.
- The container process keeps only the runc default capability set
  (`CAP_CHOWN`, `CAP_DAC_OVERRIDE`, `CAP_FOWNER`, `CAP_FSETID`, `CAP_KILL`,
  `CAP_SETGID`, `CAP_SETUID`, `CAP_SETPCAP`, `CAP_NET_BIND_SERVICE`, `CAP_NET_RAW`,
  `CAP_SYS_CHROOT`, `CAP_MKNOD`, `CAP_AUDIT_WRITE`, `CAP_SETFCAP`). `CAP_SYS_ADMIN`,
  `CAP_SYS_PTRACE`, `CAP_NET_ADMIN` and friends are dropped from all four capability
  sets.
- Seccomp denies roughly 45 syscalls covering namespace and mount manipulation,
  tracing and eBPF, keyring, time-setting and kernel module operations. `clone3`
  returns `ENOSYS` so glibc falls back to `clone`.
- `no_new_privs` is set before the seccomp filter is loaded, so setuid binaries inside
  the container cannot regain privilege.
- `/etc/resolv.conf` inherits the host's upstream resolvers, but loopback addresses are
  dropped: a stub resolver such as systemd-resolved's `127.0.0.53` is not reachable
  from inside a container, so public resolvers are used instead.

## Comparison

Nucleus is not trying to replace Docker or Podman. It is a small, readable
implementation of the underlying mechanisms.

| | Docker / Podman | Nucleus |
| :--- | :--- | :--- |
| Scope | Full ecosystem | Isolation primitives only |
| Architecture | Daemon-based | No daemon; `--detach` spawns a reaper |
| Linking | Go, static | Rust, dynamically linked against `libseccomp` |
| Host tooling | Vendored | Uses `ip`, `iptables`, `nsenter` |
| Networking | Full CNI/plugins | Single bridge, IPAM, NAT, DNAT |
| Use case | General workloads | Learning, edge, minimal hosts |

## Project Structure

| File | Responsibility |
| :--- | :--- |
| `src/main.rs` | CLI dispatch and the `exec`, `inspect`, `list`, `images`, `rmi`, `rm` commands |
| `src/args.rs` | Command-line definitions (`clap`) |
| `src/orchestrator.rs` | Host-side setup: bridge, veth, cgroups, iptables, IPAM, teardown, reaping |
| `src/container.rs` | In-container setup: namespaces, OverlayFS, `pivot_root`, `/dev`, capabilities, seccomp |
| `net.rs` | IPv4 subnet arithmetic, port mapping parsing, iptables and bridge helpers |
| `state.rs` | Container state persistence, atomic writes, liveness detection |
| `image.rs` | Image registry, download, atomic extraction |
| `stats.rs` | cgroup-based resource statistics |
| `doctor.rs` | Host capability probing for `nucleus info` |
| `properties.rs` | Property-based tests for the input parsers |
| `utils.rs` | Name and path validation, memory parsing, directory layout |

## Development

```bash
just build         # release build
just check         # fmt --check + clippy -D warnings (no side effects)
just test          # unit + property + integration tests
just ci            # everything CI runs
just test-integration   # lifecycle tests, run with sudo
```

The input parsers that form the security boundary — container names, volume
destinations, port mappings, subnets and memory strings — are covered by
property-based tests asserting that any input either errors or yields a value
satisfying the safety invariant. Two real normalisation bugs were found this way.

The unit and CLI tests run unprivileged. The container lifecycle tests need root and
self-skip otherwise; CI runs them in a separate non-blocking job.

## License

MIT OR Apache-2.0
