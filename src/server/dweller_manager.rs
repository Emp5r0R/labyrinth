use crate::error::{LabyrinthError, Result};
use crate::protocol::{AgentInfo, AgentKind, DwellerInstallRequest, Message};
use crate::security::SecurityManager;
use crate::server::agent_manager::AgentManager;
use crate::server::core::LabyrinthServer;
use crate::server::dweller_registry::{DwellerRecord, DwellerRegistry};
use crate::styling;
use colored::Colorize;
use dialoguer::{Input, Select};
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

pub struct DwellerManager;

impl DwellerManager {
    pub async fn load_registry(server: &LabyrinthServer) -> Result<()> {
        let registry = DwellerRegistry::load()?;
        server.set_dweller_registry(registry).await;
        Ok(())
    }

    pub async fn list_dwellers(server: &LabyrinthServer) {
        let registry = server.dweller_registry().read().await;
        if registry.dwellers.is_empty() {
            println!(
                "{}",
                styling::format_warning_msg(styling::WARNING_INDICATOR, "No dwellers remembered")
            );
            return;
        }

        println!(
            "\n{}",
            styling::format_section_title("Remembered Dwellers", "persistent listeners")
        );
        println!("{}", styling::format_separator(styling::SECTION_SEPARATOR));
        let agents = server.agents().read().await;
        for (index, record) in registry.list().iter().enumerate() {
            let online = agents.contains_key(&record.dweller_id);
            println!("Dweller {}", (index + 1).to_string().cyan().bold());
            println!("{}", styling::format_field("ID:", &record.dweller_id));
            println!("{}", styling::format_field("Name:", &record.dweller_name));
            println!(
                "{}",
                styling::format_field("System:", &format!("{}/{}", record.os, record.arch))
            );
            println!(
                "{}",
                styling::format_field("Address:", &record.socket_addr())
            );
            println!(
                "{}",
                styling::format_field("Status:", if online { "Online" } else { "Offline" })
            );
            if index + 1 < registry.dwellers.len() {
                println!(
                    "{}{}",
                    styling::INDENT_LEVEL_1,
                    styling::format_separator(styling::SUBSECTION_SEPARATOR)
                );
            }
        }
        println!();
    }

    pub async fn forget_dweller(server: &LabyrinthServer) -> Result<()> {
        let record = Self::select_record(server, "Forget which dweller?").await?;
        if let Some(record) = record {
            server.forget_dweller_record(&record.dweller_id).await?;
            println!(
                "{}",
                styling::format_success_msg(
                    styling::SUCCESS_INDICATOR,
                    &format!("Forgot dweller {}", record.dweller_name)
                )
            );
        }
        Ok(())
    }

    pub async fn connect_dweller(server: Arc<LabyrinthServer>) -> Result<()> {
        let Some(record) = Self::select_record(&server, "Connect to which dweller?").await? else {
            return Ok(());
        };
        Self::connect_dweller_record(server, record).await
    }

    pub async fn connect_dweller_by_id(
        server: Arc<LabyrinthServer>,
        dweller_id: &str,
    ) -> Result<()> {
        let record = {
            let registry = server.dweller_registry().read().await;
            registry.dwellers.get(dweller_id).cloned()
        }
        .ok_or_else(|| {
            LabyrinthError::Message(format!("No remembered dweller with id {}", dweller_id))
        })?;

        Self::connect_dweller_record(server, record).await
    }

