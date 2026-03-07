//! Metrics collection and health monitoring for streaming operations

use crate::streaming::errors::{ErrorSeverity, StreamError};
use crate::streaming::{ConnectionId, ConnectionStatus};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Comprehensive metrics for streaming operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamingMetrics {
    /// Connection metrics
    pub connections: ConnectionMetrics,
    /// Data transfer metrics
    pub data_transfer: DataTransferMetrics,
    /// Error metrics
    pub errors: ErrorMetrics,
    /// Performance metrics
    pub performance: PerformanceMetrics,
    /// Health status
    pub health: HealthStatus,
    /// Timestamp of last update
    pub last_updated: SystemTime,
}

impl Default for StreamingMetrics {
    fn default() -> Self {
        Self {
            connections: ConnectionMetrics::default(),
            data_transfer: DataTransferMetrics::default(),
            errors: ErrorMetrics::default(),
            performance: PerformanceMetrics::default(),
            health: HealthStatus::default(),
            last_updated: SystemTime::now(),
        }
    }
}

/// Connection-related metrics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConnectionMetrics {
    /// Total connections created
    pub total_connections: u64,
    /// Currently active connections
    pub active_connections: u64,
    /// Failed connection attempts
    pub failed_connections: u64,
    /// Connections by status
    pub connections_by_status: HashMap<String, u64>,
    /// Average connection duration in milliseconds
    pub avg_connection_duration_ms: u64,
    /// Connection establishment rate (per second)
    pub connection_rate: f64,
    /// Peak concurrent connections
    pub peak_concurrent_connections: u64,
}

/// Data transfer metrics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DataTransferMetrics {
    /// Total bytes transferred
    pub total_bytes_transferred: u64,
    /// Bytes transferred in the last minute
    pub bytes_per_minute: u64,
    /// Current throughput in bytes per second
    pub current_throughput_bps: u64,
    /// Peak throughput in bytes per second
    pub peak_throughput_bps: u64,
    /// Number of data packets processed
    pub packets_processed: u64,
    /// Average packet size in bytes
    pub avg_packet_size: u64,
}

/// Error metrics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ErrorMetrics {
    /// Total errors encountered
    pub total_errors: u64,
    /// Errors by category
    pub errors_by_category: HashMap<String, u64>,
    /// Errors by severity
    pub errors_by_severity: HashMap<String, u64>,
    /// Error rate (per minute)
    pub error_rate: f64,
    /// Recovery success rate
    pub recovery_success_rate: f64,
    /// Recent errors (last 100)
    pub recent_errors: Vec<ErrorRecord>,
}

/// Performance metrics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PerformanceMetrics {
    /// Average connection establishment time in milliseconds
    pub avg_connection_time_ms: u64,
    /// Average data processing latency in milliseconds
    pub avg_processing_latency_ms: u64,
    /// Memory usage in bytes
    pub memory_usage_bytes: u64,
    /// CPU usage percentage
    pub cpu_usage_percent: f64,
    /// File descriptor usage
    pub file_descriptor_count: u64,
    /// Queue depths
    pub queue_depths: HashMap<String, u64>,
}

/// Overall health status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    /// Overall health score (0-100)
    pub health_score: u8,
    /// Health status
    pub status: HealthState,
    /// Health checks
    pub checks: HashMap<String, HealthCheck>,
    /// Last health check timestamp
    pub last_check: SystemTime,
    /// Uptime in seconds
    pub uptime_seconds: u64,
}

impl Default for HealthStatus {
    fn default() -> Self {
        Self {
            health_score: 100,
            status: HealthState::Healthy,
            checks: HashMap::new(),
            last_check: SystemTime::now(),
            uptime_seconds: 0,
        }
    }
}

/// Health state enumeration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum HealthState {
    Healthy,
    Degraded,
    Unhealthy,
    Critical,
}

/// Individual health check result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheck {
    pub name: String,
    pub status: HealthState,
    pub message: String,
    pub last_check: SystemTime,
    pub check_duration_ms: u64,
}

/// Error record for tracking recent errors
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorRecord {
    pub timestamp: SystemTime,
    pub error_type: String,
    pub category: String,
    pub severity: ErrorSeverity,
    pub message: String,
    pub connection_id: Option<ConnectionId>,
    pub recoverable: bool,
    pub recovery_attempted: bool,
    pub recovery_successful: Option<bool>,
}

