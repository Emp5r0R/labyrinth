//! Data models for the streaming architecture

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{broadcast, mpsc, Mutex};
use uuid::Uuid;

/// Unique identifier for a connection
pub type ConnectionId = Uuid;

/// Port mapping configuration for reverse port forwarding
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PortMapping {
    pub local_port: u16,
    pub target_host: String,
    pub target_port: u16,
}

/// Connection state information
#[derive(Debug, Clone)]
pub struct ConnectionState {
    #[allow(dead_code)]
    pub id: ConnectionId,
    pub client_addr: SocketAddr,
    pub target_mapping: PortMapping,
    pub created_at: Instant,
    #[allow(dead_code)]
    pub bytes_sent: Arc<AtomicU64>,
    #[allow(dead_code)]
    pub bytes_received: Arc<AtomicU64>,
    pub status: ConnectionStatus,
}

impl ConnectionState {
    pub fn new(id: ConnectionId, client_addr: SocketAddr, target_mapping: PortMapping) -> Self {
        Self {
            id,
            client_addr,
            target_mapping,
            created_at: Instant::now(),
            bytes_sent: Arc::new(AtomicU64::new(0)),
            bytes_received: Arc::new(AtomicU64::new(0)),
            status: ConnectionStatus::Establishing,
        }
    }
}

/// Status of a connection
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConnectionStatus {
    Establishing,
    Active,
    Closing,
    Closed,
    Error(String),
}

/// Context for managing a bidirectional stream
#[derive(Debug)]
pub struct StreamContext {
    #[allow(dead_code)]
    pub connection_id: ConnectionId,
    pub client_read: Arc<Mutex<OwnedReadHalf>>,
    pub client_write: Arc<Mutex<OwnedWriteHalf>>,
    #[allow(dead_code)]
    pub agent_sender: mpsc::Sender<StreamMessage>,
    #[allow(dead_code)]
    pub shutdown_signal: broadcast::Receiver<()>,
    #[allow(dead_code)]
    pub created_at: Instant,
}

impl StreamContext {
    pub fn new(
        connection_id: ConnectionId,
        client_read: OwnedReadHalf,
        client_write: OwnedWriteHalf,
        agent_sender: mpsc::Sender<StreamMessage>,
        shutdown_signal: broadcast::Receiver<()>,
    ) -> Self {
        Self {
            connection_id,
            client_read: Arc::new(Mutex::new(client_read)),
            client_write: Arc::new(Mutex::new(client_write)),
            agent_sender,
            shutdown_signal,
            created_at: Instant::now(),
        }
    }
}

/// Messages used in the streaming protocol
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamMessage {
    /// Request to set up a new stream
    StreamRequest(StreamRequest),
    /// Response to a stream request
    StreamResponse(StreamResponse),
    /// Data flowing through the stream
    StreamData {
        stream_id: String,
        data: Vec<u8>,
    },
    /// Close a stream
    StreamClose(String),
    /// Setup a new stream connection
    Setup {
        connection_id: ConnectionId,
        mapping: PortMapping,
    },
    /// Data flowing through the stream
    Data {
        connection_id: ConnectionId,
        payload: Bytes,
        direction: DataDirection,
    },
    /// Close a stream connection
    Close {
        connection_id: ConnectionId,
        reason: CloseReason,
    },
    /// Acknowledge stream setup
    SetupAck {
        connection_id: ConnectionId,
        success: bool,
        error_message: Option<String>,
    },
    /// Heartbeat/keepalive message
    Heartbeat {
        connection_id: ConnectionId,
        timestamp: u64,
    },
}

/// Request to set up a new stream for reverse port forwarding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamRequest {
    pub stream_id: String,
    pub port: u16,
    pub target_host: String,
    pub target_port: u16,
}

/// Response to a stream request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamResponse {
    pub stream_id: String,
    pub success: bool,
    pub error_message: Option<String>,
}

/// Direction of data flow
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DataDirection {
    ClientToTarget,
    TargetToClient,
}

/// Reasons for closing a stream
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CloseReason {
    ClientDisconnected,
    TargetUnreachable,
    ProtocolError(String),
    Timeout,
    ResourceExhausted,
    Shutdown,
    UserRequested,
}

/// Statistics for connection management
#[derive(Debug, Clone, Default)]
pub struct ConnectionStats {
    pub total_connections: usize,
    pub active_connections: usize,
    pub failed_connections: usize,
    #[allow(dead_code)]
    pub total_bytes_transferred: u64,
    pub connections_by_status: HashMap<String, usize>,
    #[allow(dead_code)]
    pub average_connection_duration_ms: u64,
}

impl ConnectionStats {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn increment_total(&mut self) {
        self.total_connections += 1;
    }

    pub fn increment_active(&mut self) {
        self.active_connections += 1;
    }

    pub fn decrement_active(&mut self) {
        if self.active_connections > 0 {
            self.active_connections -= 1;
        }
    }

    pub fn increment_failed(&mut self) {
        self.failed_connections += 1;
    }

    // Removed unused method add_bytes_transferred

    pub fn update_status_count(&mut self, status: &ConnectionStatus) {
        let status_key = match status {
            ConnectionStatus::Establishing => "establishing".to_string(),
            ConnectionStatus::Active => "active".to_string(),
            ConnectionStatus::Closing => "closing".to_string(),
            ConnectionStatus::Closed => "closed".to_string(),
            ConnectionStatus::Error(_) => "error".to_string(),
        };
        
        *self.connections_by_status.entry(status_key).or_insert(0) += 1;
    }
}