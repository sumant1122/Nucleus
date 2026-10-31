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

### Run sandboxd (agent sandbox service)

```bash
cargo build --release -p sandboxd
sudo ./target/release/sandboxd
# Then find the port: ss -tlnp | grep sandboxd
```

`sandboxd` listens on `http://127.0.0.1:<port>` and exposes the sandbox REST API.
See the [AI Agent Sandbox](#ai-agent-sandbox-sandboxd) section for usage.


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

## AI Agent Sandbox (`sandboxd`)

Nucleus includes `sandboxd` — a privileged local service that turns the container
engine into a programmable sandbox for AI agents. Instead of one-shot `run` commands,
`sandboxd` manages **persistent sandboxes** that can be created once, started and
stopped repeatedly, and destroyed when no longer needed.

### Architecture

```
AI agent / SDK / MCP client
        │  HTTP/JSON over TCP (localhost)
        ▼
sandboxd (root, localhost only)
        │
        ▼
nucleus-core engine
        │
        ▼
namespaces / cgroups / OverlayFS / seccomp / iptables
```

`sandboxd` exposes a REST API on `http://127.0.0.1:<port>`. The port is chosen at
startup; check with `ss -tlnp | grep sandboxd`.

> **Security note:** Containers share the host kernel. `sandboxd` is suitable for
> semi-trusted agent-generated code on a single host. Do not expose `sandboxd` to
> untrusted networks or run untrusted multi-tenant code without a stronger isolation
> backend (gVisor, Kata, or Firecracker) behind the same API.

### Sandbox lifecycle

```bash
# Start the daemon
sudo ./target/release/sandboxd

# Create a sandbox (persistent config, no host resources yet)
curl -s -X POST http://127.0.0.1:8080/sandboxes \
  -H "Content-Type: application/json" \
  -d '{"image": "alpine", "network": "none"}'

# Start it (allocates networking, cgroups, spawns child)
curl -s -X POST http://127.0.0.1:8080/sandboxes/my-box/start

# Execute a command
curl -s -X POST http://127.0.0.1:8080/sandboxes/my-box/exec \
  -H "Content-Type: application/json" \
  -d '{"command": ["uname", "-a"]}'

# Stream output (SSE)
curl -s -N -X POST http://127.0.0.1:8080/sandboxes/my-box/exec/stream \
  -H "Content-Type: application/json" \
  -d '{"command": ["seq", "1", "5"]}'

# Stop and destroy
curl -s -X POST http://127.0.0.1:8080/sandboxes/my-box/stop
curl -s -X DELETE http://127.0.0.1:8080/sandboxes/my-box
```

### Network policy

Network access is **off by default** (`"network": "none"`). The container gets no
bridge or veth pair. Optional policies:

| Policy | Behavior |
|--------|----------|
| `none` | No network interface (safest) |
| `allowlist` | Bridge attached, but iptables only allows traffic to listed destinations |
| `open` | Full NAT/egress (not recommended for untrusted code) |

```bash
# Allowlist only package registries
curl -s -X POST http://127.0.0.1:8080/sandboxes \
  -d '{"image": "alpine", "network": "allowlist", "network_allowlist": ["3.214.224.0/24", "185.125.190.0/24"]}'
```

### File API

Read, write, and list files inside a sandbox's overlay filesystem:

```bash
# Read
curl -s "http://127.0.0.1:8080/sandboxes/my-box/files?path=/etc/hostname"

# Write
curl -s -X PUT "http://127.0.0.1:8080/sandboxes/my-box/files?path=/tmp/note.txt" \
  -d '{"path": "/tmp/note.txt", "content": "hello from agent"}'

# List directory
curl -s "http://127.0.0.1:8080/sandboxes/my-box/files?path=/tmp&list=true"
```

### Snapshot and reset

Save and restore overlay filesystem state:

```bash
# Snapshot before a change
curl -s -X POST "http://127.0.0.1:8080/sandboxes/my-box/snapshot?name=checkpoint"

# Make changes inside the sandbox...
curl -s -X POST http://127.0.0.1:8080/sandboxes/my-box/exec \
  -d '{"command": ["rm", "-rf", "/etc"]}'

# Restore from snapshot
curl -s -X POST "http://127.0.0.1:8080/sandboxes/my-box/reset?name=checkpoint"
```

### SDKs

**Python** (`sdks/python`):

```python
from nucleus_sdk import NucleusClient, SandboxSpec

client = NucleusClient("http://127.0.0.1:8080")
sandbox = client.create_sandbox(SandboxSpec(image="alpine", network="none"))
client.start_sandbox(sandbox.id)
result = client.exec_sandbox(sandbox.id, ["ls", "-la"])
print(result.stdout)
client.stop_sandbox(sandbox.id)
client.destroy_sandbox(sandbox.id)
client.close()
```

**TypeScript** (`sdks/typescript`):

```typescript
import { NucleusClient } from "@nucleus/sdk";

const client = new NucleusClient("http://127.0.0.1:8080");
const sandbox = await client.createSandbox({ image: "alpine" });
await client.startSandbox(sandbox.id);
const result = await client.execSandbox(sandbox.id, ["ls", "-la"]);
console.log(result.stdout);
await client.destroySandbox(sandbox.id);
```

### MCP server

`mcp/nucleus_mcp` exposes all sandbox operations as [Model Context Protocol](https://modelcontextprotocol.io/) tools:

```
sandbox_create, sandbox_list, sandbox_get, sandbox_start, sandbox_stop,
sandbox_destroy, sandbox_exec, sandbox_read_file, sandbox_write_file,
sandbox_list_files, sandbox_snapshot, sandbox_reset
```

Run it:

```bash
cd mcp
pip install -e .
python3 -m nucleus_mcp
```

Configure your MCP client (e.g. Claude Desktop):

```json
{
  "mcpServers": {
    "nucleus": {
      "command": "python3",
      "args": ["-m", "nucleus_mcp"],
      "cwd": "/path/to/Nucleus/mcp"
    }
  }
}
```

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

| Directory | Responsibility |
|-----------|----------------|
| `crates/nucleus-core/` | Engine library: namespaces, cgroups, OverlayFS, `pivot_root`, seccopus, sandbox lifecycle |
| `crates/nucleus-cli/` | `nucleus` CLI binary (delegates to `nucleus-core`) |
| `crates/nucleus-api/` | Shared API types (`SandboxSpec`, `ExecRequest`, `ExecEvent`, etc.) |
| `crates/sandboxd/` | Privileged sandbox daemon with HTTP API |
| `sdks/python/` | Python SDK (`NucleusClient`, `AsyncNucleusClient`) |
| `sdks/typescript/` | TypeScript SDK (`NucleusClient` over `fetch`) |
| `mcp/nucleus_mcp/` | MCP server exposing sandboxes as tools |

## Development

```bash
just build         # release build (includes nucleus-core, nucleus-cli, sandboxd)
just check         # fmt --check + clippy -D warnings (no side effects)
just test          # unit + property + integration tests
just ci            # everything CI runs
just test-integration   # lifecycle tests, run with sudo
```

Build `sandboxd` directly:

```bash
cargo build --release -p sandboxd
sudo ./target/release/sandboxd
```

The input parsers that form the security boundary — container names, volume
destinations, port mappings, subnets and memory strings — are covered by
property-based tests asserting that any input either errors or yields a value
satisfying the safety invariant. Two real normalisation bugs were found this way.

The unit and CLI tests run unprivileged. The container lifecycle tests need root and
self-skip otherwise; CI runs them in a separate non-blocking job.

## License

MIT OR Apache-2.0
