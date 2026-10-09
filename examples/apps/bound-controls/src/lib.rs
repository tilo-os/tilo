//! Bound controls: every standard control, bound to a resource field.
//!
//! A bound control shows a value read from the resource and invokes an action
//! with each value the user commits. It keeps no state of its own, so it
//! follows edits from anywhere (a collaborator, an agent, an undo) with no
//! notification code. When the action is forbidden, for a viewer, the control
//! disables itself.
//!
//! `cx.input` and `cx.checkbox` cover text and booleans; `cx.control` binds
//! any other toolkit control built from a `LocalState`.

/// The application id.
pub const APP_ID: &str = "so.tilo.example.bound-controls";
/// The window and launcher title.
pub const APP_TITLE: &str = "Bound Controls";

pub mod locales;

fn install_catalog() {
    locales::install();
}

use ui::i18n::I18nManager;
use ui::prelude::*;

/// The regions a profile can choose.
pub const REGIONS: [&str; 3] = ["Europe/Berlin", "America/New_York", "Asia/Tokyo"];

/// One field per kind of control.
#[derive(Model, Debug, Clone, PartialEq)]
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

/// Validation lives in the action, so it holds for every caller, not only for
/// the control that happens to limit its range.
#[resource(kind = "example.preferences", label = "Preferences")]
impl Preferences {
    /// Change the display name.
    #[action]
    pub fn set_name(&mut self, #[param("Display name")] name: String) {
        self.name = name;
    }

    /// Receive updates or not.
    #[action]
    pub fn set_updates(&mut self, #[param("Receive updates")] updates: bool) {
        self.updates = updates;
    }

    /// Change how often to refresh.
    #[action]
    pub fn set_refresh(
        &mut self,
        #[param("Refresh interval in minutes, 1 to 120")] minutes: f64,
    ) -> Result<(), &'static str> {
        if !(1.0..=120.0).contains(&minutes) {
            return Err("the refresh interval is 1 to 120 minutes");
        }
        self.refresh_minutes = minutes;
        Ok(())
    }

    /// Change the volume.
    #[action]
    pub fn set_volume(
        &mut self,
        #[param("Volume from 0 to 1")] volume: f64,
    ) -> Result<(), &'static str> {
        if !(0.0..=1.0).contains(&volume) {
            return Err("the volume is between 0 and 1");
        }
        self.volume = volume;
        Ok(())
    }

    /// Change the region.
    #[action]
    pub fn set_region(&mut self, #[param("Region")] region: String) -> Result<(), &'static str> {
        if !REGIONS.contains(&region.as_str()) {
            return Err("unknown region");
        }
        self.region = Atom(region);
        Ok(())
    }

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

    fn view(cx: &mut ViewCx<Preferences>) -> Element {
        install_catalog();
        // Text and booleans have dedicated bindings.
        let name = cx
            .input_with(|prefs| prefs.name.clone(), Preferences::SET_NAME)
            .with_id("name-input")
            .with_placeholder("controls-name-placeholder");
        let updates = cx
            .checkbox_with(
                "controls-updates-label",
                |prefs| prefs.updates,
                Preferences::SET_UPDATES,
            )
            .with_id("updates-checkbox");
        // Everything else goes through `cx.control`: build the control from
        // the `LocalState` it is given, say what it shows, and which action
        // its committed values invoke.
        let refresh = cx
            .control_with(
                |value| {
                    NumberInput::from_state(value)
                        .with_range(1.0, 120.0)
                        .with_step(5.0)
                        .with_suffix(&I18nManager::lookup_plain("controls-minutes-suffix"))
                },
                |prefs| prefs.refresh_minutes,
                Preferences::SET_REFRESH,
            )
            .with_id("refresh-input");
        let volume = cx
            .control_with(
                Slider::from_state,
                |prefs| prefs.volume,
                Preferences::SET_VOLUME,
            )
            .with_id("volume-slider");
        let region = cx
            .control_with(
                |value| {
                    Dropdown::from_state(
                        REGIONS.iter().map(|region| (*region).to_owned()).collect(),
                        value,
                    )
                },
                |prefs| prefs.region.0.clone(),
                Preferences::SET_REGION,
            )
            .with_id("region-select");
        let reset = cx.button("controls-reset", Preferences::RESET.invoke());

        Box::new(ui::column![
            Text::new("controls-title").with_role(TextRole::Title),
            labeled_field("controls-name-label", name),
            updates,
            labeled_field("controls-refresh-label", refresh),
            labeled_field("controls-volume-label", volume),
            labeled_field("controls-region-label", region),
            reset,
        ])
    }
}

ui::dev_reload_entry!(App);
