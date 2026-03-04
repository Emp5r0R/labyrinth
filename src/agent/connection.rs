use crate::agent::tls_config::TlsConfigManager;
use crate::error::{LabyrinthError, Result};
use crate::styling;
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

/// Single Responsibility: Connection establishment
pub struct ConnectionManager;

impl ConnectionManager {
    pub async fn establish_tls_connection(
        server_addr: &str,
        server_cert_b64: Option<String>,
        accept_fingerprint: Option<String>,
        proxy: Option<String>,
    ) -> Result<tokio_rustls::client::TlsStream<Box<dyn AsyncReadWrite>>> {
        let config = TlsConfigManager::create_tls_config(server_cert_b64, accept_fingerprint)?;
        let connector = TlsConnector::from(Arc::new(config));
        let domain = ServerName::try_from("localhost")?;

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

    pub async fn establish_tls_connection_with_retry(
        server_addr: &str,
        server_cert_b64: Option<String>,
        accept_fingerprint: Option<String>,
        proxy: Option<String>,
        retry: bool,
    ) -> Result<tokio_rustls::client::TlsStream<Box<dyn AsyncReadWrite>>> {
        loop {
            match Self::establish_tls_connection(
                server_addr,
                server_cert_b64.clone(),
                accept_fingerprint.clone(),
                proxy.clone(),
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