/// Metrics collector and aggregator
#[derive(Debug)]
pub struct MetricsCollector {
    /// Current metrics
    metrics: Arc<RwLock<StreamingMetrics>>,
    /// Connection tracking for duration calculations
    connection_tracking: Arc<RwLock<HashMap<ConnectionId, ConnectionTrackingInfo>>>,
    /// Start time for uptime calculation
    start_time: Instant,
    /// Data transfer tracking for throughput calculation
    transfer_tracking: Arc<RwLock<TransferTrackingInfo>>,
}

/// Connection tracking information
#[derive(Debug)]
struct ConnectionTrackingInfo {
    start_time: Instant,
    status: ConnectionStatus,
    bytes_sent: u64,
    bytes_received: u64,
}

/// Transfer tracking for throughput calculation
#[derive(Debug)]
struct TransferTrackingInfo {
    last_minute_bytes: Vec<(Instant, u64)>,
    last_throughput_calculation: Instant,
}

impl Default for TransferTrackingInfo {
    fn default() -> Self {
        Self {
            last_minute_bytes: Vec::new(),
            last_throughput_calculation: Instant::now(),
        }
    }
}

impl MetricsCollector {
    /// Create a new metrics collector
    pub fn new() -> Self {
        Self {
            metrics: Arc::new(RwLock::new(StreamingMetrics::default())),
            connection_tracking: Arc::new(RwLock::new(HashMap::new())),
            start_time: Instant::now(),
            transfer_tracking: Arc::new(RwLock::new(TransferTrackingInfo::default())),
        }
    }

    /// Record a new connection
    #[allow(dead_code)]
    pub async fn record_connection_created(&self, connection_id: ConnectionId) {
        let mut metrics = self.metrics.write().await;
        let mut tracking = self.connection_tracking.write().await;

        metrics.connections.total_connections += 1;
        metrics.connections.active_connections += 1;

        // Update peak concurrent connections
        if metrics.connections.active_connections > metrics.connections.peak_concurrent_connections
        {
            metrics.connections.peak_concurrent_connections =
                metrics.connections.active_connections;
        }

        // Track connection for duration calculation
        tracking.insert(
            connection_id,
            ConnectionTrackingInfo {
                start_time: Instant::now(),
                status: ConnectionStatus::Establishing,
                bytes_sent: 0,
                bytes_received: 0,
            },
        );

        // Update connection rate (simple moving average)
        self.update_connection_rate().await;

        metrics.last_updated = SystemTime::now();

        debug!("Recorded new connection: {}", connection_id);
    }

    /// Record connection status change
    pub async fn record_connection_status_change(
        &self,
        connection_id: ConnectionId,
        old_status: ConnectionStatus,
        new_status: ConnectionStatus,
    ) {
        let mut metrics = self.metrics.write().await;
        let mut tracking = self.connection_tracking.write().await;

        // Update status counts
        let old_status_key = format!("{:?}", old_status).to_lowercase();
        let new_status_key = format!("{:?}", new_status).to_lowercase();

        if let Some(count) = metrics
            .connections
            .connections_by_status
            .get_mut(&old_status_key)
        {
            if *count > 0 {
                *count -= 1;
            }
        }
        *metrics
            .connections
            .connections_by_status
            .entry(new_status_key)
            .or_insert(0) += 1;

        // Update tracking info
        if let Some(info) = tracking.get_mut(&connection_id) {
            info.status = new_status.clone();
        }

        // Handle active connection count changes
        match (&old_status, &new_status) {
            (ConnectionStatus::Establishing, ConnectionStatus::Active) => {
                // Connection became active - already counted in active_connections
            }
            (ConnectionStatus::Active, ConnectionStatus::Closing)
            | (ConnectionStatus::Active, ConnectionStatus::Closed)
            | (ConnectionStatus::Active, ConnectionStatus::Error(_)) => {
                if metrics.connections.active_connections > 0 {
                    metrics.connections.active_connections -= 1;
                }
            }
            (ConnectionStatus::Establishing, ConnectionStatus::Error(_))
            | (ConnectionStatus::Establishing, ConnectionStatus::Closed) => {
                metrics.connections.failed_connections += 1;
                if metrics.connections.active_connections > 0 {
                    metrics.connections.active_connections -= 1;
                }
            }
            _ => {}
        }

        metrics.last_updated = SystemTime::now();

        debug!(
            "Connection {} status changed: {:?} -> {:?}",
            connection_id, old_status, new_status
        );
    }

