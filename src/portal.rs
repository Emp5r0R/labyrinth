//! Portal wire policy shared by server and agent.
//!
//! Both sides must agree on what a valid target is and on how native QUIC
//! Portal streams are introduced, so that logic lives here instead of being
//! duplicated (and drifting) in role-specific modules.

use crate::error::{LabyrinthError, Result};
use crate::framing::FrameCodec;
use crate::protocol::Message;
use crate::streaming::models::{ConnectionId, PortMapping, StreamMessage};
use std::net::IpAddr;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncWrite};

pub use crate::framing::MAX_SETUP_FRAME;

const MAX_HOSTNAME_LEN: usize = 253;

/// Parse `local_port:target_host:target_port`.
///
/// IPv6 targets must use brackets (`8080:[2001:db8::1]:443`). Splitting from
/// the right avoids corrupting IPv6 addresses while rejecting ambiguous input.
pub fn parse_mapping(input: &str) -> Result<PortMapping> {
    let input = input.trim();
    let (local, remainder) = input.split_once(':').ok_or_else(|| {
        LabyrinthError::Message("mapping must be local_port:target_host:target_port".into())
    })?;
    let (host, target) = remainder.rsplit_once(':').ok_or_else(|| {
        LabyrinthError::Message("mapping must be local_port:target_host:target_port".into())
    })?;
    if host.contains(':') && !(host.starts_with('[') && host.ends_with(']')) {
        return Err(LabyrinthError::Message(
            "IPv6 target host must be enclosed in brackets".into(),
        ));
    }

    let mapping = PortMapping {
        local_port: local
            .parse()
            .map_err(|_| LabyrinthError::Message(format!("invalid local port `{local}`")))?,
        target_host: host.trim().to_string(),
        target_port: target
            .parse()
            .map_err(|_| LabyrinthError::Message(format!("invalid target port `{target}`")))?,
    };
    validate_mapping(&mapping)?;
    Ok(mapping)
}

/// Validate a full server-side mapping, including the listener port.
pub fn validate_mapping(mapping: &PortMapping) -> Result<()> {
    if mapping.local_port == 0 {
        return Err(LabyrinthError::Message(
            "local port 0 is not valid for a persistent Portal listener".into(),
        ));
    }
    validate_target(mapping)
}

/// Validate only the dial target. The agent never binds `local_port`, so it
/// uses this instead of [`validate_mapping`].
pub fn validate_target(mapping: &PortMapping) -> Result<()> {
    if mapping.target_port == 0 {
        return Err(LabyrinthError::Message("target port 0 is not valid".into()));
    }

    let host = mapping.target_host.trim();
    if host.is_empty() {
        return Err(LabyrinthError::Message("target host is empty".into()));
    }
    if host != mapping.target_host
        || host
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_whitespace())
    {
        return Err(LabyrinthError::Message(
            "target host must not contain whitespace or NUL bytes".into(),
        ));
    }
    if host.len() > MAX_HOSTNAME_LEN {
        return Err(LabyrinthError::Message("target host is too long".into()));
    }

    if host.starts_with('[') || host.ends_with(']') {
        let value = host
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .ok_or_else(|| {
                LabyrinthError::Message("IPv6 target host must be enclosed in brackets".into())
            })?;
        value.parse::<IpAddr>().map_err(|_| {
            LabyrinthError::Message(format!("invalid bracketed target host `{host}`"))
        })?;
    } else if host.parse::<IpAddr>().is_err()
        && host
            .chars()
            .any(|ch| !(ch.is_ascii_alphanumeric() || ".-_".contains(ch)))
    {
        // Hostnames are allowed, but reject characters that can alter endpoint
        // parsing or shell/log output. DNS resolution remains OS responsibility.
        return Err(LabyrinthError::Message(format!(
            "invalid target host `{host}`"
        )));
    }
    Ok(())
}

/// Build a dialable `host:port` without breaking IPv6 targets.
pub fn target_address(mapping: &PortMapping) -> Result<String> {
    validate_target(mapping)?;
    let host = mapping.target_host.as_str();
    if host.contains(':') && !(host.starts_with('[') && host.ends_with(']')) {
        Ok(format!("[{host}]:{}", mapping.target_port))
    } else {
        Ok(format!("{host}:{}", mapping.target_port))
    }
}

/// First frame on a native QUIC Portal stream (server -> agent).
pub async fn write_quic_setup<W>(
    writer: &mut W,
    connection_id: ConnectionId,
    mapping: PortMapping,
) -> Result<()>
where
    W: AsyncWrite + Unpin + ?Sized,
{
    FrameCodec::SETUP
        .write(
            writer,
            &Message::Stream(StreamMessage::Setup {
                connection_id,
                mapping,
            }),
        )
        .await
}

