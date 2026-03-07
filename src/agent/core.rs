use crate::agent::connection::ConnectionManager;
// reverse_port_forward: background response channel utilities

use crate::agent::command_executor::{CommandExecutor, OSDetector};
use crate::agent::pty_shell::PtyShellManager;
use crate::agent::system_info::SystemInfoCollector;
use crate::error::{LabyrinthError, Result};
use crate::protocol::Message;

use crate::streaming::models::{CloseReason, ConnectionId, DataDirection, StreamMessage};
use crate::styling;
use base64::{engine::general_purpose, Engine as _};
use bytes::Bytes;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};

/// Single Responsibility: Main agent logic and message handling
pub struct AgentCore;

impl AgentCore {
    pub async fn run_agent(
        server_addr: &str,
        server_cert_b64: Option<String>,
        accept_fingerprint: Option<String>,
        proxy: Option<String>,
        retry: bool,
    ) -> Result<()> {
        info!("{} Starting Labyrinth agent...", styling::SUCCESS_INDICATOR);

        // Get system information
        let agent_info = SystemInfoCollector::get_system_info();
        info!(
            "{} Agent info: {} on {}/{}",
            styling::SUCCESS_INDICATOR,
            agent_info.name,
            agent_info.os,
            agent_info.arch
        );

        loop {
            // Establish TLS connection to server
            let mut tls_stream = match ConnectionManager::establish_tls_connection_with_retry(
                server_addr,
                server_cert_b64.clone(),
                accept_fingerprint.clone(),
                proxy.clone(),
                retry,
            )
            .await
            {
                Ok(stream) => stream,
                Err(e) => {
                    if retry {
                        sleep(Duration::from_secs(5)).await;
                        continue;
                    } else {
                        return Err(e);
                    }
                }
            };

            // Send agent registration
            let register_msg = Message::AgentRegister(agent_info.clone());
            let msg_str = serde_json::to_string(&register_msg)?;

            if let Err(e) = tls_stream.write_all(msg_str.as_bytes()).await {
                error!(
                    "{} Failed to send registration: {}",
                    styling::ERROR_INDICATOR,
                    e
                );
                if retry {
                    sleep(Duration::from_secs(5)).await;
                    continue;
                } else {
                    return Err(LabyrinthError::Io(e));
                }
            }

            if let Err(e) = tls_stream.write_all(b"\n").await {
                error!(
                    "{} Failed to send delimiter: {}",
                    styling::ERROR_INDICATOR,
                    e
                );
                if retry {
                    sleep(Duration::from_secs(5)).await;
                    continue;
                } else {
                    return Err(LabyrinthError::Io(e));
                }
            }

            // Wait for acknowledgment
            let mut buf = Vec::new();
            let mut reader = tokio::io::BufReader::new(&mut tls_stream);
            match reader.read_until(b'\n', &mut buf).await {
                Ok(_) => {
                    let response: Message = match serde_json::from_slice(&buf[..buf.len() - 1]) {
                        Ok(msg) => msg,
                        Err(e) => {
                            error!(
                                "{} Failed to parse server response: {}",
                                styling::ERROR_INDICATOR,
                                e
                            );
                            if retry {
                                sleep(Duration::from_secs(5)).await;
                                continue;
                            } else {
                                return Err(LabyrinthError::Json(e));
                            }
                        }
                    };

                    match response {
                        Message::AgentAck => {
                            info!(
                                "{} Successfully registered with server",
                                styling::SUCCESS_INDICATOR
                            );
                        }
                        _ => {
                            error!(
                                "{} Unexpected response from server: {:?}",
                                styling::ERROR_INDICATOR,
                                response
                            );
                            if retry {
                                sleep(Duration::from_secs(5)).await;
                                continue;
                            } else {
                                return Err(LabyrinthError::Message(
                                    "Unexpected server response".to_string(),
                                ));
                            }
                        }
                    }
                }
                Err(e) => {
                    error!(
                        "{} Failed to read server response: {}",
                        styling::ERROR_INDICATOR,
                        e
                    );
                    if retry {
                        sleep(Duration::from_secs(5)).await;
                        continue;
                    } else {
                        return Err(LabyrinthError::Io(e));
                    }
                }
            }

            // Split the stream for concurrent reading and writing
            let (tls_reader, mut tls_writer) = tokio::io::split(tls_stream);
            let mut reader = tokio::io::BufReader::new(tls_reader);

            // Main message loop - keep connection alive and handle server commands
            info!(
                "{} Agent connected and ready for commands",
                styling::SUCCESS_INDICATOR
            );

            // Get the global response channel for processing background responses
            use crate::agent::reverse_port_forward::get_response_channel;
            let (_, response_receiver) = get_response_channel();

            loop {
                let mut buf = Vec::new();
                let mut response_receiver_guard = response_receiver.lock().await;

                tokio::select! {
                    // Handle incoming messages from server
                    read_result = reader.read_until(b'\n', &mut buf) => {
                        match read_result {
                            Ok(0) => {
                                warn!("{} Server closed connection", styling::WARNING_INDICATOR);
                                break;
                            }
                            Ok(_) => {
                                let message: Message = match serde_json::from_slice(&buf[..buf.len()-1]) {
                                    Ok(msg) => msg,
                                    Err(e) => {
                                        error!("{} Failed to parse message: {}", styling::ERROR_INDICATOR, e);
                                        continue;
                                    }
                                };

                                drop(response_receiver_guard); // Release the lock before handling message
                                if let Err(e) = Self::handle_message(message, &mut tls_writer).await {
                                    error!("{} Failed to handle message: {}", styling::ERROR_INDICATOR, e);
                                    break;
                                }
                            }
                            Err(e) => {
                                error!("{} Failed to read from server: {}", styling::ERROR_INDICATOR, e);
                                break;
                            }
                        }
                    }

                    // Handle outgoing responses from background tasks
                    response = response_receiver_guard.recv() => {
                        if let Some(response_msg) = response {
                            info!("{} Processing background response: {:?}", styling::SUCCESS_INDICATOR, std::mem::discriminant(&response_msg));

                            // Send the response back to the server
                            if let Ok(response_str) = serde_json::to_string(&response_msg) {
                                if let Err(e) = tls_writer.write_all(response_str.as_bytes()).await {
                                    error!("{} Failed to send background response to server: {}", styling::ERROR_INDICATOR, e);
                                    break;
                                } else if let Err(e) = tls_writer.write_all(b"\n").await {
                                    error!("{} Failed to send background response delimiter: {}", styling::ERROR_INDICATOR, e);
                                    break;
                                } else {
                                    info!("{} Successfully sent background response to server", styling::SUCCESS_INDICATOR);
                                }
                            } else {
                                error!("{} Failed to serialize background response", styling::ERROR_INDICATOR);
                            }
                        }
                    }
                }
            }

            if !retry {
                break;
            }

            warn!(
                "{} Connection lost, retrying in 5 seconds...",
                styling::WARNING_INDICATOR
            );
            sleep(Duration::from_secs(5)).await;
        }

        Ok(())
    }

