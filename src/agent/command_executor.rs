use crate::error::{LabyrinthError, Result};
use base64::{engine::general_purpose, Engine as _};
use serde::{Deserialize, Serialize};
use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum OperatingSystem {
    Linux,
    Windows,
    Unknown,
}

#[derive(Debug, Clone)]
pub enum CommandExecutor {
    Linux,
    Windows,
    Unknown,
}

#[derive(Debug, Clone)]
struct CommandResult {
    name: String,
    command: String,
    success: bool,
    output: String,
    error: String,
}

const MAX_LINES: usize = 80;
const MAX_CHARS: usize = 8000;

impl CommandExecutor {
    pub fn new(os: &OperatingSystem) -> Self {
        match os {
            OperatingSystem::Linux => Self::Linux,
            OperatingSystem::Windows => Self::Windows,
            OperatingSystem::Unknown => Self::Unknown,
        }
    }

    pub async fn execute_command(&self, command: &str) -> Result<String> {
        match self {
            Self::Linux => self.execute_linux_command(command).await,
            Self::Windows => self.execute_windows_command(command).await,
            Self::Unknown => Err(LabyrinthError::Message(
                "Command execution not supported on this operating system".to_string(),
            )),
        }
    }

    async fn execute_linux_command(&self, command: &str) -> Result<String> {
        if let Some(encoded) = command.strip_prefix("linux:shell_raw:") {
            return self.run_linux_shell_raw(encoded);
        }

        match command {
            "ifconfig" | "linux:ifconfig" => self.run_single_linux("ifconfig", "ifconfig"),
            "ss -tunlp" | "linux:ss" => {
                self.run_with_fallback_linux("Socket overview", "ss -tunlp", &["netstat -anp"])
            }
            "linux:whoami" => self.run_single_linux("whoami", "whoami"),
            "linux:route" => self.run_with_fallback_linux("Route table", "route -n", &["ip route"]),
            "linux:resolvectl" => self.run_with_fallback_linux(
                "Resolver status",
                "resolvectl status",
                &["cat /etc/resolv.conf"],
            ),
            "linux:sysenum" => {
                let results = vec![
                    self.run_linux("Distribution", "cat /etc/issue"),
                    self.run_linux("OS release", "cat /etc/os-release"),
                    self.run_linux("Kernel full", "uname -a"),
                    self.run_linux("Kernel version", "uname -r"),
                    self.run_linux("Architecture", "arch"),
                    self.run_linux("Hostname", "hostname"),
                    self.run_linux("Current identity", "id"),
                    self.run_linux("Shell users", "cat /etc/passwd | grep sh$"),
                ];
                Ok(OutputFormatter::format_batch_result(
                    "Linux system enumeration",
                    &OperatingSystem::Linux,
                    &results,
                ))
            }
            "linux:network_summary" => {
                let socket_info = self.run_linux("Socket overview", "ss -tunlp");
                let socket_info = if socket_info.success {
                    socket_info
                } else {
                    self.run_linux("Socket overview fallback", "netstat -anp")
                };

                let route_info = self.run_linux("Route table", "route -n");
                let route_info = if route_info.success {
                    route_info
                } else {
                    self.run_linux("Route table fallback", "ip route")
                };

                let resolver = self.run_linux("Resolver status", "resolvectl status");
                let resolver = if resolver.success {
                    resolver
                } else {
                    self.run_linux("Resolver fallback", "cat /etc/resolv.conf")
                };

                let results = vec![
                    self.run_linux("Interfaces", "ifconfig"),
                    socket_info,
                    route_info,
                    resolver,
                ];

                Ok(OutputFormatter::format_batch_result(
                    "Linux network overview",
                    &OperatingSystem::Linux,
                    &results,
                ))
            }
            "linux:privesc_placeholder" => Ok(OutputFormatter::format_placeholder(
                "Linux privilege escalation",
                &OperatingSystem::Linux,
                "Scaffold only. No checks executed yet.",
            )),
            "linux:autoenum" => self.run_linux_autoenum(),
            _ => Err(LabyrinthError::Message(format!(
                "Unsupported Linux command: {}",
                command
            ))),
        }
    }