/// Agent reply to [`write_quic_setup`]. `error` of `None` means success.
pub async fn write_quic_setup_ack<W>(
    writer: &mut W,
    connection_id: ConnectionId,
    error: Option<String>,
) -> Result<()>
where
    W: AsyncWrite + Unpin + ?Sized,
{
    FrameCodec::SETUP
        .write(
            writer,
            &Message::Stream(StreamMessage::SetupAck {
                connection_id,
                success: error.is_none(),
                error_message: error,
            }),
        )
        .await
}

/// Read one bounded QUIC setup frame. A peer that sends bytes without a
/// newline, or nothing at all, cannot hold the task past `deadline`.
pub async fn read_quic_setup<R>(
    reader: &mut R,
    deadline: Duration,
) -> Result<(ConnectionId, PortMapping)>
where
    R: AsyncBufRead + Unpin + ?Sized,
{
    match FrameCodec::SETUP
        .read_required::<_, Message>(reader, deadline)
        .await?
    {
        Message::Stream(StreamMessage::Setup {
            connection_id,
            mapping,
        }) => {
            validate_target(&mapping)?;
            Ok((connection_id, mapping))
        }
        other => Err(LabyrinthError::Message(format!(
            "unexpected Portal QUIC setup message: {other:?}"
        ))),
    }
}

