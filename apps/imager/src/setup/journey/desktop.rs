//! Desktop uses the current user's home and the canonical native package flow;
//! a host without the guided pages installs Full Tilo through the same native
//! setup, or writes a USB installer.
use super::*;

impl InstallerPreview {
    pub(super) fn prepare_desktop(&mut self) -> Result<(), &'static str> {
        if !self.live
            || self.journey.page != Page::Review
            || self.journey.destination != Some(Destination::Desktop)
            || self.direct.context.is_none()
            || self.busy()
        {
            return Err("installer-desktop-unavailable");
        }
        let child = crate::setup::view::SetupApp::for_desktop();
        self.show_desktop(child)
    }

    pub(super) fn prepare_native_full_os(&mut self) -> Result<(), &'static str> {
        if !self.live
            || !self.full_os_host
            || self.journey.page != Page::Destination
            || self.journey.destination != Some(Destination::Computer)
            || self.direct.context.is_none()
        {
            return Err("installer-review-installation-unavailable");
        }
        self.show_desktop(crate::setup::view::SetupApp::for_full_os())
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
            InstallerPreviewEffect::ContinuePreview
                if self.journey.page == Page::Destination
                    && self.journey.destination == Some(Destination::Computer) =>
            {
                self.native_full_os_review_pending = true;
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
            InstallerPreviewEffect::BackInPreview => {
                self.journey.destination = None;
                self.journey.page = Page::Destination;
            }
            _ => return Err("installer-preview-wrong-page"),
        }
        self.error = None;
        self.dirty = true;
        Ok(())
    }

    pub(super) fn full_os_host_view(&self) -> Box<dyn Widget> {
        let mut content = page::stack()
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
        page::Frame::new(content).with_cursor(self.cursor).build()
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
        let child = context.mount(|cx| crate::setup::view::setup_window(cx, child))?;
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
        self.composition_replaced = true;
        self.dirty = false;
        Ok(())
    }
}
