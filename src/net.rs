use crate::utils::{run_command, try_command};
use anyhow::{Context, Result, bail};
use std::fmt;
use std::process::Command;

/// Default subnet used when creating a new bridge.
pub const DEFAULT_SUBNET: &str = "10.0.0.1/24";

/// A parsed IPv4 CIDR block, stored as a network address plus prefix length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv4Net {
    addr: u32,
    prefix: u8,
}

impl Ipv4Net {
    /// Parses a CIDR string such as `10.0.0.1/24`. Host bits are masked off.
    pub fn parse_cidr(s: &str) -> Result<Self> {
        let (addr, prefix) = s.split_once('/').ok_or_else(|| {
            anyhow::anyhow!("invalid subnet '{s}': expected CIDR like '10.0.0.1/24'")
        })?;
        Self::new(
            parse_ipv4(addr)?,
            prefix.parse::<u8>().context("invalid prefix length")?,
        )
    }

    pub fn new(addr: u32, prefix: u8) -> Result<Self> {
        if prefix > 32 {
            bail!("invalid prefix length /{prefix}: must be 0-32");
        }
        if prefix < 8 {
            bail!("invalid prefix length /{prefix}: Nucleus requires at least a /8");
        }
        let mask = mask_for(prefix);
        Ok(Self {
            addr: addr & mask,
            prefix,
        })
    }

    pub fn prefix(&self) -> u8 {
        self.prefix
    }

    pub fn network(&self) -> u32 {
        self.addr
    }

    pub fn broadcast(&self) -> u32 {
        self.addr | !mask_for(self.prefix)
    }

    /// First assignable host address (network address + 1).
    pub fn first_host(&self) -> u32 {
        self.addr.wrapping_add(1)
    }

    /// Last assignable host address (broadcast address - 1).
    pub fn last_host(&self) -> u32 {
        self.broadcast().wrapping_sub(1)
    }

    pub fn contains(&self, ip: u32) -> bool {
        let mask = mask_for(self.prefix);
        (ip & mask) == self.addr
    }

    /// The gateway address, conventionally the first host address.
    pub fn gateway(&self) -> u32 {
        self.first_host()
    }

    /// Human-readable CIDR form.
    pub fn cidr(&self) -> String {
        format!("{}/{}", u32_to_ip(self.addr), self.prefix)
    }

    /// Address of the bridge itself, i.e. the gateway.
    pub fn gateway_str(&self) -> String {
        u32_to_ip(self.gateway())
    }
}

impl fmt::Display for Ipv4Net {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.cidr())
    }
}

fn mask_for(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix as u32)
    }
}

/// Parses a dotted-quad IPv4 address into a host-order integer.
pub fn parse_ipv4(s: &str) -> Result<u32> {
    let mut octets = [0u32; 4];
    let mut count = 0;
    for part in s.trim().split('.') {
        if count == 4 {
            bail!("invalid IPv4 address '{s}': too many octets");
        }
        let octet: u32 = part
            .parse()
            .with_context(|| format!("invalid IPv4 address '{s}'"))?;
        if octet > 255 {
            bail!("invalid IPv4 address '{s}': octet {octet} out of range");
        }
        octets[count] = octet;
        count += 1;
    }
    if count != 4 {
        bail!("invalid IPv4 address '{s}': expected 4 octets");
    }
    Ok((octets[0] << 24) | (octets[1] << 16) | (octets[2] << 8) | octets[3])
}

/// Formats a host-order integer as a dotted-quad IPv4 address.
pub fn u32_to_ip(ip: u32) -> String {
    format!(
        "{}.{}.{}.{}",
        (ip >> 24) & 0xff,
        (ip >> 16) & 0xff,
        (ip >> 8) & 0xff,
        ip & 0xff
    )
}

/// A host-to-container port forward.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PortMapping {
    /// Address to bind on the host. `None` means all interfaces.
    #[serde(default)]
    pub host_ip: Option<String>,
    pub host_port: u16,
    pub container_port: u16,
    /// Either `tcp` or `udp`.
    pub protocol: String,
}

impl fmt::Display for PortMapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.host_ip {
            Some(ip) => write!(
                f,
                "{}:{}:{}/{}",
                ip, self.host_port, self.container_port, self.protocol
            ),
            None => write!(
                f,
                "{}:{}/{}",
                self.host_port, self.container_port, self.protocol
            ),
        }
    }
}

