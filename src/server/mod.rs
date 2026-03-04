pub mod agent_connection;
pub mod agent_manager;
pub mod certificate;
pub mod core;
pub mod privileges;
pub mod reverse_port_forward;
pub mod tunnel_manager;
pub mod netstack_bridge;
pub mod ui;

use crate::error::{LabyrinthError, Result};
use crate::protocol::Message;
use crate::server::agent_manager::AgentManager;
use crate::server::certificate::CertificateManager;
use crate::server::core::LabyrinthServer;
use crate::server::privileges::PrivilegeManager;

use crate::server::tunnel_manager::TunnelManager;
use crate::server::ui::ServerUI;
use crate::styling;
use colored::Colorize;
use dialoguer::{Input, Select};
use rustyline::Editor;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tracing::{error, info, warn};
use tokio::time::Duration;
use tokio::sync::mpsc;

use crate::streaming::{
    connection_manager::ServerConnectionManager,
    stream_manager::BidirectionalStreamManager,
    models::{ConnectionId, PortMapping, StreamMessage},
    traits::{ConnectionManager as StreamConnectionManager, StreamManager as StreamManagerTrait},
    MetricsCollector, ErrorRecoveryCoordinator,
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
    let mut rl = Editor::<(), rustyline::history::DefaultHistory>::new()
        .map_err(|e| crate::error::LabyrinthError::Message(format!("Failed to create readline: {}", e)))?;

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

                rl.add_history_entry(line)
                    .map_err(|e| crate::error::LabyrinthError::Message(format!("History error: {}", e)))?;

                match line {
                    "help" | "h" => {
                        println!("\n{}", styling::format_header("Available Commands"));
                        println!("{}", styling::format_separator(styling::SECTION_SEPARATOR));
                        println!("  {}  List connected agents", "agents".cyan());
                        println!("  {}  Select an agent for operations", "select".cyan());
                        println!("  {}  Show detailed agent information", "info".cyan());
                        println!("  {}  Start Tunnel", "Fullhouse".cyan());
                        println!("  {}  Port Forwarding", "Room".cyan());
                        println!("  {}  Stop active tunnel/forwarding", "stop".cyan());
                        println!("  {}  Execute system commands on agent", "commands".cyan());
                        println!("  {}  Show server status", "status".cyan());
                        println!("  {}  Show certificate information", "cert".cyan());
                        println!("  {}  Show this help message", "help".cyan());
                        println!("  {}  Exit the server", "exit".cyan());
                        println!();
                    }
                    "agents" | "list" | "ls" => {
                        ServerUI::list_agents(&server).await;
                    }
                    "select" => {
                        if let Err(e) = ServerUI::select_agent(&server).await {
                            println!("{}", styling::format_error_msg(styling::ERROR_INDICATOR, &format!("Selection failed: {}", e)));
                        }
                    }
                    "info" | "show" => {
                        if let Err(e) = ServerUI::show_agent_info(&server).await {
                            println!("{}", styling::format_error_msg(styling::ERROR_INDICATOR, &format!("Info display failed: {}", e)));
                        }
                    }
                    "tunnel" | "fullhouse" | "Fullhouse" => {
                        if let Err(e) = TunnelManager::start_tunnel(&server).await {
                            println!("{}", styling::format_error_msg(styling::ERROR_INDICATOR, &format!("Tunnel start failed: {}", e)));
                        }
                    }
                    "stop" => {
                        if let Err(e) = TunnelManager::stop_tunnel(&server).await {
                            println!("{}", styling::format_error_msg(styling::ERROR_INDICATOR, &format!("Stop failed: {}", e)));
                        }
                    }
                    "forward" | "room" | "Room" => {
                        if let Err(e) = start_port_forwarding(server.clone()).await {
                            println!("{}", styling::format_error_msg(styling::ERROR_INDICATOR, &format!("Port forwarding failed: {}", e)));
                        }
                    }
                    "commands" | "cmd" => {
                        if let Err(e) = start_commands_mode(&server).await {
                            println!("{}", styling::format_error_msg(styling::ERROR_INDICATOR, &format!("Commands failed: {}", e)));
                        }
                    }
                    "status" => {
                        ServerUI::show_status(&server).await;
                    }
                    "cert" | "certificate" => {
                        if let Err(e) = CertificateManager::show_certificate_info() {
                            println!("{}", styling::format_error_msg(styling::ERROR_INDICATOR, &format!("Certificate info failed: {}", e)));
                        }
                    }
                    "done" => {
                        println!("{}", styling::format_success_msg(styling::SUCCESS_INDICATOR, "Operation completed"));
                    }
                    "exit" | "quit" | "q" => {
                        println!("{}", styling::format_success_msg(styling::SUCCESS_INDICATOR, "Goodbye!"));
                        break;
                    }
                    _ => {
                        println!("{}", styling::format_warning_msg(styling::WARNING_INDICATOR, &format!("Unknown command: '{}'. Type 'help' for available commands.", line)));
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
        println!("\n{}", "Room Mode (Port Forwarding)".cyan().bold());
        println!("{}", "──────────────────────────".bright_black());
        println!();

        let mappings: Vec<String> = loop {
            let input: String = Input::new()
                .with_prompt("Port mappings (format: local_port:target_host:target_port, comma-separated)")
                .interact_text()
                .map_err(|e| LabyrinthError::Message(format!("Input error: {}", e)))?;

            let mappings: Vec<String> = input.split(',').map(|s| s.trim().to_string()).collect();
            if !mappings.is_empty() && mappings.iter().all(|m| validate_port_mapping(m)) {
                for mapping in &mappings {
                    println!(
                        "{}{}",
                        styling::INDENT_LEVEL_1,
                        styling::format_check_item(&format!("Valid mapping: {}", styling::format_agent_name(mapping)))
                    );
                }
                break mappings;
            }

            println!("{}Format: local_port:target_host:target_port", styling::INDENT_LEVEL_1);
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
                return Err(LabyrinthError::Message("Selected agent not found".to_string()));
            }
        };

        if server.get_stream_manager().await.is_none() || server.get_connection_manager().await.is_none() {
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
                    error!("Streaming port forward listener error on {}: {}", local_port, e);
                }
                server_for_task.unregister_port_forward_listener(local_port).await;
            });

            match server
                .register_port_forward_listener(local_port, agent_id.clone(), handle)
                .await
            {
                Ok(()) => {
                    successful_mappings.push(mapping.clone());
                    println!(
                        "{}{}",
                        styling::INDENT_LEVEL_1,
                        styling::format_check_item(&format!("Started: {}", styling::format_agent_name(mapping)))
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
            agent.tunnel_subnet = Some(format!("Port forwarding: {}", successful_mappings.join(", ")));
        }
        drop(agents);

        println!(
            "\n{} Room Mode Active",
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
                metrics.update_performance_metrics(0, 0.0, 0, HashMap::new()).await;
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
                    "Streaming Room: client {} connected on {}",
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
        assert!(stream_message_connection_id(&StreamMessage::StreamClose("legacy".into())).is_none());
    }

    #[test]
    fn resolve_auth_key_behavior() {
        env::remove_var("LABYRINTH_AUTH_KEY");
        assert!(resolve_auth_key(false).is_err());

        env::set_var("LABYRINTH_AUTH_KEY", "secret");
        assert_eq!(
            resolve_auth_key(false).unwrap(),
            Some("secret".to_string())
        );

        env::remove_var("LABYRINTH_AUTH_KEY");
        assert_eq!(resolve_auth_key(true).unwrap(), None);
    }
}


async fn start_commands_mode(server: &LabyrinthServer) -> Result<()> {
    let current_id = server.current_agent().read().await.clone();
    if let Some(agent_id) = current_id {
        let agents = server.agents().read().await;
        if let Some(agent) = agents.get(&agent_id) {
            // Display Commands Mode header
            println!("\n{}", "Commands Mode".cyan().bold());
            println!("{}", "─────────────".bright_black());
            
            // Detect OS and show appropriate message
            let os_name = &agent.info.os;
            let available_commands = match os_name.to_lowercase().as_str() {
                s if s.contains("linux") => {
                    println!("{} Linux system identified", styling::format_success_msg(styling::SUCCESS_INDICATOR, "").trim_start());
                    vec!["ifconfig", "ss -tunlp"]
                }
                s if s.contains("windows") => {
                    println!("{} Windows system identified", styling::format_success_msg(styling::SUCCESS_INDICATOR, "").trim_start());
                    vec!["ipconfig", "netstat -aon"]
                }
                _ => {
                    println!("{} Unknown operating system: {}", styling::format_warning_msg(styling::WARNING_INDICATOR, "").trim_start(), os_name);
                    println!("{}No commands available for this OS", styling::INDENT_LEVEL_1);
                    return Ok(());
                }
            };
            
            println!("\nAvailable commands:");
            for (i, cmd) in available_commands.iter().enumerate() {
                println!("  {}. {}", i + 1, cmd.cyan());
            }
            println!("  {}. {}", available_commands.len() + 1, "Back".cyan());
            
            // Let user select a command
            let selection = Select::new()
                .with_prompt("Select a command to execute")
                .items(&{
                    let mut items = available_commands.clone();
                    items.push("Back");
                    items
                })
                .interact()
                .map_err(|e| crate::error::LabyrinthError::Message(format!("Selection error: {}", e)))?;
            
            if selection < available_commands.len() {
                let selected_command = available_commands[selection];
                println!("\n{} Executing command: {}", styling::format_success_msg(styling::SUCCESS_INDICATOR, "").trim_start(), selected_command.cyan().bold());
                
                // Create a channel to receive the command response
                let (tx, rx) = tokio::sync::oneshot::channel();
                
                // Store the sender in the agent's command_response field
                {
                    let mut command_response = agent.command_response.lock().await;
                    *command_response = Some(tx);
                }

                // Send command request to agent
                let command_msg = Message::CommandRequest {
                    command: selected_command.to_string(),
                };
                
                if let Err(e) = agent.sender.send(command_msg).await {
                    error!("Failed to send command to agent {}: {}", agent.id, e);
                    println!("{}", styling::format_error_msg(styling::ERROR_INDICATOR, "Failed to send command to agent."));
                    return Ok(());
                }
                
                println!("\n{} Command sent to agent. Waiting for response...", styling::format_success_msg(styling::SUCCESS_INDICATOR, "").trim_start());

                // Wait for the response from the agent
                match rx.await {
                    Ok(Message::CommandResponse { output, error }) => {
                        if let Some(error_msg) = error {
                            println!("\n{}", error_msg);
                        } else {
                            println!("\n{}", output);
                        }
                    }
                    Ok(_) => {
                        println!("{}", styling::format_error_msg(styling::ERROR_INDICATOR, "Received unexpected response from agent"));
                    }
                    Err(e) => {
                        println!("{}", styling::format_error_msg(styling::ERROR_INDICATOR, &format!("Failed to receive command response: {}", e)));
                    }
                }
            }
            // If "Back" was selected, just return without doing anything
            
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

pub async fn run_interactive_server(listen_addr: &str, no_auth: bool, domain: Option<String>) -> Result<()> {
    // Load or generate certificates
    let (certs, key, _cert_pem) = CertificateManager::load_or_generate_cert(domain)?;

    // Create TLS acceptor
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    let acceptor = TlsAcceptor::from(Arc::new(config));

    let auth_key = resolve_auth_key(no_auth)?;

    // Create server instance
    let server = Arc::new(LabyrinthServer::new(!no_auth, auth_key));

    // Start listening for connections
    let listener = TcpListener::bind(listen_addr).await?;
    info!("Server listening on {}", listen_addr);

    println!(
        "{} Server started on {}",
        styling::format_success_msg(styling::SUCCESS_INDICATOR, ""),
        listen_addr.cyan()
    );

    // Display copy-friendly fingerprint for easy agent connection
    if let Ok(cert_pem) = std::fs::read_to_string("cert.pem") {
        if let Ok(fingerprint) = CertificateManager::get_fingerprint_from_pem(&cert_pem) {
            println!();
            println!("{}", styling::format_success_msg(styling::SUCCESS_INDICATOR, "Certificate fingerprint for agent connections:"));
            println!("  {}", fingerprint.green().bold());
            println!();
        }
    }

    // Check and warn about sudo privileges
    PrivilegeManager::check_and_warn_privileges();

    // Clone server for the connection handler
    let server_clone = Arc::clone(&server);
    let acceptor_clone = acceptor.clone();

    // Spawn connection handler
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    let acceptor = acceptor_clone.clone();
                    let server = Arc::clone(&server_clone);
                    
                    tokio::spawn(async move {
                        match acceptor.accept(stream).await {
                            Ok(tls_stream) => {
                                if let Err(e) = AgentManager::register_agent(Arc::clone(&server), tls_stream, addr).await {
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
                    error!("Failed to accept connection: {}", e);
                }
            }
        }
    });

    // Run CLI
    run_cli(server).await
}

// Headless server mode - runs without interactive CLI
pub async fn run_headless_server(
    listen_addr: &str,
    no_auth: bool,
    _interface: Option<String>,
    _route: Option<String>,
    domain: Option<String>,
) -> Result<()> {
    // Load or generate certificates
    let (certs, key, _cert_pem) = CertificateManager::load_or_generate_cert(domain)?;

    // Create TLS acceptor
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    let acceptor = TlsAcceptor::from(Arc::new(config));

    let auth_key = resolve_auth_key(no_auth)?;

    // Create server instance
    let server = Arc::new(LabyrinthServer::new(!no_auth, auth_key));

    // Start listening for connections
    let listener = TcpListener::bind(listen_addr).await?;
    info!("Headless server listening on {}", listen_addr);

    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                let acceptor = acceptor.clone();
                let server = Arc::clone(&server);
                
                tokio::spawn(async move {
                    match acceptor.accept(stream).await {
                        Ok(tls_stream) => {
                            if let Err(e) = AgentManager::register_agent(Arc::clone(&server), tls_stream, addr).await {
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
                error!("Failed to accept connection: {}", e);
            }
        }
    }
}
