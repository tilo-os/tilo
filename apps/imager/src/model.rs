//! Canonical installation-media data, with frontend-only icon presentation.
pub use imager_backend::imager_model::*;

#[cfg(test)]
#[path = "model/compatibility_tests.rs"]
mod compatibility_tests;

/// Icon presentation is deliberately not part of the native data contract.
pub trait TargetDriveIcon {
    fn icon(&self) -> ui::icons::Icon;
}

impl TargetDriveIcon for TargetDrive {
    fn icon(&self) -> ui::icons::Icon {
        use ui::icons::Icon;
        match self.tran.as_deref() {
            Some("usb") => Icon::Usb,
            Some("mmc" | "sd") => Icon::SdCard,
            _ if self.name.starts_with("mmcblk") => Icon::SdCard,
            _ => Icon::HardDrive,
        }
    }
}
