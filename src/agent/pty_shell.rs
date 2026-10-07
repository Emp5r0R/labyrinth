use crate::agent::reverse_port_forward::get_response_channel;
use crate::error::{LabyrinthError, Result};
use crate::protocol::Message;
use base64::{engine::general_purpose, Engine as _};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

struct PtySession {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn portable_pty::Child + Send>>,
}

struct StartedPtySession {
    session: Arc<PtySession>,
    reader: Box<dyn Read + Send>,
}

const MAX_SESSION_ID_BYTES: usize = 128;
const MAX_PTY_INPUT_BYTES: usize = 64 * 1024;
const MAX_PTY_DIMENSION: u16 = 500;
const PTY_IO_TIMEOUT: Duration = Duration::from_secs(5);

fn sessions() -> &'static Mutex<HashMap<String, Arc<PtySession>>> {
    static SESSIONS: OnceLock<Mutex<HashMap<String, Arc<PtySession>>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_sessions() -> MutexGuard<'static, HashMap<String, Arc<PtySession>>> {
    match sessions().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= MAX_SESSION_ID_BYTES
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn valid_pty_size(cols: u16, rows: u16) -> bool {
    (1..=MAX_PTY_DIMENSION).contains(&cols) && (1..=MAX_PTY_DIMENSION).contains(&rows)
}

pub struct PtyShellManager;

impl PtyShellManager {
    pub async fn start_session(session_id: String, cols: u16, rows: u16) -> Result<()> {
        if !valid_session_id(&session_id) {
            return Err(LabyrinthError::Message(
                "Invalid PTY session id".to_string(),
            ));
        }
        if !valid_pty_size(cols, rows) {
            return Err(LabyrinthError::Message(format!(
                "Invalid PTY size: {}x{} (allowed range: 1..={})",
                cols, rows, MAX_PTY_DIMENSION
            )));
        }

        let startup = tokio::task::spawn_blocking(move || Self::start_session_blocking(cols, rows));
        let started = tokio::time::timeout(Duration::from_secs(15), startup)
            .await
            .map_err(|_| LabyrinthError::Message("Timed out while starting PTY shell".to_string()))?
            .map_err(|e| LabyrinthError::Message(format!("PTY startup task failed: {}", e)))??;

        {
            let mut registry = lock_sessions();
            if registry.contains_key(&session_id) {
                let _ = terminate_session(&started.session);
                return Err(LabyrinthError::Message(format!(
                    "PTY session already exists: {}",
                    session_id
                )));
            }
            registry.insert(session_id.clone(), Arc::clone(&started.session));
        }

        let (tx, _rx) = get_response_channel();
        if let Err(error) = tx
            .send(Message::ShellSessionStarted {
                session_id: session_id.clone(),
                success: true,
                message: "Interactive PTY session ready".to_string(),
            })
            .await
        {
            Self::close_session(&session_id).await?;
            return Err(LabyrinthError::Message(format!(
                "Failed to report shell session start: {}",
                error
            )));
        }

        Self::spawn_reader(session_id, started.session, started.reader);
        Ok(())
    }