    async fn connect_dweller_record(
        server: Arc<LabyrinthServer>,
        record: DwellerRecord,
    ) -> Result<()> {
        if server
            .agents()
            .read()
            .await
            .contains_key(&record.dweller_id)
        {
            *server.current_agent().write().await = Some(record.dweller_id.clone());
            println!(
                "{}",
                styling::format_success_msg(
                    styling::SUCCESS_INDICATOR,
                    &format!(
                        "Dweller {} is already connected and selected",
                        record.dweller_name
                    )
                )
            );
            return Ok(());
        }

        let config =
            SecurityManager::create_tls_client_config(None, Some(record.fingerprint.clone()))?;
        let connector = TlsConnector::from(Arc::new(config));
        let stream = TcpStream::connect(record.socket_addr())
            .await
            .map_err(LabyrinthError::Io)?;
        let server_name = rustls::pki_types::ServerName::try_from("localhost")?;
        let mut tls_stream = connector
            .connect(server_name, stream)
            .await
            .map_err(LabyrinthError::Io)?;

        let hello = serde_json::to_string(&Message::DwellerHello {
            auth_key: record.auth_key.clone(),
        })?;
        tls_stream
            .write_all(hello.as_bytes())
            .await
            .map_err(LabyrinthError::Io)?;
        tls_stream
            .write_all(b"\n")
            .await
            .map_err(LabyrinthError::Io)?;

        let mut buf = Vec::new();
        let mut reader = tokio::io::BufReader::new(&mut tls_stream);
        reader
            .read_until(b'\n', &mut buf)
            .await
            .map_err(LabyrinthError::Io)?;
        let register: Message = serde_json::from_slice(&buf[..buf.len() - 1])?;
        let agent_info = match register {
            Message::AgentRegister(info) => info,
            other => {
                return Err(LabyrinthError::Message(format!(
                    "Dweller returned unexpected handshake message: {:?}",
                    other
                )))
            }
        };

        Self::validate_dweller_identity(&record, &agent_info)?;
        drop(reader);

        AgentManager::register_live_agent(
            server.clone(),
            tls_stream,
            agent_info,
            record.socket_addr(),
            "tcp/tls".to_string(),
            None,
        )
        .await?;
        *server.current_agent().write().await = Some(record.dweller_id.clone());

        {
            let mut registry = server.dweller_registry().write().await;
            if let Some(existing) = registry.dwellers.get_mut(&record.dweller_id) {
                existing.last_connected = Some(chrono_like_now());
            }
            registry.save()?;
        }

        Ok(())
    }

    pub async fn drop_dweller(server: Arc<LabyrinthServer>) -> Result<()> {
        let current_id = server.current_agent().read().await.clone();
        let Some(agent_id) = current_id else {
            return Err(LabyrinthError::Message(
                "No agent selected. Select a connected generic agent first.".to_string(),
            ));
        };

        let (agent_name, agent_os, agent_sender, command_response, kind) = {
            let agents = server.agents().read().await;
            let agent = agents
                .get(&agent_id)
                .ok_or_else(|| LabyrinthError::Message("Selected agent not found".to_string()))?;
            (
                agent.info.name.clone(),
                agent.info.os.clone(),
                agent.sender.clone(),
                agent.command_response.clone(),
                agent.info.kind.clone(),
            )
        };

        if matches!(kind, AgentKind::Dweller) {
            return Err(LabyrinthError::Message(
                "Drop Dweller must be launched from a connected generic agent session".to_string(),
            ));
        }

        let dweller_name: String = Input::new()
            .with_prompt("Dweller name")
            .default(format!("dweller-{}", agent_name.replace('@', "-")))
            .interact_text()
            .map_err(|e| LabyrinthError::Message(format!("Input error: {}", e)))?;
        let listen_addr: String = Input::new()
            .with_prompt("Dweller listen address")
            .default("0.0.0.0".to_string())
            .interact_text()
            .map_err(|e| LabyrinthError::Message(format!("Input error: {}", e)))?;
        let listen_port: u16 = Input::new()
            .with_prompt("Dweller listen port")
            .default(45454)
            .interact_text()
            .map_err(|e| LabyrinthError::Message(format!("Input error: {}", e)))?;

        let dweller_id = Self::generate_id();
        let auth_key = Self::generate_secret();
        let cert = SecurityManager::generate_self_signed_certificate(&dweller_name)?;
        let request = Self::build_install_request(
            &agent_os,
            dweller_id,
            dweller_name,
            listen_addr,
            listen_port,
            auth_key.clone(),
            cert,
        )?;

        let response =
            Self::send_drop_request(&agent_sender, &command_response, request.clone()).await?;
        let receipt = match response {
            Message::DropDwellerResponse {
                success,
                message,
                receipt,
            } => {
                if !success {
                    return Err(LabyrinthError::Message(message));
                }
                receipt.ok_or_else(|| {
                    LabyrinthError::Message("Dweller install succeeded without receipt".to_string())
                })?
            }
            _ => {
                return Err(LabyrinthError::Message(
                    "Unexpected response type during Drop Dweller".to_string(),
                ))
            }
        };

        server
            .upsert_dweller_record(DwellerRecord::from_receipt(receipt.clone(), auth_key))
            .await?;

        println!(
            "{}",
            styling::format_success_msg(
                styling::SUCCESS_INDICATOR,
                &format!(
                    "Dweller {} dropped and remembered at {}:{}",
                    receipt.dweller_name, receipt.listen_addr, receipt.listen_port
                )
            )
        );
        println!(
            "{}",
            styling::format_hint("Use 'connect-dweller' whenever you want to attach to it.")
        );
        Ok(())
    }

