use serde::{Deserialize, Serialize};


#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct ServerConfig {
    pub bind_addr: String,
    pub cert_path: Option<String>,
    pub key_path: Option<String>,
    pub max_connections: usize,
    pub connection_timeout_secs: u64,
    pub enable_streaming: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:44344".to_string(),
            cert_path: Some("cert.pem".to_string()),
            key_path: Some("key.pem".to_string()),
            max_connections: 100,
            connection_timeout_secs: 300,
            enable_streaming: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct AgentConfig {
    pub server_addr: String,
    pub retry_interval_secs: u64,
    pub max_retries: Option<u32>,
    pub connection_timeout_secs: u64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            server_addr: "127.0.0.1:44344".to_string(),
            retry_interval_secs: 5,
            max_retries: None,
            connection_timeout_secs: 30,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[allow(dead_code)]
pub struct LabyrinthConfig {
    pub server: ServerConfig,
    pub agent: AgentConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct LoggingConfig {
    pub level: String,
    pub file: Option<String>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            file: None,
        }
    }
}

// Default is derived
