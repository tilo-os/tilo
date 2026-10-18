//! Early host compatibility uses the canonical native setup surface, without installation custody.
use super::{Destination, InstallerPreview, Page};
use crate::setup::view::{FullOsCheck, SetupApp};

#[derive(Default)]
pub(super) struct Compatibility {
    pub pending: bool,
    result: Option<ui::LocalState<Option<FullOsCheck>>>,
}

impl InstallerPreview {
    pub(super) fn drain_compatibility(&mut self) -> bool {
        if std::mem::take(&mut self.compatibility.pending) {
            if let Err(error) = self.prepare_compatibility() {
                self.error = Some(error);
                self.dirty = true;
            }
            return true;
        }
        let Some(result) = self
            .compatibility
            .result
            .as_ref()
            .and_then(|result| result.get())
        else {
            return false;
        };
        self.compatibility = Compatibility::default();
        self.direct.active = false;
        self.composed = None;
        // These are display facts, not a grant to write. The final native
        // review and privileged helper retain their independent revalidation.
        self.storage.accept(result.facts);
        let gib = crate::setup::internal_install_flow::target_extent_bytes(&result.plan) >> 30;
        if let Err(error) = self.storage.set_size(gib) {
            self.error = Some(error);
        } else if let Err(error) = self.advance_journey() {
            self.error = Some(error);
        }
        self.dirty = true;
        true
    }

    fn prepare_compatibility(&mut self) -> Result<(), &'static str> {
        if !self.live
            || !self.linux
            || self.windows.enabled
            || self.journey.page != Page::Destination
            || self.journey.destination != Some(Destination::Computer)
            || self.direct.active
            || self.personal_busy()
            || self.migration_busy()
        {
            return Err("installer-review-installation-unavailable");
        }
        let result = ui::LocalState::new(None);
        self.show_desktop(SetupApp::for_full_os_check(result.clone()))?;
        self.compatibility.result = Some(result);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::InstallerPreviewEffect;
    use super::*;

    #[test]
    fn full_os_check_is_deferred_before_identity_and_not_used_for_another_computer() {
        let mut preview = InstallerPreview {
            live: true,
            linux: true,
            ..Default::default()
        };
        preview.capabilities.prepared_installation = true;
        preview.journey.destination = Some(Destination::Computer);
        preview
            .apply(InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert!(preview.compatibility.pending);
        assert_eq!(preview.journey.page, Page::Destination);
        assert!(preview.journey.identity.is_none());
        assert!(preview.journey.password.is_empty());
        assert!(!preview.personal.busy());
        assert!(preview.direct.prepared_belongings.is_none());
        assert!(!preview.direct.active);

        let mut another = InstallerPreview {
            live: true,
            linux: true,
            ..Default::default()
        };
        another.capabilities.prepared_installation = true;
        another.journey.destination = Some(Destination::Another);
        another
            .apply(InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert!(!another.compatibility.pending);
        assert_eq!(another.journey.page, Page::Migration);
        assert!(another.storage.facts.is_none());
    }

    #[test]
    fn full_os_check_back_keeps_destination_and_history_without_preparing_identity() {
        let mut preview = InstallerPreview {
            live: true,
            linux: true,
            ..Default::default()
        };
        preview.journey.destination = Some(Destination::Computer);
        let history = preview.journey.history.clone();
        preview.compatibility.result = Some(ui::LocalState::new(None));
        preview.direct.active = true;
        preview.direct.navigation.set(true);
        preview
            .apply(InstallerPreviewEffect::BackInPreview)
            .unwrap();
        assert!(!preview.direct.active);
        assert!(preview.compatibility.result.is_none());
        assert_eq!(preview.journey.destination, Some(Destination::Computer));
        assert_eq!(preview.journey.page, Page::Destination);
        assert_eq!(preview.journey.history, history);
        assert!(preview.journey.identity.is_none());
        assert!(preview.journey.password.is_empty());
    }

    #[test]
    fn full_os_check_display_facts_enter_the_normal_migration_flow() {
        let mut preview = InstallerPreview {
            live: true,
            linux: true,
            ..Default::default()
        };
        preview.journey.destination = Some(Destination::Computer);
        let mut storage = super::super::storage::Storage::default();
        storage.stage();
        let facts = storage.facts.clone().unwrap();
        let plan = storage.plan.unwrap().unwrap();
        let guid = plan.basis.disk.guid.clone();
        preview.compatibility.result = Some(ui::LocalState::new(Some(FullOsCheck { facts, plan })));
        preview.direct.active = true;
        assert!(preview.drain_compatibility());
        assert!(!preview.direct.active);
        assert_eq!(preview.journey.page, Page::Migration);
        assert_eq!(preview.journey.history, vec![Page::Destination]);
        assert_eq!(
            preview
                .storage
                .facts
                .as_ref()
                .unwrap()
                .basis
                .as_ref()
                .unwrap()
                .disk
                .guid,
            guid
        );
        assert!(preview.journey.password.is_empty());
        assert!(preview.journey.identity.is_none());
    }
}
