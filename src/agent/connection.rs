use crate::agent::tls_config::TlsConfigManager;
use crate::error::{LabyrinthError, Result};
use crate::framing::HANDSHAKE_TIMEOUT;
use crate::security::SecurityManager;
use crate::styling;
use crate::transport::{parse_socket_addr, QuicBidiStream, TransportMode, CONTROL_ALPN};
use quinn::Endpoint;
use rustls::pki_types::ServerName;
use rustls::ClientConfig;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_socks::tcp::Socks5Stream;
use tracing::{error, info};
use url::Url;

// Define a trait that combines AsyncRead, AsyncWrite, Unpin, and Send
pub trait AsyncReadWrite: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}

// Implement this trait for any type that implements all its supertraits
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> AsyncReadWrite for T {}

pub const DEFAULT_SNI: &str = "localhost";
pub const DEFAULT_SOCKS_PORT: u16 = 1080;
pub const DEFAULT_RETRY_DELAY: Duration = Duration::from_secs(5);

pub struct EstablishedControlConnection {
    pub stream: Box<dyn AsyncReadWrite>,
    pub quic_connection: Option<quinn::Connection>,
}

#[derive(Clone, Debug)]
pub struct ControlConnectionConfig {
    pub server_addr: String,
    pub server_cert_b64: Option<String>,
    pub accept_fingerprint: Option<String>,
    pub proxy: Option<String>,
    pub transport: TransportMode,
    pub retry: bool,
    pub sni: Option<String>,
    pub alpn: Vec<String>,
}

impl ControlConnectionConfig {
    /// Reject combinations that can never connect before any I/O happens.
    pub fn validate(&self) -> Result<()> {
        if self.server_addr.trim().is_empty() {
            return Err(LabyrinthError::Message(
                "server address is empty".to_string(),
            ));
        }
        if self.proxy.is_some() && !self.transport.supports_proxy() {
            return Err(LabyrinthError::Message(
                "QUIC transport does not support SOCKS5 proxy mode".to_string(),
            ));
        }
        if let Some(proxy) = &self.proxy {
            ProxyConfig::parse(proxy)?;
        }
        server_name(self.sni.as_deref())?;
        Ok(())
    }

    /// ALPN list offered on the wire for this transport.
    pub fn alpn_protocols(&self) -> Vec<Vec<u8>> {
        let default = match self.transport {
            TransportMode::Tcp => None,
            TransportMode::Quic => Some(CONTROL_ALPN),
        };
        alpn_protocols(&self.alpn, default)
    }
}

/// SOCKS5 proxy endpoint parsed from `socks5://[user[:pass]@]host[:port]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyConfig {
    pub host: String,
    pub port: u16,
    pub credentials: Option<(String, String)>,
}

impl ProxyConfig {
    pub fn parse(proxy_url: &str) -> Result<Self> {
        let parsed = Url::parse(proxy_url.trim()).map_err(LabyrinthError::UrlParse)?;
        match parsed.scheme() {
            // socks5h asks the proxy to resolve names; tokio-socks already
            // forwards hostnames unresolved, so both behave identically.
            "socks5" | "socks5h" => {}
            other => {
                return Err(LabyrinthError::Message(format!(
                    "Unsupported proxy scheme: {}",
                    other
                )))
            }
        }
        let host = parsed
            .host_str()
            .filter(|host| !host.is_empty())
            .ok_or_else(|| LabyrinthError::Message("Proxy host missing".to_string()))?;
        // Url keeps IPv6 brackets in host_str; TcpStream::connect needs them stripped
        // when combined with a port via a tuple, but kept for "host:port" strings.
        let host = host.to_string();
        let credentials = if parsed.username().is_empty() {
            None
        } else {
            Some((
                percent_decode(parsed.username())?,
                percent_decode(parsed.password().unwrap_or(""))?,
            ))
        };
        Ok(Self {
            host,
            port: parsed.port().unwrap_or(DEFAULT_SOCKS_PORT),
            credentials,
        })
    }

    pub fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    async fn connect(&self, target: &str) -> Result<Socks5Stream<TcpStream>> {
        let proxy_addr = self.address();
        let stream = match &self.credentials {
            Some((username, password)) => {
                Socks5Stream::connect_with_password(proxy_addr.as_str(), target, username, password)
                    .await
            }
            None => Socks5Stream::connect(proxy_addr.as_str(), target).await,
        };
        stream.map_err(LabyrinthError::Socks)
    }
}

