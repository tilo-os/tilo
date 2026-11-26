//! Canonical installation-media data, with frontend-only icon presentation.
pub use imager_backend::imager_model::*;

/// Icon presentation is deliberately not part of the native data contract.
pub trait TargetDriveIcon {
    fn icon(&self) -> tilo_ui_base::icons::Icon;
}

impl TargetDriveIcon for TargetDrive {
    fn icon(&self) -> tilo_ui_base::icons::Icon {
        use tilo_ui_base::icons::Icon;
        match self.tran.as_deref() {
            Some("usb") => Icon::Usb,
            Some("mmc" | "sd") => Icon::SdCard,
            _ if self.name.starts_with("mmcblk") => Icon::SdCard,
            _ => Icon::HardDrive,
        }
    }
}
