use crate::streaming::models::StreamMessage;
use serde::{Deserialize, Serialize};

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
    /// Room mode: Server requests port forwarding
    RoomPortForward {
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
    /// Streaming protocol messages
    Stream(StreamMessage),
}

impl Message {
    // Removed unused helper methods for streaming protocol
    // These methods were never used and added unnecessary complexity
}
