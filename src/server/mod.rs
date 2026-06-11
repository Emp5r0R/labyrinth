pub mod agent_connection;
pub mod agent_manager;
pub mod certificate;
pub mod chain_manager;
pub mod core;
pub mod dashboard;
pub mod dweller_manager;
pub mod dweller_registry;
#[cfg(target_os = "windows")]
pub mod netstack_bridge_windows;
pub mod network_map;
pub mod privileges;
pub mod quic_stream_bridge;
pub mod reverse_port_forward;
pub mod topology;
pub mod tunnel_manager;
pub mod ui;

use crate::error::{LabyrinthError, Result};
use crate::protocol::Message;
use crate::server::agent_manager::AgentManager;
use crate::server::certificate::CertificateManager;
use crate::server::chain_manager::ChainManager;
use crate::server::core::LabyrinthServer;
use crate::server::dashboard::DashboardServer;
use crate::server::dweller_manager::DwellerManager;
use crate::server::privileges::PrivilegeManager;
use crate::server::quic_stream_bridge::QuicStreamBridge;

use crate::server::tunnel_manager::TunnelManager;
use crate::server::ui::ServerUI;
use crate::styling;
use crate::transport::{parse_socket_addr, QuicBidiStream, TransportMode};
use base64::{engine::general_purpose, Engine as _};
use colored::Colorize;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use dialoguer::{Input, Select};
use rustyline::completion::{Completer, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::Editor;
use rustyline::{Context as RustyContext, Helper};
use std::borrow::Cow;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::time::Duration;
use tokio_rustls::TlsAcceptor;
use tracing::{error, info, warn};

use crate::streaming::{
    connection_manager::ServerConnectionManager,
    models::{ConnectionId, PortMapping, StreamMessage},
    stream_manager::BidirectionalStreamManager,
    traits::{ConnectionManager as StreamConnectionManager, StreamManager as StreamManagerTrait},
    ErrorRecoveryCoordinator, MetricsCollector,
};

fn resolve_auth_key(no_auth: bool) -> Result<Option<String>> {
    if no_auth {
        return Ok(None);
    }

    match std::env::var("LABYRINTH_AUTH_KEY") {
        Ok(value) if !value.trim().is_empty() => Ok(Some(value)),
        _ => Err(LabyrinthError::Message(
            "LABYRINTH_AUTH_KEY must be set when authentication is enabled".to_string(),
        )),
    }
}

async fn spawn_agent_listener(
    server: Arc<LabyrinthServer>,
    listen_addr: &str,
    transport: TransportMode,
    certs: Vec<rustls::pki_types::CertificateDer<'static>>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
) -> Result<()> {
    match transport {
        TransportMode::Tcp => spawn_tcp_agent_listener(server, listen_addr, certs, key).await,
        TransportMode::Quic => spawn_quic_agent_listener(server, listen_addr, certs, key).await,
    }
}

async fn spawn_tcp_agent_listener(
    server: Arc<LabyrinthServer>,
    listen_addr: &str,
    certs: Vec<rustls::pki_types::CertificateDer<'static>>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
) -> Result<()> {
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind(listen_addr).await?;
    info!("TCP/TLS agent listener on {}", listen_addr);

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    let acceptor = acceptor.clone();
                    let server = Arc::clone(&server);

                    tokio::spawn(async move {
                        match acceptor.accept(stream).await {
                            Ok(tls_stream) => {
                                if let Err(e) =
                                    AgentManager::register_agent(server, tls_stream, addr).await
                                {
                                    error!("Agent registration failed: {}", e);
                                }
                            }
                            Err(e) => {
                                error!("TLS handshake failed: {}", e);
                            }
                        }
                    });
                }
                Err(e) => {
                    error!("Failed to accept TCP agent connection: {}", e);
                }
            }
        }
    });

    Ok(())
}

async fn spawn_quic_agent_listener(
    server: Arc<LabyrinthServer>,
    listen_addr: &str,
    certs: Vec<rustls::pki_types::CertificateDer<'static>>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
) -> Result<()> {
    let mut crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    crypto.alpn_protocols = vec![b"labyrinth-control/1".to_vec()];
    let quic_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(crypto)
        .map_err(|e| LabyrinthError::Message(format!("Invalid QUIC server config: {}", e)))?;
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_crypto));
    let listen_addr = parse_socket_addr(listen_addr)?;
    let endpoint = quinn::Endpoint::server(server_config, listen_addr)?;
    info!("QUIC agent listener on {}", listen_addr);

    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let server = Arc::clone(&server);
            tokio::spawn(async move {
                match incoming.await {
                    Ok(connection) => {
                        let remote_addr = connection.remote_address();
                        match connection.accept_bi().await {
                            Ok((send, recv)) => {
                                let stream_connection = connection.clone();
                                let stream =
                                    QuicBidiStream::with_lifetime(send, recv, None, connection);
                                if let Err(e) = AgentManager::register_quic_agent(
                                    server,
                                    stream,
                                    remote_addr,
                                    stream_connection,
                                )
                                .await
                                {
                                    error!("QUIC agent registration failed: {}", e);
                                }
                            }
                            Err(e) => {
                                error!("QUIC control stream accept failed: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        error!("QUIC handshake failed: {}", e);
                    }
                }
            });
        }
    });

    Ok(())
}

fn stream_message_connection_id(msg: &StreamMessage) -> Option<ConnectionId> {
    match msg {
        StreamMessage::Setup { connection_id, .. }
        | StreamMessage::Data { connection_id, .. }
        | StreamMessage::Close { connection_id, .. }
        | StreamMessage::SetupAck { connection_id, .. }
        | StreamMessage::Heartbeat { connection_id, .. } => Some(*connection_id),
        _ => None,
    }
}

