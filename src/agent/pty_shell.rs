use crate::agent::reverse_port_forward::get_response_channel;
use crate::error::{LabyrinthError, Result};
use crate::protocol::Message;
use base64::{engine::general_purpose, Engine as _};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Mutex, OnceLock};

struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn portable_pty::Child + Send>>,
}

fn sessions() -> &'static tokio::sync::Mutex<HashMap<String, PtySession>> {
    static SESSIONS: OnceLock<tokio::sync::Mutex<HashMap<String, PtySession>>> = OnceLock::new();
    SESSIONS.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

pub struct PtyShellManager;

impl PtyShellManager {
    pub async fn start_session(session_id: String, cols: u16, rows: u16) -> Result<()> {
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
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| LabyrinthError::Message(format!("Failed to spawn PTY shell: {}", e)))?;

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| LabyrinthError::Message(format!("Failed to clone PTY reader: {}", e)))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| LabyrinthError::Message(format!("Failed to acquire PTY writer: {}", e)))?;

        let session = PtySession {
            master: pair.master,
            writer: Mutex::new(writer),
            child: Mutex::new(child),
        };

        sessions().lock().await.insert(session_id.clone(), session);

        let (tx, _rx) = get_response_channel();
        let _ = tx
            .send(Message::ShellSessionStarted {
                session_id: session_id.clone(),
                success: true,
                message: "Interactive PTY session ready".to_string(),
            })
            .await;

        let shell_session_id = session_id.clone();
        std::thread::spawn(move || {
            let (tx, _rx) = get_response_channel();
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        let _ = tx.blocking_send(Message::ShellSessionClose {
                            session_id: shell_session_id.clone(),
                        });
                        break;
                    }
                    Ok(n) => {
                        let _ = tx.blocking_send(Message::ShellSessionOutput {
                            session_id: shell_session_id.clone(),
                            data_b64: general_purpose::STANDARD.encode(&buf[..n]),
                        });
                    }
                    Err(_) => {
                        let _ = tx.blocking_send(Message::ShellSessionClose {
                            session_id: shell_session_id.clone(),
                        });
                        break;
                    }
                }
            }
        });

        Ok(())
    }

    pub async fn send_input(session_id: &str, data_b64: &str) -> Result<()> {
        let data = general_purpose::STANDARD.decode(data_b64.as_bytes())?;
        let mut sessions_guard = sessions().lock().await;
        let session = sessions_guard.get_mut(session_id).ok_or_else(|| {
            LabyrinthError::Message(format!("Unknown shell session: {}", session_id))
        })?;

        let mut writer = session
            .writer
            .lock()
            .map_err(|_| LabyrinthError::Message("Failed to lock PTY writer".to_string()))?;
        writer.write_all(&data)?;
        writer.flush()?;
        Ok(())
    }

    pub async fn resize_session(session_id: &str, cols: u16, rows: u16) -> Result<()> {
        let mut sessions_guard = sessions().lock().await;
        let session = sessions_guard.get_mut(session_id).ok_or_else(|| {
            LabyrinthError::Message(format!("Unknown shell session: {}", session_id))
        })?;
        session
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| LabyrinthError::Message(format!("Failed to resize PTY: {}", e)))?;
        Ok(())
    }

    pub async fn close_session(session_id: &str) -> Result<()> {
        if let Some(session) = sessions().lock().await.remove(session_id) {
            let mut child = session
                .child
                .lock()
                .map_err(|_| LabyrinthError::Message("Failed to lock PTY child".to_string()))?;
            child
                .kill()
                .map_err(|e| LabyrinthError::Message(format!("Failed to kill PTY shell: {}", e)))?;
        }
        Ok(())
    }
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
        CommandBuilder::new(std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string()))
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
}