    async fn execute_windows_command(&self, command: &str) -> Result<String> {
        if let Some(encoded) = command.strip_prefix("windows:shell_raw:") {
            return self.run_windows_shell_raw(encoded);
        }

        match command {
            "ipconfig" | "windows:ipconfig_all" => {
                self.run_single_windows_cmd("ipconfig /all", "ipconfig /all")
            }
            "netstat -aon" | "windows:netstat_ano" => {
                self.run_single_windows_cmd("netstat -ano", "netstat -ano")
            }
            "windows:whoami_all" => self.run_single_windows_cmd("whoami /all", "whoami /all"),
            "windows:route_print" => self.run_single_windows_cmd("route print", "route print"),
            "windows:sysenum" => {
                let results = vec![
                    self.run_windows_cmd("System info", "systeminfo"),
                    self.run_windows_powershell(
                        "Local users",
                        "Get-LocalUser | Format-Table -AutoSize",
                    ),
                    self.run_windows_cmd("Local users fallback", "net user"),
                    self.run_windows_powershell(
                        "Admin check",
                        "([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)",
                    ),
                ];

                Ok(OutputFormatter::format_batch_result(
                    "Windows system enumeration",
                    &OperatingSystem::Windows,
                    &results,
                ))
            }
            "windows:network_summary" => {
                let results = vec![
                    self.run_windows_cmd("IP configuration", "ipconfig /all"),
                    self.run_windows_cmd("Route table", "route print"),
                    self.run_windows_cmd("Socket overview", "netstat -ano"),
                ];

                Ok(OutputFormatter::format_batch_result(
                    "Windows network overview",
                    &OperatingSystem::Windows,
                    &results,
                ))
            }
            "windows:privesc_placeholder" => Ok(OutputFormatter::format_placeholder(
                "Windows privilege escalation",
                &OperatingSystem::Windows,
                "Scaffold only. No checks executed yet.",
            )),
            "windows:autoenum" => self.run_windows_autoenum(),
            _ => Err(LabyrinthError::Message(format!(
                "Unsupported Windows command: {}",
                command
            ))),
        }
    }

    fn run_single_linux(&self, name: &str, command: &str) -> Result<String> {
        let result = self.run_linux(name, command);
        Ok(OutputFormatter::format_batch_result(
            name,
            &OperatingSystem::Linux,
            &[result],
        ))
    }

    fn run_single_windows_cmd(&self, name: &str, command: &str) -> Result<String> {
        let result = self.run_windows_cmd(name, command);
        Ok(OutputFormatter::format_batch_result(
            name,
            &OperatingSystem::Windows,
            &[result],
        ))
    }

    fn run_with_fallback_linux(
        &self,
        name: &str,
        primary: &str,
        fallback: &[&str],
    ) -> Result<String> {
        let mut result = self.run_linux(name, primary);
        if !result.success {
            for fb in fallback {
                let fb_result = self.run_linux(&format!("{} fallback", name), fb);
                if fb_result.success {
                    result = fb_result;
                    break;
                }
            }
        }

        Ok(OutputFormatter::format_batch_result(
            name,
            &OperatingSystem::Linux,
            &[result],
        ))
    }

    fn run_linux(&self, name: &str, command: &str) -> CommandResult {
        run_process(
            name,
            command,
            Command::new("sh").args(["-c", command]).output(),
        )
    }

    fn run_windows_cmd(&self, name: &str, command: &str) -> CommandResult {
        run_process(
            name,
            command,
            Command::new("cmd").args(["/C", command]).output(),
        )
    }

    fn run_windows_powershell(&self, name: &str, command: &str) -> CommandResult {
        run_process(
            name,
            command,
            Command::new("powershell")
                .args(["-NoProfile", "-Command", command])
                .output(),
        )
    }

    fn run_linux_shell_raw(&self, encoded: &str) -> Result<String> {
        let decoded = general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .map_err(|e| {
                LabyrinthError::Message(format!("Invalid encoded shell command: {}", e))
            })?;
        let cmd = String::from_utf8(decoded)
            .map_err(|e| LabyrinthError::Message(format!("Invalid UTF-8 shell command: {}", e)))?;

        // Try to allocate a pseudo-tty via `script` for prompt-heavy tools (mysql, python, etc.).
        let quoted = single_quote_for_sh(&cmd);
        let wrapped = format!(
            "if command -v script >/dev/null 2>&1; then script -qec '{}' /dev/null; else sh -lc '{}'; fi",
            quoted, quoted
        );

        let out = Command::new("sh")
            .args(["-lc", &wrapped])
            .output()
            .map_err(|e| {
                LabyrinthError::Message(format!("Failed to execute shell command: {}", e))
            })?;

        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        Ok(merge_shell_streams(&stdout, &stderr))
    }

    fn run_windows_shell_raw(&self, encoded: &str) -> Result<String> {
        let decoded = general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .map_err(|e| {
                LabyrinthError::Message(format!("Invalid encoded shell command: {}", e))
            })?;
        let cmd = String::from_utf8(decoded)
            .map_err(|e| LabyrinthError::Message(format!("Invalid UTF-8 shell command: {}", e)))?;

        let out = Command::new("powershell")
            .args(["-NoProfile", "-Command", &cmd])
            .output()
            .map_err(|e| {
                LabyrinthError::Message(format!("Failed to execute shell command: {}", e))
            })?;

        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        Ok(merge_shell_streams(&stdout, &stderr))
    }

