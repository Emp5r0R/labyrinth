use crate::error::Result;
use crate::protocol::Message;
use crate::server::core::LabyrinthServer;
#[cfg(target_os = "windows")]
use crate::server::netstack_bridge_windows::WindowsNetstackBridge;
use crate::streaming::models::{ConnectionStatus, DataDirection, StreamMessage};

use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// Handles writing outgoing messages to the agent's stream from a queue.
/// This runs in its own dedicated task for each agent.
pub async fn handle_writer<W>(mut writer: tokio::io::WriteHalf<W>, mut rx: mpsc::Receiver<Message>)
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    while let Some(message) = rx.recv().await {
        match serde_json::to_string(&message) {
            Ok(msg_str) => {
                if let Err(e) = writer.write_all(msg_str.as_bytes()).await {
                    error!("Failed to write message to stream: {}", e);
                    break;
                }
                if let Err(e) = writer.write_all(b"\n").await {
                    error!("Failed to write newline to stream: {}", e);
                    break;
                }
            }
            Err(e) => {
                error!("Failed to serialize message: {}", e);
            }
        }
    }
}

/// Handles reading incoming messages from the agent's stream.
/// This runs in its own dedicated task for each agent.
pub async fn handle_reader<R>(
    mut reader: tokio::io::BufReader<tokio::io::ReadHalf<R>>,
    server: Arc<LabyrinthServer>,
    agent_id: String,
) -> Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut buf = Vec::new();
    loop {
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) => {
                info!("Agent {} disconnected.", agent_id);
                break;
            }
            Ok(_) => {
                let message: Message = match serde_json::from_slice(&buf[..buf.len() - 1]) {
                    Ok(msg) => msg,
                    Err(e) => {
                        error!("Failed to parse message from agent {}: {}", agent_id, e);
                        buf.clear();
                        continue;
                    }
                };
                buf.clear();

                // A message was received, update the agent's last seen time.
                if let Some(agent) = server.agents().read().await.get(&agent_id) {
                    *agent.last_seen.lock().await = std::time::Instant::now();
                }

                // Process the message
                if let Err(e) = process_message(server.clone(), &agent_id, message).await {
                    error!("Error processing message from agent {}: {}", agent_id, e);
                }
            }
            Err(e) => {
                error!("Failed to read from agent stream {}: {}", agent_id, e);
                break;
            }
        }
    }

    // Cleanup: Remove the agent from the server's list of connected agents.
    server.agents().write().await.remove(&agent_id);
    warn!("Agent {} removed due to disconnection.", agent_id);

    Ok(())
}

