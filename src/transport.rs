use crate::error::{LabyrinthError, Result};
use clap::ValueEnum;
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

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
    fn only_tcp_supports_socks_proxy() {
        assert!(TransportMode::Tcp.supports_proxy());
        assert!(!TransportMode::Quic.supports_proxy());
    }
}