    fn run_linux_autoenum(&self) -> Result<String> {
        let ts = unix_ts();
        let output_path = format!("/tmp/labyrinth_autoenum_linux_{}.log", ts);
        let fallback_path = "/tmp/labyrinth_linpeas_fallback.sh";

        let (runner, source) = if file_exists("/usr/share/peass/linpeas/linpeas.sh") {
            (
                "sh /usr/share/peass/linpeas/linpeas.sh",
                "system peass: /usr/share/peass/linpeas/linpeas.sh",
            )
        } else if file_exists("/usr/share/peass/linpeas/linpeas_small.sh") {
            (
                "sh /usr/share/peass/linpeas/linpeas_small.sh",
                "system peass: /usr/share/peass/linpeas/linpeas_small.sh",
            )
        } else {
            fs::write(
                fallback_path,
                include_str!("../../assets/peas/linpeas_fallback.sh"),
            )
            .map_err(|e| {
                LabyrinthError::Message(format!("Failed to write linpeas fallback script: {}", e))
            })?;

            let chmod_status = Command::new("sh")
                .args(["-c", &format!("chmod 700 {}", fallback_path)])
                .output();
            if let Ok(status) = chmod_status {
                if !status.status.success() {
                    return Err(LabyrinthError::Message(
                        "Failed to mark linpeas fallback script executable".to_string(),
                    ));
                }
            }

            (
                "sh /tmp/labyrinth_linpeas_fallback.sh",
                "bundled fallback: assets/peas/linpeas_fallback.sh",
            )
        };

        let cmd = format!("{} > '{}' 2>&1", runner, output_path);
        let result = run_process(
            "AutoEnum (Linux)",
            &cmd,
            Command::new("sh").args(["-c", &cmd]).output(),
        );

        let preview = summarize_file_preview(&output_path, 120, 50000);
        let details = format!(
            "Source: {}\nRemote output file: {}\n\nPreview:\n{}",
            source,
            output_path,
            preview.unwrap_or_else(|| "No output preview available".to_string())
        );

        Ok(OutputFormatter::format_batch_result(
            "Linux AutoEnum (linpeas)",
            &OperatingSystem::Linux,
            &[CommandResult {
                name: "AutoEnum run".to_string(),
                command: cmd,
                success: result.success,
                output: details,
                error: result.error,
            }],
        ))
    }

    fn run_windows_autoenum(&self) -> Result<String> {
        let ts = unix_ts();
        let output_path = format!("$env:TEMP\\labyrinth_autoenum_windows_{}.log", ts);
        let fallback_path = "$env:TEMP\\labyrinth_winpeas_fallback.ps1";

        let mut script = String::new();
        script.push_str("$ErrorActionPreference='Continue'; ");
        script.push_str(&format!("$Out='{}'; ", output_path));
        script.push_str("$Source=''; ");
        script.push_str("$Candidates=@('C:\\ProgramData\\winPEASx64.exe','C:\\ProgramData\\winPEASany.exe','C:\\Tools\\winPEASx64.exe','C:\\Tools\\winPEASany.exe'); ");
        script.push_str(
            "$Peas=$Candidates | Where-Object { Test-Path $_ } | Select-Object -First 1; ",
        );
        script.push_str("if ($Peas) { $Source = \"system peass: $Peas\"; & $Peas *>&1 | Out-File -FilePath $Out -Encoding utf8; } ");
        script.push_str("else { ");
        script.push_str(&format!(
            "$Fallback='{}'; @'{}'@ | Out-File -FilePath $Fallback -Encoding utf8; ",
            fallback_path,
            include_str!("../../assets/peas/winpeas_fallback.ps1")
        ));
        script.push_str("$Source='bundled fallback: assets/peas/winpeas_fallback.ps1'; powershell -NoProfile -ExecutionPolicy Bypass -File $Fallback *>&1 | Out-File -FilePath $Out -Encoding utf8; }");
        script.push_str("Write-Output \"SOURCE:$Source\"; Write-Output \"OUTFILE:$Out\";");

        let launcher = run_process(
            "AutoEnum (Windows)",
            "powershell -NoProfile -Command <autoenum>",
            Command::new("powershell")
                .args(["-NoProfile", "-Command", &script])
                .output(),
        );

        let source = extract_tag_line(&launcher.output, "SOURCE:")
            .unwrap_or_else(|| "unknown source".to_string());
        let outfile = extract_tag_line(&launcher.output, "OUTFILE:")
            .unwrap_or_else(|| "%TEMP%\\labyrinth_autoenum_windows.log".to_string());

        let details = format!(
            "Source: {}\nRemote output file: {}\n\nNote: full output is stored remotely.\n",
            source, outfile
        );

        Ok(OutputFormatter::format_batch_result(
            "Windows AutoEnum (winpeas)",
            &OperatingSystem::Windows,
            &[CommandResult {
                name: "AutoEnum run".to_string(),
                command: "powershell -NoProfile -Command <autoenum>".to_string(),
                success: launcher.success,
                output: details,
                error: launcher.error,
            }],
        ))
    }
}