    fn start_session_blocking(cols: u16, rows: u16) -> Result<StartedPtySession> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| LabyrinthError::Message(format!("Failed to create PTY: {}", e)))?;

        let mut cmd = default_shell_command();
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| LabyrinthError::Message(format!("Failed to spawn PTY shell: {}", e)))?;

        let reader = match pair.master.try_clone_reader() {
            Ok(reader) => reader,
            Err(error) => {
                let _ = terminate_child(child.as_mut());
                return Err(LabyrinthError::Message(format!(
                    "Failed to clone PTY reader: {}",
                    error
                )));
            }
        };
        let writer = match pair.master.take_writer() {
            Ok(writer) => writer,
            Err(error) => {
                let _ = terminate_child(child.as_mut());
                return Err(LabyrinthError::Message(format!(
                    "Failed to acquire PTY writer: {}",
                    error
                )));
            }
        };

        let session = Arc::new(PtySession {
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            child: Mutex::new(child),
        });

        Ok(StartedPtySession { session, reader })
    }

    fn spawn_reader(
        session_id: String,
        session: Arc<PtySession>,
        mut reader: Box<dyn Read + Send>,
    ) {
        let shell_session_id = session_id.clone();
        std::thread::spawn(move || {
            let (tx, _rx) = get_response_channel();
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        let _ = tx.try_send(Message::ShellSessionClose {
                            session_id: shell_session_id.clone(),
                        });
                        break;
                    }
                    Ok(n) => {
                        // Never block PTY reader on a full protocol queue. Dropping output is
                        // preferable to backpressuring child process until it deadlocks.
                        let _ = tx.try_send(Message::ShellSessionOutput {
                            session_id: shell_session_id.clone(),
                            data_b64: general_purpose::STANDARD.encode(&buf[..n]),
                        });
                    }
                    Err(error) => {
                        let _ = tx.try_send(Message::ShellSessionClose {
                            session_id: shell_session_id.clone(),
                        });
                        let _ = error;
                        break;
                    }
                }
            }

            remove_and_terminate(&shell_session_id, &session);
        });
    }

    pub async fn send_input(session_id: &str, data_b64: &str) -> Result<()> {
        if !valid_session_id(session_id) {
            return Err(LabyrinthError::Message(
                "Invalid PTY session id".to_string(),
            ));
        }
        let max_encoded = MAX_PTY_INPUT_BYTES.div_ceil(3) * 4;
        if data_b64.len() > max_encoded {
            return Err(LabyrinthError::Message(format!(
                "PTY input exceeds {} bytes",
                MAX_PTY_INPUT_BYTES
            )));
        }
        let data = general_purpose::STANDARD.decode(data_b64.as_bytes())?;
        if data.len() > MAX_PTY_INPUT_BYTES {
            return Err(LabyrinthError::Message(format!(
                "PTY input exceeds {} bytes",
                MAX_PTY_INPUT_BYTES
            )));
        }
        let session = lock_sessions().get(session_id).cloned().ok_or_else(|| {
            LabyrinthError::Message(format!("Unknown shell session: {}", session_id))
        })?;

        let write_task = tokio::task::spawn_blocking(move || {
            let mut writer = match session.writer.lock() {
                Ok(writer) => writer,
                Err(poisoned) => poisoned.into_inner(),
            };
            writer.write_all(&data).map_err(LabyrinthError::Io)?;
            writer.flush().map_err(LabyrinthError::Io)
        });

        match tokio::time::timeout(PTY_IO_TIMEOUT, write_task).await {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => Err(LabyrinthError::Message(format!(
                "PTY input task failed: {}",
                error
            ))),
            Err(_) => {
                Self::close_session(session_id).await?;
                Err(LabyrinthError::Message(
                    "Timed out writing PTY input; session closed".to_string(),
                ))
            }
        }
    }

    pub async fn resize_session(session_id: &str, cols: u16, rows: u16) -> Result<()> {
        if !valid_session_id(session_id) {
            return Err(LabyrinthError::Message(
                "Invalid PTY session id".to_string(),
            ));
        }
        if !valid_pty_size(cols, rows) {
            return Err(LabyrinthError::Message(format!(
                "Invalid PTY size: {}x{} (allowed range: 1..={})",
                cols, rows, MAX_PTY_DIMENSION
            )));
        }
        let session = lock_sessions().get(session_id).cloned().ok_or_else(|| {
            LabyrinthError::Message(format!("Unknown shell session: {}", session_id))
        })?;
        let resize_task = tokio::task::spawn_blocking(move || {
            let master = match session.master.lock() {
                Ok(master) => master,
                Err(poisoned) => poisoned.into_inner(),
            };
            master
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| LabyrinthError::Message(format!("Failed to resize PTY: {}", e)))
        });

        match tokio::time::timeout(PTY_IO_TIMEOUT, resize_task).await {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => Err(LabyrinthError::Message(format!(
                "PTY resize task failed: {}",
                error
            ))),
            Err(_) => Err(LabyrinthError::Message(
                "Timed out resizing PTY".to_string(),
            )),
        }
    }

    pub async fn close_session(session_id: &str) -> Result<()> {
        if !valid_session_id(session_id) {
            return Err(LabyrinthError::Message(
                "Invalid PTY session id".to_string(),
            ));
        }
        if let Some(session) = lock_sessions().remove(session_id) {
            terminate_session(&session)?;
        }
        Ok(())
    }

    /// Close every session when agent connection or runtime shuts down.
    pub async fn close_all() -> Result<()> {
        let sessions = {
            let mut registry = lock_sessions();
            registry
                .drain()
                .map(|(_, session)| session)
                .collect::<Vec<_>>()
        };
        let mut first_error = None;
        for session in sessions {
            if let Err(error) = terminate_session(&session) {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

fn remove_and_terminate(session_id: &str, expected: &Arc<PtySession>) {
    let removed = {
        let mut registry = match sessions().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if registry
            .get(session_id)
            .is_some_and(|session| Arc::ptr_eq(session, expected))
        {
            registry.remove(session_id)
        } else {
            None
        }
    };
    if let Some(session) = removed {
        let _ = terminate_session(&session);
    }
}

fn terminate_session(session: &Arc<PtySession>) -> Result<()> {
    let mut child = match session.child.lock() {
        Ok(child) => child,
        Err(poisoned) => poisoned.into_inner(),
    };
    terminate_child(child.as_mut())
}

fn terminate_child(child: &mut (dyn portable_pty::Child + Send)) -> Result<()> {
    match child.try_wait() {
        Ok(Some(_)) => return Ok(()),
        Ok(None) => {}
        Err(_) => {
            // Child may have exited between calls; best-effort kill/reap keeps close idempotent.
        }
    }
    if let Err(kill_error) = child.kill() {
        if child.try_wait().ok().flatten().is_some() {
            return Ok(());
        }
        return Err(LabyrinthError::Message(format!(
            "Failed to kill PTY shell: {}",
            kill_error
        )));
    }
    child
        .wait()
        .map_err(|e| LabyrinthError::Message(format!("Failed to reap PTY shell: {}", e)))?;
    Ok(())
}

fn default_shell_command() -> CommandBuilder {
    #[cfg(target_os = "windows")]
    {
        if command_exists("pwsh.exe") {
            let mut cmd = CommandBuilder::new("pwsh.exe");
            cmd.args(["-NoLogo", "-NoProfile"]);
            return cmd;
        }

        if command_exists("powershell.exe") {
            let mut cmd = CommandBuilder::new("powershell.exe");
            cmd.args(["-NoLogo", "-NoProfile"]);
            return cmd;
        }

        CommandBuilder::new(std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string()))
    }

    #[cfg(not(target_os = "windows"))]
    {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|candidate| {
                let path = Path::new(candidate);
                path.is_absolute()
                    && path
                        .metadata()
                        .map(|metadata| metadata.is_file())
                        .unwrap_or(false)
            })
            .unwrap_or_else(|| "/bin/sh".to_string());
        CommandBuilder::new(shell)
    }
}

#[cfg(target_os = "windows")]
fn command_exists(command: &str) -> bool {
    std::process::Command::new("where")
        .arg(command)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(all(test, not(target_os = "windows")))]
mod tests {
    use super::*;

    #[test]
    fn default_shell_command_uses_shell_environment() {
        std::env::set_var("SHELL", "/bin/sh");
        let cmd = default_shell_command();
        assert_eq!(cmd.get_argv()[0].to_string_lossy(), "/bin/sh");
    }

    #[test]
    fn session_validation_rejects_unsafe_identifiers_and_sizes() {
        assert!(!valid_session_id(""));
        assert!(!valid_session_id("session/id"));
        assert!(!valid_session_id(&"x".repeat(MAX_SESSION_ID_BYTES + 1)));
        assert!(valid_session_id("session-1:worker"));
        assert!(!valid_pty_size(0, 24));
        assert!(!valid_pty_size(80, MAX_PTY_DIMENSION + 1));
        assert!(valid_pty_size(80, 24));
    }

    #[cfg(unix)]
    #[test]
    fn started_session_can_be_terminated_and_reaped() {
        let started = PtyShellManager::start_session_blocking(80, 24)
            .expect("PTY shell should start in test environment");
        terminate_session(&started.session).expect("PTY shell should terminate cleanly");
        // Close remains idempotent after process already reaped.
        terminate_session(&started.session).expect("PTY shell termination should be idempotent");
        drop(started.reader);
    }

    #[tokio::test]
    async fn oversized_input_rejected_before_session_lookup() {
        let oversized = vec![b'x'; MAX_PTY_INPUT_BYTES + 1];
        let encoded = general_purpose::STANDARD.encode(oversized);
        let error = PtyShellManager::send_input("missing", &encoded)
            .await
            .expect_err("oversized input must be rejected");
        assert!(error.to_string().contains("exceeds"));
    }
}
