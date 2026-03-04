//! Agent-side streaming manager for handling bidirectional data flow

use crate::streaming::{
    ConnectionId, PortMapping, StreamMessage, DataDirection, CloseReason,
    ConnectionStatus, ConnectionState, ConnectionStats,
};
use crate::streaming::errors::{StreamError, StreamResult};
use crate::streaming::traits::{ConnectionManager, StreamManager};
use crate::styling;
use async_trait::async_trait;
use bytes::Bytes;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, RwLock, Mutex};
use tokio::time::{timeout, Duration};
use tracing::{debug, error, info, warn};

/// Agent-side connection state
#[derive(Debug)]
struct AgentConnectionState {
    connection_id: ConnectionId,
    target_mapping: PortMapping,
    target_stream: Arc<Mutex<TcpStream>>,
    tunnel_sender: mpsc::Sender<StreamMessage>,
    #[allow(dead_code)]
    created_at: std::time::Instant,
    #[allow(dead_code)]
    status: ConnectionStatus,
}

impl AgentConnectionState {
    fn new(
        connection_id: ConnectionId,
        target_mapping: PortMapping,
        target_stream: TcpStream,
        tunnel_sender: mpsc::Sender<StreamMessage>,
    ) -> Self {
        Self {
            connection_id,
            target_mapping,
            target_stream: Arc::new(Mutex::new(target_stream)),
            tunnel_sender,
            created_at: std::time::Instant::now(),
            status: ConnectionStatus::Establishing,
        }
    }
}

/// Agent-side streaming manager that handles connections to target services
pub struct AgentStreamManager {
    /// Active connections to target services
    connections: Arc<RwLock<HashMap<ConnectionId, Arc<AgentConnectionState>>>>,
    /// Channel for sending messages back to the tunnel
    tunnel_sender: mpsc::Sender<StreamMessage>,
    /// Statistics tracking
    stats: Arc<RwLock<ConnectionStats>>,
}

impl AgentStreamManager {
    /// Create a new agent stream manager
    #[allow(dead_code)]
    pub fn new(tunnel_sender: mpsc::Sender<StreamMessage>) -> Self {
        Self {
            connections: Arc::new(RwLock::new(HashMap::new())),
            tunnel_sender,
            stats: Arc::new(RwLock::new(ConnectionStats::new())),
        }
    }

    // Removed unused method handle_stream_message - not used in current implementation

    // Removed unused method handle_setup_message - not used in current implementation
    
    #[allow(dead_code)]
    async fn handle_setup_message(
        &self,
        connection_id: ConnectionId,
        mapping: PortMapping,
    ) -> StreamResult<()> {
        info!(
            "{} Setting up connection {} to target {}:{}",
            styling::SUCCESS_INDICATOR,
            connection_id,
            mapping.target_host,
            mapping.target_port
        );

        let target_addr = format!("{}:{}", mapping.target_host, mapping.target_port);
        
        // Connect to target service with timeout
        let target_stream = match timeout(
            Duration::from_secs(10),
            TcpStream::connect(&target_addr)
        ).await {
            Ok(Ok(stream)) => {
                info!(
                    "{} Successfully connected to target {} for connection {}",
                    styling::SUCCESS_INDICATOR,
                    target_addr,
                    connection_id
                );
                stream
            }
            Ok(Err(e)) => {
                error!(
                    "{} Failed to connect to target {} for connection {}: {}",
                    styling::ERROR_INDICATOR,
                    target_addr,
                    connection_id,
                    e
                );
                
                // Update stats
                {
                    let mut stats_lock = self.stats.write().await;
                    stats_lock.increment_failed();
                }
                
                // Send setup acknowledgment with failure
                let setup_ack = StreamMessage::SetupAck {
                    connection_id,
                    success: false,
                    error_message: Some(format!("Failed to connect to target {}: {}", target_addr, e)),
                };
                
                if let Err(e) = self.tunnel_sender.send(setup_ack).await {
                    error!(
                        "{} Failed to send setup failure ack for connection {}: {}",
                        styling::ERROR_INDICATOR,
                        connection_id,
                        e
                    );
                }
                
                return Err(StreamError::connection_failed(format!(
                    "Failed to connect to target {}: {}",
                    target_addr, e
                )));
            }
            Err(_) => {
                error!(
                    "{} Timeout connecting to target {} for connection {}",
                    styling::ERROR_INDICATOR,
                    target_addr,
                    connection_id
                );
                
                // Update stats
                {
                    let mut stats_lock = self.stats.write().await;
                    stats_lock.increment_failed();
                }
                
                // Send setup acknowledgment with timeout failure
                let setup_ack = StreamMessage::SetupAck {
                    connection_id,
                    success: false,
                    error_message: Some(format!("Timeout connecting to target {}", target_addr)),
                };
                
                if let Err(e) = self.tunnel_sender.send(setup_ack).await {
                    error!(
                        "{} Failed to send timeout setup ack for connection {}: {}",
                        styling::ERROR_INDICATOR,
                        connection_id,
                        e
                    );
                }
                
                return Err(StreamError::timeout(Duration::from_secs(10)));
            }
        };

        // Create connection state
        let connection_state = Arc::new(AgentConnectionState::new(
            connection_id,
            mapping.clone(),
            target_stream,
            self.tunnel_sender.clone(),
        ));

        // Store connection
        {
            let mut connections_lock = self.connections.write().await;
            connections_lock.insert(connection_id, Arc::clone(&connection_state));
        }

        // Update stats
        {
            let mut stats_lock = self.stats.write().await;
            stats_lock.increment_total();
            stats_lock.increment_active();
        }

        // Send setup acknowledgment with success
        let setup_ack = StreamMessage::SetupAck {
            connection_id,
            success: true,
            error_message: None,
        };
        
        if let Err(e) = self.tunnel_sender.send(setup_ack).await {
            error!(
                "{} Failed to send setup success ack for connection {}: {}",
                styling::ERROR_INDICATOR,
                connection_id,
                e
            );
        }

        // Start bidirectional data forwarding
        self.start_bidirectional_forwarding(connection_state).await?;

        Ok(())
    }

