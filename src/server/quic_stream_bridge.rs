use crate::error::{LabyrinthError, Result};
use crate::portal;
use crate::server::core::LabyrinthServer;
use crate::server::reverse_port_forward::{
    read_quic_setup_ack, DEFAULT_CONNECT_TIMEOUT, DEFAULT_IDLE_TIMEOUT,
};
use crate::streaming::models::{ConnectionId, ConnectionStatus, PortMapping};
use crate::transport::QuicBidiStream;
use std::sync::Arc;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::{debug, error, info};

pub struct QuicStreamBridge;

impl QuicStreamBridge {
    pub async fn create_bidirectional_stream(
        server: Arc<LabyrinthServer>,
        agent_id: String,
        connection_id: ConnectionId,
        client_socket: TcpStream,
        mapping: PortMapping,
    ) -> Result<()> {
        let connection = {
            let agents = server.agents().read().await;
            agents
                .get(&agent_id)
                .and_then(|agent| agent.quic_connection.clone())
                .ok_or_else(|| {
                    LabyrinthError::Message(format!(
                        "Agent {} is not connected over QUIC",
                        agent_id
                    ))
                })?
        };

        Self::run_stream(server, connection, connection_id, client_socket, mapping).await
    }

    async fn run_stream(
        server: Arc<LabyrinthServer>,
        connection: quinn::Connection,
        connection_id: ConnectionId,
        mut client_socket: TcpStream,
        mapping: PortMapping,
    ) -> Result<()> {
        let (mut send, recv) = connection
            .open_bi()
            .await
            .map_err(|e| LabyrinthError::Message(format!("Failed to open QUIC stream: {}", e)))?;

        portal::write_quic_setup(&mut send, connection_id, mapping).await?;

        let mut reader = BufReader::new(recv);
        if let Err(error) =
            read_quic_setup_ack(&mut reader, DEFAULT_CONNECT_TIMEOUT, connection_id).await
        {
            if let Some(cm) = server.get_connection_manager().await {
                let _ = cm
                    .update_connection_status(
                        &connection_id,
                        ConnectionStatus::Error(error.to_string()),
                    )
                    .await;
            }
            server.cleanup_portal_connection(connection_id).await;
            return Err(error);
        }
        if let Some(cm) = server.get_connection_manager().await {
            let _ = cm
                .update_connection_status(&connection_id, ConnectionStatus::Active)
                .await;
        }

        // Server-speaks-first targets (SSH, SMTP, MySQL) can send their banner
        // in the same flight as the ack; it may already sit in our buffer.
        let early_data = reader.buffer().to_vec();
        if !early_data.is_empty() {
            client_socket.write_all(&early_data).await?;
        }
        let recv = reader.into_inner();
        let mut quic_stream = QuicBidiStream::new(send, recv);
        info!("QUIC native stream active for {}", connection_id);

        match timeout(
            DEFAULT_IDLE_TIMEOUT,
            tokio::io::copy_bidirectional(&mut client_socket, &mut quic_stream),
        )
        .await
        {
            Err(_) => error!("QUIC stream {} idle timeout", connection_id),
            Ok(Err(e)) => {
                error!("QUIC stream {} copy failed: {}", connection_id, e);
            }
            Ok(Ok((client_to_agent, agent_to_client))) => {
                debug!(
                    "QUIC stream {} closed after {} bytes client->agent and {} bytes agent->client",
                    connection_id, client_to_agent, agent_to_client
                );
            }
        }
        server.cleanup_portal_connection(connection_id).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::core::AgentCore;
    use crate::protocol::{AgentInfo, AgentKind};
    use crate::security::SecurityManager;
    use crate::server::core::ConnectedAgent;
    use base64::{engine::general_purpose, Engine as _};
    use std::time::Instant;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::{mpsc, Mutex};
    use tokio::time::{timeout, Duration};

    fn parse_generated_cert(
        cert_pem: &str,
        key_pem: &str,
    ) -> (
        Vec<rustls::pki_types::CertificateDer<'static>>,
        rustls::pki_types::PrivateKeyDer<'static>,
    ) {
        let mut cert_reader = cert_pem.as_bytes();
        let certs = rustls_pemfile::certs(&mut cert_reader)
            .collect::<std::result::Result<Vec<_>, std::io::Error>>()
            .unwrap();
        let mut key_reader = key_pem.as_bytes();
        let mut keys = rustls_pemfile::pkcs8_private_keys(&mut key_reader)
            .collect::<std::result::Result<Vec<_>, std::io::Error>>()
            .unwrap();
        (certs, keys.remove(0).into())
    }

    fn quic_server_config(
        certs: Vec<rustls::pki_types::CertificateDer<'static>>,
        key: rustls::pki_types::PrivateKeyDer<'static>,
    ) -> quinn::ServerConfig {
        let mut crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap();
        crypto.alpn_protocols = vec![b"labyrinth-control/1".to_vec()];
        let quic_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(crypto).unwrap();
        quinn::ServerConfig::with_crypto(Arc::new(quic_crypto))
    }

    fn quic_client_config(cert_pem: &str) -> quinn::ClientConfig {
        let cert_b64 = general_purpose::STANDARD.encode(cert_pem.as_bytes());
        let mut crypto = SecurityManager::create_tls_client_config(Some(cert_b64), None).unwrap();
        crypto.alpn_protocols = vec![b"labyrinth-control/1".to_vec()];
        let quic_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap();
        quinn::ClientConfig::new(Arc::new(quic_crypto))
    }

    #[tokio::test]
    async fn quic_bridge_moves_bytes_to_target() {
        let generated = SecurityManager::generate_self_signed_certificate("localhost").unwrap();
        let (certs, key) = parse_generated_cert(&generated.cert_pem, &generated.key_pem);
        let server_endpoint = quinn::Endpoint::server(
            quic_server_config(certs, key),
            "127.0.0.1:0".parse().unwrap(),
        )
        .unwrap();
        let server_addr = server_endpoint.local_addr().unwrap();
        let mut client_endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        client_endpoint.set_default_client_config(quic_client_config(&generated.cert_pem));

        let connecting = client_endpoint.connect(server_addr, "localhost").unwrap();
        let incoming = server_endpoint.accept().await.unwrap();
        let (client_connection, server_connection) = tokio::join!(connecting, incoming);
        let client_connection = client_connection.unwrap();
        let server_connection = server_connection.unwrap();

        let echo_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_addr = echo_listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = echo_listener.accept().await.unwrap();
            let mut buf = [0_u8; 1024];
            loop {
                let read = socket.read(&mut buf).await.unwrap();
                if read == 0 {
                    break;
                }
                socket.write_all(&buf[..read]).await.unwrap();
            }
        });

