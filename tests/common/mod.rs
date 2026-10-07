//! Shared harness for network integration tests: throwaway identities, a real
//! agent listener on an ephemeral port, agent-side helpers, and a minimal
//! SOCKS5 proxy so proxy mode is exercised against a real TCP hop.
#![allow(dead_code)]

use labyrinth::agent::connection::{
    ConnectionManager, ControlConnectionConfig, EstablishedControlConnection,
};
use labyrinth::framing::{FrameCodec, FrameReader};
use labyrinth::protocol::{AgentInfo, AgentKind, Message, NetworkInterface};
use labyrinth::security::{parse_pem_pair, SecurityManager};
use labyrinth::server::core::LabyrinthServer;
use labyrinth::server::dweller_registry::DwellerRegistry;
use labyrinth::server::listener::spawn_agent_listener;
use labyrinth::transport::TransportMode;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// Generous per-step bound; failures should be fast, hangs must not be.
pub const STEP: Duration = Duration::from_secs(10);

pub async fn within<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(STEP, future)
        .await
        .expect("network step timed out")
}

/// Poll `condition` until it holds or `STEP` elapses.
pub async fn eventually<F, Fut>(what: &str, mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + STEP;
    while tokio::time::Instant::now() < deadline {
        if condition().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("condition never held: {what}");
}

pub struct Identity {
    pub cert_pem: String,
    pub key_pem: String,
    pub fingerprint: String,
}

impl Identity {
    pub fn generate() -> Self {
        let generated =
            SecurityManager::generate_self_signed_certificate("labyrinth-test").unwrap();
        let fingerprint = SecurityManager::fingerprint_from_pem(&generated.cert_pem).unwrap();
        Self {
            cert_pem: generated.cert_pem,
            key_pem: generated.key_pem,
            fingerprint,
        }
    }

    pub fn pair(&self) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
        parse_pem_pair(&self.cert_pem, &self.key_pem).unwrap()
    }
}

pub struct TestServer {
    pub server: Arc<LabyrinthServer>,
    pub addr: SocketAddr,
    pub identity: Identity,
    pub transport: TransportMode,
}

impl TestServer {
    pub async fn start(transport: TransportMode, auth_key: Option<&str>) -> Self {
        let identity = Identity::generate();
        let server = Arc::new(
            LabyrinthServer::new(auth_key.is_some(), auth_key.map(str::to_string))
                .with_dweller_registry(DwellerRegistry::in_memory()),
        );
        let (certs, key) = identity.pair();
        let addr = spawn_agent_listener(Arc::clone(&server), "127.0.0.1:0", transport, certs, key)
            .await
            .unwrap();
        Self {
            server,
            addr,
            identity,
            transport,
        }
    }

    pub fn client_config(&self) -> ControlConnectionConfig {
        ControlConnectionConfig {
            server_addr: self.addr.to_string(),
            server_cert_b64: None,
            accept_fingerprint: Some(self.identity.fingerprint.clone()),
            proxy: None,
            transport: self.transport,
            retry: false,
            sni: None,
            alpn: Vec::new(),
        }
    }

    pub async fn agent_count(&self) -> usize {
        self.server.agents().read().await.len()
    }

    pub async fn only_agent_id(&self) -> String {
        let agents = self.server.agents().read().await;
        assert_eq!(agents.len(), 1, "expected exactly one agent");
        agents.keys().next().unwrap().clone()
    }

    pub async fn send_to_agent(&self, agent_id: &str, message: Message) {
        let sender = self.server.agents().read().await[agent_id].sender.clone();
        sender.send(message).await.unwrap();
    }
}

pub fn agent_info(name: &str, auth_key: Option<&str>) -> AgentInfo {
    AgentInfo {
        name: name.to_string(),
        hostname: format!("{name}-host"),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
        interfaces: vec![NetworkInterface {
            name: "eth0".to_string(),
            addresses: vec!["10.10.0.5/24".to_string()],
            hardware_addr: "00:11:22:33:44:55".to_string(),
            mtu: 1500,
            flags: vec!["up".to_string()],
        }],
        auth_key: auth_key.map(str::to_string),
        kind: AgentKind::Generic,
        stable_id: None,
        listener_addr: None,
        listener_port: None,
        connectivity: Default::default(),
    }
}

pub fn dweller_info(stable_id: &str, auth_key: Option<&str>) -> AgentInfo {
    AgentInfo {
        kind: AgentKind::Dweller,
        stable_id: Some(stable_id.to_string()),
        listener_addr: Some("10.10.0.9".to_string()),
        listener_port: Some(45454),
        ..agent_info(stable_id, auth_key)
    }
}

pub type AgentStream =
    FrameReader<BufReader<Box<dyn labyrinth::agent::connection::AsyncReadWrite>>>;

/// Agent side of a registered control session.
pub struct AgentSession {
    pub frames: AgentStream,
    pub quic_connection: Option<quinn::Connection>,
}

impl AgentSession {
    pub async fn send(&mut self, message: &Message) {
        FrameCodec::CONTROL
            .write(self.frames.get_mut(), message)
            .await
            .unwrap();
    }