    /// Record connection cleanup
    pub async fn record_connection_cleanup(&self, connection_id: ConnectionId) {
        let mut metrics = self.metrics.write().await;
        let mut tracking = self.connection_tracking.write().await;

        if let Some(info) = tracking.remove(&connection_id) {
            let duration = info.start_time.elapsed();

            // Decrease active connections first
            if metrics.connections.active_connections > 0 {
                metrics.connections.active_connections -= 1;
            }

            // Update average connection duration
            let current_avg = metrics.connections.avg_connection_duration_ms;
            let completed_connections =
                metrics.connections.total_connections - metrics.connections.active_connections;

            if completed_connections > 0 {
                let total_duration = current_avg * completed_connections;
                metrics.connections.avg_connection_duration_ms =
                    (total_duration + duration.as_millis() as u64) / (completed_connections + 1);
            } else {
                // First completed connection
                metrics.connections.avg_connection_duration_ms = duration.as_millis() as u64;
            }
        }

        metrics.last_updated = SystemTime::now();

        debug!("Recorded connection cleanup: {}", connection_id);
    }

    /// Record data transfer
    pub async fn record_data_transfer(
        &self,
        connection_id: ConnectionId,
        bytes: u64,
        direction: &str,
    ) {
        let mut metrics = self.metrics.write().await;
        let mut tracking = self.connection_tracking.write().await;
        let mut transfer_tracking = self.transfer_tracking.write().await;

        // Update total bytes transferred
        metrics.data_transfer.total_bytes_transferred += bytes;
        metrics.data_transfer.packets_processed += 1;

        // Update average packet size
        if metrics.data_transfer.packets_processed > 0 {
            metrics.data_transfer.avg_packet_size = metrics.data_transfer.total_bytes_transferred
                / metrics.data_transfer.packets_processed;
        }

        // Update connection-specific tracking
        if let Some(info) = tracking.get_mut(&connection_id) {
            match direction {
                "sent" => info.bytes_sent += bytes,
                "received" => info.bytes_received += bytes,
                _ => {}
            }
        }

        // Update throughput tracking
        let now = Instant::now();
        transfer_tracking.last_minute_bytes.push((now, bytes));

        // Remove entries older than 1 minute
        transfer_tracking
            .last_minute_bytes
            .retain(|(timestamp, _)| now.duration_since(*timestamp) <= Duration::from_secs(60));

        // Calculate current throughput if enough time has passed
        if now.duration_since(transfer_tracking.last_throughput_calculation)
            >= Duration::from_secs(1)
        {
            let bytes_last_minute: u64 = transfer_tracking
                .last_minute_bytes
                .iter()
                .map(|(_, b)| b)
                .sum();
            metrics.data_transfer.bytes_per_minute = bytes_last_minute;
            metrics.data_transfer.current_throughput_bps = bytes_last_minute / 60;

            if metrics.data_transfer.current_throughput_bps
                > metrics.data_transfer.peak_throughput_bps
            {
                metrics.data_transfer.peak_throughput_bps =
                    metrics.data_transfer.current_throughput_bps;
            }

            transfer_tracking.last_throughput_calculation = now;
        }

        metrics.last_updated = SystemTime::now();

        debug!(
            "Recorded data transfer: {} bytes {} for connection {}",
            bytes, direction, connection_id
        );
    }

    /// Record an error
    pub async fn record_error(&self, error: &StreamError, connection_id: Option<ConnectionId>) {
        let mut metrics = self.metrics.write().await;

        metrics.errors.total_errors += 1;

        // Update error category counts
        let category = error.category();
        *metrics
            .errors
            .errors_by_category
            .entry(category.to_string())
            .or_insert(0) += 1;

        // Update error severity counts
        let severity = error.severity();
        let severity_key = format!("{:?}", severity).to_lowercase();
        *metrics
            .errors
            .errors_by_severity
            .entry(severity_key)
            .or_insert(0) += 1;

        // Create error record
        let error_record = ErrorRecord {
            timestamp: SystemTime::now(),
            error_type: format!("{:?}", error),
            category: category.to_string(),
            severity,
            message: error.to_string(),
            connection_id,
            recoverable: error.is_recoverable(),
            recovery_attempted: false,
            recovery_successful: None,
        };

        // Add to recent errors (keep only last 100)
        metrics.errors.recent_errors.push(error_record);
        if metrics.errors.recent_errors.len() > 100 {
            metrics.errors.recent_errors.remove(0);
        }

        // Update error rate (simple calculation based on last minute)
        let one_minute_ago = SystemTime::now() - Duration::from_secs(60);
        let recent_error_count = metrics
            .errors
            .recent_errors
            .iter()
            .filter(|e| e.timestamp > one_minute_ago)
            .count();
        metrics.errors.error_rate = recent_error_count as f64;

        metrics.last_updated = SystemTime::now();

        warn!(
            "Recorded error: {} (category: {}, severity: {:?})",
            error, category, severity
        );
    }

