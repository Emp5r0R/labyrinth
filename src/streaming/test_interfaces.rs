//! Test interfaces and mock implementations for testing

use crate::streaming::errors::StreamResult;
use crate::streaming::traits::{ConnectionManager, StreamManager};
use crate::streaming::{
    ConnectionId, ConnectionState, ConnectionStats, ConnectionStatus, PortMapping,
};
use async_trait::async_trait;
use bytes::Bytes;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::RwLock;

/// Mock implementation of ConnectionManager for testing
#[derive(Debug)]
pub struct MockConnectionManager {
    connections: Arc<RwLock<HashMap<ConnectionId, ConnectionState>>>,
    stats: Arc<RwLock<ConnectionStats>>,
}

impl MockConnectionManager {
    pub fn new() -> Self {
        Self {
            connections: Arc::new(RwLock::new(HashMap::new())),
            stats: Arc::new(RwLock::new(ConnectionStats::new())),
        }
    }
}

impl Default for MockConnectionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ConnectionManager for MockConnectionManager {
    async fn handle_new_connection(
        &self,
        connection: TcpStream,
        mapping: PortMapping,
    ) -> StreamResult<ConnectionId> {
        let connection_id = uuid::Uuid::new_v4();
        let client_addr = connection.peer_addr().map_err(|e| {
            crate::streaming::errors::StreamError::connection_failed(format!(
                "Failed to get peer address: {}",
                e
            ))
        })?;

        let connection_state = ConnectionState::new(connection_id, client_addr, mapping);

        {
            let mut connections = self.connections.write().await;
            connections.insert(connection_id, connection_state);
        }

        {
            let mut stats = self.stats.write().await;
            stats.increment_total();
        }

        Ok(connection_id)
    }

    async fn cleanup_connection(&self, connection_id: &ConnectionId) -> StreamResult<()> {
        let mut connections = self.connections.write().await;
        connections.remove(connection_id);
        Ok(())
    }

    async fn get_connection_stats(&self) -> StreamResult<ConnectionStats> {
        let stats = self.stats.read().await;
        Ok(stats.clone())
    }

    async fn get_connection_state(
        &self,
        connection_id: &ConnectionId,
    ) -> StreamResult<Option<ConnectionState>> {
        let connections = self.connections.read().await;
        Ok(connections.get(connection_id).cloned())
    }

    async fn update_connection_status(
        &self,
        connection_id: &ConnectionId,
        status: ConnectionStatus,
    ) -> StreamResult<()> {
        let mut connections = self.connections.write().await;
        if let Some(connection_state) = connections.get_mut(connection_id) {
            connection_state.status = status;
        }
        Ok(())
    }

    async fn track_existing_connection(
        &self,
        connection_id: ConnectionId,
        client_addr: SocketAddr,
        mapping: PortMapping,
    ) -> StreamResult<()> {
        let mut connections = self.connections.write().await;
        let state = ConnectionState::new(connection_id, client_addr, mapping);
        connections.insert(connection_id, state);
        Ok(())
    }
}

/// Mock implementation of StreamManager for testing
#[derive(Debug)]
pub struct MockStreamManager {
    streams: Arc<RwLock<HashMap<ConnectionId, bool>>>,
}

