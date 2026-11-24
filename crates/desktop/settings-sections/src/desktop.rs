//! The desktop's own sections: how the shell is navigated, which app opens
//! each kind of file, the clock, the displays' placement and the display
//! change being tried. The session keeps them like every other section, so
//! an agent reads and changes them whether or not Settings is open.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use settings_store::hardware::{DisplayTransaction, DisplayTransactionState};
use settings_store::person_displays::{
    MAX_PLACED_DISPLAYS, ORDER_GAP, PLACEMENTS_SECTION, PlacedDisplay,
};
use ui::tilo_resource;

/// The shell's desktop navigation preferences, as the person's settings
/// hold them.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[model(defaults)]
pub struct DesktopControls {
    /// Add Super+Ctrl+h/j/k/l aliases for moving between windows and strips,
    /// beside the arrow keys.
    #[serde(default)]
    pub vim_mode: bool,
}

impl settings_model::SettingsSchema for DesktopControls {
    const SCOPE: settings_model::Scope =
        settings_model::Scope::Shared(settings_store::desktop_controls::DESKTOP_CONTROLS_SECTION);
}

#[ui::tilo_resource::resource(
    kind = "settings.desktop-controls",
    version = 2,
    migrate = crate::leaves_moved_out
)]
impl DesktopControls {}

/// The shell's desktop preferences this device holds: a small laptop panel
/// and a large shell display want different answers.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[model(defaults)]
pub struct DesktopControlsDevice {
    /// Hide the top bar until the pointer reaches for it; shown in the
    /// Appearance pane.
    #[serde(default)]
    pub hide_top_bar: bool,
}

impl settings_model::SettingsSchema for DesktopControlsDevice {
    const SCOPE: settings_model::Scope = settings_model::Scope::Shared(
        settings_store::desktop_controls::DESKTOP_CONTROLS_DEVICE_SECTION,
    );
}

#[ui::tilo_resource::resource(kind = "settings.desktop-controls-device")]
impl DesktopControlsDevice {}

/// The person's default apps, as their settings hold them.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
pub struct DefaultApps {
    /// The app each kind of file or link opens with, by the kind's id
    /// (`ext:pdf`, `mime:text/html`, `scheme:https`).
    #[serde(default)]
    #[model(readonly)]
    pub handlers: BTreeMap<String, String>,
}

impl settings_model::SettingsSchema for DefaultApps {
    const SCOPE: settings_model::Scope =
        settings_model::Scope::Shared(desktop_policy::PERSONAL_DEFAULTS_SECTION);
}

#[ui::tilo_resource::resource(kind = "settings.default-apps")]
impl DefaultApps {
    /// Open one kind of file or link with an app. Only the chosen kind
    /// changes: the handlers kept for every other kind, including kinds a
    /// newer version knows, stay.
    #[action(id = "set-default-app-handler")]
    pub fn open_with(
        &mut self,
        #[param("The kind of file or link: `ext:pdf`, `mime:text/html`, `scheme:https`")]
        kind: String,
        #[param("The app's id")] app: String,
    ) -> Result<(), tilo_resource::Refusal> {
        let kind = desktop_policy::ResourceKind::parse(&kind).ok_or_else(|| {
            tilo_resource::Refusal::new(format!("`{kind}` is not a kind of file or link"))
        })?;
        let app = app.trim();
        if app.is_empty() {
            return Err(tilo_resource::Refusal::new("an app id is needed"));
        }
        self.handlers
            .insert(kind.as_str().to_owned(), app.to_owned());
        Ok(())
    }
}

/// This device's clock settings: whether its time zone follows its
/// location. The session's zone follower reads the same leaf
/// ([`settings_store::clock::AUTOMATIC_ZONE`]).
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    ui::tilo_resource::Model,
)]
#[model(defaults)]
pub struct ClockPrefs {
    /// The zone follows the device's location.
    #[serde(default)]
    pub automatic_zone: bool,
}

impl settings_model::SettingsSchema for ClockPrefs {
    const SCOPE: settings_model::Scope =
        settings_model::Scope::Shared(settings_store::clock::SECTION);
}

#[ui::tilo_resource::resource(kind = "settings.clock")]
impl ClockPrefs {}