/// Read the agent's setup acknowledgment for `expected_connection_id`.
pub async fn read_quic_setup_ack<R>(
    reader: &mut R,
    deadline: Duration,
    expected_connection_id: ConnectionId,
) -> Result<()>
where
    R: AsyncBufRead + Unpin + ?Sized,
{
    match FrameCodec::SETUP
        .read_required::<_, Message>(reader, deadline)
        .await?
    {
        Message::Stream(StreamMessage::SetupAck {
            connection_id,
            success: true,
            ..
        }) if connection_id == expected_connection_id => Ok(()),
        Message::Stream(StreamMessage::SetupAck {
            connection_id,
            success: false,
            error_message,
        }) if connection_id == expected_connection_id => Err(LabyrinthError::Message(
            error_message.unwrap_or_else(|| "Portal target setup failed".into()),
        )),
        other => Err(LabyrinthError::Message(format!(
            "unexpected Portal QUIC setup acknowledgment: {other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, BufReader};

    const STEP: Duration = Duration::from_secs(2);

    fn mapping(local_port: u16, host: &str, target_port: u16) -> PortMapping {
        PortMapping {
            local_port,
            target_host: host.into(),
            target_port,
        }
    }

    #[test]
    fn parse_mapping_accepts_ipv4_hostname_and_bracketed_ipv6() {
        assert_eq!(
            parse_mapping("8080:192.168.1.10:80").unwrap(),
            mapping(8080, "192.168.1.10", 80)
        );
        assert_eq!(
            parse_mapping(" 8080:example.test:443 ").unwrap(),
            mapping(8080, "example.test", 443)
        );
        assert_eq!(
            parse_mapping("8080:[2001:db8::1]:443").unwrap(),
            mapping(8080, "[2001:db8::1]", 443)
        );
        assert_eq!(
            parse_mapping("1:host_name-1.internal:65535").unwrap(),
            mapping(1, "host_name-1.internal", 65535)
        );
    }

    #[test]
    fn parse_mapping_rejects_malformed_input() {
        for input in [
            "",
            "8080",
            "8080:host",
            "0:example.test:443",
            "8080:example.test:0",
            "70000:example.test:80",
            "8080:example.test:70000",
            "abc:example.test:80",
            "8080:example.test:http",
            "8080:2001:db8::1:443",
            "8080:bad host:443",
            "8080::443",
            "8080:[not-an-ip]:443",
            "8080:[::1:443",
            "8080:host;rm:443",
            "8080:host/x:443",
        ] {
            assert!(
                parse_mapping(input).is_err(),
                "{input:?} should be rejected"
            );
        }
    }

    #[test]
    fn validate_target_ignores_local_port_but_validate_mapping_does_not() {
        let agent_side = mapping(0, "10.0.0.1", 22);
        assert!(validate_target(&agent_side).is_ok());
        assert!(validate_mapping(&agent_side).is_err());
    }

    #[test]
    fn validate_target_bounds_hostname_length() {
        let long = "a".repeat(MAX_HOSTNAME_LEN);
        assert!(validate_target(&mapping(1, &long, 80)).is_ok());
        let too_long = "a".repeat(MAX_HOSTNAME_LEN + 1);
        assert!(validate_target(&mapping(1, &too_long, 80)).is_err());
    }

    #[test]
    fn validate_target_rejects_padded_and_control_hosts() {
        for host in [" 10.0.0.1", "10.0.0.1 ", "a\tb", "a\0b", "a\nb"] {
            assert!(
                validate_target(&mapping(1, host, 80)).is_err(),
                "{host:?} should be rejected"
            );
        }
    }

    #[test]
    fn target_address_wraps_bare_ipv6_and_keeps_bracketed() {
        assert_eq!(
            target_address(&mapping(1, "10.0.0.5", 80)).unwrap(),
            "10.0.0.5:80"
        );
        assert_eq!(
            target_address(&mapping(1, "2001:db8::1", 443)).unwrap(),
            "[2001:db8::1]:443"
        );
        assert_eq!(
            target_address(&mapping(1, "[2001:db8::1]", 443)).unwrap(),
            "[2001:db8::1]:443"
        );
        assert_eq!(
            target_address(&mapping(0, "db.internal", 5432)).unwrap(),
            "db.internal:5432"
        );
        assert!(target_address(&mapping(1, "bad host", 80)).is_err());
    }

    #[test]
    fn target_address_output_is_parseable_for_ip_targets() {
        for host in ["127.0.0.1", "::1", "[fe80::1]"] {
            let address = target_address(&mapping(1, host, 9)).unwrap();
            assert!(
                address.parse::<std::net::SocketAddr>().is_ok(),
                "{address} should parse"
            );
        }
    }

    #[tokio::test]
    async fn quic_setup_round_trips() {
        let (mut client, server) = duplex(1024);
        let id = ConnectionId::new_v4();
        write_quic_setup(&mut client, id, mapping(8080, "127.0.0.1", 443))
            .await
            .unwrap();
        let mut server = BufReader::new(server);
        let (read_id, read_mapping) = read_quic_setup(&mut server, STEP).await.unwrap();
        assert_eq!(read_id, id);
        assert_eq!(read_mapping, mapping(8080, "127.0.0.1", 443));
    }

    #[tokio::test]
    async fn quic_setup_rejects_oversized_frame() {
        let oversized = vec![b'x'; MAX_SETUP_FRAME + 16];
        let mut reader = BufReader::new(std::io::Cursor::new(oversized));
        assert!(matches!(
            read_quic_setup(&mut reader, STEP).await,
            Err(LabyrinthError::FrameTooLarge { .. })
        ));
    }

    #[tokio::test]
    async fn quic_setup_rejects_invalid_target_and_wrong_message() {
        let (mut client, server) = duplex(1024);
        let mut server = BufReader::new(server);
        write_quic_setup(
            &mut client,
            ConnectionId::new_v4(),
            mapping(1, "bad host", 80),
        )
        .await
        .unwrap();
        assert!(read_quic_setup(&mut server, STEP).await.is_err());

        FrameCodec::SETUP
            .write(&mut client, &Message::Ping)
            .await
            .unwrap();
        assert!(read_quic_setup(&mut server, STEP).await.is_err());
    }

    #[tokio::test]
    async fn quic_setup_times_out_on_silent_peer() {
        let (_client, server) = duplex(64);
        let mut server = BufReader::new(server);
        let started = std::time::Instant::now();
        assert!(read_quic_setup(&mut server, Duration::from_millis(50))
            .await
            .is_err());
        assert!(started.elapsed() < STEP);
    }

    #[tokio::test]
    async fn quic_setup_ack_success_failure_and_mismatch() {
        let id = ConnectionId::new_v4();
        let (mut client, server) = duplex(4096);
        let mut server = BufReader::new(server);

        write_quic_setup_ack(&mut client, id, None).await.unwrap();
        read_quic_setup_ack(&mut server, STEP, id).await.unwrap();

        write_quic_setup_ack(&mut client, id, Some("refused".into()))
            .await
            .unwrap();
        let error = read_quic_setup_ack(&mut server, STEP, id)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("refused"));

        // An ack for another stream must never be accepted as ours.
        write_quic_setup_ack(&mut client, ConnectionId::new_v4(), None)
            .await
            .unwrap();
        assert!(read_quic_setup_ack(&mut server, STEP, id).await.is_err());
    }

    #[tokio::test]
    async fn quic_setup_ack_rejects_eof() {
        let (client, server) = duplex(64);
        drop(client);
        let mut server = BufReader::new(server);
        assert!(
            read_quic_setup_ack(&mut server, STEP, ConnectionId::new_v4())
                .await
                .is_err()
        );
    }
}
