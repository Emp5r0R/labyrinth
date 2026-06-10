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
#[cfg(target_os = "linux")]
use tracing::warn;
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

            if let Err(e) = agent_sender.send(start_msg).await {
                #[cfg(target_os = "linux")]
                let _ = Self::cleanup_tunnel(server, &agent_id, &tun_name, &subnet).await;
                #[cfg(target_os = "windows")]
                let _ = Self::cleanup_tunnel_windows(&tun_name).await;
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
                WindowsNetstackBridge::start(&tun_name, agent_sender.clone()).map_err(|e| {
                    LabyrinthError::Message(format!("Failed to start Wintun bridge: {}", e))
                })?;
            }

            let mut agents = server.agents().write().await;
            if let Some(agent) = agents.get_mut(&agent_id) {
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
                #[cfg(target_os = "linux")]
                println!(
                    "{}",
                    styling::format_hint(
                        "Linux Fullhouse currently proxies TCP flows. Use connect-style tooling; ICMP/UDP are not redirected yet."
                    )
                );
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
                            server,
                            &agent_id,
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
                PrivilegeManager::create_sudo_error("Fullhouse mode"),
            ));
        }

        info!(
            "[+] Setting up tunnel interface {} for subnet {}",
            tun_name, subnet
        );

        let tun_ip = "10.0.0.1";
        let proxy_port = Self::pick_proxy_port().await?;

        match Self::create_linux_tun_device(tun_name) {
            Ok(()) => {}
            Err(err) if Self::should_recover_existing_tun(&err) => {
                warn!(
                    "Detected stale tunnel interface {}. Removing it before retrying setup.",
                    tun_name
                );
                let _ = Self::run_command("ip", &["link", "del", tun_name]);
                Self::create_linux_tun_device(tun_name)?;
            }
            Err(err) => return Err(err),
        }

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

        Self::run_command("sysctl", &["-w", "net.ipv4.ip_forward=1"])?;
        Self::run_command("ip", &["route", "replace", "local", subnet, "dev", "lo"])?;
        Self::ensure_iptables_rule(
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

        let proxy_task = Self::spawn_linux_fullhouse_proxy(
            server,
            agent_id.to_string(),
            agent_sender.clone(),
            proxy_port,
        )
        .await?;
        server
            .register_fullhouse_listener(agent_id.to_string(), proxy_port, proxy_task)
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
    fn should_recover_existing_tun(err: &LabyrinthError) -> bool {
        let message = err.to_string().to_ascii_lowercase();
        message.contains("exists")
            || message.contains("already in use")
            || message.contains("device or resource busy")
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

    #[cfg(target_os = "linux")]
    async fn pick_proxy_port() -> Result<u16> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(LabyrinthError::Io)?;
        let port = listener.local_addr().map_err(LabyrinthError::Io)?.port();
        drop(listener);
        Ok(port)
    }

    #[cfg(target_os = "linux")]
    async fn spawn_linux_fullhouse_proxy(
        server: &LabyrinthServer,
        agent_id: String,
        agent_sender: tokio::sync::mpsc::Sender<Message>,
        proxy_port: u16,
    ) -> Result<tokio::task::JoinHandle<()>> {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, proxy_port))
            .await
            .map_err(LabyrinthError::Io)?;
        let server = Arc::new(server.clone_for_tasks());

        Ok(tokio::spawn(async move {
            loop {
                let (client_socket, client_addr) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(e) => {
                        warn!("Fullhouse proxy accept error on {}: {}", proxy_port, e);
                        break;
                    }
                };

                let Ok(target_addr) = Self::original_destination(&client_socket) else {
                    warn!("Failed to resolve original destination for redirected connection");
                    continue;
                };

                let server = Arc::clone(&server);
                let agent_sender = agent_sender.clone();
                let agent_id = agent_id.clone();
                tokio::spawn(async move {
                    if let Err(e) = Self::bridge_fullhouse_connection(
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
                        warn!("Fullhouse proxy bridge failed: {}", e);
                    }
                });
            }
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
                80,
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
    async fn bridge_fullhouse_connection(
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
                LabyrinthError::Message(format!("Failed to track Fullhouse connection: {}", e))
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
                    "Failed to create QUIC Fullhouse stream: {}",
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
                "Failed to create Fullhouse stream: {}",
                e
            )));
        }

        if let Err(e) = agent_sender
            .send(Message::Stream(StreamMessage::Setup {
                connection_id,
                mapping,
            }))
            .await
        {
            let _ = stream_manager.terminate_stream(connection_id).await;
            let _ = connection_manager.cleanup_connection(&connection_id).await;
            let _ = server.unregister_connection_owner(&connection_id).await;
            return Err(LabyrinthError::Message(format!(
                "Failed to send Fullhouse setup to agent: {}",
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

        if let Some(proxy_port) = server.stop_fullhouse_listener(agent_id).await {
            let _ = Self::run_command(
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
        let _ = Self::run_command("ip", &["route", "del", "local", subnet, "dev", "lo"]);
        let _ = Self::run_command("ip", &["addr", "del", "10.0.0.1/32", "dev", tun_name]);
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
    #[cfg(target_os = "linux")]
    use crate::error::LabyrinthError;

    #[test]
    fn validate_cidr_accepts_ipv4_networks() {
        assert!(TunnelManager::validate_cidr("192.168.100.0/24"));
    }

    #[test]
    fn validate_cidr_rejects_invalid_prefix() {
        assert!(!TunnelManager::validate_cidr("192.168.100.0/99"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stale_tun_detection_matches_existing_device_errors() {
        let err = LabyrinthError::Message("device or resource busy: interface exists".to_string());
        assert!(TunnelManager::should_recover_existing_tun(&err));
    }
}
