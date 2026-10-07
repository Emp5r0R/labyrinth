//! End-to-end networking tests: a real agent listener on an ephemeral port,
//! real TCP/TLS, QUIC and SOCKS5 hops, and the production client connector.
//! Everything binds to 127.0.0.1 and needs no privileges.

mod common;

use base64::{engine::general_purpose, Engine as _};
use common::*;
use labyrinth::agent::connection::{ConnectionManager, ControlConnectionConfig};
use labyrinth::framing::{FrameCodec, MAX_HANDSHAKE_FRAME};
use labyrinth::protocol::Message;
use labyrinth::transport::TransportMode;
use labyrinth::LabyrinthError;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

const AUTH: &str = "e2e-shared-secret";

async fn registered_round_trip(transport: TransportMode) {
    let server = TestServer::start(transport, Some(AUTH)).await;
    let mut agent = register(&server.client_config(), agent_info("alpha", Some(AUTH))).await;

    eventually("agent registered", || async {
        server.agent_count().await == 1
    })
    .await;
    let agent_id = server.only_agent_id().await;
    {
        let agents = server.server.agents().read().await;
        let entry = &agents[&agent_id];
        assert_eq!(entry.info.name, "alpha");
        assert_eq!(entry.transport_label, transport.label());
        assert_eq!(
            entry.quic_connection.is_some(),
            transport == TransportMode::Quic
        );
        assert_eq!(
            agent.quic_connection.is_some(),
            transport == TransportMode::Quic
        );
    }

    // Server -> agent.
    server.send_to_agent(&agent_id, Message::Ping).await;
    assert!(matches!(agent.recv().await, Message::Ping));

    // Agent -> server reply reaches the UI task waiting on it.
    let (tx, rx) = oneshot::channel();
    let pending = server.server.agents().read().await[&agent_id]
        .command_response
        .clone();
    *pending.lock().await = Some(tx);
    agent
        .send(&Message::CommandResponse {
            output: "uid=0(root)".into(),
            error: None,
        })
        .await;
    match within(rx).await.unwrap() {
        Message::CommandResponse { output, error } => {
            assert_eq!(output, "uid=0(root)");
            assert!(error.is_none());
        }
        other => panic!("unexpected {other:?}"),
    }

    // Disconnect removes the agent.
    drop(agent);
    eventually("agent removed on disconnect", || async {
        server.agent_count().await == 0
    })
    .await;
}

#[tokio::test]
async fn tcp_agent_registers_exchanges_frames_and_is_removed_on_disconnect() {
    registered_round_trip(TransportMode::Tcp).await;
}

#[tokio::test]
async fn quic_agent_registers_exchanges_frames_and_is_removed_on_disconnect() {
    registered_round_trip(TransportMode::Quic).await;
}

#[tokio::test]
async fn in_memory_execution_responses_reach_waiting_ui() {
    // Regression: these responses fell through to "unhandled" on the server,
    // so BOF / reflective / ELF requests always timed out.
    let server = TestServer::start(TransportMode::Tcp, None).await;
    let mut agent = register(&server.client_config(), agent_info("beta", None)).await;
    eventually("agent registered", || async {
        server.agent_count().await == 1
    })
    .await;
    let agent_id = server.only_agent_id().await;
    let pending = server.server.agents().read().await[&agent_id]
        .command_response
        .clone();

    for response in [
        Message::BofExecutionResponse {
            output: "bof".into(),
            error: None,
        },
        Message::ReflectiveLoadResponse {
            output: "pe".into(),
            error: None,
        },
        Message::LinuxElfExecutionResponse {
            output: "elf".into(),
            error: Some("exit 1".into()),
        },
    ] {
        let (tx, rx) = oneshot::channel();
        *pending.lock().await = Some(tx);
        agent.send(&response).await;
        let delivered = within(rx).await.expect("response was not delivered");
        assert_eq!(
            std::mem::discriminant(&delivered),
            std::mem::discriminant(&response)
        );
    }
}