async fn run_cli(server: Arc<LabyrinthServer>) -> Result<()> {
    let mut rl =
        Editor::<CommandHelper, rustyline::history::DefaultHistory>::new().map_err(|e| {
            crate::error::LabyrinthError::Message(format!("Failed to create readline: {}", e))
        })?;
    rl.set_helper(Some(CommandHelper::new(vec![
        "help",
        "agents",
        "dwellers",
        "list",
        "ls",
        "select",
        "connect-dweller",
        "drop-dweller",
        "configure-dweller",
        "task-dweller",
        "dweller-tasks",
        "forget-dweller",
        "info",
        "show",
        "topology",
        "routes",
        "plan",
        "access",
        "chain",
        "map",
        "network-map",
        "tunnel",
        "ariadne",
        "stop",
        "forward",
        "portal",
        "commands",
        "cmd",
        "upload",
        "download",
        "status",
        "cert",
        "certificate",
        "done",
        "exit",
        "quit",
        "q",
    ])));

    println!("\n{}", styling::format_welcome_header());
    println!("{}", styling::format_welcome_subtitle());
    println!();

    loop {
        let current_agent = server.current_agent().read().await.clone();
        let agent_name = if let Some(ref agent_id) = current_agent {
            let agents = server.agents().read().await;
            agents.get(agent_id).map(|a| a.info.name.clone())
        } else {
            None
        };

        let prompt = styling::format_prompt(agent_name.as_deref());

        match rl.readline(&prompt) {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                rl.add_history_entry(line).map_err(|e| {
                    crate::error::LabyrinthError::Message(format!("History error: {}", e))
                })?;

                match line {
                    "help" | "h" => {
                        println!("\n{}", styling::format_header("Available Commands"));
                        println!("{}", styling::format_separator(styling::SECTION_SEPARATOR));
                        println!("  {}  List connected agents", "agents".cyan());
                        println!("  {}  List remembered dwellers", "dwellers".cyan());
                        println!("  {}  Select an agent for operations", "select".cyan());
                        println!(
                            "  {}  Connect to a remembered dweller",
                            "connect-dweller".cyan()
                        );
                        println!(
                            "  {}  Drop and persist a dweller via the selected agent",
                            "drop-dweller".cyan()
                        );
                        println!(
                            "  {}  Configure a remembered dweller callback server",
                            "configure-dweller".cyan()
                        );
                        println!(
                            "  {}  Queue a task for a hibernating dweller",
                            "task-dweller".cyan()
                        );
                        println!(
                            "  {}  Show queued dweller tasks and results",
                            "dweller-tasks".cyan()
                        );
                        println!("  {}  Forget a remembered dweller", "forget-dweller".cyan());
                        println!("  {}  Show detailed agent information", "info".cyan());
                        println!(
                            "  {}  Show route topology and shared networks",
                            "topology".cyan()
                        );
                        println!(
                            "  {}  Preview smart route plan for a target",
                            "plan <ip|cidr>".cyan()
                        );
                        println!(
                            "  {}  Plan and apply smart access to a target",
                            "access <ip|cidr>".cyan()
                        );
                        println!(
                            "  {}  Show smart chain state or diagnose reachability",
                            "chain status|doctor [target]".cyan()
                        );
                        println!("  {}  Show visual network map", "map".cyan());
                        println!("  {}  Start Tunnel", "Ariadne".cyan());
                        println!("  {}  Port Forwarding", "Portal".cyan());
                        println!("  {}  Stop active tunnel/forwarding", "stop".cyan());
                        println!("  {}  Execute system commands on agent", "commands".cyan());
                        println!("  {}  Upload file to selected agent", "upload".cyan());
                        println!("  {}  Download file from selected agent", "download".cyan());
                        println!("  {}  Show server status", "status".cyan());
                        println!("  {}  Show certificate information", "cert".cyan());
                        println!("  {}  Show this help message", "help".cyan());
                        println!("  {}  Exit the server", "exit".cyan());
                        println!();
                    }
                    "agents" | "list" | "ls" => {
                        ServerUI::list_agents(&server).await;
                    }
                    "dwellers" => {
                        DwellerManager::list_dwellers(&server).await;
                    }
                    "select" => {
                        if let Err(e) = ServerUI::select_agent(&server).await {
                            println!(
                                "{}",
                                styling::format_error_msg(
                                    styling::ERROR_INDICATOR,
                                    &format!("Selection failed: {}", e)
                                )
                            );
                        }
                    }
                    "connect-dweller" => {
                        if let Err(e) = DwellerManager::connect_dweller(server.clone()).await {
                            println!(
                                "{}",
                                styling::format_error_msg(
                                    styling::ERROR_INDICATOR,
                                    &format!("Dweller connection failed: {}", e)
                                )
                            );
                        }
                    }
                    "drop-dweller" => {
                        if let Err(e) = DwellerManager::drop_dweller(server.clone()).await {
                            println!(
                                "{}",
                                styling::format_error_msg(
                                    styling::ERROR_INDICATOR,
                                    &format!("Drop Dweller failed: {}", e)
                                )
                            );
                        }
                    }
                    "configure-dweller" => {
                        if let Err(e) = DwellerManager::configure_dweller(server.clone()).await {
                            println!(
                                "{}",
                                styling::format_error_msg(
                                    styling::ERROR_INDICATOR,
                                    &format!("Configure Dweller failed: {}", e)
                                )
                            );
                        }
                    }
                    "task-dweller" => {
                        if let Err(e) = DwellerManager::enqueue_dweller_task(server.clone()).await {
                            println!(
                                "{}",
                                styling::format_error_msg(
                                    styling::ERROR_INDICATOR,
                                    &format!("Queue Dweller task failed: {}", e)
                                )
                            );
                        }
                    }
                    "dweller-tasks" => {
                        if let Err(e) = DwellerManager::list_dweller_tasks(&server).await {
                            println!(
                                "{}",
                                styling::format_error_msg(
                                    styling::ERROR_INDICATOR,
                                    &format!("List Dweller tasks failed: {}", e)
                                )
                            );
                        }
                    }
                    "forget-dweller" => {
                        if let Err(e) = DwellerManager::forget_dweller(&server).await {
                            println!(
                                "{}",
                                styling::format_error_msg(
                                    styling::ERROR_INDICATOR,
                                    &format!("Forget Dweller failed: {}", e)
                                )
                            );
                        }
                    }
                    "info" | "show" => {
                        if let Err(e) = ServerUI::show_agent_info(&server).await {
                            println!(
                                "{}",
                                styling::format_error_msg(
                                    styling::ERROR_INDICATOR,
                                    &format!("Info display failed: {}", e)
                                )
                            );
                        }
                    }
                    "topology" | "routes" => {
                        ServerUI::show_topology(&server).await;
                    }
                    command if command.starts_with("plan ") => {
                        let target = command.trim_start_matches("plan").trim();
                        if let Err(e) = ChainManager::show_plan(&server, target).await {
                            println!(
                                "{}",
                                styling::format_error_msg(
                                    styling::ERROR_INDICATOR,
                                    &format!("Plan failed: {}", e)
                                )
                            );
                        }
                    }
                    command if command.starts_with("access ") => {
                        let target = command.trim_start_matches("access").trim();
                        if let Err(e) = ChainManager::access(server.clone(), target).await {
                            println!(
                                "{}",
                                styling::format_error_msg(
                                    styling::ERROR_INDICATOR,
                                    &format!("Access failed: {}", e)
                                )
                            );
                        }
                    }
                    "chain" | "chain status" => {
                        ChainManager::show_status(&server).await;
                    }
                    command if command.starts_with("chain doctor") => {
                        let target = command.trim_start_matches("chain doctor").trim();
                        let target = (!target.is_empty()).then_some(target);
                        if let Err(e) = ChainManager::doctor(&server, target).await {
                            println!(
                                "{}",
                                styling::format_error_msg(
                                    styling::ERROR_INDICATOR,
                                    &format!("Chain doctor failed: {}", e)
                                )
                            );
                        }
                    }
                    "map" | "network-map" => {
                        ServerUI::show_network_map(&server).await;
                    }
                    "tunnel" | "ariadne" | "fullhouse" | "Ariadne" | "Fullhouse" => {
                        if let Err(e) = TunnelManager::start_tunnel(&server).await {
                            println!(
                                "{}",
                                styling::format_error_msg(
                                    styling::ERROR_INDICATOR,
                                    &format!("Tunnel start failed: {}", e)
                                )
                            );
                        }
                    }
                    "stop" => {
                        if let Err(e) = TunnelManager::stop_tunnel(&server).await {
                            println!(
                                "{}",
                                styling::format_error_msg(
                                    styling::ERROR_INDICATOR,
                                    &format!("Stop failed: {}", e)
                                )
                            );
                        }
                    }
                    "forward" | "portal" | "room" | "Portal" | "Room" => {
                        if let Err(e) = start_port_forwarding(server.clone()).await {
                            println!(
                                "{}",
                                styling::format_error_msg(
                                    styling::ERROR_INDICATOR,
                                    &format!("Port forwarding failed: {}", e)
                                )
                            );
                        }
                    }
                    "commands" | "cmd" => {
                        if let Err(e) = start_commands_mode(&server).await {
                            println!(
                                "{}",
                                styling::format_error_msg(
                                    styling::ERROR_INDICATOR,
                                    &format!("Commands failed: {}", e)
                                )
                            );
                        }
                    }
                    "upload" => {
                        if let Err(e) = start_upload_mode(&server).await {
                            println!(
                                "{}",
                                styling::format_error_msg(
                                    styling::ERROR_INDICATOR,
                                    &format!("Upload failed: {}", e)
                                )
                            );
                        }
                    }
                    "download" => {
                        if let Err(e) = start_download_mode(&server).await {
                            println!(
                                "{}",
                                styling::format_error_msg(
                                    styling::ERROR_INDICATOR,
                                    &format!("Download failed: {}", e)
                                )
                            );
                        }
                    }
                    "status" => {
                        ServerUI::show_status(&server).await;
                    }
                    "cert" | "certificate" => {
                        if let Err(e) = CertificateManager::show_certificate_info() {
                            println!(
                                "{}",
                                styling::format_error_msg(
                                    styling::ERROR_INDICATOR,
                                    &format!("Certificate info failed: {}", e)
                                )
                            );
                        }
                    }
                    "done" => {
                        println!(
                            "{}",
                            styling::format_success_msg(
                                styling::SUCCESS_INDICATOR,
                                "Operation completed"
                            )
                        );
                    }
                    "exit" | "quit" | "q" => {
                        println!(
                            "{}",
                            styling::format_success_msg(styling::SUCCESS_INDICATOR, "Goodbye!")
                        );
                        break;
                    }
                    _ => {
                        println!(
                            "{}",
                            styling::format_warning_msg(
                                styling::WARNING_INDICATOR,
                                &format!(
                                    "Unknown command: '{}'. Type 'help' for available commands.",
                                    line
                                )
                            )
                        );
                    }
                }
            }
            Err(rustyline::error::ReadlineError::Interrupted) => {
                println!("^C");
                continue;
            }
            Err(rustyline::error::ReadlineError::Eof) => {
                println!("^D");
                break;
            }
            Err(err) => {
                println!("Error: {:?}", err);
                break;
            }
        }
    }

    Ok(())
}