/// Processes a single message received from an agent.
async fn process_message(
    server: Arc<LabyrinthServer>,
    agent_id: &str,
    message: Message,
) -> Result<()> {
    match message {
        Message::Pong => {
            // Pong received, agent is alive. Last_seen is already updated.
        }
        Message::TunnelStarted => {
            info!("Agent {} confirmed tunnel started.", agent_id);
        }
        Message::TunnelStopped => {
            info!("Agent {} confirmed tunnel stopped.", agent_id);
        }
        Message::CommandResponse { output, error } => {
            if let Some(agent) = server.agents().read().await.get(agent_id) {
                let mut command_response = agent.command_response.lock().await;
                if let Some(sender) = command_response.take() {
                    if sender
                        .send(Message::CommandResponse { output, error })
                        .is_err()
                    {
                        error!("Failed to send command response to waiting UI task.");
                    }
                }
            }
        }
        Message::FileUploadResponse { success, message } => {
            if let Some(agent) = server.agents().read().await.get(agent_id) {
                let mut command_response = agent.command_response.lock().await;
                if let Some(sender) = command_response.take() {
                    if sender
                        .send(Message::FileUploadResponse { success, message })
                        .is_err()
                    {
                        error!("Failed to send file upload response to waiting UI task.");
                    }
                }
            }
        }
        Message::DropDwellerResponse {
            success,
            message,
            receipt,
        } => {
            if let Some(agent) = server.agents().read().await.get(agent_id) {
                let mut command_response = agent.command_response.lock().await;
                if let Some(sender) = command_response.take() {
                    if sender
                        .send(Message::DropDwellerResponse {
                            success,
                            message,
                            receipt,
                        })
                        .is_err()
                    {
                        error!("Failed to send dweller response to waiting UI task.");
                    }
                }
            }
        }
        Message::ConfigureDwellerResponse { success, message } => {
            if let Some(agent) = server.agents().read().await.get(agent_id) {
                let mut command_response = agent.command_response.lock().await;
                if let Some(sender) = command_response.take() {
                    if sender
                        .send(Message::ConfigureDwellerResponse { success, message })
                        .is_err()
                    {
                        error!("Failed to send dweller config response to waiting UI task.");
                    }
                }
            }
        }
        Message::DwellerPollTasks {
            dweller_id,
            max_tasks,
        } => {
            if dweller_id != agent_id {
                warn!(
                    "Dweller {} tried to poll tasks for {}",
                    agent_id, dweller_id
                );
                return Ok(());
            }
            let tasks = server
                .claim_dweller_tasks(agent_id, max_tasks.clamp(1, 100))
                .await?;
            if let Some(agent) = server.agents().read().await.get(agent_id) {
                if let Err(e) = agent.sender.send(Message::DwellerTasks { tasks }).await {
                    error!("Failed to send dweller task batch: {}", e);
                }
            }
        }
        Message::DwellerTaskResult { dweller_id, result } => {
            if dweller_id != agent_id {
                warn!(
                    "Dweller {} tried to complete task for {}",
                    agent_id, dweller_id
                );
                return Ok(());
            }
            if !server.complete_dweller_task(agent_id, result).await? {
                warn!("Dweller {} returned result for unknown task", agent_id);
            }
        }
        Message::FileDownloadResponse {
            success,
            message,
            remote_path,
            content_b64,
        } => {
            if let Some(agent) = server.agents().read().await.get(agent_id) {
                let mut command_response = agent.command_response.lock().await;
                if let Some(sender) = command_response.take() {
                    if sender
                        .send(Message::FileDownloadResponse {
                            success,
                            message,
                            remote_path,
                            content_b64,
                        })
                        .is_err()
                    {
                        error!("Failed to send file download response to waiting UI task.");
                    }
                }
            }
        }
        Message::ShellSessionStarted { .. }
        | Message::ShellSessionOutput { .. }
        | Message::ShellSessionClose { .. } => {
            if let Some(agent) = server.agents().read().await.get(agent_id) {
                let shell_events = agent.shell_events.lock().await;
                if let Some(sender) = shell_events.as_ref() {
                    if sender.send(message).is_err() {
                        error!("Failed to send shell session event to interactive shell task.");
                    }
                }
            }
        }
        Message::Stream(stream_msg) => {
            #[cfg(target_os = "windows")]
            if WindowsNetstackBridge::try_handle_agent_stream(&stream_msg).await {
                return Ok(());
            }

            // Handle streaming data coming from the agent (Portal mode)
            match stream_msg {
                StreamMessage::Data {
                    connection_id,
                    payload,
                    direction,
                } => {
                    if direction == DataDirection::TargetToClient {
                        if let Some(cm) = server.get_connection_manager().await {
                            let _ = cm
                                .update_connection_status(&connection_id, ConnectionStatus::Active)
                                .await;
                        }
                        if let Some(sm) = server.get_stream_manager().await {
                            if let Err(e) = sm.send_to_client(connection_id, payload).await {
                                error!("Failed to deliver agent data to client: {}", e);
                            }
                        } else {
                            error!("Stream manager unavailable to handle agent data");
                        }
                    }
                }
                StreamMessage::Close { connection_id, .. } => {
                    if let Some(sm) = server.get_stream_manager().await {
                        let _ = sm.terminate_stream(connection_id).await;
                    }
                    if let Some(cm) = server.get_connection_manager().await {
                        let _ = cm
                            .update_connection_status(&connection_id, ConnectionStatus::Closing)
                            .await;
                        let _ = cm.cleanup_connection(&connection_id).await;
                    }
                    let _ = server.unregister_connection_owner(&connection_id).await;
                }
                StreamMessage::SetupAck {
                    connection_id,
                    success,
                    error_message,
                } => {
                    if let Some(cm) = server.get_connection_manager().await {
                        if success {
                            let _ = cm
                                .update_connection_status(&connection_id, ConnectionStatus::Active)
                                .await;
                        } else {
                            let reason =
                                error_message.unwrap_or_else(|| "unknown error".to_string());
                            let _ = cm
                                .update_connection_status(
                                    &connection_id,
                                    ConnectionStatus::Error(reason.clone()),
                                )
                                .await;
                            let _ = cm.cleanup_connection(&connection_id).await;
                            if let Some(sm) = server.get_stream_manager().await {
                                let _ = sm.terminate_stream(connection_id).await;
                            }
                            let _ = server.unregister_connection_owner(&connection_id).await;
                            warn!(
                                "Agent {} failed to establish streaming connection {}: {}",
                                agent_id, connection_id, reason
                            );
                        }
                    }
                }
                _ => {
                    // Other stream messages can be ignored for now
                }
            }
        }
        _ => {
            warn!(
                "Received unhandled message from agent {}: {:?}",
                agent_id, message
            );
        }
    }
    Ok(())
}
