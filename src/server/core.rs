use crate::error::Result;
use crate::protocol::{AgentInfo, Message};
use crate::streaming::{
    traits::{ConnectionManager as StreamConnectionManager, StreamManager as StreamManagerTrait},
    ConnectionId,
};
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use tokio::task::JoinHandle;

/// Single Responsibility: Represents a connected agent
#[derive(Clone)]
pub struct ConnectedAgent {
    pub id: String,
    pub info: AgentInfo,
    pub sender: mpsc::Sender<Message>,
    pub tunnel_active: bool,
    pub tunnel_subnet: Option<String>,
    pub tun_name: Option<String>,
    pub last_seen: Arc<Mutex<Instant>>,
    pub command_response: Arc<Mutex<Option<oneshot::Sender<Message>>>>,
    pub shell_events: Arc<Mutex<Option<mpsc::UnboundedSender<Message>>>>,
}

struct PortForwardListener {
    agent_id: String,
    handle: JoinHandle<()>,
}

impl PortForwardListener {
    fn new(agent_id: String, handle: JoinHandle<()>) -> Self {
        Self { agent_id, handle }
    }

    fn stop(self) {
        self.handle.abort();
    }
}

struct FullhouseListener {
    proxy_port: u16,
    handle: JoinHandle<()>,
}

impl FullhouseListener {
    fn new(proxy_port: u16, handle: JoinHandle<()>) -> Self {
        Self { proxy_port, handle }
    }

    fn stop(self) -> u16 {
        self.handle.abort();
        self.proxy_port
    }
}

/// Single Responsibility: Core server state management
pub struct LabyrinthServer {
    agents: Arc<RwLock<HashMap<String, ConnectedAgent>>>,
    current_agent: Arc<RwLock<Option<String>>>,
    auth_required: bool,
    auth_key: Option<String>,
    // Streaming managers used by Room mode when enabled
    stream_manager: Arc<RwLock<Option<Arc<dyn StreamManagerTrait>>>>,
    connection_manager: Arc<RwLock<Option<Arc<dyn StreamConnectionManager>>>>,
    port_forward_listeners: Arc<RwLock<HashMap<u16, PortForwardListener>>>,
    connection_owners: Arc<RwLock<HashMap<ConnectionId, String>>>,
    fullhouse_listeners: Arc<RwLock<HashMap<String, FullhouseListener>>>,
}

