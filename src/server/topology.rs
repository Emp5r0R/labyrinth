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
}
