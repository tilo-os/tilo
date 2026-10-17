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
        let identity = self
            .personal
            .prepared_handle()
            .ok_or("installer-preview-identity-not-prepared")?;
        let accounts = if self.journey.migrate == Some(true)
            && self.journey.categories[2]
            && self.accounts.has_prepared_connections()
        {
            Some(
                self.accounts
                    .prepared_handle()
                    .ok_or("installer-preview-account-handoff-unavailable")?,
            )
        } else {
            None
        };
        let install_profile = self.public_install_profile()?;
        self.media_preparation.start_with_selections(
            identity,
            accounts,
            super::prepared_media::MediaSelections {
                sessions: self.belongings.sessions.selected_for_accounts(
                    self.journey.migrate == Some(true) && self.journey.categories[2],
                ),
                home_applications: false,
                home: None,
                retained: None,
                planned_direct: None,
                applications: None,
                browser: (self.journey.migrate == Some(true)
                    && self.belongings.browser.available())
                .then(|| self.belongings.browser.selected())
                .filter(|selected| !selected.profiles().is_empty()),
            },
            zeroize::Zeroizing::new(self.journey.password.clone()),
            Some(install_profile),
        )?;
        self.desktop_pending = true;
        Ok(())
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
