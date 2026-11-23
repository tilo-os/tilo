//! The computer-name form.
use super::*;

impl InstallerPreview {
    pub(super) fn device_name_form(&self) -> ui::widget::Column {
        page::related().push(Box::new(self.field(
            "device-name",
            &self.journey.device_name,
            TextInputKind::Name,
        )))
    }
}

#[cfg(test)]
#[path = "devices_layout_tests.rs"]
mod tests;