fn percent_decode(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|pair| std::str::from_utf8(pair).ok())
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .ok_or_else(|| {
                    LabyrinthError::Message("Invalid percent-encoding in proxy URL".to_string())
                })?;
            out.push(hex);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out)
        .map_err(|_| LabyrinthError::Message("Proxy credentials are not UTF-8".to_string()))
}

/// Configured ALPN values, or `default` when none are configured. Blank
/// entries (e.g. from `--alpn h2,,http/1.1`) are dropped.
pub fn alpn_protocols(configured: &[String], default: Option<&[u8]>) -> Vec<Vec<u8>> {
    let configured: Vec<Vec<u8>> = configured
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| value.as_bytes().to_vec())
        .collect();
    match (configured.is_empty(), default) {
        (false, _) => configured,
        (true, Some(default)) => vec![default.to_vec()],
        (true, None) => Vec::new(),
    }
}

/// TLS server name for the handshake, defaulting to `localhost` because the
/// generated server certificate always carries that SAN.
pub fn server_name(sni: Option<&str>) -> Result<ServerName<'static>> {
    let name = sni
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_SNI);
    Ok(ServerName::try_from(name.to_string())?)
}

async fn with_deadline<F, T>(deadline: Duration, what: &str, future: F) -> Result<T>
where
    F: Future<Output = std::io::Result<T>>,
{
    tokio::time::timeout(deadline, future)
        .await
        .map_err(|_| {
            LabyrinthError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("{what} timed out"),
            ))
        })?
        .map_err(LabyrinthError::Io)
}

/// Single Responsibility: Connection establishment
pub struct ConnectionManager;

impl ConnectionManager {
    pub async fn establish_control_connection(
        connection_config: &ControlConnectionConfig,
    ) -> Result<EstablishedControlConnection> {
        connection_config.validate()?;
        match connection_config.transport {
            TransportMode::Tcp => {
                let stream = Self::establish_tls_connection(connection_config).await?;
                Ok(EstablishedControlConnection {
                    stream: Box::new(stream),
                    quic_connection: None,
                })
            }
            TransportMode::Quic => {
                let (stream, connection) =
                    Self::establish_quic_connection(connection_config).await?;
                Ok(EstablishedControlConnection {
                    stream: Box::new(stream),
                    quic_connection: Some(connection),
                })
            }
        }
    }

    /// Pinned TLS client config with the configured ALPN applied.
    pub fn client_tls_config(connection_config: &ControlConnectionConfig) -> Result<ClientConfig> {
        let mut config = TlsConfigManager::create_tls_config(
            connection_config.server_cert_b64.clone(),
            connection_config.accept_fingerprint.clone(),
        )?;
        config.alpn_protocols = connection_config.alpn_protocols();
        Ok(config)
    }