    async fn handle_message(
        message: Message,
        tls_writer: &mut tokio::io::WriteHalf<
            tokio_rustls::client::TlsStream<Box<dyn crate::agent::connection::AsyncReadWrite>>,
        >,
    ) -> Result<()> {
        match message {
            Message::StartTunnel { subnet, tun_name } => {
                info!(
                    "{} Server requested tunnel start for subnet: {}",
                    styling::SUCCESS_INDICATOR,
                    subnet
                );
                // Agent remains unprivileged; server owns TUN and stack. Just ACK.
                let ack_msg = Message::TunnelStarted;
                let ack_str = serde_json::to_string(&ack_msg)?;
                tls_writer.write_all(ack_str.as_bytes()).await?;
                tls_writer.write_all(b"\n").await?;
                info!(
                    "{} Tunnel acknowledged for subnet {} (server-side TUN: {})",
                    styling::SUCCESS_INDICATOR,
                    subnet,
                    tun_name
                );
            }
            Message::StopTunnel => {
                info!(
                    "{} Server requested tunnel stop",
                    styling::SUCCESS_INDICATOR
                );

                // Acknowledge tunnel stop
                let ack_msg = Message::TunnelStopped;
                let ack_str = serde_json::to_string(&ack_msg)?;

                tls_writer.write_all(ack_str.as_bytes()).await?;
                tls_writer.write_all(b"\n").await?;

                info!("{} Tunnel stopped", styling::SUCCESS_INDICATOR);
            }
            // DataPacket is server-only in ligolo-like design; agent ignores
            Message::Ping => {
                // Respond to ping
                let pong_msg = Message::Pong;
                let pong_str = serde_json::to_string(&pong_msg)?;

                tls_writer.write_all(pong_str.as_bytes()).await?;
                tls_writer.write_all(b"\n").await?;
            }
            Message::RoomPortForward {
                local_port,
                target_addr,
                auth_key: _,
            } => {
                info!(
                    "{} Server requested port forwarding: {} -> {}",
                    styling::SUCCESS_INDICATOR,
                    local_port,
                    target_addr
                );

                // Start port forwarding in the background
                let target_addr_clone = target_addr.clone();
                tokio::spawn(async move {
                    if let Err(e) = Self::start_port_forward(local_port, &target_addr_clone).await {
                        error!("{} Port forwarding failed: {}", styling::ERROR_INDICATOR, e);
                    } else {
                        info!(
                            "{} Port forwarding active: {} -> {}",
                            styling::SUCCESS_INDICATOR,
                            local_port,
                            target_addr_clone
                        );
                    }
                });
            }

            Message::CommandRequest { command } => {
                info!(
                    "{} Server requested command execution: {}",
                    styling::SUCCESS_INDICATOR,
                    command
                );

                // Detect OS and create appropriate command executor
                let os = OSDetector::detect_os();
                let executor = CommandExecutor::new(&os);

                // Execute command and send response
                let response = match executor.execute_command(&command).await {
                    Ok(output) => Message::CommandResponse {
                        output,
                        error: None,
                    },
                    Err(e) => Message::CommandResponse {
                        output: String::new(),
                        error: Some(e.to_string()),
                    },
                };

                let response_str = serde_json::to_string(&response)?;
                tls_writer.write_all(response_str.as_bytes()).await?;
                tls_writer.write_all(b"\n").await?;

                info!(
                    "{} Command execution completed: {}",
                    styling::SUCCESS_INDICATOR,
                    command
                );
            }
            Message::FileUpload {
                remote_path,
                content_b64,
            } => {
                let response = match general_purpose::STANDARD.decode(content_b64.as_bytes()) {
                    Ok(content) => {
                        let path = std::path::Path::new(&remote_path);
                        let parent_result = if let Some(parent) = path.parent() {
                            tokio::fs::create_dir_all(parent).await
                        } else {
                            Ok(())
                        };

                        match parent_result {
                            Ok(()) => match tokio::fs::write(path, &content).await {
                                Ok(()) => Message::FileUploadResponse {
                                    success: true,
                                    message: format!(
                                        "Uploaded {} bytes to {}",
                                        content.len(),
                                        remote_path
                                    ),
                                },
                                Err(e) => Message::FileUploadResponse {
                                    success: false,
                                    message: format!("Failed to write {}: {}", remote_path, e),
                                },
                            },
                            Err(e) => Message::FileUploadResponse {
                                success: false,
                                message: format!("Failed to create parent directories: {}", e),
                            },
                        }
                    }
                    Err(e) => Message::FileUploadResponse {
                        success: false,
                        message: format!("Invalid base64 content: {}", e),
                    },
                };

                let response_str = serde_json::to_string(&response)?;
                tls_writer.write_all(response_str.as_bytes()).await?;
                tls_writer.write_all(b"\n").await?;
            }
            Message::FileDownloadRequest { remote_path } => {
                let response = match tokio::fs::read(&remote_path).await {
                    Ok(content) => Message::FileDownloadResponse {
                        success: true,
                        message: format!("Read {} bytes from {}", content.len(), remote_path),
                        remote_path,
                        content_b64: Some(general_purpose::STANDARD.encode(content)),
                    },
                    Err(e) => Message::FileDownloadResponse {
                        success: false,
                        message: format!("Failed to read file: {}", e),
                        remote_path,
                        content_b64: None,
                    },
                };

                let response_str = serde_json::to_string(&response)?;
                tls_writer.write_all(response_str.as_bytes()).await?;
                tls_writer.write_all(b"\n").await?;
            }
            Message::ShellSessionStart {
                session_id,
                cols,
                rows,
            } => {
                if let Err(e) = PtyShellManager::start_session(session_id.clone(), cols, rows).await
                {
                    let response = Message::ShellSessionStarted {
                        session_id,
                        success: false,
                        message: e.to_string(),
                    };
                    let response_str = serde_json::to_string(&response)?;
                    tls_writer.write_all(response_str.as_bytes()).await?;
                    tls_writer.write_all(b"\n").await?;
                }
            }
            Message::ShellSessionInput {
                session_id,
                data_b64,
            } => {
                if let Err(e) = PtyShellManager::send_input(&session_id, &data_b64).await {
                    let response = Message::ShellSessionOutput {
                        session_id,
                        data_b64: general_purpose::STANDARD
                            .encode(format!("\n[labyrinth shell error] {}\n", e)),
                    };
                    let response_str = serde_json::to_string(&response)?;
                    tls_writer.write_all(response_str.as_bytes()).await?;
                    tls_writer.write_all(b"\n").await?;
                }
            }
            Message::ShellSessionResize {
                session_id,
                cols,
                rows,
            } => {
                if let Err(e) = PtyShellManager::resize_session(&session_id, cols, rows).await {
                    warn!(
                        "{} Failed to resize shell session {}: {}",
                        styling::WARNING_INDICATOR,
                        session_id,
                        e
                    );
                }
            }
            Message::ShellSessionClose { session_id } => {
                if let Err(e) = PtyShellManager::close_session(&session_id).await {
                    warn!(
                        "{} Failed to close shell session {}: {}",
                        styling::WARNING_INDICATOR,
                        session_id,
                        e
                    );
                }
            }
            // New reverse port forwarding message handlers
            Message::ReversePortForwardSetup {
                connection_id,
                local_port,
                target_host,
                target_port,
            } => {
                // Reverse port forwarding via agent-side manager is not active in this build.
                // The server currently handles legacy mode locally.
                warn!(
                    "{} Ignoring reverse port forward setup: id={} {} -> {}:{}",
                    styling::WARNING_INDICATOR,
                    connection_id,
                    local_port,
                    target_host,
                    target_port
                );
            }
            Message::Stream(stream_message) => {
                if let Err(e) = Self::handle_stream_message(stream_message).await {
                    error!(
                        "{} Failed to handle stream message: {}",
                        styling::ERROR_INDICATOR,
                        e
                    );
                }
            }
            _ => {
                warn!(
                    "{} Received unexpected message: {:?}",
                    styling::WARNING_INDICATOR,
                    message
                );
            }
        }
        Ok(())
    }

