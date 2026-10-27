//! Bound controls: every standard control, bound to a field by its path.
//!
//! A bound control shows the value at a path of the resource and edits it
//! there with each value the user commits. It keeps no state of its own, so
//! it follows edits from anywhere (a collaborator, an agent, an undo) with no
//! notification code, and disables itself for a viewer.
//!
//! `cx.input` and `cx.checkbox` cover text and booleans; `cx.control` binds
//! any other toolkit control built from a `LocalState`, and `cx.control_map`
//! one whose value is not the field's own type.

/// The application id.
pub const APP_ID: &str = "so.tilo.example.bound-controls";
/// The window and launcher title.
pub const APP_TITLE: &str = "Bound Controls";

pub mod locales;

use ui::i18n::I18nManager;
use ui::prelude::*;

/// The regions a profile can choose.
pub const REGIONS: [&str; 3] = ["Europe/Berlin", "America/New_York", "Asia/Tokyo"];

/// One field per kind of control. The invariant holds for every way in: a
/// control, an agent's edit, a script.
#[derive(Model, Debug, Clone, PartialEq)]
#[model(check = Preferences::valid)]
pub struct Preferences {
    /// Collaborative text: concurrent typing merges.
    pub name: String,
    pub updates: bool,
    pub refresh_minutes: f64,
    pub volume: f64,
    /// A code rather than prose, so it is an `Atom`: written whole, last
    /// writer wins, instead of merging characters.
    pub region: Atom,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            name: String::new(),
            updates: true,
            refresh_minutes: 15.0,
            volume: 0.5,
            region: Atom(String::from(REGIONS[0])),
        }
    }
}

impl Preferences {
    /// What every change must leave true, whichever field it edits.
    ///
    /// # Errors
    /// Refuses a refresh interval, volume or region out of range.
    pub fn valid(&self) -> Result<(), Refusal> {
        if !(1.0..=120.0).contains(&self.refresh_minutes) {
            return Err(Refusal::new("the refresh interval is 1 to 120 minutes"));
        }
        if !(0.0..=1.0).contains(&self.volume) {
            return Err(Refusal::new("the volume is between 0 and 1"));
        }
        if !REGIONS.contains(&self.region.0.as_str()) {
            return Err(Refusal::new("unknown region"));
        }
        Ok(())
    }
}

/// Every field is edited at its path; the one verb is what a field edit
/// cannot say.
#[resource(kind = "example.preferences", label = "Preferences")]
impl Preferences {
    /// Restore every preference to its default.
    #[action]
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

pub struct App;

impl ResourceApp for App {
    type Resource = Preferences;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    fn view(cx: &mut ViewCx<Preferences>) -> Element {
        let p = cx.paths();
        // Text and booleans have dedicated bindings.
        let name = cx
            .input(p.name)
            .with_id("name-input")
            .with_placeholder(crate::locales::keys::CONTROLS_NAME_PLACEHOLDER);
        let updates = cx
            .checkbox(crate::locales::keys::CONTROLS_UPDATES_LABEL, p.updates)
            .with_id("updates-checkbox");
        // Everything else goes through `cx.control`: build the control from
        // the `LocalState` it is given and name the field it edits.
        let refresh = cx
            .control(
                |value| {
                    NumberInput::from_state(value)
                        .with_range(1.0, 120.0)
                        .with_step(5.0)
                        .with_suffix(&I18nManager::lookup_plain("controls-minutes-suffix"))
                },
                p.refresh_minutes,
            )
            .with_id("refresh-input");
        let volume = cx
            .control(Slider::from_state, p.volume)
            .with_id("volume-slider");
        // The dropdown chooses names; the field holds an `Atom`.
        let region = cx
            .control_map(
                |value| {
                    Dropdown::from_state(
                        REGIONS.iter().map(|region| (*region).to_owned()).collect(),
                        value,
                    )
                },
                p.region,
                |region| region.0.clone(),
                Atom,
            )
            .with_id("region-select");
        let reset = cx.button(
            crate::locales::keys::CONTROLS_RESET,
            Preferences::RESET.invoke(),
        );

        Box::new(ui::column![
            Text::new("controls-title").with_role(TextRole::Title),
            labeled_field(crate::locales::keys::CONTROLS_NAME_LABEL, name),
            updates,
            labeled_field(crate::locales::keys::CONTROLS_REFRESH_LABEL, refresh),
            labeled_field(crate::locales::keys::CONTROLS_VOLUME_LABEL, volume),
            labeled_field(crate::locales::keys::CONTROLS_REGION_LABEL, region),
            reset,
        ])
    }
}

ui::dev_reload_entry!(App);
