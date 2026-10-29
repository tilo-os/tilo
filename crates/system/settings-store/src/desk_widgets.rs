//! The person's desk widgets: which ones they keep, when each shows itself,
//! where on each display it rests and how wide its card is.
//!
//! A widget is something the person added (music, the people in this
//! workspace, a calendar, the system monitor). This is the one place widgets
//! are chosen: the top bar keeps only the time, and has no widgets of its
//! own. A widget is never pushed at them: while they type it is away, and
//! it comes back in its small resting form when they pause, when they reach
//! for it, or when they ask (Super). Each widget carries one choice of when
//! it shows ([`ShowMode`]), where it rests on each display it has been on
//! ([`WidgetPrefs::places`]) and how wide its card is ([`WidgetPrefs::width`]).
//!
//! Settings → Appearance edits the choices; the compositor reads them
//! through [`DeskWidgetPrefs::from_document`], and the desk writes back
//! where a widget was dropped, its width after a resize, and its removal.
//!
//! ```json
//! "desk_widgets": {
//!   "music": {
//!     "enabled": true, "show": "pause", "always_on_strips": [],
//!     "places": [{ "display": "edid:Dell|U2720Q|CN0", "x": 1.0, "y": 0.4 }],
//!     "width": 360.0
//!   }
//! }
//! ```

use serde::{Deserialize, Serialize};

use crate::Key;

/// Settings section holding every desk widget's choices.
pub const SECTION: &str = "desk_widgets";

/// One of the widgets the desk offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WidgetId {
    /// What is playing, and play/pause/next, whichever workspace the player
    /// runs in.
    Music,
    /// The people in this workspace: who they are, what they say they are
    /// doing and where they are.
    Team,
    /// Today's date at a glance; the month around it when open.
    Calendar,
    /// How busy the machine is: processor, memory and temperature.
    SystemMonitor,
}

impl WidgetId {
    pub const ALL: [Self; 4] = [Self::Music, Self::Team, Self::Calendar, Self::SystemMonitor];

    /// The widget's leaf in [`SECTION`].
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Music => "music",
            Self::Team => "team",
            Self::Calendar => "calendar",
            Self::SystemMonitor => "system_monitor",
        }
    }

    /// Where a widget rests until the person moves it, as a [`Place`]'s
    /// fractions: music in the bottom-right corner, the people and the date
    /// down the right edge, the machine's load bottom left. None rests at
    /// the top: the top bar lives there.
    #[must_use]
    pub const fn default_place(self) -> (f64, f64) {
        match self {
            Self::Music => (1.0, 1.0),
            Self::Team => (1.0, 0.5),
            Self::Calendar => (1.0, 0.25),
            Self::SystemMonitor => (0.0, 1.0),
        }
    }

    /// Whether a person who never chose has this widget on their desk.
    /// Music and the people here come only when there is something to show;
    /// a calendar or the machine's load is there all the time, so the
    /// person adds those themselves.
    #[must_use]
    pub const fn on_by_default(self) -> bool {
        matches!(self, Self::Music | Self::Team)
    }

    /// When a widget nobody has configured shows itself. The machine's load
    /// is something to look up, not to be shown at every pause.
    #[must_use]
    pub const fn default_show(self) -> ShowMode {
        match self {
            Self::SystemMonitor => ShowMode::Ask,
            Self::Music | Self::Team | Self::Calendar => ShowMode::Pause,
        }
    }
}

/// When a widget shows itself in its quiet edge form.
#[cfg_attr(feature = "model", derive(tilo_resource::Model))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShowMode {
    /// Away while the person types; back when they pause, reach for its
    /// edge or ask.
    #[default]
    Pause,
    /// Always in its edge form on the strips in
    /// [`WidgetPrefs::always_on_strips`] (every strip when that is empty);
    /// elsewhere as [`Self::Pause`].
    Always,
    /// Only when asked: Super, or reaching for its edge.
    Ask,
}

impl ShowMode {
    pub const ALL: [Self; 3] = [Self::Pause, Self::Always, Self::Ask];

    /// Row label in Settings.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pause => "Show when I pause",
            Self::Always => "Always show on this strip",
            Self::Ask => "Only when I ask",
        }
    }
}

/// Where something kept at the screen's edges waits (the pinned people
/// overlay). Corners and the side and bottom edges; never the top edge.
#[cfg_attr(feature = "model", derive(tilo_resource::Model))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DockEdge {
    Left,
    Right,
    Bottom,
    BottomLeft,
    BottomRight,
}

