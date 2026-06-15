use crate::agent::tls_config::TlsConfigManager;
use crate::error::{LabyrinthError, Result};
use crate::security::SecurityManager;
use crate::styling;
use crate::transport::{parse_socket_addr, QuicBidiStream, TransportMode};
use quinn::Endpoint;
use rustls::pki_types::ServerName;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_socks::tcp::Socks5Stream;
use tracing::{error, info};
use url::Url;

// Define a trait that combines AsyncRead, AsyncWrite, Unpin, and Send
pub trait AsyncReadWrite: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}

// Implement this trait for any type that implements all its supertraits
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> AsyncReadWrite for T {}

pub struct EstablishedControlConnection {
    pub stream: Box<dyn AsyncReadWrite>,
    pub quic_connection: Option<quinn::Connection>,
}

/// Single Responsibility: Connection establishment
pub struct ConnectionManager;

impl ConnectionManager {
    pub async fn establish_control_connection(
        server_addr: &str,
        server_cert_b64: Option<String>,
        accept_fingerprint: Option<String>,
        proxy: Option<String>,
        transport: TransportMode,
        sni: Option<String>,
        alpn: Vec<String>,
    ) -> Result<EstablishedControlConnection> {
        match transport {
            TransportMode::Tcp => {
                let stream = Self::establish_tls_connection(
                    server_addr,
                    server_cert_b64,
                    accept_fingerprint,
                    proxy,
                    sni,
                    alpn,
                )
                .await?;
                Ok(EstablishedControlConnection {
                    stream: Box::new(stream),
                    quic_connection: None,
                })
            }
            TransportMode::Quic => {
                if proxy.is_some() {
                    return Err(LabyrinthError::Message(
                        "QUIC transport does not support SOCKS5 proxy mode".to_string(),
                    ));
                }
                let (stream, connection) = Self::establish_quic_connection(
                    server_addr,
                    server_cert_b64,
                    accept_fingerprint,
                    sni,
                    alpn,
                )
                .await?;
                Ok(EstablishedControlConnection {
                    stream: Box::new(stream),
                    quic_connection: Some(connection),
                })
            }
        }
    }

    pub async fn establish_tls_connection(
        server_addr: &str,
        server_cert_b64: Option<String>,
        accept_fingerprint: Option<String>,
        proxy: Option<String>,
        sni: Option<String>,
        alpn: Vec<String>,
    ) -> Result<tokio_rustls::client::TlsStream<Box<dyn AsyncReadWrite>>> {
        let mut config = TlsConfigManager::create_tls_config(server_cert_b64, accept_fingerprint)?;
        if !alpn.is_empty() {
            config.alpn_protocols = alpn.into_iter().map(|s| s.into_bytes()).collect();
        }

        let connector = TlsConnector::from(Arc::new(config));
        let domain_str = sni.unwrap_or_else(|| "localhost".to_string());
        let domain = ServerName::try_from(domain_str)?.to_owned();

        let server_stream: Box<dyn AsyncReadWrite> = if let Some(proxy_url) = &proxy {
            let parsed_url = Url::parse(proxy_url).map_err(LabyrinthError::UrlParse)?;
            match parsed_url.scheme() {
                "socks5" => {
                    let host = parsed_url
                        .host_str()
                        .ok_or_else(|| LabyrinthError::Message("Proxy host missing".to_string()))?;
                    let port = parsed_url.port().unwrap_or(1080);
                    let proxy_addr = format!("{}:{}", host, port);
                    info!("Connecting to server via SOCKS5 proxy: {}", proxy_addr);
                    let stream = Socks5Stream::connect(proxy_addr.as_str(), server_addr)
                        .await
                        .map_err(LabyrinthError::Socks)?;
                    Box::new(stream)
                }
                _ => {
                    return Err(LabyrinthError::Message(format!(
                        "Unsupported proxy scheme: {}",
                        parsed_url.scheme()
                    )))
                }
            }
        } else {
            info!("Connecting directly to server: {}", server_addr);
            Box::new(
                TcpStream::connect(&server_addr)
                    .await
                    .map_err(LabyrinthError::Io)?,
            )
        };

        connector
            .connect(domain, server_stream)
            .await
            .map_err(LabyrinthError::Io)
    }

    async fn establish_quic_connection(
        server_addr: &str,
        server_cert_b64: Option<String>,
        accept_fingerprint: Option<String>,
        sni: Option<String>,
        alpn: Vec<String>,
    ) -> Result<(QuicBidiStream, quinn::Connection)> {
        let mut crypto =
            SecurityManager::create_tls_client_config(server_cert_b64, accept_fingerprint)?;
        
        if !alpn.is_empty() {
            crypto.alpn_protocols = alpn.into_iter().map(|s| s.into_bytes()).collect();
        } else {
            crypto.alpn_protocols = vec![b"labyrinth-control/1".to_vec()];
        }

        let quic_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
            .map_err(|e| LabyrinthError::Message(format!("Invalid QUIC client config: {}", e)))?;
        let mut client_config = quinn::ClientConfig::new(Arc::new(quic_crypto));
        client_config.transport_config(Arc::new(quinn::TransportConfig::default()));

        let server_addr = parse_socket_addr(server_addr)?;
        let mut endpoint = Endpoint::client("0.0.0.0:0".parse()?)?;
        endpoint.set_default_client_config(client_config);

        let sni_domain = sni.as_deref().unwrap_or("localhost");
        info!("Connecting to server via QUIC: {} (SNI: {})", server_addr, sni_domain);
        let connection = endpoint
            .connect(server_addr, sni_domain)
            .map_err(|e| LabyrinthError::Message(format!("QUIC connect failed: {}", e)))?
            .await
            .map_err(|e| LabyrinthError::Message(format!("QUIC handshake failed: {}", e)))?;
        let (send, recv) = connection
            .open_bi()
            .await
            .map_err(|e| LabyrinthError::Message(format!("QUIC stream open failed: {}", e)))?;

        let stream = QuicBidiStream::with_lifetime(send, recv, Some(endpoint), connection.clone());
        Ok((stream, connection))
    }

    pub async fn establish_control_connection_with_retry(
        server_addr: &str,
        server_cert_b64: Option<String>,
        accept_fingerprint: Option<String>,
        proxy: Option<String>,
        transport: TransportMode,
        retry: bool,
        sni: Option<String>,
        alpn: Vec<String>,
    ) -> Result<EstablishedControlConnection> {
        loop {
            match Self::establish_control_connection(
                server_addr,
                server_cert_b64.clone(),
                accept_fingerprint.clone(),
                proxy.clone(),
                transport,
                sni.clone(),
                alpn.clone(),
            )
            .await
            {
                Ok(stream) => return Ok(stream),
                Err(e) => {
                    error!(
                        "{} Failed to connect to server {}: {}",
                        styling::ERROR_INDICATOR,
                        server_addr,
                        e
                    );
                    if retry {
                        info!("Retrying in 5 seconds...");
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    } else {
                        return Err(e);
                    }
                }
            }
        }
    }
}
