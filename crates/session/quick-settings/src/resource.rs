//! Quick Settings as an app: its resource is the panel itself — which page
//! shows, which section is open in place, whether it was opened to paste or
//! is being arranged — and its actions move between them.
//!
//! The panel is this device's own and is gone with the session: the desktop
//! mounts it as a private window over a record kept in memory
//! ([`crate::prepare`]), so nothing about it is written to disk or reaches
//! another device. Opening a section is a change *about* the panel, not
//! content anyone undoes (`housekeeping`).

use ui::framework::{Fixture, ResourceApp, ViewCx};
use ui::prelude::*;
use ui::widget::Element;

use crate::QsSection;
use crate::host::FixtureHost;

/// Which page of the panel shows.
#[derive(
    Model,
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    ui::tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Page {
    /// The controls, with sections opening in place.
    #[default]
    Root,
    /// The notifications kept since they arrived.
    Notifications,
    /// One notification, read in full.
    Notification,
    /// One Bluetooth accessory and the devices it can move to.
    Accessory,
}

/// The panel.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct QuickSettings {
    /// The page showing.
    pub page: Page,
    /// The accessory its page is about, by address.
    pub accessory: String,
    /// The notification being read.
    pub notification: u64,
    /// The section open in place on the root page; one at a time.
    pub section: Option<QsSection>,
    /// Opened to paste: the clipboard history alone, and a choice is pasted
    /// into the focused field.
    pub pasting: bool,
    /// Arranging which controls the panel shows.
    pub editing: bool,
}

#[resource(kind = "quick-settings.panel", label = "Quick Settings")]
impl QuickSettings {
    /// Show the controls, with nothing open: how the panel opens.
    #[action(housekeeping)]
    pub fn show_controls(&mut self) {
        *self = Self::default();
    }

    /// Open a section in place, closing any other.
    #[action(housekeeping)]
    pub fn open_section(&mut self, #[param("The section")] section: QsSection) {
        self.page = Page::Root;
        self.pasting = false;
        self.editing = false;
        self.section = Some(section);
    }

    /// Open a section, or close it when it is the one open.
    #[action(housekeeping)]
    pub fn toggle_section(&mut self, #[param("The section")] section: QsSection) {
        if self.section == Some(section) {
            self.section = None;
        } else {
            self.open_section(section);
        }
    }

    /// Close the open section.
    #[action(housekeeping)]
    pub fn close_section(&mut self) {
        self.section = None;
    }

    /// Show the notifications.
    #[action(housekeeping)]
    pub fn show_notifications(&mut self) {
        self.pasting = false;
        self.editing = false;
        self.page = Page::Notifications;
    }

    /// Read one notification in full.
    #[action(housekeeping)]
    pub fn read_notification(&mut self, #[param("The notification")] notification: u64) {
        self.page = Page::Notification;
        self.notification = notification;
    }

    /// Show a Bluetooth accessory and the devices it can move to.
    #[action(housekeeping)]
    pub fn show_accessory(&mut self, #[param("The accessory's address")] address: String) {
        self.pasting = false;
        self.editing = false;
        self.page = Page::Accessory;
        self.accessory = address;
    }

    /// Open the clipboard history to paste into the focused field.
    #[action(housekeeping)]
    pub fn paste(&mut self) {
        *self = Self {
            section: Some(QsSection::Clipboard),
            pasting: true,
            ..Self::default()
        };
    }

    /// Start or finish arranging the panel's controls.
    #[action(housekeeping)]
    pub fn edit(&mut self, #[param("Whether the controls are being arranged")] editing: bool) {
        self.page = Page::Root;
        self.section = None;
        self.pasting = false;
        self.editing = editing;
    }

    /// Go back: finish arranging, leave a page, or close the open section.
    #[action(housekeeping)]
    pub fn back(&mut self) {
        if self.editing {
            self.editing = false;
        } else if self.page == Page::Notification {
            self.page = Page::Notifications;
        } else if self.page != Page::Root {
            self.page = Page::Root;
        } else {
            self.section = None;
        }
    }
}

impl QuickSettings {
    /// What the panel shows, as the host is told.
    #[must_use]
    pub fn showing(&self) -> crate::host::Showing {
        crate::host::Showing {
            page: self.page,
            accessory: if self.page == Page::Accessory {
                self.accessory.clone()
            } else {
                String::new()
            },
            notification: if self.page == Page::Notification {
                self.notification
            } else {
                0
            },
            section: (self.page == Page::Root && !self.editing)
                .then_some(self.section)
                .flatten(),
            pasting: self.pasting,
            editing: self.editing,
        }
    }
}

/// The app.
pub struct QuickSettingsApp;

/// The width of the panel, as the desktop places it.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a panel's width is a small positive whole number of pixels"
)]
pub const PANEL_WIDTH: u32 = ui::tokens::Metrics::BASE.panel_width_list as u32;

impl ResourceApp for QuickSettingsApp {
    type Resource = QuickSettings;
    const ID: &'static str = "so.tilo.quick-settings";
    const TITLE: &'static str = "Quick Settings";
    const ICON: Option<&'static str> = Some("settings");
    // The panel's record is the panel's state: nothing to look back on.
    const DOCUMENT_HISTORY: bool = false;
    const PRIVATE_WINDOWS: bool = true;
    // The panel shows this device's desktop: it is nobody's workspace
    // participant and opens before anyone is chosen.
    const ATTACHES_RUNTIME: bool = false;
    const OPENS_WITHOUT_IDENTITY: bool = true;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::i18n::CATALOG)
    }

    fn view(cx: &mut ViewCx<QuickSettings>) -> Element {
        crate::view::panel(cx)
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        let height = match scenario {
            Some("paste") => 420,
            Some("short") => 480,
            _ => 900,
        };
        (PANEL_WIDTH, height)
    }

    fn scenarios() -> &'static [&'static str] {
        crate::scenarios::NAMES
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<QuickSettings>> {
        crate::scenarios::actions(name)
    }

    fn fixture(fixture: &mut Fixture<QuickSettings>) {
        let readings = crate::scenarios::readings(fixture.scenario());
        fixture.provide(FixtureHost::new(readings));
    }
}
