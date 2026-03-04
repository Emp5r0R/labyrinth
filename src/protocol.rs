
use serde::{Serialize, Deserialize};
use crate::streaming::models::StreamMessage;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NetworkInterface {
    pub name: String,
    pub addresses: Vec<String>,
    pub hardware_addr: String,
    pub mtu: u32,
    pub flags: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AgentInfo {
    pub name: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub interfaces: Vec<NetworkInterface>,
    pub auth_key: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Message {
    /// Agent registration with network information
    AgentRegister(AgentInfo),
    /// Server acknowledges agent registration
    AgentAck,
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
        command: String 
    },
    /// Command execution response
    CommandResponse { 
        output: String, 
        error: Option<String> 
    },
    /// Streaming protocol messages
    Stream(StreamMessage),
}

impl Message {
    // Removed unused helper methods for streaming protocol
    // These methods were never used and added unnecessary complexity
}
