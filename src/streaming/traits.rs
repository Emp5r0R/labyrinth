//! Core traits for the streaming architecture

use crate::streaming::errors::StreamResult;
use crate::streaming::{
    ConnectionId, ConnectionState, ConnectionStats, PortMapping, StreamMessage,
};
use async_trait::async_trait;
use bytes::Bytes;
use tokio::net::TcpStream;

/// Trait for managing connection lifecycle and state
#[async_trait]
pub trait ConnectionManager: Send + Sync {
    /// Handle a new incoming connection and create a connection entry
    #[allow(dead_code)]
    async fn handle_new_connection(
        &self,
        connection: TcpStream,
        mapping: PortMapping,
    ) -> StreamResult<ConnectionId>;

    /// Clean up resources associated with a connection
    async fn cleanup_connection(&self, connection_id: &ConnectionId) -> StreamResult<()>;

    /// Get statistics for all managed connections
    #[allow(dead_code)]
    async fn get_connection_stats(&self) -> StreamResult<ConnectionStats>;

    /// Get the state of a specific connection
    async fn get_connection_state(
        &self,
        connection_id: &ConnectionId,
    ) -> StreamResult<Option<ConnectionState>>;

    /// Track an existing connection that has already been accepted externally.
    async fn track_existing_connection(
        &self,
        connection_id: ConnectionId,
        client_addr: std::net::SocketAddr,
        mapping: PortMapping,
    ) -> StreamResult<()>;

    /// Update the status of a connection
    async fn update_connection_status(
        &self,
        connection_id: &ConnectionId,
        status: crate::streaming::ConnectionStatus,
    ) -> StreamResult<()>;
}

/// Trait for managing bidirectional data streams
#[async_trait]
pub trait StreamManager: Send + Sync {
    /// Create a bidirectional stream for a client connection
    async fn create_bidirectional_stream(
        &self,
        connection_id: ConnectionId,
        client_stream: TcpStream,
    ) -> StreamResult<()>;

    /// Handle incoming data from the agent side
    async fn handle_agent_stream(
        &self,
        connection_id: ConnectionId,
        agent_data: Bytes,
    ) -> StreamResult<()>;

    /// Terminate a specific stream and clean up resources
    async fn terminate_stream(&self, connection_id: ConnectionId) -> StreamResult<()>;

    /// Send data to the client side of a stream
    async fn send_to_client(&self, connection_id: ConnectionId, data: Bytes) -> StreamResult<()>;

    /// Send data to the agent side of a stream
    #[allow(dead_code)]
    async fn send_to_agent(&self, connection_id: ConnectionId, data: Bytes) -> StreamResult<()>;
}

/// Trait for tunnel protocol communication
#[async_trait]
#[allow(dead_code)]
pub trait TunnelProtocol: Send + Sync {
    /// Send stream setup message to establish a new connection
    async fn send_stream_setup(
        &self,
        connection_id: ConnectionId,
        mapping: PortMapping,
    ) -> StreamResult<()>;

    /// Send data through the tunnel
    async fn send_stream_data(&self, connection_id: ConnectionId, data: Bytes) -> StreamResult<()>;

    /// Send stream close message
    async fn send_stream_close(
        &self,
        connection_id: ConnectionId,
        reason: crate::streaming::CloseReason,
    ) -> StreamResult<()>;

    /// Handle incoming stream message
    async fn handle_stream_message(&self, message: StreamMessage) -> StreamResult<()>;
}

/// Trait for resource management and monitoring
#[async_trait]
#[cfg(test)]
pub trait ResourceManager: Send + Sync {
    /// Track a new resource (connection, stream, etc.)
    #[allow(dead_code)]
    async fn track_resource(
        &self,
        connection_id: ConnectionId,
        resource_type: ResourceType,
    ) -> StreamResult<()>;

    /// Release a tracked resource
    #[allow(dead_code)]
    async fn release_resource(&self, connection_id: ConnectionId) -> StreamResult<()>;

    /// Get current resource usage statistics
    async fn get_resource_usage(&self) -> StreamResult<ResourceUsage>;

    /// Perform graceful shutdown of all resources
    #[allow(dead_code)]
    async fn graceful_shutdown(&self) -> StreamResult<()>;

    /// Check if resource limits are exceeded
    #[allow(dead_code)]
    async fn check_resource_limits(&self) -> StreamResult<bool>;
}

/// Types of resources that can be tracked
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub enum ResourceType {
    Connection,
    Stream,
    FileDescriptor,
    Memory(usize),
}

/// Resource usage statistics
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct ResourceUsage {
    pub active_connections: usize,
    pub active_streams: usize,
    pub memory_usage_bytes: usize,
    pub file_descriptors: usize,
}
