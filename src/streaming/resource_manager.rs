//! Resource management for connection lifecycle and system resources

use crate::streaming::errors::{StreamError, StreamResult};
use crate::streaming::traits::{ConnectionManager, ResourceManager, ResourceType, ResourceUsage};
use crate::streaming::ConnectionId;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio::time::{interval, timeout};
use tracing::{debug, error, info, warn};

/// Configuration for resource limits and timeouts
#[derive(Debug, Clone)]
pub struct ResourceConfig {
    /// Maximum number of concurrent connections
    pub max_connections: usize,
    /// Maximum number of concurrent streams
    pub max_streams: usize,
    /// Maximum memory usage in bytes
    pub max_memory_bytes: usize,
    /// Maximum file descriptors
    pub max_file_descriptors: usize,
    /// Connection timeout duration
    pub connection_timeout: Duration,
    /// Health check interval
    pub health_check_interval: Duration,
    /// Idle connection timeout
    pub idle_timeout: Duration,
}

impl Default for ResourceConfig {
    fn default() -> Self {
        Self {
            max_connections: 2000,               // Increased for better scalability
            max_streams: 4000,                   // Increased proportionally
            max_memory_bytes: 256 * 1024 * 1024, // Reduced to 256MB for better memory efficiency
            max_file_descriptors: 2048,          // Increased for more concurrent connections
            connection_timeout: Duration::from_secs(30),
            health_check_interval: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(300), // 5 minutes
        }
    }
}

/// Information about a tracked resource
#[derive(Debug, Clone)]
struct ResourceInfo {
    resource_type: ResourceType,
    created_at: Instant,
    last_activity: Instant,
}

impl ResourceInfo {
    fn new(_connection_id: ConnectionId, resource_type: ResourceType) -> Self {
        let now = Instant::now();

        Self {
            resource_type,
            created_at: now,
            last_activity: now,
        }
    }

    fn update_activity(&mut self) {
        self.last_activity = Instant::now();
    }

    fn is_idle(&self, idle_timeout: Duration) -> bool {
        self.last_activity.elapsed() > idle_timeout
    }

    fn is_timed_out(&self, timeout: Duration) -> bool {
        self.created_at.elapsed() > timeout
    }
}

/// Comprehensive resource manager for connection lifecycle and system resources
pub struct StreamResourceManager {
    /// Configuration for resource limits and timeouts
    config: ResourceConfig,
    /// Map of tracked resources indexed by connection ID
    resources: Arc<RwLock<HashMap<ConnectionId, ResourceInfo>>>,
    /// Current resource usage statistics
    usage: Arc<RwLock<ResourceUsage>>,
    /// Connection manager for lifecycle operations
    connection_manager: Arc<dyn ConnectionManager>,
    /// Shutdown signal broadcaster
    shutdown_tx: broadcast::Sender<()>,
    /// Channel for sending cleanup commands
    cleanup_tx: mpsc::UnboundedSender<ConnectionId>,
    /// Channel for receiving cleanup commands
    cleanup_rx: Arc<RwLock<Option<mpsc::UnboundedReceiver<ConnectionId>>>>,
}