    /// Run the client TLS handshake over an already-open byte stream (direct
    /// TCP, SOCKS tunnel, or an in-memory pipe in tests).
    pub async fn tls_handshake<S>(
        config: ClientConfig,
        sni: Option<&str>,
        stream: S,
    ) -> Result<tokio_rustls::client::TlsStream<S>>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let connector = TlsConnector::from(Arc::new(config));
        let domain = server_name(sni)?;
        with_deadline(
            HANDSHAKE_TIMEOUT,
            "TLS handshake",
            connector.connect(domain, stream),
        )
        .await
    }

    pub async fn establish_tls_connection(
        connection_config: &ControlConnectionConfig,
    ) -> Result<tokio_rustls::client::TlsStream<Box<dyn AsyncReadWrite>>> {
        let config = Self::client_tls_config(connection_config)?;

        let server_stream: Box<dyn AsyncReadWrite> =
            if let Some(proxy_url) = &connection_config.proxy {
                let proxy = ProxyConfig::parse(proxy_url)?;
                info!("Connecting to server via SOCKS5 proxy: {}", proxy.address());
                let stream = tokio::time::timeout(
                    HANDSHAKE_TIMEOUT,
                    proxy.connect(&connection_config.server_addr),
                )
                .await
                .map_err(|_| {
                    LabyrinthError::Io(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "SOCKS5 connect timed out",
                    ))
                })??;
                Box::new(stream)
            } else {
                info!(
                    "Connecting directly to server: {}",
                    connection_config.server_addr
                );
                Box::new(
                    with_deadline(
                        HANDSHAKE_TIMEOUT,
                        "TCP connect",
                        TcpStream::connect(&connection_config.server_addr),
                    )
                    .await?,
                )
            };

        Self::tls_handshake(config, connection_config.sni.as_deref(), server_stream).await
    }

    async fn establish_quic_connection(
        connection_config: &ControlConnectionConfig,
    ) -> Result<(QuicBidiStream, quinn::Connection)> {
        let mut crypto = SecurityManager::create_tls_client_config(
            connection_config.server_cert_b64.clone(),
            connection_config.accept_fingerprint.clone(),
        )?;
        crypto.alpn_protocols = connection_config.alpn_protocols();

        let quic_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
            .map_err(|e| LabyrinthError::Message(format!("Invalid QUIC client config: {}", e)))?;
        let mut client_config = quinn::ClientConfig::new(Arc::new(quic_crypto));
        client_config.transport_config(Arc::new(quinn::TransportConfig::default()));

        let server_addr = parse_socket_addr(&connection_config.server_addr)?;
        let bind_addr = if server_addr.is_ipv6() {
            "[::]:0"
        } else {
            "0.0.0.0:0"
        };
        let mut endpoint = Endpoint::client(bind_addr.parse()?)?;
        endpoint.set_default_client_config(client_config);

        let sni_domain = connection_config
            .sni
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_SNI);
        info!(
            "Connecting to server via QUIC: {} (SNI: {})",
            server_addr, sni_domain
        );
        let connecting = endpoint
            .connect(server_addr, sni_domain)
            .map_err(|e| LabyrinthError::Message(format!("QUIC connect failed: {}", e)))?;
        let connection = tokio::time::timeout(HANDSHAKE_TIMEOUT, connecting)
            .await
            .map_err(|_| LabyrinthError::Message("QUIC handshake timed out".to_string()))?
            .map_err(|e| LabyrinthError::Message(format!("QUIC handshake failed: {}", e)))?;
        let (send, recv) = connection
            .open_bi()
            .await
            .map_err(|e| LabyrinthError::Message(format!("QUIC stream open failed: {}", e)))?;

        let stream = QuicBidiStream::with_lifetime(send, recv, Some(endpoint), connection.clone());
        Ok((stream, connection))
    }

    pub async fn establish_control_connection_with_retry(
        connection_config: &ControlConnectionConfig,
    ) -> Result<EstablishedControlConnection> {
        let policy = if connection_config.retry {
            RetryPolicy::forever(DEFAULT_RETRY_DELAY)
        } else {
            RetryPolicy::once()
        };
        policy
            .run(|| async {
                let result = Self::establish_control_connection(connection_config).await;
                if let Err(e) = &result {
                    error!(
                        "{} Failed to connect to server {}: {}",
                        styling::ERROR_INDICATOR,
                        connection_config.server_addr,
                        e
                    );
                }
                result
            })
            .await
    }
}

/// How often and how long to retry a fallible async operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    pub delay: Duration,
    /// `None` retries forever.
    pub max_attempts: Option<usize>,
}

impl RetryPolicy {
    pub fn once() -> Self {
        Self {
            delay: Duration::ZERO,
            max_attempts: Some(1),
        }
    }

    pub fn forever(delay: Duration) -> Self {
        Self {
            delay,
            max_attempts: None,
        }
    }

    pub fn attempts(max_attempts: usize, delay: Duration) -> Self {
        Self {
            delay,
            max_attempts: Some(max_attempts.max(1)),
        }
    }

