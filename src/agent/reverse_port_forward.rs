//! Agent-side Portal response and connection lifecycle primitives.
//!
//! Control-loop writes and data-plane tasks share one bounded response bus.
//! Connection writers live in an explicit registry so duplicate setup and
//! cleanup cannot silently replace or leak sockets.

use crate::error::{LabyrinthError, Result};
use crate::protocol::Message;
use crate::streaming::models::{CloseReason, DataDirection, PortMapping, StreamMessage};
use crate::streaming::ConnectionId;

pub use crate::portal::target_address;
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::time::timeout;

/// Bounded to apply backpressure to background Portal and shell producers.
pub const RESPONSE_CHANNEL_CAPACITY: usize = 1024;
/// Prevent one malformed/buggy producer from exhausting agent memory.
pub const MAX_PORTAL_PAYLOAD: usize = 1024 * 1024;
pub const PORTAL_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub type ResponseReceiver = Arc<Mutex<mpsc::Receiver<Message>>>;
pub type ResponseChannel = (mpsc::Sender<Message>, ResponseReceiver);

/// Construct isolated response bus. Useful for tests and embedders; normal
/// agent runtime uses process-global `get_response_channel`.
pub fn new_response_channel(capacity: usize) -> ResponseChannel {
    let capacity = capacity.max(1);
    let (tx, rx) = mpsc::channel(capacity);
    (tx, Arc::new(Mutex::new(rx)))
}

static RESPONSE_CHANNEL: OnceLock<ResponseChannel> = OnceLock::new();

pub fn get_response_channel() -> &'static ResponseChannel {
    RESPONSE_CHANNEL.get_or_init(|| new_response_channel(RESPONSE_CHANNEL_CAPACITY))
}

/// Send background response through shared control-loop bus. Closed bus means
/// control connection already ended; callers receive explicit failure.
pub async fn send_response(message: Message) -> Result<()> {
    get_response_channel()
        .0
        .send(message)
        .await
        .map_err(|_| LabyrinthError::Message("agent response channel closed".into()))
}

pub async fn connect_target(mapping: &PortMapping) -> Result<tokio::net::TcpStream> {
    let address = target_address(mapping)?;
    timeout(
        PORTAL_CONNECT_TIMEOUT,
        tokio::net::TcpStream::connect(address),
    )
    .await
    .map_err(|_| {
        LabyrinthError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "Portal target connect timeout",
        ))
    })?
    .map_err(LabyrinthError::Io)
}

/// Registry entry for one target writer. `OwnedWriteHalf` preserves read side
/// ownership in its dedicated task, allowing independent half-close.
type Writer = Arc<Mutex<OwnedWriteHalf>>;

/// Agent Portal writer registry. One owner, one writer per connection ID.
#[derive(Default)]
pub struct PortalConnectionRegistry {
    writers: RwLock<HashMap<ConnectionId, Writer>>,
}

impl PortalConnectionRegistry {
    pub async fn insert(&self, connection_id: ConnectionId, writer: OwnedWriteHalf) -> Result<()> {
        let mut writers = self.writers.write().await;
        if writers.contains_key(&connection_id) {
            return Err(LabyrinthError::Message(format!(
                "Portal connection {} already registered",
                connection_id
            )));
        }
        writers.insert(connection_id, Arc::new(Mutex::new(writer)));
        Ok(())
    }

    pub async fn write(&self, connection_id: &ConnectionId, payload: &Bytes) -> Result<()> {
        if payload.len() > MAX_PORTAL_PAYLOAD {
            return Err(LabyrinthError::Message(format!(
                "Portal payload exceeds {} bytes",
                MAX_PORTAL_PAYLOAD
            )));
        }
        let writer = {
            let writers = self.writers.read().await;
            writers.get(connection_id).cloned()
        }
        .ok_or_else(|| {
            LabyrinthError::Message(format!("Unknown Portal connection {}", connection_id))
        })?;
        writer.lock().await.write_all(payload).await?;
        Ok(())
    }

