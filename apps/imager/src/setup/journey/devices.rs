//! The computer-name form. Enrollment-approved names remain read-only.
use super::*;

impl InstallerPreview {
    pub(super) fn device_name_form(&self) -> ui::widget::Column {
        if self.device_label_locked() {
            page::related()
                .push(Box::new(
                    Text::new(&self.journey.device_name).with_id("installer.devices.approved-name"),
                ))
                .push(Box::new(
                    Text::new(&t(self.device_label_locked_message()))
                        .with_id("installer.devices.name-locked"),
                ))
        } else {
            page::related().push(Box::new(self.field(
                "device-name",
                &self.journey.device_name,
                TextInputKind::Name,
            )))
        }
    }
}

#[cfg(test)]
#[path = "devices_layout_tests.rs"]
mod tests;
