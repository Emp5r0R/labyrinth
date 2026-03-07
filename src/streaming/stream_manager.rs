//! Bidirectional stream management implementation

use crate::streaming::{
    CloseReason, ConnectionId, DataDirection, ErrorRecoveryCoordinator, MetricsCollector,
    StreamContext, StreamError, StreamManager, StreamMessage, StreamResult,
};
use async_trait::async_trait;
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, instrument, warn};

/// Optimized buffer size for stream operations
/// Increased from 8KB to 64KB for better throughput with fewer syscalls
const STREAM_BUFFER_SIZE: usize = 65536;

/// Manages bidirectional streams for reverse port forwarding
pub struct BidirectionalStreamManager {
    /// Active stream contexts indexed by connection ID
    streams: Arc<RwLock<HashMap<ConnectionId, Arc<StreamContext>>>>,

    /// Channel for sending messages to the agent
    agent_sender: mpsc::Sender<StreamMessage>,

    /// Broadcast sender for shutdown signals
    shutdown_sender: broadcast::Sender<()>,

    /// Active stream tasks for cleanup
    stream_tasks: Arc<Mutex<HashMap<ConnectionId, Vec<JoinHandle<()>>>>>,

    /// Metrics collector for detailed monitoring
    metrics_collector: Option<Arc<MetricsCollector>>,

    /// Error recovery coordinator for handling failures
    recovery_coordinator: Option<Arc<ErrorRecoveryCoordinator>>,
}

impl BidirectionalStreamManager {
    /// Create a new BidirectionalStreamManager
    pub fn new(agent_sender: mpsc::Sender<StreamMessage>) -> Self {
        let (shutdown_sender, _) = broadcast::channel(1000); // Increased buffer for better performance

        Self {
            streams: Arc::new(RwLock::new(HashMap::new())),
            agent_sender,
            shutdown_sender,
            stream_tasks: Arc::new(Mutex::new(HashMap::new())),
            metrics_collector: None,
            recovery_coordinator: None,
        }
    }

    // Removed unused method with_monitoring - not used in current implementation

    /// Create a new BidirectionalStreamManager with custom channel buffer size
    pub fn with_buffer_size(buffer_size: usize) -> (Self, mpsc::Receiver<StreamMessage>) {
        let (agent_sender, agent_receiver) = mpsc::channel(buffer_size);
        let manager = Self::new(agent_sender);
        (manager, agent_receiver)
    }

    // Removed unused methods: get_shutdown_receiver, shutdown, active_stream_count, has_stream
}

