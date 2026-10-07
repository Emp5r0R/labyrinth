use crate::error::{LabyrinthError, Result};
use crate::framing::{FrameCodec, HANDSHAKE_TIMEOUT};
use crate::protocol::{AgentInfo, AgentKind, Message};
use crate::security::keys_match;
use crate::server::agent_connection::{handle_reader, handle_writer};
use crate::server::core::{ConnectedAgent, LabyrinthServer};
use crate::styling;
use colored::Colorize;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Single Responsibility: Manages the registration and lifecycle of agents.
pub struct AgentManager;

impl AgentManager {
    pub async fn register_agent<S>(
        server: Arc<LabyrinthServer>,
        stream: S,
        client_addr: SocketAddr,
    ) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        Self::register_agent_stream(server, stream, client_addr, "tcp/tls", None).await
    }

    pub async fn register_quic_agent<S>(
        server: Arc<LabyrinthServer>,
        stream: S,
        client_addr: SocketAddr,
        connection: quinn::Connection,
    ) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        Self::register_agent_stream(server, stream, client_addr, "quic/udp", Some(connection)).await
    }

    async fn register_agent_stream<S>(
        server: Arc<LabyrinthServer>,
        stream: S,
        client_addr: SocketAddr,
        transport_label: &str,
        quic_connection: Option<quinn::Connection>,
    ) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        info!("New agent connection from {}", client_addr);

        // Unauthenticated peer: small frame limit and a deadline. The reader is
        // kept (not unwrapped) so bytes sent after registration are not lost.
        let mut stream = tokio::io::BufReader::new(stream);
        let message: Message = FrameCodec::HANDSHAKE
            .read_required(&mut stream, HANDSHAKE_TIMEOUT)
            .await?;

        if let Message::AgentRegister(agent_info) = message {
            // Authenticate the agent if required.
            Self::authenticate_agent(&server, &agent_info, client_addr)?;
            Self::register_live_agent(
                server,
                stream,
                agent_info,
                format!("{} via {}", client_addr, transport_label),
                transport_label.to_string(),
                quic_connection,
            )
            .await
        } else {
            error!("Expected AgentRegister message, got {:?}", message);
            Err(LabyrinthError::Message(
                "Invalid registration message".to_string(),
            ))
        }
    }

    fn authenticate_agent(
        server: &LabyrinthServer,
        agent_info: &AgentInfo,
        client_addr: SocketAddr,
    ) -> Result<()> {
        verify_agent_key(
            server.auth_required(),
            server.auth_key().as_deref(),
            agent_info.auth_key.as_deref(),
        )
        .inspect_err(|e| error!("Rejected agent from {}: {}", client_addr, e))
    }

    pub async fn register_live_agent<S>(
        server: Arc<LabyrinthServer>,
        mut stream: S,
        agent_info: AgentInfo,
        remote_addr: String,
        transport_label: String,
        quic_connection: Option<quinn::Connection>,
    ) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        FrameCodec::HANDSHAKE
            .write(&mut stream, &Message::AgentAck)
            .await?;

        let (reader, writer) = tokio::io::split(stream);
        let (tx, rx) = mpsc::channel(100);

        let agent_id = match agent_info.kind {
            AgentKind::Dweller => agent_info
                .stable_id
                .clone()
                .unwrap_or_else(|| Uuid::new_v4().to_string()),
            AgentKind::Generic => Uuid::new_v4().to_string()[..8].to_string(),
        };

        let agent = ConnectedAgent {
            id: agent_id.clone(),
            info: agent_info.clone(),
            sender: tx,
            transport_label: transport_label.to_string(),
            quic_connection,
            tunnel_active: false,
            tunnel_subnet: None,
            tun_name: None,
            last_seen: Arc::new(tokio::sync::Mutex::new(std::time::Instant::now())),
            command_response: Arc::new(tokio::sync::Mutex::new(None)),
            shell_events: Arc::new(tokio::sync::Mutex::new(None)),
        };

        let session = agent.sender.clone();
        if let Some(previous) = server
            .agents()
            .write()
            .await
            .insert(agent_id.clone(), agent)
        {
            // Same stable ID reconnected (e.g. a dweller callback). The old
            // session's reader will see it no longer owns the entry.
            warn!(
                "Agent {} reconnected; replacing session from {}",
                agent_id, previous.transport_label
            );
        }

        tokio::spawn(handle_writer(writer, rx));
        tokio::spawn(handle_reader(
            tokio::io::BufReader::new(reader),
            server.clone(),
            agent_id.clone(),
            session,
        ));

        if matches!(agent_info.kind, AgentKind::Dweller) {
            let mut registry = server.dweller_registry().write().await;
            if let Some(record) = registry.dwellers.get_mut(&agent_id) {
                record.last_connected = Some(chrono_like_now());
                if record.path.is_empty() {
                    record.path.push(crate::protocol::DwellerPathHop {
                        agent_id: agent_id.clone(),
                        agent_name: agent_info.name.clone(),
                        address: agent_info
                            .listener_addr
                            .clone()
                            .unwrap_or_else(|| remote_addr.clone()),
                        cidr: None,
                    });
                }
                let _ = registry.save();
            }
        }

        let label = match agent_info.kind {
            AgentKind::Dweller => "Dweller",
            AgentKind::Generic => "Agent",
        };

        println!(
            "{} {} {} ({}) connected from {}",
            styling::format_success_msg(styling::SUCCESS_INDICATOR, "").trim_start(),
            label,
            styling::format_agent_name(&agent_info.name),
            styling::format_agent_id(&agent_id),
            remote_addr.blue()
        );

        Ok(())
    }
}