fn parse_port(value: &str, spec: &str) -> Result<u16> {
    let port: u32 = value
        .parse()
        .with_context(|| format!("invalid port mapping '{spec}': '{value}' is not a number"))?;
    if port == 0 || port > 65535 {
        bail!("invalid port mapping '{spec}': port must be between 1 and 65535");
    }
    Ok(port as u16)
}

/// Parses a port mapping.
///
/// Accepted forms:
/// - `host:container`
/// - `ip:host:container`
/// - `container` (host port is the same as the container port)
/// - any of the above with a `/tcp` or `/udp` suffix
pub fn parse_port_mapping(spec: &str) -> Result<PortMapping> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        bail!("port mapping must not be empty");
    }

    let (body, protocol) = match trimmed.split_once('/') {
        Some((body, proto)) => {
            let proto = proto.to_ascii_lowercase();
            if proto != "tcp" && proto != "udp" {
                bail!(
                    "invalid port mapping '{spec}': unknown protocol '{proto}' (expected tcp or udp)"
                );
            }
            (body, proto)
        }
        None => (trimmed, "tcp".to_string()),
    };

    let parts: Vec<&str> = body.split(':').collect();
    let mapping = match parts.as_slice() {
        [container] => PortMapping {
            host_ip: None,
            host_port: parse_port(container, spec)?,
            container_port: parse_port(container, spec)?,
            protocol,
        },
        [host, container] => PortMapping {
            host_ip: None,
            host_port: parse_port(host, spec)?,
            container_port: parse_port(container, spec)?,
            protocol,
        },
        [ip, host, container] => {
            // Validate eagerly so a typo fails at parse time, not at iptables time.
            parse_ipv4(ip).with_context(|| format!("invalid port mapping '{spec}'"))?;
            PortMapping {
                host_ip: Some((*ip).to_string()),
                host_port: parse_port(host, spec)?,
                container_port: parse_port(container, spec)?,
                protocol,
            }
        }
        _ => bail!("invalid port mapping '{spec}': expected 'host:container'"),
    };

    Ok(mapping)
}

/// Detects the subnet already configured on a bridge interface.
///
/// Returns `None` if the bridge does not exist or has no IPv4 address.
pub fn detect_bridge_subnet(bridge: &str) -> Option<Ipv4Net> {
    let output = Command::new("ip")
        .args(["-4", "-o", "addr", "show", "dev", bridge])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    // Output looks like: "4: br0    inet 10.0.0.1/24 scope global br0\       valid_lft forever"
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(idx) = line.find("inet ") {
            let cidr = line[idx + 5..].split_whitespace().next()?;
            if let Ok(net) = Ipv4Net::parse_cidr(cidr) {
                return Some(net);
            }
        }
    }
    None
}

/// Creates the bridge if absent and brings it up, then applies `subnet`.
///
/// The subnet is only applied to a freshly created bridge; an existing bridge
/// keeps whatever address it already has.
pub fn ensure_bridge(bridge: &str, subnet: &Ipv4Net) -> Result<()> {
    let exists = Command::new("ip")
        .args(["link", "show", "dev", bridge])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !exists {
        run_command("ip", &["link", "add", "bridge", bridge, "name", bridge])
            .or_else(|_| run_command("ip", &["link", "add", bridge, "type", "bridge"]))?;

        // The bridge address must be the subnet gateway, not the raw CIDR host.
        let addr = format!("{}/{}", subnet.gateway_str(), subnet.prefix());
        run_command("ip", &["addr", "add", &addr, "dev", bridge])?;
    }

    run_command("ip", &["link", "set", bridge, "up"])?;
    Ok(())
}

/// Enables IPv4 forwarding on the host. Required for container egress.
pub fn enable_ip_forwarding() -> Result<()> {
    if std::fs::write("/proc/sys/net/ipv4/ip_forward", "1").is_ok() {
        return Ok(());
    }
    // Some hardened kernels mount /proc/sys read-only; fall back to sysctl(8).
    run_command("sysctl", &["-w", "net.ipv4.ip_forward=1"])
        .context("Failed to enable IPv4 forwarding")
}

/// Runs `iptables` with the given arguments.
fn iptables(args: &[String]) -> Result<()> {
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    run_command("iptables", &borrowed)
}