#[async_trait]
impl StreamManager for BidirectionalStreamManager {
    #[instrument(skip(self, client_stream), fields(connection_id = %connection_id))]
    async fn create_bidirectional_stream(
        &self,
        connection_id: ConnectionId,
        client_stream: TcpStream,
    ) -> StreamResult<()> {
        info!(
            "Creating bidirectional stream for connection {}",
            connection_id
        );

        // Split the client stream into read and write halves
        let (client_read, client_write) = client_stream.into_split();

        // Create stream context
        let shutdown_receiver = self.shutdown_sender.subscribe();
        let stream_context = Arc::new(StreamContext::new(
            connection_id,
            client_read,
            client_write,
            self.agent_sender.clone(),
            shutdown_receiver,
        ));

        // Store the stream context
        {
            let mut streams = self.streams.write().await;
            streams.insert(connection_id, stream_context.clone());
        }

        // Create tasks for bidirectional data handling
        let mut task_handles = Vec::new();

        // Task for client-to-agent data flow
        let client_to_agent_task = {
            let agent_sender = self.agent_sender.clone();
            let shutdown_receiver = self.shutdown_sender.subscribe();
            let client_read_arc = stream_context.client_read.clone();
            let metrics_collector = self.metrics_collector.clone();
            let recovery_coordinator = self.recovery_coordinator.clone();

            tokio::spawn(async move {
                let mut buffer = vec![0u8; STREAM_BUFFER_SIZE];
                let mut shutdown_receiver = shutdown_receiver;

                loop {
                    let read_result = {
                        let mut client_read = client_read_arc.lock().await;
                        tokio::select! {
                            _ = shutdown_receiver.recv() => {
                                debug!("Received shutdown signal for client-to-agent stream {}", connection_id);
                                break;
                            }
                            result = client_read.read(&mut buffer) => result
                        }
                    };

                    match read_result {
                        Ok(0) => {
                            // Client disconnected
                            info!("Client disconnected for connection {}", connection_id);
                            let _ = agent_sender
                                .send(StreamMessage::Close {
                                    connection_id,
                                    reason: CloseReason::ClientDisconnected,
                                })
                                .await;
                            break;
                        }
                        Ok(bytes_read) => {
                            // Record data transfer metrics
                            if let Some(metrics) = &metrics_collector {
                                metrics
                                    .record_data_transfer(connection_id, bytes_read as u64, "sent")
                                    .await;
                            }

                            let data = Bytes::copy_from_slice(&buffer[..bytes_read]);
                            let message = StreamMessage::Data {
                                connection_id,
                                payload: data,
                                direction: DataDirection::ClientToTarget,
                            };

                            if let Err(e) = agent_sender.send(message).await {
                                let error = StreamError::channel_send(format!(
                                    "Failed to send data to agent: {}",
                                    e
                                ));

                                // Record error in metrics
                                if let Some(metrics) = &metrics_collector {
                                    metrics.record_error(&error, Some(connection_id)).await;
                                }

                                // Attempt error recovery
                                if let Some(recovery) = &recovery_coordinator {
                                    let error_context =
                                        error.with_connection_context(connection_id, "data_send");
                                    if let Err(recovery_err) = recovery
                                        .attempt_recovery(error_context, Some(connection_id))
                                        .await
                                    {
                                        error!(
                                            "Recovery failed for connection {}: {}",
                                            connection_id, recovery_err
                                        );
                                    }
                                }

                                error!(
                                    "Failed to send data to agent for connection {}: {}",
                                    connection_id, e
                                );
                                break;
                            }

                            debug!(
                                "Forwarded {} bytes from client to agent for connection {}",
                                bytes_read, connection_id
                            );
                        }
                        Err(e) => {
                            let error = StreamError::stream_broken(
                                connection_id,
                                format!("Error reading from client: {}", e),
                            );

                            // Record error in metrics
                            if let Some(metrics) = &metrics_collector {
                                metrics.record_error(&error, Some(connection_id)).await;
                            }

                            // Attempt error recovery
                            if let Some(recovery) = &recovery_coordinator {
                                let error_context =
                                    error.with_connection_context(connection_id, "client_read");
                                if let Err(recovery_err) = recovery
                                    .attempt_recovery(error_context, Some(connection_id))
                                    .await
                                {
                                    error!(
                                        "Recovery failed for connection {}: {}",
                                        connection_id, recovery_err
                                    );
                                }
                            }

                            error!(
                                "Error reading from client for connection {}: {}",
                                connection_id, e
                            );
                            let _ = agent_sender
                                .send(StreamMessage::Close {
                                    connection_id,
                                    reason: CloseReason::ProtocolError(e.to_string()),
                                })
                                .await;
                            break;
                        }
                    }
                }

                debug!(
                    "Client-to-agent stream handler finished for connection {}",
                    connection_id
                );
            })
        };

        task_handles.push(client_to_agent_task);

        // Store task handles for cleanup
        {
            let mut tasks = self.stream_tasks.lock().await;
            tasks.insert(connection_id, task_handles);
        }

        info!(
            "Bidirectional stream created successfully for connection {}",
            connection_id
        );
        Ok(())
    }

