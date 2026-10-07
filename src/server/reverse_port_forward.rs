//! Server-side Portal lifecycle and TCP relay primitives.
//!
//! This module owns policy that is easy to accidentally duplicate in the CLI:
//! endpoint validation, bounded listener concurrency, cancellation, and
//! half-close aware relaying.  The server workflow supplies the connection
//! handler; this module does not know about agent or stream-manager state.

use crate::error::{LabyrinthError, Result};
use crate::streaming::models::PortMapping;
use async_trait::async_trait;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, Mutex, Semaphore};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tracing::{debug, error, warn};

pub use crate::portal::{
    parse_mapping, read_quic_setup, read_quic_setup_ack, target_address, validate_mapping,
    MAX_SETUP_FRAME,
};

/// Maximum concurrent client connections per Portal listener.
pub const DEFAULT_MAX_CONNECTIONS: usize = 256;
/// Bound target connection establishment.  A dead route must not pin a task.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound inactive streams while preserving long-lived active connections.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Keep individual Portal payloads bounded when handlers use this registry.
pub const MAX_PORTAL_PAYLOAD: usize = 1024 * 1024;

/// Result of a completed Portal relay.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RelayStats {
    pub client_to_target: u64,
    pub target_to_client: u64,
}

/// Relay two TCP streams with backpressure and independent half-close.
///
/// EOF in one direction shuts down only the corresponding writer.  The other
/// direction can finish normally, matching TCP FIN semantics.  Idle timeout
/// applies per direction, so active transfers are not killed by a wall clock.
pub async fn relay_tcp(
    client: TcpStream,
    target: TcpStream,
    idle_timeout: Option<Duration>,
) -> Result<RelayStats> {
    let (mut client_reader, mut client_writer) = client.into_split();
    let (mut target_reader, mut target_writer) = target.into_split();

    let client_to_target = copy_direction(
        &mut client_reader,
        &mut target_writer,
        idle_timeout,
        "client-to-target",
    );
    let target_to_client = copy_direction(
        &mut target_reader,
        &mut client_writer,
        idle_timeout,
        "target-to-client",
    );
    let (client_to_target, target_to_client) =
        tokio::try_join!(client_to_target, target_to_client).map_err(LabyrinthError::Io)?;
    Ok(RelayStats {
        client_to_target,
        target_to_client,
    })
}

async fn copy_direction<R, W>(
    reader: &mut R,
    writer: &mut W,
    idle_timeout: Option<Duration>,
    direction: &'static str,
) -> io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buffer = vec![0u8; 64 * 1024];
    let mut transferred = 0u64;
    loop {
        let read = match idle_timeout {
            Some(duration) => timeout(duration, reader.read(&mut buffer))
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::TimedOut, format!("{direction} idle timeout"))
                })?,
            None => reader.read(&mut buffer).await,
        }?;
        if read == 0 {
            writer.shutdown().await?;
            return Ok(transferred);
        }

        match idle_timeout {
            Some(duration) => timeout(duration, writer.write_all(&buffer[..read]))
                .await
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("{direction} write timeout"),
                    )
                })??,
            None => writer.write_all(&buffer[..read]).await?,
        }
        transferred += read as u64;
    }
}

/// Handler supplied by server workflow.  Keeps listener mechanics independent
/// from stream-manager and agent state.
#[async_trait]
pub trait PortalConnectionHandler: Send + Sync + 'static {
    async fn handle(&self, stream: TcpStream, peer: SocketAddr, mapping: PortMapping)
        -> Result<()>;
}