/// Adds an iptables rule only if an identical rule is not already present.
pub fn ensure_iptables_rule(args: &[String]) -> Result<()> {
    let mut check = Vec::with_capacity(args.len() + 1);
    check.push("-C".to_string());
    check.extend_from_slice(args);

    let borrowed: Vec<&str> = check.iter().map(String::as_str).collect();
    if try_command("iptables", &borrowed) {
        return Ok(());
    }

    let mut add = Vec::with_capacity(args.len() + 1);
    add.push("-A".to_string());
    add.extend_from_slice(args);
    iptables(&add)
}

/// Removes an iptables rule, ignoring the error when it is already absent.
pub fn remove_iptables_rule(args: &[String]) {
    let mut del = Vec::with_capacity(args.len() + 1);
    del.push("-D".to_string());
    del.extend_from_slice(args);

    let borrowed: Vec<&str> = del.iter().map(String::as_str).collect();
    try_command("iptables", &borrowed);
}

fn s(v: &str) -> String {
    v.to_string()
}

/// Installs the NAT and forwarding rules needed for container egress.
pub fn ensure_host_nat(subnet: &Ipv4Net, bridge: &str) -> Result<()> {
    let subnet_cidr = subnet.cidr();

    ensure_iptables_rule(&[
        s("-t"),
        s("nat"),
        s("POSTROUTING"),
        s("-s"),
        subnet_cidr,
        s("!"),
        s("-o"),
        bridge.to_string(),
        s("-j"),
        s("MASQUERADE"),
    ])?;
    ensure_iptables_rule(&[s("-i"), bridge.to_string(), s("-j"), s("ACCEPT")])?;
    ensure_iptables_rule(&[s("-o"), bridge.to_string(), s("-j"), s("ACCEPT")])?;
    // Allow container-to-container traffic within the bridge.
    ensure_iptables_rule(&[
        s("-i"),
        bridge.to_string(),
        s("-o"),
        bridge.to_string(),
        s("-j"),
        s("ACCEPT"),
    ])?;
    Ok(())
}

/// Builds the `-p <proto> [-d <ip>] --dport <port>` match for a mapping.
fn port_match(mapping: &PortMapping, port: u16, dest_ip: Option<&str>) -> Vec<String> {
    let mut args = vec![s("-p"), mapping.protocol.clone()];
    if let Some(ip) = dest_ip {
        args.push(s("-d"));
        args.push(ip.to_string());
    }
    args.push(s("--dport"));
    args.push(port.to_string());
    args
}

/// Publishes a container port on the host via DNAT.
pub fn publish_port(mapping: &PortMapping, container_ip: &str) -> Result<()> {
    let destination = format!("{container_ip}:{}", mapping.container_port);

    // Accept the DNATed traffic in the forward chain.
    let mut forward = vec![s("FORWARD")];
    forward.extend(port_match(
        mapping,
        mapping.container_port,
        Some(container_ip),
    ));
    forward.extend([
        s("-m"),
        s("state"),
        s("--state"),
        s("NEW,ESTABLISHED,RELATED"),
        s("-j"),
        s("ACCEPT"),
    ]);
    ensure_iptables_rule(&forward)?;

    // Rewrite the destination address on the way in.
    let mut dnat = vec![s("-t"), s("nat"), s("PREROUTING")];
    dnat.extend(port_match(
        mapping,
        mapping.host_port,
        mapping.host_ip.as_deref(),
    ));
    dnat.extend([s("-j"), s("DNAT"), s("--to-destination"), destination]);
    ensure_iptables_rule(&dnat)?;
    Ok(())
}

