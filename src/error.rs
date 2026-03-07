use thiserror::Error;

#[derive(Error, Debug)]
pub enum LabyrinthError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TLS error: {0}")]
    Tls(#[from] rustls::Error),

    #[error("JSON serialization/deserialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Base64 decoding error: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("Certificate generation error: {0}")]
    Rcgen(#[from] rcgen::Error),

    #[error("Invalid server name: {0}")]
    InvalidServerName(#[from] rustls::pki_types::InvalidDnsNameError),

    #[error("Address parsing error: {0}")]
    AddrParse(#[from] std::net::AddrParseError),

    #[cfg(target_os = "linux")]
    #[error("TUN error: {0}")]
    Tun(#[from] tokio_tun::Error),

    #[error("Integer parsing error: {0}")]
    ParseInt(#[from] std::num::ParseIntError),

    #[error("URL parse error: {0}")]
    UrlParse(#[from] url::ParseError),

    #[error("SOCKS error: {0}")]
    Socks(#[from] tokio_socks::Error),

    #[error("Custom error: {0}")]
    Message(String),
}

pub type Result<T> = std::result::Result<T, LabyrinthError>;
