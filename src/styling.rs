use colored::Colorize;

// Visual separator constants
pub const SECTION_SEPARATOR: &str = "─────────────────";
pub const SUBSECTION_SEPARATOR: &str = "───────────────────────";
pub const _LONG_SEPARATOR: &str = "─────────────────────────────";

// Status indicators - replacing emojis with uniform bracket indicators
pub const SUCCESS_INDICATOR: &str = "[+]";
pub const ERROR_INDICATOR: &str = "[-]";
pub const WARNING_INDICATOR: &str = "[!]";
pub const INFO_INDICATOR: &str = "[i]";
pub const ARROW_INDICATOR: &str = "→";
pub const CHECK_INDICATOR: &str = "✓";
pub const CROSS_INDICATOR: &str = "✗";

// Color scheme constants - removed unused Colors struct

// Helper functions for consistent formatting
pub fn format_header(text: &str) -> String {
    format!("{}", text.cyan().bold())
}

pub fn format_separator(separator: &str) -> String {
    format!("{}", separator.bright_black())
}

pub fn format_success_msg(indicator: &str, message: &str) -> String {
    format!("{} {}", indicator.green(), message)
}

pub fn format_error_msg(indicator: &str, message: &str) -> String {
    format!("{} {}", indicator.red(), message)
}

pub fn format_warning_msg(indicator: &str, message: &str) -> String {
    format!("{} {}", indicator.yellow(), message)
}

pub fn format_field(label: &str, value: &str) -> String {
    format!("{:<20} {}", label, value)
}

pub fn format_section_title(title: &str, subtitle: &str) -> String {
    format!("{} {}", title.cyan().bold(), subtitle.bright_black())
}

pub fn format_status_badge(label: &str, ok: bool) -> String {
    if ok {
        format!("{} {}", CHECK_INDICATOR.green(), label.green())
    } else {
        format!("{} {}", CROSS_INDICATOR.red(), label.red())
    }
}

pub fn format_hint(text: &str) -> String {
    format!("{} {}", INFO_INDICATOR.blue(), text.bright_black())
}

pub fn format_arrow_mapping(from: &str, to: &str) -> String {
    format!("{} {} {}", from.yellow(), ARROW_INDICATOR.cyan(), to.cyan())
}

pub fn format_check_item(item: &str) -> String {
    format!("  {} {}", CHECK_INDICATOR.green(), item)
}

pub fn format_cross_item(item: &str) -> String {
    format!("  {} {}", CROSS_INDICATOR.red(), item)
}

pub fn format_numbered_item(number: usize, name: &str, detail: &str) -> String {
    format!(
        "[{}]: {} ({})",
        number.to_string().cyan().bold(),
        name.green(),
        detail.bright_black()
    )
}

// Consistent indentation patterns
pub const INDENT_LEVEL_1: &str = "  ";
pub const INDENT_LEVEL_2: &str = "    ";

// Logo and branding
pub fn format_logo() -> String {
    let logo = r#"
 )   _ ( _        _ o  _  _)_ ( _  
(__ (_( )_) (_(  )  ( ) ) (_   ) ) 
              _)
"#;
    format!(
        "{}\n{}",
        logo.yellow(),
        "                 by Emp5r0R".bright_black()
    )
}

pub fn format_welcome_header() -> String {
    format!(
        "{} {}",
        SUCCESS_INDICATOR.green(),
        "Labyrinth Control Interface".cyan().bold()
    )
}

pub fn format_welcome_subtitle() -> String {
    format!(
        "{}",
        "Navigate the network maze with precision".bright_black()
    )
}

// Command prompt formatting
pub fn format_prompt(agent_name: Option<&str>) -> String {
    match agent_name {
        Some(name) => format!("labyrinth ({}) {} ", name.cyan(), ARROW_INDICATOR.cyan()),
        None => format!("labyrinth {} ", ARROW_INDICATOR.cyan()),
    }
}

// Status formatting helpers
pub fn format_status_active(text: &str) -> colored::ColoredString {
    text.green()
}

pub fn format_status_inactive(text: &str) -> colored::ColoredString {
    text.red()
}

pub fn format_agent_id(id: &str) -> colored::ColoredString {
    id.yellow()
}

pub fn format_agent_name(name: &str) -> colored::ColoredString {
    name.cyan()
}

pub fn format_system_info(info: &str) -> colored::ColoredString {
    info.bright_white()
}

pub fn format_network_address(addr: &str) -> colored::ColoredString {
    addr.blue()
}