/// How the person's displays are arranged, wherever they connect them. Each display is one entry
/// of a keyed collection, so a device that places its own displays writes only their entries.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[serde(default)]
pub struct SharedDisplayPrefs {
    /// The place of each of the person's displays, by `<device>/<output>`.
    #[model(readonly)]
    #[serde(
        default,
        with = "settings_store::person_displays::places",
        skip_serializing_if = "settings_store::person_displays::places::is_empty"
    )]
    pub placement: ui::tilo_resource::Keyed<PlacedDisplay>,
}

impl settings_model::SettingsSchema for SharedDisplayPrefs {
    const SCOPE: settings_model::Scope = settings_model::Scope::Shared(PLACEMENTS_SECTION);
}

#[ui::tilo_resource::resource(kind = "settings.display-placement")]
impl SharedDisplayPrefs {
    /// Place one of the person's displays in the shared layout: where it stands, its order and
    /// whether it has a place yet. Every other display's entry stays.
    #[action(id = "place-display")]
    pub fn place_display(
        &mut self,
        #[param("The device that drives the display")] device: String,
        #[param("The device's output id")] output: String,
        #[param("Left, in the shared space's logical pixels")] x: i32,
        #[param("Top, in the shared space's logical pixels")] y: i32,
        #[param("Where it comes in the order; none keeps it, or puts a new display last")]
        order: Option<i64>,
        #[param("Whether it has a place yet; none marks a new display placed")] placed: Option<
            bool,
        >,
    ) -> Result<(), tilo_resource::Refusal> {
        let id = ui::tilo_resource::Id::from_raw(format!("{device}/{output}"));
        let mut entry = self
            .placement
            .find(&id)
            .cloned()
            .unwrap_or_else(|| PlacedDisplay {
                device,
                output,
                order: self
                    .placement
                    .iter()
                    .map(|entry| entry.order)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(ORDER_GAP),
                placed: true,
                x,
                y,
            });
        entry.x = x;
        entry.y = y;
        if let Some(order) = order {
            entry.order = order;
        }
        entry.placed = placed.unwrap_or(true);
        if !entry.valid() {
            return Err(tilo_resource::Refusal::new(
                "the display's name, place or order is out of bounds",
            ));
        }
        if let Some(held) = self.placement.find_mut(&id) {
            *held = entry;
        } else if self.placement.len() < MAX_PLACED_DISPLAYS {
            self.placement.push(entry);
        } else {
            return Err(tilo_resource::Refusal::new(format!(
                "the layout keeps at most {MAX_PLACED_DISPLAYS} displays"
            )));
        }
        Ok(())
    }
}

/// The person's placement as the settings document `document` holds it: the entries this device
/// reads, or none.
#[must_use]
pub fn placements_in(
    document: &serde_json::Map<String, serde_json::Value>,
) -> ui::tilo_resource::Keyed<PlacedDisplay> {
    document
        .get(PLACEMENTS_SECTION)
        .cloned()
        .and_then(|section| serde_json::from_value::<SharedDisplayPrefs>(section).ok())
        .map_or_else(Default::default, |prefs| prefs.placement)
}

/// Whether a display nearby offers itself as this device's input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[serde(default)]
pub struct AutomaticDisplayPrefs {
    /// A device administrator's choice: changed only through its verb.
    #[model(readonly)]
    pub automatic_display_input: bool,
}

impl Default for AutomaticDisplayPrefs {
    fn default() -> Self {
        Self {
            automatic_display_input: settings_store::continuity_privacy::ON_UNLESS_SET,
        }
    }
}

impl settings_model::SettingsSchema for AutomaticDisplayPrefs {
    const SCOPE: settings_model::Scope = settings_model::Scope::Shared("continuity");
}

#[ui::tilo_resource::resource(kind = "settings.automatic-display")]
impl AutomaticDisplayPrefs {
    /// Opt this device into automatic grants to its enrolled online input
    /// devices, or out. The separate Continuity master and Display/input
    /// switches must also be on.
    #[action(id = "set-automatic-display-input")]
    pub fn set_automatic_display_input(&mut self, on: bool) -> Result<(), tilo_resource::Refusal> {
        self.automatic_display_input = on;
        Ok(())
    }
}