async fn start_port_forwarding(server: Arc<LabyrinthServer>) -> Result<()> {
    let current_id = server.current_agent().read().await.clone();
    if let Some(agent_id) = current_id {
        println!("\n{}", "Portal Mode (Port Forwarding)".cyan().bold());
        println!("{}", "────────────────────────────".bright_black());
        println!();

        let mappings: Vec<String> = loop {
            let input: String = Input::new()
                .with_prompt(
                    "Port mappings (format: local_port:target_host:target_port, comma-separated)",
                )
                .interact_text()
                .map_err(|e| LabyrinthError::Message(format!("Input error: {}", e)))?;

            let mappings: Vec<String> = input.split(',').map(|s| s.trim().to_string()).collect();
            if !mappings.is_empty() && mappings.iter().all(|m| validate_port_mapping(m)) {
                for mapping in &mappings {
                    println!(
                        "{}{}",
                        styling::INDENT_LEVEL_1,
                        styling::format_check_item(&format!(
                            "Valid mapping: {}",
                            styling::format_agent_name(mapping)
                        ))
                    );
                }
                break mappings;
            }

            println!(
                "{}Format: local_port:target_host:target_port",
                styling::INDENT_LEVEL_1
            );
            println!("{}Examples:", styling::INDENT_LEVEL_1);
            println!(
                "{} {} Single mapping:    8080:192.168.1.100:80",
                styling::INDENT_LEVEL_2,
                styling::ARROW_INDICATOR.cyan()
            );
            println!(
                "{} {} Multiple mappings: 8080:192.168.1.100:80,9090:192.168.1.200:443",
                styling::INDENT_LEVEL_2,
                styling::ARROW_INDICATOR.cyan()
            );
            println!();
        };

        let agent_sender = {
            let agents = server.agents().read().await;
            if let Some(agent) = agents.get(&agent_id) {
                agent.sender.clone()
            } else {
                return Err(LabyrinthError::Message(
                    "Selected agent not found".to_string(),
                ));
            }
        };

        if server.get_stream_manager().await.is_none()
            || server.get_connection_manager().await.is_none()
        {
            initialize_streaming_managers(server.clone()).await?;
        }

        let mut successful_mappings = Vec::new();
        for mapping in &mappings {
            let parts: Vec<&str> = mapping.split(':').collect();
            let local_port: u16 = parts[0].parse().unwrap();
            let target_host = parts[1].to_string();
            let target_port: u16 = parts[2].parse().unwrap();

            let server_for_task = server.clone();
            let agent_sender_clone = agent_sender.clone();
            let agent_id_clone = agent_id.clone();
            let target_host_clone = target_host.clone();

            let handle = tokio::spawn(async move {
                if let Err(e) = run_streaming_port_forward_listener(
                    local_port,
                    target_host_clone,
                    target_port,
                    server_for_task.clone(),
                    agent_sender_clone,
                    agent_id_clone.clone(),
                )
                .await
                {
                    error!(
                        "Streaming port forward listener error on {}: {}",
                        local_port, e
                    );
                }
                server_for_task.unregister_portal_listener(local_port).await;
            });

            match server
                .register_portal_listener(
                    local_port,
                    agent_id.clone(),
                    PortMapping {
                        local_port,
                        target_host: target_host.clone(),
                        target_port,
                    },
                    handle,
                )
                .await
            {
                Ok(()) => {
                    successful_mappings.push(mapping.clone());
                    println!(
                        "{}{}",
                        styling::INDENT_LEVEL_1,
                        styling::format_check_item(&format!(
                            "Started: {}",
                            styling::format_agent_name(mapping)
                        ))
                    );
                }
                Err(e) => {
                    println!(
                        "{}{}",
                        styling::INDENT_LEVEL_1,
                        styling::format_cross_item(&format!("Failed to start {}: {}", mapping, e))
                    );
                }
            }
        }

        if successful_mappings.is_empty() {
            println!(
                "{}",
                styling::format_error_msg(
                    styling::ERROR_INDICATOR,
                    "No port forwarding mappings were successfully established"
                )
            );
            return Ok(());
        }

        let mut agents = server.agents().write().await;
        if let Some(agent) = agents.get_mut(&agent_id) {
            agent.tunnel_active = true;
            agent.tunnel_subnet = Some(format!(
                "Port forwarding: {}",
                successful_mappings.join(", ")
            ));
        }
        drop(agents);

        println!(
            "\n{} Portal Mode Active",
            styling::format_success_msg(styling::CHECK_INDICATOR, "")
                .trim_start()
                .bold()
        );
        println!("Port forwarding configured:");
        for mapping in &successful_mappings {
            let parts: Vec<&str> = mapping.split(':').collect();
            println!(
                "  {}",
                styling::format_arrow_mapping(
                    &format!("localhost:{}", parts[0]),
                    &format!("{}:{}", parts[1], parts[2])
                )
            );
        }
        println!();
        println!(
            "{}",
            styling::format_success_msg(
                styling::SUCCESS_INDICATOR,
                "Type 'done' when finished or 'stop' to stop port forwarding"
            )
        );
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

fn validate_port_mapping(mapping: &str) -> bool {
    let parts: Vec<&str> = mapping.split(':').collect();
    if parts.len() != 3 {
        return false;
    }

    // Validate local port
    if parts[0].parse::<u16>().is_err() {
        return false;
    }

    // Validate target host (basic check - not empty)
    if parts[1].is_empty() {
        return false;
    }

    // Validate target port
    if parts[2].parse::<u16>().is_err() {
        return false;
    }

    true
}

async fn initialize_streaming_managers(server: Arc<LabyrinthServer>) -> Result<()> {
    use std::collections::HashMap;

    let connection_manager = Arc::new(ServerConnectionManager::new());
    let (stream_manager_impl, mut stream_rx) = BidirectionalStreamManager::with_buffer_size(1000);
    let stream_manager: Arc<dyn StreamManagerTrait> = Arc::new(stream_manager_impl);

    let metrics = Arc::new(MetricsCollector::new());
    metrics.reset_metrics().await;

    {
        let metrics = Arc::clone(&metrics);
        tokio::spawn(async move {
            loop {
                metrics
                    .update_performance_metrics(0, 0.0, 0, HashMap::new())
                    .await;
                let _ = metrics.perform_health_check().await;
                let _ = metrics.get_metrics().await;
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }

    {
        let coordinator = ErrorRecoveryCoordinator::new(
            Arc::clone(&connection_manager) as Arc<dyn StreamConnectionManager>,
            Arc::clone(&stream_manager) as Arc<dyn StreamManagerTrait>,
            Arc::clone(&metrics),
        );
        tokio::spawn(async move {
            loop {
                let _ = coordinator.perform_proactive_recovery().await;
                tokio::time::sleep(Duration::from_secs(300)).await;
            }
        });
    }

    let server_for_bridge = Arc::clone(&server);
    tokio::spawn(async move {
        while let Some(msg) = stream_rx.recv().await {
            let Some(connection_id) = stream_message_connection_id(&msg) else {
                warn!("Received streaming message without connection id");
                continue;
            };

            if let Some(agent_id) = server_for_bridge.owner_for_connection(&connection_id).await {
                let sender = {
                    let agents = server_for_bridge.agents().read().await;
                    agents.get(&agent_id).map(|agent| agent.sender.clone())
                };

                if let Some(agent_sender) = sender {
                    if let Err(e) = agent_sender.send(Message::Stream(msg.clone())).await {
                        error!(
                            "Failed to forward stream message to agent {} for {}: {}",
                            agent_id, connection_id, e
                        );
                    }
                } else {
                    warn!(
                        "No active agent {} found for streaming connection {}",
                        agent_id, connection_id
                    );
                }
            } else {
                warn!(
                    "No owner registered for streaming connection {}",
                    connection_id
                );
            }
        }
    });

    server
        .set_streaming_managers(stream_manager.clone(), connection_manager.clone())
        .await;

    Ok(())
}

async fn run_streaming_port_forward_listener(
    local_port: u16,
    target_host: String,
    target_port: u16,
    server: Arc<LabyrinthServer>,
    agent_sender: mpsc::Sender<Message>,
    agent_id: String,
) -> Result<()> {
    let addr = format!("127.0.0.1:{}", local_port);
    let listener = TcpListener::bind(&addr).await?;
    info!(
        "Streaming port forward listener started on {} -> {}:{}",
        addr, target_host, target_port
    );

    let stream_manager = server
        .get_stream_manager()
        .await
        .ok_or_else(|| LabyrinthError::Message("Streaming manager not initialized".to_string()))?;
    let connection_manager = server
        .get_connection_manager()
        .await
        .ok_or_else(|| LabyrinthError::Message("Connection manager not initialized".to_string()))?;

    loop {
        match listener.accept().await {
            Ok((client_socket, client_addr)) => {
                info!(
                    "Streaming Portal: client {} connected on {}",
                    client_addr, addr
                );

                let mapping = PortMapping {
                    local_port,
                    target_host: target_host.clone(),
                    target_port,
                };

                let connection_id = ConnectionId::new_v4();
                let tracking_mapping = mapping.clone();
                if let Err(e) = connection_manager
                    .track_existing_connection(connection_id, client_addr, tracking_mapping)
                    .await
                {
                    error!(
                        "Failed to register streaming connection for {}:{} -> {}:{}: {}",
                        addr, local_port, target_host, target_port, e
                    );
                    continue;
                }

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
                        agent_id.clone(),
                        connection_id,
                        client_socket,
                        mapping,
                    )
                    .await
                    {
                        error!(
                            "Failed to create QUIC stream for {}:{} -> {}:{}: {}",
                            addr, local_port, target_host, target_port, e
                        );
                        let _ = connection_manager.cleanup_connection(&connection_id).await;
                        let _ = server.unregister_connection_owner(&connection_id).await;
                    }
                    continue;
                }

                if let Err(e) = stream_manager
                    .create_bidirectional_stream(connection_id, client_socket)
                    .await
                {
                    error!(
                        "Failed to create bidirectional stream for {}:{} -> {}:{}: {}",
                        addr, local_port, target_host, target_port, e
                    );
                    let _ = connection_manager.cleanup_connection(&connection_id).await;
                    let _ = server.unregister_connection_owner(&connection_id).await;
                    continue;
                }

                let setup_msg = StreamMessage::Setup {
                    connection_id,
                    mapping: mapping.clone(),
                };
                if let Err(e) = agent_sender.send(Message::Stream(setup_msg)).await {
                    error!(
                        "Failed to send stream setup for {}:{} -> {}:{}: {}",
                        addr, local_port, target_host, target_port, e
                    );
                    let _ = stream_manager.terminate_stream(connection_id).await;
                    let _ = connection_manager.cleanup_connection(&connection_id).await;
                    let _ = server.unregister_connection_owner(&connection_id).await;
                    continue;
                }
            }
            Err(e) => {
                error!(
                    "Failed to accept streaming client on {} ({}): {}",
                    addr, agent_id, e
                );
                break;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn validate_mapping_formats() {
        assert!(validate_port_mapping("8080:example.com:80"));
        assert!(!validate_port_mapping("invalid"));
        assert!(!validate_port_mapping("8080::80"));
        assert!(!validate_port_mapping("abc:host:80"));
    }

    #[test]
    fn extract_stream_message_connection_id() {
        let connection_id = ConnectionId::new_v4();
        let msg = StreamMessage::Close {
            connection_id,
            reason: crate::streaming::CloseReason::UserRequested,
        };
        assert_eq!(stream_message_connection_id(&msg), Some(connection_id));
        assert!(
            stream_message_connection_id(&StreamMessage::StreamClose("legacy".into())).is_none()
        );
    }

    #[test]
    fn resolve_auth_key_behavior() {
        env::remove_var("LABYRINTH_AUTH_KEY");
        assert!(resolve_auth_key(false).is_err());

        env::set_var("LABYRINTH_AUTH_KEY", "secret");
        assert_eq!(resolve_auth_key(false).unwrap(), Some("secret".to_string()));

        env::remove_var("LABYRINTH_AUTH_KEY");
        assert_eq!(resolve_auth_key(true).unwrap(), None);
    }

    #[test]
    fn autoenum_timeout_is_extended() {
        let timeout = command_timeout_for_token("linux:autoenum");
        assert_eq!(timeout, Duration::from_secs(20 * 60));
    }

    #[test]
    fn default_command_timeout_is_standard() {
        let timeout = command_timeout_for_token("linux:whoami");
        assert_eq!(timeout, Duration::from_secs(120));
    }

    #[test]
    fn shell_local_commands_use_prefixed_tokens() {
        assert_eq!(
            shell_local_command_token(CommandsOs::Linux, "/sysenum"),
            Some("linux:sysenum".to_string())
        );
    }

    #[test]
    fn shell_plain_commands_are_not_intercepted() {
        assert_eq!(
            shell_local_command_token(CommandsOs::Linux, "sysenum"),
            None
        );
    }

    #[test]
    fn shell_input_encoding_produces_transport_token() {
        let encoded = general_purpose::STANDARD.encode("ls\n".as_bytes());
        assert_eq!(encoded, "bHMK");
    }

    #[test]
    fn raw_shell_key_translation_forwards_control_c() {
        let input = raw_shell_key_input(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(matches!(input, RawShellInput::Bytes(bytes) if bytes == vec![0x03]));
    }

    #[test]
    fn raw_shell_key_translation_detaches_on_control_bracket() {
        let input = raw_shell_key_input(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL));
        assert!(matches!(input, RawShellInput::Detach));
    }

    #[test]
    fn raw_shell_key_translation_maps_arrow_keys_to_ansi() {
        let input = raw_shell_key_input(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert!(matches!(input, RawShellInput::Bytes(bytes) if bytes == b"\x1b[A".to_vec()));
    }

    #[test]
    fn raw_shell_key_translation_ignores_release_events() {
        let input = raw_shell_key_input(KeyEvent {
            code: KeyCode::Char('x'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: crossterm::event::KeyEventState::NONE,
        });
        assert!(matches!(input, RawShellInput::Ignore));
    }

    #[test]
    fn command_helper_completes_top_level_commands() {
        let helper = CommandHelper::new(vec!["help", "hello", "status"]);
        let (_start, matches) = helper.complete_pairs("he", 2);
        let replacements: Vec<String> = matches.into_iter().map(|pair| pair.replacement).collect();
        assert_eq!(replacements, vec!["help".to_string(), "hello".to_string()]);
    }

    #[test]
    fn shell_helper_only_completes_local_slash_commands() {
        let helper = ShellHelper::new(vec!["/help", "/history", "/exit"]);
        let (_start, matches) = helper.complete_local_command("/h", 2);
        let replacements: Vec<String> = matches.into_iter().map(|pair| pair.replacement).collect();
        assert_eq!(
            replacements,
            vec!["/help".to_string(), "/history".to_string()]
        );
    }

    #[test]
    fn upload_normalizer_keeps_regular_paths() {
        assert_eq!(
            normalize_remote_upload_path("/tmp/text", r"C:\Temp\text.txt"),
            r"C:\Temp\text.txt"
        );
    }

    #[test]
    fn upload_normalizer_fixes_windows_prompt_directory_input() {
        assert_eq!(
            normalize_remote_upload_path("/tmp/text", r"C:\Users\gMSA_ADFS_prod$\Documents>"),
            r"C:\Users\gMSA_ADFS_prod$\Documents\text"
        );
    }
}

#[derive(Clone, Copy)]
enum CommandsOs {
    Linux,
    Windows,
}

#[derive(Clone)]
struct CommandHelper {
    commands: Vec<String>,
}

impl CommandHelper {
    fn new(commands: Vec<&str>) -> Self {
        Self {
            commands: commands.into_iter().map(str::to_string).collect(),
        }
    }

    fn complete_pairs(&self, line: &str, pos: usize) -> (usize, Vec<Pair>) {
        let start = line[..pos].rfind(' ').map(|idx| idx + 1).unwrap_or(0);
        let needle = &line[start..pos];
        let matches = self
            .commands
            .iter()
            .filter(|cmd| cmd.starts_with(needle))
            .map(|cmd| Pair {
                display: cmd.clone(),
                replacement: cmd.clone(),
            })
            .collect();
        (start, matches)
    }
}

impl Helper for CommandHelper {}
impl Highlighter for CommandHelper {
    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        Cow::Owned(hint.bright_black().to_string())
    }
}
impl Validator for CommandHelper {}

impl Hinter for CommandHelper {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, _ctx: &RustyContext<'_>) -> Option<String> {
        let (_, matches) = self.complete_pairs(line, pos);
        let first = matches.first()?;
        first
            .replacement
            .strip_prefix(&line[..pos])
            .map(str::to_string)
    }
}

impl Completer for CommandHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &RustyContext<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        Ok(self.complete_pairs(line, pos))
    }
}

#[derive(Clone)]
struct ShellHelper {
    local_commands: Vec<String>,
}

impl ShellHelper {
    fn new(commands: Vec<&str>) -> Self {
        Self {
            local_commands: commands.into_iter().map(str::to_string).collect(),
        }
    }

    fn complete_local_command(&self, line: &str, pos: usize) -> (usize, Vec<Pair>) {
        let typed = &line[..pos];
        let command = typed.split_whitespace().next().unwrap_or(typed);
        let matches = self
            .local_commands
            .iter()
            .filter(|candidate| candidate.starts_with(command))
            .map(|candidate| Pair {
                display: candidate.clone(),
                replacement: candidate.clone(),
            })
            .collect();
        (0, matches)
    }
}

impl Helper for ShellHelper {}
impl Highlighter for ShellHelper {
    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        Cow::Owned(hint.bright_black().to_string())
    }
}
impl Validator for ShellHelper {}

impl Hinter for ShellHelper {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, _ctx: &RustyContext<'_>) -> Option<String> {
        if !line.starts_with('/') {
            return None;
        }

        let (_, matches) = self.complete_local_command(line, pos);
        let first = matches.first()?;
        first
            .replacement
            .strip_prefix(&line[..pos])
            .map(str::to_string)
    }
}

impl Completer for ShellHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &RustyContext<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        if line.starts_with('/') {
            Ok(self.complete_local_command(line, pos))
        } else {
            Ok((pos, Vec::new()))
        }
    }
}