fn run_process(
    name: &str,
    command: &str,
    output: std::io::Result<std::process::Output>,
) -> CommandResult {
    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            CommandResult {
                name: name.to_string(),
                command: command.to_string(),
                success: out.status.success(),
                output: OutputFormatter::truncate(&stdout),
                error: OutputFormatter::truncate(&stderr),
            }
        }
        Err(e) => CommandResult {
            name: name.to_string(),
            command: command.to_string(),
            success: false,
            output: String::new(),
            error: format!("Failed to execute: {}", e),
        },
    }
}

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

pub struct OutputFormatter;

impl OutputFormatter {
    fn format_placeholder(title: &str, os: &OperatingSystem, message: &str) -> String {
        format!(
            "=== {} ===\nOS: {}\nSummary: Placeholder command\n\n{}",
            title,
            Self::os_name(os),
            message
        )
    }

    fn format_batch_result(title: &str, os: &OperatingSystem, results: &[CommandResult]) -> String {
        let total = results.len();
        let ok = results.iter().filter(|r| r.success).count();
        let failed = total.saturating_sub(ok);

        let mut out = String::new();
        out.push_str(&format!("=== {} ===\n", title));
        out.push_str(&format!("OS: {}\n", Self::os_name(os)));
        out.push_str(&format!("Summary: {} succeeded, {} failed\n\n", ok, failed));

        if failed > 0 {
            out.push_str("Failures:\n");
            for r in results.iter().filter(|r| !r.success) {
                out.push_str(&format!(
                    "- {} (`{}`): {}\n",
                    r.name,
                    r.command,
                    first_line(&r.error)
                ));
            }
            out.push('\n');
        }

        out.push_str("Details:\n");
        for r in results {
            out.push_str(&format!(
                "\n[{}] {}\nCommand: {}\n",
                if r.success { "OK" } else { "FAIL" },
                r.name,
                r.command
            ));

            if !r.output.trim().is_empty() {
                out.push_str("Output:\n");
                out.push_str(&r.output);
                out.push('\n');
            }

            if !r.error.trim().is_empty() {
                out.push_str("Error:\n");
                out.push_str(&r.error);
                out.push('\n');
            }
        }

        out
    }

    fn os_name(os: &OperatingSystem) -> &'static str {
        match os {
            OperatingSystem::Linux => "Linux",
            OperatingSystem::Windows => "Windows",
            OperatingSystem::Unknown => "Unknown",
        }
    }

    fn truncate(s: &str) -> String {
        let mut lines: Vec<&str> = s.lines().take(MAX_LINES).collect();
        let mut joined = lines.join("\n");
        if joined.len() > MAX_CHARS {
            joined.truncate(MAX_CHARS);
            joined.push_str("\n...[truncated]");
            return joined;
        }

        if s.lines().count() > MAX_LINES {
            lines.push("...[truncated]");
            return lines.join("\n");
        }

        joined
    }
}

fn first_line(input: &str) -> String {
    input.lines().next().unwrap_or("unknown error").to_string()
}

fn unix_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn file_exists(path: &str) -> bool {
    fs::metadata(path).is_ok()
}

fn summarize_file_preview(path: &str, max_lines: usize, max_chars: usize) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    let mut collected = Vec::new();
    for line in content.lines().take(max_lines) {
        collected.push(line);
    }
    let mut out = collected.join("\n");
    if out.len() > max_chars {
        out.truncate(max_chars);
        out.push_str("\n...[truncated]");
    } else if content.lines().count() > max_lines {
        out.push_str("\n...[truncated]");
    }
    Some(out)
}

fn extract_tag_line(content: &str, prefix: &str) -> Option<String> {
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix(prefix) {
            return Some(rest.trim().to_string());
        }
    }
    None
}

fn merge_shell_streams(stdout: &str, stderr: &str) -> String {
    match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
        (false, false) => format!("{}\n{}", stdout.trim_end(), stderr.trim_end()),
        (false, true) => stdout.trim_end().to_string(),
        (true, false) => stderr.trim_end().to_string(),
        (true, true) => String::new(),
    }
}

fn single_quote_for_sh(input: &str) -> String {
    input.replace('\'', "'\"'\"'")
}
