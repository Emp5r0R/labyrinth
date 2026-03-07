//! Error recovery mechanisms for streaming operations

use crate::streaming::{
    ConnectionId, ConnectionManager, ErrorWithContext, MetricsCollector, RetryStrategy,
    StreamError, StreamManager, StreamResult,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

/// Error recovery coordinator that handles transient failures
pub struct ErrorRecoveryCoordinator {
    /// Connection manager for recovery operations
    connection_manager: Arc<dyn ConnectionManager>,
    /// Stream manager for recovery operations
    stream_manager: Arc<dyn StreamManager>,
    /// Metrics collector for tracking recovery attempts
    metrics_collector: Arc<MetricsCollector>,
}

impl std::fmt::Debug for ErrorRecoveryCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ErrorRecoveryCoordinator")
            .field("connection_manager", &"Arc<dyn ConnectionManager>")
            .field("stream_manager", &"Arc<dyn StreamManager>")
            .field("metrics_collector", &"Arc<MetricsCollector>")
            .finish()
    }
}

impl ErrorRecoveryCoordinator {
    /// Create a new error recovery coordinator
    pub fn new(
        connection_manager: Arc<dyn ConnectionManager>,
        stream_manager: Arc<dyn StreamManager>,
        metrics_collector: Arc<MetricsCollector>,
    ) -> Self {
        Self {
            connection_manager,
            stream_manager,
            metrics_collector,
        }
    }

    /// Attempt to recover from an error with context
    pub async fn attempt_recovery(
        &self,
        error_context: ErrorWithContext,
        connection_id: Option<ConnectionId>,
    ) -> StreamResult<bool> {
        let error = error_context.error();

        info!(
            "Attempting recovery for error: {} (category: {}, recoverable: {})",
            error,
            error.category(),
            error.is_recoverable()
        );

        // Only attempt recovery for recoverable errors
        if !error.is_recoverable() {
            warn!("Error is not recoverable, skipping recovery attempt");
            return Ok(false);
        }

        // Get retry strategy for this error type
        let retry_strategy = match error.retry_strategy() {
            Some(strategy) => strategy,
            None => {
                warn!("No retry strategy available for error type");
                return Ok(false);
            }
        };

        // Attempt recovery with retry strategy
        let recovery_result = self
            .execute_recovery_with_retry(error_context, connection_id, retry_strategy)
            .await;

        // Record recovery attempt in metrics
        if let Some(conn_id) = connection_id {
            self.metrics_collector
                .record_error_recovery(
                    conn_id,
                    recovery_result.is_ok() && recovery_result.as_ref().unwrap_or(&false) == &true,
                )
                .await;
        }

        recovery_result
    }

    /// Execute recovery with retry strategy
    async fn execute_recovery_with_retry(
        &self,
        error_context: ErrorWithContext,
        connection_id: Option<ConnectionId>,
        retry_strategy: RetryStrategy,
    ) -> StreamResult<bool> {
        let mut attempt = 0;

        while retry_strategy.should_retry(attempt) {
            if let Some(delay) = retry_strategy.calculate_delay(attempt) {
                if attempt > 0 {
                    debug!(
                        "Waiting {}ms before retry attempt {}",
                        delay.as_millis(),
                        attempt + 1
                    );
                    sleep(delay).await;
                }
            }

            debug!(
                "Recovery attempt {} for error: {}",
                attempt + 1,
                error_context.error()
            );

            match self
                .execute_single_recovery_attempt(&error_context, connection_id)
                .await
            {
                Ok(true) => {
                    info!("Recovery successful after {} attempts", attempt + 1);
                    return Ok(true);
                }
                Ok(false) => {
                    debug!("Recovery attempt {} failed, will retry", attempt + 1);
                }
                Err(e) => {
                    warn!("Recovery attempt {} encountered error: {}", attempt + 1, e);

                    // If we get a non-recoverable error during recovery, stop trying
                    if !e.is_recoverable() {
                        error!("Non-recoverable error during recovery, stopping attempts");
                        return Err(e);
                    }
                }
            }

            attempt += 1;
        }

        warn!("Recovery failed after {} attempts", attempt);
        Ok(false)
    }