/// The display change being tried on this device, as the session writes it. What a change is
/// tried *with* is a display's own action (`session:displays`), so this is read-only: the change
/// being tried, how it stands, and when it goes back unless the person keeps it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[serde(default)]
pub struct PendingDisplayChange {
    /// The change's id; none is pending while it is 0.
    #[model(readonly)]
    pub id: u64,
    /// How it stands: asked for, awaiting an answer, kept, reverted or
    /// refused.
    #[model(readonly)]
    pub state: DisplayTransactionState,
    /// When it reverts unless kept, in Unix milliseconds.
    #[model(readonly)]
    pub deadline_unix_ms: Option<u64>,
    /// The session's diagnostic, in its own terms.
    #[model(readonly)]
    pub message: Option<String>,
}

impl settings_model::SettingsSchema for PendingDisplayChange {
    const SCOPE: settings_model::Scope =
        settings_model::Scope::Shared(settings_store::hardware::DISPLAY_TRANSACTION_SECTION);
}

#[ui::tilo_resource::resource(kind = "settings.display-change")]
impl PendingDisplayChange {
    /// Stage a display change prepared outside the session, whole: a device step laying out a
    /// machine's monitors builds the transaction as the session's own Settings does. A view tries
    /// a change through the display's own actions instead.
    #[action(id = "request")]
    pub fn request(
        &mut self,
        #[param("The change asked for, as a display transaction")] change: DisplayTransaction,
    ) -> Result<(), tilo_resource::Refusal> {
        if change.id == 0 || change.state != DisplayTransactionState::Requested {
            return Err(tilo_resource::Refusal::new(
                "only a change asked for, with its id, is staged",
            ));
        }
        *self = Self {
            id: change.id,
            state: change.state,
            deadline_unix_ms: change.deadline_unix_ms,
            message: change.message,
        };
        Ok(())
    }
}

impl PendingDisplayChange {
    /// The change the section holds, if it holds one.
    #[must_use]
    pub fn transaction(&self) -> Option<DisplayTransaction> {
        (self.id != 0).then(|| DisplayTransaction {
            id: self.id,
            state: self.state,
            deadline_unix_ms: self.deadline_unix_ms,
            message: self.message.clone(),
            ..DisplayTransaction::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_display_is_placed_by_its_own_action;

        phase placing_one_display_keeps_every_other_entry;
        {
            let mut prefs = SharedDisplayPrefs::default();
            prefs
                .place_display("laptop".into(), "panel".into(), 0, 0, None, None)
                .unwrap();
            prefs
                .place_display("phone".into(), "screen".into(), 3200, 0, None, None)
                .unwrap();
            prefs
                .place_display("phone".into(), "screen".into(), 3200, 100, None, None)
                .unwrap();
            let phone = prefs
                .placement
                .find(&ui::tilo_resource::Id::from_raw("phone/screen"))
                .unwrap();
            assert_eq!((phone.x, phone.y, phone.placed), (3200, 100, true));
            assert_eq!(prefs.placement.len(), 2, "placing one display keeps the other's");

            let value = serde_json::to_value(&prefs).unwrap();
            assert_eq!(
                value["placement"].as_object().map(serde_json::Map::len),
                Some(2),
                "the document keeps each display under its own key: {value}"
            );
            assert_eq!(
                serde_json::from_value::<SharedDisplayPrefs>(value.clone()).unwrap(),
                prefs
            );
            let document = serde_json::Map::from_iter([(PLACEMENTS_SECTION.into(), value)]);
            assert_eq!(placements_in(&document), prefs.placement);
        }

        phase an_out_of_bounds_place_is_refused;
        {
            let mut prefs = SharedDisplayPrefs::default();
            for (device, x, order) in [
                (String::new(), 0, None),
                ("laptop".to_owned(), 2_000_000, None),
                ("laptop".to_owned(), 0, Some(i64::MIN)),
            ] {
                assert!(
                    prefs
                        .place_display(device, "panel".into(), x, 0, order, None)
                        .is_err()
                );
            }
            assert!(prefs.placement.is_empty(), "a refused place changes nothing");
        }
    }
}