        let agent_connection = client_connection.clone();
        tokio::spawn(async move {
            let (send, recv) = agent_connection.accept_bi().await.unwrap();
            AgentCore::handle_quic_stream(send, recv).await.unwrap();
        });

        let local_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_addr = local_listener.local_addr().unwrap();
        let client_task = tokio::spawn(TcpStream::connect(local_addr));
        let (bridge_socket, client_addr) = local_listener.accept().await.unwrap();
        let mut client_socket = client_task.await.unwrap().unwrap();

        let server = Arc::new(LabyrinthServer::new(false, None));
        let (sender, _rx) = mpsc::channel(1);
        let agent_id = "agent-quic".to_string();
        server.agents().write().await.insert(
            agent_id.clone(),
            ConnectedAgent {
                id: agent_id.clone(),
                info: AgentInfo {
                    name: "agent".to_string(),
                    hostname: "agent".to_string(),
                    os: "linux".to_string(),
                    arch: "x86_64".to_string(),
                    interfaces: vec![],
                    auth_key: None,
                    kind: AgentKind::Generic,
                    stable_id: None,
                    listener_addr: None,
                    listener_port: None,
                    connectivity: Default::default(),
                },
                sender,
                transport_label: "quic/udp".to_string(),
                quic_connection: Some(server_connection),
                tunnel_active: false,
                tunnel_subnet: None,
                tun_name: None,
                last_seen: Arc::new(Mutex::new(Instant::now())),
                command_response: Arc::new(Mutex::new(None)),
                shell_events: Arc::new(Mutex::new(None)),
            },
        );