impl DockEdge {
    pub const ALL: [Self; 5] = [
        Self::Left,
        Self::Right,
        Self::Bottom,
        Self::BottomLeft,
        Self::BottomRight,
    ];

    /// Row label in Settings.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Left => "Left edge",
            Self::Right => "Right edge",
            Self::Bottom => "Bottom edge",
            Self::BottomLeft => "Bottom-left corner",
            Self::BottomRight => "Bottom-right corner",
        }
    }
}

/// Where a widget rests on one display: its resting form's place in the
/// display's work area, as fractions of the room it can move in there (0
/// against the left or top margin, 1 against the right or bottom). A
/// widget dropped against an edge or into a corner keeps to it when the
/// display's resolution changes, and one dropped in the open keeps its
/// proportion.
#[cfg_attr(feature = "model", derive(tilo_resource::Model))]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Place {
    /// The display, by its monitor's stable identity (the EDID token the
    /// display partition knows it by, `output:N` without one), so a place
    /// survives the monitor being unplugged, replugged or moved to
    /// another port.
    #[serde(default)]
    pub display: String,
    #[serde(default)]
    #[cfg_attr(feature = "model", model(range = 0.0..=1.0))]
    pub x: f64,
    #[serde(default)]
    #[cfg_attr(feature = "model", model(range = 0.0..=1.0))]
    pub y: f64,
}

/// One widget's choices.
#[cfg_attr(feature = "model", derive(tilo_resource::Model))]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct WidgetPrefs {
    /// Whether the person keeps this widget on their desk.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// When the widget shows itself.
    #[serde(default)]
    pub show: ShowMode,
    /// The strips (by id) [`ShowMode::Always`] applies to; empty is every
    /// strip.
    #[serde(default)]
    pub always_on_strips: Vec<u64>,
    /// Where it rests on each display it was dropped on, the display it
    /// was last dropped on first: it lives there while that display is
    /// connected, and on the next connected one in this list (or the
    /// primary display, at its own default place) while it is not.
    #[serde(default)]
    pub places: Vec<Place>,
    /// Its open card's width, in logical pixels, after the person resized
    /// it; unset is the widget's own default. Each widget keeps it within
    /// its own narrowest and widest, all inside this range.
    #[serde(default)]
    #[cfg_attr(feature = "model", model(range = 160.0..=640.0))]
    pub width: Option<f64>,
}

crate::read_through_json_value!(WidgetPrefs);

const fn default_true() -> bool {
    true
}

impl WidgetPrefs {
    /// A widget nobody has configured yet.
    #[must_use]
    pub fn default_for(id: WidgetId) -> Self {
        Self {
            enabled: id.on_by_default(),
            show: id.default_show(),
            always_on_strips: Vec::new(),
            places: Vec::new(),
            width: None,
        }
    }

    /// Where it rests on the first of `connected` displays it has a place
    /// on, in the order it was dropped there (most recent first): that
    /// display's identity and the place's fractions.
    #[must_use]
    pub fn place_on<'a>(&'a self, connected: &[&str]) -> Option<&'a Place> {
        self.places
            .iter()
            .find(|place| connected.contains(&place.display.as_str()))
    }

    /// Dropped at `x`, `y` on `display`: that display becomes the one it
    /// lives on, and its place there is this one.
    pub fn drop_on(&mut self, display: &str, x: f64, y: f64) {
        self.places.retain(|place| place.display != display);
        self.places.insert(
            0,
            Place {
                display: display.to_string(),
                x: x.clamp(0.0, 1.0),
                y: y.clamp(0.0, 1.0),
            },
        );
    }

    /// Whether [`ShowMode::Always`] holds on `strip`.
    #[must_use]
    pub fn always_on(&self, strip: Option<u64>) -> bool {
        self.show == ShowMode::Always
            && (self.always_on_strips.is_empty()
                || strip.is_some_and(|strip| self.always_on_strips.contains(&strip)))
    }
}

/// The Music widget's choices.
pub const MUSIC: Key<WidgetPrefs> = Key::new(SECTION, "music", || {
    WidgetPrefs::default_for(WidgetId::Music)
});
/// The Team widget's choices.
pub const TEAM: Key<WidgetPrefs> =
    Key::new(SECTION, "team", || WidgetPrefs::default_for(WidgetId::Team));