    async fn start_port_forward(local_port: u16, target_addr: &str) -> Result<()> {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", local_port))
            .await
            .map_err(LabyrinthError::Io)?;

        info!(
            "{} Port forwarding listening on 0.0.0.0:{}",
            styling::SUCCESS_INDICATOR,
            local_port
        );

        loop {
            match listener.accept().await {
                Ok((mut inbound, client_addr)) => {
                    let target_addr = target_addr.to_string();
                    tokio::spawn(async move {
                        match TcpStream::connect(&target_addr).await {
                            Ok(mut outbound) => {
                                info!(
                                    "{} Forwarding connection from {} to {}",
                                    styling::SUCCESS_INDICATOR,
                                    client_addr,
                                    target_addr
                                );
                                if let Err(e) =
                                    tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await
                                {
                                    error!(
                                        "{} Port forwarding error: {}",
                                        styling::ERROR_INDICATOR,
                                        e
                                    );
                                }
                            }
                            Err(e) => {
                                error!(
                                    "{} Failed to connect to target {}: {}",
                                    styling::ERROR_INDICATOR,
                                    target_addr,
                                    e
                                );
                            }
                        }
                    });
                }
                Err(e) => {
                    error!(
                        "{} Failed to accept connection: {}",
                        styling::ERROR_INDICATOR,
                        e
                    );
                    break;
                }
            }
        }

        Ok(())
    }

