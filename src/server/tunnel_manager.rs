use crate::error::{LabyrinthError, Result};
// Message import already present above
use crate::protocol::Message;
use crate::server::core::LabyrinthServer;
#[cfg(target_os = "linux")]
use crate::server::netstack_bridge::NetstackBridge;
#[cfg(target_os = "windows")]
use crate::server::netstack_bridge_windows::WindowsNetstackBridge;
#[cfg(target_os = "linux")]
use crate::server::privileges::PrivilegeManager;
use crate::styling;
use colored::Colorize;
use dialoguer::Input;
use std::process::Command;
use tracing::{error, info};

// Server-only TUN; userland stack handled by NetstackBridge

/// Single Responsibility: Tunnel management operations
pub struct TunnelManager;

impl TunnelManager {
    pub async fn start_tunnel(server: &LabyrinthServer) -> Result<()> {
        let current_id = server.current_agent().read().await.clone();
        if let Some(agent_id) = current_id {
            // Display Fullhouse Mode header
            println!(
                "\n{}",
                styling::format_section_title(
                    "Fullhouse Mode",
                    "IP tunneling and ligolo-style pivoting"
                )
            );
            println!("{}", "──────────────────────────".bright_black());
            println!("{}", styling::format_hint("The selected agent stays untouched until local preflight and route setup succeed."));
            println!();

            Self::run_fullhouse_preflight()?;
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

            #[cfg(target_os = "linux")]
            let tun = Self::setup_tunnel(&tun_name, &subnet).await?;
            #[cfg(target_os = "windows")]
            Self::setup_tunnel_windows(&tun_name, &subnet).await?;

            // Send tunnel start message to agent
            let mut agents = server.agents().write().await;
            if let Some(agent) = agents.get_mut(&agent_id) {
                let start_msg = Message::StartTunnel {
                    subnet: subnet.clone(),
                    tun_name: tun_name.clone(),
                };

                if let Err(e) = agent.sender.send(start_msg).await {
                    error!(
                        "Failed to send tunnel start request to agent {}: {}",
                        agent.id, e
                    );
                    return Err(LabyrinthError::Message(format!(
                        "Failed to send tunnel start request: {}",
                        e
                    )));
                }

                // Start server-side bridge to drive ligolo-like behavior
                #[cfg(target_os = "linux")]
                NetstackBridge::start(tun, agent.sender.clone());
                #[cfg(target_os = "windows")]
                {
                    WindowsNetstackBridge::start(&tun_name, agent.sender.clone()).map_err(|e| {
                        LabyrinthError::Message(format!("Failed to start Wintun bridge: {}", e))
                    })?;
                }

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

    fn run_fullhouse_preflight() -> Result<()> {
        println!(
            "{}",
            styling::format_section_title("Fullhouse Preflight", "host capability checks")
        );
        println!("{}", "──────────────────".bright_black());

        #[cfg(target_os = "linux")]
        {
            let root = PrivilegeManager::has_sudo_privileges();
            if root {
                println!("{}", styling::format_check_item("Root privileges detected"));
            } else {
                println!("{}", styling::format_cross_item("Root privileges missing"));
                println!(
                    "{}",
                    styling::format_hint(
                        "Re-run the server with sudo to create TUN devices and routing rules."
                    )
                );
                return Err(LabyrinthError::Message(
                    PrivilegeManager::create_sudo_error("Fullhouse mode"),
                ));
            }

            for bin in ["ip", "iptables", "sysctl"] {
                if Self::command_exists(bin) {
                    println!(
                        "{}",
                        styling::format_check_item(&format!("Found '{}'", bin))
                    );
                } else {
                    println!(
                        "{}",
                        styling::format_cross_item(&format!("Missing '{}'", bin))
                    );
                    println!(
                        "{}",
                        styling::format_hint(
                            "Install the missing networking utility before enabling Fullhouse."
                        )
                    );
                    return Err(LabyrinthError::Message(format!(
                        "Required system tool '{}' is missing",
                        bin
                    )));
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            let admin = Self::windows_is_admin()?;
            if admin {
                println!(
                    "{}",
                    styling::format_check_item("Administrator privileges detected")
                );
            } else {
                println!(
                    "{}",
                    styling::format_cross_item("Administrator privileges missing")
                );
                println!(
                    "{}",
                    styling::format_hint(
                        "Launch Labyrinth from an elevated PowerShell or Command Prompt."
                    )
                );
                return Err(LabyrinthError::Message(
                    "Fullhouse mode on Windows requires running as Administrator".to_string(),
                ));
            }

            let wintun_ok = unsafe { wintun::load().is_ok() };
            if wintun_ok {
                println!("{}", styling::format_check_item("Loaded wintun.dll"));
            } else {
                println!("{}", styling::format_cross_item("wintun.dll not found"));
                println!(
                    "{}",
                    styling::format_hint(
                        "Place wintun.dll beside labyrinth.exe or add it to PATH."
                    )
                );
                return Err(LabyrinthError::Message(
                    "wintun.dll is required for Windows Fullhouse mode. Place it next to labyrinth.exe or in PATH."
                        .to_string(),
                ));
            }

            if Self::command_exists("powershell") {
                println!("{}", styling::format_check_item("PowerShell available"));
            } else {
                println!("{}", styling::format_cross_item("PowerShell not available"));
                println!(
                    "{}",
                    styling::format_hint(
                        "PowerShell is used to assign IPs and routes to the Wintun adapter."
                    )
                );
                return Err(LabyrinthError::Message(
                    "PowerShell is required for Windows Fullhouse route setup".to_string(),
                ));
            }
        }

        println!("{}", styling::format_check_item("Preflight checks passed"));
        Ok(())
    }

    fn command_exists(cmd: &str) -> bool {
        #[cfg(target_os = "windows")]
        {
            Command::new("where")
                .arg(cmd)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        }

        #[cfg(not(target_os = "windows"))]
        {
            Command::new("sh")
                .args(["-c", &format!("command -v {}", cmd)])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        }
    }

    #[cfg(target_os = "windows")]
    fn windows_is_admin() -> Result<bool> {
        let cmd = "([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)";
        let out = Command::new("powershell")
            .args(["-NoProfile", "-Command", cmd])
            .output()?;
        if !out.status.success() {
            return Err(LabyrinthError::Message(format!(
                "Failed to check Windows admin privileges: {}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .to_ascii_lowercase()
            .contains("true"))
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
                                &format!(
                                    "Failed to send stop tunnel request to agent {}: {}",
                                    agent.id, e
                                )
                            )
                        );
                    }

                    // Cleanup local tunnel
                    if let Some(ref tun_name) = agent.tun_name {
                        #[cfg(target_os = "linux")]
                        let cleanup_result = Self::cleanup_tunnel(
                            tun_name,
                            agent
                                .tunnel_subnet
                                .as_ref()
                                .unwrap_or(&"unknown".to_string()),
                        )
                        .await;

                        #[cfg(target_os = "windows")]
                        let cleanup_result = Self::cleanup_tunnel_windows(tun_name).await;

                        if let Err(e) = cleanup_result {
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

    #[cfg(target_os = "linux")]
    async fn setup_tunnel(tun_name: &str, subnet: &str) -> Result<tokio_tun::Tun> {
        // Check for sudo privileges before attempting tunnel operations
        if !PrivilegeManager::has_sudo_privileges() {
            return Err(LabyrinthError::Message(
                PrivilegeManager::create_sudo_error("Fullhouse mode"),
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
        Self::run_command("ip", &["route", "replace", subnet, "dev", tun_name])?;

        // Setup iptables for NAT
        Self::ensure_iptables_rule(
            "iptables",
            &[
                "-t",
                "nat",
                "POSTROUTING",
                "-s",
                "10.0.0.0/24",
                "-j",
                "MASQUERADE",
            ],
        )?;
        Self::ensure_iptables_rule("iptables", &["FORWARD", "-i", tun_name, "-j", "ACCEPT"])?;
        Self::ensure_iptables_rule("iptables", &["FORWARD", "-o", tun_name, "-j", "ACCEPT"])?;

        Ok(tun)
    }

    #[cfg(target_os = "linux")]
    fn ensure_iptables_rule(cmd: &str, rule_args: &[&str]) -> Result<()> {
        let mut check_args = Vec::with_capacity(rule_args.len() + 1);
        if rule_args.starts_with(&["-t", "nat"]) {
            check_args.extend(["-t", "nat", "-C"]);
            check_args.extend(rule_args.iter().skip(2).copied());
        } else {
            check_args.push("-C");
            check_args.extend(rule_args.iter().copied());
        }

        if Self::command_succeeds(cmd, &check_args)? {
            return Ok(());
        }

        let mut add_args = Vec::with_capacity(rule_args.len() + 1);
        if rule_args.starts_with(&["-t", "nat"]) {
            add_args.extend(["-t", "nat", "-A"]);
            add_args.extend(rule_args.iter().skip(2).copied());
        } else {
            add_args.push("-A");
            add_args.extend(rule_args.iter().copied());
        }

        Self::run_command(cmd, &add_args)
    }

    #[cfg(target_os = "windows")]
    async fn setup_tunnel_windows(tun_name: &str, subnet: &str) -> Result<()> {
        info!(
            "[+] Preparing Wintun interface {} for subnet {}",
            tun_name, subnet
        );

        let ps = format!(
            "$name='{}'; \
            $a=Get-NetAdapter -Name $name -ErrorAction SilentlyContinue; \
            if (-not $a) {{ exit 0 }}; \
            $idx=$a.ifIndex; \
            New-NetIPAddress -InterfaceIndex $idx -IPAddress 10.0.0.1 -PrefixLength 24 -AddressFamily IPv4 -ErrorAction SilentlyContinue | Out-Null; \
            New-NetRoute -DestinationPrefix '{}' -InterfaceIndex $idx -NextHop 0.0.0.0 -ErrorAction SilentlyContinue | Out-Null;",
            tun_name, subnet
        );

        let output = Command::new("powershell")
            .args(["-NoProfile", "-Command", &ps])
            .output()?;
        if !output.status.success() {
            return Err(LabyrinthError::Message(format!(
                "Failed to configure Wintun routes: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        Ok(())
    }

    #[cfg(target_os = "linux")]
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

    #[cfg(target_os = "windows")]
    async fn cleanup_tunnel_windows(tun_name: &str) -> Result<()> {
        let ps = format!(
            "$name='{}'; $a=Get-NetAdapter -Name $name -ErrorAction SilentlyContinue; \
            if ($a) {{ Remove-NetIPAddress -InterfaceIndex $a.ifIndex -Confirm:$false -ErrorAction SilentlyContinue | Out-Null }}",
            tun_name
        );
        let _ = Command::new("powershell")
            .args(["-NoProfile", "-Command", &ps])
            .output();
        Ok(())
    }

    #[cfg(target_os = "linux")]
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

    #[cfg(target_os = "linux")]
    fn command_succeeds(cmd: &str, args: &[&str]) -> Result<bool> {
        Ok(Command::new(cmd).args(args).output()?.status.success())
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

#[cfg(test)]
mod tests {
    use super::TunnelManager;

    #[test]
    fn validate_cidr_accepts_ipv4_networks() {
        assert!(TunnelManager::validate_cidr("192.168.100.0/24"));
    }

    #[test]
    fn validate_cidr_rejects_invalid_prefix() {
        assert!(!TunnelManager::validate_cidr("192.168.100.0/99"));
    }
}
