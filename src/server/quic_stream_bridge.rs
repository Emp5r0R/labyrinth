use crate::error::{LabyrinthError, Result};
use crate::protocol::Message;
use crate::server::core::LabyrinthServer;
use crate::streaming::models::{ConnectionId, ConnectionStatus, PortMapping, StreamMessage};
use crate::transport::QuicBidiStream;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpStream;
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

        tokio::spawn(async move {
            if let Err(e) =
                Self::run_stream(server, connection, connection_id, client_socket, mapping).await
            {
                error!("QUIC stream {} failed: {}", connection_id, e);
            }
        });

        Ok(())
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

        let setup = Message::Stream(StreamMessage::Setup {
            connection_id,
            mapping,
        });
        let setup_line = serde_json::to_string(&setup)?;
        send.write_all(setup_line.as_bytes())
            .await
            .map_err(|e| LabyrinthError::Message(format!("QUIC write failed: {}", e)))?;
        send.write_all(b"\n")
            .await
            .map_err(|e| LabyrinthError::Message(format!("QUIC write failed: {}", e)))?;

        let mut reader = BufReader::new(recv);
        let mut ack_buf = Vec::new();
        reader.read_until(b'\n', &mut ack_buf).await?;
        let ack: Message = serde_json::from_slice(&ack_buf[..ack_buf.len().saturating_sub(1)])?;
        match ack {
            Message::Stream(StreamMessage::SetupAck {
                success: true,
                connection_id: ack_id,
                ..
            }) if ack_id == connection_id => {
                if let Some(cm) = server.get_connection_manager().await {
                    let _ = cm
                        .update_connection_status(&connection_id, ConnectionStatus::Active)
                        .await;
                }
            }
            Message::Stream(StreamMessage::SetupAck {
                success: false,
                error_message,
                ..
            }) => {
                let reason = error_message.unwrap_or_else(|| "target setup failed".to_string());
                if let Some(cm) = server.get_connection_manager().await {
                    let _ = cm
                        .update_connection_status(
                            &connection_id,
                            ConnectionStatus::Error(reason.clone()),
                        )
                        .await;
                    let _ = cm.cleanup_connection(&connection_id).await;
                }
                let _ = server.unregister_connection_owner(&connection_id).await;
                return Err(LabyrinthError::Message(reason));
            }
            other => {
                let _ = server.unregister_connection_owner(&connection_id).await;
                return Err(LabyrinthError::Message(format!(
                    "Unexpected QUIC stream ack: {:?}",
                    other
                )));
            }
        }

        let recv = reader.into_inner();
        let mut quic_stream = QuicBidiStream::new(send, recv);
        info!("QUIC native stream active for {}", connection_id);

        match tokio::io::copy_bidirectional(&mut client_socket, &mut quic_stream).await {
            Ok((client_to_agent, agent_to_client)) => {
                debug!(
                    "QUIC stream {} closed after {} bytes client->agent and {} bytes agent->client",
                    connection_id, client_to_agent, agent_to_client
                );
            }
            Err(e) => {
                error!("QUIC stream {} copy failed: {}", connection_id, e);
            }
        }

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

        QuicStreamBridge::create_bidirectional_stream(
            Arc::clone(&server),
            agent_id,
            ConnectionId::new_v4(),
            bridge_socket,
            PortMapping {
                local_port: client_addr.port(),
                target_host: echo_addr.ip().to_string(),
                target_port: echo_addr.port(),
            },
        )
        .await
        .unwrap();

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
    }
}