/// The Calendar widget's choices.
pub const CALENDAR: Key<WidgetPrefs> = Key::new(SECTION, "calendar", || {
    WidgetPrefs::default_for(WidgetId::Calendar)
});
/// The System monitor widget's choices.
pub const SYSTEM_MONITOR: Key<WidgetPrefs> = Key::new(SECTION, "system_monitor", || {
    WidgetPrefs::default_for(WidgetId::SystemMonitor)
});

/// The people overlay: the list of everyone here, pinned out of the bar's
/// people menu to wait at an edge like a widget, and dragged between edges
/// the same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeopleOverlayPrefs {
    /// Whether the person pinned it to their desk.
    #[serde(default)]
    pub pinned: bool,
    #[serde(default = "people_overlay_edge")]
    pub edge: DockEdge,
}

const fn people_overlay_edge() -> DockEdge {
    DockEdge::Right
}

impl Default for PeopleOverlayPrefs {
    fn default() -> Self {
        Self {
            pinned: false,
            edge: people_overlay_edge(),
        }
    }
}

/// The people overlay's choices.
pub const PEOPLE_OVERLAY: Key<PeopleOverlayPrefs> =
    Key::new(SECTION, "people_overlay", PeopleOverlayPrefs::default);

/// The key holding `id`'s choices.
#[must_use]
pub const fn prefs_key(id: WidgetId) -> &'static Key<WidgetPrefs> {
    match id {
        WidgetId::Music => &MUSIC,
        WidgetId::Team => &TEAM,
        WidgetId::Calendar => &CALENDAR,
        WidgetId::SystemMonitor => &SYSTEM_MONITOR,
    }
}

/// Every widget's choices, as one read.
#[derive(Clone, Debug, PartialEq)]
pub struct DeskWidgetPrefs {
    pub music: WidgetPrefs,
    pub team: WidgetPrefs,
    pub calendar: WidgetPrefs,
    pub system_monitor: WidgetPrefs,
}

impl Default for DeskWidgetPrefs {
    fn default() -> Self {
        Self {
            music: WidgetPrefs::default_for(WidgetId::Music),
            team: WidgetPrefs::default_for(WidgetId::Team),
            calendar: WidgetPrefs::default_for(WidgetId::Calendar),
            system_monitor: WidgetPrefs::default_for(WidgetId::SystemMonitor),
        }
    }
}

impl DeskWidgetPrefs {
    /// The choices in an already loaded settings document; anything unset
    /// or unreadable reads as its default.
    #[must_use]
    pub fn from_document(document: &serde_json::Map<String, serde_json::Value>) -> Self {
        Self {
            music: MUSIC.from_document(document),
            team: TEAM.from_document(document),
            calendar: CALENDAR.from_document(document),
            system_monitor: SYSTEM_MONITOR.from_document(document),
        }
    }

    #[must_use]
    pub const fn get(&self, id: WidgetId) -> &WidgetPrefs {
        match id {
            WidgetId::Music => &self.music,
            WidgetId::Team => &self.team,
            WidgetId::Calendar => &self.calendar,
            WidgetId::SystemMonitor => &self.system_monitor,
        }
    }