async fn start_commands_mode(server: &LabyrinthServer) -> Result<()> {
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

    let (agent_name, agent_os, agent_sender, command_response, shell_events) = {
        let agents = server.agents().read().await;
        let Some(agent) = agents.get(&agent_id) else {
            println!(
                "{}",
                styling::format_error_msg(styling::ERROR_INDICATOR, "Selected agent not found")
            );
            return Ok(());
        };

        (
            agent.info.name.clone(),
            agent.info.os.clone(),
            agent.sender.clone(),
            agent.command_response.clone(),
            agent.shell_events.clone(),
        )
    };

    println!("\n{}", "Commands Mode".cyan().bold());
    println!("{}", "─────────────".bright_black());
    println!(
        "Selected agent: {} ({})",
        agent_name.cyan(),
        agent_os.bright_black()
    );

    loop {
        let auto_detected = detect_os_profile(&agent_os);
        let auto_label = match auto_detected {
            Some(CommandsOs::Linux) => "Automatic (detected: Linux)",
            Some(CommandsOs::Windows) => "Automatic (detected: Windows)",
            None => "Automatic (detected: Unknown)",
        };

        let os_choices = vec![auto_label, "Custom", "Back"];
        let os_selection = Select::new()
            .with_prompt("Select command profile")
            .items(&os_choices)
            .interact()
            .map_err(|e| LabyrinthError::Message(format!("Selection error: {}", e)))?;

        let selected_os = match os_selection {
            0 => {
                if let Some(os) = auto_detected {
                    os
                } else {
                    println!(
                        "{}",
                        styling::format_warning_msg(
                            styling::WARNING_INDICATOR,
                            "Automatic detection failed. Use Custom and choose Linux or Windows."
                        )
                    );
                    continue;
                }
            }
            1 => {
                let custom_choices = vec!["Linux", "Windows", "Back"];
                let custom_selection = Select::new()
                    .with_prompt("Select OS profile")
                    .items(&custom_choices)
                    .interact()
                    .map_err(|e| LabyrinthError::Message(format!("Selection error: {}", e)))?;

                match custom_selection {
                    0 => CommandsOs::Linux,
                    1 => CommandsOs::Windows,
                    _ => continue,
                }
            }
            _ => break,
        };

        loop {
            let category_choices = vec![
                "General", "Network", "AutoEnum", "Priv esc", "Shell", "Upload", "Download", "Back",
            ];
            let category_selection = Select::new()
                .with_prompt("Select category")
                .items(&category_choices)
                .interact()
                .map_err(|e| LabyrinthError::Message(format!("Selection error: {}", e)))?;

            match category_selection {
                0 => {
                    let commands = general_commands_for(selected_os);
                    if !run_command_menu(&agent_name, &agent_sender, &command_response, &commands)
                        .await?
                    {
                        break;
                    }
                }
                1 => {
                    let commands = network_commands_for(selected_os);
                    if !run_command_menu(&agent_name, &agent_sender, &command_response, &commands)
                        .await?
                    {
                        break;
                    }
                }
                2 => {
                    let commands = autoenum_commands_for(selected_os);
                    if !run_command_menu(&agent_name, &agent_sender, &command_response, &commands)
                        .await?
                    {
                        break;
                    }
                }
                3 => {
                    let commands = priv_esc_commands_for(selected_os);
                    if !run_command_menu(&agent_name, &agent_sender, &command_response, &commands)
                        .await?
                    {
                        break;
                    }
                }
                4 => {
                    if let Err(e) = start_shell_mode(
                        &agent_name,
                        selected_os,
                        &agent_sender,
                        &command_response,
                        &shell_events,
                    )
                    .await
                    {
                        println!(
                            "{}",
                            styling::format_error_msg(
                                styling::ERROR_INDICATOR,
                                &format!("Shell session error: {}", e)
                            )
                        );
                    }
                }
                5 => {
                    if let Err(e) = start_upload_mode_with_handles(
                        &agent_name,
                        &agent_sender,
                        &command_response,
                    )
                    .await
                    {
                        println!(
                            "{}",
                            styling::format_error_msg(
                                styling::ERROR_INDICATOR,
                                &format!("Upload failed: {}", e)
                            )
                        );
                    }
                }
                6 => {
                    if let Err(e) = start_download_mode_with_handles(
                        &agent_name,
                        &agent_sender,
                        &command_response,
                    )
                    .await
                    {
                        println!(
                            "{}",
                            styling::format_error_msg(
                                styling::ERROR_INDICATOR,
                                &format!("Download failed: {}", e)
                            )
                        );
                    }
                }
                _ => break,
            }
        }
    }

    Ok(())
}

fn autoenum_commands_for(os: CommandsOs) -> Vec<(&'static str, &'static str)> {
    match os {
        CommandsOs::Linux => vec![("autoenum (linpeas)", "linux:autoenum")],
        CommandsOs::Windows => vec![("autoenum (winpeas)", "windows:autoenum")],
    }
}

fn detect_os_profile(os_name: &str) -> Option<CommandsOs> {
    let normalized = os_name.to_lowercase();
    if normalized.contains("linux") {
        Some(CommandsOs::Linux)
    } else if normalized.contains("windows") {
        Some(CommandsOs::Windows)
    } else {
        None
    }
}

fn general_commands_for(os: CommandsOs) -> Vec<(&'static str, &'static str)> {
    match os {
        CommandsOs::Linux => vec![("whoami", "linux:whoami"), ("sysenum", "linux:sysenum")],
        CommandsOs::Windows => vec![
            ("whoami /all", "windows:whoami_all"),
            ("sysenum", "windows:sysenum"),
        ],
    }
}

fn network_commands_for(os: CommandsOs) -> Vec<(&'static str, &'static str)> {
    match os {
        CommandsOs::Linux => vec![
            ("network summary", "linux:network_summary"),
            ("ifconfig", "linux:ifconfig"),
            ("ss -tunlp", "linux:ss"),
            ("route", "linux:route"),
            ("resolvectl status", "linux:resolvectl"),
        ],
        CommandsOs::Windows => vec![
            ("network summary", "windows:network_summary"),
            ("ipconfig /all", "windows:ipconfig_all"),
            ("route print", "windows:route_print"),
            ("netstat -ano", "windows:netstat_ano"),
        ],
    }
}

fn priv_esc_commands_for(os: CommandsOs) -> Vec<(&'static str, &'static str)> {
    match os {
        CommandsOs::Linux => vec![("priv esc scaffold (no-op)", "linux:privesc_placeholder")],
        CommandsOs::Windows => vec![("priv esc scaffold (no-op)", "windows:privesc_placeholder")],
    }
}

async fn run_command_menu(
    agent_name: &str,
    agent_sender: &mpsc::Sender<Message>,
    command_response: &Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<Message>>>>,
    commands: &[(&str, &str)],
) -> Result<bool> {
    let mut items: Vec<String> = commands
        .iter()
        .map(|(label, _)| (*label).to_string())
        .collect();
    items.push("Back".to_string());

    let selection = Select::new()
        .with_prompt("Select command")
        .items(&items)
        .interact()
        .map_err(|e| LabyrinthError::Message(format!("Selection error: {}", e)))?;

    if selection >= commands.len() {
        return Ok(false);
    }

    let (display, token) = commands[selection];
    println!(
        "\n{} Executing: {}",
        styling::format_success_msg(styling::SUCCESS_INDICATOR, "").trim_start(),
        display.cyan().bold()
    );

    println!(
        "{}",
        styling::format_success_msg(styling::SUCCESS_INDICATOR, "Waiting for response...")
    );

    execute_remote_message(
        agent_name,
        display,
        token,
        agent_sender,
        command_response,
        Message::CommandRequest {
            command: token.to_string(),
        },
        command_timeout_for_token(token),
    )
    .await;

    Ok(true)
}

