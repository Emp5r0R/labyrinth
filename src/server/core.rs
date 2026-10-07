use crate::error::{LabyrinthError, Result};
use crate::protocol::{AgentInfo, DwellerTask, DwellerTaskKind, DwellerTaskResult, Message};
use crate::server::dweller_registry::{DwellerRecord, DwellerRegistry};
use crate::server::reverse_port_forward::PortalListener as PortalSocketListener;
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
    pub transport_label: String,
    pub quic_connection: Option<quinn::Connection>,
    pub tunnel_active: bool,
    pub tunnel_subnet: Option<String>,
    pub tun_name: Option<String>,
    pub last_seen: Arc<Mutex<Instant>>,
    pub command_response: Arc<Mutex<Option<oneshot::Sender<Message>>>>,
    pub shell_events: Arc<Mutex<Option<mpsc::UnboundedSender<Message>>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalSnapshot {
    pub local_port: u16,
    pub agent_id: String,
    pub target_host: String,
    pub target_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AriadneSnapshot {
    pub agent_id: String,
    pub proxy_port: u16,
}

struct PortalListener {
    agent_id: String,
    mapping: crate::streaming::models::PortMapping,
    listener: PortalSocketListener,
}

impl PortalListener {
    fn new(
        agent_id: String,
        mapping: crate::streaming::models::PortMapping,
        listener: PortalSocketListener,
    ) -> Self {
        Self {
            agent_id,
            mapping,
            listener,
        }
    }

    async fn stop(self) {
        self.listener.stop().await;
    }
}

struct AriadneListener {
    proxy_port: u16,
    handle: JoinHandle<()>,
}

impl AriadneListener {
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
    // Streaming managers used by Portal mode when enabled
    stream_manager: Arc<RwLock<Option<Arc<dyn StreamManagerTrait>>>>,
    connection_manager: Arc<RwLock<Option<Arc<dyn StreamConnectionManager>>>>,
    portal_listeners: Arc<RwLock<HashMap<u16, PortalListener>>>,
    connection_owners: Arc<RwLock<HashMap<ConnectionId, String>>>,
    ariadne_listeners: Arc<RwLock<HashMap<String, AriadneListener>>>,
    dweller_registry: Arc<RwLock<DwellerRegistry>>,
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
            portal_listeners: Arc::new(RwLock::new(HashMap::new())),
            connection_owners: Arc::new(RwLock::new(HashMap::new())),
            ariadne_listeners: Arc::new(RwLock::new(HashMap::new())),
            dweller_registry: Arc::new(RwLock::new(DwellerRegistry::default())),
        }
    }

    /// Replace the dweller registry, e.g. with `DwellerRegistry::in_memory()`
    /// so tests never write `dwellers.json` into the working directory.
    pub fn with_dweller_registry(self, registry: DwellerRegistry) -> Self {
        Self {
            dweller_registry: Arc::new(RwLock::new(registry)),
            ..self
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
            portal_listeners: Arc::clone(&self.portal_listeners),
            connection_owners: Arc::clone(&self.connection_owners),
            ariadne_listeners: Arc::clone(&self.ariadne_listeners),
            dweller_registry: Arc::clone(&self.dweller_registry),
        }
    }

    pub fn dweller_registry(&self) -> &Arc<RwLock<DwellerRegistry>> {
        &self.dweller_registry
    }

    pub async fn set_dweller_registry(&self, registry: DwellerRegistry) {
        *self.dweller_registry.write().await = registry;
    }

    pub async fn upsert_dweller_record(&self, record: DwellerRecord) -> Result<()> {
        let mut registry = self.dweller_registry.write().await;
        registry.upsert(record);
        registry.save()
    }

    pub async fn forget_dweller_record(&self, dweller_id: &str) -> Result<Option<DwellerRecord>> {
        let mut registry = self.dweller_registry.write().await;
        let removed = registry.remove(dweller_id);
        registry.save()?;
        Ok(removed)
    }

    pub async fn enqueue_dweller_task(
        &self,
        dweller_id: &str,
        kind: DwellerTaskKind,
    ) -> Result<Option<DwellerTask>> {
        let mut registry = self.dweller_registry.write().await;
        let task = registry.enqueue_task(dweller_id, kind, chrono_like_now());
        registry.save()?;
        Ok(task)
    }

    pub async fn claim_dweller_tasks(
        &self,
        dweller_id: &str,
        limit: usize,
    ) -> Result<Vec<DwellerTask>> {
        let mut registry = self.dweller_registry.write().await;
        let tasks = registry.claim_tasks(dweller_id, limit, chrono_like_now());
        registry.save()?;
        Ok(tasks)
    }

    pub async fn complete_dweller_task(
        &self,
        dweller_id: &str,
        result: DwellerTaskResult,
    ) -> Result<bool> {
        let mut registry = self.dweller_registry.write().await;
        let completed = registry.complete_task(dweller_id, result);
        registry.save()?;
        Ok(completed)
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

    pub async fn register_portal_listener(
        &self,
        local_port: u16,
        agent_id: String,
        mapping: crate::streaming::models::PortMapping,
        listener: PortalSocketListener,
    ) -> Result<()> {
        if local_port != mapping.local_port {
            listener.stop().await;
            return Err(LabyrinthError::Message(format!(
                "Portal listener port {} does not match mapping port {}",
                local_port, mapping.local_port
            )));
        }
        let mut listeners = self.portal_listeners.write().await;
        if listeners.contains_key(&local_port) {
            drop(listeners);
            listener.stop().await;
            return Err(crate::error::LabyrinthError::Message(format!(
                "Port {} already in use for port forwarding",
                local_port
            )));
        }
        listeners.insert(local_port, PortalListener::new(agent_id, mapping, listener));
        Ok(())
    }

    pub async fn unregister_portal_listener(&self, local_port: u16) {
        let mut listeners = self.portal_listeners.write().await;
        let listener = listeners.remove(&local_port);
        drop(listeners);
        if let Some(listener) = listener {
            let agent_id = listener.agent_id.clone();
            listener.stop().await;
            self.cleanup_portal_connections_for_agent(&agent_id).await;
        }
    }

    pub async fn has_portal_forwarding(&self, agent_id: &str) -> bool {
        let listeners = self.portal_listeners.read().await;
        listeners
            .values()
            .any(|listener| listener.agent_id == agent_id)
    }

    pub async fn stop_portal_forwarding_for_agent(&self, agent_id: &str) -> Vec<u16> {
        let ports: Vec<u16> = {
            let listeners = self.portal_listeners.read().await;
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

        let removed: Vec<PortalListener> = if !ports.is_empty() {
            let mut listeners = self.portal_listeners.write().await;
            ports
                .iter()
                .filter_map(|port| listeners.remove(port))
                .collect()
        } else {
            Vec::new()
        };
        for listener in removed {
            listener.stop().await;
        }
        self.cleanup_portal_connections_for_agent(agent_id).await;
        ports
    }

    /// Tear down every Portal data-plane resource owned by an agent. This is
    /// idempotent and safe to call from listener stop, agent disconnect, or a
    /// per-connection failure path.
    pub async fn cleanup_portal_connections_for_agent(&self, agent_id: &str) {
        let connection_ids = self.connection_ids_for_agent(agent_id).await;
        for connection_id in connection_ids {
            self.cleanup_portal_connection(connection_id).await;
        }
    }

    pub async fn cleanup_portal_connection(&self, connection_id: ConnectionId) {
        if let Some(stream_manager) = self.get_stream_manager().await {
            let _ = stream_manager.terminate_stream(connection_id).await;
        }
        if let Some(connection_manager) = self.get_connection_manager().await {
            let _ = connection_manager
                .update_connection_status(
                    &connection_id,
                    crate::streaming::ConnectionStatus::Closing,
                )
                .await;
            let _ = connection_manager.cleanup_connection(&connection_id).await;
        }
        let _ = self.unregister_connection_owner(&connection_id).await;
    }

    pub async fn register_connection_owner(&self, connection_id: ConnectionId, agent_id: String) {
        let mut owners = self.connection_owners.write().await;
        owners.insert(connection_id, agent_id);
    }

    pub async fn register_ariadne_listener(
        &self,
        agent_id: String,
        proxy_port: u16,
        handle: JoinHandle<()>,
    ) {
        let mut listeners = self.ariadne_listeners.write().await;
        if let Some(existing) = listeners.remove(&agent_id) {
            existing.stop();
        }
        listeners.insert(agent_id, AriadneListener::new(proxy_port, handle));
    }

    pub async fn portal_snapshots(&self) -> Vec<PortalSnapshot> {
        let listeners = self.portal_listeners.read().await;
        let mut snapshots: Vec<_> = listeners
            .values()
            .map(|listener| PortalSnapshot {
                local_port: listener.mapping.local_port,
                agent_id: listener.agent_id.clone(),
                target_host: listener.mapping.target_host.clone(),
                target_port: listener.mapping.target_port,
            })
            .collect();
        snapshots.sort_by(|left, right| {
            left.agent_id
                .cmp(&right.agent_id)
                .then_with(|| left.local_port.cmp(&right.local_port))
        });
        snapshots
    }

    pub async fn ariadne_snapshots(&self) -> Vec<AriadneSnapshot> {
        let listeners = self.ariadne_listeners.read().await;
        let mut snapshots: Vec<_> = listeners
            .iter()
            .map(|(agent_id, listener)| AriadneSnapshot {
                agent_id: agent_id.clone(),
                proxy_port: listener.proxy_port,
            })
            .collect();
        snapshots.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
        snapshots
    }

    pub async fn stop_ariadne_listener(&self, agent_id: &str) -> Option<u16> {
        let mut listeners = self.ariadne_listeners.write().await;
        listeners.remove(agent_id).map(AriadneListener::stop)
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

fn chrono_like_now() -> String {
    format!("{:?}", std::time::SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::reverse_port_forward::PortalConnectionHandler;
    use async_trait::async_trait;
    use std::io::ErrorKind;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::net::{TcpListener, TcpStream};

    struct NoopPortalHandler;

    #[async_trait]
    impl PortalConnectionHandler for NoopPortalHandler {
        async fn handle(
            &self,
            _stream: TcpStream,
            _peer: SocketAddr,
            _mapping: crate::streaming::models::PortMapping,
        ) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn register_and_stop_port_forwarding() {
        let server = LabyrinthServer::new(false, None);
        let probe = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind Portal test listener: {error}"),
        };
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let listener = match PortalSocketListener::bind(
            crate::streaming::models::PortMapping {
                local_port: port,
                target_host: "127.0.0.1".to_string(),
                target_port: 80,
            },
            1,
            Arc::new(NoopPortalHandler),
        )
        .await
        {
            Ok(listener) => listener,
            Err(LabyrinthError::Io(error)) if error.kind() == ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to create Portal listener: {error}"),
        };
        server
            .register_portal_listener(
                port,
                "agent".to_string(),
                crate::streaming::models::PortMapping {
                    local_port: port,
                    target_host: "127.0.0.1".to_string(),
                    target_port: 80,
                },
                listener,
            )
            .await
            .unwrap();
        assert!(server.has_portal_forwarding("agent").await);

        let stopped = server.stop_portal_forwarding_for_agent("agent").await;
        assert_eq!(stopped, vec![port]);
        assert!(!server.has_portal_forwarding("agent").await);
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
