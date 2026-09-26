//! Native host architecture for selecting bootable installation media.

/// An emulated Imager must select media for the host, not for its own binary.
/// Detection errors are propagated rather than falling back to the build target.
#[cfg(target_os = "windows")]
pub(crate) fn architecture() -> Result<Option<&'static str>, String> {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn IsWow64Process2(
            process: *mut c_void,
            process_machine: *mut u16,
            native_machine: *mut u16,
        ) -> i32;
    }

    let mut process_machine = 0_u16;
    let mut native_machine = 0_u16;
    // SAFETY: the pseudo-handle is valid for this process; both output pointers
    // refer to live, writable USHORTs for the duration of this synchronous call.
    // IsWow64Process2's native-machine output stays ARM64 even for an x64 app
    // running under Windows emulation (Windows 10 version 1709 and later).
    let detected = unsafe {
        IsWow64Process2(
            GetCurrentProcess(),
            &raw mut process_machine,
            &raw mut native_machine,
        )
    };
    if detected == 0 {
        return Err(format!(
            "Could not detect the Windows host architecture: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(windows_catalog_architecture(native_machine))
}

#[cfg(not(target_os = "windows"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "same interface as fallible Windows native-architecture detection"
)]
pub(crate) fn architecture() -> Result<Option<&'static str>, String> {
    Ok(match std::env::consts::ARCH {
        "x86_64" => Some("x86_64"),
        "aarch64" => Some("aarch64"),
        _ => None,
    })
}

#[cfg(any(target_os = "windows", test))]
fn windows_catalog_architecture(native_machine: u16) -> Option<&'static str> {
    // IMAGE_FILE_MACHINE_AMD64 and IMAGE_FILE_MACHINE_ARM64 from winnt.h.
    match native_machine {
        0x8664 => Some("x86_64"),
        0xaa64 => Some("aarch64"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Host architecture detection and mapping.
    #[test]
    fn host_architecture_selects_only_matching_catalog_entries() {
        // Phase `windows_native_machine_selects_the_matching_catalog_architecture`.
        {
            assert_eq!(windows_catalog_architecture(0xaa64), Some("aarch64"));
            assert_eq!(windows_catalog_architecture(0x8664), Some("x86_64"));
        }

        // Phase `unsupported_windows_machines_never_fall_back_to_x86_64`.
        {
            // Unknown, 32-bit x86, 32-bit ARM, and ARM64EC are not native targets
            // in the release catalog. Do not guess which disk image can boot them.
            for native_machine in [0, 0x014c, 0x01c4, 0xa641, 0xffff] {
                assert_eq!(windows_catalog_architecture(native_machine), None);
            }
        }

        // Phase `live_host_architecture_is_detectable`.
        {
            assert!(architecture().is_ok());
        }
    }
}