    #[instrument(skip(self, agent_data), fields(connection_id = %connection_id, data_len = agent_data.len()))]
    async fn handle_agent_stream(
        &self,
        connection_id: ConnectionId,
        agent_data: Bytes,
    ) -> StreamResult<()> {
        debug!(
            "Handling agent stream data for connection {}: {} bytes",
            connection_id,
            agent_data.len()
        );

        let streams = self.streams.read().await;
        let stream_context = streams.get(&connection_id).ok_or_else(|| {
            let error = StreamError::connection_not_found(connection_id);

            // Record error in metrics
            if let Some(metrics) = &self.metrics_collector {
                let metrics = Arc::clone(metrics);
                tokio::spawn(async move {
                    let error = StreamError::connection_not_found(connection_id);
                    metrics.record_error(&error, Some(connection_id)).await;
                });
            }

            error
        })?;

        // Write data to client
        let write_result = {
            let mut client_writer = stream_context.client_write.lock().await;
            let write_result = client_writer.write_all(&agent_data).await;
            if write_result.is_ok() {
                client_writer.flush().await
            } else {
                write_result
            }
        };

        match write_result {
            Ok(()) => {
                // Record successful data transfer
                if let Some(metrics) = &self.metrics_collector {
                    metrics
                        .record_data_transfer(connection_id, agent_data.len() as u64, "received")
                        .await;
                }

                debug!(
                    "Successfully forwarded {} bytes from agent to client for connection {}",
                    agent_data.len(),
                    connection_id
                );
                Ok(())
            }
            Err(e) => {
                let error = StreamError::stream_broken(
                    connection_id,
                    format!("Failed to write to client: {}", e),
                );
                let error_msg = error.to_string();

                // Record error in metrics
                if let Some(metrics) = &self.metrics_collector {
                    metrics.record_error(&error, Some(connection_id)).await;
                }

                // Attempt error recovery
                if let Some(recovery) = &self.recovery_coordinator {
                    let error_context =
                        error.with_connection_context(connection_id, "agent_to_client_write");
                    let recovery_coordinator = Arc::clone(recovery);
                    tokio::spawn(async move {
                        if let Err(recovery_err) = recovery_coordinator
                            .attempt_recovery(error_context, Some(connection_id))
                            .await
                        {
                            error!(
                                "Recovery failed for connection {}: {}",
                                connection_id, recovery_err
                            );
                        }
                    });
                }

                error!(
                    "Failed to write agent data to client for connection {}: {}",
                    connection_id, e
                );
                Err(StreamError::stream_broken(connection_id, error_msg))
            }
        }
    }

    async fn terminate_stream(&self, connection_id: ConnectionId) -> StreamResult<()> {
        info!("Terminating stream for connection {}", connection_id);

        // Remove stream from active streams
        let stream_context = {
            let mut streams = self.streams.write().await;
            streams.remove(&connection_id)
        };

        if stream_context.is_none() {
            warn!(
                "Attempted to terminate non-existent stream {}",
                connection_id
            );
            return Ok(());
        }

        // Cancel associated tasks
        let mut tasks = self.stream_tasks.lock().await;
        if let Some(task_handles) = tasks.remove(&connection_id) {
            for handle in task_handles {
                handle.abort();
            }
        }

        // Send close message to agent
        let close_message = StreamMessage::Close {
            connection_id,
            reason: CloseReason::UserRequested,
        };

        self.agent_sender.send(close_message).await.map_err(|e| {
            StreamError::channel_send(format!("Failed to send close message: {}", e))
        })?;

        debug!("Stream terminated for connection {}", connection_id);
        Ok(())
    }

    async fn send_to_client(&self, connection_id: ConnectionId, data: Bytes) -> StreamResult<()> {
        self.handle_agent_stream(connection_id, data).await
    }