    /// Remove and half-close writer. Returns false when already cleaned up.
    pub async fn close(&self, connection_id: &ConnectionId) -> Result<bool> {
        let writer = self.writers.write().await.remove(connection_id);
        if let Some(writer) = writer {
            writer.lock().await.shutdown().await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Idempotent cleanup used on setup failure, close, and control disconnect.
    pub async fn remove(&self, connection_id: &ConnectionId) -> bool {
        self.writers.write().await.remove(connection_id).is_some()
    }

    pub async fn contains(&self, connection_id: &ConnectionId) -> bool {
        self.writers.read().await.contains_key(connection_id)
    }

    pub async fn len(&self) -> usize {
        self.writers.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.writers.read().await.is_empty()
    }

    /// Close all writers. Error from first shutdown is returned after all
    /// entries are removed, so cleanup never leaves stale registry state.
    pub async fn close_all(&self) -> Result<usize> {
        let writers: Vec<_> = self
            .writers
            .write()
            .await
            .drain()
            .map(|(_, writer)| writer)
            .collect();
        let count = writers.len();
        let mut first_error = None;
        for writer in writers {
            if let Err(error) = writer.lock().await.shutdown().await {
                first_error.get_or_insert(error);
            }
        }
        if let Some(error) = first_error {
            return Err(LabyrinthError::Io(error));
        }
        Ok(count)
    }
}

/// Agent Portal data plane: dials targets, owns writer registration, and pumps
/// target bytes back to the server. Registry and response bus are injected so
/// the data plane can run in isolation from process-global agent state.
#[derive(Clone)]
pub struct AgentPortal {
    registry: Arc<PortalConnectionRegistry>,
    responses: mpsc::Sender<Message>,
}

impl AgentPortal {
    pub fn new(registry: Arc<PortalConnectionRegistry>, responses: mpsc::Sender<Message>) -> Self {
        Self {
            registry,
            responses,
        }
    }

    pub fn registry(&self) -> &PortalConnectionRegistry {
        &self.registry
    }

    pub async fn handle(&self, message: StreamMessage) -> Result<()> {
        match message {
            StreamMessage::Setup {
                connection_id,
                mapping,
            } => self.setup(connection_id, &mapping).await,
            StreamMessage::Data {
                connection_id,
                payload,
                direction: DataDirection::ClientToTarget,
            } => self.forward_to_target(connection_id, &payload).await,
            StreamMessage::Close { connection_id, .. } => {
                self.registry.close(&connection_id).await?;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    async fn setup(&self, connection_id: ConnectionId, mapping: &PortMapping) -> Result<()> {
        let stream = match connect_target(mapping).await {
            Ok(stream) => stream,
            Err(error) => {
                self.send_setup_ack(
                    connection_id,
                    Some(format!(
                        "Failed to connect to target {}:{}: {}",
                        mapping.target_host, mapping.target_port, error
                    )),
                )
                .await?;
                return Err(error);
            }
        };

        let (read_half, write_half) = stream.into_split();
        if let Err(error) = self.registry.insert(connection_id, write_half).await {
            self.send_setup_ack(connection_id, Some(error.to_string()))
                .await?;
            return Err(error);
        }
        if let Err(error) = self.send_setup_ack(connection_id, None).await {
            self.registry.remove(&connection_id).await;
            return Err(error);
        }

        tokio::spawn(self.clone().pump_target_to_server(connection_id, read_half));
        Ok(())
    }

    async fn forward_to_target(&self, connection_id: ConnectionId, payload: &Bytes) -> Result<()> {
        if let Err(error) = self.registry.write(&connection_id, payload).await {
            // Server still believes the stream is live; tell it to release the client.
            self.registry.remove(&connection_id).await;
            let _ = self
                .responses
                .send(Message::Stream(StreamMessage::Close {
                    connection_id,
                    reason: CloseReason::ProtocolError(error.to_string()),
                }))
                .await;
            return Err(error);
        }
        Ok(())
    }

    async fn pump_target_to_server(self, connection_id: ConnectionId, mut target: OwnedReadHalf) {
        let mut buf = vec![0u8; 64 * 1024];
        let reason = loop {
            match target.read(&mut buf).await {
                Ok(0) => break CloseReason::ClientDisconnected,
                Ok(read) => {
                    let data = Message::Stream(StreamMessage::Data {
                        connection_id,
                        payload: Bytes::copy_from_slice(&buf[..read]),
                        direction: DataDirection::TargetToClient,
                    });
                    if self.responses.send(data).await.is_err() {
                        // Control loop is gone; nobody is left to notify.
                        self.registry.remove(&connection_id).await;
                        return;
                    }
                }
                Err(error) => break CloseReason::ProtocolError(error.to_string()),
            }
        };
        self.registry.remove(&connection_id).await;
        let _ = self
            .responses
            .send(Message::Stream(StreamMessage::Close {
                connection_id,
                reason,
            }))
            .await;
    }

    async fn send_setup_ack(
        &self,
        connection_id: ConnectionId,
        error_message: Option<String>,
    ) -> Result<()> {
        self.responses
            .send(Message::Stream(StreamMessage::SetupAck {
                connection_id,
                success: error_message.is_none(),
                error_message,
            }))
            .await
            .map_err(|_| LabyrinthError::Message("agent response channel closed".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::models::ConnectionId;
    use tokio::io::AsyncReadExt;
    use tokio::net::{TcpListener, TcpStream};
    use tokio::time::Duration;

    const STEP: Duration = Duration::from_secs(5);

    fn isolated_portal() -> (AgentPortal, mpsc::Receiver<Message>) {
        let (tx, rx) = mpsc::channel(64);
        (
            AgentPortal::new(Arc::new(PortalConnectionRegistry::default()), tx),
            rx,
        )
    }

    fn mapping_for(addr: std::net::SocketAddr) -> PortMapping {
        PortMapping {
            local_port: 0,
            target_host: addr.ip().to_string(),
            target_port: addr.port(),
        }
    }

    async fn next(rx: &mut mpsc::Receiver<Message>) -> StreamMessage {
        match timeout(STEP, rx.recv()).await.unwrap().unwrap() {
            Message::Stream(message) => message,
            other => panic!("expected stream message, got {other:?}"),
        }
    }

    /// Bound but never listening: dials are refused, and holding the socket
    /// keeps a parallel test from reusing the port (bind-then-drop races).
    fn refusing_port() -> (tokio::net::TcpSocket, std::net::SocketAddr) {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = socket.local_addr().unwrap();
        (socket, addr)
    }

    #[tokio::test]
    async fn new_response_channel_clamps_zero_capacity() {
        let (tx, rx) = new_response_channel(0);
        tx.send(Message::Pong).await.unwrap();
        assert!(matches!(rx.lock().await.recv().await, Some(Message::Pong)));
    }

    #[tokio::test]
    async fn portal_setup_acks_and_relays_both_directions() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_addr = listener.local_addr().unwrap();
        let target = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4];
            socket.read_exact(&mut request).await.unwrap();
            socket.write_all(b"pong").await.unwrap();
            request
        });

        let (portal, mut rx) = isolated_portal();
        let id = ConnectionId::new_v4();
        timeout(
            STEP,
            portal.handle(StreamMessage::Setup {
                connection_id: id,
                mapping: mapping_for(target_addr),
            }),
        )
        .await
        .unwrap()
        .unwrap();

        match next(&mut rx).await {
            StreamMessage::SetupAck {
                connection_id,
                success,
                error_message,
            } => {
                assert_eq!(connection_id, id);
                assert!(success);
                assert!(error_message.is_none());
            }
            other => panic!("expected SetupAck, got {other:?}"),
        }
        assert!(portal.registry().contains(&id).await);

        portal
            .handle(StreamMessage::Data {
                connection_id: id,
                payload: Bytes::from_static(b"ping"),
                direction: DataDirection::ClientToTarget,
            })
            .await
            .unwrap();
        assert_eq!(&target.await.unwrap(), b"ping");

        match next(&mut rx).await {
            StreamMessage::Data {
                connection_id,
                payload,
                direction,
            } => {
                assert_eq!(connection_id, id);
                assert_eq!(&payload[..], b"pong");
                assert_eq!(direction, DataDirection::TargetToClient);
            }
            other => panic!("expected Data, got {other:?}"),
        }

        // Target task dropped its socket: agent must report Close and forget the writer.
        match next(&mut rx).await {
            StreamMessage::Close {
                connection_id,
                reason,
            } => {
                assert_eq!(connection_id, id);
                assert_eq!(reason, CloseReason::ClientDisconnected);
            }
            other => panic!("expected Close, got {other:?}"),
        }
        assert!(!portal.registry().contains(&id).await);
    }

    #[tokio::test]
    async fn portal_setup_failure_sends_negative_ack_and_registers_nothing() {
        let (portal, mut rx) = isolated_portal();
        let id = ConnectionId::new_v4();
        let (_reserved, refusing) = refusing_port();
        let result = portal
            .handle(StreamMessage::Setup {
                connection_id: id,
                mapping: mapping_for(refusing),
            })
            .await;
        assert!(result.is_err());

        match next(&mut rx).await {
            StreamMessage::SetupAck {
                connection_id,
                success,
                error_message,
            } => {
                assert_eq!(connection_id, id);
                assert!(!success);
                assert!(error_message.unwrap().contains("Failed to connect"));
            }
            other => panic!("expected failed SetupAck, got {other:?}"),
        }
        assert_eq!(portal.registry().len().await, 0);
    }

    #[tokio::test]
    async fn portal_rejects_invalid_mapping_without_dialing() {
        let (portal, mut rx) = isolated_portal();
        let id = ConnectionId::new_v4();
        let result = portal
            .handle(StreamMessage::Setup {
                connection_id: id,
                mapping: PortMapping {
                    local_port: 1,
                    target_host: "bad host".into(),
                    target_port: 80,
                },
            })
            .await;
        assert!(result.is_err());
        assert!(matches!(
            next(&mut rx).await,
            StreamMessage::SetupAck { success: false, .. }
        ));
    }

    #[tokio::test]
    async fn portal_duplicate_setup_is_rejected_and_keeps_original() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _accept = tokio::spawn(async move {
            let mut sockets = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                sockets.push(socket);
            }
        });

        let (portal, mut rx) = isolated_portal();
        let id = ConnectionId::new_v4();
        let setup = || StreamMessage::Setup {
            connection_id: id,
            mapping: mapping_for(addr),
        };
        portal.handle(setup()).await.unwrap();
        assert!(matches!(
            next(&mut rx).await,
            StreamMessage::SetupAck { success: true, .. }
        ));

        assert!(portal.handle(setup()).await.is_err());
        assert!(matches!(
            next(&mut rx).await,
            StreamMessage::SetupAck { success: false, .. }
        ));
        assert_eq!(portal.registry().len().await, 1);
    }

    #[tokio::test]
    async fn portal_data_for_unknown_connection_notifies_server() {
        let (portal, mut rx) = isolated_portal();
        let id = ConnectionId::new_v4();
        let result = portal
            .handle(StreamMessage::Data {
                connection_id: id,
                payload: Bytes::from_static(b"orphan"),
                direction: DataDirection::ClientToTarget,
            })
            .await;
        assert!(result.is_err());
        assert!(matches!(
            next(&mut rx).await,
            StreamMessage::Close {
                connection_id,
                reason: CloseReason::ProtocolError(_),
            } if connection_id == id
        ));
    }

    #[tokio::test]
    async fn portal_ignores_target_to_client_and_unrelated_messages() {
        let (portal, mut rx) = isolated_portal();
        let id = ConnectionId::new_v4();
        portal
            .handle(StreamMessage::Data {
                connection_id: id,
                payload: Bytes::from_static(b"x"),
                direction: DataDirection::TargetToClient,
            })
            .await
            .unwrap();
        portal
            .handle(StreamMessage::Heartbeat {
                connection_id: id,
                timestamp: 1,
            })
            .await
            .unwrap();
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn portal_close_is_idempotent() {
        let (portal, _rx) = isolated_portal();
        let id = ConnectionId::new_v4();
        let close = || StreamMessage::Close {
            connection_id: id,
            reason: CloseReason::UserRequested,
        };
        portal.handle(close()).await.unwrap();
        portal.handle(close()).await.unwrap();
    }

    #[tokio::test]
    async fn portal_setup_fails_cleanly_when_response_bus_closed() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _accept = tokio::spawn(async move { listener.accept().await });

        let (portal, rx) = isolated_portal();
        drop(rx);
        let id = ConnectionId::new_v4();
        let result = portal
            .handle(StreamMessage::Setup {
                connection_id: id,
                mapping: mapping_for(addr),
            })
            .await;
        assert!(result.is_err());
        assert!(!portal.registry().contains(&id).await);
    }

    #[tokio::test]
    async fn registry_close_all_drains_every_writer() {
        let registry = PortalConnectionRegistry::default();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut peers = Vec::new();
        for _ in 0..3 {
            let client = TcpStream::connect(addr).await.unwrap();
            let (server_side, _) = listener.accept().await.unwrap();
            registry
                .insert(ConnectionId::new_v4(), client.into_split().1)
                .await
                .unwrap();
            peers.push(server_side);
        }
        assert_eq!(registry.close_all().await.unwrap(), 3);
        assert_eq!(registry.len().await, 0);
        for mut peer in peers {
            let mut probe = [0u8; 1];
            assert_eq!(
                timeout(STEP, peer.read(&mut probe)).await.unwrap().unwrap(),
                0
            );
        }
        assert_eq!(registry.close_all().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn isolated_response_channel_preserves_backpressure_contract() {
        let (sender, receiver) = new_response_channel(1);
        sender.send(Message::Ping).await.unwrap();
        let mut guard = receiver.lock().await;
        assert!(matches!(guard.recv().await, Some(Message::Ping)));
    }

    #[tokio::test]
    async fn registry_rejects_duplicate_and_cleans_up() {
        let registry = PortalConnectionRegistry::default();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client = tokio::spawn(async move { TcpStream::connect(addr).await.unwrap() });
        let (stream, _) = listener.accept().await.unwrap();
        let (_read, writer) = stream.into_split();
        let id = ConnectionId::new_v4();
        registry.insert(id, writer).await.unwrap();
        let duplicate = registry
            .insert(id, TcpStream::connect(addr).await.unwrap().into_split().1)
            .await;
        assert!(duplicate.is_err());
        assert!(registry.contains(&id).await);
        assert!(registry.remove(&id).await);
        assert!(!registry.remove(&id).await);
        client.abort();
    }

    #[tokio::test]
    async fn registry_writes_and_close_half_closes_target() {
        let registry = PortalConnectionRegistry::default();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client = tokio::spawn(async move {
            let mut stream = TcpStream::connect(addr).await.unwrap();
            let mut received = vec![0u8; 4];
            stream.read_exact(&mut received).await.unwrap();
            let mut eof_probe = [0u8; 1];
            let read = stream.read(&mut eof_probe).await.unwrap();
            (received, read)
        });
        let (stream, _) = listener.accept().await.unwrap();
        let (id, writer) = (ConnectionId::new_v4(), stream.into_split().1);
        registry.insert(id, writer).await.unwrap();
        registry
            .write(&id, &Bytes::from_static(b"test"))
            .await
            .unwrap();
        assert!(registry.close(&id).await.unwrap());
        let (received, eof) = client.await.unwrap();
        assert_eq!(received, b"test");
        assert_eq!(eof, 0);
    }

    #[tokio::test]
    async fn registry_rejects_oversized_payload_and_unknown_connection() {
        let registry = PortalConnectionRegistry::default();
        let unknown = ConnectionId::new_v4();
        assert!(registry
            .write(&unknown, &Bytes::from_static(b"x"))
            .await
            .is_err());
        let oversized = Bytes::from(vec![0u8; MAX_PORTAL_PAYLOAD + 1]);
        assert!(registry.write(&unknown, &oversized).await.is_err());
    }
}
