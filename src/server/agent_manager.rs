use crate::error::{LabyrinthError, Result};
use crate::protocol::{AgentInfo, AgentKind, Message};
use crate::server::agent_connection::{handle_reader, handle_writer};
use crate::server::core::{ConnectedAgent, LabyrinthServer};
use crate::styling;
use colored::Colorize;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tracing::{error, info};
use uuid::Uuid;

/// Single Responsibility: Manages the registration and lifecycle of agents.
pub struct AgentManager;

impl AgentManager {
    pub async fn register_agent(
        server: Arc<LabyrinthServer>,
        mut stream: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
        client_addr: SocketAddr,
    ) -> Result<()> {
        info!("New agent connection from {}", client_addr);

        // Read the initial registration message from the agent.
        let mut buf = Vec::new();
        let mut reader = tokio::io::BufReader::new(&mut stream);
        tokio::io::AsyncBufReadExt::read_until(&mut reader, b'\n', &mut buf).await?;

        let message: Message = serde_json::from_slice(&buf[..buf.len() - 1])?;

        if let Message::AgentRegister(agent_info) = message {
            // Authenticate the agent if required.
            Self::authenticate_agent(&server, &agent_info, client_addr)?;
            Self::register_live_agent(server, stream, agent_info, client_addr.to_string()).await
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
        if server.auth_required() {
            if let Some(ref expected_key) = server.auth_key() {
                if let Some(ref provided_key) = agent_info.auth_key {
                    if expected_key != provided_key {
                        error!("Authentication failed for agent from {}", client_addr);
                        return Err(LabyrinthError::Message("Authentication failed".to_string()));
                    }
                } else {
                    error!("No auth key provided by agent from {}", client_addr);
                    return Err(LabyrinthError::Message("No auth key provided".to_string()));
                }
            }
        }
        Ok(())
    }

    pub async fn register_live_agent<S>(
        server: Arc<LabyrinthServer>,
        mut stream: S,
        agent_info: AgentInfo,
        remote_addr: String,
    ) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let ack_msg = serde_json::to_string(&Message::AgentAck)?;
        stream.write_all(ack_msg.as_bytes()).await?;
        stream.write_all(b"\n").await?;

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
            tunnel_active: false,
            tunnel_subnet: None,
            tun_name: None,
            last_seen: Arc::new(tokio::sync::Mutex::new(std::time::Instant::now())),
            command_response: Arc::new(tokio::sync::Mutex::new(None)),
            shell_events: Arc::new(tokio::sync::Mutex::new(None)),
        };

        tokio::spawn(handle_writer(writer, rx));
        tokio::spawn(handle_reader(
            tokio::io::BufReader::new(reader),
            server.clone(),
            agent_id.clone(),
        ));

        server
            .agents()
            .write()
            .await
            .insert(agent_id.clone(), agent);

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
        assert!(matches!(result, Err(LabyrinthError::Message(_))));
    }

    #[test]
    fn authenticate_rejects_wrong_key() {
        let server = LabyrinthServer::new(true, Some("secret".into()));
        let agent = make_agent(Some("bad"));

        let result =
            AgentManager::authenticate_agent(&server, &agent, "0.0.0.0:0".parse().unwrap());
        assert!(matches!(result, Err(LabyrinthError::Message(_))));
    }

    #[test]
    fn authenticate_accepts_matching_key() {
        let server = LabyrinthServer::new(true, Some("secret".into()));
        let agent = make_agent(Some("secret"));

        let result =
            AgentManager::authenticate_agent(&server, &agent, "0.0.0.0:0".parse().unwrap());
        assert!(result.is_ok());
    }
}