    /// Record error recovery attempt
    pub async fn record_error_recovery(&self, connection_id: ConnectionId, successful: bool) {
        let mut metrics = self.metrics.write().await;

        // Find the most recent error for this connection and update it
        if let Some(error_record) = metrics
            .errors
            .recent_errors
            .iter_mut()
            .rev()
            .find(|e| e.connection_id == Some(connection_id) && !e.recovery_attempted)
        {
            error_record.recovery_attempted = true;
            error_record.recovery_successful = Some(successful);
        }

        // Update recovery success rate
        let recovery_attempts = metrics
            .errors
            .recent_errors
            .iter()
            .filter(|e| e.recovery_attempted)
            .count();

        if recovery_attempts > 0 {
            let successful_recoveries = metrics
                .errors
                .recent_errors
                .iter()
                .filter(|e| e.recovery_successful == Some(true))
                .count();

            metrics.errors.recovery_success_rate =
                (successful_recoveries as f64 / recovery_attempts as f64) * 100.0;
        }

        metrics.last_updated = SystemTime::now();

        info!(
            "Recorded error recovery for connection {}: {}",
            connection_id,
            if successful { "successful" } else { "failed" }
        );
    }

    /// Update performance metrics
    pub async fn update_performance_metrics(
        &self,
        memory_usage: u64,
        cpu_usage: f64,
        file_descriptors: u64,
        queue_depths: HashMap<String, u64>,
    ) {
        let mut metrics = self.metrics.write().await;

        metrics.performance.memory_usage_bytes = memory_usage;
        metrics.performance.cpu_usage_percent = cpu_usage;
        metrics.performance.file_descriptor_count = file_descriptors;
        metrics.performance.queue_depths = queue_depths;

        metrics.last_updated = SystemTime::now();

        debug!(
            "Updated performance metrics: memory={}MB, cpu={}%, fds={}",
            memory_usage / (1024 * 1024),
            cpu_usage,
            file_descriptors
        );
    }

    /// Perform health check and update health status
    pub async fn perform_health_check(&self) -> HealthStatus {
        let mut metrics = self.metrics.write().await;
        let now = SystemTime::now();

        let mut health_checks = HashMap::new();
        let mut health_score = 100u8;

        // Check connection health
        let connection_check = self.check_connection_health(&metrics).await;
        if connection_check.status != HealthState::Healthy {
            health_score = health_score.saturating_sub(20);
        }
        health_checks.insert("connections".to_string(), connection_check);

        // Check error rate health
        let error_check = self.check_error_health(&metrics).await;
        if error_check.status != HealthState::Healthy {
            health_score = health_score.saturating_sub(25);
        }
        health_checks.insert("errors".to_string(), error_check);

        // Check performance health
        let performance_check = self.check_performance_health(&metrics).await;
        if performance_check.status != HealthState::Healthy {
            health_score = health_score.saturating_sub(15);
        }
        health_checks.insert("performance".to_string(), performance_check);

        // Check resource health
        let resource_check = self.check_resource_health(&metrics).await;
        if resource_check.status != HealthState::Healthy {
            health_score = health_score.saturating_sub(30);
        }
        health_checks.insert("resources".to_string(), resource_check);

        // Determine overall health state
        let overall_status = match health_score {
            90..=100 => HealthState::Healthy,
            70..=89 => HealthState::Degraded,
            40..=69 => HealthState::Unhealthy,
            _ => HealthState::Critical,
        };

        let health_status = HealthStatus {
            health_score,
            status: overall_status,
            checks: health_checks,
            last_check: now,
            uptime_seconds: self.start_time.elapsed().as_secs(),
        };

        metrics.health = health_status.clone();
        metrics.last_updated = now;

        info!(
            "Health check completed: score={}, status={:?}",
            health_score, health_status.status
        );

        health_status
    }

    /// Get current metrics snapshot
    pub async fn get_metrics(&self) -> StreamingMetrics {
        self.metrics.read().await.clone()
    }