impl LabyrinthServer {
    pub fn new(auth_required: bool, auth_key: Option<String>) -> Self {
        Self {
            agents: Arc::new(RwLock::new(HashMap::new())),
            current_agent: Arc::new(RwLock::new(None)),
            auth_required,
            auth_key,
            stream_manager: Arc::new(RwLock::new(None)),
            connection_manager: Arc::new(RwLock::new(None)),
            port_forward_listeners: Arc::new(RwLock::new(HashMap::new())),
            connection_owners: Arc::new(RwLock::new(HashMap::new())),
            fullhouse_listeners: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn agents(&self) -> &Arc<RwLock<HashMap<String, ConnectedAgent>>> {
        &self.agents
    }

    pub fn current_agent(&self) -> &Arc<RwLock<Option<String>>> {
        &self.current_agent
    }

    pub fn auth_required(&self) -> bool {
        self.auth_required
    }

    pub fn auth_key(&self) -> &Option<String> {
        &self.auth_key
    }

    pub fn clone_for_tasks(&self) -> Self {
        Self {
            agents: Arc::clone(&self.agents),
            current_agent: Arc::clone(&self.current_agent),
            auth_required: self.auth_required,
            auth_key: self.auth_key.clone(),
            stream_manager: Arc::clone(&self.stream_manager),
            connection_manager: Arc::clone(&self.connection_manager),
            port_forward_listeners: Arc::clone(&self.port_forward_listeners),
            connection_owners: Arc::clone(&self.connection_owners),
            fullhouse_listeners: Arc::clone(&self.fullhouse_listeners),
        }
    }

    // Streaming manager accessors
    pub async fn set_streaming_managers(
        &self,
        stream_manager: Arc<dyn StreamManagerTrait>,
        connection_manager: Arc<dyn StreamConnectionManager>,
    ) {
        {
            let mut sm = self.stream_manager.write().await;
            *sm = Some(stream_manager);
        }
        {
            let mut cm = self.connection_manager.write().await;
            *cm = Some(connection_manager);
        }
    }

    pub async fn get_stream_manager(&self) -> Option<Arc<dyn StreamManagerTrait>> {
        self.stream_manager.read().await.deref().clone()
    }

    pub async fn get_connection_manager(&self) -> Option<Arc<dyn StreamConnectionManager>> {
        self.connection_manager.read().await.deref().clone()
    }

    pub async fn register_port_forward_listener(
        &self,
        local_port: u16,
        agent_id: String,
        handle: JoinHandle<()>,
    ) -> Result<()> {
        let mut listeners = self.port_forward_listeners.write().await;
        if listeners.contains_key(&local_port) {
            return Err(crate::error::LabyrinthError::Message(format!(
                "Port {} already in use for port forwarding",
                local_port
            )));
        }
        listeners.insert(local_port, PortForwardListener::new(agent_id, handle));
        Ok(())
    }

    pub async fn unregister_port_forward_listener(&self, local_port: u16) {
        let mut listeners = self.port_forward_listeners.write().await;
        listeners.remove(&local_port);
    }

    pub async fn has_port_forwarding(&self, agent_id: &str) -> bool {
        let listeners = self.port_forward_listeners.read().await;
        listeners
            .values()
            .any(|listener| listener.agent_id == agent_id)
    }

    pub async fn stop_port_forwarding_for_agent(&self, agent_id: &str) -> Vec<u16> {
        let ports: Vec<u16> = {
            let listeners = self.port_forward_listeners.read().await;
            listeners
                .iter()
                .filter_map(|(port, listener)| {
                    if listener.agent_id == agent_id {
                        Some(*port)
                    } else {
                        None
                    }
                })
                .collect()
        };

        if !ports.is_empty() {
            let mut listeners = self.port_forward_listeners.write().await;
            for port in &ports {
                if let Some(listener) = listeners.remove(port) {
                    listener.stop();
                }
            }
        }
        ports
    }

    pub async fn register_connection_owner(&self, connection_id: ConnectionId, agent_id: String) {
        let mut owners = self.connection_owners.write().await;
        owners.insert(connection_id, agent_id);
    }

    pub async fn register_fullhouse_listener(
        &self,
        agent_id: String,
        proxy_port: u16,
        handle: JoinHandle<()>,
    ) {
        let mut listeners = self.fullhouse_listeners.write().await;
        if let Some(existing) = listeners.remove(&agent_id) {
            existing.stop();
        }
        listeners.insert(agent_id, FullhouseListener::new(proxy_port, handle));
    }

    pub async fn stop_fullhouse_listener(&self, agent_id: &str) -> Option<u16> {
        let mut listeners = self.fullhouse_listeners.write().await;
        listeners.remove(agent_id).map(FullhouseListener::stop)
    }

    pub async fn unregister_connection_owner(
        &self,
        connection_id: &ConnectionId,
    ) -> Option<String> {
        let mut owners = self.connection_owners.write().await;
        owners.remove(connection_id)
    }

    pub async fn owner_for_connection(&self, connection_id: &ConnectionId) -> Option<String> {
        let owners = self.connection_owners.read().await;
        owners.get(connection_id).cloned()
    }

    pub async fn connection_ids_for_agent(&self, agent_id: &str) -> Vec<ConnectionId> {
        let owners = self.connection_owners.read().await;
        owners
            .iter()
            .filter_map(|(connection_id, owner)| {
                if owner == agent_id {
                    Some(*connection_id)
                } else {
                    None
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::Duration;

    fn dummy_handle() -> JoinHandle<()> {
        tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(60)).await;
        })
    }

    #[tokio::test]
    async fn register_and_stop_port_forwarding() {
        let server = LabyrinthServer::new(false, None);
        server
            .register_port_forward_listener(8080, "agent".to_string(), dummy_handle())
            .await
            .unwrap();
        assert!(server.has_port_forwarding("agent").await);

        let stopped = server.stop_port_forwarding_for_agent("agent").await;
        assert_eq!(stopped, vec![8080]);
        assert!(!server.has_port_forwarding("agent").await);
    }

    #[tokio::test]
    async fn connection_owner_tracking() {
        let server = LabyrinthServer::new(false, None);
        let connection_id = ConnectionId::new_v4();
        server
            .register_connection_owner(connection_id, "agent-1".to_string())
            .await;

        assert_eq!(
            server.owner_for_connection(&connection_id).await,
            Some("agent-1".to_string())
        );

        let ids = server.connection_ids_for_agent("agent-1").await;
        assert_eq!(ids, vec![connection_id]);

        assert_eq!(
            server.unregister_connection_owner(&connection_id).await,
            Some("agent-1".to_string())
        );
        assert!(server.owner_for_connection(&connection_id).await.is_none());
    }
}
