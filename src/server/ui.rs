use crate::error::{LabyrinthError, Result};
use crate::protocol::AgentKind;
use crate::server::core::LabyrinthServer;
use crate::server::topology::TopologyManager;

use crate::styling;
use colored::Colorize;
use dialoguer::Select;

/// Single Responsibility: User interface operations
pub struct ServerUI;

impl ServerUI {
    pub async fn list_agents(server: &LabyrinthServer) {
        let agents = server.agents().read().await;
        if agents.is_empty() {
            println!(
                "\n{}",
                styling::format_warning_msg(styling::WARNING_INDICATOR, "No agents connected")
            );
            println!(
                "{}",
                styling::format_hint("Start an agent, then use 'agents' to refresh.")
            );
            return;
        }

        println!(
            "\n{}",
            styling::format_section_title("Connected Agents", &format!("{} online", agents.len()))
        );
        println!("{}", styling::format_separator(styling::SECTION_SEPARATOR));

        let agent_list: Vec<_> = agents.values().collect();
        for (index, agent) in agent_list.iter().enumerate() {
            // Agent card header
            println!("Agent {}", (index + 1).to_string().cyan().bold());

            // Agent details in vertical format
            println!(
                "{}",
                styling::format_field("ID:", &styling::format_agent_id(&agent.id).to_string())
            );
            println!(
                "{}",
                styling::format_field(
                    "Name:",
                    &styling::format_agent_name(&agent.info.name).to_string()
                )
            );
            println!(
                "{}",
                styling::format_field(
                    "System:",
                    &styling::format_system_info(&format!("{}/{}", agent.info.os, agent.info.arch))
                        .to_string()
                )
            );
            println!(
                "{}",
                styling::format_field(
                    "Type:",
                    match agent.info.kind {
                        AgentKind::Dweller => "Dweller",
                        AgentKind::Generic => "Agent",
                    }
                )
            );
            println!(
                "{}",
                styling::format_field("Status:", &styling::format_status_badge("Online", true))
            );

            // Tunnel status with color coding
            let tunnel_status = if agent.tunnel_active {
                styling::format_status_active(&format!(
                    "Active ({})",
                    agent
                        .tunnel_subnet
                        .as_ref()
                        .unwrap_or(&"Unknown".to_string())
                ))
                .to_string()
            } else {
                styling::format_status_inactive("Inactive").to_string()
            };
            println!(
                "{}",
                styling::format_field("Fullhouse (Tunnel):", &tunnel_status)
            );
            let route_hint = TopologyManager::best_route_for_agent(&agent.info.interfaces)
                .map(|route| route.cidr.clone())
                .unwrap_or_else(|| "manual CIDR required".to_string());
            println!("{}", styling::format_field("Auto route:", &route_hint));

            // Add visual separator between agents (except for the last one)
            if index < agent_list.len() - 1 {
                println!(
                    "{}{}",
                    styling::INDENT_LEVEL_1,
                    styling::format_separator(styling::SUBSECTION_SEPARATOR)
                );
            }
        }
        println!(); // Add spacing after the list
    }

    pub async fn select_agent(server: &LabyrinthServer) -> Result<()> {
        // Don't run cleanup during select - let the periodic health check handle it
        let agents = server.agents().read().await;
        if agents.is_empty() {
            println!(
                "{}",
                styling::format_error_msg(styling::ERROR_INDICATOR, "No agents available")
            );
            return Ok(());
        }

        let agent_list: Vec<_> = agents.values().collect();
        let selections: Vec<String> = agent_list
            .iter()
            .map(|a| {
                format!(
                    "{} - {} ({}) [{}]",
                    a.id,
                    a.info.name,
                    a.info.hostname,
                    match a.info.kind {
                        AgentKind::Dweller => "dweller",
                        AgentKind::Generic => "agent",
                    }
                )
            })
            .collect();

        println!(
            "\n{}",
            styling::format_section_title("Available Agents", "choose an active session")
        );
        println!("{}", styling::format_separator(styling::SECTION_SEPARATOR));

        for (i, selection) in selections.iter().enumerate() {
            println!("  {}. {}", i + 1, selection.cyan());
        }
        println!(
            "\n{}",
            styling::format_hint(
                "Tip: use 'info' after selecting to inspect interfaces and routing context."
            )
        );
        println!();

        let selection = Select::new()
            .with_prompt("Select an agent")
            .items(&selections)
            .interact()
            .map_err(|e| LabyrinthError::Message(format!("Selection error: {}", e)))?;

        let selected_agent = &agent_list[selection];
        *server.current_agent().write().await = Some(selected_agent.id.clone());

        println!(
            "\n{} Selected agent: {} ({})",
            styling::format_success_msg(styling::SUCCESS_INDICATOR, "").trim_start(),
            styling::format_agent_name(&selected_agent.info.name),
            styling::format_agent_id(&selected_agent.id)
        );

        Ok(())
    }