async fn start_upload_mode(server: &LabyrinthServer) -> Result<()> {
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

    let (agent_name, agent_sender, command_response) = {
        let agents = server.agents().read().await;
        let Some(agent) = agents.get(&agent_id) else {
            return Err(LabyrinthError::Message(
                "Selected agent not found".to_string(),
            ));
        };
        (
            agent.info.name.clone(),
            agent.sender.clone(),
            agent.command_response.clone(),
        )
    };

    start_upload_mode_with_handles(&agent_name, &agent_sender, &command_response).await
}

async fn start_upload_mode_with_handles(
    agent_name: &str,
    agent_sender: &mpsc::Sender<Message>,
    command_response: &Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<Message>>>>,
) -> Result<()> {
    let local_path: String = Input::new()
        .with_prompt("Local file path")
        .interact_text()
        .map_err(|e| LabyrinthError::Message(format!("Input error: {}", e)))?;
    let remote_path: String = Input::new()
        .with_prompt("Remote destination path")
        .interact_text()
        .map_err(|e| LabyrinthError::Message(format!("Input error: {}", e)))?;

    perform_upload(
        agent_name,
        agent_sender,
        command_response,
        &local_path,
        &remote_path,
    )
    .await
}

async fn start_download_mode(server: &LabyrinthServer) -> Result<()> {
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

    let (agent_name, agent_sender, command_response) = {
        let agents = server.agents().read().await;
        let Some(agent) = agents.get(&agent_id) else {
            return Err(LabyrinthError::Message(
                "Selected agent not found".to_string(),
            ));
        };
        (
            agent.info.name.clone(),
            agent.sender.clone(),
            agent.command_response.clone(),
        )
    };

    start_download_mode_with_handles(&agent_name, &agent_sender, &command_response).await
}

async fn start_download_mode_with_handles(
    agent_name: &str,
    agent_sender: &mpsc::Sender<Message>,
    command_response: &Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<Message>>>>,
) -> Result<()> {
    let remote_path: String = Input::new()
        .with_prompt("Remote file path")
        .interact_text()
        .map_err(|e| LabyrinthError::Message(format!("Input error: {}", e)))?;
    let local_path: String = Input::new()
        .with_prompt("Local destination path")
        .interact_text()
        .map_err(|e| LabyrinthError::Message(format!("Input error: {}", e)))?;

    perform_download(
        agent_name,
        agent_sender,
        command_response,
        &remote_path,
        &local_path,
    )
    .await
}

async fn perform_upload(
    agent_name: &str,
    agent_sender: &mpsc::Sender<Message>,
    command_response: &Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<Message>>>>,
    local_path: &str,
    remote_path: &str,
) -> Result<()> {
    let remote_path = normalize_remote_upload_path(local_path, remote_path);
    let bytes = fs::read(local_path).map_err(|e| {
        LabyrinthError::Message(format!("Failed to read local file '{}': {}", local_path, e))
    })?;
    let encoded = general_purpose::STANDARD.encode(bytes);

    println!(
        "{}",
        styling::format_success_msg(
            styling::SUCCESS_INDICATOR,
            &format!("Uploading {} -> {}", local_path, remote_path)
        )
    );

    execute_remote_message(
        agent_name,
        "upload",
        &format!("upload:{}", remote_path),
        agent_sender,
        command_response,
        Message::FileUpload {
            remote_path: remote_path.clone(),
            content_b64: encoded,
        },
        Duration::from_secs(300),
    )
    .await;

    Ok(())
}

async fn perform_download(
    agent_name: &str,
    agent_sender: &mpsc::Sender<Message>,
    command_response: &Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<Message>>>>,
    remote_path: &str,
    local_path: &str,
) -> Result<()> {
    println!(
        "{}",
        styling::format_success_msg(
            styling::SUCCESS_INDICATOR,
            &format!("Downloading {} -> {}", remote_path, local_path)
        )
    );

    let (tx, rx) = tokio::sync::oneshot::channel();
    {
        let mut pending = command_response.lock().await;
        *pending = Some(tx);
    }

    agent_sender
        .send(Message::FileDownloadRequest {
            remote_path: remote_path.to_string(),
        })
        .await
        .map_err(|e| LabyrinthError::Message(format!("Failed to send download request: {}", e)))?;

    let response = tokio::time::timeout(Duration::from_secs(300), rx)
        .await
        .map_err(|_| {
            LabyrinthError::Message("Timed out waiting for download response".to_string())
        })?
        .map_err(|e| {
            LabyrinthError::Message(format!("Failed receiving download response: {}", e))
        })?;

    match response {
        Message::FileDownloadResponse {
            success,
            message,
            remote_path: source,
            content_b64,
        } => {
            if success {
                let payload = content_b64.ok_or_else(|| {
                    LabyrinthError::Message("Download succeeded but content missing".to_string())
                })?;
                let bytes = general_purpose::STANDARD
                    .decode(payload.as_bytes())
                    .map_err(|e| {
                        LabyrinthError::Message(format!("Invalid base64 payload: {}", e))
                    })?;

                let local_path_buf = PathBuf::from(local_path);
                if let Some(parent) = local_path_buf.parent() {
                    fs::create_dir_all(parent).map_err(|e| {
                        LabyrinthError::Message(format!("Failed to create local directory: {}", e))
                    })?;
                }
                fs::write(&local_path_buf, &bytes).map_err(|e| {
                    LabyrinthError::Message(format!("Failed to write local file: {}", e))
                })?;

                let output = format!(
                    "=== File Download ===\nSummary: Download succeeded\nDetails:\nSource: {}\nSaved to: {}\nBytes: {}\nMessage: {}",
                    source,
                    local_path_buf.display(),
                    bytes.len(),
                    message
                );
                println!("\n{}", decorate_command_output(&output));
                if let Ok(path) = persist_command_output(
                    agent_name,
                    "download",
                    &format!("download:{}", remote_path),
                    &output,
                    None,
                ) {
                    println!(
                        "{}",
                        styling::format_success_msg(
                            styling::SUCCESS_INDICATOR,
                            &format!("Saved command output to {}", path.display())
                        )
                    );
                }
            } else {
                let output = format!(
                    "=== File Download ===\nSummary: Download failed\nDetails:\nSource: {}\nMessage: {}",
                    source, message
                );
                println!("\n{}", decorate_command_output(&output));
                let _ = persist_command_output(
                    agent_name,
                    "download",
                    &format!("download:{}", remote_path),
                    &output,
                    Some(message.as_str()),
                );
            }
        }
        _ => {
            return Err(LabyrinthError::Message(
                "Received unexpected response for download request".to_string(),
            ));
        }
    }

    Ok(())
}

async fn start_shell_mode(
    agent_name: &str,
    selected_os: CommandsOs,
    agent_sender: &mpsc::Sender<Message>,
    command_response: &Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<Message>>>>,
    shell_events: &Arc<tokio::sync::Mutex<Option<mpsc::UnboundedSender<Message>>>>,
) -> Result<()> {
    println!(
        "\n{}",
        styling::format_section_title("Interactive Shell", "remote PTY session")
    );
    println!("{}", "────────────────".bright_black());

    let choices = vec![
        "Interactive terminal (SSH/WinRM style)",
        "Control shell (Labyrinth slash commands)",
        "Back",
    ];
    let selection = Select::new()
        .with_prompt("Select shell mode")
        .items(&choices)
        .interact()
        .map_err(|e| LabyrinthError::Message(format!("Selection error: {}", e)))?;

    match selection {
        0 => start_raw_shell_mode(agent_name, agent_sender, shell_events).await,
        1 => {
            start_control_shell_mode(
                agent_name,
                selected_os,
                agent_sender,
                command_response,
                shell_events,
            )
            .await
        }
        _ => Ok(()),
    }
}

