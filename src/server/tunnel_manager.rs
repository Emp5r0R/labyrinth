use crate::error::{LabyrinthError, Result};
use crate::protocol::Message;
use crate::server::core::LabyrinthServer;
#[cfg(target_os = "windows")]
use crate::server::netstack_bridge_windows::WindowsNetstackBridge;
#[cfg(target_os = "linux")]
use crate::server::privileges::PrivilegeManager;
#[cfg(target_os = "linux")]
use crate::server::quic_stream_bridge::QuicStreamBridge;
use crate::server::topology::{DetectedRoute, TopologyManager};
#[cfg(target_os = "linux")]
use crate::streaming::{models::PortMapping, ConnectionId, StreamMessage};
use crate::styling;
use colored::Colorize;
use dialoguer::Input;
#[cfg(target_os = "linux")]
use std::mem;
#[cfg(target_os = "linux")]
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::process::Command;
#[cfg(target_os = "linux")]
use std::sync::Arc;
#[cfg(target_os = "linux")]
use tokio::net::TcpListener;
use tokio::time::{timeout, Duration};
#[cfg(target_os = "linux")]
use tracing::warn;
use tracing::{error, info};

const AGENT_CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(target_os = "linux")]
const ARIADNE_PROXY_BIND_ADDR: &str = "127.0.0.1";
#[cfg(target_os = "linux")]
const SO_ORIGINAL_DST: libc::c_int = 80;

// Server-only TUN; userland stack handled by NetstackBridge

/// Single Responsibility: Tunnel management operations
pub struct TunnelManager;

static ARIADNE_NETWORK_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

impl TunnelManager {
    pub async fn start_tunnel_for_agent(
        server: &LabyrinthServer,
        agent_id: &str,
        subnet: &str,
        tun_name: &str,
    ) -> Result<()> {
        if !Self::validate_cidr(subnet) {
            return Err(LabyrinthError::Message(format!(
                "Invalid subnet format: {}",
                subnet
            )));
        }

        let subnet = Self::normalize_ariadne_subnet(subnet)?;
        Self::validate_tunnel_name(tun_name)?;
        let _network_guard = ARIADNE_NETWORK_LOCK.lock().await;

        // Check state before privileged preflight. Repeating an idempotent request must
        // remain cheap and must not fail because the caller temporarily lost sudo.

        let agent_sender = {
            let agents = server.agents().read().await;
            let Some(agent) = agents.get(agent_id) else {
                return Err(LabyrinthError::Message(
                    "Selected agent not found".to_string(),
                ));
            };

            if agent.tunnel_active {
                if agent
                    .tunnel_subnet
                    .as_deref()
                    .map(|active| active == subnet)
                    .unwrap_or(false)
                {
                    return Ok(());
                }
                return Err(LabyrinthError::Message(format!(
                    "{} already has an active tunnel for {}",
                    agent.info.name,
                    agent.tunnel_subnet.as_deref().unwrap_or("another subnet")
                )));
            }

            if agents.iter().any(|(other_id, other)| {
                other_id != agent_id
                    && other.tunnel_active
                    && other.tunnel_subnet.as_deref() == Some(subnet.as_str())
            }) {
                return Err(LabyrinthError::Message(format!(
                    "Subnet {} already has an active Ariadne tunnel on another agent",
                    subnet
                )));
            }

            agent.sender.clone()
        };

        if server
            .ariadne_snapshots()
            .await
            .iter()
            .any(|snapshot| snapshot.agent_id == agent_id)
        {
            return Err(LabyrinthError::Message(
                "Ariadne listener already exists for selected agent; stop it before retrying"
                    .to_string(),
            ));
        }

        Self::run_ariadne_preflight()?;

        #[cfg(target_os = "linux")]
        Self::setup_tunnel(server, agent_id, &agent_sender, tun_name, &subnet).await?;
        #[cfg(target_os = "windows")]
        Self::setup_tunnel_windows(tun_name, &subnet).await?;

        let start_msg = Message::StartTunnel {
            subnet: subnet.clone(),
            tun_name: tun_name.to_string(),
        };

        if let Err(e) = Self::send_agent_message(&agent_sender, start_msg, "start tunnel").await {
            #[cfg(target_os = "linux")]
            let _ = Self::cleanup_tunnel(server, agent_id, tun_name, &subnet).await;
            #[cfg(target_os = "windows")]
            let _ = Self::cleanup_tunnel_windows(tun_name, &subnet).await;
            return Err(LabyrinthError::Message(format!(
                "Failed to send tunnel start request: {}",
                e
            )));
        }

        #[cfg(target_os = "windows")]
        {
            if let Err(e) = WindowsNetstackBridge::start(tun_name, agent_sender.clone()) {
                let _ =
                    Self::send_agent_message(&agent_sender, Message::StopTunnel, "rollback tunnel")
                        .await;
                let _ = Self::cleanup_tunnel_windows(tun_name, &subnet).await;
                return Err(LabyrinthError::Message(format!(
                    "Failed to start Wintun bridge: {}",
                    e
                )));
            }
        }

        let mut agents = server.agents().write().await;
        if let Some(agent) = agents.get_mut(agent_id) {
            agent.tunnel_active = true;
            agent.tunnel_subnet = Some(subnet);
            agent.tun_name = Some(tun_name.to_string());
        } else {
            drop(agents);
            let _ = Self::send_agent_message(&agent_sender, Message::StopTunnel, "rollback tunnel")
                .await;
            #[cfg(target_os = "linux")]
            let _ = Self::cleanup_tunnel(server, agent_id, tun_name, &subnet).await;
            #[cfg(target_os = "windows")]
            let _ = Self::cleanup_tunnel_windows(tun_name, &subnet).await;
            return Err(LabyrinthError::Message(
                "Selected agent disconnected while starting tunnel".to_string(),
            ));
        }

        Ok(())
    }

