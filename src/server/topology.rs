use crate::protocol::NetworkInterface;
use crate::server::core::ConnectedAgent;
use std::collections::{BTreeMap, HashMap};
use std::net::Ipv4Addr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DetectedRoute {
    pub(crate) cidr: String,
    pub(crate) interface_name: String,
    pub(crate) source_address: String,
    pub(crate) score: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentRoute {
    pub(crate) agent_id: String,
    pub(crate) agent_name: String,
    pub(crate) cidr: String,
    pub(crate) interface_name: String,
    pub(crate) source_address: String,
    pub(crate) score: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SharedRouteGroup {
    pub(crate) cidr: String,
    pub(crate) agents: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteConflict {
    pub(crate) cidr: String,
    pub(crate) agents: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TopologySnapshot {
    pub(crate) routes: Vec<AgentRoute>,
    pub(crate) shared_routes: Vec<SharedRouteGroup>,
    pub(crate) conflicts: Vec<RouteConflict>,
}

pub(crate) struct TopologyManager;

impl TopologyManager {
    pub(crate) fn build_snapshot(agents: &HashMap<String, ConnectedAgent>) -> TopologySnapshot {
        let mut routes = Vec::new();

        for agent in agents.values() {
            for route in Self::detect_agent_routes(&agent.info.interfaces) {
                routes.push(AgentRoute {
                    agent_id: agent.id.clone(),
                    agent_name: agent.info.name.clone(),
                    cidr: route.cidr,
                    interface_name: route.interface_name,
                    source_address: route.source_address,
                    score: route.score,
                });
            }
        }

        routes.sort_by(|a, b| {
            a.cidr
                .cmp(&b.cidr)
                .then_with(|| b.score.cmp(&a.score))
                .then_with(|| a.agent_id.cmp(&b.agent_id))
        });

        TopologySnapshot {
            shared_routes: Self::shared_route_groups(&routes),
            conflicts: Self::route_conflicts(&routes),
            routes,
        }
    }

    pub(crate) fn detect_agent_routes(interfaces: &[NetworkInterface]) -> Vec<DetectedRoute> {
        let mut routes = Vec::new();

        for iface in interfaces {
            for address in &iface.addresses {
                if let Some(cidr) = Self::normalize_ipv4_cidr(address) {
                    if Self::is_auto_route_candidate(&cidr, iface) {
                        routes.push(DetectedRoute {
                            score: Self::score_detected_route(&cidr, iface),
                            cidr: cidr.0,
                            interface_name: iface.name.clone(),
                            source_address: address.clone(),
                        });
                    }
                }
            }
        }

        routes.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.cidr.cmp(&b.cidr))
                .then_with(|| a.interface_name.cmp(&b.interface_name))
        });
        routes.dedup_by(|a, b| a.cidr == b.cidr);
        routes
    }

    pub(crate) fn best_route_for_agent(interfaces: &[NetworkInterface]) -> Option<DetectedRoute> {
        Self::detect_agent_routes(interfaces).into_iter().next()
    }

    pub(crate) fn normalize_ipv4_cidr(input: &str) -> Option<(String, Ipv4Addr, u8)> {
        let (ip_part, prefix_part) = input.split_once('/')?;
        let ip = ip_part.parse::<Ipv4Addr>().ok()?;
        let prefix = prefix_part.parse::<u8>().ok()?;
        if prefix > 32 {
            return None;
        }

        let mask = Self::prefix_mask(prefix);
        let network = Ipv4Addr::from(u32::from(ip) & mask);
        Some((format!("{}/{}", network, prefix), ip, prefix))
    }

    pub(crate) fn route_contains_ip(cidr: &str, ip: Ipv4Addr) -> bool {
        let Some((_, network, prefix)) = Self::normalize_ipv4_cidr(cidr) else {
            return false;
        };
        let mask = Self::prefix_mask(prefix);
        (u32::from(network) & mask) == (u32::from(ip) & mask)
    }

    fn shared_route_groups(routes: &[AgentRoute]) -> Vec<SharedRouteGroup> {
        let mut by_cidr: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for route in routes {
            by_cidr
                .entry(route.cidr.clone())
                .or_default()
                .push(format!("{} ({})", route.agent_name, route.agent_id));
        }

        by_cidr
            .into_iter()
            .filter_map(|(cidr, mut agents)| {
                agents.sort();
                agents.dedup();
                (agents.len() > 1).then_some(SharedRouteGroup { cidr, agents })
            })
            .collect()
    }

    fn route_conflicts(routes: &[AgentRoute]) -> Vec<RouteConflict> {
        let mut conflicts: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for (left_index, left) in routes.iter().enumerate() {
            for right in routes.iter().skip(left_index + 1) {
                if left.agent_id == right.agent_id {
                    continue;
                }

                let Some((_, left_network, _)) = Self::normalize_ipv4_cidr(&left.cidr) else {
                    continue;
                };
                let Some((_, right_network, _)) = Self::normalize_ipv4_cidr(&right.cidr) else {
                    continue;
                };

                if left.cidr == right.cidr
                    || Self::route_contains_ip(&left.cidr, right_network)
                    || Self::route_contains_ip(&right.cidr, left_network)
                {
                    let key = if left.cidr <= right.cidr {
                        left.cidr.clone()
                    } else {
                        right.cidr.clone()
                    };
                    let agents = conflicts.entry(key).or_default();
                    agents.push(format!("{} ({})", left.agent_name, left.agent_id));
                    agents.push(format!("{} ({})", right.agent_name, right.agent_id));
                }
            }
        }

        conflicts
            .into_iter()
            .map(|(cidr, mut agents)| {
                agents.sort();
                agents.dedup();
                RouteConflict { cidr, agents }
            })
            .collect()
    }

    fn is_auto_route_candidate(cidr: &(String, Ipv4Addr, u8), iface: &NetworkInterface) -> bool {
        let (_, ip, prefix) = cidr;
        if *prefix == 0 {
            return false;
        }
        if ip.is_loopback()
            || ip.is_link_local()
            || ip.is_multicast()
            || ip.is_unspecified()
            || ip.octets() == [255, 255, 255, 255]
        {
            return false;
        }

        let iface_name = iface.name.to_ascii_lowercase();
        if iface_name == "lo" || iface_name.starts_with("lo:") {
            return false;
        }
        if iface
            .flags
            .iter()
            .any(|flag| flag.eq_ignore_ascii_case("LOOPBACK"))
        {
            return false;
        }

        true
    }

    fn score_detected_route(cidr: &(String, Ipv4Addr, u8), iface: &NetworkInterface) -> u16 {
        let (_, ip, prefix) = cidr;
        let mut score: u16 = 0;

        if Self::is_private_ipv4(*ip) {
            score += 100;
        }
        if (16..=30).contains(prefix) {
            score += 40;
        } else if *prefix == 32 {
            score += 5;
        } else {
            score += 15;
        }
        if iface
            .flags
            .iter()
            .any(|flag| flag.eq_ignore_ascii_case("UP"))
        {
            score += 20;
        }
        if iface
            .flags
            .iter()
            .any(|flag| flag.eq_ignore_ascii_case("LOWER_UP"))
        {
            score += 20;
        }

        let iface_name = iface.name.to_ascii_lowercase();
        if iface_name.starts_with('e')
            || iface_name.starts_with("en")
            || iface_name.starts_with("eth")
            || iface_name.starts_with("wl")
        {
            score += 10;
        }
        if iface_name.starts_with("docker")
            || iface_name.starts_with("br-")
            || iface_name.starts_with("veth")
            || iface_name.starts_with("virbr")
            || iface_name.starts_with("labyrinth")
        {
            score = score.saturating_sub(80);
        }

        score
    }

    fn is_private_ipv4(ip: Ipv4Addr) -> bool {
        let octets = ip.octets();
        octets[0] == 10
            || (octets[0] == 172 && (16..=31).contains(&octets[1]))
            || (octets[0] == 192 && octets[1] == 168)
    }

    fn prefix_mask(prefix: u8) -> u32 {
        if prefix == 0 {
            0
        } else {
            u32::MAX << (32 - prefix)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TopologyManager;
    use crate::protocol::NetworkInterface;

    fn iface(name: &str, addresses: Vec<&str>) -> NetworkInterface {
        NetworkInterface {
            name: name.to_string(),
            addresses: addresses.into_iter().map(str::to_string).collect(),
            hardware_addr: "00:11:22:33:44:55".to_string(),
            mtu: 1500,
            flags: vec!["UP".to_string(), "LOWER_UP".to_string()],
        }
    }

    #[test]
    fn normalize_ipv4_cidr_maps_host_to_network() {
        let (cidr, ip, prefix) = TopologyManager::normalize_ipv4_cidr("192.168.55.23/24").unwrap();
        assert_eq!(cidr, "192.168.55.0/24");
        assert_eq!(ip.to_string(), "192.168.55.23");
        assert_eq!(prefix, 24);
    }

    #[test]
    fn detect_agent_routes_skips_loopback_and_ranks_lan() {
        let interfaces = vec![
            NetworkInterface {
                name: "lo".to_string(),
                addresses: vec!["127.0.0.1/8".to_string()],
                hardware_addr: "00:00:00:00:00:00".to_string(),
                mtu: 65536,
                flags: vec!["LOOPBACK".to_string(), "UP".to_string()],
            },
            iface("docker0", vec!["172.17.0.1/16"]),
            iface("eth0", vec!["192.168.10.42/24"]),
        ];

        let routes = TopologyManager::detect_agent_routes(&interfaces);
        assert_eq!(routes[0].cidr, "192.168.10.0/24");
        assert!(routes.iter().all(|route| route.cidr != "127.0.0.0/8"));
    }

    #[test]
    fn detect_agent_routes_deduplicates_same_network() {
        let interfaces = vec![iface("eth0", vec!["10.10.1.2/24", "10.10.1.3/24"])];

        let routes = TopologyManager::detect_agent_routes(&interfaces);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].cidr, "10.10.1.0/24");
    }

    #[test]
    fn route_contains_ip_matches_prefix() {
        assert!(TopologyManager::route_contains_ip(
            "172.16.10.0/24",
            "172.16.10.99".parse().unwrap()
        ));
        assert!(!TopologyManager::route_contains_ip(
            "172.16.10.0/24",
            "172.16.11.99".parse().unwrap()
        ));
    }

    #[test]
    fn shared_route_groups_identify_multi_hop_candidates() {
        let routes = vec![
            super::AgentRoute {
                agent_id: "agent-b".to_string(),
                agent_name: "Agent B".to_string(),
                cidr: "172.16.10.0/24".to_string(),
                interface_name: "eth0".to_string(),
                source_address: "172.16.10.20/24".to_string(),
                score: 170,
            },
            super::AgentRoute {
                agent_id: "agent-c".to_string(),
                agent_name: "Agent C".to_string(),
                cidr: "172.16.10.0/24".to_string(),
                interface_name: "eth0".to_string(),
                source_address: "172.16.10.30/24".to_string(),
                score: 170,
            },
            super::AgentRoute {
                agent_id: "agent-d".to_string(),
                agent_name: "Agent D".to_string(),
                cidr: "10.8.0.0/24".to_string(),
                interface_name: "eth1".to_string(),
                source_address: "10.8.0.5/24".to_string(),
                score: 170,
            },
        ];

        let shared = TopologyManager::shared_route_groups(&routes);
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].cidr, "172.16.10.0/24");
        assert_eq!(shared[0].agents.len(), 2);
    }

    #[test]
    fn route_conflicts_identify_overlapping_cidrs() {
        let routes = vec![
            super::AgentRoute {
                agent_id: "agent-a".to_string(),
                agent_name: "Agent A".to_string(),
                cidr: "172.16.0.0/16".to_string(),
                interface_name: "eth0".to_string(),
                source_address: "172.16.1.10/16".to_string(),
                score: 160,
            },
            super::AgentRoute {
                agent_id: "agent-b".to_string(),
                agent_name: "Agent B".to_string(),
                cidr: "172.16.10.0/24".to_string(),
                interface_name: "eth0".to_string(),
                source_address: "172.16.10.20/24".to_string(),
                score: 170,
            },
        ];

        let conflicts = TopologyManager::route_conflicts(&routes);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].agents.len(), 2);
    }

    fn iface_with_flags(name: &str, address: &str, flags: &[&str]) -> NetworkInterface {
        NetworkInterface {
            flags: flags.iter().map(|f| f.to_string()).collect(),
            ..iface(name, vec![address])
        }
    }

    #[test]
    fn normalize_ipv4_cidr_rejects_malformed_input() {
        for bad in [
            "10.0.0.1",
            "10.0.0.1/33",
            "10.0.0.1/-1",
            "10.0.0/24",
            "/24",
            "10.0.0.1/",
            "fe80::1/64",
            "",
        ] {
            assert!(
                TopologyManager::normalize_ipv4_cidr(bad).is_none(),
                "{bad:?}"
            );
        }
        assert_eq!(
            TopologyManager::normalize_ipv4_cidr("10.1.2.3/0")
                .unwrap()
                .0,
            "0.0.0.0/0"
        );
        assert_eq!(
            TopologyManager::normalize_ipv4_cidr("10.1.2.3/32")
                .unwrap()
                .0,
            "10.1.2.3/32"
        );
    }

    #[test]
    fn route_contains_ip_edges() {
        use std::net::Ipv4Addr;
        let ip = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        assert!(TopologyManager::route_contains_ip(
            "0.0.0.0/0",
            ip("203.0.113.9")
        ));
        assert!(TopologyManager::route_contains_ip(
            "10.0.0.5/32",
            ip("10.0.0.5")
        ));
        assert!(!TopologyManager::route_contains_ip(
            "10.0.0.5/32",
            ip("10.0.0.6")
        ));
        assert!(TopologyManager::route_contains_ip(
            "10.0.0.0/24",
            ip("10.0.0.255")
        ));
        assert!(!TopologyManager::route_contains_ip(
            "10.0.0.0/24",
            ip("10.0.1.0")
        ));
        // Host bits in the CIDR are ignored.
        assert!(TopologyManager::route_contains_ip(
            "10.0.0.77/24",
            ip("10.0.0.1")
        ));
        assert!(!TopologyManager::route_contains_ip(
            "not-a-cidr",
            ip("10.0.0.1")
        ));
    }

    #[test]
    fn detect_routes_skips_non_routable_and_loopback_interfaces() {
        let interfaces = vec![
            iface(
                "eth0",
                vec!["169.254.10.10/16", "224.0.0.5/4", "0.0.0.0/8", "fe80::1/64"],
            ),
            iface("eth1", vec!["10.9.9.9/0"]),
            iface("lo:1", vec!["10.10.10.10/24"]),
            iface_with_flags("tap0", "10.20.20.20/24", &["UP", "LOOPBACK"]),
        ];
        assert!(TopologyManager::detect_agent_routes(&interfaces).is_empty());
        assert!(TopologyManager::best_route_for_agent(&interfaces).is_none());
        assert!(TopologyManager::best_route_for_agent(&[]).is_none());
    }

    #[test]
    fn scoring_prefers_physical_private_lan_over_virtual_and_public() {
        let interfaces = vec![
            iface("docker0", vec!["172.17.0.1/16"]),
            iface("veth12ab", vec!["172.18.0.1/24"]),
            iface("eth0", vec!["203.0.113.10/24"]),
            iface_with_flags("ens3", "192.168.56.10/24", &["UP", "LOWER_UP"]),
            iface_with_flags("wlan0", "10.50.0.3/32", &["UP"]),
        ];
        let routes = TopologyManager::detect_agent_routes(&interfaces);
        assert_eq!(routes[0].cidr, "192.168.56.0/24");
        assert_eq!(routes[0].interface_name, "ens3");
        let position = |cidr: &str| routes.iter().position(|r| r.cidr == cidr).unwrap();
        // Private physical LAN beats public, and beats container bridges.
        assert!(position("192.168.56.0/24") < position("203.0.113.0/24"));
        assert!(position("192.168.56.0/24") < position("172.17.0.0/16"));
        assert!(position("192.168.56.0/24") < position("172.18.0.0/24"));
        let score = |cidr: &str| routes[position(cidr)].score;
        // Same address on a container bridge: -80 penalty and no +10 NIC-name bonus.
        let physical_equivalent =
            TopologyManager::detect_agent_routes(&[iface("eth9", vec!["172.17.0.1/16"])])[0].score;
        assert_eq!(physical_equivalent - score("172.17.0.0/16"), 90);
        assert!(routes.windows(2).all(|w| w[0].score >= w[1].score));
    }

    #[test]
    fn build_snapshot_groups_shared_lans_and_flags_overlaps() {
        use crate::protocol::{AgentInfo, AgentKind};
        use crate::server::core::ConnectedAgent;
        use std::collections::HashMap;
        use std::sync::Arc;
        use tokio::sync::{mpsc, Mutex};

        let make = |id: &str, cidrs: Vec<&str>| {
            let (sender, _rx) = mpsc::channel(1);
            ConnectedAgent {
                id: id.into(),
                info: AgentInfo {
                    name: id.to_uppercase(),
                    hostname: id.into(),
                    os: "linux".into(),
                    arch: "x86_64".into(),
                    interfaces: vec![iface("eth0", cidrs)],
                    auth_key: None,
                    kind: AgentKind::Generic,
                    stable_id: None,
                    listener_addr: None,
                    listener_port: None,
                    connectivity: Default::default(),
                },
                sender,
                transport_label: "tcp/tls".into(),
                quic_connection: None,
                tunnel_active: false,
                tunnel_subnet: None,
                tun_name: None,
                last_seen: Arc::new(Mutex::new(std::time::Instant::now())),
                command_response: Arc::new(Mutex::new(None)),
                shell_events: Arc::new(Mutex::new(None)),
            }
        };
        let mut agents = HashMap::new();
        agents.insert("a".to_string(), make("a", vec!["10.0.5.4/24"]));
        agents.insert("b".to_string(), make("b", vec!["10.0.5.9/24"]));
        agents.insert("c".to_string(), make("c", vec!["10.0.0.1/16"]));
        agents.insert("d".to_string(), make("d", vec!["192.168.7.7/24"]));

        let snapshot = TopologyManager::build_snapshot(&agents);
        assert_eq!(snapshot.routes.len(), 4);
        assert_eq!(snapshot.shared_routes.len(), 1);
        assert_eq!(snapshot.shared_routes[0].cidr, "10.0.5.0/24");
        assert_eq!(snapshot.shared_routes[0].agents.len(), 2);

        let all_conflicting: std::collections::BTreeSet<_> = snapshot
            .conflicts
            .iter()
            .flat_map(|c| c.agents.iter().cloned())
            .collect();
        assert!(all_conflicting.contains("C (c)"));
        assert!(all_conflicting.contains("A (a)"));
        assert!(!all_conflicting.iter().any(|a| a.contains("(d)")));

        // Deterministic regardless of HashMap iteration order.
        assert_eq!(snapshot, TopologyManager::build_snapshot(&agents));
    }
}