async fn start_control_shell_mode(
    agent_name: &str,
    selected_os: CommandsOs,
    agent_sender: &mpsc::Sender<Message>,
    command_response: &Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<Message>>>>,
    shell_events: &Arc<tokio::sync::Mutex<Option<mpsc::UnboundedSender<Message>>>>,
) -> Result<()> {
    println!(
        "\n{}",
        styling::format_section_title("Control Shell", "stateful operator session")
    );
    println!("{}", "────────────────".bright_black());
    println!("{}", styling::format_hint("Local commands use a '/' prefix so nested prompts like mysql, python, and powershell stay fully interactive."));

    let transcript = create_shell_transcript(agent_name)?;
    println!(
        "{}",
        styling::format_success_msg(
            styling::SUCCESS_INDICATOR,
            &format!("Shell transcript: {}", transcript.display())
        )
    );

    let mut rl = Editor::<ShellHelper, rustyline::history::DefaultHistory>::new()
        .map_err(|e| LabyrinthError::Message(format!("Failed to start shell prompt: {}", e)))?;

    rl.set_helper(Some(ShellHelper::new(vec![
        "/help",
        "/clear",
        "/history",
        "/upload",
        "/download",
        "/sysenum",
        "/network",
        "/autoenum",
        "/privesc",
        "/resize",
        "/exit",
        "/quit",
        "/back",
    ])));

    let session_id = uuid::Uuid::new_v4().to_string();
    let (shell_tx, mut shell_rx) = mpsc::unbounded_channel();
    {
        let mut sink = shell_events.lock().await;
        *sink = Some(shell_tx);
    }

    let (cols, rows) = local_terminal_size();
    if let Err(e) =
        start_remote_shell_session(&session_id, agent_sender, &mut shell_rx, cols, rows).await
    {
        let mut sink = shell_events.lock().await;
        *sink = None;
        return Err(e);
    }
    let initial_output = collect_shell_output(
        &session_id,
        &mut shell_rx,
        Duration::from_millis(800),
        Duration::from_millis(120),
    )
    .await?;
    print_shell_output(&initial_output);
    append_shell_transcript(&transcript, &initial_output);

    loop {
        let line = match rl.readline(&shell_prompt(agent_name)) {
            Ok(v) => v.trim().to_string(),
            Err(rustyline::error::ReadlineError::Interrupted) => continue,
            Err(rustyline::error::ReadlineError::Eof) => break,
            Err(e) => return Err(LabyrinthError::Message(format!("Shell input error: {}", e))),
        };

        if line.is_empty() {
            send_shell_input(&session_id, "\n", agent_sender).await?;
            let output = collect_shell_output(
                &session_id,
                &mut shell_rx,
                Duration::from_millis(500),
                Duration::from_millis(120),
            )
            .await?;
            print_shell_output(&output);
            append_shell_transcript(&transcript, &output);
            continue;
        }
        let _ = rl.add_history_entry(line.as_str());

        if matches!(line.as_str(), "/exit" | "/back" | "/quit") {
            break;
        }

        if line == "/help" {
            println!("{}", "Shell Built-ins:".yellow().bold());
            println!("  {}  exit shell", "/exit".cyan());
            println!("  {}  clear local terminal", "/clear".cyan());
            println!("  {}  show local shell history", "/history".cyan());
            println!(
                "  {}  upload file to target",
                "/upload [local remote]".cyan()
            );
            println!(
                "  {}  download file from target",
                "/download [remote local]".cyan()
            );
            println!(
                "  {}  run Labyrinth presets",
                "/sysenum | /network | /autoenum | /privesc".cyan()
            );
            println!(
                "  {}  resize the remote PTY",
                "/resize <cols> <rows>".cyan()
            );
            println!(
                "  {}  send raw input to the current program prompt",
                "any other text".cyan()
            );
            continue;
        }

        if line == "/clear" {
            print!("\x1B[2J\x1B[H");
            let _ = std::io::stdout().flush();
            continue;
        }

        if line == "/history" {
            for (idx, entry) in rl.history().iter().enumerate() {
                println!("{:>4}  {}", idx + 1, entry);
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("/resize ") {
            let mut parts = rest.split_whitespace();
            let cols = parts.next().and_then(|v| v.parse::<u16>().ok());
            let rows = parts.next().and_then(|v| v.parse::<u16>().ok());
            match (cols, rows) {
                (Some(cols), Some(rows)) => {
                    agent_sender
                        .send(Message::ShellSessionResize {
                            session_id: session_id.clone(),
                            cols,
                            rows,
                        })
                        .await
                        .map_err(|e| {
                            LabyrinthError::Message(format!(
                                "Failed to resize shell session: {}",
                                e
                            ))
                        })?;
                    refresh_remote_shell_prompt(
                        &session_id,
                        agent_sender,
                        &mut shell_rx,
                        &transcript,
                    )
                    .await?;
                }
                _ => println!(
                    "{}",
                    styling::format_warning_msg(
                        styling::WARNING_INDICATOR,
                        "Usage: /resize <cols> <rows>"
                    )
                ),
            }
            continue;
        }

        append_shell_transcript(&transcript, &format!("> {}", line));

        if line == "/upload" {
            let local_path: String = Input::new()
                .with_prompt("Local file path")
                .interact_text()
                .map_err(|e| LabyrinthError::Message(format!("Input error: {}", e)))?;
            let remote_path: String = Input::new()
                .with_prompt("Remote destination path")
                .interact_text()
                .map_err(|e| LabyrinthError::Message(format!("Input error: {}", e)))?;
            if let Err(e) = perform_upload(
                agent_name,
                agent_sender,
                command_response,
                &local_path,
                &remote_path,
            )
            .await
            {
                println!(
                    "{}",
                    styling::format_error_msg(styling::ERROR_INDICATOR, &e.to_string())
                );
            } else {
                append_shell_transcript(
                    &transcript,
                    &format!("< upload completed: {} -> {}", local_path, remote_path),
                );
            }
            refresh_remote_shell_prompt(&session_id, agent_sender, &mut shell_rx, &transcript)
                .await?;
            continue;
        }

        if let Some(rest) = line.strip_prefix("/upload ") {
            let mut parts = rest.splitn(2, ' ');
            let local = parts.next().unwrap_or("").trim();
            let remote = parts.next().unwrap_or("").trim();
            if local.is_empty() || remote.is_empty() {
                println!(
                    "{}",
                    styling::format_warning_msg(
                        styling::WARNING_INDICATOR,
                        "Usage: /upload <local_path> <remote_path>"
                    )
                );
                continue;
            }
            if let Err(e) =
                perform_upload(agent_name, agent_sender, command_response, local, remote).await
            {
                println!(
                    "{}",
                    styling::format_error_msg(styling::ERROR_INDICATOR, &e.to_string())
                );
            }
            refresh_remote_shell_prompt(&session_id, agent_sender, &mut shell_rx, &transcript)
                .await?;
            continue;
        }

        if line == "/download" {
            let remote_path: String = Input::new()
                .with_prompt("Remote file path")
                .interact_text()
                .map_err(|e| LabyrinthError::Message(format!("Input error: {}", e)))?;
            let local_path: String = Input::new()
                .with_prompt("Local destination path")
                .interact_text()
                .map_err(|e| LabyrinthError::Message(format!("Input error: {}", e)))?;
            if let Err(e) = perform_download(
                agent_name,
                agent_sender,
                command_response,
                &remote_path,
                &local_path,
            )
            .await
            {
                println!(
                    "{}",
                    styling::format_error_msg(styling::ERROR_INDICATOR, &e.to_string())
                );
                append_shell_transcript(&transcript, &format!("! {}", e));
            } else {
                append_shell_transcript(
                    &transcript,
                    &format!("< download completed: {} -> {}", remote_path, local_path),
                );
            }
            refresh_remote_shell_prompt(&session_id, agent_sender, &mut shell_rx, &transcript)
                .await?;
            continue;
        }

        if let Some(rest) = line.strip_prefix("/download ") {
            let mut parts = rest.splitn(2, ' ');
            let remote = parts.next().unwrap_or("").trim();
            let local = parts.next().unwrap_or("").trim();
            if remote.is_empty() || local.is_empty() {
                println!(
                    "{}",
                    styling::format_warning_msg(
                        styling::WARNING_INDICATOR,
                        "Usage: /download <remote_path> <local_path>"
                    )
                );
                continue;
            }
            if let Err(e) =
                perform_download(agent_name, agent_sender, command_response, remote, local).await
            {
                println!(
                    "{}",
                    styling::format_error_msg(styling::ERROR_INDICATOR, &e.to_string())
                );
                append_shell_transcript(&transcript, &format!("! {}", e));
            } else {
                append_shell_transcript(
                    &transcript,
                    &format!("< download completed: {} -> {}", remote, local),
                );
            }
            refresh_remote_shell_prompt(&session_id, agent_sender, &mut shell_rx, &transcript)
                .await?;
            continue;
        }

        if let Some(command_token) = shell_local_command_token(selected_os, &line) {
            if let Some(log_line) = execute_remote_shell_message(
                agent_name,
                agent_sender,
                command_response,
                Message::CommandRequest {
                    command: command_token.clone(),
                },
                command_timeout_for_token(&command_token),
                &transcript,
            )
            .await
            {
                append_shell_transcript(&transcript, &format!("< {}", log_line));
            }
            refresh_remote_shell_prompt(&session_id, agent_sender, &mut shell_rx, &transcript)
                .await?;
            continue;
        }

        send_shell_input(&session_id, &(line + "\n"), agent_sender).await?;
        let output = collect_shell_output(
            &session_id,
            &mut shell_rx,
            Duration::from_secs(2),
            Duration::from_millis(150),
        )
        .await?;
        print_shell_output(&output);
        append_shell_transcript(&transcript, &output);
    }

    let _ = agent_sender
        .send(Message::ShellSessionClose {
            session_id: session_id.clone(),
        })
        .await;
    {
        let mut sink = shell_events.lock().await;
        *sink = None;
    }
    append_shell_transcript(&transcript, "[session ended]");

    Ok(())
}

async fn start_raw_shell_mode(
    agent_name: &str,
    agent_sender: &mpsc::Sender<Message>,
    shell_events: &Arc<tokio::sync::Mutex<Option<mpsc::UnboundedSender<Message>>>>,
) -> Result<()> {
    let transcript = create_shell_transcript(agent_name)?;
    println!(
        "{}",
        styling::format_success_msg(
            styling::SUCCESS_INDICATOR,
            &format!("Shell transcript: {}", transcript.display())
        )
    );
    println!(
        "{}",
        styling::format_hint("Entering raw terminal mode. Press Ctrl-] to detach.")
    );

    let session_id = uuid::Uuid::new_v4().to_string();
    let (shell_tx, mut shell_rx) = mpsc::unbounded_channel();
    {
        let mut sink = shell_events.lock().await;
        *sink = Some(shell_tx);
    }

    let (cols, rows) = local_terminal_size();
    if let Err(e) =
        start_remote_shell_session(&session_id, agent_sender, &mut shell_rx, cols, rows).await
    {
        let mut sink = shell_events.lock().await;
        *sink = None;
        return Err(e);
    }

    let _raw_guard = TerminalRawMode::enter()?;
    let stop = Arc::new(AtomicBool::new(false));
    let mut input_task =
        spawn_raw_shell_input_task(session_id.clone(), agent_sender.clone(), Arc::clone(&stop));
    let mut output_task = tokio::spawn(pump_raw_shell_output(
        session_id.clone(),
        shell_rx,
        transcript.clone(),
        Arc::clone(&stop),
    ));

    tokio::select! {
        input_result = &mut input_task => {
            stop.store(true, Ordering::SeqCst);
            match input_result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(e),
                Err(e) => return Err(LabyrinthError::Message(format!("Raw shell input task failed: {}", e))),
            }
            let _ = agent_sender
                .send(Message::ShellSessionClose {
                    session_id: session_id.clone(),
                })
                .await;
            output_task.abort();
        }
        output_result = &mut output_task => {
            stop.store(true, Ordering::SeqCst);
            match output_result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(e),
                Err(e) if e.is_cancelled() => {}
                Err(e) => return Err(LabyrinthError::Message(format!("Raw shell output task failed: {}", e))),
            }
            input_task.abort();
        }
    }

    {
        let mut sink = shell_events.lock().await;
        *sink = None;
    }
    append_shell_transcript(&transcript, "[raw session ended]");

    println!();
    println!(
        "{}",
        styling::format_success_msg(styling::SUCCESS_INDICATOR, "Detached from shell")
    );

    Ok(())
}

fn shell_local_command_token(selected_os: CommandsOs, input: &str) -> Option<String> {
    match selected_os {
        CommandsOs::Linux => match input {
            "/sysenum" => Some("linux:sysenum".to_string()),
            "/network" | "/network summary" => Some("linux:network_summary".to_string()),
            "/autoenum" => Some("linux:autoenum".to_string()),
            "/privesc" => Some("linux:privesc_placeholder".to_string()),
            _ => None,
        },
        CommandsOs::Windows => match input {
            "/sysenum" => Some("windows:sysenum".to_string()),
            "/network" | "/network summary" => Some("windows:network_summary".to_string()),
            "/autoenum" => Some("windows:autoenum".to_string()),
            "/privesc" => Some("windows:privesc_placeholder".to_string()),
            _ => None,
        },
    }
}