    pub async fn recv(&mut self) -> Message {
        within(self.frames.next::<Message>())
            .await
            .unwrap()
            .expect("server closed control stream")
    }
}

pub async fn connect(
    config: &ControlConnectionConfig,
) -> labyrinth::Result<EstablishedControlConnection> {
    within(ConnectionManager::establish_control_connection(config)).await
}

/// Send `AgentRegister` and return the reply (or the failure to read one).
pub async fn try_register(
    config: &ControlConnectionConfig,
    info: AgentInfo,
) -> labyrinth::Result<(AgentSession, Message)> {
    let connection = connect(config).await?;
    let mut frames = FrameReader::new(BufReader::new(connection.stream), FrameCodec::CONTROL);
    FrameCodec::HANDSHAKE
        .write(frames.get_mut(), &Message::AgentRegister(info))
        .await?;
    let reply = within(frames.next::<Message>()).await?.ok_or_else(|| {
        labyrinth::LabyrinthError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "server closed connection during registration",
        ))
    })?;
    Ok((
        AgentSession {
            frames,
            quic_connection: connection.quic_connection,
        },
        reply,
    ))
}

pub async fn register(config: &ControlConnectionConfig, info: AgentInfo) -> AgentSession {
    let (session, reply) = try_register(config, info).await.unwrap();
    assert!(
        matches!(reply, Message::AgentAck),
        "expected AgentAck, got {reply:?}"
    );
    session
}

/// Minimal SOCKS5 CONNECT proxy (RFC 1928) with optional username/password
/// auth (RFC 1929). Counts relayed connections so tests can prove traffic
/// actually traversed the proxy.
pub struct Socks5Proxy {
    pub addr: SocketAddr,
    pub relayed: Arc<AtomicUsize>,
}

impl Socks5Proxy {
    pub async fn start(credentials: Option<(&str, &str)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let relayed = Arc::new(AtomicUsize::new(0));
        let credentials = credentials.map(|(u, p)| (u.to_string(), p.to_string()));
        let counter = Arc::clone(&relayed);
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let credentials = credentials.clone();
                let counter = Arc::clone(&counter);
                tokio::spawn(async move {
                    let _ = socks5_session(client, credentials, counter).await;
                });
            }
        });
        Self { addr, relayed }
    }

    pub fn url(&self) -> String {
        format!("socks5://{}", self.addr)
    }

    pub fn relayed(&self) -> usize {
        self.relayed.load(Ordering::SeqCst)
    }
}

async fn socks5_session(
    mut client: TcpStream,
    credentials: Option<(String, String)>,
    relayed: Arc<AtomicUsize>,
) -> std::io::Result<()> {
    let mut header = [0u8; 2];
    client.read_exact(&mut header).await?;
    assert_eq!(header[0], 5, "not a SOCKS5 client");
    let mut methods = vec![0u8; header[1] as usize];
    client.read_exact(&mut methods).await?;

    match &credentials {
        Some((user, pass)) => {
            if !methods.contains(&2) {
                client.write_all(&[5, 0xff]).await?;
                return Ok(());
            }
            client.write_all(&[5, 2]).await?;
            let mut version_and_len = [0u8; 2];
            client.read_exact(&mut version_and_len).await?;
            let mut got_user = vec![0u8; version_and_len[1] as usize];
            client.read_exact(&mut got_user).await?;
            let mut pass_len = [0u8; 1];
            client.read_exact(&mut pass_len).await?;
            let mut got_pass = vec![0u8; pass_len[0] as usize];
            client.read_exact(&mut got_pass).await?;
            if got_user != user.as_bytes() || got_pass != pass.as_bytes() {
                client.write_all(&[1, 1]).await?;
                return Ok(());
            }
            client.write_all(&[1, 0]).await?;
        }
        None => client.write_all(&[5, 0]).await?,
    }

    let mut request = [0u8; 4];
    client.read_exact(&mut request).await?;
    let host = match request[3] {
        1 => {
            let mut ip = [0u8; 4];
            client.read_exact(&mut ip).await?;
            std::net::Ipv4Addr::from(ip).to_string()
        }
        3 => {
            let mut len = [0u8; 1];
            client.read_exact(&mut len).await?;
            let mut name = vec![0u8; len[0] as usize];
            client.read_exact(&mut name).await?;
            String::from_utf8(name).unwrap()
        }
        4 => {
            let mut ip = [0u8; 16];
            client.read_exact(&mut ip).await?;
            format!("[{}]", std::net::Ipv6Addr::from(ip))
        }
        other => panic!("unsupported SOCKS5 address type {other}"),
    };
    let mut port = [0u8; 2];
    client.read_exact(&mut port).await?;
    let port = u16::from_be_bytes(port);

    let mut upstream = match TcpStream::connect(format!("{host}:{port}")).await {
        Ok(stream) => stream,
        Err(_) => {
            client.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            return Ok(());
        }
    };
    client.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    relayed.fetch_add(1, Ordering::SeqCst);
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

/// Address that refuses connections. The socket is bound but never listens;
/// keep it alive for the test so a parallel test cannot reuse the port.
pub fn refusing_port() -> (tokio::net::TcpSocket, SocketAddr) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = socket.local_addr().unwrap();
    (socket, addr)
}