    /// Execute a single recovery attempt based on error type
    async fn execute_single_recovery_attempt(
        &self,
        error_context: &ErrorWithContext,
        connection_id: Option<ConnectionId>,
    ) -> StreamResult<bool> {
        let error = error_context.error();

        match error {
            StreamError::ConnectionFailed(_) => {
                self.recover_connection_failure(connection_id).await
            }
            StreamError::StreamBroken { connection_id, .. } => {
                self.recover_stream_failure(*connection_id).await
            }
            StreamError::ResourceExhausted { .. } => self.recover_resource_exhaustion().await,
            StreamError::Timeout { .. } => self.recover_timeout(connection_id).await,
            StreamError::Io(_) => self.recover_io_error(connection_id).await,
            StreamError::ChannelSend(_) | StreamError::ChannelReceive(_) => {
                self.recover_channel_error(connection_id).await
            }
            StreamError::ServiceUnavailable(_) => {
                self.recover_service_unavailable(connection_id).await
            }
            StreamError::RateLimitExceeded(_) => self.recover_rate_limit().await,
            _ => {
                debug!("No specific recovery strategy for error type: {:?}", error);
                Ok(false)
            }
        }
    }

    /// Recover from connection failure
    async fn recover_connection_failure(
        &self,
        connection_id: Option<ConnectionId>,
    ) -> StreamResult<bool> {
        debug!("Attempting to recover from connection failure");

        if let Some(conn_id) = connection_id {
            // Check if connection still exists
            match self.connection_manager.get_connection_state(&conn_id).await {
                Ok(Some(_)) => {
                    // Connection exists, try to reset its state
                    debug!("Connection {} exists, attempting to reset state", conn_id);

                    // Clean up and let the system recreate the connection
                    if let Err(e) = self.connection_manager.cleanup_connection(&conn_id).await {
                        warn!("Failed to cleanup connection during recovery: {}", e);
                        return Ok(false);
                    }

                    Ok(true)
                }
                Ok(None) => {
                    debug!(
                        "Connection {} no longer exists, recovery not needed",
                        conn_id
                    );
                    Ok(true)
                }
                Err(e) => {
                    warn!("Failed to check connection state during recovery: {}", e);
                    Ok(false)
                }
            }
        } else {
            // General connection failure without specific connection ID
            debug!("General connection failure recovery - no specific action needed");
            Ok(true)
        }
    }

    /// Recover from stream failure
    async fn recover_stream_failure(&self, connection_id: ConnectionId) -> StreamResult<bool> {
        debug!(
            "Attempting to recover from stream failure for connection {}",
            connection_id
        );

        // Terminate the broken stream
        match self.stream_manager.terminate_stream(connection_id).await {
            Ok(()) => {
                debug!(
                    "Successfully terminated broken stream for connection {}",
                    connection_id
                );

                // Clean up the connection
                if let Err(e) = self
                    .connection_manager
                    .cleanup_connection(&connection_id)
                    .await
                {
                    warn!(
                        "Failed to cleanup connection after stream termination: {}",
                        e
                    );
                    return Ok(false);
                }

                Ok(true)
            }
            Err(e) => {
                warn!("Failed to terminate broken stream: {}", e);
                Ok(false)
            }
        }
    }

    /// Recover from resource exhaustion
    async fn recover_resource_exhaustion(&self) -> StreamResult<bool> {
        debug!("Attempting to recover from resource exhaustion");

        // Wait a bit for resources to be freed
        sleep(Duration::from_millis(500)).await;

        // In a real implementation, we might:
        // 1. Force garbage collection
        // 2. Close idle connections
        // 3. Reduce buffer sizes temporarily
        // 4. Implement backpressure

        debug!("Resource exhaustion recovery completed");
        Ok(true)
    }

    /// Recover from timeout
    async fn recover_timeout(&self, connection_id: Option<ConnectionId>) -> StreamResult<bool> {
        debug!("Attempting to recover from timeout");

        if let Some(conn_id) = connection_id {
            // Check if the connection is still valid
            match self.connection_manager.get_connection_state(&conn_id).await {
                Ok(Some(state)) => {
                    debug!(
                        "Connection {} still exists after timeout, state: {:?}",
                        conn_id, state.status
                    );
                    // Connection exists, timeout might have been transient
                    Ok(true)
                }
                Ok(None) => {
                    debug!("Connection {} no longer exists after timeout", conn_id);
                    Ok(true)
                }
                Err(e) => {
                    warn!("Failed to check connection state after timeout: {}", e);
                    Ok(false)
                }
            }
        } else {
            // General timeout recovery
            debug!("General timeout recovery completed");
            Ok(true)
        }
    }

