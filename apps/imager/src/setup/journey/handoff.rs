//! Where the review hands over to a setup page: Desktop to this person's
//! own packages, this computer's check before its guided pages, and Full Tilo
//! installed beside the current system. A host without the guided pages
//! installs Full Tilo through the same page, or writes a USB installer.
use super::native::{Handoff, NativeSetup};
use super::*;

/// Why a setup page cannot continue: its own ways forward are its actions.
const CANNOT_CONTINUE: &str = "Complete the current setup choice before continuing";

impl InstallerPreview {
    pub(super) fn prepare_desktop(&mut self) -> Result<(), &'static str> {
        if !self.live
            || self.journey.page != Page::Review
            || self.journey.destination != Some(Destination::Desktop)
            || self.busy()
        {
            return Err("installer-desktop-unavailable");
        }
        self.open_native(NativeSetup::new(Handoff::Desktop), true);
        Ok(())
    }

    pub(super) fn prepare_native_full_os(&mut self) -> Result<(), &'static str> {
        if !self.live
            || !self.full_os_host
            || self.journey.page != Page::Destination
            || self.journey.destination != Some(Destination::Computer)
        {
            return Err("installer-review-installation-unavailable");
        }
        self.open_native(NativeSetup::new(Handoff::Install), true);
        Ok(())
    }

    /// Check this computer for Full Tilo before the guided pages ask
    /// anything: the check decides whether they can install here.
    pub(super) fn prepare_compatibility(&mut self) -> Result<(), &'static str> {
        if !self.live
            || !self.linux
            || self.full_os_host
            || self.journey.page != Page::Destination
            || self.journey.destination != Some(Destination::Computer)
            || self.native.is_some()
            || self.busy()
        {
            return Err("installer-review-installation-unavailable");
        }
        self.open_native(NativeSetup::new(Handoff::Check), true);
        Ok(())
    }

    /// Show `native`, reading this computer first when `start` says so.
    pub(super) fn open_native(&mut self, mut native: NativeSetup, start: bool) {
        if start {
            native.start();
        }
        self.native = Some(native);
        self.pending_slide = Some(1.0);
        self.error = None;
        self.dirty = true;
    }

    /// Leave the setup page for the review it came from.
    pub(super) fn close_native(&mut self) -> Result<(), &'static str> {
        let desktop = self
            .native
            .take()
            .is_some_and(|native| native.handoff == Handoff::Desktop);
        if desktop {
            self.journey.back()?;
        }
        self.pending_slide = Some(-1.0);
        self.error = None;
        self.dirty = true;
        Ok(())
    }

    /// Whether the open setup page answers `effect` itself.
    pub(super) fn native_answers(&self, effect: &InstallerPreviewEffect) -> bool {
        self.native.is_some()
            && (native::is_native(effect.id())
                || matches!(effect, InstallerPreviewEffect::ContinuePreview))
    }

    /// Run `effect` on the open setup page: why it did not, in words.
    pub(super) fn apply_native(&mut self, effect: InstallerPreviewEffect) -> Result<(), String> {
        let native = self
            .native
            .as_mut()
            .ok_or_else(|| t("installer-preview-wrong-page"))?;
        self.dirty = true;
        if !matches!(effect, InstallerPreviewEffect::ContinuePreview) {
            return native.apply(effect);
        }
        if native.handoff != Handoff::Check {
            return Err(CANNOT_CONTINUE.into());
        }
        let (facts, plan) = native.checked().inspect_err(|error| {
            native.notice = Some(error.clone());
        })?;
        self.native = None;
        self.storage.accept(facts);
        let gib = crate::setup::internal_install_flow::target_extent_bytes(&plan) >> 30;
        if let Err(error) = self.storage.set_size(gib) {
            self.error = Some(error);
        } else if let Err(error) = self.advance_journey() {
            self.error = Some(error);
        }
        self.pending_slide = Some(1.0);
        Ok(())
    }

    /// A host without the guided pages chooses between installing here, with
    /// the native Full Tilo setup, and writing a USB installer.
    pub(super) fn apply_full_os_host(
        &mut self,
        effect: InstallerPreviewEffect,
    ) -> Result<(), &'static str> {
        match effect {
            InstallerPreviewEffect::ReadPreview => return Ok(()),
            InstallerPreviewEffect::ChoosePreview { choice }
                if self.journey.page == Page::Destination =>
            {
                self.journey.destination = Some(match choice.as_str() {
                    "computer" => Destination::Computer,
                    "another" => Destination::Another,
                    _ => return Err("installer-preview-invalid-choice"),
                });
            }
            InstallerPreviewEffect::ContinuePreview if self.journey.destination.is_none() => {
                return Err("installer-preview-choose-first");
            }
            InstallerPreviewEffect::ContinuePreview
                if self.journey.page == Page::Destination
                    && self.journey.destination == Some(Destination::Computer) =>
            {
                return self.prepare_native_full_os();
            }
            InstallerPreviewEffect::ContinuePreview | InstallerPreviewEffect::PrepareUsb
                if self.journey.page == Page::Destination
                    && self.journey.destination == Some(Destination::Another) =>
            {
                self.media
                    .as_ref()
                    .ok_or("installer-preview-storage-unavailable")?
                    .set(true);
            }
            InstallerPreviewEffect::BackInPreview => return Err("installer-preview-at-start"),
            _ => return Err("installer-preview-wrong-page"),
        }
        self.error = None;
        self.dirty = true;
        Ok(())
    }

    pub(super) fn full_os_host_view(&self) -> Box<dyn Widget> {
        let mut content = layout::stack()
            .push(Box::new(
                Text::new(&t("installer-preview-destination-heading")).with_role(TextRole::Heading),
            ))
            .push(Box::new(self.choice(
                "computer",
                self.journey.destination == Some(Destination::Computer),
            )))
            .push(Box::new(self.choice(
                "another",
                self.journey.destination == Some(Destination::Another),
            )));
        if let Some(error) = self.error {
            content = content.push(Box::new(Text::new(&t(error))));
        }
        content = content.push(Box::new(
            self.button(
                "installer.continue",
                "installer-preview-continue",
                InstallerPreviewEffect::ContinuePreview,
            )
            .with_enabled(self.journey.destination.is_some()),
        ));
        GuidedPage::new(content).cursor(self.cursor).build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A preview of the Linux route these phases model; a Windows host takes
    /// the native Full Tilo setup straight from its destination instead.
    fn linux_preview() -> InstallerPreview {
        InstallerPreview {
            live: true,
            linux: true,
            full_os_host: false,
            ..Default::default()
        }
    }

    fn handoff(preview: &InstallerPreview) -> Option<Handoff> {
        preview.native.as_ref().map(|native| native.handoff)
    }

    tilo_test_env::scenario! {
        fn the_full_os_check_runs_only_where_it_decides_something;

        phase full_os_check_comes_first_and_is_not_used_for_another_computer;
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(crate::setup::internal_install_flow::RUNTIME_ALLOW_VAR, "1");
            let mut preview = linux_preview();
            preview.capabilities.prepared_installation = true;
            preview.journey.destination = Some(Destination::Computer);
            preview
                .apply(InstallerPreviewEffect::ContinuePreview)
                .unwrap();
            assert_eq!(handoff(&preview), Some(Handoff::Check));
            assert_eq!(preview.journey.page, Page::Destination);
            assert!(preview.journey.password.is_empty());

            let mut another = linux_preview();
            another.capabilities.prepared_installation = true;
            another.journey.destination = Some(Destination::Another);
            another
                .apply(InstallerPreviewEffect::ContinuePreview)
                .unwrap();
            assert_eq!(handoff(&another), None);
            assert_eq!(another.journey.page, Page::Target);
            assert!(!another.target.ready());
            assert!(another.storage.facts.is_none());
        }

        phase full_os_check_is_skipped_when_this_build_offers_only_a_usb_installer;
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(crate::setup::internal_install_flow::RUNTIME_ALLOW_VAR, "0");
            let mut preview = linux_preview();
            preview.capabilities.prepared_installation = true;
            preview.journey.destination = Some(Destination::Computer);
            preview
                .apply(InstallerPreviewEffect::ContinuePreview)
                .unwrap();
            assert_eq!(handoff(&preview), None);
            assert_eq!(preview.journey.page, Page::Migrate);
            assert!(preview.storage.facts.is_none());
        }

        phase full_os_check_is_not_repeated_for_facts_this_host_already_reported;
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(crate::setup::internal_install_flow::RUNTIME_ALLOW_VAR, "1");
            let mut preview = linux_preview();
            preview.capabilities.prepared_installation = true;
            preview.storage.stage();
            preview.journey.destination = Some(Destination::Computer);
            preview
                .apply(InstallerPreviewEffect::ContinuePreview)
                .unwrap();
            assert_eq!(handoff(&preview), None);
            assert_eq!(preview.journey.page, Page::Migrate);
        }

        phase full_os_check_back_keeps_destination_and_history;
        {
            let mut preview = linux_preview();
            preview.journey.destination = Some(Destination::Computer);
            let history = preview.journey.history.clone();
            preview.open_native(NativeSetup::new(Handoff::Check), false);
            preview
                .apply(InstallerPreviewEffect::BackInPreview)
                .unwrap();
            assert_eq!(handoff(&preview), None);
            assert_eq!(preview.journey.destination, Some(Destination::Computer));
            assert_eq!(preview.journey.page, Page::Destination);
            assert_eq!(preview.journey.history, history);
            assert!(preview.journey.password.is_empty());
        }
    }
}