    async fn send_to_agent(&self, connection_id: ConnectionId, data: Bytes) -> StreamResult<()> {
        let message = StreamMessage::Data {
            connection_id,
            payload: data,
            direction: DataDirection::ClientToTarget,
        };

        self.agent_sender.send(message).await.map_err(|e| {
            StreamError::channel_send(format!("Failed to send data to agent: {}", e))
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use tokio::net::{TcpListener, TcpStream};
    use tokio::time::{timeout, Duration};

    async fn tcp_pair() -> Option<(TcpStream, TcpStream)> {
        let listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(e) => {
                if e.kind() == ErrorKind::PermissionDenied {
                    eprintln!("Skipping stream manager tests (socket permissions): {e}");
                    return None;
                }
                panic!("Unexpected socket error: {e}");
            }
        };
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr)
            .await
            .expect("client connect failed");
        let (server, _) = listener.accept().await.expect("server accept failed");
        Some((client, server))
    }

    #[tokio::test]
    async fn test_stream_manager_creation() {
        let (manager, _receiver) = BidirectionalStreamManager::with_buffer_size(100);
        // Test basic creation - manager should be created successfully
        assert!(manager.streams.read().await.is_empty());
    }

    #[tokio::test]
    async fn test_terminate_nonexistent_stream() {
        let (manager, _receiver) = BidirectionalStreamManager::with_buffer_size(100);
        let connection_id = ConnectionId::new_v4();

        // Terminating non-existent stream should not error
        let result = manager.terminate_stream(connection_id).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_send_to_nonexistent_connection() {
        let (manager, _receiver) = BidirectionalStreamManager::with_buffer_size(100);
        let connection_id = ConnectionId::new_v4();
        let data = Bytes::from("test data");

        // Sending to non-existent connection should error
        let result = manager.send_to_client(connection_id, data).await;
        assert!(result.is_err());

        if let Err(StreamError::ConnectionNotFound { .. }) = result {
            // Expected error type
        } else {
            panic!("Expected ConnectionNotFound error");
        }
    }

    #[tokio::test]
    async fn test_agent_channel_communication() {
        let (manager, mut receiver) = BidirectionalStreamManager::with_buffer_size(100);
        let connection_id = ConnectionId::new_v4();
        let test_data = Bytes::from("test message");

        // Send data to agent
        let result = manager
            .send_to_agent(connection_id, test_data.clone())
            .await;
        assert!(result.is_ok());

        // Receive the message
        let received = timeout(Duration::from_millis(100), receiver.recv()).await;
        assert!(received.is_ok());

        let message = received.unwrap().unwrap();
        match message {
            StreamMessage::Data {
                connection_id: recv_id,
                payload,
                direction,
            } => {
                assert_eq!(recv_id, connection_id);
                assert_eq!(payload, test_data);
                assert_eq!(direction, DataDirection::ClientToTarget);
            }
            _ => panic!("Expected Data message"),
        }
    }

    #[tokio::test]
    async fn test_create_bidirectional_stream() {
        let (manager, _receiver) = BidirectionalStreamManager::with_buffer_size(100);
        let connection_id = ConnectionId::new_v4();

        // Create a mock TCP connection using a pair of connected sockets
        let Some((client_stream, server_stream)) = tcp_pair().await else {
            return;
        };

        // Create bidirectional stream
        let result = manager
            .create_bidirectional_stream(connection_id, server_stream)
            .await;
        assert!(result.is_ok());

        // Verify stream was created by checking internal state
        assert!(manager.streams.read().await.contains_key(&connection_id));

        // Clean up
        let _ = manager.terminate_stream(connection_id).await;
        drop(client_stream);
    }

    #[tokio::test]
    async fn test_stream_lifecycle() {
        let (manager, mut receiver) = BidirectionalStreamManager::with_buffer_size(100);
        let connection_id = ConnectionId::new_v4();

        // Initially no stream
        assert!(manager.streams.read().await.is_empty());

        // Create a mock TCP connection
        let Some((client_stream, server_stream)) = tcp_pair().await else {
            return;
        };

        // Create stream
        let result = manager
            .create_bidirectional_stream(connection_id, server_stream)
            .await;
        assert!(result.is_ok());
        assert!(manager.streams.read().await.contains_key(&connection_id));

        // Terminate stream
        let result = manager.terminate_stream(connection_id).await;
        assert!(result.is_ok());

        // Verify close message was sent to agent
        let close_message = timeout(Duration::from_millis(100), receiver.recv()).await;
        assert!(close_message.is_ok());

        let message = close_message.unwrap().unwrap();
        match message {
            StreamMessage::Close {
                connection_id: recv_id,
                reason,
            } => {
                assert_eq!(recv_id, connection_id);
                assert_eq!(reason, CloseReason::UserRequested);
            }
            _ => panic!("Expected Close message"),
        }

        // Stream should be removed
        assert!(!manager.streams.read().await.contains_key(&connection_id));

        // Clean up
        drop(client_stream);
    }
}