    pub async fn run<F, Fut, T>(self, mut operation: F) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let mut attempt = 0usize;
        loop {
            attempt += 1;
            match operation().await {
                Ok(value) => return Ok(value),
                Err(error) => {
                    if self.max_attempts.is_some_and(|max| attempt >= max) {
                        return Err(error);
                    }
                    info!("Retrying in {:?}...", self.delay);
                    tokio::time::sleep(self.delay).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{parse_pem_pair, FingerprintVerifier};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn config(transport: TransportMode) -> ControlConnectionConfig {
        ControlConnectionConfig {
            server_addr: "127.0.0.1:44344".into(),
            server_cert_b64: None,
            accept_fingerprint: Some("ab".repeat(32)),
            proxy: None,
            transport,
            retry: false,
            sni: None,
            alpn: Vec::new(),
        }
    }

    #[test]
    fn proxy_parse_defaults_port_and_accepts_socks5h() {
        assert_eq!(
            ProxyConfig::parse("socks5://127.0.0.1").unwrap(),
            ProxyConfig {
                host: "127.0.0.1".into(),
                port: DEFAULT_SOCKS_PORT,
                credentials: None,
            }
        );
        let parsed = ProxyConfig::parse("socks5h://proxy.internal:9050").unwrap();
        assert_eq!(parsed.address(), "proxy.internal:9050");
        assert_eq!(
            ProxyConfig::parse("socks5://[::1]:1080").unwrap().address(),
            "[::1]:1080"
        );
    }

    #[test]
    fn proxy_parse_decodes_credentials() {
        let parsed = ProxyConfig::parse("socks5://op%20er:p%40ss%3A1@10.0.0.1:1080").unwrap();
        assert_eq!(
            parsed.credentials,
            Some(("op er".to_string(), "p@ss:1".to_string()))
        );
        let user_only = ProxyConfig::parse("socks5://op@10.0.0.1").unwrap();
        assert_eq!(user_only.credentials, Some(("op".into(), String::new())));
    }

    #[test]
    fn proxy_parse_rejects_bad_urls() {
        for url in [
            "",
            "10.0.0.1:1080",
            "http://10.0.0.1:8080",
            "socks4://10.0.0.1",
            "socks5://",
            "socks5://10.0.0.1:99999",
            "socks5://u:%zz@10.0.0.1",
            "socks5://u:%ff@10.0.0.1",
        ] {
            assert!(
                ProxyConfig::parse(url).is_err(),
                "{url:?} should be rejected"
            );
        }
    }

    #[test]
    fn percent_decode_handles_edges() {
        assert_eq!(percent_decode("").unwrap(), "");
        assert_eq!(percent_decode("plain").unwrap(), "plain");
        assert_eq!(percent_decode("%41%62").unwrap(), "Ab");
        assert!(percent_decode("%4").is_err());
        assert!(percent_decode("%").is_err());
    }

    #[test]
    fn alpn_protocols_drop_blanks_and_apply_default() {
        let configured = vec!["h2".to_string(), "  ".to_string(), " http/1.1 ".to_string()];
        assert_eq!(
            alpn_protocols(&configured, Some(b"x")),
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );
        assert_eq!(alpn_protocols(&[], Some(b"x")), vec![b"x".to_vec()]);
        assert!(alpn_protocols(&[], None).is_empty());
        assert!(alpn_protocols(&["".into()], None).is_empty());
    }

    #[test]
    fn transport_specific_alpn_defaults() {
        assert!(config(TransportMode::Tcp).alpn_protocols().is_empty());
        assert_eq!(
            config(TransportMode::Quic).alpn_protocols(),
            vec![CONTROL_ALPN.to_vec()]
        );
        let mut custom = config(TransportMode::Quic);
        custom.alpn = vec!["h3".into()];
        assert_eq!(custom.alpn_protocols(), vec![b"h3".to_vec()]);
    }

    #[test]
    fn server_name_defaults_to_localhost_and_accepts_ips() {
        assert_eq!(
            server_name(None).unwrap(),
            ServerName::try_from("localhost").unwrap()
        );
        assert_eq!(
            server_name(Some("  ")).unwrap(),
            ServerName::try_from("localhost").unwrap()
        );
        assert!(matches!(
            server_name(Some("10.0.0.1")).unwrap(),
            ServerName::IpAddress(_)
        ));
        assert!(server_name(Some("cdn.example.com")).is_ok());
        assert!(server_name(Some("bad name")).is_err());
    }

    #[test]
    fn validate_rejects_impossible_combinations() {
        assert!(config(TransportMode::Tcp).validate().is_ok());

        let mut quic_proxy = config(TransportMode::Quic);
        quic_proxy.proxy = Some("socks5://127.0.0.1:1080".into());
        assert!(quic_proxy.validate().is_err());

        let mut bad_proxy = config(TransportMode::Tcp);
        bad_proxy.proxy = Some("http://127.0.0.1".into());
        assert!(bad_proxy.validate().is_err());

        let mut bad_sni = config(TransportMode::Tcp);
        bad_sni.sni = Some("not valid".into());
        assert!(bad_sni.validate().is_err());

        let mut empty = config(TransportMode::Tcp);
        empty.server_addr = " ".into();
        assert!(empty.validate().is_err());
    }

    #[tokio::test]
    async fn quic_requires_socket_address_not_hostname() {
        let mut quic = config(TransportMode::Quic);
        quic.server_addr = "server.example:44344".into();
        assert!(ConnectionManager::establish_control_connection(&quic)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn retry_policy_once_does_not_retry() {
        let calls = AtomicUsize::new(0);
        let result: Result<()> = RetryPolicy::once()
            .run(|| async {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(LabyrinthError::Message("boom".into()))
            })
            .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retry_policy_stops_at_max_attempts_and_returns_last_error() {
        let calls = AtomicUsize::new(0);
        let result: Result<()> = RetryPolicy::attempts(3, Duration::from_millis(1))
            .run(|| async {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                Err(LabyrinthError::Message(format!("attempt {n}")))
            })
            .await;
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert!(result.unwrap_err().to_string().contains("attempt 2"));
        assert_eq!(
            RetryPolicy::attempts(0, Duration::ZERO).max_attempts,
            Some(1)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn retry_policy_forever_waits_between_attempts_until_success() {
        let calls = AtomicUsize::new(0);
        let started = tokio::time::Instant::now();
        let value = RetryPolicy::forever(Duration::from_secs(5))
            .run(|| async {
                if calls.fetch_add(1, Ordering::SeqCst) < 4 {
                    Err(LabyrinthError::Message("not yet".into()))
                } else {
                    Ok(42)
                }
            })
            .await
            .unwrap();
        assert_eq!(value, 42);
        assert_eq!(calls.load(Ordering::SeqCst), 5);
        assert_eq!(started.elapsed(), Duration::from_secs(20));
    }

    /// Run a pinned client handshake against an in-memory rustls server.
    async fn handshake_over_pipe(
        pinned_fingerprint: &str,
        client_alpn: &[&str],
        server_alpn: &[&[u8]],
    ) -> Result<Option<Vec<u8>>> {
        let identity = SecurityManager::generate_self_signed_certificate("pipe").unwrap();
        let (certs, key) = parse_pem_pair(&identity.cert_pem, &identity.key_pem).unwrap();
        let mut server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap();
        server_config.alpn_protocols = server_alpn.iter().map(|p| p.to_vec()).collect();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));

        let pin = if pinned_fingerprint.is_empty() {
            SecurityManager::fingerprint_from_pem(&identity.cert_pem).unwrap()
        } else {
            pinned_fingerprint.to_string()
        };
        let mut client_config = SecurityManager::client_config_with_verifier(
            FingerprintVerifier::from_fingerprint(&pin).unwrap(),
        );
        client_config.alpn_protocols = alpn_protocols(
            &client_alpn
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
            None,
        );

        let (client_io, server_io) = tokio::io::duplex(16 * 1024);
        let server = tokio::spawn(async move { acceptor.accept(server_io).await.map(|_| ()) });
        let client = ConnectionManager::tls_handshake(client_config, None, client_io).await;
        let _ = server.await;
        client.map(|stream| stream.get_ref().1.alpn_protocol().map(<[u8]>::to_vec))
    }

    #[tokio::test]
    async fn tls_handshake_over_any_stream_with_pin_and_alpn() {
        assert_eq!(
            handshake_over_pipe("", &["h2"], &[b"h2"]).await.unwrap(),
            Some(b"h2".to_vec())
        );
        assert_eq!(handshake_over_pipe("", &[], &[]).await.unwrap(), None);
    }

    #[tokio::test]
    async fn tls_handshake_rejects_unpinned_certificate() {
        assert!(handshake_over_pipe(&"cd".repeat(32), &[], &[])
            .await
            .is_err());
    }

    #[tokio::test]
    async fn tls_handshake_fails_on_alpn_mismatch() {
        // rustls servers with ALPN configured refuse clients offering none of it.
        assert!(handshake_over_pipe("", &["h2"], &[b"http/1.1"])
            .await
            .is_err());
    }
}