#[tokio::test]
async fn server_messages_arrive_in_order_and_large_frames_survive() {
    let server = TestServer::start(TransportMode::Tcp, None).await;
    let mut agent = register(&server.client_config(), agent_info("gamma", None)).await;
    eventually("agent registered", || async {
        server.agent_count().await == 1
    })
    .await;
    let agent_id = server.only_agent_id().await;

    for i in 0..200 {
        server
            .send_to_agent(
                &agent_id,
                Message::CommandRequest {
                    command: format!("cmd-{i}"),
                },
            )
            .await;
    }
    for i in 0..200 {
        match agent.recv().await {
            Message::CommandRequest { command } => assert_eq!(command, format!("cmd-{i}")),
            other => panic!("unexpected {other:?}"),
        }
    }

    // Larger than the pre-auth limit, well under the control limit.
    let content = general_purpose::STANDARD.encode(vec![0x5a; MAX_HANDSHAKE_FRAME * 4]);
    server
        .send_to_agent(
            &agent_id,
            Message::FileUpload {
                remote_path: "/tmp/payload".into(),
                content_b64: content.clone(),
            },
        )
        .await;
    match agent.recv().await {
        Message::FileUpload { content_b64, .. } => assert_eq!(content_b64, content),
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn wrong_or_missing_auth_key_is_rejected_on_both_transports() {
    for transport in [TransportMode::Tcp, TransportMode::Quic] {
        let server = TestServer::start(transport, Some(AUTH)).await;
        for key in [Some("wrong"), None, Some("")] {
            let result = try_register(&server.client_config(), agent_info("mallory", key)).await;
            assert!(
                result.is_err(),
                "{transport}: key {key:?} must not receive an ack"
            );
        }
        assert_eq!(server.agent_count().await, 0, "{transport}");
    }
}

#[tokio::test]
async fn server_without_auth_accepts_agents_without_key() {
    let server = TestServer::start(TransportMode::Tcp, None).await;
    let _agent = register(&server.client_config(), agent_info("open", None)).await;
    eventually("agent registered", || async {
        server.agent_count().await == 1
    })
    .await;
}

#[tokio::test]
async fn non_registration_first_frame_is_rejected() {
    let server = TestServer::start(TransportMode::Tcp, None).await;
    let connection = connect(&server.client_config()).await.unwrap();
    let mut stream = tokio::io::BufReader::new(connection.stream);
    FrameCodec::HANDSHAKE
        .write(&mut stream, &Message::Ping)
        .await
        .unwrap();
    let reply = within(FrameCodec::CONTROL.read::<_, Message>(&mut stream)).await;
    assert!(!matches!(reply, Ok(Some(Message::AgentAck))));
    assert_eq!(server.agent_count().await, 0);
}

#[tokio::test]
async fn pinned_trust_accepts_fingerprint_formats_and_base64_cert() {
    let server = TestServer::start(TransportMode::Tcp, None).await;

    let mut by_upper = server.client_config();
    by_upper.accept_fingerprint = Some(server.identity.fingerprint.to_uppercase());
    connect(&by_upper).await.unwrap();

    let mut by_colons = server.client_config();
    by_colons.accept_fingerprint = Some(
        server
            .identity
            .fingerprint
            .as_bytes()
            .chunks(2)
            .map(|pair| std::str::from_utf8(pair).unwrap())
            .collect::<Vec<_>>()
            .join(":"),
    );
    connect(&by_colons).await.unwrap();

    let mut by_cert = server.client_config();
    by_cert.accept_fingerprint = None;
    by_cert.server_cert_b64 = Some(general_purpose::STANDARD.encode(&server.identity.cert_pem));
    connect(&by_cert).await.unwrap();
}

#[tokio::test]
async fn wrong_pin_fails_handshake_on_both_transports() {
    let impostor = Identity::generate();
    for transport in [TransportMode::Tcp, TransportMode::Quic] {
        let server = TestServer::start(transport, None).await;
        let mut config = server.client_config();
        config.accept_fingerprint = Some(impostor.fingerprint.clone());
        assert!(
            connect(&config).await.is_err(),
            "{transport}: connected to a server with an unpinned certificate"
        );
        assert_eq!(server.agent_count().await, 0);
    }
}

/// Serves a fixed `CertifiedKey` without checking that key matches cert,
/// which is exactly what an attacker replaying a public certificate does.
#[derive(Debug)]
struct FixedKey(Arc<CertifiedKey>);

impl ResolvesServerCert for FixedKey {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(Arc::clone(&self.0))
    }
}

async fn tls_server_with(certified: CertifiedKey) -> std::net::SocketAddr {
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(FixedKey(Arc::new(certified))));
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(stream).await {
                    let mut sink = Vec::new();
                    let _ = tls.read_to_end(&mut sink).await;
                }
            });
        }
    });
    addr
}