        let bridge_task = tokio::spawn(QuicStreamBridge::create_bidirectional_stream(
            Arc::clone(&server),
            agent_id,
            ConnectionId::new_v4(),
            bridge_socket,
            PortMapping {
                local_port: client_addr.port(),
                target_host: echo_addr.ip().to_string(),
                target_port: echo_addr.port(),
            },
        ));

        client_socket.write_all(b"labyrinth-quic").await.unwrap();
        let mut echoed = [0_u8; 14];
        timeout(
            Duration::from_secs(5),
            client_socket.read_exact(&mut echoed),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(&echoed, b"labyrinth-quic");
        drop(client_socket);
        bridge_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn quic_bridge_delivers_target_banner_sent_before_client_speaks() {
        // Regression: bytes buffered alongside the setup ack were dropped by
        // `into_inner()`, losing SSH/SMTP-style greetings.
        let generated = SecurityManager::generate_self_signed_certificate("localhost").unwrap();
        let (certs, key) = parse_generated_cert(&generated.cert_pem, &generated.key_pem);
        let server_endpoint = quinn::Endpoint::server(
            quic_server_config(certs, key),
            "127.0.0.1:0".parse().unwrap(),
        )
        .unwrap();
        let mut client_endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        client_endpoint.set_default_client_config(quic_client_config(&generated.cert_pem));
        let connecting = client_endpoint
            .connect(server_endpoint.local_addr().unwrap(), "localhost")
            .unwrap();
        let incoming = server_endpoint.accept().await.unwrap();
        let (client_connection, server_connection) = tokio::join!(connecting, incoming);
        let (client_connection, server_connection) =
            (client_connection.unwrap(), server_connection.unwrap());

        let banner = b"SSH-2.0-OpenSSH_9.6\r\n";
        // Fake agent: ack and the target's greeting leave in one write, so
        // they land in the bridge's read buffer together.
        tokio::spawn(async move {
            let (mut send, recv) = client_connection.accept_bi().await.unwrap();
            let mut recv = tokio::io::BufReader::new(recv);
            let (connection_id, _) = portal::read_quic_setup(&mut recv, Duration::from_secs(5))
                .await
                .unwrap();
            let mut flight = crate::framing::FrameCodec::SETUP
                .encode(&crate::protocol::Message::Stream(
                    crate::streaming::models::StreamMessage::SetupAck {
                        connection_id,
                        success: true,
                        error_message: None,
                    },
                ))
                .unwrap();
            flight.extend_from_slice(banner);
            send.write_all(&flight).await.unwrap();
            let mut sink = Vec::new();
            let _ = recv.read_to_end(&mut sink).await;
        });
        let target_addr: std::net::SocketAddr = "127.0.0.1:9".parse().unwrap();

        let local = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_addr = local.local_addr().unwrap();
        let client_task = tokio::spawn(TcpStream::connect(local_addr));
        let (bridge_socket, _) = local.accept().await.unwrap();
        let mut client_socket = client_task.await.unwrap().unwrap();

        let server = Arc::new(LabyrinthServer::new(false, None));
        let bridge = tokio::spawn(QuicStreamBridge::run_stream(
            server,
            server_connection,
            ConnectionId::new_v4(),
            bridge_socket,
            PortMapping {
                local_port: local_addr.port(),
                target_host: target_addr.ip().to_string(),
                target_port: target_addr.port(),
            },
        ));

        let mut received = vec![0u8; banner.len()];
        timeout(
            Duration::from_secs(5),
            client_socket.read_exact(&mut received),
        )
        .await
        .expect("banner never arrived")
        .unwrap();
        assert_eq!(&received, banner);
        drop(client_socket);
        let _ = timeout(Duration::from_secs(5), bridge).await;
    }

    #[tokio::test]
    async fn bridge_requires_quic_connected_agent() {
        let server = Arc::new(LabyrinthServer::new(false, None));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_client, accepted) = tokio::join!(TcpStream::connect(addr), listener.accept());
        let (socket, _) = accepted.unwrap();
        let error = QuicStreamBridge::create_bidirectional_stream(
            server,
            "missing".into(),
            ConnectionId::new_v4(),
            socket,
            PortMapping {
                local_port: 1,
                target_host: "127.0.0.1".into(),
                target_port: 1,
            },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("not connected over QUIC"));
    }
}
