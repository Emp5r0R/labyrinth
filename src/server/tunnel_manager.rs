use crate::error::{LabyrinthError, Result};
// Message import already present above
use crate::server::core::LabyrinthServer;
use crate::server::privileges::PrivilegeManager;
use crate::styling;
use colored::Colorize;
use dialoguer::Input;
use std::process::Command;
use tracing::{error, info};
use crate::protocol::Message;
use crate::server::netstack_bridge::NetstackBridge;

// Server-only TUN; userland stack handled by NetstackBridge

/// Single Responsibility: Tunnel management operations
pub struct TunnelManager;

impl TunnelManager {
    pub async fn start_tunnel(server: &LabyrinthServer) -> Result<()> {
        let current_id = server.current_agent().read().await.clone();
        if let Some(agent_id) = current_id {
            // Display Fullhouse Mode header
            println!("\n{}", "Fullhouse Mode (IP Tunneling)".cyan().bold());
            println!("{}", "──────────────────────────".bright_black());
            println!();

            // Get tunnel configuration from user with validation
            let subnet: String = loop {
                let input: String = Input::new()
                    .with_prompt("Target subnet in CIDR notation")
                    .interact_text()
                    .map_err(|e| LabyrinthError::Message(format!("Input error: {}", e)))?;

                // Validate CIDR notation
                if Self::validate_cidr(&input) {
                    println!(
                        "{}{}",
                        styling::INDENT_LEVEL_1,
                        styling::format_check_item(&format!(
                            "Valid subnet format: {}",
                            styling::format_agent_name(&input)
                        ))
                    );
                    break input;
                } else {
                    println!(
                        "{}{}",
                        styling::INDENT_LEVEL_1,
                        styling::format_cross_item("Invalid subnet format")
                    );
                    println!("{}Format: network/prefix", styling::INDENT_LEVEL_1);
                    println!("{}Examples:", styling::INDENT_LEVEL_1);
                    println!(
                        "{} {} Single mapping:    192.168.1.100/32",
                        styling::INDENT_LEVEL_2,
                        styling::ARROW_INDICATOR.cyan()
                    );
                    println!(
                        "{} {} Network range:  192.168.1.0/24",
                        styling::INDENT_LEVEL_2,
                        styling::ARROW_INDICATOR.cyan()
                    );
                    println!(
                        "{} {} Entire network: 10.0.0.0/8",
                        styling::INDENT_LEVEL_2,
                        styling::ARROW_INDICATOR.cyan()
                    );
                    println!();
                }
            };

            let tun_name: String = Input::new()
                .with_prompt("Interface name")
                .default("labyrinth".to_string())
                .interact_text()
                .map_err(|e| LabyrinthError::Message(format!("Input error: {}", e)))?;

            println!(
                "{}{}",
                styling::INDENT_LEVEL_1,
                styling::format_check_item(&format!(
                    "Interface: {}",
                    styling::format_agent_name(&tun_name)
                ))
            );

            // Create TUN interface and setup routing
            let tun = Self::setup_tunnel(&tun_name, &subnet).await?;

            // Send tunnel start message to agent
            let mut agents = server.agents().write().await;
            if let Some(agent) = agents.get_mut(&agent_id) {
                let start_msg = Message::StartTunnel {
                    subnet: subnet.clone(),
                    tun_name: tun_name.clone(),
                };

                if let Err(e) = agent.sender.send(start_msg).await {
                    error!("Failed to send tunnel start request to agent {}: {}", agent.id, e);
                    return Err(LabyrinthError::Message(format!("Failed to send tunnel start request: {}", e)));
                }

                // Start server-side bridge to drive ligolo-like behavior
                NetstackBridge::start(tun, agent.sender.clone());

                // Update agent status
                agent.tunnel_active = true;
                agent.tunnel_subnet = Some(subnet.clone());
                agent.tun_name = Some(tun_name.clone());

                println!(
                    "\n{} Fullhouse Mode Active",
                    styling::format_success_msg(styling::CHECK_INDICATOR, "")
                        .trim_start()
                        .bold()
                );
                println!(
                    "Tunnel established for subnet: {}",
                    styling::format_agent_name(&subnet)
                );
                println!("Interface: {}", styling::format_agent_name(&tun_name));
                println!();
            } else {
                return Err(LabyrinthError::Message(
                    "Selected agent not found".to_string(),
                ));
            }
        }
        else {
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

    pub async fn stop_tunnel(server: &LabyrinthServer) -> Result<()> {
        let current_id = server.current_agent().read().await.clone();
        if let Some(agent_id) = current_id {
            let mut agents = server.agents().write().await;
            if let Some(agent) = agents.get_mut(&agent_id) {
                if !agent.tunnel_active {
                    println!(
                        "{}",
                        styling::format_warning_msg(
                            styling::WARNING_INDICATOR,
                            "No active tunnel or port forwarding for this agent"
                        )
                    );
                    return Ok(());
                }

                if server.has_port_forwarding(&agent_id).await {
                    let stopped_ports = server.stop_port_forwarding_for_agent(&agent_id).await;

                    let connection_ids = server.connection_ids_for_agent(&agent_id).await;
                    if let Some(stream_manager) = server.get_stream_manager().await {
                        for connection_id in &connection_ids {
                            let _ = stream_manager.terminate_stream(*connection_id).await;
                        }
                    }
                    if let Some(connection_manager) = server.get_connection_manager().await {
                        for connection_id in &connection_ids {
                            let _ = connection_manager.cleanup_connection(connection_id).await;
                        }
                    }
                    for connection_id in connection_ids {
                        let _ = server.unregister_connection_owner(&connection_id).await;
                    }

                    agent.tunnel_active = false;
                    agent.tunnel_subnet = None;

                    if stopped_ports.is_empty() {
                        println!(
                            "{}",
                            styling::format_warning_msg(
                                styling::WARNING_INDICATOR,
                                "Port forwarding listeners were not running"
                            )
                        );
                    } else {
                        println!(
                            "{}",
                            styling::format_success_msg(
                                styling::SUCCESS_INDICATOR,
                                &format!(
                                    "Port forwarding stopped on ports: {}",
                                    stopped_ports
                                        .iter()
                                        .map(|p| p.to_string())
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                )
                            )
                        );
                    }
                } else {
                    // It's a tunnel - send stop message and cleanup
                    let stop_msg = Message::StopTunnel;

                    if let Err(e) = agent.sender.send(stop_msg).await {
                        println!(
                            "{}",
                            styling::format_warning_msg(
                                styling::WARNING_INDICATOR,
                                &format!("Failed to send stop tunnel request to agent {}: {}", agent.id, e)
                            )
                        );
                    }

                    // Cleanup local tunnel
                    if let Some(ref tun_name) = agent.tun_name {
                        if let Err(e) = Self::cleanup_tunnel(
                            tun_name,
                            agent
                                .tunnel_subnet
                                .as_ref()
                                .unwrap_or(&"unknown".to_string()),
                        )
                        .await
                        {
                            println!(
                                "{}",
                                styling::format_warning_msg(
                                    styling::WARNING_INDICATOR,
                                    &format!("Tunnel cleanup failed: {}", e)
                                )
                            );
                        }
                    }

                    agent.tunnel_active = false;
                    agent.tunnel_subnet = None;
                    agent.tun_name = None;

                    println!(
                        "{}",
                        styling::format_success_msg(styling::SUCCESS_INDICATOR, "Tunnel stopped")
                    );
                }
            } else {
                return Err(LabyrinthError::Message(
                    "Selected agent not found".to_string(),
                ));
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

    async fn setup_tunnel(tun_name: &str, subnet: &str) -> Result<tokio_tun::Tun> {
        // Check for sudo privileges before attempting tunnel operations
        if !PrivilegeManager::has_sudo_privileges() {
            return Err(LabyrinthError::Message(
                PrivilegeManager::create_sudo_error("Fullhouse mode")
            ));
        }

        info!(
            "[+] Setting up tunnel interface {} for subnet {}",
            tun_name, subnet
        );

        // Create TUN interface
        let tun_ip = "10.0.0.1"; // Default server IP for tunnel
        let tun = tokio_tun::Tun::builder()
            .name(tun_name)
            .tap(false)
            .packet_info(false)
            .up()
            .address(tun_ip.parse().unwrap())
            .try_build()?;

        // Setup routing
        Self::run_command("sysctl", &["-w", "net.ipv4.ip_forward=1"])?;
        Self::run_command("ip", &["route", "add", subnet, "dev", tun_name])?;

        // Setup iptables for NAT
        Self::run_command(
            "iptables",
            &[
                "-t",
                "nat",
                "-A",
                "POSTROUTING",
                "-s",
                "10.0.0.0/24",
                "-j",
                "MASQUERADE",
            ],
        )?;
        Self::run_command(
            "iptables",
            &["-A", "FORWARD", "-i", tun_name, "-j", "ACCEPT"],
        )?;
        Self::run_command(
            "iptables",
            &["-A", "FORWARD", "-o", tun_name, "-j", "ACCEPT"],
        )?;

        Ok(tun)
    }

    async fn cleanup_tunnel(tun_name: &str, subnet: &str) -> Result<()> {
        info!(
            "[+] Cleaning up tunnel interface {} for subnet {}",
            tun_name, subnet
        );

        // Remove routes and iptables rules
        let _ = Self::run_command("ip", &["route", "del", subnet, "dev", tun_name]);
        let _ = Self::run_command(
            "iptables",
            &[
                "-t",
                "nat",
                "-D",
                "POSTROUTING",
                "-s",
                "10.0.0.0/24",
                "-j",
                "MASQUERADE",
            ],
        );
        let _ = Self::run_command(
            "iptables",
            &["-D", "FORWARD", "-i", tun_name, "-j", "ACCEPT"],
        );
        let _ = Self::run_command(
            "iptables",
            &["-D", "FORWARD", "-o", tun_name, "-j", "ACCEPT"],
        );
        let _ = Self::run_command("ip", &["link", "del", tun_name]);

        Ok(())
    }

    fn run_command(cmd: &str, args: &[&str]) -> Result<()> {
        let output = Command::new(cmd).args(args).output()?;
        if !output.status.success() {
            return Err(LabyrinthError::Message(format!(
                "Command failed: {} {:?} -> {}",
                cmd,
                args,
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(())
    }

    fn validate_cidr(input: &str) -> bool {
        // Check if input contains CIDR notation (has a slash)
        if !input.contains('/') {
            return false;
        }

        let parts: Vec<&str> = input.split('/').collect();
        if parts.len() != 2 {
            return false;
        }

        // Validate IP address part
        let ip_part = parts[0];
        let prefix_part = parts[1];

        // Try to parse as IPv4 address
        if ip_part.parse::<std::net::Ipv4Addr>().is_ok() {
            // Validate prefix length for IPv4 (0-32)
            if let Ok(prefix) = prefix_part.parse::<u8>() {
                return prefix <= 32;
            }
        }

        // Try to parse as IPv6 address
        if ip_part.parse::<std::net::Ipv6Addr>().is_ok() {
            // Validate prefix length for IPv6 (0-128)
            if let Ok(prefix) = prefix_part.parse::<u8>() {
                return prefix <= 128;
            }
        }

        false
    }
}
