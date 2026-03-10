use crate::error::Result;
use crate::security::SecurityManager;
use rustls::ClientConfig;

pub struct TlsConfigManager;

impl TlsConfigManager {
    pub fn create_tls_config(
        server_cert_b64: Option<String>,
        accept_fingerprint: Option<String>,
    ) -> Result<ClientConfig> {
        SecurityManager::create_tls_client_config(server_cert_b64, accept_fingerprint)
    }
}