    pub async fn show_agent_info(server: &LabyrinthServer) -> Result<()> {
        let current_id = server.current_agent().read().await.clone();
        if let Some(agent_id) = current_id {
            let agents = server.agents().read().await;
            if let Some(agent) = agents.get(&agent_id) {
                // Agent Profile Header
                println!(
                    "\n{}",
                    styling::format_section_title("Agent Profile", &agent.info.name)
                );
                println!("{}", styling::format_separator("────────────"));

                // Basic agent information in structured format
                println!(
                    "{}",
                    styling::format_field("ID:", &styling::format_agent_id(&agent.id).to_string())
                );
                println!(
                    "{}",
                    styling::format_field(
                        "Name:",
                        &styling::format_agent_name(&agent.info.name).to_string()
                    )
                );
                println!("{}", styling::format_field("Host:", &agent.info.hostname));
                println!(
                    "{}",
                    styling::format_field(
                        "System:",
                        &styling::format_system_info(&format!(
                            "{}/{}",
                            agent.info.os, agent.info.arch
                        ))
                        .to_string()
                    )
                );
                println!(
                    "{}",
                    styling::format_field(
                        "Type:",
                        match agent.info.kind {
                            AgentKind::Dweller => "Dweller",
                            AgentKind::Generic => "Agent",
                        }
                    )
                );

                // Connection status with color coding
                let connection_status = if agent.tunnel_active {
                    styling::format_status_active(&format!(
                        "Active - {}",
                        agent
                            .tunnel_subnet
                            .as_ref()
                            .unwrap_or(&"Unknown".to_string())
                    ))
                    .to_string()
                } else {
                    styling::format_status_active("Connected").to_string()
                };
                println!(
                    "{}",
                    styling::format_field("Connection:", &connection_status)
                );

                // Network Interfaces section
                println!(
                    "\n{}",
                    styling::format_section_title(
                        "Network Interfaces",
                        &format!("{} detected", agent.info.interfaces.len())
                    )
                );
                println!("{}", styling::format_separator(styling::SECTION_SEPARATOR));

                for (i, iface) in agent.info.interfaces.iter().enumerate() {
                    // Interface header with number and name
                    println!(
                        "{}",
                        styling::format_numbered_item(i + 1, &iface.name, &iface.hardware_addr)
                    );

                    // Display addresses with proper indentation
                    for addr in &iface.addresses {
                        println!(
                            "{}{}",
                            styling::INDENT_LEVEL_2,
                            styling::format_network_address(addr)
                        );
                    }

                    // Add spacing between interfaces (except for the last one)
                    if i < agent.info.interfaces.len() - 1 {
                        println!();
                    }
                }

                let detected_routes = TopologyManager::detect_agent_routes(&agent.info.interfaces);
                println!(
                    "\n{}",
                    styling::format_section_title("Detected Routes", "Fullhouse auto candidates")
                );
                println!("{}", styling::format_separator(styling::SECTION_SEPARATOR));
                if detected_routes.is_empty() {
                    println!(
                        "{}",
                        styling::format_warning_msg(
                            styling::WARNING_INDICATOR,
                            "No routable IPv4 CIDR detected"
                        )
                    );
                } else {
                    for (i, route) in detected_routes.iter().take(5).enumerate() {
                        println!(
                            "{}",
                            styling::format_numbered_item(
                                i + 1,
                                &route.cidr,
                                &format!("{} from {}", route.interface_name, route.source_address)
                            )
                        );
                    }
                }

                println!(); // Add final spacing
            } else {
                println!(
                    "{}",
                    styling::format_error_msg(styling::ERROR_INDICATOR, "Selected agent not found")
                );
            }
        } else {
            println!(
                "{}",
                styling::format_warning_msg(
                    styling::WARNING_INDICATOR,
                    "No agent selected. Use 'select' command first."
                )
            );
        }
        Ok(())
    }

