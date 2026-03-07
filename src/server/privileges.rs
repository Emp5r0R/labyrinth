use crate::styling;

/// Single Responsibility: System privilege detection and management
pub struct PrivilegeManager;

impl PrivilegeManager {
    /// Check if the current process has sudo/root privileges
    pub fn has_sudo_privileges() -> bool {
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim() == "0")
            .unwrap_or(false)
    }

    /// Display warning when running without sudo privileges
    pub fn display_sudo_warning() {
        println!(
            "{}",
            styling::format_warning_msg(
                styling::WARNING_INDICATOR,
                "Running without sudo privileges"
            )
        );
        println!("{}Some features may be limited:", styling::INDENT_LEVEL_1);
        println!(
            "{}• Fullhouse mode (TUN interface creation)",
            styling::INDENT_LEVEL_2
        );
        println!(
            "{}• Network interface manipulation",
            styling::INDENT_LEVEL_2
        );
        println!("{}• System-level routing changes", styling::INDENT_LEVEL_2);
        println!(
            "{}Run with 'sudo' for full functionality",
            styling::INDENT_LEVEL_1
        );
        println!();
    }

    /// Check privileges and display warning if needed
    pub fn check_and_warn_privileges() {
        if !Self::has_sudo_privileges() {
            Self::display_sudo_warning();
        }
    }

    /// Create a detailed error message for operations requiring sudo
    #[cfg(target_os = "linux")]
    pub fn create_sudo_error(operation: &str) -> String {
        format!(
            "{} requires sudo privileges.\n\
             TUN interface creation and network operations need root access.\n\
             Please run: sudo labyrinth server",
            operation
        )
    }
}
