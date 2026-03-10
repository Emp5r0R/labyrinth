use crate::error::{LabyrinthError, Result};
use crate::protocol::DwellerInstallReceipt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const DWELLER_REGISTRY_FILE: &str = "dwellers.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DwellerRecord {
    pub dweller_id: String,
    pub dweller_name: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub listen_addr: String,
    pub listen_port: u16,
    pub fingerprint: String,
    pub auth_key: String,
    pub install_path: String,
    pub config_dir: String,
    pub service_name: String,
    pub last_connected: Option<String>,
}

impl DwellerRecord {
    pub fn from_receipt(receipt: DwellerInstallReceipt, auth_key: String) -> Self {
        Self {
            dweller_id: receipt.dweller_id,
            dweller_name: receipt.dweller_name,
            hostname: receipt.hostname,
            os: receipt.os,
            arch: receipt.arch,
            listen_addr: receipt.listen_addr,
            listen_port: receipt.listen_port,
            fingerprint: receipt.fingerprint,
            auth_key,
            install_path: receipt.install_path,
            config_dir: receipt.config_dir,
            service_name: receipt.service_name,
            last_connected: None,
        }
    }

    pub fn socket_addr(&self) -> String {
        format!("{}:{}", self.listen_addr, self.listen_port)
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct DwellerRegistry {
    pub dwellers: HashMap<String, DwellerRecord>,
}

impl DwellerRegistry {
    pub fn load() -> Result<Self> {
        let path = Self::path();
        Self::load_from_path(&path)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        self.save_to_path(&path)
    }

    fn load_from_path(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = fs::read_to_string(path).map_err(LabyrinthError::Io)?;
        serde_json::from_str(&contents).map_err(LabyrinthError::Json)
    }

    fn save_to_path(&self, path: &Path) -> Result<()> {
        let body = serde_json::to_string_pretty(self)?;
        fs::write(path, body).map_err(LabyrinthError::Io)
    }

    pub fn upsert(&mut self, record: DwellerRecord) {
        self.dwellers.insert(record.dweller_id.clone(), record);
    }

    pub fn remove(&mut self, dweller_id: &str) -> Option<DwellerRecord> {
        self.dwellers.remove(dweller_id)
    }

    pub fn list(&self) -> Vec<&DwellerRecord> {
        let mut items: Vec<&DwellerRecord> = self.dwellers.values().collect();
        items.sort_by(|a, b| a.dweller_name.cmp(&b.dweller_name));
        items
    }

    fn path() -> PathBuf {
        Path::new(DWELLER_REGISTRY_FILE).to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_receipt() -> DwellerInstallReceipt {
        DwellerInstallReceipt {
            dweller_id: "dweller123".to_string(),
            dweller_name: "alpha".to_string(),
            hostname: "host1".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            listen_addr: "10.0.0.5".to_string(),
            listen_port: 45454,
            fingerprint: "abcd".to_string(),
            install_path: "/usr/local/bin/alpha".to_string(),
            config_dir: "/etc/labyrinth/alpha".to_string(),
            service_name: "labyrinth-dweller-alpha".to_string(),
        }
    }

    fn temp_registry_path() -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "labyrinth-dweller-registry-{}.json",
            std::process::id()
        ));
        path
    }

    #[test]
    fn from_receipt_preserves_install_metadata() {
        let record = DwellerRecord::from_receipt(sample_receipt(), "secret".to_string());
        assert_eq!(record.dweller_id, "dweller123");
        assert_eq!(record.auth_key, "secret");
        assert_eq!(record.install_path, "/usr/local/bin/alpha");
        assert_eq!(record.socket_addr(), "10.0.0.5:45454");
    }

    #[test]
    fn registry_save_and_load_round_trip() {
        let path = temp_registry_path();
        let _ = fs::remove_file(&path);

        let mut registry = DwellerRegistry::default();
        registry.upsert(DwellerRecord::from_receipt(
            sample_receipt(),
            "secret".to_string(),
        ));
        registry.save_to_path(&path).unwrap();

        let loaded = DwellerRegistry::load_from_path(&path).unwrap();
        let item = loaded.dwellers.get("dweller123").unwrap();
        assert_eq!(item.dweller_name, "alpha");
        assert_eq!(item.listen_port, 45454);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn registry_list_is_sorted_by_name() {
        let mut registry = DwellerRegistry::default();
        let mut bravo = DwellerRecord::from_receipt(sample_receipt(), "one".to_string());
        bravo.dweller_name = "bravo".to_string();
        bravo.dweller_id = "b".to_string();
        let mut alpha = DwellerRecord::from_receipt(sample_receipt(), "two".to_string());
        alpha.dweller_name = "alpha".to_string();
        alpha.dweller_id = "a".to_string();
        registry.upsert(bravo);
        registry.upsert(alpha);

        let names: Vec<&str> = registry
            .list()
            .into_iter()
            .map(|item| item.dweller_name.as_str())
            .collect();
        assert_eq!(names, vec!["alpha", "bravo"]);
    }
}