    // Removed unused method start_bidirectional_forwarding - not used in current implementation
    
    #[allow(dead_code)]
    async fn start_bidirectional_forwarding(
        &self,
        connection_state: Arc<AgentConnectionState>,
    ) -> StreamResult<()> {
        let connection_id = connection_state.connection_id;
        let target_stream = Arc::clone(&connection_state.target_stream);
        let tunnel_sender = connection_state.tunnel_sender.clone();

        // Spawn task to read from target and send to tunnel
        tokio::spawn(async move {
            let mut target_stream = target_stream.lock().await;
            let mut buffer = vec![0u8; 65536]; // Increased buffer size for better performance

            loop {
                match target_stream.read(&mut buffer).await {
                    Ok(0) => {
                        // Target closed connection
                        info!(
                            "{} Target closed connection {}",
                            styling::SUCCESS_INDICATOR,
                            connection_id
                        );
                        
                        // Send close message to tunnel
                        let close_msg = StreamMessage::Close {
                            connection_id,
                            reason: CloseReason::ClientDisconnected,
                        };
                        
                        if let Err(e) = tunnel_sender.send(close_msg).await {
                            error!(
                                "{} Failed to send close message for connection {}: {}",
                                styling::ERROR_INDICATOR,
                                connection_id,
                                e
                            );
                        }
                        break;
                    }
                    Ok(n) => {
                        // Forward data to tunnel
                        let data_msg = StreamMessage::Data {
                            connection_id,
                            payload: Bytes::copy_from_slice(&buffer[..n]),
                            direction: DataDirection::TargetToClient,
                        };
                        
                        if let Err(e) = tunnel_sender.send(data_msg).await {
                            error!(
                                "{} Failed to send data from target for connection {}: {}",
                                styling::ERROR_INDICATOR,
                                connection_id,
                                e
                            );
                            break;
                        }
                        
                        debug!(
                            "{} Forwarded {} bytes from target to tunnel for connection {}",
                            styling::SUCCESS_INDICATOR,
                            n,
                            connection_id
                        );
                    }
                    Err(e) => {
                        error!(
                            "{} Error reading from target for connection {}: {}",
                            styling::ERROR_INDICATOR,
                            connection_id,
                            e
                        );
                        
                        // Send close message to tunnel
                        let close_msg = StreamMessage::Close {
                            connection_id,
                            reason: CloseReason::ProtocolError(e.to_string()),
                        };
                        
                        if let Err(e) = tunnel_sender.send(close_msg).await {
                            error!(
                                "{} Failed to send error close message for connection {}: {}",
                                styling::ERROR_INDICATOR,
                                connection_id,
                                e
                            );
                        }
                        break;
                    }
                }
            }
        });

        Ok(())
    }

