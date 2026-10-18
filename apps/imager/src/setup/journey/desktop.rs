//! Desktop uses the current user's home and the canonical native package flow.
use super::{Destination, InstallerPreview, InstallerPreviewEffect, Page};
use ui::widget::{ButtonRole, Row};
impl InstallerPreview {
    pub(super) fn prepare_desktop(&mut self) -> Result<(), &'static str> {
        if !self.live
            || self.journey.page != Page::Review
            || self.journey.destination != Some(Destination::Desktop)
            || self.direct.context.is_none()
            || self.media_preparation.busy()
            || self.personal_busy()
            || self.migration_busy()
        {
            return Err("installer-desktop-unavailable");
        }
        let child = crate::setup::view::SetupApp::for_desktop();
        self.show_desktop(child)
    }
    pub(super) fn finish_desktop(
        &mut self,
        prepared: std::sync::Arc<super::prepared_media::PreparedMedia>,
    ) -> Result<(), &'static str> {
        self.desktop_pending = false;
        let home = super::super::home_transfer::current_home()
            .map_err(|_| "installer-desktop-unavailable")?;
        let child = crate::setup::view::SetupApp::from_guided_desktop(prepared, home)
            .map_err(|_| "installer-desktop-unavailable")?;
        self.show_desktop(child)
    }

    pub(super) fn prepare_native_full_os(&mut self) -> Result<(), &'static str> {
        if !self.live
            || !self.windows.enabled
            || self.journey.page != Page::Destination
            || self.journey.destination != Some(Destination::Computer)
            || self.direct.context.is_none()
            || self.windows.busy()
            || self.personal_busy()
            || self.migration_busy()
        {
            return Err("installer-review-installation-unavailable");
        }
        self.show_desktop(crate::setup::view::SetupApp::for_full_os())
    }

    pub(super) fn show_desktop(
        &mut self,
        child: crate::setup::view::SetupApp,
    ) -> Result<(), &'static str> {
        let navigation = child.guided_navigation_guard();
        let context = self
            .direct
            .context
            .as_ref()
            .ok_or("installer-desktop-unavailable")?;
        let child = context.surface(child)?;
        let back = self
            .button(
                "installer.back",
                "installer-preview-back",
                InstallerPreviewEffect::BackInPreview,
            )
            .role(ButtonRole::Plain)
            .enabled_from(&navigation);
        self.composed = Some(Box::new(
            crate::page::stack()
                .push(child)
                .push(Box::new(Row::new().push(Box::new(back)))),
        ));
        self.direct.navigation = navigation;
        self.direct.active = true;
        self.dirty = false;
        Ok(())
    }
}
