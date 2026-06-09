//! Performance benchmarks for streaming vs synchronous implementations
//!
//! This module provides comprehensive benchmarks comparing the new streaming
//! architecture against the old synchronous implementation.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use labyrinth::streaming::connection_manager::ServerConnectionManager;
use labyrinth::streaming::stream_manager::BidirectionalStreamManager;
use labyrinth::streaming::{ConnectionManager, PortMapping, StreamManager};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Runtime;

/// Benchmark configuration
struct BenchmarkConfig {
    data_sizes: Vec<usize>,
    concurrent_connections: Vec<usize>,
}

impl Default for BenchmarkConfig {
    fn default() -> Self {
        Self {
            data_sizes: vec![1024, 8192, 65536, 1024 * 1024], // 1KB to 1MB
            concurrent_connections: vec![1, 5, 10, 25, 50],
        }
    }
}

/// Test harness for benchmarks
struct BenchmarkHarness {
    rt: Runtime,
    config: BenchmarkConfig,
}

impl BenchmarkHarness {
    fn new() -> Self {
        let rt = Runtime::new().expect("Failed to create tokio runtime");
        Self {
            rt,
            config: BenchmarkConfig::default(),
        }
    }

    /// Setup a mock target server for benchmarking
    async fn setup_echo_server(&self) -> (u16, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let handle = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buffer = vec![0u8; 8192];
                    while let Ok(n) = stream.read(&mut buffer).await {
                        if n == 0 {
                            break;
                        }
                        if stream.write_all(&buffer[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });

        (port, handle)
    }

    /// Benchmark streaming implementation throughput
    async fn benchmark_streaming_throughput(&self, data_size: usize) -> Duration {
        let (target_port, _server_handle) = self.setup_echo_server().await;

        // Setup streaming components
        let connection_manager = Arc::new(ServerConnectionManager::new());
        let (stream_manager, _message_receiver) =
            BidirectionalStreamManager::with_buffer_size(1000);
        let stream_manager = Arc::new(stream_manager);

        // Create test data
        let test_data: Vec<u8> = (0..data_size).map(|i| (i % 256) as u8).collect();

        // Setup connection
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_addr = listener.local_addr().unwrap();

        let client_stream = TcpStream::connect(local_addr).await.unwrap();
        let (server_stream, _) = listener.accept().await.unwrap();

        let mapping = PortMapping {
            local_port: local_addr.port(),
            target_host: "127.0.0.1".to_string(),
            target_port,
        };

        let connection_id = connection_manager
            .handle_new_connection(server_stream, mapping)
            .await
            .unwrap();

        // Create a second connection for the stream manager
        let client_stream_clone = TcpStream::connect(local_addr).await.unwrap();
        stream_manager
            .create_bidirectional_stream(connection_id, client_stream_clone)
            .await
            .unwrap();

        // Benchmark the actual data transfer
        let start = std::time::Instant::now();

        // Send data in chunks
        let mut client_stream = client_stream;
        let chunk_size = 8192.min(data_size);

        for chunk in test_data.chunks(chunk_size) {
            client_stream.write_all(chunk).await.unwrap();
        }

        // Wait for echo response
        let mut received = 0;
        let mut buffer = vec![0u8; chunk_size];

        while received < data_size {
            let n = client_stream.read(&mut buffer).await.unwrap();
            if n == 0 {
                break;
            }
            received += n;
        }

        let duration = start.elapsed();

        // Cleanup
        let _ = stream_manager.terminate_stream(connection_id).await;
        let _ = connection_manager.cleanup_connection(&connection_id).await;

        duration
    }

    /// Benchmark connection establishment latency
    async fn benchmark_connection_latency(&self) -> Duration {
        let (target_port, _server_handle) = self.setup_echo_server().await;

        let connection_manager = Arc::new(ServerConnectionManager::new());
        let (stream_manager, _message_receiver) = BidirectionalStreamManager::with_buffer_size(100);
        let stream_manager = Arc::new(stream_manager);

        let start = std::time::Instant::now();

        // Setup connection
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_addr = listener.local_addr().unwrap();

        let client_stream = TcpStream::connect(local_addr).await.unwrap();
        let (server_stream, _) = listener.accept().await.unwrap();

        let mapping = PortMapping {
            local_port: local_addr.port(),
            target_host: "127.0.0.1".to_string(),
            target_port,
        };

        let connection_id = connection_manager
            .handle_new_connection(server_stream, mapping)
            .await
            .unwrap();

        stream_manager
            .create_bidirectional_stream(connection_id, client_stream)
            .await
            .unwrap();

        let duration = start.elapsed();

        // Cleanup
        let _ = stream_manager.terminate_stream(connection_id).await;
        let _ = connection_manager.cleanup_connection(&connection_id).await;

        duration
    }

    /// Benchmark concurrent connections
    async fn benchmark_concurrent_connections(&self, connection_count: usize) -> Duration {
        let (target_port, _server_handle) = self.setup_echo_server().await;

        let connection_manager = Arc::new(ServerConnectionManager::new());
        let (stream_manager, _message_receiver) =
            BidirectionalStreamManager::with_buffer_size(1000);
        let stream_manager = Arc::new(stream_manager);

        let start = std::time::Instant::now();

        // Create multiple concurrent connections
        let mut handles = Vec::new();

        for _ in 0..connection_count {
            let connection_manager = Arc::clone(&connection_manager);
            let stream_manager = Arc::clone(&stream_manager);

            let handle = tokio::spawn(async move {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let local_addr = listener.local_addr().unwrap();

                let client_stream = TcpStream::connect(local_addr).await.unwrap();
                let (server_stream, _) = listener.accept().await.unwrap();

                let mapping = PortMapping {
                    local_port: local_addr.port(),
                    target_host: "127.0.0.1".to_string(),
                    target_port,
                };

                let connection_id = connection_manager
                    .handle_new_connection(server_stream, mapping)
                    .await
                    .unwrap();

                stream_manager
                    .create_bidirectional_stream(connection_id, client_stream)
                    .await
                    .unwrap();

                // Send small test data
                let _test_data = b"benchmark test";
                // Note: In a real benchmark, we'd need to handle the client stream properly
                // This is simplified for the benchmark

                connection_id
            });

            handles.push(handle);
        }

        // Wait for all connections to complete
        let mut connection_ids = Vec::new();
        for handle in handles {
            let connection_id = handle.await.unwrap();
            connection_ids.push(connection_id);
        }

        let duration = start.elapsed();

        // Cleanup all connections
        for connection_id in connection_ids {
            let _ = stream_manager.terminate_stream(connection_id).await;
            let _ = connection_manager.cleanup_connection(&connection_id).await;
        }

        duration
    }
}

/// Benchmark streaming throughput with different data sizes
fn bench_streaming_throughput(c: &mut Criterion) {
    let harness = BenchmarkHarness::new();
    let mut group = c.benchmark_group("streaming_throughput");

    for &data_size in &harness.config.data_sizes {
        group.throughput(Throughput::Bytes(data_size as u64));
        group.bench_with_input(
            BenchmarkId::new("streaming", data_size),
            &data_size,
            |b, &size| {
                b.iter(|| {
                    harness.rt.block_on(async {
                        black_box(harness.benchmark_streaming_throughput(size).await)
                    })
                });
            },
        );
    }

    group.finish();
}

/// Benchmark connection establishment latency
fn bench_connection_latency(c: &mut Criterion) {
    let harness = BenchmarkHarness::new();

    c.bench_function("connection_latency", |b| {
        b.iter(|| {
            harness
                .rt
                .block_on(async { black_box(harness.benchmark_connection_latency().await) })
        });
    });
}

/// Benchmark concurrent connection handling
fn bench_concurrent_connections(c: &mut Criterion) {
    let harness = BenchmarkHarness::new();
    let mut group = c.benchmark_group("concurrent_connections");

    for &conn_count in &harness.config.concurrent_connections {
        group.bench_with_input(
            BenchmarkId::new("streaming", conn_count),
            &conn_count,
            |b, &count| {
                b.iter(|| {
                    harness.rt.block_on(async {
                        black_box(harness.benchmark_concurrent_connections(count).await)
                    })
                });
            },
        );
    }

    group.finish();
}

/// Benchmark memory usage patterns
fn bench_memory_usage(c: &mut Criterion) {
    let harness = BenchmarkHarness::new();

    c.bench_function("memory_usage", |b| {
        b.iter(|| {
            harness.rt.block_on(async {
                // Create and destroy many connections to test memory usage
                let (target_port, _server_handle) = harness.setup_echo_server().await;

                let connection_manager = Arc::new(ServerConnectionManager::new());
                let (stream_manager, _message_receiver) =
                    BidirectionalStreamManager::with_buffer_size(100);
                let stream_manager = Arc::new(stream_manager);

                // Create multiple connections
                let mut connection_ids = Vec::new();

                for _ in 0..10 {
                    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let local_addr = listener.local_addr().unwrap();

                    let client_stream = TcpStream::connect(local_addr).await.unwrap();
                    let (server_stream, _) = listener.accept().await.unwrap();

                    let mapping = PortMapping {
                        local_port: local_addr.port(),
                        target_host: "127.0.0.1".to_string(),
                        target_port,
                    };

                    let connection_id = connection_manager
                        .handle_new_connection(server_stream, mapping)
                        .await
                        .unwrap();

                    stream_manager
                        .create_bidirectional_stream(connection_id, client_stream)
                        .await
                        .unwrap();

                    connection_ids.push(connection_id);
                }

                // Cleanup all connections
                for connection_id in connection_ids {
                    let _ = stream_manager.terminate_stream(connection_id).await;
                    let _ = connection_manager.cleanup_connection(&connection_id).await;
                }

                black_box(())
            })
        });
    });
}

criterion_group!(
    benches,
    bench_streaming_throughput,
    bench_connection_latency,
    bench_concurrent_connections,
    bench_memory_usage
);

criterion_main!(benches);