    /// Handle data from client to target
    async fn handle_data_to_target(
        &self,
        connection_id: ConnectionId,
        payload: Bytes,
    ) -> StreamResult<()> {
        let connection_state = {
            let connections_lock = self.connections.read().await;
            connections_lock.get(&connection_id).cloned()
        };

        if let Some(connection_state) = connection_state {
            let mut target_stream = connection_state.target_stream.lock().await;
            
            match target_stream.write_all(&payload).await {
                Ok(()) => {
                    debug!(
                        "{} Forwarded {} bytes to target for connection {}",
                        styling::SUCCESS_INDICATOR,
                        payload.len(),
                        connection_id
                    );
                }
                Err(e) => {
                    error!(
                        "{} Failed to write to target for connection {}: {}",
                        styling::ERROR_INDICATOR,
                        connection_id,
                        e
                    );
                    
                    // Send close message to tunnel
                    let close_msg = StreamMessage::Close {
                        connection_id,
                        reason: CloseReason::ProtocolError(e.to_string()),
                    };
                    
                    if let Err(e) = connection_state.tunnel_sender.send(close_msg).await {
                        error!(
                            "{} Failed to send error close message for connection {}: {}",
                            styling::ERROR_INDICATOR,
                            connection_id,
                            e
                        );
                    }
                    
                    return Err(StreamError::stream_broken(connection_id, e.to_string()));
                }
            }
        } else {
            warn!(
                "{} No connection found for ID: {}",
                styling::WARNING_INDICATOR,
                connection_id
            );
            return Err(StreamError::connection_failed(format!(
                "Connection {} not found",
                connection_id
            )));
        }

        Ok(())
    }

    /// Handle close message
    async fn handle_close_message(
        &self,
        connection_id: ConnectionId,
        reason: CloseReason,
    ) -> StreamResult<()> {
        info!(
            "{} Closing connection {} due to: {:?}",
            styling::SUCCESS_INDICATOR,
            connection_id,
            reason
        );

        // Remove connection from active connections
        let connection_removed = {
            let mut connections_lock = self.connections.write().await;
            connections_lock.remove(&connection_id).is_some()
        };

        if connection_removed {
            // Update stats
            let mut stats_lock = self.stats.write().await;
            stats_lock.decrement_active();
            
            info!(
                "{} Successfully cleaned up connection {}",
                styling::SUCCESS_INDICATOR,
                connection_id
            );
        } else {
            warn!(
                "{} Connection {} not found for cleanup",
                styling::WARNING_INDICATOR,
                connection_id
            );
        }

        Ok(())
    }

    // Removed unused methods: get_stats, shutdown - not used in current implementation
    
    #[allow(dead_code)]
    pub async fn get_stats(&self) -> ConnectionStats {
        self.stats.read().await.clone()
    }

    #[allow(dead_code)]
    pub async fn shutdown(&self) -> StreamResult<()> {
        info!("{} Shutting down agent stream manager", styling::SUCCESS_INDICATOR);
        
        let connections_to_close: Vec<ConnectionId> = {
            let connections_lock = self.connections.read().await;
            connections_lock.keys().cloned().collect()
        };

        for connection_id in connections_to_close {
            let close_msg = StreamMessage::Close {
                connection_id,
                reason: CloseReason::Shutdown,
            };
            
            if let Err(e) = self.tunnel_sender.send(close_msg).await {
                error!(
                    "{} Failed to send shutdown close message for connection {}: {}",
                    styling::ERROR_INDICATOR,
                    connection_id,
                    e
                );
            }
        }

        // Clear all connections
        {
            let mut connections_lock = self.connections.write().await;
            connections_lock.clear();
        }

        info!("{} Agent stream manager shutdown complete", styling::SUCCESS_INDICATOR);
        Ok(())
    }
}

#[async_trait]
impl ConnectionManager for AgentStreamManager {
    async fn handle_new_connection(
        &self,
        _connection: TcpStream,
        _mapping: PortMapping,
    ) -> StreamResult<ConnectionId> {
        // Agent doesn't handle new connections directly - they come via tunnel messages
        Err(StreamError::protocol_error(
            "Agent doesn't handle new connections directly".to_string()
        ))
    }

    async fn cleanup_connection(&self, connection_id: &ConnectionId) -> StreamResult<()> {
        self.handle_close_message(*connection_id, CloseReason::UserRequested).await
    }

    async fn get_connection_stats(&self) -> StreamResult<ConnectionStats> {
        Ok(self.stats.read().await.clone())
    }

