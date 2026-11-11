//! Quick Settings as an app: its resource is the panel itself — which page
//! shows, which section's page replaced the controls, whether it was opened
//! to paste or is being arranged — and its actions move between them.
//!
//! The panel is this device's own and is gone with the session: the session
//! runs it as a surface of its own over a record kept in memory, so nothing
//! about it is written to disk or reaches another device. Opening a section
//! is a change *about* the panel, not content anyone undoes (`housekeeping`).

use ui::framework::{Fixture, ResourceApp, ViewCx};
use ui::prelude::*;
use ui::widget::Element;

use crate::QsSection;
use crate::host::FixtureHost;

/// Which page of the panel shows.
#[derive(
    Model, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Page {
    /// The controls, or the page of the section opened from them.
    #[default]
    Root,
    /// One Bluetooth accessory and the devices it can move to.
    Accessory,
}

impl ui::tilo_resource::JsonSchema for Page {
    /// The pages the panel has.
    fn json_schema() -> ui::serde_json::Value {
        ui::serde_json::json!({ "type": "string", "enum": ["root", "accessory"] })
    }
}

/// The panel.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct QuickSettings {
    /// The page showing.
    pub page: Page,
    /// The accessory its page is about, by address.
    pub accessory: String,
    /// The section whose page replaces the controls; one at a time.
    pub section: Option<QsSection>,
    /// Opened to paste: the clipboard history alone, and a choice is pasted
    /// into the focused field.
    pub pasting: bool,
    /// Arranging which controls the panel shows.
    pub editing: bool,
}

#[resource(kind = "quick-settings.panel", label = "Quick Settings", answer = answer)]
impl QuickSettings {
    /// Show the controls, with nothing open: how the panel opens.
    #[action(housekeeping)]
    pub fn show_controls(&mut self) {
        *self = Self::default();
    }

    /// Open a section's page, closing any other.
    #[action(housekeeping)]
    pub fn open_section(&mut self, #[param("The section")] section: QsSection) {
        self.page = Page::Root;
        self.pasting = false;
        self.editing = false;
        self.section = Some(section);
    }

    /// Open a section's page, or go back to the controls when it is the one
    /// open.
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
    ///
    /// It answers whether it had anywhere to go back to
    /// ([`answer`]): the session that shows the panel closes it only from
    /// the panel's main page, and asks it to go back everywhere else.
    #[action(housekeeping, answers)]
    pub fn back(&mut self) {
        if self.editing {
            self.editing = false;
        } else if self.page != Page::Root {
            self.page = Page::Root;
        } else {
            self.section = None;
        }
    }
}

/// What [`QuickSettings::back`] answers its caller: whether it had anywhere
/// to go back to, which is exactly whether it changed anything. A panel on
/// its main page changes nothing, and its caller closes it.
fn answer(
    _doc: &ui::tilo_resource::Doc,
    action: &str,
    changes: &[ui::tilo_resource::Patch],
) -> Option<ui::serde_json::Value> {
    (action == QuickSettings::BACK.id)
        .then(|| ui::serde_json::json!({ "handled": !changes.is_empty() }))
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

/// The panel's footprint, as the desk sizes a popover and the window the
/// card fills: the desk's `POPOVER_WIDTH` and `POPOVER_HEIGHT` place the
/// window, so what the panel shows never changes where it stands.
pub const PANEL_WIDTH: u32 = 380;
pub const PANEL_HEIGHT: u32 = 640;

impl ResourceApp for QuickSettingsApp {
    type Resource = QuickSettings;
    const ID: &'static str = "so.tilo.quick-settings";
    const TITLE: &'static str = "Quick Settings";
    const ICON: Option<&'static str> = Some("settings");
    // The panel's record is the panel's state: nothing to look back on.
    const DOCUMENT_HISTORY: bool = false;
    const PRIVATE_WINDOWS: bool = true;
    // The panel shows this device's desktop: it is nobody's workspace
    // participant.
    const ATTACHES_RUNTIME: bool = false;
    // The panel is a card over this device's desktop, not a window with a
    // surface of its own: what its card does not paint shows what lies
    // under it, and the card's finish paints the material the theme gives
    // a floating surface.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::i18n::CATALOG)
    }

    fn view(cx: &mut ViewCx<QuickSettings>) -> Element {
        crate::view::panel(cx)
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        // The panel's footprint, which the desk gives its window: the shot is
        // the surface, so it is the size the surface stands at whatever it
        // shows.
        (PANEL_WIDTH, PANEL_HEIGHT)
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