    pub async fn start_tunnel(server: &LabyrinthServer) -> Result<()> {
        let current_id = server.current_agent().read().await.clone();
        if let Some(agent_id) = current_id {
            // Display Ariadne Mode header
            println!(
                "\n{}",
                styling::format_section_title(
                    "Ariadne Mode",
                    "IP tunneling and ligolo-style pivoting"
                )
            );
            println!("{}", "──────────────────────────".bright_black());
            println!("{}", styling::format_hint("The selected agent stays untouched until local preflight and route setup succeed."));
            println!();

            Self::run_ariadne_preflight()?;
            println!();

            let (agent_sender, route_candidates) = {
                let agents = server.agents().read().await;
                let Some(agent) = agents.get(&agent_id) else {
                    return Err(LabyrinthError::Message(
                        "Selected agent not found".to_string(),
                    ));
                };
                (
                    agent.sender.clone(),
                    TopologyManager::detect_agent_routes(&agent.info.interfaces),
                )
            };

            Self::print_detected_routes(&route_candidates);

            // Get tunnel configuration from detected agent routes with manual override.
            let default_subnet = route_candidates.first().map(|route| route.cidr.clone());
            let subnet: String = loop {
                let mut prompt = Input::new().with_prompt("Target subnet in CIDR notation");
                if let Some(default) = &default_subnet {
                    prompt = prompt.default(default.clone());
                }

                let input: String = prompt
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
            let subnet = Self::normalize_ariadne_subnet(&subnet)?;
            Self::validate_tunnel_name(&tun_name)?;
            let _network_guard = ARIADNE_NETWORK_LOCK.lock().await;
            {
                let agents = server.agents().read().await;
                let Some(agent) = agents.get(&agent_id) else {
                    return Err(LabyrinthError::Message(
                        "Selected agent disconnected before tunnel setup".to_string(),
                    ));
                };
                if agent.tunnel_active {
                    if agent.tunnel_subnet.as_deref() == Some(subnet.as_str()) {
                        println!(
                            "{}",
                            styling::format_hint("Ariadne tunnel already active; reusing it.")
                        );
                        return Ok(());
                    }
                    return Err(LabyrinthError::Message(format!(
                        "{} already has an active tunnel for {}",
                        agent.info.name,
                        agent.tunnel_subnet.as_deref().unwrap_or("another subnet")
                    )));
                }
                if agents.iter().any(|(other_id, other)| {
                    other_id != &agent_id
                        && other.tunnel_active
                        && other.tunnel_subnet.as_deref() == Some(subnet.as_str())
                }) {
                    return Err(LabyrinthError::Message(format!(
                        "Subnet {} already has an active Ariadne tunnel on another agent",
                        subnet
                    )));
                }
            }
            if server
                .ariadne_snapshots()
                .await
                .iter()
                .any(|snapshot| snapshot.agent_id == agent_id)
            {
                return Err(LabyrinthError::Message(
                    "Ariadne listener already exists for selected agent; stop it before retrying"
                        .to_string(),
                ));
            }

            println!(
                "{}{}",
                styling::INDENT_LEVEL_1,
                styling::format_check_item(&format!(
                    "Interface: {}",
                    styling::format_agent_name(&tun_name)
                ))
            );

            #[cfg(target_os = "linux")]
            Self::setup_tunnel(server, &agent_id, &agent_sender, &tun_name, &subnet).await?;
            #[cfg(target_os = "windows")]
            Self::setup_tunnel_windows(&tun_name, &subnet).await?;

            let start_msg = Message::StartTunnel {
                subnet: subnet.clone(),
                tun_name: tun_name.clone(),
            };

            if let Err(e) = Self::send_agent_message(&agent_sender, start_msg, "start tunnel").await
            {
                #[cfg(target_os = "linux")]
                let _ = Self::cleanup_tunnel(server, &agent_id, &tun_name, &subnet).await;
                #[cfg(target_os = "windows")]
                let _ = Self::cleanup_tunnel_windows(&tun_name, &subnet).await;
                error!(
                    "Failed to send tunnel start request to agent {}: {}",
                    agent_id, e
                );
                return Err(LabyrinthError::Message(format!(
                    "Failed to send tunnel start request: {}",
                    e
                )));
            }

            #[cfg(target_os = "windows")]
            {
                if let Err(e) = WindowsNetstackBridge::start(&tun_name, agent_sender.clone()) {
                    let _ = Self::send_agent_message(
                        &agent_sender,
                        Message::StopTunnel,
                        "rollback tunnel",
                    )
                    .await;
                    let _ = Self::cleanup_tunnel_windows(&tun_name, &subnet).await;
                    return Err(LabyrinthError::Message(format!(
                        "Failed to start Wintun bridge: {}",
                        e
                    )));
                }
            }

            let mut agents = server.agents().write().await;
            if let Some(agent) = agents.get_mut(&agent_id) {
                agent.tunnel_active = true;
                agent.tunnel_subnet = Some(subnet.clone());
                agent.tun_name = Some(tun_name.clone());

                println!(
                    "\n{} Ariadne Mode Active",
                    styling::format_success_msg(styling::CHECK_INDICATOR, "")
                        .trim_start()
                        .bold()
                );
                println!(
                    "Tunnel established for subnet: {}",
                    styling::format_agent_name(&subnet)
                );
                println!("Interface: {}", styling::format_agent_name(&tun_name));
                #[cfg(target_os = "linux")]
                println!(
                    "{}",
                    styling::format_hint(
                        "Linux Ariadne currently proxies TCP flows. Use connect-style tooling; ICMP/UDP are not redirected yet."
                    )
                );
                println!();
            } else {
                drop(agents);
                let _ =
                    Self::send_agent_message(&agent_sender, Message::StopTunnel, "rollback tunnel")
                        .await;
                #[cfg(target_os = "linux")]
                let _ = Self::cleanup_tunnel(server, &agent_id, &tun_name, &subnet).await;
                #[cfg(target_os = "windows")]
                let _ = Self::cleanup_tunnel_windows(&tun_name, &subnet).await;
                return Err(LabyrinthError::Message(
                    "Selected agent disconnected while starting tunnel".to_string(),
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

    fn run_ariadne_preflight() -> Result<()> {
        println!(
            "{}",
            styling::format_section_title("Ariadne Preflight", "host capability checks")
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
                    PrivilegeManager::create_sudo_error("Ariadne mode"),
                ));
            }

            for bin in ["ip", "iptables"] {
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
                            "Install the missing networking utility before enabling Ariadne."
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
                    "Ariadne mode on Windows requires running as Administrator".to_string(),
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
                    "wintun.dll is required for Windows Ariadne mode. Place it next to labyrinth.exe or in PATH."
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
                    "PowerShell is required for Windows Ariadne route setup".to_string(),
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
        let Some(agent_id) = current_id else {
            println!(
                "{}",
                styling::format_warning_msg(
                    styling::WARNING_INDICATOR,
                    "No agent selected. Use 'select' command first."
                )
            );
            return Ok(());
        };
        let _network_guard = ARIADNE_NETWORK_LOCK.lock().await;

        let (tunnel_active, sender, tun_name, subnet) = {
            let agents = server.agents().read().await;
            let Some(agent) = agents.get(&agent_id) else {
                return Err(LabyrinthError::Message(
                    "Selected agent not found".to_string(),
                ));
            };
            (
                agent.tunnel_active,
                agent.sender.clone(),
                agent.tun_name.clone(),
                agent.tunnel_subnet.clone(),
            )
        };

        let has_portal = server.has_portal_forwarding(&agent_id).await;
        let has_ariadne = server
            .ariadne_snapshots()
            .await
            .iter()
            .any(|snapshot| snapshot.agent_id == agent_id);
        if !tunnel_active && !has_portal && !has_ariadne {
            println!(
                "{}",
                styling::format_warning_msg(
                    styling::WARNING_INDICATOR,
                    "No active tunnel or port forwarding for this agent"
                )
            );
            return Ok(());
        }

        let mut failures = Vec::new();
        let stopped_ports = if has_portal {
            server.stop_portal_forwarding_for_agent(&agent_id).await
        } else {
            Vec::new()
        };

        if !has_portal {
            if let Err(error) =
                Self::send_agent_message(&sender, Message::StopTunnel, "stop tunnel").await
            {
                failures.push(error.to_string());
            }
        }

        let connection_ids = server.connection_ids_for_agent(&agent_id).await;
        if let Some(stream_manager) = server.get_stream_manager().await {
            for connection_id in &connection_ids {
                if let Err(error) = stream_manager.terminate_stream(*connection_id).await {
                    failures.push(format!("stream {} cleanup: {}", connection_id, error));
                }
            }
        }
        if let Some(connection_manager) = server.get_connection_manager().await {
            for connection_id in &connection_ids {
                if let Err(error) = connection_manager.cleanup_connection(connection_id).await {
                    failures.push(format!("connection {} cleanup: {}", connection_id, error));
                }
            }
        }
        for connection_id in connection_ids {
            let _ = server.unregister_connection_owner(&connection_id).await;
        }

        if has_ariadne || !has_portal {
            if let (Some(tun_name), Some(subnet)) = (tun_name.as_deref(), subnet.as_deref()) {
                #[cfg(target_os = "linux")]
                let cleanup_result =
                    Self::cleanup_tunnel(server, &agent_id, tun_name, subnet).await;

                #[cfg(target_os = "windows")]
                let cleanup_result = Self::cleanup_tunnel_windows(tun_name, subnet).await;

                #[cfg(not(any(target_os = "linux", target_os = "windows")))]
                let cleanup_result: Result<()> = Err(LabyrinthError::Message(
                    "Ariadne is supported on Linux and Windows only".to_string(),
                ));

                if let Err(error) = cleanup_result {
                    failures.push(error.to_string());
                }
            } else if has_ariadne {
                let _ = server.stop_ariadne_listener(&agent_id).await;
            }
        }

        if !stopped_ports.is_empty() {
            println!(
                "{}",
                styling::format_success_msg(
                    styling::SUCCESS_INDICATOR,
                    &format!(
                        "Port forwarding stopped on ports: {}",
                        stopped_ports
                            .iter()
                            .map(|port| port.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
            );
        }

        if let Some(agent) = server.agents().write().await.get_mut(&agent_id) {
            agent.tunnel_active = false;
            agent.tunnel_subnet = None;
            agent.tun_name = None;
        }

        if failures.is_empty() {
            println!(
                "{}",
                styling::format_success_msg(styling::SUCCESS_INDICATOR, "Tunnel stopped")
            );
            Ok(())
        } else {
            Err(LabyrinthError::Message(format!(
                "Tunnel stop completed with errors: {}",
                failures.join("; ")
            )))
        }
    }

    /// Cleanup tunnel, Portal, and stream resources when agent disconnects.
    ///
    /// Agent control channel is already unavailable in this path, so cleanup
    /// never sends a protocol message and remains safe after agent removal.
    pub async fn cleanup_agent_resources(server: &LabyrinthServer, agent_id: &str) -> Result<()> {
        let _network_guard = ARIADNE_NETWORK_LOCK.lock().await;

        let (tun_name, subnet) = {
            let agents = server.agents().read().await;
            agents
                .get(agent_id)
                .map(|agent| (agent.tun_name.clone(), agent.tunnel_subnet.clone()))
                .unwrap_or((None, None))
        };

        let has_ariadne = server
            .ariadne_snapshots()
            .await
            .iter()
            .any(|snapshot| snapshot.agent_id == agent_id);
        let mut failures = Vec::new();
        let _stopped_ports = server.stop_portal_forwarding_for_agent(agent_id).await;

        let connection_ids = server.connection_ids_for_agent(agent_id).await;
        if let Some(stream_manager) = server.get_stream_manager().await {
            for connection_id in &connection_ids {
                if let Err(error) = stream_manager.terminate_stream(*connection_id).await {
                    failures.push(format!("stream {} cleanup: {}", connection_id, error));
                }
            }
        }
        if let Some(connection_manager) = server.get_connection_manager().await {
            for connection_id in &connection_ids {
                if let Err(error) = connection_manager.cleanup_connection(connection_id).await {
                    failures.push(format!("connection {} cleanup: {}", connection_id, error));
                }
            }
        }
        for connection_id in connection_ids {
            let _ = server.unregister_connection_owner(&connection_id).await;
        }

        if has_ariadne {
            if let (Some(tun_name), Some(subnet)) = (tun_name.as_deref(), subnet.as_deref()) {
                #[cfg(target_os = "linux")]
                let cleanup_result = Self::cleanup_tunnel(server, agent_id, tun_name, subnet).await;

                #[cfg(target_os = "windows")]
                let cleanup_result = Self::cleanup_tunnel_windows(tun_name, subnet).await;

                #[cfg(not(any(target_os = "linux", target_os = "windows")))]
                let cleanup_result: Result<()> = Err(LabyrinthError::Message(
                    "Ariadne is supported on Linux and Windows only".to_string(),
                ));

                if let Err(error) = cleanup_result {
                    failures.push(error.to_string());
                }
            } else {
                // Preserve listener cancellation even when state was partially
                // written or agent record was removed before this call.
                let _ = server.stop_ariadne_listener(agent_id).await;
            }
        }

        if let Some(agent) = server.agents().write().await.get_mut(agent_id) {
            agent.tunnel_active = false;
            agent.tunnel_subnet = None;
            agent.tun_name = None;
        }
        if server.current_agent().read().await.as_deref() == Some(agent_id) {
            *server.current_agent().write().await = None;
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(LabyrinthError::Message(format!(
                "Disconnected agent resource cleanup failed: {}",
                failures.join("; ")
            )))
        }
    }

    #[cfg(target_os = "linux")]
    async fn setup_tunnel(
        server: &LabyrinthServer,
        agent_id: &str,
        agent_sender: &tokio::sync::mpsc::Sender<Message>,
        tun_name: &str,
        subnet: &str,
    ) -> Result<()> {
        // Check for sudo privileges before attempting tunnel operations
        if !PrivilegeManager::has_sudo_privileges() {
            return Err(LabyrinthError::Message(
                PrivilegeManager::create_sudo_error("Ariadne mode"),
            ));
        }

        info!(
            "[+] Setting up tunnel interface {} for subnet {}",
            tun_name, subnet
        );

        let tun_ip = "10.0.0.1";
        // Keep listener bound while installing the redirect rule. This removes
        // the ephemeral-port release/rebind race and avoids exposing proxy to LAN.
        let proxy_listener = TcpListener::bind((ARIADNE_PROXY_BIND_ADDR, 0))
            .await
            .map_err(LabyrinthError::Io)?;
        let proxy_port = proxy_listener
            .local_addr()
            .map_err(LabyrinthError::Io)?
            .port();
        let mut tunnel_created = false;
        let mut redirect_added = false;

        let setup_result = (|| -> Result<()> {
            // Never delete an existing interface by name: it may belong to another
            // process. Operator must choose another name or clean stale state.
            Self::create_linux_tun_device(tun_name)?;
            tunnel_created = true;

            Self::run_command(
                "ip",
                &[
                    "addr",
                    "replace",
                    &format!("{}/32", tun_ip),
                    "dev",
                    tun_name,
                ],
            )?;
            Self::run_command("ip", &["link", "set", tun_name, "up"])?;
            // Ariadne is a userland TCP proxy. Enabling global IP forwarding is
            // unnecessary and would leave a host-wide setting changed on stop.
            Self::run_command("ip", &["route", "replace", "local", subnet, "dev", "lo"])?;
            redirect_added = Self::ensure_iptables_rule(
                "iptables",
                &[
                    "-t",
                    "nat",
                    "OUTPUT",
                    "-p",
                    "tcp",
                    "-d",
                    subnet,
                    "-j",
                    "REDIRECT",
                    "--to-ports",
                    &proxy_port.to_string(),
                ],
            )?;
            Ok(())
        })();

        if let Err(error) = setup_result {
            if tunnel_created {
                let _ =
                    Self::cleanup_linux_network(tun_name, subnet, proxy_port, redirect_added, true);
            }
            return Err(error);
        }

        let proxy_task = Self::spawn_linux_ariadne_proxy(
            proxy_listener,
            server,
            agent_id.to_string(),
            agent_sender.clone(),
            proxy_port,
        )
        .await?;
        server
            .register_ariadne_listener(agent_id.to_string(), proxy_port, proxy_task)
            .await;

        println!(
            "{}",
            styling::format_hint(&format!(
                "Transparent TCP pivot active on local redirect port {}.",
                proxy_port
            ))
        );

        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn create_linux_tun_device(tun_name: &str) -> Result<()> {
        Self::run_command("ip", &["tuntap", "add", "dev", tun_name, "mode", "tun"])
    }

    #[cfg(target_os = "linux")]
    fn ensure_iptables_rule(cmd: &str, rule_args: &[&str]) -> Result<bool> {
        let mut check_args = Vec::with_capacity(rule_args.len() + 1);
        if rule_args.starts_with(&["-t", "nat"]) {
            check_args.extend(["-t", "nat", "-C"]);
            check_args.extend(rule_args.iter().skip(2).copied());
        } else {
            check_args.push("-C");
            check_args.extend(rule_args.iter().copied());
        }

        if Self::command_succeeds(cmd, &check_args)? {
            return Ok(false);
        }

        let mut add_args = Vec::with_capacity(rule_args.len() + 1);
        if rule_args.starts_with(&["-t", "nat"]) {
            add_args.extend(["-t", "nat", "-A"]);
            add_args.extend(rule_args.iter().skip(2).copied());
        } else {
            add_args.push("-A");
            add_args.extend(rule_args.iter().copied());
        }

        Self::run_command(cmd, &add_args)?;
        Ok(true)
    }

    #[cfg(target_os = "linux")]
    async fn spawn_linux_ariadne_proxy(
        listener: TcpListener,
        server: &LabyrinthServer,
        agent_id: String,
        agent_sender: tokio::sync::mpsc::Sender<Message>,
        proxy_port: u16,
    ) -> Result<tokio::task::JoinHandle<()>> {
        let server = Arc::new(server.clone_for_tasks());

        Ok(tokio::spawn(async move {
            let mut bridges = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (client_socket, client_addr) = match accepted {
                            Ok(accepted) => accepted,
                            Err(e) => {
                                warn!("Ariadne proxy accept error on {}: {}", proxy_port, e);
                                break;
                            }
                        };

                        let target_addr = match Self::original_destination(&client_socket) {
                            Ok(target_addr)
                                if target_addr.port() != 0
                                    && !target_addr.ip().is_unspecified()
                                    && target_addr.port() != proxy_port =>
                            {
                                target_addr
                            }
                            Ok(target_addr) => {
                                warn!(
                                    "Rejecting invalid Ariadne original destination {} from {}",
                                    target_addr, client_addr
                                );
                                continue;
                            }
                            Err(e) => {
                                warn!(
                                    "Failed to resolve original destination for {}: {}",
                                    client_addr, e
                                );
                                continue;
                            }
                        };

                        let server = Arc::clone(&server);
                        let agent_sender = agent_sender.clone();
                        let agent_id = agent_id.clone();
                        bridges.spawn(async move {
                            if let Err(e) = Self::bridge_ariadne_connection(
                                server,
                                agent_id,
                                agent_sender,
                                client_socket,
                                client_addr,
                                target_addr,
                                proxy_port,
                            )
                            .await
                            {
                                warn!("Ariadne proxy bridge failed: {}", e);
                            }
                        });
                    }
                    completed = bridges.join_next(), if !bridges.is_empty() => {
                        if let Some(Err(e)) = completed {
                            warn!("Ariadne proxy bridge task failed: {}", e);
                        }
                    }
                }
            }
            // Stop closes listener; abort any bridge still holding client sockets.
            bridges.abort_all();
        }))
    }

    #[cfg(target_os = "linux")]
    fn original_destination(stream: &tokio::net::TcpStream) -> Result<SocketAddr> {
        let fd = stream.as_raw_fd();
        let mut addr: libc::sockaddr_in = unsafe { mem::zeroed() };
        let mut len = mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_IP,
                SO_ORIGINAL_DST,
                &mut addr as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        if rc != 0 {
            return Err(LabyrinthError::Message(format!(
                "getsockopt(SO_ORIGINAL_DST) failed: {}",
                std::io::Error::last_os_error()
            )));
        }

        let ip = Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr));
        let port = u16::from_be(addr.sin_port);
        Ok(SocketAddr::V4(SocketAddrV4::new(ip, port)))
    }

    #[cfg(target_os = "linux")]
    async fn bridge_ariadne_connection(
        server: Arc<LabyrinthServer>,
        agent_id: String,
        agent_sender: tokio::sync::mpsc::Sender<Message>,
        client_socket: tokio::net::TcpStream,
        client_addr: SocketAddr,
        target_addr: SocketAddr,
        proxy_port: u16,
    ) -> Result<()> {
        let stream_manager = server.get_stream_manager().await.ok_or_else(|| {
            LabyrinthError::Message("Streaming manager not initialized".to_string())
        })?;
        let connection_manager = server.get_connection_manager().await.ok_or_else(|| {
            LabyrinthError::Message("Connection manager not initialized".to_string())
        })?;

        let mapping = PortMapping {
            local_port: proxy_port,
            target_host: target_addr.ip().to_string(),
            target_port: target_addr.port(),
        };

        let connection_id = ConnectionId::new_v4();
        connection_manager
            .track_existing_connection(connection_id, client_addr, mapping.clone())
            .await
            .map_err(|e| {
                LabyrinthError::Message(format!("Failed to track Ariadne connection: {}", e))
            })?;
        server
            .register_connection_owner(connection_id, agent_id.clone())
            .await;

        let use_quic_stream = {
            let agents = server.agents().read().await;
            agents
                .get(&agent_id)
                .and_then(|agent| agent.quic_connection.as_ref())
                .is_some()
        };

        if use_quic_stream {
            if let Err(e) = QuicStreamBridge::create_bidirectional_stream(
                Arc::clone(&server),
                agent_id,
                connection_id,
                client_socket,
                mapping,
            )
            .await
            {
                let _ = connection_manager.cleanup_connection(&connection_id).await;
                let _ = server.unregister_connection_owner(&connection_id).await;
                return Err(LabyrinthError::Message(format!(
                    "Failed to create QUIC Ariadne stream: {}",
                    e
                )));
            }
            return Ok(());
        }

        if let Err(e) = stream_manager
            .create_bidirectional_stream(connection_id, client_socket)
            .await
        {
            let _ = connection_manager.cleanup_connection(&connection_id).await;
            let _ = server.unregister_connection_owner(&connection_id).await;
            return Err(LabyrinthError::Message(format!(
                "Failed to create Ariadne stream: {}",
                e
            )));
        }

        if let Err(e) = Self::send_agent_message(
            &agent_sender,
            Message::Stream(StreamMessage::Setup {
                connection_id,
                mapping,
            }),
            "send Ariadne stream setup",
        )
        .await
        {
            let _ = stream_manager.terminate_stream(connection_id).await;
            let _ = connection_manager.cleanup_connection(&connection_id).await;
            let _ = server.unregister_connection_owner(&connection_id).await;
            return Err(LabyrinthError::Message(format!(
                "Failed to send Ariadne setup to agent: {}",
                e
            )));
        }

        Ok(())
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
            if (-not $a) {{ throw \"Wintun adapter '$name' not found\" }}; \
            $idx=$a.ifIndex; \
            if (-not (Get-NetIPAddress -InterfaceIndex $idx -IPAddress 10.0.0.1 -ErrorAction SilentlyContinue)) {{ \
                New-NetIPAddress -InterfaceIndex $idx -IPAddress 10.0.0.1 -PrefixLength 24 -AddressFamily IPv4 -ErrorAction Stop | Out-Null \
            }}; \
            if (-not (Get-NetRoute -DestinationPrefix '{}' -InterfaceIndex $idx -ErrorAction SilentlyContinue)) {{ \
                New-NetRoute -DestinationPrefix '{}' -InterfaceIndex $idx -NextHop 0.0.0.0 -ErrorAction Stop | Out-Null \
            }};",
            tun_name, subnet, subnet
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
    async fn cleanup_tunnel(
        server: &LabyrinthServer,
        agent_id: &str,
        tun_name: &str,
        subnet: &str,
    ) -> Result<()> {
        info!(
            "[+] Cleaning up tunnel interface {} for subnet {}",
            tun_name, subnet
        );

        let proxy_port = server.stop_ariadne_listener(agent_id).await;
        Self::cleanup_linux_network(
            tun_name,
            subnet,
            proxy_port.unwrap_or_default(),
            proxy_port.is_some(),
            true,
        )
    }

    #[cfg(target_os = "linux")]
    fn cleanup_linux_network(
        tun_name: &str,
        subnet: &str,
        proxy_port: u16,
        remove_redirect: bool,
        remove_tun: bool,
    ) -> Result<()> {
        let mut failures = Vec::new();
        let mut run = |cmd: &str, args: &[&str]| {
            if let Err(error) = Self::run_command_idempotent(cmd, args) {
                failures.push(error.to_string());
            }
        };

        if remove_redirect {
            run(
                "iptables",
                &[
                    "-t",
                    "nat",
                    "-D",
                    "OUTPUT",
                    "-p",
                    "tcp",
                    "-d",
                    subnet,
                    "-j",
                    "REDIRECT",
                    "--to-ports",
                    &proxy_port.to_string(),
                ],
            );
        }
        run("ip", &["route", "del", "local", subnet, "dev", "lo"]);
        run("ip", &["addr", "del", "10.0.0.1/32", "dev", tun_name]);
        if remove_tun {
            run("ip", &["link", "del", tun_name]);
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(LabyrinthError::Message(format!(
                "Ariadne cleanup failed: {}",
                failures.join("; ")
            )))
        }
    }

    #[cfg(target_os = "windows")]
    async fn cleanup_tunnel_windows(tun_name: &str, subnet: &str) -> Result<()> {
        let ps = format!(
            "$name='{}'; $a=Get-NetAdapter -Name $name -ErrorAction SilentlyContinue; \
            if ($a) {{ \
                Remove-NetRoute -DestinationPrefix '{}' -InterfaceIndex $a.ifIndex -Confirm:$false -ErrorAction SilentlyContinue; \
                Remove-NetIPAddress -InterfaceIndex $a.ifIndex -IPAddress 10.0.0.1 -Confirm:$false -ErrorAction SilentlyContinue \
            }}",
            tun_name, subnet
        );
        let output = Command::new("powershell")
            .args(["-NoProfile", "-Command", &ps])
            .output()?;
        if output.status.success() {
            Ok(())
        } else {
            Err(LabyrinthError::Message(format!(
                "Failed to clean up Wintun routes: {}",
                String::from_utf8_lossy(&output.stderr)
            )))
        }
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

    #[cfg(target_os = "linux")]
    fn run_command_idempotent(cmd: &str, args: &[&str]) -> Result<()> {
        match Self::run_command(cmd, args) {
            Ok(()) => Ok(()),
            Err(error) if Self::is_absent_network_resource(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }

    #[cfg(target_os = "linux")]
    fn is_absent_network_resource(error: &LabyrinthError) -> bool {
        let message = error.to_string().to_ascii_lowercase();
        [
            "cannot find device",
            "does a matching rule exist",
            "no chain/target/match",
            "no such process",
            "not found",
        ]
        .iter()
        .any(|marker| message.contains(marker))
    }

    async fn send_agent_message(
        sender: &tokio::sync::mpsc::Sender<Message>,
        message: Message,
        operation: &str,
    ) -> Result<()> {
        timeout(AGENT_CONTROL_TIMEOUT, sender.send(message))
            .await
            .map_err(|_| {
                LabyrinthError::Message(format!(
                    "Timed out after {} seconds while attempting to {}",
                    AGENT_CONTROL_TIMEOUT.as_secs(),
                    operation
                ))
            })?
            .map_err(|e| LabyrinthError::Message(format!("Failed to {}: {}", operation, e)))
    }

    fn validate_tunnel_name(tun_name: &str) -> Result<()> {
        // Linux IFNAMSIZ is 16 bytes including NUL. Restricting names to this
        // portable subset also keeps Windows PowerShell interpolation safe.
        let valid = !tun_name.is_empty()
            && tun_name.len() <= 15
            && tun_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
        if valid {
            Ok(())
        } else {
            Err(LabyrinthError::Message(
                "Invalid tunnel interface name: use 1-15 ASCII letters, digits, '.', '_' or '-'"
                    .to_string(),
            ))
        }
    }

    fn normalize_ariadne_subnet(input: &str) -> Result<String> {
        let (ip, prefix) = input.split_once('/').ok_or_else(|| {
            LabyrinthError::Message(format!("Invalid IPv4 subnet format: {}", input))
        })?;
        let ip = ip.parse::<std::net::Ipv4Addr>().map_err(|_| {
            LabyrinthError::Message(format!(
                "Ariadne currently supports IPv4 subnets only: {}",
                input
            ))
        })?;
        let prefix = prefix.parse::<u8>().map_err(|_| {
            LabyrinthError::Message(format!("Invalid IPv4 prefix length: {}", input))
        })?;
        if prefix == 0 || prefix > 32 {
            return Err(LabyrinthError::Message(if prefix == 0 {
                "Ariadne refuses 0.0.0.0/0 because it would redirect server control traffic"
                    .to_string()
            } else {
                format!("Invalid IPv4 prefix length: {}", prefix)
            }));
        }
        let mask = u32::MAX << (32 - u32::from(prefix));
        let network = std::net::Ipv4Addr::from(u32::from(ip) & mask);
        Ok(format!("{}/{}", network, prefix))
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

    fn print_detected_routes(routes: &[DetectedRoute]) {
        println!(
            "{}",
            styling::format_section_title("Detected Agent Routes", "from client interfaces")
        );
        println!("{}", "─────────────────────".bright_black());

        if routes.is_empty() {
            println!(
                "{}",
                styling::format_warning_msg(
                    styling::WARNING_INDICATOR,
                    "No routable IPv4 CIDR was detected from the selected agent."
                )
            );
            println!(
                "{}",
                styling::format_hint("Enter the target subnet manually.")
            );
            println!();
            return;
        }

        for (index, route) in routes.iter().take(5).enumerate() {
            let marker = if index == 0 { "auto" } else { "candidate" };
            println!(
                "{} {} {} via {} ({})",
                styling::INDENT_LEVEL_1,
                marker.cyan(),
                styling::format_agent_name(&route.cidr),
                route.interface_name.bright_white(),
                route.source_address.bright_black()
            );
        }
        println!(
            "{}",
            styling::format_hint("Press Enter to use the auto route, or type a different CIDR.")
        );
        println!();
    }
}

#[cfg(test)]
mod tests {
    use super::TunnelManager;
    use crate::protocol::{AgentInfo, AgentKind};
    use crate::server::core::ConnectedAgent;
    use std::sync::Arc;
    use std::time::Instant;
    use tokio::sync::{mpsc, Mutex};

    fn active_agent(sender: mpsc::Sender<crate::protocol::Message>) -> ConnectedAgent {
        ConnectedAgent {
            id: "agent-1".to_string(),
            info: AgentInfo {
                name: "test-agent".to_string(),
                hostname: "test-host".to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
                interfaces: Vec::new(),
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
            tunnel_active: true,
            tunnel_subnet: Some("192.168.10.0/24".to_string()),
            tun_name: Some("labyrinth".to_string()),
            last_seen: Arc::new(Mutex::new(Instant::now())),
            command_response: Arc::new(Mutex::new(None)),
            shell_events: Arc::new(Mutex::new(None)),
        }
    }

    #[test]
    fn validate_cidr_accepts_ipv4_networks() {
        assert!(TunnelManager::validate_cidr("192.168.100.0/24"));
    }

    #[test]
    fn validate_cidr_rejects_invalid_prefix() {
        assert!(!TunnelManager::validate_cidr("192.168.100.0/99"));
    }

    #[test]
    fn tunnel_name_rejects_shell_metacharacters_and_long_names() {
        assert!(TunnelManager::validate_tunnel_name("labyrinth").is_ok());
        assert!(TunnelManager::validate_tunnel_name("lab;rm").is_err());
        assert!(TunnelManager::validate_tunnel_name("this-name-is-too-long").is_err());
    }

    #[test]
    fn ariadne_subnet_normalizes_host_bits_and_rejects_default_route() {
        assert_eq!(
            TunnelManager::normalize_ariadne_subnet("192.168.10.99/24").unwrap(),
            "192.168.10.0/24"
        );
        assert!(TunnelManager::normalize_ariadne_subnet("0.0.0.0/0").is_err());
        assert!(TunnelManager::normalize_ariadne_subnet("192.168.10.1/33").is_err());
        assert!(TunnelManager::normalize_ariadne_subnet("2001:db8::/64").is_err());
    }

    #[tokio::test]
    async fn agent_message_reports_closed_control_channel() {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        drop(receiver);

        let error = TunnelManager::send_agent_message(
            &sender,
            crate::protocol::Message::StopTunnel,
            "stop tunnel",
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("Failed to stop tunnel"));
    }

    #[tokio::test]
    async fn agent_message_delivers_control_message() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        TunnelManager::send_agent_message(
            &sender,
            crate::protocol::Message::StopTunnel,
            "stop tunnel",
        )
        .await
        .unwrap();
        assert!(matches!(
            receiver.recv().await,
            Some(crate::protocol::Message::StopTunnel)
        ));
    }

    #[tokio::test]
    async fn start_existing_tunnel_is_idempotent() {
        let server = crate::server::core::LabyrinthServer::new(false, None);
        let (sender, _receiver) = mpsc::channel(1);
        server
            .agents()
            .write()
            .await
            .insert("agent-1".to_string(), active_agent(sender));

        // Existing state returns before privileged preflight or resource mutation.
        TunnelManager::start_tunnel_for_agent(&server, "agent-1", "192.168.10.99/24", "labyrinth")
            .await
            .unwrap();

        let agent = server.agents().read().await;
        assert_eq!(
            agent
                .get("agent-1")
                .and_then(|entry| entry.tunnel_subnet.as_deref()),
            Some("192.168.10.0/24")
        );
    }

    #[tokio::test]
    async fn disconnected_agent_cleanup_is_idempotent_without_state() {
        let server = crate::server::core::LabyrinthServer::new(false, None);
        *server.current_agent().write().await = Some("gone-agent".to_string());

        TunnelManager::cleanup_agent_resources(&server, "gone-agent")
            .await
            .unwrap();
        TunnelManager::cleanup_agent_resources(&server, "gone-agent")
            .await
            .unwrap();
        assert!(server.ariadne_snapshots().await.is_empty());
        assert!(server.current_agent().read().await.is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cleanup_error_classifier_only_ignores_absent_resources() {
        assert!(TunnelManager::is_absent_network_resource(
            &crate::error::LabyrinthError::Message(
                "RTNETLINK answers: No such process".to_string()
            )
        ));
        assert!(!TunnelManager::is_absent_network_resource(
            &crate::error::LabyrinthError::Message(
                "Command failed: ip [\"route\"] -> permission denied".to_string()
            )
        ));
    }
}
