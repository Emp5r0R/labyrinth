use crate::error::{LabyrinthError, Result};
use serde::{Deserialize, Serialize};
use std::process::Command;

/// Operating System enumeration for command execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum OperatingSystem {
    Linux,
    Windows,
    Unknown,
}

/// Command executor enum following SOLID principles for extensibility
#[derive(Debug, Clone)]
pub enum CommandExecutor {
    Linux,
    Windows,
    Unknown,
}

impl CommandExecutor {
    pub fn new(os: &OperatingSystem) -> Self {
        match os {
            OperatingSystem::Linux => CommandExecutor::Linux,
            OperatingSystem::Windows => CommandExecutor::Windows,
            OperatingSystem::Unknown => CommandExecutor::Unknown,
        }
    }

    #[allow(dead_code)]
    pub fn get_available_commands(&self) -> Vec<&'static str> {
        match self {
            CommandExecutor::Linux => vec!["ifconfig", "ss -tunlp"],
            CommandExecutor::Windows => vec!["ipconfig", "netstat -aon"],
            CommandExecutor::Unknown => vec![],
        }
    }

    pub async fn execute_command(&self, command: &str) -> Result<String> {
        match self {
            CommandExecutor::Linux => self.execute_linux_command(command).await,
            CommandExecutor::Windows => self.execute_windows_command(command).await,
            CommandExecutor::Unknown => Err(LabyrinthError::Message(
                "Command execution not supported on this operating system".to_string(),
            )),
        }
    }

    #[allow(dead_code)]
    pub fn get_os(&self) -> OperatingSystem {
        match self {
            CommandExecutor::Linux => OperatingSystem::Linux,
            CommandExecutor::Windows => OperatingSystem::Windows,
            CommandExecutor::Unknown => OperatingSystem::Unknown,
        }
    }

    async fn execute_linux_command(&self, command: &str) -> Result<String> {
        match command {
            "ifconfig" => self.execute_ifconfig().await,
            "ss -tunlp" => self.execute_ss().await,
            _ => Err(LabyrinthError::Message(format!(
                "Unsupported Linux command: {}",
                command
            ))),
        }
    }

    async fn execute_windows_command(&self, command: &str) -> Result<String> {
        match command {
            "ipconfig" => self.execute_ipconfig().await,
            "netstat -aon" => self.execute_netstat().await,
            _ => Err(LabyrinthError::Message(format!(
                "Unsupported Windows command: {}",
                command
            ))),
        }
    }

    async fn execute_ifconfig(&self) -> Result<String> {
        let output = Command::new("ifconfig")
            .output()
            .map_err(|e| LabyrinthError::Message(format!("Failed to execute ifconfig: {}", e)))?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            let error = String::from_utf8_lossy(&output.stderr).to_string();
            Err(LabyrinthError::Message(format!(
                "ifconfig failed: {}",
                error
            )))
        }
    }

    async fn execute_ss(&self) -> Result<String> {
        let output = Command::new("ss")
            .args(["-tunlp"]) 
            .output()
            .map_err(|e| LabyrinthError::Message(format!("Failed to execute ss: {}", e)))?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            let error = String::from_utf8_lossy(&output.stderr).to_string();
            Err(LabyrinthError::Message(format!("ss failed: {}", error)))
        }
    }

    async fn execute_ipconfig(&self) -> Result<String> {
        let output = Command::new("ipconfig")
            .output()
            .map_err(|e| LabyrinthError::Message(format!("Failed to execute ipconfig: {}", e)))?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            let error = String::from_utf8_lossy(&output.stderr).to_string();
            Err(LabyrinthError::Message(format!(
                "ipconfig failed: {}",
                error
            )))
        }
    }

    async fn execute_netstat(&self) -> Result<String> {
        let output = Command::new("netstat")
            .args(["-aon"]) 
            .output()
            .map_err(|e| LabyrinthError::Message(format!("Failed to execute netstat: {}", e)))?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            let error = String::from_utf8_lossy(&output.stderr).to_string();
            Err(LabyrinthError::Message(format!(
                "netstat failed: {}",
                error
            )))
        }
    }
}

/// OS detection utility
pub struct OSDetector;

impl OSDetector {
    pub fn detect_os() -> OperatingSystem {
        if cfg!(target_os = "linux") {
            OperatingSystem::Linux
        } else if cfg!(target_os = "windows") {
            OperatingSystem::Windows
        } else {
            OperatingSystem::Unknown
        }
    }
}

/// Output formatter for beautiful command results
pub struct OutputFormatter;

impl OutputFormatter {
    pub fn format_command_output(command: &str, output: &str, os: &OperatingSystem) -> String {
        let separator = "═".repeat(60);
        let os_name = match os {
            OperatingSystem::Linux => "Linux",
            OperatingSystem::Windows => "Windows",
            OperatingSystem::Unknown => "Unknown",
        };

        format!(
            "╔{}\n║ Command: {}\n║ OS: {}\n╠{}\n{}\n╚{}",
            separator,
            command,
            os_name,
            separator,
            Self::indent_output(output),
            separator
        )
    }

    pub fn format_command_error(command: &str, error: &str, os: &OperatingSystem) -> String {
        let separator = "═".repeat(60);
        let os_name = match os {
            OperatingSystem::Linux => "Linux",
            OperatingSystem::Windows => "Windows",
            OperatingSystem::Unknown => "Unknown",
        };

        format!(
            "╔{}\n║ Command: {} (FAILED)\n║ OS: {}\n╠{}\n║ ERROR:\n{}\n╚{}",
            separator,
            command,
            os_name,
            separator,
            Self::indent_output(error),
            separator
        )
    }

    fn indent_output(output: &str) -> String {
        output
            .lines()
            .map(|line| format!("║ {}", line))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
