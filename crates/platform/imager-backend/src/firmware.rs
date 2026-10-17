//! Firmware observation and the compile-time native test-override gate.

use crate::{efi, platform::Platform};

/// Firmware facts of the running host, as the plan's basis records them.
/// # Errors
/// Returns an error if firmware variables cannot be read or decoded.
pub fn read_firmware(
    platform: &dyn Platform,
) -> super::platform::IoResult<super::plan::FirmwareFacts> {
    if !platform.has_uefi() {
        return Ok(super::plan::FirmwareFacts {
            uefi: false,
            secure_boot: false,
            boot_current: None,
            boot_order: Vec::new(),
            boot_next: None,
        });
    }
    let number = |bytes: Option<Vec<u8>>| -> Result<Option<String>, String> {
        match bytes {
            None => Ok(None),
            Some(b) => {
                let list = efi::parse_u16_list(&b)?;
                match list.as_slice() {
                    [n] => Ok(Some(efi::format_boot_number(*n))),
                    _ => Err("boot number variable is not one u16".into()),
                }
            }
        }
    };
    Ok(super::plan::FirmwareFacts {
        uefi: true,
        secure_boot: platform
            .get_efi_var("SecureBoot")?
            .is_some_and(|b| b.first() == Some(&1)),
        boot_current: number(platform.get_efi_var("BootCurrent")?)?,
        boot_order: efi::parse_u16_list(&platform.get_efi_var("BootOrder")?.unwrap_or_default())?
            .into_iter()
            .map(efi::format_boot_number)
            .collect(),
        boot_next: number(platform.get_efi_var("BootNext")?)?,
    })
}

/// Test-only host overrides (loop disk, fake efivarfs, record directory)
/// for the Linux loop-device and VM harnesses. Honoured only in debug builds
/// or builds compiled with `TILO_IMAGER_STAGING_TEST_OVERRIDES=1`.
pub fn test_overrides_allowed() -> bool {
    cfg!(debug_assertions) || option_env!("TILO_IMAGER_STAGING_TEST_OVERRIDES") == Some("1")
}