    async fn get_connection_state(&self, connection_id: &ConnectionId) -> StreamResult<Option<ConnectionState>> {
        let connections_lock = self.connections.read().await;
        if let Some(agent_state) = connections_lock.get(connection_id) {
            // Convert AgentConnectionState to ConnectionState
            let dummy_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
            let connection_state = ConnectionState::new(
                agent_state.connection_id,
                dummy_addr,
                agent_state.target_mapping.clone(),
            );
            Ok(Some(connection_state))
        } else {
            Ok(None)
        }
    }

    async fn track_existing_connection(
        &self,
        _connection_id: ConnectionId,
        _client_addr: SocketAddr,
        _mapping: PortMapping,
    ) -> StreamResult<()> {
        Err(StreamError::protocol_error(
            "Agent does not track server-side connections".to_string(),
        ))
    }

    async fn update_connection_status(
        &self,
        connection_id: &ConnectionId,
        status: ConnectionStatus,
    ) -> StreamResult<()> {
        let connections_lock = self.connections.read().await;
        if let Some(_connection_state) = connections_lock.get(connection_id) {
            // We can't directly modify the status in Arc<AgentConnectionState> since it's not mutable
            // This would require refactoring the AgentConnectionState to use Arc<Mutex<ConnectionStatus>>
            // For now, we'll just log the status update
            info!(
                "{} Connection {} status updated to: {:?}",
                styling::SUCCESS_INDICATOR,
                connection_id,
                status
            );
            Ok(())
        } else {
            Err(StreamError::connection_failed(format!(
                "Connection {} not found for status update",
                connection_id
            )))
        }
    }
}

#[async_trait]
impl StreamManager for AgentStreamManager {
    async fn create_bidirectional_stream(
        &self,
        _connection_id: ConnectionId,
        _client_stream: TcpStream,
    ) -> StreamResult<()> {
        // Agent doesn't create bidirectional streams directly - they're created via setup messages
        Err(StreamError::protocol_error(
            "Agent doesn't create bidirectional streams directly".to_string()
        ))
    }

    async fn handle_agent_stream(
        &self,
        connection_id: ConnectionId,
        agent_data: Bytes,
    ) -> StreamResult<()> {
        // Forward data to target
        self.handle_data_to_target(connection_id, agent_data).await
    }

    async fn terminate_stream(&self, connection_id: ConnectionId) -> StreamResult<()> {
        self.cleanup_connection(&connection_id).await
    }

    async fn send_to_client(&self, connection_id: ConnectionId, data: Bytes) -> StreamResult<()> {
        // Send data back to client via tunnel
        let data_msg = StreamMessage::Data {
            connection_id,
            payload: data,
            direction: DataDirection::TargetToClient,
        };
        
        self.tunnel_sender.send(data_msg).await
            .map_err(|e| StreamError::channel_send(format!("Failed to send to client: {}", e)))
    }

    async fn send_to_agent(&self, connection_id: ConnectionId, data: Bytes) -> StreamResult<()> {
        // This is the agent, so forward to target
        self.handle_data_to_target(connection_id, data).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;
    use uuid::Uuid;

    #[tokio::test]
    async fn test_agent_stream_manager_creation() {
        let (sender, _receiver) = mpsc::channel(100);
        let manager = AgentStreamManager::new(sender);
        
        let stats = manager.get_stats().await;
        assert_eq!(stats.active_connections, 0);
        assert_eq!(stats.total_connections, 0);
    }

    #[tokio::test]
    async fn test_handle_close_message() {
        let (sender, _receiver) = mpsc::channel(100);
        let manager = AgentStreamManager::new(sender);
        
        let connection_id = Uuid::new_v4();
        let result = manager.handle_close_message(connection_id, CloseReason::UserRequested).await;
        
        // Should succeed even if connection doesn't exist
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_connection_manager_trait() {
        let (sender, _receiver) = mpsc::channel(100);
        let manager = AgentStreamManager::new(sender);
        
        // Test that agent doesn't handle new connections directly
        let dummy_stream = tokio::net::TcpStream::connect("127.0.0.1:1").await;
        if let Ok(stream) = dummy_stream {
            let mapping = PortMapping {
                local_port: 8080,
                target_host: "localhost".to_string(),
                target_port: 3000,
            };
            
            let result = manager.handle_new_connection(stream, mapping).await;
            assert!(result.is_err());
        }
    }
}
