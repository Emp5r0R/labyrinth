use crate::error::{LabyrinthError, Result};
use crate::server::core::LabyrinthServer;
use crate::server::dweller_manager::DwellerManager;
use crate::server::dweller_registry::DwellerRecord;
use crate::server::topology::{AgentRoute, TopologyManager};
use crate::server::tunnel_manager::TunnelManager;
use crate::styling;
use colored::Colorize;
use dialoguer::Confirm;
use serde::Serialize;
use std::net::Ipv4Addr;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum ChainAction {
    ReuseTunnel {
        agent_id: String,
        agent_name: String,
        cidr: String,
    },
    StartTunnel {
        agent_id: String,
        agent_name: String,
        cidr: String,
        tun_name: String,
    },
    ConnectDweller {
        dweller_id: String,
        dweller_name: String,
        address: String,
    },
    RetryAfterDweller {
        dweller_id: String,
        dweller_name: String,
    },
    Blocked {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ChainPlan {
    pub(crate) target: String,
    pub(crate) target_ip: String,
    pub(crate) actions: Vec<ChainAction>,
    pub(crate) ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    display: String,
    ip: Ipv4Addr,
}

pub(crate) struct ChainManager;

impl ChainManager {
    pub(crate) async fn show_plan(server: &LabyrinthServer, target: &str) -> Result<()> {
        let plan = Self::build_plan(server, target).await?;
        Self::print_plan(&plan);
        Ok(())
    }

    pub(crate) async fn access(
        server: std::sync::Arc<LabyrinthServer>,
        target: &str,
    ) -> Result<()> {
        let mut plan = Self::build_plan(&server, target).await?;
        Self::print_plan(&plan);

        if plan.ready {
            println!(
                "{}",
                styling::format_success_msg(
                    styling::SUCCESS_INDICATOR,
                    "Target is already covered by an active chain"
                )
            );
            return Ok(());
        }

        if plan
            .actions
            .iter()
            .any(|action| matches!(action, ChainAction::Blocked { .. }))
        {
            return Err(LabyrinthError::Message(
                "No applicable chain plan is available for this target".to_string(),
            ));
        }

        let confirmed = Confirm::new()
            .with_prompt("Apply this chain plan?")
            .default(true)
            .interact()
            .map_err(|e| LabyrinthError::Message(format!("Input error: {}", e)))?;
        if !confirmed {
            println!("{}", styling::format_hint("Plan was not applied."));
            return Ok(());
        }

        for _ in 0..4 {
            let mut needs_retry = false;
            for action in plan.actions.clone() {
                match action {
                    ChainAction::ReuseTunnel {
                        cidr, agent_name, ..
                    } => {
                        println!(
                            "{}",
                            styling::format_check_item(&format!(
                                "Reusing active tunnel {} via {}",
                                cidr, agent_name
                            ))
                        );
                    }
                    ChainAction::StartTunnel {
                        agent_id,
                        agent_name,
                        cidr,
                        tun_name,
                    } => {
                        println!(
                            "{}",
                            styling::format_check_item(&format!(
                                "Starting tunnel {} via {}",
                                cidr, agent_name
                            ))
                        );
                        TunnelManager::start_tunnel_for_agent(&server, &agent_id, &cidr, &tun_name)
                            .await?;
                    }
                    ChainAction::ConnectDweller {
                        dweller_id,
                        dweller_name,
                        ..
                    } => {
                        println!(
                            "{}",
                            styling::format_check_item(&format!(
                                "Connecting dweller {}",
                                dweller_name
                            ))
                        );
                        DwellerManager::connect_dweller_by_id(server.clone(), &dweller_id).await?;
                        needs_retry = true;
                    }
                    ChainAction::RetryAfterDweller { .. } => {
                        needs_retry = true;
                    }
                    ChainAction::Blocked { reason } => {
                        return Err(LabyrinthError::Message(reason));
                    }
                }
            }

            if !needs_retry {
                break;
            }

            plan = Self::build_plan(&server, target).await?;
            if plan.ready {
                Self::print_plan(&plan);
                println!(
                    "{}",
                    styling::format_success_msg(
                        styling::SUCCESS_INDICATOR,
                        "Target is covered after dweller connection"
                    )
                );
                return Ok(());
            }
            if plan
                .actions
                .iter()
                .any(|action| matches!(action, ChainAction::Blocked { .. }))
            {
                Self::print_plan(&plan);
                return Err(LabyrinthError::Message(
                    "Dweller connected, but no route to the requested target is advertised yet"
                        .to_string(),
                ));
            }
        }

        println!(
            "{}",
            styling::format_success_msg(styling::SUCCESS_INDICATOR, "Chain plan applied")
        );
        Ok(())
    }

    pub(crate) async fn show_status(server: &LabyrinthServer) {
        let agents = server.agents().read().await;
        let dwellers = server.dweller_registry().read().await;
        let topology = TopologyManager::build_snapshot(&agents);

        println!(
            "\n{}",
            styling::format_section_title("Chain Status", "smart access control plane")
        );
        println!("{}", styling::format_separator(styling::SECTION_SEPARATOR));

        let active: Vec<_> = agents
            .values()
            .filter(|agent| agent.tunnel_active)
            .collect();
        if active.is_empty() {
            println!(
                "{}",
                styling::format_hint("No active smart access tunnels.")
            );
        } else {
            for agent in active {
                println!(
                    "{} {} via {} ({})",
                    styling::INDENT_LEVEL_1,
                    agent.tunnel_subnet.as_deref().unwrap_or("unknown").cyan(),
                    agent.info.name.bright_white(),
                    agent.transport_label.bright_black()
                );
            }
        }

        println!();
        println!("{}", "Remembered dwellers".cyan().bold());
        let online: std::collections::HashSet<&str> = agents.keys().map(String::as_str).collect();
        if dwellers.dwellers.is_empty() {
            println!("{}", styling::format_hint("No remembered dwellers."));
        } else {
            for record in dwellers.list() {
                let state = if online.contains(record.dweller_id.as_str()) {
                    "online".green()
                } else if Self::dweller_parent_route(record, &topology.routes).is_some() {
                    "reachable after parent tunnel".yellow()
                } else {
                    "offline/unrouted".red()
                };
                println!(
                    "{} {} {} {}",
                    styling::INDENT_LEVEL_1,
                    record.dweller_name.bright_white(),
                    record.socket_addr().bright_black(),
                    state
                );
            }
        }

        println!();
    }

    pub(crate) async fn doctor(server: &LabyrinthServer, target: Option<&str>) -> Result<()> {
        if let Some(target) = target {
            let plan = Self::build_plan(server, target).await?;
            Self::print_plan(&plan);
            return Ok(());
        }

        let agents = server.agents().read().await;
        let dwellers = server.dweller_registry().read().await;
        let topology = TopologyManager::build_snapshot(&agents);

        println!(
            "\n{}",
            styling::format_section_title("Chain Doctor", "reachability diagnostics")
        );
        println!("{}", styling::format_separator(styling::SECTION_SEPARATOR));
        if topology.routes.is_empty() {
            println!(
                "{}",
                styling::format_warning_msg(
                    styling::WARNING_INDICATOR,
                    "No routable CIDRs are advertised by connected agents"
                )
            );
        }
        if dwellers.dwellers.is_empty() {
            println!(
                "{}",
                styling::format_hint("No dwellers are remembered yet.")
            );
        }
        for record in dwellers.list() {
            if Self::dweller_parent_route(record, &topology.routes).is_none()
                && !agents.contains_key(&record.dweller_id)
            {
                println!(
                    "{} {} cannot be reached from any currently advertised route ({})",
                    styling::INDENT_LEVEL_1,
                    record.dweller_name.red(),
                    record.socket_addr()
                );
            }
        }
        println!();
        Ok(())
    }

    pub(crate) async fn build_plan(server: &LabyrinthServer, target: &str) -> Result<ChainPlan> {
        let target = Self::parse_target(target)?;
        let agents = server.agents().read().await;
        let dwellers = server.dweller_registry().read().await;
        let topology = TopologyManager::build_snapshot(&agents);

        if let Some(route) = Self::best_route_for_target(&topology.routes, target.ip) {
            let Some(agent) = agents.get(&route.agent_id) else {
                return Ok(Self::blocked(&target, "Route owner is no longer connected"));
            };

            if let Some(active_cidr) = agent.tunnel_subnet.as_deref() {
                if agent.tunnel_active && TopologyManager::route_contains_ip(active_cidr, target.ip)
                {
                    return Ok(ChainPlan {
                        target: target.display,
                        target_ip: target.ip.to_string(),
                        ready: true,
                        actions: vec![ChainAction::ReuseTunnel {
                            agent_id: agent.id.clone(),
                            agent_name: agent.info.name.clone(),
                            cidr: active_cidr.to_string(),
                        }],
                    });
                }
            }

            if agent.tunnel_active {
                return Ok(Self::blocked(
                    &target,
                    &format!(
                        "{} already has an active tunnel for {}",
                        agent.info.name,
                        agent.tunnel_subnet.as_deref().unwrap_or("another target")
                    ),
                ));
            }

            return Ok(ChainPlan {
                target: target.display,
                target_ip: target.ip.to_string(),
                ready: false,
                actions: vec![ChainAction::StartTunnel {
                    agent_id: route.agent_id.clone(),
                    agent_name: route.agent_name.clone(),
                    cidr: route.cidr.clone(),
                    tun_name: Self::tun_name_for_agent(&route.agent_id),
                }],
            });
        }

        if let Some((record, parent)) = dwellers
            .list()
            .into_iter()
            .filter(|record| !agents.contains_key(&record.dweller_id))
            .filter_map(|record| {
                Self::dweller_parent_route(record, &topology.routes).map(|route| (record, route))
            })
            .next()
        {
            let mut actions = Vec::new();
            let parent_agent = agents.get(&parent.agent_id);
            let parent_tunnel_ready = parent_agent
                .and_then(|agent| agent.tunnel_subnet.as_deref().map(|cidr| (agent, cidr)))
                .map(|(agent, cidr)| {
                    agent.tunnel_active
                        && Self::parse_ip(&record.listen_addr)
                            .map(|ip| TopologyManager::route_contains_ip(cidr, ip))
                            .unwrap_or(false)
                })
                .unwrap_or(false);

            if parent_tunnel_ready {
                actions.push(ChainAction::ReuseTunnel {
                    agent_id: parent.agent_id.clone(),
                    agent_name: parent.agent_name.clone(),
                    cidr: parent.cidr.clone(),
                });
            } else {
                actions.push(ChainAction::StartTunnel {
                    agent_id: parent.agent_id.clone(),
                    agent_name: parent.agent_name.clone(),
                    cidr: parent.cidr.clone(),
                    tun_name: Self::tun_name_for_agent(&parent.agent_id),
                });
            }
            actions.push(ChainAction::ConnectDweller {
                dweller_id: record.dweller_id.clone(),
                dweller_name: record.dweller_name.clone(),
                address: record.socket_addr(),
            });
            actions.push(ChainAction::RetryAfterDweller {
                dweller_id: record.dweller_id.clone(),
                dweller_name: record.dweller_name.clone(),
            });

            return Ok(ChainPlan {
                target: target.display,
                target_ip: target.ip.to_string(),
                ready: false,
                actions,
            });
        }

        Ok(Self::blocked(
            &target,
            "No connected agent advertises this target and no remembered dweller is reachable through current routes",
        ))
    }

    pub(crate) async fn suggestions(server: &LabyrinthServer) -> Vec<ChainPlan> {
        let agents = server.agents().read().await;
        let topology = TopologyManager::build_snapshot(&agents);
        drop(agents);

        let mut plans = Vec::new();
        for route in topology.routes.iter().take(12) {
            if let Ok(plan) = Self::build_plan(server, &route.cidr).await {
                plans.push(plan);
            }
        }
        plans
    }

    fn print_plan(plan: &ChainPlan) {
        println!(
            "\n{}",
            styling::format_section_title("Smart Access Plan", &plan.target)
        );
        println!("{}", styling::format_separator(styling::SECTION_SEPARATOR));
        if plan.actions.is_empty() {
            println!("{}", styling::format_hint("No actions required."));
        }
        for (index, action) in plan.actions.iter().enumerate() {
            let step = format!("{}. ", index + 1).bright_black();
            match action {
                ChainAction::ReuseTunnel {
                    agent_name, cidr, ..
                } => println!(
                    "{}Reuse active tunnel {} via {}",
                    step,
                    cidr.cyan(),
                    agent_name
                ),
                ChainAction::StartTunnel {
                    agent_name,
                    cidr,
                    tun_name,
                    ..
                } => println!(
                    "{}Start Ariadne {} via {} on {}",
                    step,
                    cidr.cyan(),
                    agent_name,
                    tun_name.bright_black()
                ),
                ChainAction::ConnectDweller {
                    dweller_name,
                    address,
                    ..
                } => println!(
                    "{}Connect remembered dweller {} at {}",
                    step,
                    dweller_name.cyan(),
                    address.bright_black()
                ),
                ChainAction::RetryAfterDweller { dweller_name, .. } => println!(
                    "{}Refresh topology after {} connects",
                    step,
                    dweller_name.cyan()
                ),
                ChainAction::Blocked { reason } => println!("{}Blocked: {}", step, reason.red()),
            }
        }
        println!();
    }

    /// Longest-prefix match, like a routing table: an agent sitting directly
    /// on the target's /24 beats one advertising an enclosing /16. Score and
    /// IDs only break ties so the choice is deterministic.
    fn best_route_for_target(routes: &[AgentRoute], target_ip: Ipv4Addr) -> Option<AgentRoute> {
        let prefix_len = |route: &AgentRoute| {
            TopologyManager::normalize_ipv4_cidr(&route.cidr)
                .map(|(_, _, prefix)| prefix)
                .unwrap_or(0)
        };
        let mut candidates: Vec<_> = routes
            .iter()
            .filter(|route| TopologyManager::route_contains_ip(&route.cidr, target_ip))
            .cloned()
            .collect();
        candidates.sort_by(|left, right| {
            prefix_len(right)
                .cmp(&prefix_len(left))
                .then_with(|| right.score.cmp(&left.score))
                .then_with(|| left.cidr.cmp(&right.cidr))
                .then_with(|| left.agent_id.cmp(&right.agent_id))
        });
        candidates.into_iter().next()
    }

    fn dweller_parent_route(record: &DwellerRecord, routes: &[AgentRoute]) -> Option<AgentRoute> {
        let ip = Self::parse_ip(&record.listen_addr)?;
        Self::best_route_for_target(routes, ip)
    }

    fn parse_target(input: &str) -> Result<Target> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(LabyrinthError::Message(
                "Target IP or CIDR is required".to_string(),
            ));
        }
        if let Some((cidr, network, _)) = TopologyManager::normalize_ipv4_cidr(trimmed) {
            return Ok(Target {
                display: cidr,
                ip: network,
            });
        }
        let ip = trimmed.parse::<Ipv4Addr>().map_err(|_| {
            LabyrinthError::Message("Target must be an IPv4 address or CIDR".to_string())
        })?;
        Ok(Target {
            display: format!("{}/32", ip),
            ip,
        })
    }

    fn parse_ip(input: &str) -> Option<Ipv4Addr> {
        input.parse::<Ipv4Addr>().ok()
    }

    fn tun_name_for_agent(agent_id: &str) -> String {
        let suffix: String = agent_id
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .take(8)
            .collect();
        if suffix.is_empty() {
            "lab-chain".to_string()
        } else {
            format!("lab-{}", suffix.to_ascii_lowercase())
        }
    }

    fn blocked(target: &Target, reason: &str) -> ChainPlan {
        ChainPlan {
            target: target.display.clone(),
            target_ip: target.ip.to_string(),
            ready: false,
            actions: vec![ChainAction::Blocked {
                reason: reason.to_string(),
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{AgentInfo, AgentKind, NetworkInterface};
    use crate::server::core::ConnectedAgent;
    use std::sync::Arc;
    use std::time::Instant;
    use tokio::sync::{mpsc, Mutex};

    trait AddInterface {
        fn interfaces_mut_add(&mut self, cidr: &str);
    }

    impl AddInterface for ConnectedAgent {
        fn interfaces_mut_add(&mut self, cidr: &str) {
            let mut extra = self.info.interfaces[0].clone();
            extra.name = "eth1".into();
            extra.addresses = vec![cidr.into()];
            self.info.interfaces.push(extra);
        }
    }

    fn agent(id: &str, name: &str, cidr: &str) -> ConnectedAgent {
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
                    addresses: vec![cidr.to_string()],
                    hardware_addr: "00:00:00:00:00:00".to_string(),
                    mtu: 1500,
                    flags: vec!["UP".to_string()],
                }],
                auth_key: None,
                kind: AgentKind::Generic,
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

    #[tokio::test]
    async fn plan_starts_tunnel_for_matching_agent_route() {
        let server = LabyrinthServer::new(false, None);
        server.agents().write().await.insert(
            "agent-a".to_string(),
            agent("agent-a", "A", "172.16.10.4/24"),
        );

        let plan = ChainManager::build_plan(&server, "172.16.10.55")
            .await
            .unwrap();
        assert!(!plan.ready);
        assert!(matches!(
            &plan.actions[0],
            ChainAction::StartTunnel { cidr, .. } if cidr == "172.16.10.0/24"
        ));
    }

    #[tokio::test]
    async fn plan_reuses_active_tunnel_for_target() {
        let server = LabyrinthServer::new(false, None);
        let mut a = agent("agent-a", "A", "172.16.10.4/24");
        a.tunnel_active = true;
        a.tunnel_subnet = Some("172.16.10.0/24".to_string());
        server
            .agents()
            .write()
            .await
            .insert("agent-a".to_string(), a);

        let plan = ChainManager::build_plan(&server, "172.16.10.55")
            .await
            .unwrap();
        assert!(plan.ready);
        assert!(matches!(&plan.actions[0], ChainAction::ReuseTunnel { .. }));
    }

    fn record(id: &str, listen_addr: &str) -> DwellerRecord {
        DwellerRecord::from_receipt(
            crate::protocol::DwellerInstallReceipt {
                dweller_id: id.into(),
                dweller_name: format!("{id}-name"),
                hostname: "h".into(),
                os: "linux".into(),
                arch: "x86_64".into(),
                listen_addr: listen_addr.into(),
                listen_port: 45454,
                fingerprint: "00".repeat(32),
                install_path: "/x".into(),
                config_dir: "/y".into(),
                service_name: "svc".into(),
                callback_servers: vec![],
                parent_path: vec![],
                hibernation: Default::default(),
            },
            "secret".into(),
        )
    }

    fn server_with_registry() -> LabyrinthServer {
        LabyrinthServer::new(false, None)
            .with_dweller_registry(crate::server::dweller_registry::DwellerRegistry::in_memory())
    }

    #[test]
    fn parse_target_accepts_ip_and_cidr_and_rejects_others() {
        let target = ChainManager::parse_target(" 10.0.0.5 ").unwrap();
        assert_eq!(target.display, "10.0.0.5/32");
        assert_eq!(target.ip, Ipv4Addr::new(10, 0, 0, 5));
        let target = ChainManager::parse_target("10.0.0.5/24").unwrap();
        assert_eq!(target.display, "10.0.0.0/24");
        for bad in [
            "",
            "  ",
            "host.internal",
            "10.0.0.256",
            "10.0.0.0/33",
            "::1",
        ] {
            assert!(ChainManager::parse_target(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn tun_names_are_short_safe_and_deterministic() {
        assert_eq!(ChainManager::tun_name_for_agent("Agent-A"), "lab-agenta");
        assert_eq!(ChainManager::tun_name_for_agent("!!!"), "lab-chain");
        let long = ChainManager::tun_name_for_agent("0123456789abcdef");
        assert_eq!(long, "lab-01234567");
        // Linux IFNAMSIZ is 16 including NUL.
        assert!(long.len() <= 15);
        assert!(long.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }

    #[tokio::test]
    async fn plan_prefers_most_specific_route() {
        let server = server_with_registry();
        server
            .agents()
            .write()
            .await
            .insert("wide".into(), agent("wide", "Wide", "10.0.1.4/16"));
        server
            .agents()
            .write()
            .await
            .insert("near".into(), agent("near", "Near", "10.0.5.4/24"));

        let plan = ChainManager::build_plan(&server, "10.0.5.20")
            .await
            .unwrap();
        assert!(matches!(
            &plan.actions[0],
            ChainAction::StartTunnel { agent_id, cidr, .. }
                if agent_id == "near" && cidr == "10.0.5.0/24"
        ));
        // Outside the /24 only the /16 can reach it.
        let plan = ChainManager::build_plan(&server, "10.0.9.1").await.unwrap();
        assert!(matches!(
            &plan.actions[0],
            ChainAction::StartTunnel { agent_id, .. } if agent_id == "wide"
        ));
    }

    #[tokio::test]
    async fn plan_is_blocked_when_owner_tunnel_serves_another_subnet() {
        let server = server_with_registry();
        let mut busy = agent("a", "A", "172.16.10.4/24");
        busy.interfaces_mut_add("192.168.50.4/24");
        busy.tunnel_active = true;
        busy.tunnel_subnet = Some("192.168.50.0/24".into());
        server.agents().write().await.insert("a".into(), busy);

        let plan = ChainManager::build_plan(&server, "172.16.10.9")
            .await
            .unwrap();
        assert!(!plan.ready);
        assert!(matches!(
            &plan.actions[..],
            [ChainAction::Blocked { reason }] if reason.contains("already has an active tunnel")
        ));
    }

    #[tokio::test]
    async fn plan_is_blocked_without_any_route() {
        let server = server_with_registry();
        server
            .agents()
            .write()
            .await
            .insert("a".into(), agent("a", "A", "172.16.10.4/24"));
        let plan = ChainManager::build_plan(&server, "8.8.8.8").await.unwrap();
        assert_eq!(plan.target, "8.8.8.8/32");
        assert!(matches!(&plan.actions[..], [ChainAction::Blocked { .. }]));
    }

    #[tokio::test]
    async fn plan_routes_through_remembered_dweller_behind_agent() {
        let server = server_with_registry();
        server
            .agents()
            .write()
            .await
            .insert("a".into(), agent("a", "A", "10.20.0.4/24"));
        server
            .dweller_registry()
            .write()
            .await
            .upsert(record("dw", "10.20.0.9"));

        let plan = ChainManager::build_plan(&server, "192.168.99.5")
            .await
            .unwrap();
        assert!(!plan.ready);
        assert!(matches!(
            &plan.actions[..],
            [
                ChainAction::StartTunnel { agent_id, .. },
                ChainAction::ConnectDweller { dweller_id, address, .. },
                ChainAction::RetryAfterDweller { .. },
            ] if agent_id == "a" && dweller_id == "dw" && address == "10.20.0.9:45454"
        ));

        // Parent tunnel already covering the dweller: reuse it.
        {
            let mut agents = server.agents().write().await;
            let parent = agents.get_mut("a").unwrap();
            parent.tunnel_active = true;
            parent.tunnel_subnet = Some("10.20.0.0/24".into());
        }
        let plan = ChainManager::build_plan(&server, "192.168.99.5")
            .await
            .unwrap();
        assert!(matches!(&plan.actions[0], ChainAction::ReuseTunnel { .. }));
    }

    #[tokio::test]
    async fn connected_or_unreachable_dwellers_are_not_planned() {
        let server = server_with_registry();
        server
            .agents()
            .write()
            .await
            .insert("a".into(), agent("a", "A", "10.20.0.4/24"));
        server
            .dweller_registry()
            .write()
            .await
            .upsert(record("far", "10.99.0.9"));
        let plan = ChainManager::build_plan(&server, "192.168.99.5")
            .await
            .unwrap();
        assert!(matches!(&plan.actions[..], [ChainAction::Blocked { .. }]));

        server
            .dweller_registry()
            .write()
            .await
            .upsert(record("dw", "10.20.0.9"));
        server
            .agents()
            .write()
            .await
            .insert("dw".into(), agent("dw", "DW", "10.30.0.4/24"));
        let plan = ChainManager::build_plan(&server, "192.168.99.5")
            .await
            .unwrap();
        assert!(matches!(&plan.actions[..], [ChainAction::Blocked { .. }]));
    }

    #[tokio::test]
    async fn plan_is_read_only_and_deterministic() {
        // AGENTS.md contract: `plan` never mutates tunnels, dwellers or selection.
        let server = server_with_registry();
        server
            .agents()
            .write()
            .await
            .insert("a".into(), agent("a", "A", "10.20.0.4/24"));
        server
            .dweller_registry()
            .write()
            .await
            .upsert(record("dw", "10.20.0.9"));

        let snapshot = |server: &LabyrinthServer| {
            let server = server.clone_for_tasks();
            async move {
                let agents = server.agents().read().await;
                let mut state: Vec<_> = agents
                    .values()
                    .map(|a| (a.id.clone(), a.tunnel_active, a.tunnel_subnet.clone()))
                    .collect();
                state.sort();
                let registry = server.dweller_registry().read().await;
                let dwellers = serde_json::to_string(&registry.list()).unwrap();
                (state, dwellers, server.current_agent().read().await.clone())
            }
        };

        let before = snapshot(&server).await;
        let first = ChainManager::build_plan(&server, "192.168.99.5")
            .await
            .unwrap();
        for target in ["10.20.0.50", "192.168.99.5", "8.8.8.8", "10.20.0.0/24"] {
            ChainManager::build_plan(&server, target).await.unwrap();
        }
        let _ = ChainManager::suggestions(&server).await;
        let again = ChainManager::build_plan(&server, "192.168.99.5")
            .await
            .unwrap();
        assert_eq!(first, again);
        assert_eq!(before, snapshot(&server).await);
    }
}