    pub fn get_mut(&mut self, id: WidgetId) -> &mut WidgetPrefs {
        match id {
            WidgetId::Music => &mut self.music,
            WidgetId::Team => &mut self.team,
            WidgetId::Calendar => &mut self.calendar,
            WidgetId::SystemMonitor => &mut self.system_monitor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_and_partial_choices_read_as_their_defaults() {
        let document = serde_json::json!({
            "desk_widgets": {
                "team": { "show": "ask", "places": [{ "display": "edid:A|B|1", "x": 0.0, "y": 0.5 }] },
                "music": { "show": "never-heard-of-it" }
            }
        });
        let prefs = DeskWidgetPrefs::from_document(document.as_object().unwrap());
        assert_eq!(prefs.music, WidgetPrefs::default_for(WidgetId::Music));
        assert_eq!(prefs.team.show, ShowMode::Ask);
        assert_eq!(
            prefs.team.place_on(&["edid:A|B|1"]).map(|p| (p.x, p.y)),
            Some((0.0, 0.5))
        );
        assert!(prefs.team.enabled, "kept unless turned off");
        assert_eq!(
            DeskWidgetPrefs::from_document(&serde_json::Map::new()),
            DeskWidgetPrefs::default()
        );
    }

    /// The calendar and the system monitor, once the top bar's own
    /// widgets, are desk widgets the person adds: off until chosen, then
    /// read and written like any other.
    #[test]
    fn the_calendar_and_system_monitor_are_desk_widgets_the_person_adds() {
        let prefs = DeskWidgetPrefs::default();
        assert!(prefs.music.enabled && prefs.team.enabled);
        assert!(!prefs.calendar.enabled, "added by the person");
        assert!(!prefs.system_monitor.enabled, "added by the person");
        assert_eq!(prefs.system_monitor.show, ShowMode::Ask);
        let document = serde_json::json!({
            "desk_widgets": {
                "calendar": { "enabled": true, "show": "always", "width": 420.0 },
                "system_monitor": { "enabled": true }
            }
        });
        let prefs = DeskWidgetPrefs::from_document(document.as_object().unwrap());
        assert!(prefs.calendar.enabled);
        assert_eq!(prefs.calendar.show, ShowMode::Always);
        assert_eq!(prefs.calendar.width, Some(420.0));
        assert!(prefs.system_monitor.enabled);
        for id in WidgetId::ALL {
            assert_eq!(prefs_key(id).name(), id.key());
        }
    }

    #[test]
    fn always_is_scoped_to_the_strips_it_names() {
        let mut prefs = WidgetPrefs::default_for(WidgetId::Music);
        assert!(!prefs.always_on(Some(1)));
        prefs.show = ShowMode::Always;
        assert!(prefs.always_on(Some(1)), "no strips named: every strip");
        prefs.always_on_strips = vec![7];
        assert!(prefs.always_on(Some(7)));
        assert!(!prefs.always_on(Some(1)));
        assert!(!prefs.always_on(None));
    }

    #[test]
    fn the_people_overlay_keeps_its_pin_and_edge_beside_the_widgets() {
        assert_eq!(
            PEOPLE_OVERLAY.from_document(&serde_json::Map::new()),
            PeopleOverlayPrefs::default()
        );
        let document = serde_json::json!({
            "desk_widgets": { "people_overlay": { "pinned": true, "edge": "bottom_left" } }
        });
        let prefs = PEOPLE_OVERLAY.from_document(document.as_object().unwrap());
        assert!(prefs.pinned);
        assert_eq!(prefs.edge, DockEdge::BottomLeft);
    }

    #[test]
    fn the_persisted_shape_round_trips() {
        let mut prefs = WidgetPrefs {
            enabled: false,
            show: ShowMode::Always,
            always_on_strips: vec![3],
            places: Vec::new(),
            width: Some(300.0),
        };
        prefs.drop_on("edid:A|B|1", 0.25, 1.5);
        let value = serde_json::to_value(&prefs).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "enabled": false, "show": "always", "always_on_strips": [3],
                "places": [{ "display": "edid:A|B|1", "x": 0.25, "y": 1.0 }],
                "width": 300.0
            })
        );
        assert_eq!(serde_json::from_value::<WidgetPrefs>(value).unwrap(), prefs);
    }
}

#[cfg(test)]
mod place_tests {
    use super::*;

    /// A widget lives on the display it was last dropped on while that one
    /// is connected, keeps its place on every other display it was on, and
    /// goes back to it when it is plugged in again.
    #[test]
    fn a_widget_keeps_a_place_per_display_and_goes_home_when_replugged() {
        let mut prefs = WidgetPrefs::default_for(WidgetId::Music);
        prefs.drop_on("panel", 0.0, 1.0);
        prefs.drop_on("monitor", 0.5, 0.5);
        let at = |prefs: &WidgetPrefs, connected: &[&str]| {
            prefs
                .place_on(connected)
                .map(|p| (p.display.clone(), p.x, p.y))
        };
        assert_eq!(
            at(&prefs, &["panel", "monitor"]),
            Some(("monitor".into(), 0.5, 0.5))
        );
        assert_eq!(
            at(&prefs, &["panel"]),
            Some(("panel".into(), 0.0, 1.0)),
            "monitor unplugged: back where it was on the panel"
        );
        assert_eq!(at(&prefs, &["other"]), None, "a stranger: its default");
        prefs.drop_on("panel", 1.0, 0.0);
        assert_eq!(prefs.places.len(), 2, "one place per display");
        assert_eq!(at(&prefs, &["panel", "monitor"]).unwrap().0, "panel");
    }
}