impl MockStreamManager {
    pub fn new() -> Self {
        Self {
            streams: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for MockStreamManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl StreamManager for MockStreamManager {
    async fn create_bidirectional_stream(
        &self,
        connection_id: ConnectionId,
        _client_stream: TcpStream,
    ) -> StreamResult<()> {
        let mut streams = self.streams.write().await;
        streams.insert(connection_id, true);
        Ok(())
    }

    async fn handle_agent_stream(
        &self,
        _connection_id: ConnectionId,
        _agent_data: Bytes,
    ) -> StreamResult<()> {
        // Mock implementation - just return success
        Ok(())
    }

    async fn terminate_stream(&self, connection_id: ConnectionId) -> StreamResult<()> {
        let mut streams = self.streams.write().await;
        streams.remove(&connection_id);
        Ok(())
    }

    async fn send_to_client(&self, _connection_id: ConnectionId, _data: Bytes) -> StreamResult<()> {
        // Mock implementation - just return success
        Ok(())
    }

    async fn send_to_agent(&self, _connection_id: ConnectionId, _data: Bytes) -> StreamResult<()> {
        // Mock implementation - just return success
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // Removed unused import: use super::*;
    use crate::streaming::*;
    use std::net::SocketAddr;
    use uuid::Uuid;

    #[test]
    fn test_connection_id_type() {
        let id: ConnectionId = Uuid::new_v4();
        assert!(!id.to_string().is_empty());
    }

    #[test]
    fn test_port_mapping_creation() {
        let mapping = PortMapping {
            local_port: 8080,
            target_host: "localhost".to_string(),
            target_port: 3000,
        };
        assert_eq!(mapping.local_port, 8080);
        assert_eq!(mapping.target_host, "localhost");
        assert_eq!(mapping.target_port, 3000);
    }

    #[test]
    fn test_connection_state_creation() {
        let id = Uuid::new_v4();
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let mapping = PortMapping {
            local_port: 8080,
            target_host: "localhost".to_string(),
            target_port: 3000,
        };

        let state = ConnectionState::new(id, addr, mapping.clone());
        assert_eq!(state.id, id);
        assert_eq!(state.client_addr, addr);
        assert_eq!(state.target_mapping, mapping);
        assert_eq!(state.status, ConnectionStatus::Establishing);
    }

    #[test]
    fn test_stream_message_variants() {
        let id = Uuid::new_v4();
        let mapping = PortMapping {
            local_port: 8080,
            target_host: "localhost".to_string(),
            target_port: 3000,
        };

        // Test Setup message
        let setup_msg = StreamMessage::Setup {
            connection_id: id,
            mapping: mapping.clone(),
        };

        match setup_msg {
            StreamMessage::Setup {
                connection_id,
                mapping: _,
            } => {
                assert_eq!(connection_id, id);
            }
            _ => panic!("Expected Setup message"),
        }

        // Test Close message
        let close_msg = StreamMessage::Close {
            connection_id: id,
            reason: CloseReason::ClientDisconnected,
        };

        match close_msg {
            StreamMessage::Close {
                connection_id,
                reason,
            } => {
                assert_eq!(connection_id, id);
                assert_eq!(reason, CloseReason::ClientDisconnected);
            }
            _ => panic!("Expected Close message"),
        }
    }

    #[test]
    fn test_stream_error_creation() {
        let id = Uuid::new_v4();

        // Test various error types
        let conn_err = StreamError::connection_failed("Connection refused");
        assert!(matches!(conn_err, StreamError::ConnectionFailed(_)));

        let stream_err = StreamError::stream_broken(id, "Stream closed unexpectedly");
        assert!(matches!(stream_err, StreamError::StreamBroken { .. }));

        let protocol_err = StreamError::protocol_error("Invalid message format");
        assert!(matches!(protocol_err, StreamError::ProtocolError(_)));

        let timeout_err = StreamError::timeout(std::time::Duration::from_secs(30));
        assert!(matches!(timeout_err, StreamError::Timeout { .. }));
    }

    #[test]
    fn test_error_recoverability() {
        let recoverable_err = StreamError::connection_failed("Temporary failure");
        assert!(recoverable_err.is_recoverable());

        let non_recoverable_err = StreamError::protocol_error("Invalid format");
        assert!(!non_recoverable_err.is_recoverable());
    }

    #[test]
    fn test_error_categories() {
        let conn_err = StreamError::connection_failed("Test");
        assert_eq!(conn_err.category(), "connection");

        let protocol_err = StreamError::protocol_error("Test");
        assert_eq!(protocol_err.category(), "protocol");

        let timeout_err = StreamError::timeout(std::time::Duration::from_secs(1));
        assert_eq!(timeout_err.category(), "timeout");
    }

    #[test]
    fn test_connection_stats() {
        let mut stats = ConnectionStats::new();
        assert_eq!(stats.total_connections, 0);
        assert_eq!(stats.active_connections, 0);

        stats.increment_total();
        stats.increment_active();
        assert_eq!(stats.total_connections, 1);
        assert_eq!(stats.active_connections, 1);

        stats.decrement_active();
        assert_eq!(stats.active_connections, 0);
    }
}
