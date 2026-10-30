//! Property-based tests for the input parsers that form Nucleus' security
//! boundary.
//!
//! Container names, volume destinations, port mappings, subnets and memory
//! strings are all attacker-influenced in the sense that they decide which paths
//! get opened and which rules get installed. Three of the bugs fixed in 0.3.0
//! were path-traversal or silent-acceptance bugs in exactly these functions.
//!
//! The contract asserted throughout is:
//!
//! > For **any** input, the parser either returns an error, or returns a value
//! > that satisfies the security invariant.
//!
//! There is no third option, in particular no panic and no silently-accepted
//! unsafe value. `proptest` explores far more inputs than the example-based tests
//! and shrinks any counterexample to a minimal reproducer.

use crate::net::{Ipv4Net, parse_ipv4, parse_port_mapping, u32_to_ip};
use crate::utils::{
    MAX_NAME_LEN, MemoryLimit, generate_veth_names, normalize_container_path, parse_env_assignment,
    parse_volume, validate_container_name,
};
use proptest::prelude::*;

/// Characters the name validator is documented to permit.
fn name_char() -> impl Strategy<Value = char> {
    prop_oneof![
        Just('a'),
        Just('Z'),
        Just('0'),
        Just('9'),
        Just('_'),
        Just('-'),
        Just('.'),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// A name is only accepted if it cannot escape the state or cgroup path.
    #[test]
    fn accepted_names_cannot_traverse(name in any::<String>()) {
        if let Ok(()) = validate_container_name(&name) {
            prop_assert!(!name.contains('/'), "accepted a name with a slash: {name:?}");
            prop_assert!(!name.contains('\\'), "accepted a backslash: {name:?}");
            prop_assert!(name != ".", "accepted '.'");
            prop_assert!(name != "..", "accepted '..'");
            prop_assert!(name.chars().count() <= MAX_NAME_LEN);
            prop_assert!(
                name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'),
                "accepted an out-of-charset name: {name:?}"
            );
            // Nothing that could be read as a relative path component.
            prop_assert!(!name.split('/').any(|c| c == ".."), "{name:?}");
        }
    }

    /// The reverse direction: anything built from the documented alphabet and
    /// length is accepted, so the validator is not silently over-restrictive.
    #[test]
    fn well_formed_names_are_accepted(
        first in name_char(),
        rest in prop::collection::vec(name_char(), 0..20),
    ) {
        let name: String = std::iter::once(first).chain(rest).collect();
        // "." and ".." are deliberately reserved and rejected.
        prop_assume!(name != "." && name != "..");
        prop_assert!(validate_container_name(&name).is_ok(), "rejected {name:?}");
    }

    /// A normalised container path is always relative and never traverses.
    #[test]
    fn normalized_paths_cannot_escape(path in any::<String>()) {
        if let Ok(normalized) = normalize_container_path(&path) {
            prop_assert!(!normalized.is_empty(), "empty result from {path:?}");
            prop_assert!(!normalized.starts_with('/'), "absolute result from {path:?}: {normalized:?}");
            prop_assert!(!normalized.contains('\0'), "NUL in result from {path:?}");
            for component in normalized.split('/') {
                prop_assert!(component != "..", "'..' survived normalisation of {path:?}");
                prop_assert!(!component.is_empty(), "empty component from {path:?}");
            }
            // Re-normalising an already-normalised path is a no-op, which shows
            // the output is in the normal form the function claims to produce.
            let again = normalize_container_path(&normalized);
            prop_assert_eq!(again.unwrap(), normalized);
        }
    }

    /// Normalisation is idempotent for any input it accepts.
    #[test]
    fn normalization_is_idempotent(path in any::<String>()) {
        if let Ok(once) = normalize_container_path(&path) {
            let twice = normalize_container_path(&once).unwrap();
            prop_assert_eq!(once, twice);
        }
    }

    /// An accepted volume's destination must satisfy the path invariant, since
    /// it is joined onto the container's merged rootfs.
    #[test]
    fn accepted_volumes_have_safe_destinations(spec in any::<String>()) {
        if let Ok(volume) = parse_volume(&spec) {
            prop_assert!(!volume.destination.is_empty());
            prop_assert!(!volume.destination.starts_with('/'));
            prop_assert!(
                !volume.destination.split('/').any(|c| c == ".."),
                "volume {spec:?} kept a '..' component: {:?}",
                volume.destination
            );
            prop_assert!(!volume.source.contains('\0'), "NUL in source of {spec:?}");
            prop_assert!(!volume.destination.contains('\0'));
            // A named volume is one that is not obviously a host path.
            prop_assert_eq!(
                volume.named,
                !(volume.source.starts_with('/')
                    || volume.source.starts_with('.')
                    || volume.source.starts_with('~')),
                "named/source classification disagreed for {:?}",
                spec
            );
        }
    }

    /// Accepted environment assignments are always well formed.
    #[test]
    fn accepted_env_assignments_are_well_formed(spec in any::<String>()) {
        if let Ok((key, value)) = parse_env_assignment(&spec) {
            prop_assert!(!key.is_empty(), "empty key from {spec:?}");
            prop_assert!(!key.contains('\0'));
            prop_assert!(!value.contains('\0'));
            // ASCII whitespace is stripped; other Unicode whitespace is kept
            // verbatim so the transform stays idempotent.
            prop_assert!(
                !key.starts_with(|c: char| c.is_ascii_whitespace())
                    && !key.ends_with(|c: char| c.is_ascii_whitespace()),
                "key not ASCII-trimmed from {:?}",
                spec
            );
            prop_assert!(!key.contains('='), "key contains '=' from {:?}", spec);
        }
    }

    /// A memory limit either is unlimited or round-trips through the exact
    /// string that gets written to the cgroup.
    #[test]
    fn memory_limits_round_trip_through_the_cgroup(spec in any::<String>()) {
        if let Ok(limit) = MemoryLimit::parse(&spec) {
            let written = limit.cgroup_value();
            match limit {
                MemoryLimit::Max => prop_assert_eq!(written, "max"),
                MemoryLimit::Bytes(bytes) => {
                    let reparsed: u64 = written.parse().expect("cgroup value must be numeric");
                    prop_assert_eq!(reparsed, bytes);
                    prop_assert!(bytes > 0, "zero-byte limit from {spec:?}");
                }
            }
        }
    }

    /// A parsed memory limit is always re-parsable from its own cgroup form.
    #[test]
    fn cgroup_values_reparse(spec in any::<String>()) {
        if let Ok(limit) = MemoryLimit::parse(&spec) {
            let written = limit.cgroup_value();
            let again = MemoryLimit::parse(&written);
            prop_assert!(again.is_ok(), "could not reparse {written:?}");
        }
    }

    /// Generated interface names always fit the kernel's IFNAMSIZ limit.
    #[test]
    fn veth_names_always_fit_the_kernel_limit(name in any::<String>()) {
        let (host, child) = generate_veth_names(&name);
        prop_assert!(host.len() <= 15, "host name too long for {name:?}: {host:?}");
        prop_assert!(child.len() <= 15, "child name too long for {name:?}: {child:?}");
        prop_assert!(host.starts_with("vh-"), "{host:?}");
        prop_assert!(child.starts_with("vc-"), "{child:?}");
        prop_assert!(host != child, "host and child names collided for {:?}", name);
        // Only characters the kernel accepts in an interface name.
        prop_assert!(
            host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "bad char in {host:?}"
        );
    }

    /// An accepted subnet is internally consistent.
    #[test]
    fn accepted_subnets_are_consistent(cidr in any::<String>()) {
        if let Ok(net) = Ipv4Net::parse_cidr(&cidr) {
            prop_assert!(net.prefix() >= 8 && net.prefix() <= 32);
            // The network address must have no host bits set.
            prop_assert_eq!(net.network() & !mask_for(net.prefix()), 0);
            prop_assert!(net.first_host() > net.network(), "no host range in {cidr:?}");
            prop_assert!(net.last_host() < net.broadcast(), "bad broadcast in {cidr:?}");
            // The gateway must be inside the block.
            prop_assert!(net.contains(net.gateway()), "gateway outside {cidr:?}");
            // Round-tripping the CIDR form is stable.
            prop_assert_eq!(Ipv4Net::parse_cidr(&net.cidr()).unwrap().cidr(), net.cidr());
        }
    }

    /// Every subnet reports a non-empty, ordered host range.
    #[test]
    fn subnet_host_ranges_are_ordered(cidr in any::<String>()) {
        if let Ok(net) = Ipv4Net::parse_cidr(&cidr) {
            prop_assert!(net.first_host() <= net.last_host(), "inverted range in {cidr:?}");
        }
    }

    /// Address formatting and parsing are exact inverses.
    #[test]
    fn ip_formatting_round_trips(raw in any::<u32>()) {
        let text = u32_to_ip(raw);
        let parsed = parse_ipv4(&text).expect("formatted address must parse");
        prop_assert_eq!(parsed, raw);
    }

    /// Containment agrees with a plain numeric range check.
    #[test]
    fn subnet_containment_agrees_with_range_check(
        addr in any::<u32>(),
        prefix in 8u8..=32,
        candidate in any::<u32>(),
    ) {
        let net = Ipv4Net::new(addr, prefix).unwrap();
        let network = addr & mask_for(prefix);
        let broadcast = network | !mask_for(prefix);
        let expected = candidate >= network && candidate <= broadcast;
        prop_assert_eq!(net.contains(candidate), expected);
    }

    /// An accepted port mapping is always in range and uses a known protocol.
    #[test]
    fn accepted_port_mappings_are_sane(spec in any::<String>()) {
        if let Ok(mapping) = parse_port_mapping(&spec) {
            prop_assert!(mapping.host_port >= 1, "zero host port from {spec:?}");
            prop_assert!(mapping.container_port >= 1, "zero container port from {spec:?}");
            prop_assert!(
                mapping.protocol == "tcp" || mapping.protocol == "udp",
                "bad protocol from {spec:?}: {}",
                mapping.protocol
            );
            if let Some(ip) = &mapping.host_ip {
                // A bind address must be a real address if it was accepted.
                prop_assert!(parse_ipv4(ip).is_ok(), "bad bind address from {spec:?}: {ip:?}");
            }
        }
    }
}

/// Computes the mask for a prefix length, mirroring the implementation.
fn mask_for(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix as u32)
    }
}