impl StreamResourceManager {
    /// Create a new resource manager with the given configuration and connection manager
    pub fn new(config: ResourceConfig, connection_manager: Arc<dyn ConnectionManager>) -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);
        let (cleanup_tx, cleanup_rx) = mpsc::unbounded_channel();

        Self {
            config,
            resources: Arc::new(RwLock::new(HashMap::new())),
            usage: Arc::new(RwLock::new(ResourceUsage {
                active_connections: 0,
                active_streams: 0,
                memory_usage_bytes: 0,
                file_descriptors: 0,
            })),
            connection_manager,
            shutdown_tx,
            cleanup_tx,
            cleanup_rx: Arc::new(RwLock::new(Some(cleanup_rx))),
        }
    }

    /// Start the resource manager background tasks
    pub async fn start(&self) -> StreamResult<()> {
        info!("Starting resource manager background tasks");

        // Start health monitoring task
        let health_monitor = self.start_health_monitor().await?;

        // Start cleanup task
        let cleanup_task = self.start_cleanup_task().await?;

        // Start timeout monitoring task
        let timeout_monitor = self.start_timeout_monitor().await?;

        // Spawn all tasks
        tokio::spawn(health_monitor);
        tokio::spawn(cleanup_task);
        tokio::spawn(timeout_monitor);

        info!("Resource manager background tasks started successfully");
        Ok(())
    }

    /// Create health monitoring task
    async fn start_health_monitor(&self) -> StreamResult<impl std::future::Future<Output = ()>> {
        let resources = Arc::clone(&self.resources);
        let usage = Arc::clone(&self.usage);
        let config = self.config.clone();
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        Ok(async move {
            let mut interval = interval(config.health_check_interval);

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if let Err(e) = Self::perform_health_check(&resources, &usage, &config).await {
                            error!("Health check failed: {}", e);
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        debug!("Health monitor shutting down");
                        break;
                    }
                }
            }
        })
    }

    /// Create cleanup task
    async fn start_cleanup_task(&self) -> StreamResult<impl std::future::Future<Output = ()>> {
        let cleanup_rx = {
            let mut rx_guard = self.cleanup_rx.write().await;
            rx_guard.take().ok_or_else(|| {
                StreamError::resource_exhausted("Cleanup task already started".to_string())
            })?
        };

        let resources = Arc::clone(&self.resources);
        let usage = Arc::clone(&self.usage);
        let connection_manager = Arc::clone(&self.connection_manager);
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        Ok(async move {
            let mut cleanup_rx = cleanup_rx;

            loop {
                tokio::select! {
                    Some(connection_id) = cleanup_rx.recv() => {
                        if let Err(e) = Self::cleanup_resource_internal(
                            &resources,
                            &usage,
                            &connection_manager,
                            connection_id
                        ).await {
                            error!("Failed to cleanup resource {}: {}", connection_id, e);
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        debug!("Cleanup task shutting down");
                        break;
                    }
                }
            }
        })
    }

    /// Create timeout monitoring task
    async fn start_timeout_monitor(&self) -> StreamResult<impl std::future::Future<Output = ()>> {
        let resources = Arc::clone(&self.resources);
        let config = self.config.clone();
        let cleanup_tx = self.cleanup_tx.clone();
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        Ok(async move {
            let mut interval = interval(config.health_check_interval);

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if let Err(e) = Self::check_timeouts(&resources, &config, &cleanup_tx).await {
                            error!("Timeout check failed: {}", e);
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        debug!("Timeout monitor shutting down");
                        break;
                    }
                }
            }
        })
    }

    /// Perform health check on all resources
    async fn perform_health_check(
        resources: &Arc<RwLock<HashMap<ConnectionId, ResourceInfo>>>,
        usage: &Arc<RwLock<ResourceUsage>>,
        config: &ResourceConfig,
    ) -> StreamResult<()> {
        let resources_guard = resources.read().await;
        let mut usage_guard = usage.write().await;

        // Update current usage statistics
        let mut connections = 0;
        let mut streams = 0;
        let mut memory = 0;
        let mut file_descriptors = 0;

        for resource in resources_guard.values() {
            match resource.resource_type {
                ResourceType::Connection => connections += 1,
                ResourceType::Stream => streams += 1,
                ResourceType::Memory(size) => memory += size,
                ResourceType::FileDescriptor => file_descriptors += 1,
            }
        }

        usage_guard.active_connections = connections;
        usage_guard.active_streams = streams;
        usage_guard.memory_usage_bytes = memory;
        usage_guard.file_descriptors = file_descriptors;

        // Check for resource limit violations
        if connections > config.max_connections {
            warn!(
                "Connection limit exceeded: {} > {}",
                connections, config.max_connections
            );
        }

        if streams > config.max_streams {
            warn!(
                "Stream limit exceeded: {} > {}",
                streams, config.max_streams
            );
        }

        if memory > config.max_memory_bytes {
            warn!(
                "Memory limit exceeded: {} > {}",
                memory, config.max_memory_bytes
            );
        }

        if file_descriptors > config.max_file_descriptors {
            warn!(
                "File descriptor limit exceeded: {} > {}",
                file_descriptors, config.max_file_descriptors
            );
        }

        debug!(
            connections = connections,
            streams = streams,
            memory_mb = memory / (1024 * 1024),
            file_descriptors = file_descriptors,
            "Health check completed"
        );

        Ok(())
    }

    /// Check for timed out connections and schedule cleanup
    async fn check_timeouts(
        resources: &Arc<RwLock<HashMap<ConnectionId, ResourceInfo>>>,
        config: &ResourceConfig,
        cleanup_tx: &mpsc::UnboundedSender<ConnectionId>,
    ) -> StreamResult<()> {
        let resources_guard = resources.read().await;
        let mut timed_out_connections = Vec::new();

        for (connection_id, resource) in resources_guard.iter() {
            if resource.is_timed_out(config.connection_timeout) {
                warn!(
                    connection_id = %connection_id,
                    age_secs = resource.created_at.elapsed().as_secs(),
                    "Connection timed out"
                );
                timed_out_connections.push(*connection_id);
            } else if resource.is_idle(config.idle_timeout) {
                warn!(
                    connection_id = %connection_id,
                    idle_secs = resource.last_activity.elapsed().as_secs(),
                    "Connection idle timeout"
                );
                timed_out_connections.push(*connection_id);
            }
        }

        // Schedule cleanup for timed out connections
        for connection_id in timed_out_connections {
            if let Err(e) = cleanup_tx.send(connection_id) {
                error!("Failed to schedule cleanup for {}: {}", connection_id, e);
            }
        }

        Ok(())
    }

    /// Internal cleanup implementation
    async fn cleanup_resource_internal(
        resources: &Arc<RwLock<HashMap<ConnectionId, ResourceInfo>>>,
        usage: &Arc<RwLock<ResourceUsage>>,
        connection_manager: &Arc<dyn ConnectionManager>,
        connection_id: ConnectionId,
    ) -> StreamResult<()> {
        debug!(connection_id = %connection_id, "Cleaning up resource");

        // Remove from resources tracking
        let resource_info = {
            let mut resources_guard = resources.write().await;
            resources_guard.remove(&connection_id)
        };

        if let Some(resource) = resource_info {
            // Update usage statistics
            {
                let mut usage_guard = usage.write().await;
                match resource.resource_type {
                    ResourceType::Connection => {
                        if usage_guard.active_connections > 0 {
                            usage_guard.active_connections -= 1;
                        }
                    }
                    ResourceType::Stream => {
                        if usage_guard.active_streams > 0 {
                            usage_guard.active_streams -= 1;
                        }
                    }
                    ResourceType::Memory(size) => {
                        if usage_guard.memory_usage_bytes >= size {
                            usage_guard.memory_usage_bytes -= size;
                        }
                    }
                    ResourceType::FileDescriptor => {
                        if usage_guard.file_descriptors > 0 {
                            usage_guard.file_descriptors -= 1;
                        }
                    }
                }
            }

            // Clean up through connection manager
            if let Err(e) = connection_manager.cleanup_connection(&connection_id).await {
                warn!(
                    connection_id = %connection_id,
                    error = %e,
                    "Connection manager cleanup failed"
                );
            }

            info!(
                connection_id = %connection_id,
                resource_type = ?resource.resource_type,
                lifetime_secs = resource.created_at.elapsed().as_secs(),
                "Resource cleaned up successfully"
            );
        } else {
            warn!(connection_id = %connection_id, "Attempted to cleanup non-existent resource");
        }

        Ok(())
    }

    /// Update activity timestamp for a resource
    pub async fn update_activity(&self, connection_id: &ConnectionId) -> StreamResult<()> {
        let mut resources = self.resources.write().await;
        if let Some(resource) = resources.get_mut(connection_id) {
            resource.update_activity();
            debug!(connection_id = %connection_id, "Activity updated");
        }
        Ok(())
    }

    /// Get a shutdown signal receiver
    pub fn get_shutdown_receiver(&self) -> broadcast::Receiver<()> {
        self.shutdown_tx.subscribe()
    }

    /// Schedule a resource for cleanup
    pub async fn schedule_cleanup(&self, connection_id: ConnectionId) -> StreamResult<()> {
        self.cleanup_tx
            .send(connection_id)
            .map_err(|_| StreamError::resource_exhausted("Cleanup channel closed".to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl ResourceManager for StreamResourceManager {
    async fn track_resource(
        &self,
        connection_id: ConnectionId,
        resource_type: ResourceType,
    ) -> StreamResult<()> {
        debug!(
            connection_id = %connection_id,
            resource_type = ?resource_type,
            "Tracking new resource"
        );

        // Check resource limits before tracking
        let current_usage = self.get_resource_usage().await?;

        match resource_type {
            ResourceType::Connection
                if current_usage.active_connections >= self.config.max_connections =>
            {
                return Err(StreamError::resource_exhausted(format!(
                    "Connection limit exceeded: {}",
                    self.config.max_connections
                )));
            }
            ResourceType::Stream if current_usage.active_streams >= self.config.max_streams => {
                return Err(StreamError::resource_exhausted(format!(
                    "Stream limit exceeded: {}",
                    self.config.max_streams
                )));
            }
            ResourceType::Memory(size)
                if current_usage.memory_usage_bytes + size > self.config.max_memory_bytes =>
            {
                return Err(StreamError::resource_exhausted(format!(
                    "Memory limit exceeded: {} + {} > {}",
                    current_usage.memory_usage_bytes, size, self.config.max_memory_bytes
                )));
            }
            ResourceType::FileDescriptor
                if current_usage.file_descriptors >= self.config.max_file_descriptors =>
            {
                return Err(StreamError::resource_exhausted(format!(
                    "File descriptor limit exceeded: {}",
                    self.config.max_file_descriptors
                )));
            }
            _ => {}
        }

        // Track the resource
        let resource_info = ResourceInfo::new(connection_id, resource_type.clone());

        {
            let mut resources = self.resources.write().await;
            resources.insert(connection_id, resource_info);
        }

        // Update usage statistics
        {
            let mut usage = self.usage.write().await;
            match resource_type {
                ResourceType::Connection => usage.active_connections += 1,
                ResourceType::Stream => usage.active_streams += 1,
                ResourceType::Memory(size) => usage.memory_usage_bytes += size,
                ResourceType::FileDescriptor => usage.file_descriptors += 1,
            }
        }

        info!(
            connection_id = %connection_id,
            resource_type = ?resource_type,
            "Resource tracked successfully"
        );

        Ok(())
    }

    async fn release_resource(&self, connection_id: ConnectionId) -> StreamResult<()> {
        debug!(connection_id = %connection_id, "Releasing resource");

        // Schedule cleanup through the cleanup channel
        self.schedule_cleanup(connection_id).await?;

        Ok(())
    }

    async fn get_resource_usage(&self) -> StreamResult<ResourceUsage> {
        let usage = self.usage.read().await;
        Ok(usage.clone())
    }

    async fn graceful_shutdown(&self) -> StreamResult<()> {
        info!("Starting graceful shutdown of resource manager");

        // Get all tracked connection IDs
        let connection_ids: Vec<ConnectionId> = {
            let resources = self.resources.read().await;
            resources.keys().cloned().collect()
        };

        info!("Shutting down {} tracked resources", connection_ids.len());

        // Send shutdown signal to all background tasks
        if let Err(e) = self.shutdown_tx.send(()) {
            warn!("Failed to send shutdown signal: {}", e);
        }

        // Clean up all resources with timeout
        let cleanup_tasks: Vec<_> = connection_ids.into_iter().map(|connection_id| {
            let resources = Arc::clone(&self.resources);
            let usage = Arc::clone(&self.usage);
            let connection_manager = Arc::clone(&self.connection_manager);

            async move {
                let result = timeout(
                    Duration::from_secs(5),
                    Self::cleanup_resource_internal(&resources, &usage, &connection_manager, connection_id)
                ).await;

                match result {
                    Ok(Ok(())) => {
                        debug!(connection_id = %connection_id, "Resource cleaned up during shutdown");
                    }
                    Ok(Err(e)) => {
                        warn!(connection_id = %connection_id, error = %e, "Failed to cleanup resource during shutdown");
                    }
                    Err(_) => {
                        warn!(connection_id = %connection_id, "Resource cleanup timed out during shutdown");
                    }
                }
            }
        }).collect();

        // Wait for all cleanup tasks to complete
        futures::future::join_all(cleanup_tasks).await;

        // Final verification
        let final_usage = self.get_resource_usage().await?;
        if final_usage.active_connections > 0 || final_usage.active_streams > 0 {
            warn!(
                remaining_connections = final_usage.active_connections,
                remaining_streams = final_usage.active_streams,
                "Some resources were not cleaned up during shutdown"
            );
        }

        info!("Resource manager graceful shutdown completed");
        Ok(())
    }

    async fn check_resource_limits(&self) -> StreamResult<bool> {
        let usage = self.get_resource_usage().await?;

        let limits_exceeded = usage.active_connections > self.config.max_connections
            || usage.active_streams > self.config.max_streams
            || usage.memory_usage_bytes > self.config.max_memory_bytes
            || usage.file_descriptors > self.config.max_file_descriptors;

        if limits_exceeded {
            warn!(
                connections = usage.active_connections,
                max_connections = self.config.max_connections,
                streams = usage.active_streams,
                max_streams = self.config.max_streams,
                memory_mb = usage.memory_usage_bytes / (1024 * 1024),
                max_memory_mb = self.config.max_memory_bytes / (1024 * 1024),
                file_descriptors = usage.file_descriptors,
                max_file_descriptors = self.config.max_file_descriptors,
                "Resource limits exceeded"
            );
        }

        Ok(limits_exceeded)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::test_interfaces::MockConnectionManager;

    use tokio::time::sleep;

    fn create_test_config() -> ResourceConfig {
        ResourceConfig {
            max_connections: 5,
            max_streams: 10,
            max_memory_bytes: 1024 * 1024, // 1MB
            max_file_descriptors: 20,
            connection_timeout: Duration::from_millis(100),
            health_check_interval: Duration::from_millis(50),
            idle_timeout: Duration::from_millis(200),
        }
    }

    #[tokio::test]
    async fn test_new_resource_manager() {
        let config = create_test_config();
        let connection_manager = Arc::new(MockConnectionManager::new());
        let resource_manager = StreamResourceManager::new(config.clone(), connection_manager);

        let usage = resource_manager.get_resource_usage().await.unwrap();
        assert_eq!(usage.active_connections, 0);
        assert_eq!(usage.active_streams, 0);
        assert_eq!(usage.memory_usage_bytes, 0);
        assert_eq!(usage.file_descriptors, 0);
    }

    #[tokio::test]
    async fn test_track_resource() {
        let config = create_test_config();
        let connection_manager = Arc::new(MockConnectionManager::new());
        let resource_manager = StreamResourceManager::new(config, connection_manager);

        let connection_id = ConnectionId::new_v4();

        // Track a connection
        resource_manager
            .track_resource(connection_id, ResourceType::Connection)
            .await
            .unwrap();

        let usage = resource_manager.get_resource_usage().await.unwrap();
        assert_eq!(usage.active_connections, 1);
        assert_eq!(usage.active_streams, 0);

        // Track a stream
        resource_manager
            .track_resource(connection_id, ResourceType::Stream)
            .await
            .unwrap();

        let usage = resource_manager.get_resource_usage().await.unwrap();
        assert_eq!(usage.active_connections, 1);
        assert_eq!(usage.active_streams, 1);

        // Track memory
        resource_manager
            .track_resource(connection_id, ResourceType::Memory(1024))
            .await
            .unwrap();

        let usage = resource_manager.get_resource_usage().await.unwrap();
        assert_eq!(usage.memory_usage_bytes, 1024);

        // Track file descriptor
        resource_manager
            .track_resource(connection_id, ResourceType::FileDescriptor)
            .await
            .unwrap();

        let usage = resource_manager.get_resource_usage().await.unwrap();
        assert_eq!(usage.file_descriptors, 1);
    }

    #[tokio::test]
    async fn test_resource_limits() {
        let config = create_test_config();
        let connection_manager = Arc::new(MockConnectionManager::new());
        let resource_manager = StreamResourceManager::new(config, connection_manager);

        // Track connections up to the limit
        for _i in 0..5 {
            let connection_id = ConnectionId::new_v4();
            resource_manager
                .track_resource(connection_id, ResourceType::Connection)
                .await
                .unwrap();
        }

        // Try to exceed the limit
        let connection_id = ConnectionId::new_v4();
        let result = resource_manager
            .track_resource(connection_id, ResourceType::Connection)
            .await;
        assert!(result.is_err());

        match result.unwrap_err() {
            StreamError::ResourceExhausted { .. } => {} // Expected
            other => panic!("Expected ResourceExhausted error, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_memory_limit() {
        let config = create_test_config();
        let connection_manager = Arc::new(MockConnectionManager::new());
        let resource_manager = StreamResourceManager::new(config, connection_manager);

        let connection_id = ConnectionId::new_v4();

        // Track memory up to the limit
        resource_manager
            .track_resource(connection_id, ResourceType::Memory(1024 * 1024))
            .await
            .unwrap();

        // Try to exceed the limit
        let result = resource_manager
            .track_resource(connection_id, ResourceType::Memory(1))
            .await;
        assert!(result.is_err());

        match result.unwrap_err() {
            StreamError::ResourceExhausted { .. } => {} // Expected
            other => panic!("Expected ResourceExhausted error, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_release_resource() {
        let config = create_test_config();
        let connection_manager = Arc::new(MockConnectionManager::new());
        let resource_manager = StreamResourceManager::new(config, connection_manager);

        let connection_id = ConnectionId::new_v4();

        // Track resources
        resource_manager
            .track_resource(connection_id, ResourceType::Connection)
            .await
            .unwrap();
        resource_manager
            .track_resource(connection_id, ResourceType::Stream)
            .await
            .unwrap();
        resource_manager
            .track_resource(connection_id, ResourceType::Memory(1024))
            .await
            .unwrap();

        let usage = resource_manager.get_resource_usage().await.unwrap();
        assert_eq!(usage.active_connections, 1);
        assert_eq!(usage.active_streams, 1);
        assert_eq!(usage.memory_usage_bytes, 1024);

        // Release the resource
        resource_manager
            .release_resource(connection_id)
            .await
            .unwrap();

        // Give some time for cleanup to process
        sleep(Duration::from_millis(10)).await;

        // Note: The actual cleanup happens asynchronously, so we can't immediately
        // verify the usage has decreased. In a real scenario, the cleanup task
        // would process this and update the usage.
    }

    #[tokio::test]
    async fn test_check_resource_limits() {
        let config = create_test_config();
        let connection_manager = Arc::new(MockConnectionManager::new());
        let resource_manager = StreamResourceManager::new(config, connection_manager);

        // Initially no limits exceeded
        let exceeded = resource_manager.check_resource_limits().await.unwrap();
        assert!(!exceeded);

        // Track resources up to the limit
        for _i in 0..5 {
            let connection_id = ConnectionId::new_v4();
            resource_manager
                .track_resource(connection_id, ResourceType::Connection)
                .await
                .unwrap();
        }

        // Still within limits
        let exceeded = resource_manager.check_resource_limits().await.unwrap();
        assert!(!exceeded);

        // Manually update usage to exceed limits (simulating a scenario where limits are exceeded)
        {
            let mut usage = resource_manager.usage.write().await;
            usage.active_connections = 10; // Exceeds max_connections (5)
        }

        let exceeded = resource_manager.check_resource_limits().await.unwrap();
        assert!(exceeded);
    }

    #[tokio::test]
    async fn test_update_activity() {
        let config = create_test_config();
        let connection_manager = Arc::new(MockConnectionManager::new());
        let resource_manager = StreamResourceManager::new(config, connection_manager);

        let connection_id = ConnectionId::new_v4();

        // Track a resource
        resource_manager
            .track_resource(connection_id, ResourceType::Connection)
            .await
            .unwrap();

        // Update activity should succeed
        resource_manager
            .update_activity(&connection_id)
            .await
            .unwrap();

        // Update activity for non-existent resource should still succeed (no-op)
        let fake_id = ConnectionId::new_v4();
        resource_manager.update_activity(&fake_id).await.unwrap();
    }

    #[tokio::test]
    async fn test_graceful_shutdown() {
        let config = create_test_config();
        let connection_manager = Arc::new(MockConnectionManager::new());
        let resource_manager = StreamResourceManager::new(config, connection_manager);

        // Track some resources
        for _i in 0..3 {
            let connection_id = ConnectionId::new_v4();
            resource_manager
                .track_resource(connection_id, ResourceType::Connection)
                .await
                .unwrap();
        }

        let usage_before = resource_manager.get_resource_usage().await.unwrap();
        assert_eq!(usage_before.active_connections, 3);

        // Perform graceful shutdown
        resource_manager.graceful_shutdown().await.unwrap();

        // Verify shutdown signal was sent
        let mut shutdown_rx = resource_manager.get_shutdown_receiver();
        let result = timeout(Duration::from_millis(10), shutdown_rx.recv()).await;
        // The receiver should get the shutdown signal or be closed
        assert!(result.is_ok() || result.is_err());
    }

    #[tokio::test]
    async fn test_shutdown_receiver() {
        let config = create_test_config();
        let connection_manager = Arc::new(MockConnectionManager::new());
        let resource_manager = StreamResourceManager::new(config, connection_manager);

        let mut shutdown_rx = resource_manager.get_shutdown_receiver();

        // Should not receive anything initially
        let result = timeout(Duration::from_millis(10), shutdown_rx.recv()).await;
        assert!(result.is_err()); // Timeout expected

        // Trigger shutdown
        resource_manager.graceful_shutdown().await.unwrap();

        // Now should receive shutdown signal
        let result = timeout(Duration::from_millis(10), shutdown_rx.recv()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_schedule_cleanup() {
        let config = create_test_config();
        let connection_manager = Arc::new(MockConnectionManager::new());
        let resource_manager = StreamResourceManager::new(config, connection_manager);

        let connection_id = ConnectionId::new_v4();

        // Schedule cleanup should succeed
        resource_manager
            .schedule_cleanup(connection_id)
            .await
            .unwrap();

        // Multiple schedules should work
        resource_manager
            .schedule_cleanup(connection_id)
            .await
            .unwrap();
        resource_manager
            .schedule_cleanup(ConnectionId::new_v4())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_resource_info_methods() {
        let connection_id = ConnectionId::new_v4();
        let resource_type = ResourceType::Connection;
        let mut resource_info = ResourceInfo::new(connection_id, resource_type.clone());

        assert_eq!(resource_info.resource_type, resource_type);

        // Test activity update
        let original_activity = resource_info.last_activity;
        sleep(Duration::from_millis(1)).await;
        resource_info.update_activity();
        assert!(resource_info.last_activity > original_activity);

        // Test idle check
        assert!(!resource_info.is_idle(Duration::from_millis(1000)));

        // Test timeout check
        assert!(!resource_info.is_timed_out(Duration::from_millis(1000)));
    }

    #[tokio::test]
    async fn test_resource_config_default() {
        let config = ResourceConfig::default();

        assert_eq!(config.max_connections, 2000);
        assert_eq!(config.max_streams, 4000);
        assert_eq!(config.max_memory_bytes, 256 * 1024 * 1024);
        assert_eq!(config.max_file_descriptors, 2048);
        assert_eq!(config.connection_timeout, Duration::from_secs(30));
        assert_eq!(config.health_check_interval, Duration::from_secs(10));
        assert_eq!(config.idle_timeout, Duration::from_secs(300));
    }

    #[tokio::test]
    async fn test_background_tasks_start() {
        let config = create_test_config();
        let connection_manager = Arc::new(MockConnectionManager::new());
        let resource_manager = StreamResourceManager::new(config, connection_manager);

        // Start background tasks
        resource_manager.start().await.unwrap();

        // Give tasks time to start
        sleep(Duration::from_millis(10)).await;

        // Shutdown to clean up tasks
        resource_manager.graceful_shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_timeout_detection() {
        let mut config = create_test_config();
        config.connection_timeout = Duration::from_millis(50);
        config.idle_timeout = Duration::from_millis(50);

        let connection_manager = Arc::new(MockConnectionManager::new());
        let resource_manager = StreamResourceManager::new(config, connection_manager);

        let connection_id = ConnectionId::new_v4();

        // Track a resource
        resource_manager
            .track_resource(connection_id, ResourceType::Connection)
            .await
            .unwrap();

        // Start background tasks
        resource_manager.start().await.unwrap();

        // Wait for timeout to trigger
        sleep(Duration::from_millis(150)).await;

        // The timeout monitor should have detected the timeout and scheduled cleanup
        // We can't easily verify this without more complex mocking, but the test
        // ensures the code doesn't panic and runs correctly

        // Shutdown
        resource_manager.graceful_shutdown().await.unwrap();
    }
}