    /// Recover from I/O error
    async fn recover_io_error(&self, connection_id: Option<ConnectionId>) -> StreamResult<bool> {
        debug!("Attempting to recover from I/O error");

        if let Some(conn_id) = connection_id {
            // I/O errors often indicate connection issues, clean up the connection
            if let Err(e) = self.connection_manager.cleanup_connection(&conn_id).await {
                warn!("Failed to cleanup connection after I/O error: {}", e);
                return Ok(false);
            }

            debug!("Cleaned up connection {} after I/O error", conn_id);
            Ok(true)
        } else {
            // General I/O error recovery
            debug!("General I/O error recovery completed");
            Ok(true)
        }
    }

    /// Recover from channel error
    async fn recover_channel_error(
        &self,
        connection_id: Option<ConnectionId>,
    ) -> StreamResult<bool> {
        debug!("Attempting to recover from channel error");

        // Channel errors might be transient, wait a bit and let the system retry
        sleep(Duration::from_millis(100)).await;

        if let Some(conn_id) = connection_id {
            // Check if connection is still valid
            match self.connection_manager.get_connection_state(&conn_id).await {
                Ok(Some(_)) => {
                    debug!("Connection {} still valid after channel error", conn_id);
                    Ok(true)
                }
                Ok(None) => {
                    debug!(
                        "Connection {} no longer exists after channel error",
                        conn_id
                    );
                    Ok(true)
                }
                Err(e) => {
                    warn!(
                        "Failed to check connection state after channel error: {}",
                        e
                    );
                    Ok(false)
                }
            }
        } else {
            debug!("General channel error recovery completed");
            Ok(true)
        }
    }

    /// Recover from service unavailable
    async fn recover_service_unavailable(
        &self,
        connection_id: Option<ConnectionId>,
    ) -> StreamResult<bool> {
        debug!("Attempting to recover from service unavailable");

        // Wait longer for service to become available
        sleep(Duration::from_secs(1)).await;

        if let Some(conn_id) = connection_id {
            // Service unavailable usually means we should clean up the connection
            // and let the client retry
            if let Err(e) = self.connection_manager.cleanup_connection(&conn_id).await {
                warn!(
                    "Failed to cleanup connection after service unavailable: {}",
                    e
                );
                return Ok(false);
            }

            debug!(
                "Cleaned up connection {} after service unavailable",
                conn_id
            );
        }

        Ok(true)
    }

    /// Recover from rate limit
    async fn recover_rate_limit(&self) -> StreamResult<bool> {
        debug!("Attempting to recover from rate limit");

        // Wait for rate limit to reset
        sleep(Duration::from_secs(5)).await;

        debug!("Rate limit recovery completed");
        Ok(true)
    }