struct TerminalRawMode;

impl TerminalRawMode {
    fn enter() -> Result<Self> {
        enable_raw_mode()
            .map_err(|e| LabyrinthError::Message(format!("Failed to enter raw mode: {}", e)))?;
        Ok(Self)
    }
}

impl Drop for TerminalRawMode {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

enum RawShellInput {
    Bytes(Vec<u8>),
    Detach,
    Ignore,
}

fn local_terminal_size() -> (u16, u16) {
    crossterm::terminal::size().unwrap_or((120, 32))
}

fn spawn_raw_shell_input_task(
    session_id: String,
    agent_sender: mpsc::Sender<Message>,
    stop: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<Result<()>> {
    tokio::task::spawn_blocking(move || {
        while !stop.load(Ordering::SeqCst) {
            if !event::poll(Duration::from_millis(100)).map_err(|e| {
                LabyrinthError::Message(format!("Failed to poll terminal input: {}", e))
            })? {
                continue;
            }

            let input = match event::read().map_err(|e| {
                LabyrinthError::Message(format!("Failed to read terminal input: {}", e))
            })? {
                Event::Key(key) => raw_shell_key_input(key),
                Event::Paste(text) => RawShellInput::Bytes(text.into_bytes()),
                Event::Resize(cols, rows) => {
                    let _ = agent_sender.blocking_send(Message::ShellSessionResize {
                        session_id: session_id.clone(),
                        cols,
                        rows,
                    });
                    RawShellInput::Ignore
                }
                _ => RawShellInput::Ignore,
            };

            match input {
                RawShellInput::Bytes(bytes) if !bytes.is_empty() => {
                    agent_sender
                        .blocking_send(Message::ShellSessionInput {
                            session_id: session_id.clone(),
                            data_b64: general_purpose::STANDARD.encode(bytes),
                        })
                        .map_err(|e| {
                            LabyrinthError::Message(format!("Failed to send shell input: {}", e))
                        })?;
                }
                RawShellInput::Detach => break,
                _ => {}
            }
        }

        Ok(())
    })
}

async fn pump_raw_shell_output(
    session_id: String,
    mut shell_rx: mpsc::UnboundedReceiver<Message>,
    transcript: PathBuf,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    while let Some(message) = shell_rx.recv().await {
        match message {
            Message::ShellSessionOutput {
                session_id: msg_session,
                data_b64,
            } if msg_session == session_id => {
                let bytes = general_purpose::STANDARD.decode(data_b64.as_bytes())?;
                std::io::stdout().write_all(&bytes)?;
                std::io::stdout().flush()?;
                append_shell_transcript(&transcript, &String::from_utf8_lossy(&bytes));
            }
            Message::ShellSessionClose {
                session_id: msg_session,
            } if msg_session == session_id => {
                stop.store(true, Ordering::SeqCst);
                std::io::stdout().write_all(b"\r\n[labyrinth] remote shell session closed\r\n")?;
                std::io::stdout().flush()?;
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

fn raw_shell_key_input(key: KeyEvent) -> RawShellInput {
    if key.kind == KeyEventKind::Release {
        return RawShellInput::Ignore;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char(']') {
        return RawShellInput::Detach;
    }

    let mut bytes = Vec::new();
    if key.modifiers.contains(KeyModifiers::ALT) {
        bytes.push(0x1b);
    }

    match key.code {
        KeyCode::Backspace => bytes.push(0x7f),
        KeyCode::Enter => bytes.push(b'\r'),
        KeyCode::Left => bytes.extend_from_slice(b"\x1b[D"),
        KeyCode::Right => bytes.extend_from_slice(b"\x1b[C"),
        KeyCode::Up => bytes.extend_from_slice(b"\x1b[A"),
        KeyCode::Down => bytes.extend_from_slice(b"\x1b[B"),
        KeyCode::Home => bytes.extend_from_slice(b"\x1b[H"),
        KeyCode::End => bytes.extend_from_slice(b"\x1b[F"),
        KeyCode::PageUp => bytes.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => bytes.extend_from_slice(b"\x1b[6~"),
        KeyCode::Tab | KeyCode::BackTab => bytes.push(b'\t'),
        KeyCode::Delete => bytes.extend_from_slice(b"\x1b[3~"),
        KeyCode::Insert => bytes.extend_from_slice(b"\x1b[2~"),
        KeyCode::Esc => bytes.push(0x1b),
        KeyCode::Char(ch) if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(byte) = control_char_byte(ch) {
                bytes.push(byte);
            }
        }
        KeyCode::Char(ch) => {
            let mut encoded = [0u8; 4];
            bytes.extend_from_slice(ch.encode_utf8(&mut encoded).as_bytes());
        }
        _ => {}
    }

    if bytes.is_empty() {
        RawShellInput::Ignore
    } else {
        RawShellInput::Bytes(bytes)
    }
}

fn control_char_byte(ch: char) -> Option<u8> {
    let upper = ch.to_ascii_uppercase();
    if upper.is_ascii_alphabetic() {
        Some((upper as u8) - b'A' + 1)
    } else {
        match ch {
            '@' => Some(0x00),
            '[' => Some(0x1b),
            '\\' => Some(0x1c),
            '^' => Some(0x1e),
            '_' => Some(0x1f),
            '?' => Some(0x7f),
            _ => None,
        }
    }
}

async fn start_remote_shell_session(
    session_id: &str,
    agent_sender: &mpsc::Sender<Message>,
    shell_rx: &mut mpsc::UnboundedReceiver<Message>,
    cols: u16,
    rows: u16,
) -> Result<()> {
    agent_sender
        .send(Message::ShellSessionStart {
            session_id: session_id.to_string(),
            cols,
            rows,
        })
        .await
        .map_err(|e| LabyrinthError::Message(format!("Failed to start shell session: {}", e)))?;

    match tokio::time::timeout(Duration::from_secs(30), shell_rx.recv()).await {
        Ok(Some(Message::ShellSessionStarted {
            session_id: msg_session,
            success,
            message,
        })) if msg_session == session_id => {
            if success {
                Ok(())
            } else {
                Err(LabyrinthError::Message(message))
            }
        }
        Ok(Some(other)) => Err(LabyrinthError::Message(format!(
            "Unexpected shell session response: {:?}",
            other
        ))),
        Ok(None) => Err(LabyrinthError::Message(
            "Shell session channel closed unexpectedly".to_string(),
        )),
        Err(_) => Err(LabyrinthError::Message(
            "Timed out waiting for remote shell session".to_string(),
        )),
    }
}

async fn send_shell_input(
    session_id: &str,
    input: &str,
    agent_sender: &mpsc::Sender<Message>,
) -> Result<()> {
    agent_sender
        .send(Message::ShellSessionInput {
            session_id: session_id.to_string(),
            data_b64: general_purpose::STANDARD.encode(input.as_bytes()),
        })
        .await
        .map_err(|e| LabyrinthError::Message(format!("Failed to send shell input: {}", e)))
}

async fn collect_shell_output(
    session_id: &str,
    shell_rx: &mut mpsc::UnboundedReceiver<Message>,
    initial_wait: Duration,
    idle_wait: Duration,
) -> Result<String> {
    let mut output = String::new();
    let mut seen_output = false;

    loop {
        let wait = if seen_output { idle_wait } else { initial_wait };
        match tokio::time::timeout(wait, shell_rx.recv()).await {
            Ok(Some(Message::ShellSessionOutput {
                session_id: msg_session,
                data_b64,
            })) if msg_session == session_id => {
                let bytes = general_purpose::STANDARD.decode(data_b64.as_bytes())?;
                output.push_str(&String::from_utf8_lossy(&bytes));
                seen_output = true;
            }
            Ok(Some(Message::ShellSessionClose {
                session_id: msg_session,
            })) if msg_session == session_id => {
                output.push_str("\n[labyrinth] remote shell session closed\n");
                break;
            }
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => break,
        }
    }

    Ok(output)
}

fn print_shell_output(output: &str) {
    if !output.is_empty() {
        print!("{}", output);
        let _ = std::io::stdout().flush();
    }
}

fn shell_prompt(agent_name: &str) -> String {
    format!(
        "shell ({}) {} ",
        agent_name.cyan(),
        styling::ARROW_INDICATOR.cyan()
    )
}

fn normalize_remote_upload_path(local_path: &str, remote_path: &str) -> String {
    let trimmed = remote_path.trim();
    if trimmed.is_empty() {
        return trimmed.to_string();
    }

    let mut candidate = trimmed.to_string();
    let mut treat_as_directory = false;

    if candidate.ends_with('>') && candidate.contains('\\') {
        candidate.pop();
        candidate = candidate.trim_end().to_string();
        treat_as_directory = true;
    }

    if candidate.ends_with(['\\', '/']) {
        candidate = candidate.trim_end_matches(['\\', '/']).to_string();
        treat_as_directory = true;
    }

    if treat_as_directory {
        if let Some(file_name) = Path::new(local_path)
            .file_name()
            .and_then(|name| name.to_str())
        {
            let separator = if candidate.contains('\\') { "\\" } else { "/" };
            return format!("{}{}{}", candidate, separator, file_name);
        }
    }

    candidate
}

async fn refresh_remote_shell_prompt(
    session_id: &str,
    agent_sender: &mpsc::Sender<Message>,
    shell_rx: &mut mpsc::UnboundedReceiver<Message>,
    transcript: &PathBuf,
) -> Result<()> {
    send_shell_input(session_id, "\n", agent_sender).await?;
    let output = collect_shell_output(
        session_id,
        shell_rx,
        Duration::from_millis(700),
        Duration::from_millis(120),
    )
    .await?;
    print_shell_output(&output);
    append_shell_transcript(transcript, &output);
    Ok(())
}

async fn execute_remote_message(
    agent_name: &str,
    display: &str,
    token: &str,
    agent_sender: &mpsc::Sender<Message>,
    command_response: &Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<Message>>>>,
    outbound: Message,
    timeout: Duration,
) -> Option<String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    {
        let mut pending = command_response.lock().await;
        *pending = Some(tx);
    }

    if let Err(e) = agent_sender.send(outbound).await {
        error!("Failed to send request to agent: {}", e);
        println!(
            "{}",
            styling::format_error_msg(styling::ERROR_INDICATOR, "Failed to send request to agent")
        );
        return Some("Failed to send request to agent".to_string());
    }

    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(Message::CommandResponse { output, error })) => {
            let saved =
                persist_command_output(agent_name, display, token, &output, error.as_deref());
            let mut raw_log = String::new();
            if let Some(err) = error {
                println!("\n{}", decorate_command_output(&err));
                raw_log.push_str(&err);
                raw_log.push('\n');
            }
            if !output.trim().is_empty() {
                println!("\n{}", decorate_command_output(&output));
                raw_log.push_str(&output);
            }
            if let Ok(path) = saved {
                println!(
                    "{}",
                    styling::format_success_msg(
                        styling::SUCCESS_INDICATOR,
                        &format!("Saved command output to {}", path.display())
                    )
                );
            }
            Some(raw_log)
        }
        Ok(Ok(Message::FileUploadResponse { success, message })) => {
            let output = format!(
                "=== File Upload ===\nSummary: {}\nDetails:\n{}",
                if success {
                    "Upload succeeded"
                } else {
                    "Upload failed"
                },
                message
            );
            let error = if success {
                None
            } else {
                Some(message.as_str())
            };
            let saved = persist_command_output(agent_name, display, token, &output, error);
            println!("\n{}", decorate_command_output(&output));
            if let Ok(path) = saved {
                println!(
                    "{}",
                    styling::format_success_msg(
                        styling::SUCCESS_INDICATOR,
                        &format!("Saved command output to {}", path.display())
                    )
                );
            }
            Some(output)
        }
        Ok(Ok(_)) => {
            println!(
                "{}",
                styling::format_error_msg(
                    styling::ERROR_INDICATOR,
                    "Received unexpected response type"
                )
            );
            Some("Received unexpected response type".to_string())
        }
        Ok(Err(e)) => {
            println!(
                "{}",
                styling::format_error_msg(
                    styling::ERROR_INDICATOR,
                    &format!("Failed to receive command response: {}", e)
                )
            );
            Some(format!("Failed to receive command response: {}", e))
        }
        Err(_) => {
            println!(
                "{}",
                styling::format_error_msg(
                    styling::ERROR_INDICATOR,
                    "Timed out waiting for command response"
                )
            );
            Some("Timed out waiting for command response".to_string())
        }
    }
}

async fn execute_remote_shell_message(
    _agent_name: &str,
    agent_sender: &mpsc::Sender<Message>,
    command_response: &Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<Message>>>>,
    outbound: Message,
    timeout: Duration,
    transcript: &PathBuf,
) -> Option<String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    {
        let mut pending = command_response.lock().await;
        *pending = Some(tx);
    }

    if let Err(e) = agent_sender.send(outbound).await {
        let msg = format!("Failed to send shell command: {}", e);
        println!(
            "{}",
            styling::format_error_msg(styling::ERROR_INDICATOR, &msg)
        );
        append_shell_transcript(transcript, &format!("! {}", msg));
        return Some(msg);
    }

    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(Message::CommandResponse { output, error })) => {
            if let Some(err) = error {
                if !err.trim().is_empty() {
                    println!("{}", err.red());
                    append_shell_transcript(transcript, &format!("! {}", err));
                    return Some(err);
                }
            }
            if !output.trim().is_empty() {
                println!("{}", output);
                return Some(output);
            }
            Some(String::new())
        }
        Ok(Ok(Message::FileUploadResponse { success, message })) => {
            let line = if success {
                format!("Upload succeeded: {}", message)
            } else {
                format!("Upload failed: {}", message)
            };
            if success {
                println!("{}", line.green());
            } else {
                println!("{}", line.red());
            }
            Some(line)
        }
        Ok(Ok(_)) => {
            let msg = "Received unexpected response type".to_string();
            println!(
                "{}",
                styling::format_error_msg(styling::ERROR_INDICATOR, &msg)
            );
            Some(msg)
        }
        Ok(Err(e)) => {
            let msg = format!("Failed to receive shell response: {}", e);
            println!(
                "{}",
                styling::format_error_msg(styling::ERROR_INDICATOR, &msg)
            );
            Some(msg)
        }
        Err(_) => {
            let msg = "Shell command timed out".to_string();
            println!(
                "{}",
                styling::format_error_msg(styling::ERROR_INDICATOR, &msg)
            );
            Some(msg)
        }
    }
}