    // Simple in-memory map of active target writers for streaming connections
    fn stream_writers() -> &'static tokio::sync::RwLock<
        std::collections::HashMap<ConnectionId, Arc<tokio::sync::Mutex<OwnedWriteHalf>>>,
    > {
        static WRITERS: std::sync::OnceLock<
            tokio::sync::RwLock<
                std::collections::HashMap<ConnectionId, Arc<tokio::sync::Mutex<OwnedWriteHalf>>>,
            >,
        > = std::sync::OnceLock::new();
        WRITERS.get_or_init(|| tokio::sync::RwLock::new(std::collections::HashMap::new()))
    }

    async fn handle_stream_message(stream_message: StreamMessage) -> Result<()> {
        match stream_message {
            StreamMessage::Setup {
                connection_id,
                mapping,
            } => {
                // Connect to target and start piping data back to server
                let target_addr = format!("{}:{}", mapping.target_host, mapping.target_port);
                let (tx, _rx) = crate::agent::reverse_port_forward::get_response_channel();
                let stream = match TcpStream::connect(&target_addr).await {
                    Ok(stream) => {
                        let ack = StreamMessage::SetupAck {
                            connection_id,
                            success: true,
                            error_message: None,
                        };
                        if let Err(e) = tx.send(Message::Stream(ack)).await {
                            error!(
                                "{} Failed to send setup acknowledgment for {}: {}",
                                styling::ERROR_INDICATOR,
                                target_addr,
                                e
                            );
                        }
                        stream
                    }
                    Err(e) => {
                        let ack = StreamMessage::SetupAck {
                            connection_id,
                            success: false,
                            error_message: Some(format!(
                                "Failed to connect to target {}: {}",
                                target_addr, e
                            )),
                        };
                        let _ = tx.send(Message::Stream(ack)).await;
                        return Err(LabyrinthError::Io(e));
                    }
                };

                let (mut read_half, write_half) = stream.into_split();

                // Store writer for future ClientToTarget writes
                {
                    let mut writers = Self::stream_writers().write().await;
                    writers.insert(connection_id, Arc::new(tokio::sync::Mutex::new(write_half)));
                }

                // Spawn a task to read from target and send to server
                let tx_clone = tx.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 65536];
                    loop {
                        match read_half.read(&mut buf).await {
                            Ok(0) => {
                                // Target closed
                                let _ = tx_clone
                                    .send(Message::Stream(StreamMessage::Close {
                                        connection_id,
                                        reason: CloseReason::ClientDisconnected,
                                    }))
                                    .await;
                                break;
                            }
                            Ok(n) => {
                                let payload = Bytes::copy_from_slice(&buf[..n]);
                                let _ = tx_clone
                                    .send(Message::Stream(StreamMessage::Data {
                                        connection_id,
                                        payload,
                                        direction: DataDirection::TargetToClient,
                                    }))
                                    .await;
                            }
                            Err(e) => {
                                let _ = tx_clone
                                    .send(Message::Stream(StreamMessage::Close {
                                        connection_id,
                                        reason: CloseReason::ProtocolError(e.to_string()),
                                    }))
                                    .await;
                                break;
                            }
                        }
                    }
                });
            }
            StreamMessage::Data {
                connection_id,
                payload,
                direction,
            } => {
                if matches!(direction, DataDirection::ClientToTarget) {
                    // Write client->target data to the stored writer
                    let writer_arc = {
                        let writers = Self::stream_writers().read().await;
                        writers.get(&connection_id).cloned()
                    };
                    if let Some(writer_arc) = writer_arc {
                        let mut writer = writer_arc.lock().await;
                        writer
                            .write_all(&payload)
                            .await
                            .map_err(LabyrinthError::Io)?;
                    }
                }
            }
            StreamMessage::Close { connection_id, .. } => {
                // Remove writer to cleanup
                let mut writers = Self::stream_writers().write().await;
                writers.remove(&connection_id);
            }
            _ => {}
        }
        Ok(())
    }

    // Removed unused reverse port forward helpers; streaming handles data plane
}

