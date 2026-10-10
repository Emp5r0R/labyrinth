//! Catalog of top-level interactive server commands.
//!
//! The help screen and tab completion are both derived from this table, so a
//! new command only needs to be added here and in the dispatcher.

use crate::styling;
use colored::Colorize;

pub struct CommandSpec {
    /// Usage shown in help, e.g. `plan <ip|cidr>`.
    pub usage: &'static str,
    pub description: &'static str,
    /// Words offered by tab completion (primary name first, then aliases).
    pub names: &'static [&'static str],
}

pub struct CommandGroup {
    pub title: &'static str,
    pub commands: &'static [CommandSpec],
}

const fn cmd(
    usage: &'static str,
    description: &'static str,
    names: &'static [&'static str],
) -> CommandSpec {
    CommandSpec {
        usage,
        description,
        names,
    }
}

pub const COMMAND_GROUPS: &[CommandGroup] = &[
    CommandGroup {
        title: "Agents",
        commands: &[
            cmd("agents", "List connected agents", &["agents", "list", "ls"]),
            cmd("select", "Choose the agent to operate on", &["select"]),
            cmd(
                "info",
                "Show details for the selected agent",
                &["info", "show"],
            ),
            cmd(
                "commands",
                "Open the operator menu (shell, checks, files)",
                &["commands", "cmd"],
            ),
            cmd("upload", "Upload a file to the selected agent", &["upload"]),
            cmd(
                "download",
                "Download a file from the selected agent",
                &["download"],
            ),
            cmd(
                "bloodhound",
                "Run BloodHound collection (Windows agents)",
                &["bloodhound"],
            ),
        ],
    },
    CommandGroup {
        title: "Pivoting",
        commands: &[
            cmd(
                "tunnel",
                "Start an Ariadne tunnel via the selected agent",
                &["tunnel", "ariadne"],
            ),
            cmd(
                "portal",
                "Start Portal reverse port forwarding",
                &["portal", "forward"],
            ),
            cmd("stop", "Stop the active tunnel or forwarding", &["stop"]),
            cmd(
                "plan <ip|cidr>",
                "Preview a smart route to a target (read-only)",
                &["plan"],
            ),
            cmd(
                "access <ip|cidr>",
                "Plan and apply smart access after confirmation",
                &["access"],
            ),
            cmd(
                "chain [status|doctor]",
                "Show chain state or diagnose reachability",
                &["chain"],
            ),
            cmd(
                "topology",
                "Show routes and shared networks",
                &["topology", "routes"],
            ),
            cmd("map", "Show the network map", &["map", "network-map"]),
        ],
    },
    CommandGroup {
        title: "Dwellers",
        commands: &[
            cmd("dwellers", "List remembered dwellers", &["dwellers"]),
            cmd(
                "connect-dweller",
                "Connect to a remembered dweller",
                &["connect-dweller"],
            ),
            cmd(
                "drop-dweller",
                "Drop a dweller via the selected agent",
                &["drop-dweller"],
            ),
            cmd(
                "configure-dweller",
                "Change a dweller's callback settings",
                &["configure-dweller"],
            ),
            cmd(
                "task-dweller",
                "Queue a task for a hibernating dweller",
                &["task-dweller"],
            ),
            cmd(
                "dweller-tasks",
                "Show queued dweller tasks and results",
                &["dweller-tasks"],
            ),
            cmd(
                "forget-dweller",
                "Forget a remembered dweller",
                &["forget-dweller"],
            ),
        ],
    },
    CommandGroup {
        title: "Server",
        commands: &[
            cmd("status", "Show server status", &["status"]),
            cmd(
                "cert",
                "Show the certificate fingerprint",
                &["cert", "certificate"],
            ),
            cmd("help", "Show this help", &["help", "h"]),
            cmd("exit", "Quit Labyrinth", &["exit", "quit", "q"]),
        ],
    },
];

/// Every word tab completion should offer at the main prompt.
pub fn completion_words() -> Vec<&'static str> {
    COMMAND_GROUPS
        .iter()
        .flat_map(|group| group.commands.iter())
        .flat_map(|command| command.names.iter().copied())
        .collect()
}

pub fn render_help() -> String {
    let width = COMMAND_GROUPS
        .iter()
        .flat_map(|group| group.commands.iter())
        .map(|command| command.usage.len())
        .max()
        .unwrap_or(0);

    let mut out = String::new();
    out.push_str(&format!("\n{}\n", styling::format_header("Commands")));
    for group in COMMAND_GROUPS {
        out.push_str(&format!("\n  {}\n", group.title.yellow().bold()));
        for command in group.commands {
            out.push_str(&format!(
                "    {}  {}\n",
                format!("{:<width$}", command.usage, width = width).cyan(),
                command.description
            ));
        }
    }
    out.push_str(&format!(
        "\n{}\n",
        styling::format_hint(
            "Menus: arrows/j/k to move, Enter to pick, Esc or q to go back. Ctrl-C never quits Labyrinth."
        )
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn completion_words_are_unique() {
        let words = completion_words();
        let unique: HashSet<_> = words.iter().collect();
        assert_eq!(words.len(), unique.len());
    }

    #[test]
    fn every_command_has_a_completion_name() {
        for group in COMMAND_GROUPS {
            for command in group.commands {
                assert!(!command.names.is_empty(), "{} has no names", command.usage);
                assert!(
                    command.usage.starts_with(command.names[0]),
                    "{} should start with its primary name",
                    command.usage
                );
            }
        }
    }

    #[test]
    fn help_lists_every_group() {
        let help = render_help();
        for group in COMMAND_GROUPS {
            assert!(help.contains(group.title));
        }
    }
}