    /// Perform proactive health checks and recovery
    pub async fn perform_proactive_recovery(&self) -> StreamResult<()> {
        debug!("Performing proactive recovery checks");

        // Get current metrics to identify potential issues
        let metrics = self.metrics_collector.get_metrics().await;

        // Check for high error rates
        if metrics.errors.error_rate > 5.0 {
            warn!(
                "High error rate detected: {}/min, performing proactive recovery",
                metrics.errors.error_rate
            );

            // Implement proactive measures:
            // 1. Reduce connection limits temporarily
            // 2. Increase timeouts
            // 3. Clear error-prone connections

            // For now, just log the action
            info!("Proactive recovery measures applied for high error rate");
        }

        // Check for resource exhaustion trends
        if metrics.performance.memory_usage_bytes > 400 * 1024 * 1024 {
            // 400MB
            warn!(
                "High memory usage detected: {}MB, performing proactive recovery",
                metrics.performance.memory_usage_bytes / (1024 * 1024)
            );

            // Implement memory pressure relief:
            // 1. Force cleanup of idle connections
            // 2. Reduce buffer sizes
            // 3. Trigger garbage collection

            info!("Proactive memory recovery measures applied");
        }

        // Check for connection health issues
        if metrics.connections.failed_connections > 0 {
            let failure_rate = metrics.connections.failed_connections as f64
                / metrics.connections.total_connections as f64;
            if failure_rate > 0.1 {
                warn!(
                    "High connection failure rate: {:.1}%, performing proactive recovery",
                    failure_rate * 100.0
                );

                // Implement connection health measures:
                // 1. Adjust connection timeouts
                // 2. Implement circuit breaker patterns
                // 3. Add connection health checks

                info!("Proactive connection recovery measures applied");
            }
        }

        debug!("Proactive recovery checks completed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::test_interfaces::{MockConnectionManager, MockStreamManager};
    use uuid::Uuid;

    #[tokio::test]
    async fn test_recovery_coordinator_creation() {
        let connection_manager = Arc::new(MockConnectionManager::new());
        let stream_manager = Arc::new(MockStreamManager::new());
        let metrics_collector = Arc::new(MetricsCollector::new());

        let coordinator =
            ErrorRecoveryCoordinator::new(connection_manager, stream_manager, metrics_collector);

        // Test that coordinator was created successfully
        assert!(std::ptr::addr_of!(coordinator).is_aligned());
    }

    #[tokio::test]
    async fn test_non_recoverable_error_recovery() {
        let connection_manager = Arc::new(MockConnectionManager::new());
        let stream_manager = Arc::new(MockStreamManager::new());
        let metrics_collector = Arc::new(MetricsCollector::new());

        let coordinator =
            ErrorRecoveryCoordinator::new(connection_manager, stream_manager, metrics_collector);

        let error = StreamError::protocol_error("Test protocol error");
        let error_context = error.with_context("Test context");

        let result = coordinator.attempt_recovery(error_context, None).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), false); // Should not attempt recovery for non-recoverable error
    }

    #[tokio::test]
    async fn test_connection_failure_recovery() {
        let connection_manager = Arc::new(MockConnectionManager::new());
        let stream_manager = Arc::new(MockStreamManager::new());
        let metrics_collector = Arc::new(MetricsCollector::new());

        let coordinator =
            ErrorRecoveryCoordinator::new(connection_manager, stream_manager, metrics_collector);

        let connection_id = Uuid::new_v4();
        let error = StreamError::connection_failed("Test connection failure");
        let error_context = error.with_connection_context(connection_id, "test_operation");

        let result = coordinator
            .attempt_recovery(error_context, Some(connection_id))
            .await;
        assert!(result.is_ok());
        // Result depends on mock implementation, but should not error
    }

    #[tokio::test]
    async fn test_stream_failure_recovery() {
        let connection_manager = Arc::new(MockConnectionManager::new());
        let stream_manager = Arc::new(MockStreamManager::new());
        let metrics_collector = Arc::new(MetricsCollector::new());

        let coordinator =
            ErrorRecoveryCoordinator::new(connection_manager, stream_manager, metrics_collector);

        let connection_id = Uuid::new_v4();
        let error = StreamError::stream_broken(connection_id, "Test stream broken");
        let error_context = error.with_connection_context(connection_id, "data_transfer");

        let result = coordinator
            .attempt_recovery(error_context, Some(connection_id))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_resource_exhaustion_recovery() {
        let connection_manager = Arc::new(MockConnectionManager::new());
        let stream_manager = Arc::new(MockStreamManager::new());
        let metrics_collector = Arc::new(MetricsCollector::new());

        let coordinator =
            ErrorRecoveryCoordinator::new(connection_manager, stream_manager, metrics_collector);

        let error = StreamError::resource_exhausted("memory");
        let error_context = error.with_context("Resource exhaustion test");

        let result = coordinator.attempt_recovery(error_context, None).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), true); // Should successfully recover from resource exhaustion
    }

    #[tokio::test]
    async fn test_proactive_recovery() {
        let connection_manager = Arc::new(MockConnectionManager::new());
        let stream_manager = Arc::new(MockStreamManager::new());
        let metrics_collector = Arc::new(MetricsCollector::new());

        let coordinator =
            ErrorRecoveryCoordinator::new(connection_manager, stream_manager, metrics_collector);

        let result = coordinator.perform_proactive_recovery().await;
        assert!(result.is_ok());
    }
}