fn tls_config(addr: std::net::SocketAddr, fingerprint: &str) -> ControlConnectionConfig {
    ControlConnectionConfig {
        server_addr: addr.to_string(),
        server_cert_b64: None,
        accept_fingerprint: Some(fingerprint.to_string()),
        proxy: None,
        transport: TransportMode::Tcp,
        retry: false,
        sni: None,
        alpn: Vec::new(),
    }
}

#[tokio::test]
async fn pinned_certificate_without_its_private_key_is_rejected() {
    // Regression: the verifier asserted handshake signatures valid without
    // checking them, so anyone replaying the (public) pinned certificate with
    // their own key passed fingerprint pinning.
    let victim = Identity::generate();
    let attacker = Identity::generate();
    let (victim_certs, victim_key) = victim.pair();
    let (_, attacker_key) = attacker.pair();

    let provider = rustls::crypto::ring::default_provider();
    let honest = CertifiedKey::from_der(victim_certs.clone(), victim_key, &provider).unwrap();
    let honest_addr = tls_server_with(honest).await;
    within(ConnectionManager::establish_tls_connection(&tls_config(
        honest_addr,
        &victim.fingerprint,
    )))
    .await
    .expect("harness sanity: genuine server must be accepted");

    let forged_signer = rustls::crypto::ring::sign::any_supported_type(&attacker_key).unwrap();
    let forged = CertifiedKey::new(victim_certs, forged_signer);
    let mitm_addr = tls_server_with(forged).await;
    let result = within(ConnectionManager::establish_tls_connection(&tls_config(
        mitm_addr,
        &victim.fingerprint,
    )))
    .await;
    assert!(
        result.is_err(),
        "accepted a server that does not hold the pinned certificate's key"
    );
}

/// SNI and ALPN list from the last ClientHello.
type SeenHello = (Option<String>, Vec<Vec<u8>>);

#[derive(Debug)]
struct RecordingResolver {
    key: Arc<CertifiedKey>,
    seen: Mutex<Option<SeenHello>>,
}

impl ResolvesServerCert for RecordingResolver {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let alpn = hello
            .alpn()
            .map(|protocols| protocols.map(<[u8]>::to_vec).collect())
            .unwrap_or_default();
        *self.seen.lock().unwrap() = Some((hello.server_name().map(str::to_string), alpn));
        Some(Arc::clone(&self.key))
    }
}

async fn recording_tls_server(
    identity: &Identity,
    server_alpn: &[&[u8]],
) -> (std::net::SocketAddr, Arc<RecordingResolver>) {
    let (certs, key) = identity.pair();
    let provider = rustls::crypto::ring::default_provider();
    let resolver = Arc::new(RecordingResolver {
        key: Arc::new(CertifiedKey::from_der(certs, key, &provider).unwrap()),
        seen: Mutex::new(None),
    });
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(resolver.clone());
    config.alpn_protocols = server_alpn.iter().map(|p| p.to_vec()).collect();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(stream).await {
                    let mut sink = Vec::new();
                    let _ = tls.read_to_end(&mut sink).await;
                }
            });
        }
    });
    (addr, resolver)
}

