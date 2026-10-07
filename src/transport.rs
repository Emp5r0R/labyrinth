use crate::error::{LabyrinthError, Result};
use clap::ValueEnum;
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// ALPN identifying the Labyrinth control protocol on QUIC.
pub const CONTROL_ALPN: &[u8] = b"labyrinth-control/1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TransportMode {
    Tcp,
    Quic,
}

impl fmt::Display for TransportMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Tcp => "tcp",
            Self::Quic => "quic",
        })
    }
}

impl TransportMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Tcp => "tcp/tls",
            Self::Quic => "quic/udp",
        }
    }

    pub fn supports_proxy(self) -> bool {
        matches!(self, Self::Tcp)
    }
}

pub struct QuicBidiStream {
    send: SendStream,
    recv: RecvStream,
    _endpoint: Option<Endpoint>,
    _connection: Option<Connection>,
}

impl QuicBidiStream {
    pub fn new(send: SendStream, recv: RecvStream) -> Self {
        Self {
            send,
            recv,
            _endpoint: None,
            _connection: None,
        }
    }

    pub fn with_lifetime(
        send: SendStream,
        recv: RecvStream,
        endpoint: Option<Endpoint>,
        connection: Connection,
    ) -> Self {
        Self {
            send,
            recv,
            _endpoint: endpoint,
            _connection: Some(connection),
        }
    }
}

impl AsyncRead for QuicBidiStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.recv).poll_read(cx, buf)
    }
}

impl AsyncWrite for QuicBidiStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.send)
            .poll_write(cx, buf)
            .map_err(quic_write_error)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.send).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.send).poll_shutdown(cx)
    }
}

fn quic_write_error(error: quinn::WriteError) -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, error)
}

pub fn parse_socket_addr(addr: &str) -> Result<SocketAddr> {
    addr.parse::<SocketAddr>()
        .map_err(LabyrinthError::AddrParse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_labels_are_stable() {
        assert_eq!(TransportMode::Tcp.label(), "tcp/tls");
        assert_eq!(TransportMode::Quic.label(), "quic/udp");
    }

    #[test]
    fn display_matches_cli_value_names() {
        use clap::ValueEnum;
        for mode in [TransportMode::Tcp, TransportMode::Quic] {
            let cli_name = mode.to_possible_value().unwrap().get_name().to_string();
            assert_eq!(mode.to_string(), cli_name);
            assert_eq!(TransportMode::from_str(&cli_name, true).unwrap(), mode);
        }
        assert!(TransportMode::from_str("udp", true).is_err());
    }

    #[test]
    fn parse_socket_addr_accepts_v4_v6_and_rejects_hostnames() {
        assert_eq!(parse_socket_addr("127.0.0.1:44344").unwrap().port(), 44344);
        assert!(parse_socket_addr("[::1]:443").unwrap().is_ipv6());
        for bad in [
            "localhost:44344",
            "127.0.0.1",
            "::1:443",
            "",
            "1.2.3.4:99999",
        ] {
            assert!(
                matches!(parse_socket_addr(bad), Err(LabyrinthError::AddrParse(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn control_alpn_is_stable() {
        assert_eq!(CONTROL_ALPN, b"labyrinth-control/1");
    }

    #[test]
    fn only_tcp_supports_socks_proxy() {
        assert!(TransportMode::Tcp.supports_proxy());
        assert!(!TransportMode::Quic.supports_proxy());
    }
}