pub async fn run_agent(
    server_addr: &str,
    server_cert_b64: Option<String>,
    accept_fingerprint: Option<String>,
    proxy: Option<String>,
    retry: bool,
) -> Result<()> {
    AgentCore::run_agent(
        server_addr,
        server_cert_b64,
        accept_fingerprint,
        proxy,
        retry,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::models::{ConnectionId, PortMapping, StreamMessage};
    use std::io::ErrorKind;
    use std::sync::OnceLock;
    use tokio::net::TcpListener;
    use tokio::sync::Mutex as TokioMutex;
    use tokio::time::{timeout, Duration};

    fn stream_test_lock() -> &'static TokioMutex<()> {
        static LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| TokioMutex::new(()))
    }

    async fn next_response_message(timeout_ms: u64) -> Option<Message> {
        let (_, receiver) = crate::agent::reverse_port_forward::get_response_channel();
        let mut guard = receiver.lock().await;
        timeout(Duration::from_millis(timeout_ms), guard.recv())
            .await
            .ok()
            .flatten()
    }

    async fn drain_responses() {
        while next_response_message(10).await.is_some() {}
    }

    async fn wait_for_setup_ack(connection_id: ConnectionId, timeout_ms: u64) -> Option<Message> {
        let deadline = Duration::from_millis(timeout_ms);
        let started = tokio::time::Instant::now();

        loop {
            let elapsed = started.elapsed();
            if elapsed >= deadline {
                return None;
            }

            let remaining = deadline - elapsed;
            let step_timeout_ms = remaining.as_millis().min(50) as u64;
            let msg = next_response_message(step_timeout_ms).await;
            match msg {
                Some(Message::Stream(StreamMessage::SetupAck {
                    connection_id: ack_id,
                    success,
                    error_message,
                })) if ack_id == connection_id => {
                    return Some(Message::Stream(StreamMessage::SetupAck {
                        connection_id: ack_id,
                        success,
                        error_message,
                    }));
                }
                Some(_) => continue,
                None => return None,
            }
        }
    }

    #[tokio::test]
    async fn setup_stream_sends_success_ack() {
        let _guard = stream_test_lock().lock().await;
        drain_responses().await;

        let listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(e) => {
                if e.kind() == ErrorKind::PermissionDenied {
                    eprintln!("Skipping agent stream setup test (socket permissions): {e}");
                    return;
                }
                panic!("Unexpected socket error: {e}");
            }
        };
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        let mapping = PortMapping {
            local_port: 0,
            target_host: "127.0.0.1".to_string(),
            target_port: port,
        };
        let connection_id = ConnectionId::new_v4();

        AgentCore::handle_stream_message(StreamMessage::Setup {
            connection_id,
            mapping,
        })
        .await
        .unwrap();

        let msg = wait_for_setup_ack(connection_id, 500)
            .await
            .expect("expected ack");
        match msg {
            Message::Stream(StreamMessage::SetupAck { success, .. }) => assert!(success),
            other => panic!("unexpected message {other:?}"),
        }
        drain_responses().await;
    }

    #[tokio::test]
    async fn setup_stream_sends_failure_ack() {
        let _guard = stream_test_lock().lock().await;
        drain_responses().await;

        let mapping = PortMapping {
            local_port: 0,
            target_host: "127.0.0.1".to_string(),
            target_port: 9, // reserved port, likely closed
        };
        let connection_id = ConnectionId::new_v4();

        let result = AgentCore::handle_stream_message(StreamMessage::Setup {
            connection_id,
            mapping,
        })
        .await;
        assert!(result.is_err());

        let msg = wait_for_setup_ack(connection_id, 500)
            .await
            .expect("expected ack");
        match msg {
            Message::Stream(StreamMessage::SetupAck { success, .. }) => assert!(!success),
            other => panic!("unexpected message {other:?}"),
        }
        drain_responses().await;
    }
}