fn create_shell_transcript(agent_name: &str) -> Result<PathBuf> {
    let dir = PathBuf::from("shell_sessions");
    fs::create_dir_all(&dir)
        .map_err(|e| LabyrinthError::Message(format!("Failed to create shell_sessions: {}", e)))?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("{}_{}.log", sanitize_filename(agent_name), ts));
    let header = format!("Shell transcript for {}\nStarted: {}\n\n", agent_name, ts);
    fs::write(&path, header)
        .map_err(|e| LabyrinthError::Message(format!("Failed to create transcript file: {}", e)))?;
    Ok(path)
}

fn append_shell_transcript(path: &PathBuf, line: &str) {
    if let Ok(mut f) = fs::OpenOptions::new().append(true).open(path) {
        let _ = writeln!(f, "{}", line);
    }
}

fn command_timeout_for_token(token: &str) -> Duration {
    if token.contains("autoenum") {
        // linpeas/winpeas can take significant time on large hosts.
        Duration::from_secs(20 * 60)
    } else {
        Duration::from_secs(120)
    }
}

fn persist_command_output(
    agent_name: &str,
    display: &str,
    token: &str,
    output: &str,
    error: Option<&str>,
) -> Result<PathBuf> {
    let dir = PathBuf::from("command_outputs");
    fs::create_dir_all(&dir)
        .map_err(|e| LabyrinthError::Message(format!("Failed to create command_outputs: {}", e)))?;

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let file_name = format!(
        "{}_{}_{}.log",
        sanitize_filename(agent_name),
        sanitize_filename(token),
        ts
    );
    let path = dir.join(file_name);

    let mut body = String::new();
    body.push_str(&format!("Agent: {}\n", agent_name));
    body.push_str(&format!("Command label: {}\n", display));
    body.push_str(&format!("Command token: {}\n", token));
    body.push_str(&format!("Timestamp (unix): {}\n\n", ts));
    if let Some(err) = error {
        body.push_str("Error:\n");
        body.push_str(err);
        body.push_str("\n\n");
    }
    body.push_str("Output:\n");
    body.push_str(output);

    fs::write(&path, body)
        .map_err(|e| LabyrinthError::Message(format!("Failed to write command log: {}", e)))?;

    Ok(path)
}

fn sanitize_filename(input: &str) -> String {
    input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
}

fn decorate_command_output(raw: &str) -> String {
    let mut rendered = Vec::new();

    for line in raw.lines() {
        let styled = if line.starts_with("=== ") && line.ends_with(" ===") {
            line.cyan().bold().to_string()
        } else if let Some(rest) = line.strip_prefix("OS: ") {
            format!("{} {}", "OS:".blue().bold(), rest.bright_white())
        } else if let Some(rest) = line.strip_prefix("Summary: ") {
            format!("{} {}", "Summary:".green().bold(), rest.white())
        } else if line == "Failures:" {
            line.red().bold().to_string()
        } else if line == "Details:" {
            line.yellow().bold().to_string()
        } else if line.starts_with("[OK]") {
            line.green().bold().to_string()
        } else if line.starts_with("[FAIL]") {
            line.red().bold().to_string()
        } else if let Some(rest) = line.strip_prefix("Command: ") {
            format!("{} {}", "Command:".magenta().bold(), rest.bright_white())
        } else if line == "Output:" {
            line.cyan().bold().to_string()
        } else if line == "Error:" {
            line.red().bold().to_string()
        } else if let Some(rest) = line.strip_prefix("Source: ") {
            format!("{} {}", "Source:".blue().bold(), rest.bright_white())
        } else if let Some(rest) = line.strip_prefix("Remote output file: ") {
            format!(
                "{} {}",
                "Remote output file:".yellow().bold(),
                rest.bright_white()
            )
        } else if line.starts_with("- ") {
            line.red().to_string()
        } else {
            line.to_string()
        };

        rendered.push(styled);
    }

    rendered.join("\n")
}

pub async fn run_interactive_server(
    listen_addr: &str,
    no_auth: bool,
    domain: Option<String>,
    transport: TransportMode,
    web_ui_enabled: bool,
    web_ui_addr: &str,
) -> Result<()> {
    // Load or generate certificates
    let (certs, key, _cert_pem) = CertificateManager::load_or_generate_cert(domain)?;

    let auth_key = resolve_auth_key(no_auth)?;

    // Create server instance
    let server = Arc::new(LabyrinthServer::new(!no_auth, auth_key));
    DwellerManager::load_registry(&server).await?;

    println!(
        "{} Server started on {} ({})",
        styling::format_success_msg(styling::SUCCESS_INDICATOR, ""),
        listen_addr.cyan(),
        transport.label().cyan()
    );

    if web_ui_enabled {
        if let Err(e) = DashboardServer::spawn(Arc::clone(&server), web_ui_addr).await {
            println!(
                "{}",
                styling::format_warning_msg(
                    styling::WARNING_INDICATOR,
                    &format!("Web UI unavailable on {}: {}", web_ui_addr, e)
                )
            );
        }
    }

    // Display copy-friendly fingerprint for easy agent connection
    if let Ok(cert_pem) = std::fs::read_to_string("cert.pem") {
        if let Ok(fingerprint) = CertificateManager::get_fingerprint_from_pem(&cert_pem) {
            println!();
            println!(
                "{}",
                styling::format_success_msg(
                    styling::SUCCESS_INDICATOR,
                    "Certificate fingerprint for agent connections:"
                )
            );
            println!("  {}", fingerprint.green().bold());
            println!();
        }
    }

    // Check and warn about sudo privileges
    PrivilegeManager::check_and_warn_privileges();

    spawn_agent_listener(Arc::clone(&server), listen_addr, transport, certs, key).await?;

    // Run CLI
    run_cli(server).await
}

// Headless server mode - runs without interactive CLI
pub async fn run_headless_server(
    listen_addr: &str,
    no_auth: bool,
    domain: Option<String>,
    transport: TransportMode,
    web_ui_enabled: bool,
    web_ui_addr: &str,
) -> Result<()> {
    // Load or generate certificates
    let (certs, key, _cert_pem) = CertificateManager::load_or_generate_cert(domain)?;

    let auth_key = resolve_auth_key(no_auth)?;

    // Create server instance
    let server = Arc::new(LabyrinthServer::new(!no_auth, auth_key));
    DwellerManager::load_registry(&server).await?;

    if web_ui_enabled {
        if let Err(e) = DashboardServer::spawn(Arc::clone(&server), web_ui_addr).await {
            warn!("Web UI unavailable on {}: {}", web_ui_addr, e);
        }
    }

    spawn_agent_listener(Arc::clone(&server), listen_addr, transport, certs, key).await?;
    info!(
        "Headless server listening on {} ({})",
        listen_addr,
        transport.label()
    );

    std::future::pending::<()>().await;
    Ok(())
}