#[tokio::test]
async fn sni_and_alpn_overrides_are_sent_and_negotiated() {
    let identity = Identity::generate();
    let (addr, resolver) = recording_tls_server(&identity, &[b"h2", b"http/1.1"]).await;

    let mut config = tls_config(addr, &identity.fingerprint);
    config.sni = Some("cdn.example.com".into());
    config.alpn = vec!["h2".into(), " ".into(), "http/1.1".into()];
    let stream = within(ConnectionManager::establish_tls_connection(&config))
        .await
        .unwrap();
    assert_eq!(stream.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
    let (sni, offered) = resolver.seen.lock().unwrap().clone().unwrap();
    assert_eq!(sni.as_deref(), Some("cdn.example.com"));
    assert_eq!(offered, vec![b"h2".to_vec(), b"http/1.1".to_vec()]);
}

#[tokio::test]
async fn default_tls_identity_is_localhost_without_alpn() {
    let identity = Identity::generate();
    let (addr, resolver) = recording_tls_server(&identity, &[]).await;
    let stream = within(ConnectionManager::establish_tls_connection(&tls_config(
        addr,
        &identity.fingerprint,
    )))
    .await
    .unwrap();
    assert_eq!(stream.get_ref().1.alpn_protocol(), None);
    let (sni, offered) = resolver.seen.lock().unwrap().clone().unwrap();
    assert_eq!(sni.as_deref(), Some("localhost"));
    assert!(offered.is_empty());
}

#[tokio::test]
async fn quic_alpn_override_must_match_server() {
    // The QUIC listener only speaks the Labyrinth control ALPN.
    let server = TestServer::start(TransportMode::Quic, None).await;
    let mut config = server.client_config();
    config.alpn = vec!["h3".into()];
    assert!(connect(&config).await.is_err());
    config.alpn = vec!["labyrinth-control/1".into()];
    connect(&config).await.unwrap();
}

#[tokio::test]
async fn agent_registers_through_socks5_proxy() {
    let server = TestServer::start(TransportMode::Tcp, Some(AUTH)).await;
    let proxy = Socks5Proxy::start(None).await;
    let mut config = server.client_config();
    config.proxy = Some(proxy.url());

    let _agent = register(&config, agent_info("proxied", Some(AUTH))).await;
    eventually("agent registered", || async {
        server.agent_count().await == 1
    })
    .await;
    assert_eq!(proxy.relayed(), 1, "traffic did not traverse the proxy");
}

#[tokio::test]
async fn socks5_credentials_are_sent_percent_decoded() {
    let server = TestServer::start(TransportMode::Tcp, None).await;
    let proxy = Socks5Proxy::start(Some(("op", "p@ss:word"))).await;

    let mut config = server.client_config();
    config.proxy = Some(format!("socks5://op:p%40ss%3Aword@{}", proxy.addr));
    let _agent = register(&config, agent_info("authed-proxy", None)).await;
    assert_eq!(proxy.relayed(), 1);

    config.proxy = Some(format!("socks5://op:wrong@{}", proxy.addr));
    assert!(connect(&config).await.is_err());

    config.proxy = Some(proxy.url());
    assert!(
        connect(&config).await.is_err(),
        "proxy demanding auth must reject anonymous clients"
    );
    assert_eq!(proxy.relayed(), 1);
}

#[tokio::test]
async fn socks5_proxy_failures_surface_as_errors() {
    let server = TestServer::start(TransportMode::Tcp, None).await;

    let (_reserved_proxy, refusing_proxy) = refusing_port();
    let mut dead_proxy = server.client_config();
    dead_proxy.proxy = Some(format!("socks5://{refusing_proxy}"));
    assert!(connect(&dead_proxy).await.is_err());

    let proxy = Socks5Proxy::start(None).await;
    let (_reserved_target, refusing_target) = refusing_port();
    let mut dead_target = server.client_config();
    dead_target.server_addr = refusing_target.to_string();
    dead_target.proxy = Some(proxy.url());
    assert!(connect(&dead_target).await.is_err());
}

#[tokio::test]
async fn quic_with_proxy_is_rejected_before_any_io() {
    let server = TestServer::start(TransportMode::Quic, None).await;
    let proxy = Socks5Proxy::start(None).await;
    let mut config = server.client_config();
    config.proxy = Some(proxy.url());
    let error = connect(&config).await.err().unwrap();
    assert!(error.to_string().contains("SOCKS5"));
    assert_eq!(proxy.relayed(), 0);
}

#[tokio::test]
async fn refused_connection_fails_fast_without_retry() {
    let (_reserved, refusing) = refusing_port();
    let config = tls_config(refusing, &"00".repeat(32));
    let started = std::time::Instant::now();
    let result = within(ConnectionManager::establish_control_connection_with_retry(
        &config,
    ))
    .await;
    assert!(matches!(result, Err(LabyrinthError::Io(_))));
    assert!(started.elapsed() < STEP);
}

#[tokio::test]
async fn server_survives_peers_that_close_or_flood_before_registering() {
    let server = TestServer::start(TransportMode::Tcp, None).await;

    // Plain TCP connect and immediate close (no TLS at all).
    drop(TcpStream::connect(server.addr).await.unwrap());

    // TLS, then close without a frame. Regression: underflow panic in the
    // registration read on an empty buffer.
    drop(connect(&server.client_config()).await.unwrap());

    // TLS, then an unterminated frame far beyond the pre-auth limit.
    let mut flood = connect(&server.client_config()).await.unwrap().stream;
    let chunk = vec![b'a'; 64 * 1024];
    let mut closed_by_server = false;
    for _ in 0..(MAX_HANDSHAKE_FRAME / chunk.len() + 64) {
        if flood.write_all(&chunk).await.is_err() {
            closed_by_server = true;
            break;
        }
    }
    if !closed_by_server {
        let mut probe = [0u8; 1];
        let read = within(flood.read(&mut probe)).await;
        assert!(
            matches!(read, Ok(0) | Err(_)),
            "server kept a flooding peer"
        );
    }

    // The listener is still healthy.
    let _agent = register(&server.client_config(), agent_info("after-abuse", None)).await;
    eventually("agent registered", || async {
        server.agent_count().await == 1
    })
    .await;
}

#[tokio::test]
async fn reconnecting_dweller_is_not_evicted_by_its_stale_session() {
    // Regression: the old session's reader removed the agent entry by ID on
    // disconnect, deleting the *new* session that had replaced it.
    let server = TestServer::start(TransportMode::Tcp, None).await;
    let config = server.client_config();

    let stale = register(&config, dweller_info("dw-1", None)).await;
    eventually("first session", || async {
        server.agent_count().await == 1
    })
    .await;
    let mut fresh = register(&config, dweller_info("dw-1", None)).await;
    // Wait until the map entry points at the fresh session.
    let mut probe_ok = false;
    for _ in 0..50 {
        server.send_to_agent("dw-1", Message::Ping).await;
        if let Ok(Ok(Some(Message::Ping))) = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            fresh.frames.next::<Message>(),
        )
        .await
        {
            probe_ok = true;
            break;
        }
    }
    assert!(probe_ok, "fresh session never became the live entry");

    drop(stale);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        server.agent_count().await,
        1,
        "stale session evicted the new one"
    );
    server.send_to_agent("dw-1", Message::Ping).await;
    assert!(matches!(fresh.recv().await, Message::Ping));

    drop(fresh);
    eventually("dweller removed after real disconnect", || async {
        server.agent_count().await == 0
    })
    .await;
}

#[tokio::test]
async fn many_agents_register_concurrently_with_distinct_ids() {
    let server = TestServer::start(TransportMode::Tcp, Some(AUTH)).await;
    let config = server.client_config();
    let sessions = futures::future::join_all(
        (0..16).map(|i| register(&config, agent_info(&format!("agent-{i}"), Some(AUTH)))),
    )
    .await;
    eventually("all agents registered", || async {
        server.agent_count().await == 16
    })
    .await;
    let ids: std::collections::HashSet<_> = server
        .server
        .agents()
        .read()
        .await
        .keys()
        .cloned()
        .collect();
    assert_eq!(ids.len(), 16);
    drop(sessions);
    eventually("all agents removed", || async {
        server.agent_count().await == 0
    })
    .await;
}