    async fn send_drop_request(
        agent_sender: &tokio::sync::mpsc::Sender<Message>,
        command_response: &Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<Message>>>>,
        request: DwellerInstallRequest,
    ) -> Result<Message> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        *command_response.lock().await = Some(tx);
        agent_sender
            .send(Message::DropDweller { request })
            .await
            .map_err(|e| {
                LabyrinthError::Message(format!("Failed to send dweller drop request: {}", e))
            })?;

        tokio::time::timeout(Duration::from_secs(300), rx)
            .await
            .map_err(|_| {
                LabyrinthError::Message("Timed out waiting for Drop Dweller response".to_string())
            })?
            .map_err(|e| LabyrinthError::Message(format!("Dweller response channel closed: {}", e)))
    }

    async fn select_record(
        server: &LabyrinthServer,
        prompt: &str,
    ) -> Result<Option<DwellerRecord>> {
        let registry = server.dweller_registry().read().await;
        let records = registry.list();
        if records.is_empty() {
            println!(
                "{}",
                styling::format_warning_msg(styling::WARNING_INDICATOR, "No remembered dwellers")
            );
            return Ok(None);
        }

        let items: Vec<String> = records
            .iter()
            .map(|record| {
                format!(
                    "{} ({}) - {}",
                    record.dweller_name,
                    record.dweller_id,
                    record.socket_addr()
                )
            })
            .collect();
        let selection = Select::new()
            .with_prompt(prompt)
            .items(&items)
            .interact()
            .map_err(|e| LabyrinthError::Message(format!("Selection error: {}", e)))?;
        Ok(Some(records[selection].clone()))
    }

    fn build_install_request(
        agent_os: &str,
        dweller_id: String,
        dweller_name: String,
        listen_addr: String,
        listen_port: u16,
        auth_key: String,
        cert: crate::security::GeneratedCertificate,
    ) -> Result<DwellerInstallRequest> {
        let normalized = agent_os.to_lowercase();
        let (install_path, config_dir, service_name) = if normalized.contains("windows") {
            (
                format!(r"C:\ProgramData\Labyrinth\{}.exe", dweller_name),
                format!(r"C:\ProgramData\Labyrinth\{}", dweller_name),
                format!("LabyrinthDweller_{}", &dweller_id[..8]),
            )
        } else if normalized.contains("linux") {
            (
                format!("/usr/local/bin/{}", dweller_name),
                format!("/etc/labyrinth/{}", dweller_name),
                format!("labyrinth-dweller-{}", &dweller_id[..8]),
            )
        } else {
            return Err(LabyrinthError::Message(format!(
                "Drop Dweller is not implemented for remote OS '{}'",
                agent_os
            )));
        };

        Ok(DwellerInstallRequest {
            dweller_id,
            dweller_name,
            listen_addr,
            listen_port,
            auth_key,
            cert_pem: cert.cert_pem,
            key_pem: cert.key_pem,
            install_path,
            config_dir,
            service_name,
        })
    }

    fn validate_dweller_identity(record: &DwellerRecord, info: &AgentInfo) -> Result<()> {
        if !matches!(info.kind, AgentKind::Dweller) {
            return Err(LabyrinthError::Message(
                "Connected endpoint is not a dweller".to_string(),
            ));
        }
        if info.stable_id.as_deref() != Some(record.dweller_id.as_str()) {
            return Err(LabyrinthError::Message(
                "Dweller stable identity mismatch".to_string(),
            ));
        }
        Ok(())
    }

    fn generate_id() -> String {
        thread_rng()
            .sample_iter(Alphanumeric)
            .take(16)
            .map(char::from)
            .collect()
    }

    fn generate_secret() -> String {
        thread_rng()
            .sample_iter(Alphanumeric)
            .take(32)
            .map(char::from)
            .collect()
    }
}

