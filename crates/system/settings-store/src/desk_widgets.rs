//! The person's desk widgets: which ones they keep, when each shows itself
//! and which edge of the display it waits at.
//!
//! A widget is something the person added (music, the people in this
//! workspace, a calendar, the system monitor). This is the one place widgets
//! are chosen: the top bar keeps only the time, and has no widgets of its
//! own. A widget is never pushed at them: while they type it is away, and
//! it comes back in a quiet edge form when they pause, when they reach for
//! its edge, or when they ask (Super). Each widget carries one choice of
//! when it shows ([`ShowMode`]) and one edge ([`DockEdge`]).
//!
//! Settings → Appearance edits the choices; the compositor reads them
//! through [`DeskWidgetPrefs::from_document`], and dragging a widget to
//! another edge writes [`WidgetPrefs::edge`] back through `PREFS`.
//!
//! ```json
//! "desk_widgets": {
//!   "music": { "enabled": true, "show": "pause", "edge": "bottom_right", "always_on_strips": [] },
//!   "team":  { "enabled": true, "show": "pause", "edge": "right", "always_on_strips": [] },
//!   "calendar": { "enabled": false, "show": "pause", "edge": "right", "always_on_strips": [] },
//!   "system_monitor": { "enabled": false, "show": "ask", "edge": "bottom_left", "always_on_strips": [] }
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

    /// The edge a widget waits at until the person moves it. None uses the
    /// top edge: the top bar and a maximized window's revealed bar live
    /// there.
    #[must_use]
    pub const fn default_edge(self) -> DockEdge {
        match self {
            Self::Music => DockEdge::BottomRight,
            Self::Team | Self::Calendar => DockEdge::Right,
            Self::SystemMonitor => DockEdge::BottomLeft,
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

/// Where a widget waits. Corners and the side and bottom edges; never the
/// top edge.
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

/// One widget's choices.
#[cfg_attr(feature = "model", derive(tilo_resource::Model))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct WidgetPrefs {
    /// Whether the person keeps this widget on their desk.
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub show: ShowMode,
    pub edge: DockEdge,
    /// The strips (by id) [`ShowMode::Always`] applies to; empty is every
    /// strip.
    #[serde(default)]
    pub always_on_strips: Vec<u64>,
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
            edge: id.default_edge(),
            always_on_strips: Vec::new(),
        }
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
                "team": { "show": "ask", "edge": "left" },
                "music": { "show": "never-heard-of-it" }
            }
        });
        let prefs = DeskWidgetPrefs::from_document(document.as_object().unwrap());
        assert_eq!(prefs.music, WidgetPrefs::default_for(WidgetId::Music));
        assert_eq!(prefs.team.show, ShowMode::Ask);
        assert_eq!(prefs.team.edge, DockEdge::Left);
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
                "calendar": { "enabled": true, "show": "always", "edge": "left" },
                "system_monitor": { "enabled": true, "edge": "bottom" }
            }
        });
        let prefs = DeskWidgetPrefs::from_document(document.as_object().unwrap());
        assert!(prefs.calendar.enabled);
        assert_eq!(prefs.calendar.show, ShowMode::Always);
        assert_eq!(prefs.calendar.edge, DockEdge::Left);
        assert!(prefs.system_monitor.enabled);
        assert_eq!(prefs.system_monitor.edge, DockEdge::Bottom);
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
        let prefs = WidgetPrefs {
            enabled: false,
            show: ShowMode::Always,
            edge: DockEdge::BottomLeft,
            always_on_strips: vec![3],
        };
        let value = serde_json::to_value(&prefs).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "enabled": false, "show": "always", "edge": "bottom_left", "always_on_strips": [3]
            })
        );
        assert_eq!(serde_json::from_value::<WidgetPrefs>(value).unwrap(), prefs);
    }
}
