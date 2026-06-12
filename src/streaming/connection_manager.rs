//! Server-side connection management implementation

use crate::streaming::errors::{StreamError, StreamResult};
use crate::streaming::traits::ConnectionManager;
use crate::streaming::{
    ConnectionId, ConnectionState, ConnectionStats, ConnectionStatus, ErrorRecoveryCoordinator,
    MetricsCollector, PortMapping,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

/// Server-side connection manager that tracks and manages connection lifecycle
#[derive(Debug)]
pub struct ServerConnectionManager {
    /// Map of active connections indexed by connection ID
    connections: Arc<RwLock<HashMap<ConnectionId, ConnectionState>>>,
    /// Statistics for all connections
    stats: Arc<RwLock<ConnectionStats>>,
    /// Metrics collector for detailed monitoring
    metrics_collector: Option<Arc<MetricsCollector>>,
    /// Error recovery coordinator for handling failures
    recovery_coordinator: Option<Arc<ErrorRecoveryCoordinator>>,
}

impl ServerConnectionManager {
    /// Create a new server connection manager
    pub fn new() -> Self {
        Self {
            connections: Arc::new(RwLock::new(HashMap::new())),
            stats: Arc::new(RwLock::new(ConnectionStats::new())),
            metrics_collector: None,
            recovery_coordinator: None,
        }
    }

    // Removed unused methods: with_monitoring, active_connection_count, get_all_connection_ids

    /// Internal method to validate connection state transitions
    fn is_valid_status_transition(from: &ConnectionStatus, to: &ConnectionStatus) -> bool {
        use ConnectionStatus::*;
        match (from, to) {
            (Establishing, Active) => true,
            (Establishing, Closing) => true,
            (Establishing, Closed) => true,
            (Establishing, Error(_)) => true,
            (Active, Closing) => true,
            (Active, Closed) => true,
            (Active, Error(_)) => true,
            (Closing, Closed) => true,
            (Closing, Error(_)) => true,
            (Closed, _) => false,       // Closed is terminal
            (Error(_), Closed) => true, // Allow cleanup of errored connections
            _ => false,
        }
    }

    /// Internal method to update statistics when connection status changes
    async fn update_stats_for_status_change(
        &self,
        old_status: &ConnectionStatus,
        new_status: &ConnectionStatus,
    ) -> StreamResult<()> {
        let mut stats = self.stats.write().await;

        // Update active connection count
        match (old_status, new_status) {
            (ConnectionStatus::Establishing, ConnectionStatus::Active) => {
                stats.increment_active();
            }
            (ConnectionStatus::Active, ConnectionStatus::Closing)
            | (ConnectionStatus::Active, ConnectionStatus::Closed)
            | (ConnectionStatus::Active, ConnectionStatus::Error(_)) => {
                stats.decrement_active();
            }
            (ConnectionStatus::Establishing, ConnectionStatus::Error(_))
            | (ConnectionStatus::Establishing, ConnectionStatus::Closed) => {
                stats.increment_failed();
            }
            _ => {}
        }

        // Update status counts
        stats.update_status_count(new_status);

        Ok(())
    }
}

impl Default for ServerConnectionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ConnectionManager for ServerConnectionManager {
    #[instrument(skip(self, connection), fields(connection_id, client_addr, target))]
    async fn handle_new_connection(
        &self,
        connection: TcpStream,
        mapping: PortMapping,
    ) -> StreamResult<ConnectionId> {
        let connection_id = Uuid::new_v4();
        let client_addr = connection.peer_addr().map_err(|e| {
            let error =
                StreamError::connection_failed(format!("Failed to get peer address: {}", e));
            if let Some(metrics) = &self.metrics_collector {
                let error_msg = error.to_string();
                let _error_category = error.category();
                tokio::spawn({
                    let metrics = Arc::clone(metrics);
                    async move {
                        let error = StreamError::connection_failed(error_msg);
                        metrics.record_error(&error, Some(connection_id)).await;
                    }
                });
            }
            error
        })?;

        // Update tracing span with actual values
        tracing::Span::current().record("connection_id", tracing::field::display(&connection_id));
        tracing::Span::current().record("client_addr", tracing::field::display(&client_addr));
        tracing::Span::current().record(
            "target",
            tracing::field::display(&format!("{}:{}", mapping.target_host, mapping.target_port)),
        );

        debug!(
            connection_id = %connection_id,
            client_addr = %client_addr,
            target = %format!("{}:{}", mapping.target_host, mapping.target_port),
            "Creating new connection"
        );

        let connection_state = ConnectionState::new(connection_id, client_addr, mapping.clone());

        // Store the connection
        {
            let mut connections = self.connections.write().await;
            connections.insert(connection_id, connection_state);
        }

        // Update statistics
        {
            let mut stats = self.stats.write().await;
            stats.increment_total();
            stats.update_status_count(&ConnectionStatus::Establishing);
        }

        // Record metrics if available
        if let Some(metrics) = &self.metrics_collector {
            metrics.record_connection_created(connection_id).await;
        }

        info!(
            connection_id = %connection_id,
            client_addr = %client_addr,
            target = %format!("{}:{}", mapping.target_host, mapping.target_port),
            "New connection created and tracked"
        );

        Ok(connection_id)
    }

    #[instrument(skip(self), fields(connection_id = %connection_id))]
    async fn cleanup_connection(&self, connection_id: &ConnectionId) -> StreamResult<()> {
        debug!(connection_id = %connection_id, "Cleaning up connection");

        let connection_state = {
            let mut connections = self.connections.write().await;
            connections.remove(connection_id)
        };

        match connection_state {
            Some(state) => {
                // Update statistics based on final state
                if matches!(state.status, ConnectionStatus::Active) {
                    let mut stats = self.stats.write().await;
                    stats.decrement_active();
                }

                // Calculate connection duration for statistics
                let duration = state.created_at.elapsed();

                // Record metrics if available
                if let Some(metrics) = &self.metrics_collector {
                    metrics.record_connection_cleanup(*connection_id).await;
                }

                info!(
                    connection_id = %connection_id,
                    duration_ms = duration.as_millis(),
                    final_status = ?state.status,
                    client_addr = %state.client_addr,
                    target = %format!("{}:{}", state.target_mapping.target_host, state.target_mapping.target_port),
                    "Connection cleaned up successfully"
                );

                Ok(())
            }
            None => {
                let error = StreamError::connection_not_found(*connection_id);

                // Record error in metrics if available
                if let Some(metrics) = &self.metrics_collector {
                    metrics.record_error(&error, Some(*connection_id)).await;
                }

                warn!(
                    connection_id = %connection_id,
                    error = %error,
                    "Attempted to cleanup non-existent connection"
                );

                Err(error)
            }
        }
    }

    async fn get_connection_stats(&self) -> StreamResult<ConnectionStats> {
        let stats = self.stats.read().await;
        Ok(stats.clone())
    }

    async fn get_connection_state(
        &self,
        connection_id: &ConnectionId,
    ) -> StreamResult<Option<ConnectionState>> {
        let connections = self.connections.read().await;
        Ok(connections.get(connection_id).cloned())
    }

    async fn track_existing_connection(
        &self,
        connection_id: ConnectionId,
        client_addr: SocketAddr,
        mapping: PortMapping,
    ) -> StreamResult<()> {
        let mut connections = self.connections.write().await;
        if connections.contains_key(&connection_id) {
            return Err(StreamError::invalid_connection_state(
                "absent".to_string(),
                "duplicate".to_string(),
            ));
        }

        let state = ConnectionState::new(connection_id, client_addr, mapping);
        connections.insert(connection_id, state);

        let mut stats = self.stats.write().await;
        stats.increment_total();
        stats.update_status_count(&ConnectionStatus::Establishing);
        Ok(())
    }

    #[instrument(skip(self), fields(connection_id = %connection_id, new_status = ?status))]
    async fn update_connection_status(
        &self,
        connection_id: &ConnectionId,
        status: ConnectionStatus,
    ) -> StreamResult<()> {
        debug!(
            connection_id = %connection_id,
            new_status = ?status,
            "Updating connection status"
        );

        let mut connections = self.connections.write().await;

        match connections.get_mut(connection_id) {
            Some(connection_state) => {
                let old_status = connection_state.status.clone();

                if old_status == status {
                    debug!(
                        connection_id = %connection_id,
                        status = ?status,
                        "Connection status already current"
                    );
                    return Ok(());
                }

                // Validate status transition
                if !Self::is_valid_status_transition(&old_status, &status) {
                    let error = StreamError::invalid_connection_state(
                        format!("{:?}", old_status),
                        format!("{:?}", status),
                    );

                    // Record error in metrics if available
                    if let Some(metrics) = &self.metrics_collector {
                        metrics.record_error(&error, Some(*connection_id)).await;
                    }

                    error!(
                        connection_id = %connection_id,
                        old_status = ?old_status,
                        new_status = ?status,
                        error = %error,
                        "Invalid status transition"
                    );

                    return Err(error);
                }

                // Update the status
                connection_state.status = status.clone();

                // Drop the connections lock before updating stats to avoid deadlock
                drop(connections);

                // Update statistics
                self.update_stats_for_status_change(&old_status, &status)
                    .await?;

                // Record metrics if available
                if let Some(metrics) = &self.metrics_collector {
                    metrics
                        .record_connection_status_change(
                            *connection_id,
                            old_status.clone(),
                            status.clone(),
                        )
                        .await;
                }

                // Attempt error recovery if transitioning to error state
                if let ConnectionStatus::Error(error_msg) = &status {
                    if let Some(recovery) = &self.recovery_coordinator {
                        let error = StreamError::connection_failed(error_msg.clone());
                        let error_context =
                            error.with_connection_context(*connection_id, "status_update");
                        let conn_id = *connection_id; // Copy the connection ID to avoid lifetime issues

                        // Spawn recovery attempt in background
                        let recovery_coordinator = Arc::clone(recovery);
                        tokio::spawn(async move {
                            match recovery_coordinator
                                .attempt_recovery(error_context, Some(conn_id))
                                .await
                            {
                                Ok(true) => {
                                    info!(connection_id = %conn_id, "Error recovery successful");
                                }
                                Ok(false) => {
                                    warn!(connection_id = %conn_id, "Error recovery failed");
                                }
                                Err(e) => {
                                    error!(connection_id = %conn_id, error = %e, "Error recovery encountered error");
                                }
                            }
                        });
                    }
                }

                info!(
                    connection_id = %connection_id,
                    old_status = ?old_status,
                    new_status = ?status,
                    "Connection status updated successfully"
                );

                Ok(())
            }
            None => {
                let error = StreamError::connection_not_found(*connection_id);

                // Record error in metrics if available
                if let Some(metrics) = &self.metrics_collector {
                    metrics.record_error(&error, Some(*connection_id)).await;
                }

                warn!(
                    connection_id = %connection_id,
                    error = %error,
                    "Attempted to update status of non-existent connection"
                );

                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use tokio::net::{TcpListener, TcpStream};

    async fn create_test_connection() -> Option<(TcpStream, std::net::SocketAddr)> {
        let listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(e) => {
                if e.kind() == ErrorKind::PermissionDenied {
                    eprintln!("Skipping connection manager tests (socket permissions): {e}");
                    return None;
                }
                panic!("Unexpected socket error: {e}");
            }
        };

        let addr = listener.local_addr().unwrap();
        let client_task = tokio::spawn(async move {
            TcpStream::connect(addr)
                .await
                .expect("client connect failed")
        });

        let (_server_stream, client_addr) =
            listener.accept().await.expect("accept failed during test");
        let client_stream = client_task.await.expect("client join failed");

        Some((client_stream, client_addr))
    }

    fn create_test_mapping() -> PortMapping {
        PortMapping {
            local_port: 8080,
            target_host: "localhost".to_string(),
            target_port: 3000,
        }
    }

    #[tokio::test]
    async fn test_new_connection_manager() {
        let manager = ServerConnectionManager::new();
        let stats = manager.get_connection_stats().await.unwrap();

        assert_eq!(stats.total_connections, 0);
        assert_eq!(stats.active_connections, 0);
        assert_eq!(stats.failed_connections, 0);
    }

    #[tokio::test]
    async fn test_handle_new_connection() {
        let manager = ServerConnectionManager::new();
        let Some((connection, _)) = create_test_connection().await else {
            return;
        };
        let mapping = create_test_mapping();

        let connection_id = manager
            .handle_new_connection(connection, mapping.clone())
            .await
            .unwrap();

        // Verify connection was created
        let state = manager.get_connection_state(&connection_id).await.unwrap();
        assert!(state.is_some());

        let state = state.unwrap();
        assert_eq!(state.id, connection_id);
        assert_eq!(state.target_mapping, mapping);
        assert_eq!(state.status, ConnectionStatus::Establishing);

        // Verify statistics
        let stats = manager.get_connection_stats().await.unwrap();
        assert_eq!(stats.total_connections, 1);
        assert_eq!(stats.active_connections, 0); // Still establishing
    }

    #[tokio::test]
    async fn test_connection_status_transitions() {
        let manager = ServerConnectionManager::new();
        let Some((connection, _)) = create_test_connection().await else {
            return;
        };
        let mapping = create_test_mapping();

        let connection_id = manager
            .handle_new_connection(connection, mapping)
            .await
            .unwrap();

        // Test valid transition: Establishing -> Active
        manager
            .update_connection_status(&connection_id, ConnectionStatus::Active)
            .await
            .unwrap();

        let state = manager
            .get_connection_state(&connection_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.status, ConnectionStatus::Active);

        // Verify active count increased
        let stats = manager.get_connection_stats().await.unwrap();
        assert_eq!(stats.active_connections, 1);

        // Test valid transition: Active -> Closing
        manager
            .update_connection_status(&connection_id, ConnectionStatus::Closing)
            .await
            .unwrap();

        let state = manager
            .get_connection_state(&connection_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.status, ConnectionStatus::Closing);

        // Verify active count decreased
        let stats = manager.get_connection_stats().await.unwrap();
        assert_eq!(stats.active_connections, 0);
    }

    #[tokio::test]
    async fn test_duplicate_status_update_is_idempotent() {
        let manager = ServerConnectionManager::new();
        let Some((connection, _)) = create_test_connection().await else {
            return;
        };
        let mapping = create_test_mapping();

        let connection_id = manager
            .handle_new_connection(connection, mapping)
            .await
            .unwrap();

        manager
            .update_connection_status(&connection_id, ConnectionStatus::Active)
            .await
            .unwrap();
        manager
            .update_connection_status(&connection_id, ConnectionStatus::Active)
            .await
            .unwrap();

        let state = manager
            .get_connection_state(&connection_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.status, ConnectionStatus::Active);

        let stats = manager.get_connection_stats().await.unwrap();
        assert_eq!(stats.active_connections, 1);
    }

    #[tokio::test]
    async fn test_invalid_status_transition() {
        let manager = ServerConnectionManager::new();
        let Some((connection, _)) = create_test_connection().await else {
            return;
        };
        let mapping = create_test_mapping();

        let connection_id = manager
            .handle_new_connection(connection, mapping)
            .await
            .unwrap();

        // Transition to Closed first
        manager
            .update_connection_status(&connection_id, ConnectionStatus::Closed)
            .await
            .unwrap();

        // Try invalid transition: Closed -> Active (should fail)
        let result = manager
            .update_connection_status(&connection_id, ConnectionStatus::Active)
            .await;
        assert!(result.is_err());

        match result.unwrap_err() {
            StreamError::InvalidConnectionState { .. } => {} // Expected
            other => panic!("Expected InvalidConnectionState error, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_cleanup_connection() {
        let manager = ServerConnectionManager::new();
        let Some((connection, _)) = create_test_connection().await else {
            return;
        };
        let mapping = create_test_mapping();

        let connection_id = manager
            .handle_new_connection(connection, mapping)
            .await
            .unwrap();

        // Make connection active first
        manager
            .update_connection_status(&connection_id, ConnectionStatus::Active)
            .await
            .unwrap();

        // Cleanup the connection
        manager.cleanup_connection(&connection_id).await.unwrap();

        // Verify connection is removed
        let state = manager.get_connection_state(&connection_id).await.unwrap();
        assert!(state.is_none());

        // Verify active count decreased
        let stats = manager.get_connection_stats().await.unwrap();
        assert_eq!(stats.active_connections, 0);
    }

    #[tokio::test]
    async fn test_cleanup_nonexistent_connection() {
        let manager = ServerConnectionManager::new();
        let fake_id = Uuid::new_v4();

        let result = manager.cleanup_connection(&fake_id).await;
        assert!(result.is_err());

        match result.unwrap_err() {
            StreamError::ConnectionNotFound { connection_id } => {
                assert_eq!(connection_id, fake_id);
            }
            other => panic!("Expected ConnectionNotFound error, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_update_nonexistent_connection_status() {
        let manager = ServerConnectionManager::new();
        let fake_id = Uuid::new_v4();

        let result = manager
            .update_connection_status(&fake_id, ConnectionStatus::Active)
            .await;
        assert!(result.is_err());

        match result.unwrap_err() {
            StreamError::ConnectionNotFound { connection_id } => {
                assert_eq!(connection_id, fake_id);
            }
            other => panic!("Expected ConnectionNotFound error, got: {:?}", other),
        }
    }

    // Removed tests for deleted methods: test_active_connection_count, test_get_all_connection_ids

    #[tokio::test]
    async fn test_connection_statistics() {
        let manager = ServerConnectionManager::new();
        let Some((connection, _)) = create_test_connection().await else {
            return;
        };
        let mapping = create_test_mapping();

        // Create connection
        let connection_id = manager
            .handle_new_connection(connection, mapping)
            .await
            .unwrap();

        let stats = manager.get_connection_stats().await.unwrap();
        assert_eq!(stats.total_connections, 1);
        assert_eq!(stats.active_connections, 0);
        assert_eq!(stats.failed_connections, 0);

        // Make it active
        manager
            .update_connection_status(&connection_id, ConnectionStatus::Active)
            .await
            .unwrap();

        let stats = manager.get_connection_stats().await.unwrap();
        assert_eq!(stats.active_connections, 1);

        // Fail the connection
        manager
            .update_connection_status(
                &connection_id,
                ConnectionStatus::Error("Test error".to_string()),
            )
            .await
            .unwrap();

        let stats = manager.get_connection_stats().await.unwrap();
        assert_eq!(stats.active_connections, 0);
    }
}