/// Fail-closed shared-key check. Auth required without a configured key
/// rejects everyone rather than silently admitting every agent.
pub fn verify_agent_key(
    required: bool,
    expected: Option<&str>,
    provided: Option<&str>,
) -> Result<()> {
    if !required {
        return Ok(());
    }
    let expected = expected.ok_or_else(|| {
        LabyrinthError::Auth("server requires authentication but has no key".to_string())
    })?;
    let provided =
        provided.ok_or_else(|| LabyrinthError::Auth("No auth key provided".to_string()))?;
    if keys_match(provided, expected) {
        Ok(())
    } else {
        Err(LabyrinthError::Auth("Authentication failed".to_string()))
    }
}

fn chrono_like_now() -> String {
    format!("{:?}", std::time::SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::NetworkInterface;
    use crate::server::core::LabyrinthServer;

    fn make_agent(auth_key: Option<&str>) -> AgentInfo {
        AgentInfo {
            name: "test".into(),
            hostname: "host".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            interfaces: vec![NetworkInterface {
                name: "eth0".into(),
                addresses: vec!["127.0.0.1/8".into()],
                hardware_addr: "00:00:00:00:00:00".into(),
                mtu: 1500,
                flags: vec![],
            }],
            auth_key: auth_key.map(str::to_string),
            kind: AgentKind::Generic,
            stable_id: None,
            listener_addr: None,
            listener_port: None,
            connectivity: Default::default(),
        }
    }

    #[test]
    fn authenticate_allows_when_not_required() {
        let server = LabyrinthServer::new(false, None);
        let agent = make_agent(None);

        let result =
            AgentManager::authenticate_agent(&server, &agent, "0.0.0.0:0".parse().unwrap());
        assert!(result.is_ok());
    }

    #[test]
    fn authenticate_rejects_missing_key() {
        let server = LabyrinthServer::new(true, Some("secret".into()));
        let agent = make_agent(None);

        let result =
            AgentManager::authenticate_agent(&server, &agent, "0.0.0.0:0".parse().unwrap());
        assert!(matches!(result, Err(LabyrinthError::Auth(_))));
    }

    #[test]
    fn authenticate_rejects_wrong_key() {
        let server = LabyrinthServer::new(true, Some("secret".into()));
        let agent = make_agent(Some("bad"));

        let result =
            AgentManager::authenticate_agent(&server, &agent, "0.0.0.0:0".parse().unwrap());
        assert!(matches!(result, Err(LabyrinthError::Auth(_))));
    }

    #[test]
    fn authenticate_accepts_matching_key() {
        let server = LabyrinthServer::new(true, Some("secret".into()));
        let agent = make_agent(Some("secret"));

        let result =
            AgentManager::authenticate_agent(&server, &agent, "0.0.0.0:0".parse().unwrap());
        assert!(result.is_ok());
    }

    #[test]
    fn verify_agent_key_policy_matrix() {
        assert!(verify_agent_key(false, None, None).is_ok());
        assert!(verify_agent_key(false, Some("k"), Some("wrong")).is_ok());
        assert!(verify_agent_key(true, Some("k"), Some("k")).is_ok());
        assert!(matches!(
            verify_agent_key(true, Some("k"), Some("K")),
            Err(LabyrinthError::Auth(_))
        ));
        assert!(matches!(
            verify_agent_key(true, Some("k"), None),
            Err(LabyrinthError::Auth(_))
        ));
        assert!(matches!(
            verify_agent_key(true, Some("k"), Some("")),
            Err(LabyrinthError::Auth(_))
        ));
    }

    #[test]
    fn auth_required_without_configured_key_fails_closed() {
        // Regression: this combination used to admit every agent.
        assert!(matches!(
            verify_agent_key(true, None, Some("anything")),
            Err(LabyrinthError::Auth(_))
        ));
        let server = LabyrinthServer::new(true, None);
        let agent = make_agent(Some("anything"));
        assert!(
            AgentManager::authenticate_agent(&server, &agent, "0.0.0.0:0".parse().unwrap())
                .is_err()
        );
    }
}