/// Removes the iptables rules created by [`publish_port`].
pub fn unpublish_port(mapping: &PortMapping, container_ip: &str) {
    let destination = format!("{container_ip}:{}", mapping.container_port);

    let mut forward = vec![s("-D"), s("FORWARD")];
    forward.extend(port_match(
        mapping,
        mapping.container_port,
        Some(container_ip),
    ));
    forward.extend([
        s("-m"),
        s("state"),
        s("--state"),
        s("NEW,ESTABLISHED,RELATED"),
        s("-j"),
        s("ACCEPT"),
    ]);
    remove_iptables_rule(&forward);

    let mut dnat = vec![s("-t"), s("nat"), s("-D"), s("PREROUTING")];
    dnat.extend(port_match(
        mapping,
        mapping.host_port,
        mapping.host_ip.as_deref(),
    ));
    dnat.extend([s("-j"), s("DNAT"), s("--to-destination"), destination]);
    remove_iptables_rule(&dnat);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cidr() {
        let net = Ipv4Net::parse_cidr("10.0.0.1/24").unwrap();
        assert_eq!(net.network(), parse_ipv4("10.0.0.0").unwrap());
        assert_eq!(net.gateway_str(), "10.0.0.1");
        assert_eq!(net.broadcast(), parse_ipv4("10.0.0.255").unwrap());
        assert_eq!(net.cidr(), "10.0.0.0/24");
    }

    #[test]
    fn test_parse_cidr_masks_host_bits() {
        // The gateway address must not become the network address.
        let net = Ipv4Net::parse_cidr("192.168.5.7/24").unwrap();
        assert_eq!(net.cidr(), "192.168.5.0/24");
        assert_eq!(net.gateway_str(), "192.168.5.1");
    }

    #[test]
    fn test_parse_cidr_rejects_invalid() {
        assert!(Ipv4Net::parse_cidr("10.0.0.1").is_err());
        assert!(Ipv4Net::parse_cidr("10.0.0.1/33").is_err());
        assert!(Ipv4Net::parse_cidr("10.0.0.1/4").is_err());
        assert!(Ipv4Net::parse_cidr("10.0.0.256/24").is_err());
        assert!(Ipv4Net::parse_cidr("not-an-ip/24").is_err());
    }

    #[test]
    fn test_subnet_containment() {
        let net = Ipv4Net::parse_cidr("10.0.0.1/24").unwrap();
        let contains = |s: &str| net.contains(parse_ipv4(s).unwrap());
        assert!(contains("10.0.0.1"));
        assert!(contains("10.0.0.254"));
        assert!(!contains("10.0.1.1"));
        assert!(!contains("192.168.0.1"));
    }

    #[test]
    fn test_subnet_host_range_excludes_network_and_broadcast() {
        let net = Ipv4Net::parse_cidr("10.0.0.1/29").unwrap();
        assert_eq!(u32_to_ip(net.first_host()), "10.0.0.1");
        assert_eq!(u32_to_ip(net.last_host()), "10.0.0.6");
    }

    #[test]
    fn test_ipv4_roundtrip() {
        for s in ["0.0.0.0", "10.0.0.1", "192.168.1.255", "255.255.255.255"] {
            assert_eq!(u32_to_ip(parse_ipv4(s).unwrap()), s);
        }
    }

    #[test]
    fn test_parse_port_mapping_forms() {
        let m = parse_port_mapping("8080:80").unwrap();
        assert_eq!(m.host_ip, None);
        assert_eq!(m.host_port, 8080);
        assert_eq!(m.container_port, 80);
        assert_eq!(m.protocol, "tcp");

        let m = parse_port_mapping("127.0.0.1:8080:80").unwrap();
        assert_eq!(m.host_ip.as_deref(), Some("127.0.0.1"));

        let m = parse_port_mapping("53:53/udp").unwrap();
        assert_eq!(m.protocol, "udp");
        assert_eq!(m.host_port, 53);
        assert_eq!(m.container_port, 53);

        let m = parse_port_mapping("80").unwrap();
        assert_eq!(m.host_port, 80);
        assert_eq!(m.container_port, 80);
    }

    #[test]
    fn test_parse_port_mapping_rejects_invalid() {
        assert!(parse_port_mapping("").is_err());
        assert!(parse_port_mapping("0:80").is_err());
        assert!(parse_port_mapping("80:0").is_err());
        assert!(parse_port_mapping("70000:80").is_err());
        assert!(parse_port_mapping("abc:80").is_err());
        assert!(parse_port_mapping("80:80/sctp").is_err());
        assert!(parse_port_mapping("999.1.1.1:80:80").is_err());
        assert!(parse_port_mapping("1:2:3:4").is_err());
    }

    #[test]
    fn test_port_mapping_display_roundtrip() {
        for spec in ["8080:80", "127.0.0.1:8080:80", "53:53/udp"] {
            let m = parse_port_mapping(spec).unwrap();
            assert_eq!(parse_port_mapping(&m.to_string()).unwrap(), m);
        }
    }
}