    pub async fn show_status(server: &LabyrinthServer) {
        let agents = server.agents().read().await;
        let current_agent = server.current_agent().read().await.clone();
        let active_tunnels = agents.values().filter(|a| a.tunnel_active).count();

        println!(
            "\n{}",
            styling::format_section_title("Labyrinth Status", "control plane overview")
        );
        println!("{}", "───────────────".bright_black());
        println!("{:<20} {}", "Server:", "Running".green());
        println!(
            "{:<20} {}",
            "Security:",
            if server.auth_required() {
                "Authentication Enabled".green()
            } else {
                "Authentication Disabled".red()
            }
        );
        println!("{:<20} {}", "Agents:", agents.len().to_string().cyan());
        println!(
            "{:<20} {}",
            "Active connections:",
            active_tunnels.to_string().cyan()
        );

        if let Some(agent_id) = current_agent {
            if let Some(agent) = agents.get(&agent_id) {
                println!(
                    "\n{}",
                    styling::format_section_title("Selected Agent", "active context")
                );
                println!("{}", "──────────────".bright_black());
                println!(
                    "{:<20} {} ({})",
                    "Agent:",
                    agent.info.name.cyan(),
                    agent_id.bright_black()
                );
                println!(
                    "{:<20} {}",
                    "Fullhouse (Tunnel):",
                    if agent.tunnel_active {
                        "Active".green()
                    } else {
                        "Inactive".red()
                    }
                );
                let route_hint = TopologyManager::best_route_for_agent(&agent.info.interfaces)
                    .map(|route| route.cidr.clone())
                    .unwrap_or_else(|| "manual CIDR required".to_string());
                println!("{:<20} {}", "Auto route:", route_hint.cyan());
            }
        }
        println!();
    }

    pub async fn show_topology(server: &LabyrinthServer) {
        let agents = server.agents().read().await;
        let snapshot = TopologyManager::build_snapshot(&agents);

        println!(
            "\n{}",
            styling::format_section_title("Route Topology", "multi-hop control plane")
        );
        println!("{}", styling::format_separator(styling::SECTION_SEPARATOR));

        if snapshot.routes.is_empty() {
            println!(
                "{}",
                styling::format_warning_msg(
                    styling::WARNING_INDICATOR,
                    "No routable agent CIDRs detected"
                )
            );
            println!();
            return;
        }

        println!("{}", "Advertised Routes".cyan().bold());
        for route in &snapshot.routes {
            println!(
                "{} {} via {} ({}, score {})",
                styling::INDENT_LEVEL_1,
                styling::format_agent_name(&route.cidr),
                route.agent_name.bright_white(),
                route.interface_name.bright_black(),
                route.score
            );
        }

        println!();
        println!("{}", "Shared Networks".cyan().bold());
        if snapshot.shared_routes.is_empty() {
            println!(
                "{}",
                styling::format_hint("No shared agent networks detected yet.")
            );
        } else {
            for group in &snapshot.shared_routes {
                println!(
                    "{} {}",
                    styling::INDENT_LEVEL_1,
                    styling::format_agent_name(&group.cidr)
                );
                for agent in &group.agents {
                    println!("{}{}", styling::INDENT_LEVEL_2, agent.bright_white());
                }
            }
        }

        println!();
        println!("{}", "Route Conflicts".cyan().bold());
        if snapshot.conflicts.is_empty() {
            println!(
                "{}",
                styling::format_hint("No overlapping route ownership conflicts detected.")
            );
        } else {
            for conflict in &snapshot.conflicts {
                println!(
                    "{} {} is advertised by {} agents",
                    styling::INDENT_LEVEL_1,
                    styling::format_agent_name(&conflict.cidr),
                    conflict.agents.len().to_string().yellow()
                );
            }
        }
        println!();
    }
}