    /// Reset metrics (useful for testing)
    pub async fn reset_metrics(&self) {
        let mut metrics = self.metrics.write().await;
        *metrics = StreamingMetrics::default();

        let mut tracking = self.connection_tracking.write().await;
        tracking.clear();

        let mut transfer_tracking = self.transfer_tracking.write().await;
        *transfer_tracking = TransferTrackingInfo::default();

        info!("Metrics reset");
    }

    // Private helper methods

    #[allow(dead_code)]
    async fn update_connection_rate(&self) {
        // Simple implementation - in production, this would use a more sophisticated algorithm
        let uptime_seconds = self.start_time.elapsed().as_secs();

        if uptime_seconds > 0 {
            let total_connections = {
                let metrics = self.metrics.read().await;
                metrics.connections.total_connections
            };

            let mut metrics = self.metrics.write().await;
            metrics.connections.connection_rate = total_connections as f64 / uptime_seconds as f64;
        }
    }

    async fn check_connection_health(&self, metrics: &StreamingMetrics) -> HealthCheck {
        let start = Instant::now();

        let (status, message) = if metrics.connections.failed_connections > 0 {
            let failure_rate = metrics.connections.failed_connections as f64
                / metrics.connections.total_connections as f64;
            if failure_rate > 0.1 {
                (
                    HealthState::Unhealthy,
                    format!("High connection failure rate: {:.1}%", failure_rate * 100.0),
                )
            } else if failure_rate > 0.05 {
                (
                    HealthState::Degraded,
                    format!(
                        "Elevated connection failure rate: {:.1}%",
                        failure_rate * 100.0
                    ),
                )
            } else {
                (
                    HealthState::Healthy,
                    "Connection health is good".to_string(),
                )
            }
        } else {
            (HealthState::Healthy, "No connection failures".to_string())
        };

        HealthCheck {
            name: "connections".to_string(),
            status,
            message,
            last_check: SystemTime::now(),
            check_duration_ms: start.elapsed().as_millis() as u64,
        }
    }

    async fn check_error_health(&self, metrics: &StreamingMetrics) -> HealthCheck {
        let start = Instant::now();

        let (status, message) = if metrics.errors.error_rate > 10.0 {
            (
                HealthState::Critical,
                format!("Critical error rate: {:.1}/min", metrics.errors.error_rate),
            )
        } else if metrics.errors.error_rate > 5.0 {
            (
                HealthState::Unhealthy,
                format!("High error rate: {:.1}/min", metrics.errors.error_rate),
            )
        } else if metrics.errors.error_rate > 1.0 {
            (
                HealthState::Degraded,
                format!("Elevated error rate: {:.1}/min", metrics.errors.error_rate),
            )
        } else {
            (HealthState::Healthy, "Error rate is acceptable".to_string())
        };

        HealthCheck {
            name: "errors".to_string(),
            status,
            message,
            last_check: SystemTime::now(),
            check_duration_ms: start.elapsed().as_millis() as u64,
        }
    }

    async fn check_performance_health(&self, metrics: &StreamingMetrics) -> HealthCheck {
        let start = Instant::now();

        let (status, message) = if metrics.performance.cpu_usage_percent > 90.0 {
            (
                HealthState::Critical,
                format!(
                    "Critical CPU usage: {:.1}%",
                    metrics.performance.cpu_usage_percent
                ),
            )
        } else if metrics.performance.cpu_usage_percent > 80.0 {
            (
                HealthState::Unhealthy,
                format!(
                    "High CPU usage: {:.1}%",
                    metrics.performance.cpu_usage_percent
                ),
            )
        } else if metrics.performance.cpu_usage_percent > 70.0 {
            (
                HealthState::Degraded,
                format!(
                    "Elevated CPU usage: {:.1}%",
                    metrics.performance.cpu_usage_percent
                ),
            )
        } else {
            (HealthState::Healthy, "Performance is good".to_string())
        };

        HealthCheck {
            name: "performance".to_string(),
            status,
            message,
            last_check: SystemTime::now(),
            check_duration_ms: start.elapsed().as_millis() as u64,
        }
    }

