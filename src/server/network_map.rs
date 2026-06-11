use crate::protocol::{AgentKind, InternetAccess};
use crate::server::core::{AriadneSnapshot, ConnectedAgent, PortalSnapshot};
use crate::server::dweller_registry::DwellerRegistry;
use crate::server::topology::{AgentRoute, SharedRouteGroup, TopologySnapshot};
use colored::Colorize;
use std::collections::{BTreeMap, HashMap, HashSet};

pub(crate) struct NetworkMapRenderer;

impl NetworkMapRenderer {
    pub(crate) fn render(
        agents: &HashMap<String, ConnectedAgent>,
        dwellers: &DwellerRegistry,
        topology: &TopologySnapshot,
        port_forwards: &[PortalSnapshot],
        ariadne: &[AriadneSnapshot],
    ) -> String {
        let mut lines = Vec::new();
        lines.push(format!(
            "{} {}",
            "Network Map".cyan().bold(),
            "visualization only".bright_black()
        ));
        lines.push("────────────────────────".bright_black().to_string());
        lines.push(format!(
            "{}",
            "Legend: [S] server  [A] agent  [D] dweller  [N] network  [PF] port forward"
                .bright_black()
        ));
        lines.push(format!(
            "{}",
            "Edges: tcp/tls or quic/udp = encrypted transport, local/unenc = local listener or host-side plaintext"
                .bright_black()
        ));
        lines.push(String::new());

        lines.push(format!(
            "{} {}",
            "[S]".cyan().bold(),
            "Labyrinth Server / Proxy".bright_white().bold()
        ));

        if agents.is_empty() && dwellers.dwellers.is_empty() {
            lines.push(format!(
                "  {} {}",
                "└─".bright_black(),
                "No connected agents or remembered dwellers".yellow()
            ));
            return lines.join("\n");
        }

        let by_agent_routes = Self::routes_by_agent(&topology.routes);
        let by_agent_forwards = Self::port_forwards_by_agent(port_forwards);
        let by_agent_ariadne = Self::ariadne_by_agent(ariadne);

        let mut agent_list: Vec<_> = agents.values().collect();
        agent_list.sort_by(|left, right| {
            left.info
                .name
                .cmp(&right.info.name)
                .then_with(|| left.id.cmp(&right.id))
        });

        for agent in agent_list {
            let node = Self::agent_node(agent);
            lines.push(format!(
                "  {} {} {}",
                format!("├─ {} online →", agent.transport_label).green(),
                node,
                Self::short_id(&agent.id).bright_black()
            ));
            lines.push(format!(
                "  │  {} {}",
                "status".bright_black(),
                Self::agent_status(agent)
            ));

            if let Some(route) = by_agent_ariadne.get(&agent.id) {
                lines.push(format!(
                    "  │  {} ariadne tunnel local/unenc → tun/{} proxy:{}",
                    "├─".bright_black(),
                    Self::agent_transport_label(agent).cyan(),
                    route.proxy_port.to_string().cyan()
                ));
            } else if agent.tunnel_active && !Self::is_portal_transport(agent) {
                lines.push(format!(
                    "  │  {} ariadne tunnel local/unenc → tun/{}",
                    "├─".bright_black(),
                    Self::agent_transport_label(agent).cyan()
                ));
            } else if agent.tunnel_active {
                lines.push(format!(
                    "  │  {} active portal transport {}",
                    "├─".bright_black(),
                    Self::agent_transport_label(agent).cyan()
                ));
            }

            if let Some(forwards) = by_agent_forwards.get(&agent.id) {
                for forward in forwards {
                    lines.push(format!(
                        "  │  {} [PF] local/unenc :{} → {}:{} → stream/tls/enc",
                        "├─".bright_black(),
                        forward.local_port.to_string().yellow(),
                        forward.target_host.cyan(),
                        forward.target_port.to_string().cyan()
                    ));
                }
            }

            match by_agent_routes.get(&agent.id) {
                Some(routes) if !routes.is_empty() => {
                    let title = if routes.len() > 1 {
                        "multi-network"
                    } else {
                        "network"
                    };
                    lines.push(format!("  │  {} {}", "├─".bright_black(), title.cyan()));
                    for route in routes {
                        lines.push(format!(
                            "  │  │  {} [N] {} via {} ({})",
                            "├─".bright_black(),
                            route.cidr.green(),
                            route.interface_name.bright_white(),
                            route.source_address.bright_black()
                        ));
                    }
                }
                _ => {
                    lines.push(format!(
                        "  │  {} {}",
                        "├─".bright_black(),
                        "no detected routable CIDR".yellow()
                    ));
                }
            }
        }

        let offline_dwellers = Self::offline_dwellers(agents, dwellers);
        for record in offline_dwellers {
            lines.push(format!(
                "  {} [D] {} {} remembered/offline {}",
                "├─".bright_black(),
                record.dweller_name.cyan(),
                record.socket_addr().bright_white(),
                format!("{}/{}", record.os, record.arch).bright_black()
            ));
        }

        Self::append_shared_networks(&mut lines, &topology.shared_routes);
        Self::append_conflicts(&mut lines, topology);

        lines.join("\n")
    }

