//! Agent-facing transport listeners.
//!
//! Owns TLS/QUIC server configuration and the accept loops. Registration and
//! everything after it belong to `AgentManager`; this module only turns
//! sockets into authenticated-transport streams.

use crate::error::{LabyrinthError, Result};
use crate::framing::HANDSHAKE_TIMEOUT;
use crate::server::agent_manager::AgentManager;
use crate::server::core::LabyrinthServer;
use crate::transport::{parse_socket_addr, QuicBidiStream, TransportMode, CONTROL_ALPN};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;
use tracing::{error, info};

pub fn tls_server_config(
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<rustls::ServerConfig> {
    Ok(rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?)
}

pub fn quic_server_config(
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<quinn::ServerConfig> {
    let mut crypto = tls_server_config(certs, key)?;
    crypto.alpn_protocols = vec![CONTROL_ALPN.to_vec()];
    let quic_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(crypto)
        .map_err(|e| LabyrinthError::Message(format!("Invalid QUIC server config: {}", e)))?;
    Ok(quinn::ServerConfig::with_crypto(Arc::new(quic_crypto)))
}

/// Bind the agent listener and spawn its accept loop. Returns the bound
/// address, which differs from `listen_addr` when port 0 is requested.
pub async fn spawn_agent_listener(
    server: Arc<LabyrinthServer>,
    listen_addr: &str,
    transport: TransportMode,
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<SocketAddr> {
    match transport {
        TransportMode::Tcp => spawn_tcp_agent_listener(server, listen_addr, certs, key).await,
        TransportMode::Quic => spawn_quic_agent_listener(server, listen_addr, certs, key).await,
    }
}

async fn spawn_tcp_agent_listener(
    server: Arc<LabyrinthServer>,
    listen_addr: &str,
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<SocketAddr> {
    let acceptor = TlsAcceptor::from(Arc::new(tls_server_config(certs, key)?));
    let listener = TcpListener::bind(listen_addr).await?;
    let local_addr = listener.local_addr()?;
    info!("TCP/TLS agent listener on {}", local_addr);

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    let acceptor = acceptor.clone();
                    let server = Arc::clone(&server);

                    tokio::spawn(async move {
                        // A peer that opens TCP and never speaks TLS must not pin a task.
                        match timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                            Ok(Ok(tls_stream)) => {
                                if let Err(e) =
                                    AgentManager::register_agent(server, tls_stream, addr).await
                                {
                                    error!("Agent registration failed: {}", e);
                                }
                            }
                            Ok(Err(e)) => error!("TLS handshake failed: {}", e),
                            Err(_) => error!("TLS handshake from {} timed out", addr),
                        }
                    });
                }
                Err(e) => {
                    error!("Failed to accept TCP agent connection: {}", e);
                }
            }
        }
    });

    Ok(local_addr)
}

async fn spawn_quic_agent_listener(
    server: Arc<LabyrinthServer>,
    listen_addr: &str,
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<SocketAddr> {
    let server_config = quic_server_config(certs, key)?;
    let listen_addr = parse_socket_addr(listen_addr)?;
    let endpoint = quinn::Endpoint::server(server_config, listen_addr)?;
    let local_addr = endpoint.local_addr()?;
    info!("QUIC agent listener on {}", local_addr);

    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let server = Arc::clone(&server);
            tokio::spawn(async move {
                let connection = match timeout(HANDSHAKE_TIMEOUT, incoming).await {
                    Ok(Ok(connection)) => connection,
                    Ok(Err(e)) => {
                        error!("QUIC handshake failed: {}", e);
                        return;
                    }
                    Err(_) => {
                        error!("QUIC handshake timed out");
                        return;
                    }
                };
                let remote_addr = connection.remote_address();
                match timeout(HANDSHAKE_TIMEOUT, connection.accept_bi()).await {
                    Ok(Ok((send, recv))) => {
                        let stream_connection = connection.clone();
                        let stream = QuicBidiStream::with_lifetime(send, recv, None, connection);
                        if let Err(e) = AgentManager::register_quic_agent(
                            server,
                            stream,
                            remote_addr,
                            stream_connection,
                        )
                        .await
                        {
                            error!("QUIC agent registration failed: {}", e);
                        }
                    }
                    Ok(Err(e)) => error!("QUIC control stream accept failed: {}", e),
                    Err(_) => {
                        error!("QUIC control stream from {} never opened", remote_addr);
                        connection.close(0u32.into(), b"control stream timeout");
                    }
                }
            });
        }
    });

    Ok(local_addr)
}
