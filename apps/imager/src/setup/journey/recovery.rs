//! Recovery controls use the same semantic effects as automation.
#[cfg(all(test, target_os = "linux"))]
mod tests;
use super::{InstallerPreview, InstallerPreviewEffect, Text, TextRole, t};
pub(super) fn is_recovery_action(id: &str) -> bool {
    matches!(
        id,
        "choose-recovery-destination"
            | "select-recovery-directory"
            | "review-recovery-destination"
            | "review-existing-recovery-copy"
            | "start-recovery-copy"
            | "retry-recovery-copy"
            | "cancel-recovery-copy"
    )
}
pub(super) fn is_recovery_effect(effect: &InstallerPreviewEffect) -> bool {
    matches!(
        effect,
        InstallerPreviewEffect::ChooseRecoveryDestination { .. }
            | InstallerPreviewEffect::SelectRecoveryDirectory { .. }
            | InstallerPreviewEffect::ReviewRecoveryDestination { .. }
            | InstallerPreviewEffect::ReviewExistingRecoveryCopy { .. }
            | InstallerPreviewEffect::StartRecoveryCopy
            | InstallerPreviewEffect::RetryRecoveryCopy
            | InstallerPreviewEffect::CancelRecoveryCopy
    )
}
impl InstallerPreview {
    pub(super) fn recovery_admitted(&self) -> bool {
        self.live
            && (self.windows.enabled
                || matches!(
                    self.journey.page,
                    super::Page::Belongings | super::Page::Storage | super::Page::Review
                ))
            && !self.windows.busy()
    }
    pub(super) fn recovery_content(&self) -> ui::widget::Column {
        let mut content = crate::page::stack();
        let Some(inventory) = &self.belongings.windows else {
            return content;
        };
        if !self.live {
            return content;
        }
        content = content
            .push(Box::new(Text::new(&t("installer-recovery-heading"))))
            .push(Box::new(
                Text::new(&t("installer-recovery-retain-source")).with_role(TextRole::Caption),
            ));
        for root in &inventory.roots {
            content = content
                .push(Box::new(Text::new(&root.path.to_string_lossy())))
                .push(Box::new(
                    self.button(
                        &format!("installer.recovery.destination.{}", root.source_id),
                        "installer-recovery-choose-directory",
                        InstallerPreviewEffect::ChooseRecoveryDestination {
                            source_id: root.source_id.clone(),
                        },
                    )
                    .with_enabled(!self.recovery.busy()),
                ));
        }
        let report = self.recovery.report();
        if report["durable_copy_available"] == false {
            content = content.push(Box::new(Text::new(&t(
                "installer-recovery-native-unavailable",
            ))));
        }
        let state = report["state"].as_str().unwrap_or("not-selected");
        content = content.push(Box::new(
            Text::new(&t(&format!("installer-recovery-state-{state}")))
                .with_id("installer.recovery.state"),
        ));
        if let Some(output) = report["reviewed"]["output"].as_str() {
            content = content.push(Box::new(
                Text::new(output).with_id("installer.recovery.output"),
            ));
        }
        if report["reviewed"]["independent_storage"] == false {
            content = content.push(Box::new(
                Text::new(&t("installer-recovery-not-independent")).with_role(TextRole::Caption),
            ));
        }
        if !report["error"].is_null() {
            content = content.push(Box::new(Text::new(&t("installer-recovery-review-failed"))));
        }
        let effect = if state == "reviewed" {
            InstallerPreviewEffect::StartRecoveryCopy
        } else {
            InstallerPreviewEffect::RetryRecoveryCopy
        };
        if !report["reviewed"].is_null() {
            content = content
                .push(Box::new(
                    self.button(
                        "installer.recovery.copy",
                        if state == "reviewed" {
                            "installer-recovery-copy"
                        } else {
                            "installer-recovery-retry"
                        },
                        effect,
                    )
                    .with_enabled(!self.recovery.busy()),
                ))
                .push(Box::new(
                    self.button(
                        "installer.recovery.cancel",
                        "installer-recovery-cancel",
                        InstallerPreviewEffect::CancelRecoveryCopy,
                    )
                    .with_enabled(self.recovery.busy()),
                ));
        }
        content
    }
}