fn chrono_like_now() -> String {
    format!("{:?}", std::time::SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{DwellerInstallReceipt, NetworkInterface};

    fn sample_record() -> DwellerRecord {
        DwellerRecord::from_receipt(
            DwellerInstallReceipt {
                dweller_id: "dweller1234567890".to_string(),
                dweller_name: "alpha".to_string(),
                hostname: "host1".to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
                listen_addr: "10.10.10.10".to_string(),
                listen_port: 45454,
                fingerprint: "abcd".to_string(),
                install_path: "/usr/local/bin/alpha".to_string(),
                config_dir: "/etc/labyrinth/alpha".to_string(),
                service_name: "labyrinth-dweller-alpha".to_string(),
            },
            "secret".to_string(),
        )
    }

    fn sample_info(kind: AgentKind, stable_id: Option<&str>) -> AgentInfo {
        AgentInfo {
            name: "alpha".to_string(),
            hostname: "host1".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            interfaces: vec![NetworkInterface {
                name: "eth0".to_string(),
                addresses: vec!["10.10.10.10/24".to_string()],
                hardware_addr: "00:11:22:33:44:55".to_string(),
                mtu: 1500,
                flags: vec!["UP".to_string()],
            }],
            auth_key: None,
            kind,
            stable_id: stable_id.map(str::to_string),
            listener_addr: Some("10.10.10.10".to_string()),
            listener_port: Some(45454),
        }
    }

    #[test]
    fn build_install_request_for_linux_uses_system_paths() {
        let cert = SecurityManager::generate_self_signed_certificate("alpha").unwrap();
        let request = DwellerManager::build_install_request(
            "linux",
            "abcdef1234567890".to_string(),
            "alpha".to_string(),
            "0.0.0.0".to_string(),
            45454,
            "secret".to_string(),
            cert,
        )
        .unwrap();

        assert_eq!(request.install_path, "/usr/local/bin/alpha");
        assert_eq!(request.config_dir, "/etc/labyrinth/alpha");
        assert!(request.service_name.starts_with("labyrinth-dweller-"));
        assert_eq!(request.listen_port, 45454);
    }

    #[test]
    fn build_install_request_for_windows_uses_programdata_paths() {
        let cert = SecurityManager::generate_self_signed_certificate("alpha").unwrap();
        let request = DwellerManager::build_install_request(
            "windows",
            "abcdef1234567890".to_string(),
            "alpha".to_string(),
            "0.0.0.0".to_string(),
            45454,
            "secret".to_string(),
            cert,
        )
        .unwrap();

        assert_eq!(request.install_path, r"C:\ProgramData\Labyrinth\alpha.exe");
        assert_eq!(request.config_dir, r"C:\ProgramData\Labyrinth\alpha");
        assert!(request.service_name.starts_with("LabyrinthDweller_"));
    }

    #[test]
    fn build_install_request_rejects_unknown_os() {
        let cert = SecurityManager::generate_self_signed_certificate("alpha").unwrap();
        let err = DwellerManager::build_install_request(
            "solaris",
            "abcdef1234567890".to_string(),
            "alpha".to_string(),
            "0.0.0.0".to_string(),
            45454,
            "secret".to_string(),
            cert,
        )
        .unwrap_err();

        assert!(err.to_string().contains("not implemented"));
    }

    #[test]
    fn validate_dweller_identity_accepts_matching_dweller() {
        let record = sample_record();
        let info = sample_info(AgentKind::Dweller, Some("dweller1234567890"));
        assert!(DwellerManager::validate_dweller_identity(&record, &info).is_ok());
    }

    #[test]
    fn validate_dweller_identity_rejects_generic_agent() {
        let record = sample_record();
        let info = sample_info(AgentKind::Generic, Some("dweller1234567890"));
        assert!(DwellerManager::validate_dweller_identity(&record, &info).is_err());
    }

    #[test]
    fn validate_dweller_identity_rejects_id_mismatch() {
        let record = sample_record();
        let info = sample_info(AgentKind::Dweller, Some("different"));
        assert!(DwellerManager::validate_dweller_identity(&record, &info).is_err());
    }

    #[test]
    fn generated_identifiers_have_expected_lengths() {
        let id = DwellerManager::generate_id();
        let secret = DwellerManager::generate_secret();
        assert_eq!(id.len(), 16);
        assert_eq!(secret.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric()));
        assert!(secret.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
