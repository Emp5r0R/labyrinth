//! Interactive prompt primitives shared by every server menu.
//!
//! Every menu in the interactive server goes through this module so that
//! navigation is consistent: `Esc`, `q`, or `Ctrl-C` always mean "go back one
//! level" and never terminate the server process.

use crate::error::{LabyrinthError, Result};
use dialoguer::console::Term;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, Select};
use std::io;
use std::sync::OnceLock;

/// A selectable menu entry with a short label and a dimmed description.
pub struct MenuItem<T> {
    pub label: &'static str,
    pub detail: String,
    pub value: T,
}

impl<T> MenuItem<T> {
    pub fn new(label: &'static str, detail: impl Into<String>, value: T) -> Self {
        Self {
            label,
            detail: detail.into(),
            value,
        }
    }
}

fn theme() -> &'static ColorfulTheme {
    static THEME: OnceLock<ColorfulTheme> = OnceLock::new();
    THEME.get_or_init(ColorfulTheme::default)
}

/// `console` raises SIGINT when Ctrl-C is pressed inside a dialoguer prompt.
/// Installing a no-op handler keeps that keystroke from killing the server so
/// prompts can treat it as "back" instead. Only the interactive server calls
/// this; headless mode keeps the default Ctrl-C behavior.
pub fn install_interrupt_guard() {
    tokio::spawn(async { while tokio::signal::ctrl_c().await.is_ok() {} });
}

/// Renders `items` as an aligned two-column list and returns the chosen value,
/// or `None` when the operator backs out.
pub fn select<T: Copy>(prompt: &str, items: &[MenuItem<T>]) -> Result<Option<T>> {
    let width = items.iter().map(|item| item.label.len()).max().unwrap_or(0);
    let rendered: Vec<String> = items
        .iter()
        .map(|item| {
            if item.detail.is_empty() {
                item.label.to_string()
            } else {
                format!(
                    "{:<width$}   {}",
                    item.label,
                    dim(&item.detail),
                    width = width
                )
            }
        })
        .collect();

    let selection = handle_cancel(
        Select::with_theme(theme())
            .with_prompt(prompt_with_hint(prompt))
            .items(&rendered)
            .default(0)
            .interact_opt(),
    )?
    .flatten();

    Ok(selection.map(|idx| items[idx].value))
}

/// Plain list selection for dynamic labels such as agent names.
pub fn select_index(prompt: &str, labels: &[String]) -> Result<Option<usize>> {
    Ok(handle_cancel(
        Select::with_theme(theme())
            .with_prompt(prompt_with_hint(prompt))
            .items(labels)
            .default(0)
            .interact_opt(),
    )?
    .flatten())
}

/// Text input. An empty answer or Ctrl-C returns `None`.
pub fn input(prompt: &str) -> Result<Option<String>> {
    let value: Option<String> = handle_cancel(
        Input::<String>::with_theme(theme())
            .with_prompt(prompt)
            .allow_empty(true)
            .interact_text(),
    )?;
    Ok(value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty()))
}

/// Yes/no confirmation. Esc, `q`, or Ctrl-C count as "no".
pub fn confirm(prompt: &str, default: bool) -> Result<bool> {
    Ok(handle_cancel(
        Confirm::with_theme(theme())
            .with_prompt(prompt)
            .default(default)
            .interact_opt(),
    )?
    .flatten()
    .unwrap_or(false))
}

fn prompt_with_hint(prompt: &str) -> String {
    format!("{}  {}", prompt, dim("(Esc to go back)"))
}

fn dim(text: &str) -> String {
    dialoguer::console::style(text).dim().to_string()
}

/// Maps a Ctrl-C interruption to `Ok(None)` and restores the cursor that
/// dialoguer hides while a prompt is active.
fn handle_cancel<T>(result: dialoguer::Result<T>) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(dialoguer::Error::IO(err)) if err.kind() == io::ErrorKind::Interrupted => {
            let term = Term::stderr();
            let _ = term.show_cursor();
            let _ = term.write_line("");
            Ok(None)
        }
        Err(err) => Err(LabyrinthError::Message(format!("Prompt error: {}", err))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_prompt_is_treated_as_back() {
        let result: dialoguer::Result<usize> = Err(dialoguer::Error::IO(io::Error::new(
            io::ErrorKind::Interrupted,
            "read interrupted",
        )));
        assert!(matches!(handle_cancel(result), Ok(None)));
    }

    #[test]
    fn other_prompt_errors_are_reported() {
        let result: dialoguer::Result<usize> = Err(dialoguer::Error::IO(io::Error::new(
            io::ErrorKind::NotConnected,
            "not a terminal",
        )));
        assert!(handle_cancel(result).is_err());
    }

    #[test]
    fn successful_prompt_passes_value_through() {
        assert!(matches!(handle_cancel(Ok(3usize)), Ok(Some(3))));
    }
}
