use crate::error::{LabyrinthError, Result};
use crate::protocol::{
    DwellerHibernationConfig, DwellerInstallReceipt, DwellerPathHop, DwellerServerEndpoint,
    DwellerTask, DwellerTaskKind, DwellerTaskResult, DwellerTaskStatus,
};
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
    #[serde(default)]
    pub callback_servers: Vec<DwellerServerEndpoint>,
    #[serde(default)]
    pub path: Vec<DwellerPathHop>,
    #[serde(default)]
    pub hibernation: DwellerHibernationConfig,
    #[serde(default)]
    pub tasks: Vec<DwellerTask>,
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
            callback_servers: receipt.callback_servers,
            path: receipt.parent_path,
            hibernation: receipt.hibernation,
            tasks: Vec::new(),
        }
    }

    pub fn socket_addr(&self) -> String {
        format!("{}:{}", self.listen_addr, self.listen_port)
    }
}

/// Where a registry persists itself. Kept out of the serialized form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryStorage {
    File(PathBuf),
    /// No persistence; used by tests and embedders.
    Memory,
}

impl Default for RegistryStorage {
    fn default() -> Self {
        Self::File(PathBuf::from(DWELLER_REGISTRY_FILE))
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct DwellerRegistry {
    pub dwellers: HashMap<String, DwellerRecord>,
    #[serde(skip)]
    storage: RegistryStorage,
}

impl DwellerRegistry {
    pub fn in_memory() -> Self {
        Self {
            dwellers: HashMap::new(),
            storage: RegistryStorage::Memory,
        }
    }

    pub fn load() -> Result<Self> {
        Self::load_from_path(Path::new(DWELLER_REGISTRY_FILE))
    }

    pub fn storage(&self) -> &RegistryStorage {
        &self.storage
    }

    pub fn save(&self) -> Result<()> {
        match &self.storage {
            RegistryStorage::File(path) => self.save_to_path(path),
            RegistryStorage::Memory => Ok(()),
        }
    }

    pub fn load_from_path(path: &Path) -> Result<Self> {
        let mut registry = if path.exists() {
            let contents = fs::read_to_string(path).map_err(LabyrinthError::Io)?;
            serde_json::from_str::<Self>(&contents).map_err(LabyrinthError::Json)?
        } else {
            Self::default()
        };
        registry.storage = RegistryStorage::File(path.to_path_buf());
        Ok(registry)
    }

    /// Write via a sibling temp file and rename, so a crash mid-write never
    /// leaves a truncated registry (which would forget every dweller).
    pub fn save_to_path(&self, path: &Path) -> Result<()> {
        let body = serde_json::to_string_pretty(self)?;
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        fs::write(&tmp, body).map_err(LabyrinthError::Io)?;
        fs::rename(&tmp, path).map_err(LabyrinthError::Io)
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

    pub fn enqueue_task(
        &mut self,
        dweller_id: &str,
        kind: DwellerTaskKind,
        now: String,
    ) -> Option<DwellerTask> {
        let record = self.dwellers.get_mut(dweller_id)?;
        let task = DwellerTask {
            task_id: uuid::Uuid::new_v4().to_string(),
            kind,
            status: DwellerTaskStatus::Pending,
            created_at: now.clone(),
            updated_at: Some(now),
            attempts: 0,
            result: None,
        };
        record.tasks.push(task.clone());
        Some(task)
    }

    pub fn claim_tasks(&mut self, dweller_id: &str, limit: usize, now: String) -> Vec<DwellerTask> {
        let Some(record) = self.dwellers.get_mut(dweller_id) else {
            return Vec::new();
        };
        let mut claimed = Vec::new();
        for task in record.tasks.iter_mut() {
            if claimed.len() >= limit {
                break;
            }
            if task.status == DwellerTaskStatus::Pending {
                task.status = DwellerTaskStatus::Running;
                task.updated_at = Some(now.clone());
                task.attempts = task.attempts.saturating_add(1);
                claimed.push(task.clone());
            }
        }
        claimed
    }

    pub fn complete_task(&mut self, dweller_id: &str, result: DwellerTaskResult) -> bool {
        let Some(record) = self.dwellers.get_mut(dweller_id) else {
            return false;
        };
        let Some(task) = record
            .tasks
            .iter_mut()
            .find(|task| task.task_id == result.task_id)
        else {
            return false;
        };
        task.status = if result.success {
            DwellerTaskStatus::Completed
        } else {
            DwellerTaskStatus::Failed
        };
        task.updated_at = Some(result.finished_at.clone());
        task.result = Some(result);
        true
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
            callback_servers: Vec::new(),
            parent_path: Vec::new(),
            hibernation: DwellerHibernationConfig::default(),
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
        assert!(record.hibernation.enabled);
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

    #[test]
    fn task_queue_claims_and_completes_tasks() {
        let mut registry = DwellerRegistry::default();
        registry.upsert(DwellerRecord::from_receipt(
            sample_receipt(),
            "secret".to_string(),
        ));
        let task = registry
            .enqueue_task(
                "dweller123",
                DwellerTaskKind::Command {
                    command: "whoami".to_string(),
                },
                "now".to_string(),
            )
            .unwrap();
        let claimed = registry.claim_tasks("dweller123", 10, "later".to_string());
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].task_id, task.task_id);

        assert!(registry.complete_task(
            "dweller123",
            DwellerTaskResult {
                task_id: task.task_id,
                success: true,
                output: "ok".to_string(),
                error: None,
                finished_at: "done".to_string(),
            }
        ));
        let record = registry.dwellers.get("dweller123").unwrap();
        assert_eq!(record.tasks[0].status, DwellerTaskStatus::Completed);
    }

    fn command(cmd: &str) -> DwellerTaskKind {
        DwellerTaskKind::Command {
            command: cmd.to_string(),
        }
    }

    fn seeded() -> DwellerRegistry {
        let mut registry = DwellerRegistry::in_memory();
        registry.upsert(DwellerRecord::from_receipt(
            sample_receipt(),
            "secret".to_string(),
        ));
        registry
    }

    #[test]
    fn in_memory_registry_never_touches_disk() {
        let registry = seeded();
        assert_eq!(registry.storage(), &RegistryStorage::Memory);
        // Saving succeeds without any file-system target at all.
        registry.save().unwrap();
    }

    #[test]
    fn default_registry_persists_to_working_directory_file() {
        assert_eq!(
            DwellerRegistry::default().storage(),
            &RegistryStorage::File(PathBuf::from(DWELLER_REGISTRY_FILE))
        );
    }

    #[test]
    fn load_missing_file_yields_empty_registry_bound_to_that_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dwellers.json");
        let registry = DwellerRegistry::load_from_path(&path).unwrap();
        assert!(registry.dwellers.is_empty());
        assert_eq!(registry.storage(), &RegistryStorage::File(path.clone()));
        registry.save().unwrap();
        assert!(path.exists());
    }

    #[test]
    fn save_is_atomic_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dwellers.json");
        let mut registry = DwellerRegistry::load_from_path(&path).unwrap();
        registry.upsert(DwellerRecord::from_receipt(
            sample_receipt(),
            "secret".to_string(),
        ));
        registry.save().unwrap();
        let entries: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("dwellers.json")]);
        let reloaded = DwellerRegistry::load_from_path(&path).unwrap();
        assert!(reloaded.dwellers.contains_key("dweller123"));
    }

    #[test]
    fn storage_path_is_not_serialized() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dwellers.json");
        seeded().save_to_path(&path).unwrap();
        let body = fs::read_to_string(&path).unwrap();
        assert!(!body.contains("storage"));
    }

    #[test]
    fn load_rejects_corrupt_registry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dwellers.json");
        fs::write(&path, "{ not json").unwrap();
        assert!(matches!(
            DwellerRegistry::load_from_path(&path),
            Err(LabyrinthError::Json(_))
        ));
    }

    #[test]
    fn enqueue_for_unknown_dweller_returns_none() {
        let mut registry = seeded();
        assert!(registry
            .enqueue_task("missing", command("id"), "now".into())
            .is_none());
    }

    #[test]
    fn claim_respects_limit_order_and_only_pending_tasks() {
        let mut registry = seeded();
        let ids: Vec<String> = (0..5)
            .map(|i| {
                registry
                    .enqueue_task("dweller123", command(&format!("cmd{i}")), "t0".into())
                    .unwrap()
                    .task_id
            })
            .collect();

        let first = registry.claim_tasks("dweller123", 2, "t1".into());
        assert_eq!(
            first.iter().map(|t| &t.task_id).collect::<Vec<_>>(),
            vec![&ids[0], &ids[1]]
        );
        assert!(first
            .iter()
            .all(|t| t.status == DwellerTaskStatus::Running && t.attempts == 1));

        // Running tasks are not handed out twice.
        let second = registry.claim_tasks("dweller123", 10, "t2".into());
        assert_eq!(
            second.iter().map(|t| &t.task_id).collect::<Vec<_>>(),
            vec![&ids[2], &ids[3], &ids[4]]
        );
        assert!(registry
            .claim_tasks("dweller123", 10, "t3".into())
            .is_empty());
        assert!(registry.claim_tasks("missing", 10, "t3".into()).is_empty());
        assert!(registry
            .claim_tasks("dweller123", 0, "t3".into())
            .is_empty());
    }

    #[test]
    fn failed_result_marks_task_failed_and_unknown_ids_are_rejected() {
        let mut registry = seeded();
        let task = registry
            .enqueue_task("dweller123", command("false"), "t0".into())
            .unwrap();
        registry.claim_tasks("dweller123", 1, "t1".into());

        let result = |task_id: &str| DwellerTaskResult {
            task_id: task_id.to_string(),
            success: false,
            output: String::new(),
            error: Some("exit 1".into()),
            finished_at: "t2".into(),
        };
        assert!(!registry.complete_task("dweller123", result("nope")));
        assert!(!registry.complete_task("missing", result(&task.task_id)));
        assert!(registry.complete_task("dweller123", result(&task.task_id)));

        let stored = &registry.dwellers["dweller123"].tasks[0];
        assert_eq!(stored.status, DwellerTaskStatus::Failed);
        assert_eq!(stored.updated_at.as_deref(), Some("t2"));
        assert_eq!(
            stored.result.as_ref().unwrap().error.as_deref(),
            Some("exit 1")
        );
    }

    #[test]
    fn remove_forgets_dweller() {
        let mut registry = seeded();
        assert!(registry.remove("dweller123").is_some());
        assert!(registry.remove("dweller123").is_none());
        assert!(registry.list().is_empty());
    }
}