    fn routes_by_agent(routes: &[AgentRoute]) -> BTreeMap<String, Vec<&AgentRoute>> {
        let mut by_agent: BTreeMap<String, Vec<&AgentRoute>> = BTreeMap::new();
        for route in routes {
            by_agent
                .entry(route.agent_id.clone())
                .or_default()
                .push(route);
        }
        by_agent
    }

    fn port_forwards_by_agent(
        port_forwards: &[PortalSnapshot],
    ) -> BTreeMap<String, Vec<&PortalSnapshot>> {
        let mut by_agent: BTreeMap<String, Vec<&PortalSnapshot>> = BTreeMap::new();
        for forward in port_forwards {
            by_agent
                .entry(forward.agent_id.clone())
                .or_default()
                .push(forward);
        }
        by_agent
    }

    fn ariadne_by_agent(ariadne: &[AriadneSnapshot]) -> BTreeMap<String, &AriadneSnapshot> {
        ariadne
            .iter()
            .map(|snapshot| (snapshot.agent_id.clone(), snapshot))
            .collect()
    }

    fn offline_dwellers<'a>(
        agents: &HashMap<String, ConnectedAgent>,
        dwellers: &'a DwellerRegistry,
    ) -> Vec<&'a crate::server::dweller_registry::DwellerRecord> {
        let online: HashSet<&str> = agents.keys().map(String::as_str).collect();
        dwellers
            .list()
            .into_iter()
            .filter(|record| !online.contains(record.dweller_id.as_str()))
            .collect()
    }

    fn append_shared_networks(lines: &mut Vec<String>, shared_routes: &[SharedRouteGroup]) {
        if shared_routes.is_empty() {
            return;
        }

        lines.push(String::new());
        lines.push(format!("{}", "Shared / Multi-Hop Candidates".cyan().bold()));
        for group in shared_routes {
            lines.push(format!(
                "  {} [N] {} shared by {}",
                "├─".bright_black(),
                group.cidr.green(),
                group.agents.len().to_string().yellow()
            ));
            for agent in &group.agents {
                lines.push(format!(
                    "  │  {} {}",
                    "├─".bright_black(),
                    agent.bright_white()
                ));
            }
        }
    }

    fn append_conflicts(lines: &mut Vec<String>, topology: &TopologySnapshot) {
        if topology.conflicts.is_empty() {
            return;
        }

        lines.push(String::new());
        lines.push(format!("{}", "Route Conflicts".yellow().bold()));
        for conflict in &topology.conflicts {
            lines.push(format!(
                "  {} [N] {} overlaps across {} agents",
                "├─".bright_black(),
                conflict.cidr.yellow(),
                conflict.agents.len().to_string().yellow()
            ));
        }
    }

    fn agent_node(agent: &ConnectedAgent) -> String {
        let label = match agent.info.kind {
            AgentKind::Dweller => "[D]".magenta().bold(),
            AgentKind::Generic => "[A]".green().bold(),
        };
        format!(
            "{} {} {}",
            label,
            agent.info.name.bright_white().bold(),
            format!("{}/{}", agent.info.os, agent.info.arch).bright_black()
        )
    }

    fn agent_status(agent: &ConnectedAgent) -> String {
        let kind = match agent.info.kind {
            AgentKind::Dweller => "dweller",
            AgentKind::Generic => "agent",
        };
        let tunnel = if agent.tunnel_active {
            agent
                .tunnel_subnet
                .as_deref()
                .unwrap_or("transport active")
                .green()
                .to_string()
        } else {
            "transport idle".bright_black().to_string()
        };
        let internet = match agent.info.connectivity.internet_access {
            InternetAccess::Confirmed => "internet confirmed".green().to_string(),
            InternetAccess::ServerReachable => "server reachable".cyan().to_string(),
            InternetAccess::RouteOnly => "route only".yellow().to_string(),
            InternetAccess::Unreachable => "no outbound".red().to_string(),
            InternetAccess::Unknown => "internet unknown".bright_black().to_string(),
        };
        format!("{} {} {}", kind.bright_white(), tunnel, internet)
    }

    fn agent_transport_label(agent: &ConnectedAgent) -> &str {
        &agent.transport_label
    }

    fn is_portal_transport(agent: &ConnectedAgent) -> bool {
        agent
            .tunnel_subnet
            .as_deref()
            .is_some_and(|label| label.starts_with("Port forwarding:"))
    }

    fn short_id(id: &str) -> String {
        let mut chars = id.chars();
        let short: String = chars.by_ref().take(8).collect();
        if chars.next().is_some() {
            format!("({}...)", short)
        } else {
            format!("({})", id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{AgentInfo, NetworkInterface};
    use crate::server::core::ConnectedAgent;
    use crate::server::dweller_registry::DwellerRecord;
    use std::sync::Arc;
    use std::time::Instant;
    use tokio::sync::{mpsc, Mutex};

    fn test_agent(id: &str, name: &str, kind: AgentKind, addresses: Vec<&str>) -> ConnectedAgent {
        let (sender, _rx) = mpsc::channel(1);
        ConnectedAgent {
            id: id.to_string(),
            info: AgentInfo {
                name: name.to_string(),
                hostname: name.to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
                interfaces: vec![NetworkInterface {
                    name: "eth0".to_string(),
                    addresses: addresses.into_iter().map(str::to_string).collect(),
                    hardware_addr: "00:11:22:33:44:55".to_string(),
                    mtu: 1500,
                    flags: vec!["UP".to_string(), "LOWER_UP".to_string()],
                }],
                auth_key: None,
                kind,
                stable_id: None,
                listener_addr: None,
                listener_port: None,
                connectivity: Default::default(),
            },
            sender,
            transport_label: "tcp/tls".to_string(),
            quic_connection: None,
            tunnel_active: false,
            tunnel_subnet: None,
            tun_name: None,
            last_seen: Arc::new(Mutex::new(Instant::now())),
            command_response: Arc::new(Mutex::new(None)),
            shell_events: Arc::new(Mutex::new(None)),
        }
    }

    #[test]
    fn render_includes_agent_routes_and_port_forward_edges() {
        let mut agents = HashMap::new();
        agents.insert(
            "agent-a".to_string(),
            test_agent("agent-a", "alpha", AgentKind::Generic, vec!["10.10.1.4/24"]),
        );
        let topology = crate::server::topology::TopologyManager::build_snapshot(&agents);
        let output = NetworkMapRenderer::render(
            &agents,
            &DwellerRegistry::default(),
            &topology,
            &[PortalSnapshot {
                local_port: 8080,
                agent_id: "agent-a".to_string(),
                target_host: "10.10.1.20".to_string(),
                target_port: 80,
            }],
            &[],
        );

        assert!(output.contains("Network Map"));
        assert!(output.contains("[PF]"));
        assert!(output.contains("10.10.1.0/24"));
        assert!(output.contains("tcp/tls"));
        assert!(output.contains("local/unenc"));
    }

    #[test]
    fn render_shows_offline_dweller_records() {
        let mut dwellers = DwellerRegistry::default();
        dwellers.upsert(DwellerRecord {
            dweller_id: "dweller-a".to_string(),
            dweller_name: "delta".to_string(),
            hostname: "host".to_string(),
            os: "windows".to_string(),
            arch: "x86_64".to_string(),
            listen_addr: "10.20.30.40".to_string(),
            listen_port: 45454,
            fingerprint: "abcd".to_string(),
            auth_key: "secret".to_string(),
            install_path: r"C:\ProgramData\Labyrinth\delta.exe".to_string(),
            config_dir: r"C:\ProgramData\Labyrinth\delta".to_string(),
            service_name: "LabyrinthDweller_delta".to_string(),
            last_connected: None,
            callback_servers: Vec::new(),
            path: Vec::new(),
            hibernation: crate::protocol::DwellerHibernationConfig::default(),
            tasks: Vec::new(),
        });

        let output = NetworkMapRenderer::render(
            &HashMap::new(),
            &dwellers,
            &TopologySnapshot::default(),
            &[],
            &[],
        );

        assert!(output.contains("[D]"));
        assert!(output.contains("remembered/offline"));
        assert!(output.contains("10.20.30.40:45454"));
    }
}
