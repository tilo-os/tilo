//! Device-local migration status and canonical retry; no profile data is published.
use super::{BrowserOperation, BrowserWidget};
use ui::tilo_resource::Effect as _;

use crate::resource::CATALOG;

impl BrowserWidget {
    /// Grant the continuity controller its profile and state directories,
    /// as the picker would. Fails when this window has no controller (no
    /// live canonical person attached), so a test says why instead of
    /// panicking inside the surface.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn continuity_grant_for_test(
        &mut self,
        root: std::fs::File,
        state: tilo_files::native::atomic::StorageDirectory,
    ) -> Result<(), &'static str> {
        self.continuity
            .as_mut()
            .ok_or("no live canonical person attachment")?
            .load_granted(root, state);
        self.structure_dirty = true;
        Ok(())
    }
    pub(crate) fn attach_continuity(
        &mut self,
        person: &str,
        files: ui::storage::AppFiles,
        waker: ui::WakeHandle,
    ) {
        if !crate::continuity_handoff::supported() {
            return;
        }
        let Ok(identity) = blake3::Hash::from_hex(person) else {
            return;
        };
        if identity.to_hex().as_str() != person
            || self.person_bookmarks.is_none()
            || self.person_history.is_none()
        {
            return;
        }
        self.continuity = Some(crate::continuity_handoff::Controller::start(
            *identity.as_bytes(),
            files,
            waker,
        ));
    }
    pub(super) fn poll_continuity(&mut self) {
        if let (Some(controller), Some(bookmarks), Some(history)) = (
            &mut self.continuity,
            &self.person_bookmarks,
            &self.person_history,
        ) {
            self.structure_dirty |= controller.poll(bookmarks, history);
        }
    }
    pub(super) fn continuity_operation(
        &mut self,
        operation: &BrowserOperation,
    ) -> Option<Result<ui::framework::Effected, String>> {
        if !matches!(
            operation,
            BrowserOperation::ReadContinuity | BrowserOperation::RetryContinuity
        ) {
            return None;
        }
        let result = (|| {
            if !crate::continuity_handoff::supported() {
                return Err("Browser migration is unavailable on this host".into());
            }
            let controller = self
                .continuity
                .as_mut()
                .ok_or("No browser migration is available")?;
            if matches!(operation, BrowserOperation::RetryContinuity) && !controller.retry() {
                return Err("Browser migration is already running".into());
            }
            Ok(ui::framework::Effected::Done(
                ui::tilo_actions::ActionOutput {
                    value: Some(serde_json::json!({"state":controller.status()})),
                    resources: Vec::new(),
                },
            ))
        })();
        Some(result)
    }
    pub(super) fn continuity_notice(&self) -> Option<ui::widget::Element> {
        {
            if self.browsing_mode.is_private()
                || self.person_bookmarks.is_none()
                || self.person_history.is_none()
                || self.continuity.as_ref()?.status() != crate::continuity_handoff::Status::Failed
            {
                return None;
            }
            let button = ui::widget::Button::new(
                "browser.continuity.retry",
                &CATALOG.t("browser-continuity-retry"),
            );
            let button = match self.unit_binding("browser.continuity", "retry-continuity") {
                Some(binding) => button.on_action(binding),
                None => button.with_enabled(false),
            };
            Some(Box::new(
                ui::widget::Banner::error(CATALOG.t("browser-continuity-failed"))
                    .with_id("browser.continuity.failure")
                    .with_action(button),
            ))
        }
    }

    pub(super) fn continuity_resource(&self) -> Option<ui::tilo_actions::Resource> {
        if let Some(controller) = &self.continuity {
            use crate::continuity_handoff::Status;
            if controller.status() == Status::Absent {
                return None;
            }
            let mut resource = ui::tilo_actions::Resource::new(
                "browser.continuity",
                "browser.continuity",
                "Bring your browsing",
            )
            .with_action(crate::effects::BrowserEffect::ReadContinuity.bound());
            if controller.status() == Status::Failed {
                resource =
                    resource.with_action(crate::effects::BrowserEffect::RetryContinuity.bound());
            }
            return Some(resource);
        }
        None
    }
}