    async fn check_resource_health(&self, metrics: &StreamingMetrics) -> HealthCheck {
        let start = Instant::now();

        let memory_mb = metrics.performance.memory_usage_bytes / (1024 * 1024);
        let (status, message) = if memory_mb > 1024 {
            (
                HealthState::Critical,
                format!("Critical memory usage: {}MB", memory_mb),
            )
        } else if memory_mb > 512 {
            (
                HealthState::Unhealthy,
                format!("High memory usage: {}MB", memory_mb),
            )
        } else if memory_mb > 256 {
            (
                HealthState::Degraded,
                format!("Elevated memory usage: {}MB", memory_mb),
            )
        } else {
            (
                HealthState::Healthy,
                "Resource usage is acceptable".to_string(),
            )
        };

        HealthCheck {
            name: "resources".to_string(),
            status,
            message,
            last_check: SystemTime::now(),
            check_duration_ms: start.elapsed().as_millis() as u64,
        }
    }
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[tokio::test]
    async fn test_metrics_collector_creation() {
        let collector = MetricsCollector::new();
        let metrics = collector.get_metrics().await;

        assert_eq!(metrics.connections.total_connections, 0);
        assert_eq!(metrics.connections.active_connections, 0);
        assert_eq!(metrics.data_transfer.total_bytes_transferred, 0);
        assert_eq!(metrics.errors.total_errors, 0);
    }

    #[tokio::test]
    async fn test_connection_tracking() {
        let collector = MetricsCollector::new();
        let connection_id = Uuid::new_v4();

        // Record connection creation
        collector.record_connection_created(connection_id).await;

        let metrics = collector.get_metrics().await;
        assert_eq!(metrics.connections.total_connections, 1);
        assert_eq!(metrics.connections.active_connections, 1);

        // Record status change
        collector
            .record_connection_status_change(
                connection_id,
                ConnectionStatus::Establishing,
                ConnectionStatus::Active,
            )
            .await;

        let metrics = collector.get_metrics().await;
        assert_eq!(metrics.connections.active_connections, 1);

        // Add a small delay to ensure measurable duration
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Record cleanup
        collector.record_connection_cleanup(connection_id).await;

        let metrics = collector.get_metrics().await;
        assert!(metrics.connections.avg_connection_duration_ms > 0);
    }

    #[tokio::test]
    async fn test_data_transfer_tracking() {
        let collector = MetricsCollector::new();
        let connection_id = Uuid::new_v4();

        collector
            .record_data_transfer(connection_id, 1024, "sent")
            .await;
        collector
            .record_data_transfer(connection_id, 512, "received")
            .await;

        let metrics = collector.get_metrics().await;
        assert_eq!(metrics.data_transfer.total_bytes_transferred, 1536);
        assert_eq!(metrics.data_transfer.packets_processed, 2);
        assert_eq!(metrics.data_transfer.avg_packet_size, 768);
    }

    #[tokio::test]
    async fn test_error_tracking() {
        let collector = MetricsCollector::new();
        let connection_id = Uuid::new_v4();
        let error = StreamError::connection_failed("Test error");

        collector.record_error(&error, Some(connection_id)).await;

        let metrics = collector.get_metrics().await;
        assert_eq!(metrics.errors.total_errors, 1);
        assert_eq!(
            metrics.errors.errors_by_category.get("connection"),
            Some(&1)
        );
        assert_eq!(metrics.errors.recent_errors.len(), 1);

        // Test recovery tracking
        collector.record_error_recovery(connection_id, true).await;

        let metrics = collector.get_metrics().await;
        assert!(metrics.errors.recovery_success_rate > 0.0);
    }

    #[tokio::test]
    async fn test_health_check() {
        let collector = MetricsCollector::new();

        let health = collector.perform_health_check().await;
        assert_eq!(health.status, HealthState::Healthy);
        assert_eq!(health.health_score, 100);
        assert!(health.checks.contains_key("connections"));
        assert!(health.checks.contains_key("errors"));
        assert!(health.checks.contains_key("performance"));
        assert!(health.checks.contains_key("resources"));
    }

    #[tokio::test]
    async fn test_metrics_reset() {
        let collector = MetricsCollector::new();
        let connection_id = Uuid::new_v4();

        // Add some data
        collector.record_connection_created(connection_id).await;
        collector
            .record_data_transfer(connection_id, 1024, "sent")
            .await;

        let metrics_before = collector.get_metrics().await;
        assert_eq!(metrics_before.connections.total_connections, 1);
        assert_eq!(metrics_before.data_transfer.total_bytes_transferred, 1024);

        // Reset metrics
        collector.reset_metrics().await;

        let metrics_after = collector.get_metrics().await;
        assert_eq!(metrics_after.connections.total_connections, 0);
        assert_eq!(metrics_after.data_transfer.total_bytes_transferred, 0);
    }
}