/// Bounded, cancellable Portal listener.  `stop` aborts all active handlers;
/// dropping listener has same safety property as a last-resort cleanup.
pub struct PortalListener {
    local_addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    accept_task: Option<JoinHandle<()>>,
    active_tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl PortalListener {
    pub async fn bind<H>(
        mapping: PortMapping,
        max_connections: usize,
        handler: Arc<H>,
    ) -> Result<Self>
    where
        H: PortalConnectionHandler,
    {
        validate_mapping(&mapping)?;
        if max_connections == 0 {
            return Err(LabyrinthError::Message(
                "Portal max connections must be greater than zero".into(),
            ));
        }

        let listener = TcpListener::bind(("127.0.0.1", mapping.local_port)).await?;
        let local_addr = listener.local_addr()?;
        let (shutdown, mut shutdown_rx) = oneshot::channel();
        let active_tasks = Arc::new(Mutex::new(Vec::new()));
        let active_for_task = Arc::clone(&active_tasks);
        let permits = Arc::new(Semaphore::new(max_connections));
        let mapping_for_task = mapping.clone();
        let accept_task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => {
                        let (stream, peer) = match accepted {
                            Ok(value) => value,
                            Err(error) => {
                                // Listener errors are terminal. Existing handlers are
                                // still aborted by stop/drop.
                                error!(%error, %local_addr, "Portal accept failed");
                                break;
                            }
                        };
                        let permit = match Arc::clone(&permits).try_acquire_owned() {
                            Ok(permit) => permit,
                            Err(_) => {
                                debug!(%peer, "Portal connection limit reached");
                                drop(stream);
                                continue;
                            }
                        };
                        let handler = Arc::clone(&handler);
                        let mapping = mapping_for_task.clone();
                        let task = tokio::spawn(async move {
                            let _permit = permit;
                            if let Err(error) = handler.handle(stream, peer, mapping).await {
                                warn!(%peer, %error, "Portal connection failed");
                            }
                        });
                        active_for_task.lock().await.push(task);
                    }
                }
            }
        });

        Ok(Self {
            local_addr,
            shutdown: Some(shutdown),
            accept_task: Some(accept_task),
            active_tasks,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.accept_task.take() {
            let _ = task.await;
        }
        let tasks = std::mem::take(&mut *self.active_tasks.lock().await);
        for task in tasks {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for PortalListener {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.accept_task.take() {
            task.abort();
        }
        // Drop cannot await. Abort every accepted task so no client socket is
        // retained after listener ownership ends.
        if let Ok(mut tasks) = self.active_tasks.try_lock() {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
    }
}

/// Connect to mapping target with bounded DNS/TCP establishment.
pub async fn connect_target(
    mapping: &PortMapping,
    timeout_duration: Duration,
) -> Result<TcpStream> {
    let address = target_address(mapping)?;
    timeout(timeout_duration, TcpStream::connect(address))
        .await
        .map_err(|_| {
            LabyrinthError::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                "Portal target connect timeout",
            ))
        })?
        .map_err(LabyrinthError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn mapping(local_port: u16, host: &str, target_port: u16) -> PortMapping {
        PortMapping {
            local_port,
            target_host: host.into(),
            target_port,
        }
    }

    #[tokio::test]
    async fn duplicate_listener_bind_is_rejected() {
        let probe = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind test probe: {error}"),
        };
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let first = match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => listener,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind first listener: {error}"),
        };
        let second = TcpListener::bind(("127.0.0.1", port)).await;
        assert!(second.is_err());
        drop(first);
    }

    #[tokio::test]
    async fn relay_preserves_half_close_and_backpressure() {
        let target_listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind target listener: {error}"),
        };
        let target_addr = target_listener.local_addr().unwrap();
        let target_task = tokio::spawn(async move {
            let (mut target, _) = target_listener.accept().await.unwrap();
            let mut input = Vec::new();
            target.read_to_end(&mut input).await.unwrap();
            target.write_all(b"reply").await.unwrap();
            target.shutdown().await.unwrap();
            input
        });

        let client_listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind client listener: {error}"),
        };
        let client_addr = client_listener.local_addr().unwrap();
        let client_task = tokio::spawn(async move {
            let (mut client_side, _) = client_listener.accept().await.unwrap();
            client_side.write_all(b"request").await.unwrap();
            client_side.shutdown().await.unwrap();
            let mut reply = Vec::new();
            client_side.read_to_end(&mut reply).await.unwrap();
            reply
        });

        let client = TcpStream::connect(client_addr).await.unwrap();
        let target = TcpStream::connect(target_addr).await.unwrap();
        let stats = relay_tcp(client, target, Some(Duration::from_secs(2)))
            .await
            .unwrap();
        assert_eq!(stats.client_to_target, 7);
        assert_eq!(stats.target_to_client, 5);
        assert_eq!(target_task.await.unwrap(), b"request");
        assert_eq!(client_task.await.unwrap(), b"reply");
    }

    struct CountingHandler(AtomicUsize);

    #[async_trait]
    impl PortalConnectionHandler for CountingHandler {
        async fn handle(
            &self,
            _stream: TcpStream,
            _peer: SocketAddr,
            _mapping: PortMapping,
        ) -> Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn listener_shutdown_stops_accept_loop() {
        let handler = Arc::new(CountingHandler(AtomicUsize::new(0)));
        let probe = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind test probe: {error}"),
        };
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let listener = match PortalListener::bind(mapping(port, "127.0.0.1", 1), 1, handler.clone())
            .await
        {
            Ok(listener) => listener,
            Err(LabyrinthError::Io(error)) if error.kind() == io::ErrorKind::PermissionDenied => {
                return
            }
            Err(error) => panic!("failed to bind Portal listener: {error}"),
        };
        let addr = listener.local_addr();
        let _ = TcpStream::connect(addr).await.unwrap();
        tokio::task::yield_now().await;
        listener.stop().await;
        assert_eq!(handler.0.load(Ordering::SeqCst), 1);
    }
}
