//! An app's toolbar is slots in the window's bar, never a row it draws.
//!
//! The desktop draws one bar per window (`Workspace → Strip → WindowGroup →
//! Tab`): the app's icon, its tabs, the window's controls. What an app adds
//! to it it declares here, as slots the bar lays out and draws with the
//! system's own controls:
//!
//! - **navigation:** the app's sidebar toggle, back and forward, and
//!   whether each can go;
//! - **location:** where the window is and can go, the label the bar shows
//!   at rest (a page's domain, a folder's path, a document's name), and
//!   whether it can be edited. An editable location turns into a field
//!   holding the full value, all of it selected, on a click or Ctrl+L, with a
//!   go button at its end; Enter or the go button commits the text to the
//!   app, Esc puts the location back. A location with a hierarchy (a
//!   folder's path) declares its levels as [`LocationSegment`]s: the bar
//!   draws them as a breadcrumb with drawn chevrons, and a click on a level
//!   commits that level's value. Its own picture ([`ToolbarImage`], a page's
//!   favicon) stands at the bar's leading edge in place of the app's icon.
//!   While the field is edited the app hears each change of its text
//!   ([`ToolbarEvent::Edit`]) and may answer with [`LocationSuggestion`]s,
//!   which the bar lists under the field;
//! - **actions:** icon buttons (reload, bookmark…), at most [`MAX_ACTIONS`]
//!   of them in the row; the rest, and those the row has no room for, are
//!   in the menu of a trailing overflow button.
//!
//! The tab label (what tells two tabs apart) stays the window's title; the
//! location is where you are. A window whose app declares no slots shows
//! its title, as before.
//!
//! The bar's own controls reach an app in its own process the same way: the
//! app says what its window offers them ([`WindowOffer`]: what the search
//! acts on in place and its count, and its History control), and the bar
//! hands it what the person did with them as [`ToolbarEvent`]s (a query, a
//! step, a close, the History control). The app answers through its own
//! widgets, as the window does for an app in its process, and draws neither
//! control itself.
//!
//! An app declares its current slots with [`declare`] whenever they change
//! (an equal declaration is free) and reads what the person did in the bar
//! with [`take_events`]. The app runner carries both over the app's
//! authenticated `shell-ui` connection. While the desktop's bar shows the
//! slots ([`is_hosted`]) the app draws no toolbar of its own; outside a
//! Tilo session (a window of its own, a test) nothing hosts them and the app
//! keeps its own row.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// The most actions the row shows as buttons; the rest go in its overflow
/// menu.
pub const MAX_ACTIONS: usize = 3;

/// The most actions one window declares, buttons and overflow together.
pub const MAX_DECLARED_ACTIONS: usize = 12;

/// The most levels a location's breadcrumb carries.
pub const MAX_SEGMENTS: usize = 64;

/// The most suggestions the bar lists under an edited location.
pub const MAX_SUGGESTIONS: usize = 8;

/// Largest encoded location picture, in bytes. A favicon is a few hundred.
pub const MAX_IMAGE_BYTES: usize = 64 * 1024;

/// Largest location picture, in pixels (edge squared).
pub const MAX_IMAGE_PIXELS: u32 = 128 * 128;

/// Longest label, value or name the bar takes, in bytes. Longer text is cut
/// at a character boundary: the bar elides far sooner, and the wire never
/// carries a page's worth of text per frame.
pub const MAX_TEXT_BYTES: usize = 8 * 1024;

/// What an app shows in its window's bar.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowToolbar {
    /// The app's sidebar toggle, and whether the sidebar is open: the
    /// window's navigation, leading the row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar: Option<bool>,
    /// Back and forward, for an app that keeps a trail of places.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub navigation: Option<ToolbarNavigation>,
    /// Where the window is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<ToolbarLocation>,
    /// The app's most used actions, at most [`MAX_ACTIONS`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ToolbarAction>,
}

/// Back and forward: which of them can go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolbarNavigation {
    pub back: bool,
    pub forward: bool,
}

/// Where the window is and can go.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolbarLocation {
    /// What the bar shows at rest: a page's domain, a folder, a document.
    pub label: String,
    /// What an edit starts from: the whole address. Empty means the label.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
    /// The field's hint while it is empty ("Search or enter address").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub placeholder: String,
    /// The location's own icon, by its icon-set name (a lock for a secure
    /// page); the bar shows the app's icon when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// A click or Ctrl+L turns it into a field.
    #[serde(default)]
    pub editable: bool,
    /// The field's accessible name ("Address and search").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The go button's name ("Go").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub go_name: String,
    /// The location's levels, outermost first, for a breadcrumb in place
    /// of the label (a folder's path). Empty: the label.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<LocationSegment>,
    /// The location's own picture (a page's favicon), shown at the bar's
    /// leading edge instead of the app's icon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ToolbarImage>,
    /// What the bar lists under the field while it is edited, best first:
    /// the app's answer to the text it heard last ([`ToolbarEvent::Edit`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<LocationSuggestion>,
}

/// One level of a location's hierarchy.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocationSegment {
    /// What the breadcrumb shows ("Downloads").
    pub label: String,
    /// What a click on it commits ([`ToolbarEvent::Commit`]): the level's
    /// full location ("/home/ana/Downloads").
    pub value: String,
}

/// A location's picture, as PNG bytes.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolbarImage {
    pub png: Vec<u8>,
}

impl std::fmt::Debug for ToolbarImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolbarImage")
            .field("bytes", &self.png.len())
            .finish()
    }
}

impl ToolbarImage {
    /// The picture as the bar paints it: decoded once, at most
    /// [`MAX_IMAGE_PIXELS`]. `None` for bytes that are not such a PNG.
    #[must_use]
    pub fn app_image(&self) -> Option<crate::icons::AppImage> {
        if self.png.is_empty() || self.png.len() > MAX_IMAGE_BYTES {
            return None;
        }
        let decoded =
            crate::png::decode_rgba8_within(&self.png, u64::from(MAX_IMAGE_PIXELS)).ok()?;
        let pixels = crate::raster::rgba8(decoded.rgba, decoded.width, decoded.height);
        let key = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            self.png.hash(&mut hasher);
            format!("window-toolbar-image:{:016x}", hasher.finish())
        };
        Some(crate::icons::AppImage::new(DecodedImage { key, pixels }))
    }
}

/// A decoded [`ToolbarImage`].
struct DecodedImage {
    key: String,
    pixels: vello::peniko::ImageData,
}

impl crate::icons::AppImageSource for DecodedImage {
    fn key(&self) -> &str {
        &self.key
    }

    fn pixels(&self, _device_px: u32) -> Option<vello::peniko::ImageData> {
        Some(self.pixels.clone())
    }
}

/// One suggestion under an edited location.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocationSuggestion {
    /// What the row says ("rust-lang.org" or "Search for rust").
    pub label: String,
    /// What choosing it commits ([`ToolbarEvent::Commit`]).
    pub value: String,
    /// Its icon, by its icon-set name (a clock for history, a magnifier for
    /// a search).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

impl ToolbarLocation {
    /// The text an edit starts from.
    #[must_use]
    pub fn edit_value(&self) -> &str {
        if self.value.is_empty() {
            &self.label
        } else {
            &self.value
        }
    }
}

/// One action button.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolbarAction {
    /// What the app gets back in [`ToolbarEvent::Action`].
    pub id: String,
    /// The icon, by its icon-set name ([`crate::icons::Icon::from_name`]).
    pub icon: String,
    /// The outcome it names, for its tooltip and assistive technology.
    pub name: String,
    /// It can act now.
    #[serde(default = "enabled")]
    pub enabled: bool,
    /// It is a toggle that is on (a bookmarked page's star).
    #[serde(default)]
    pub on: bool,
}

fn enabled() -> bool {
    true
}

/// What the person did with the app's slots in the bar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolbarEvent {
    /// The sidebar toggle was pressed.
    ToggleSidebar,
    Back,
    Forward,
    /// The location field was committed (Enter or the go button) with this
    /// text, exactly as typed; or a breadcrumb level or a suggestion was
    /// chosen, with its value.
    Commit {
        text: String,
    },
    /// The edited location's text is now this: the app may answer with
    /// suggestions.
    Edit {
        text: String,
    },
    /// An action button was pressed.
    Action {
        id: String,
    },
    /// The window's search is open on the app with this query (empty when
    /// nothing is typed yet): highlight a document's matches, or narrow a
    /// list. Sent when the search opens and whenever the query changes.
    Search {
        query: String,
    },
    /// Step to the next (or previous) match in the document.
    SearchStep {
        forward: bool,
    },
    /// The window's search closed: stop highlighting or narrowing.
    SearchClose,
    /// The window's History control was pressed: open History, or close it
    /// while it shows.
    History,
}

/// What a window's search acts on in place, as its app offers it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchOffer {
    /// Nothing: the bar's search opens the platform search narrowed to the
    /// app.
    #[default]
    Nothing,
    /// A document: matches are highlighted and stepped through ("3 of 12").
    Document,
    /// A list: it narrows to what matches ("12 of 40").
    List,
}

/// The search's count: in a document the current match of all of them, in
/// a list the rows shown of all rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchCount {
    pub shown: u32,
    pub total: u32,
}

/// The window's History control, as its app offers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryOffer {
    /// History is closed; the control opens it.
    Closed,
    /// History shows; the control reads as on and closes it.
    Open,
}

/// What an app's window offers the bar's own controls: its search in place
/// and the count of the open search, and its History control. Sent whenever
/// it changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowOffer {
    #[serde(default)]
    pub search: SearchOffer,
    /// The open search's count; none before anything is typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<SearchCount>,
    /// The History control; none where the app has no History.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<HistoryOffer>,
}

impl SearchOffer {
    /// What `root`'s widgets offer the window's search.
    #[must_use]
    pub fn of(root: &mut dyn crate::widget::Widget) -> Self {
        crate::widget::InPlace::of(root).into()
    }
}

impl From<crate::widget::InPlace> for SearchOffer {
    fn from(in_place: crate::widget::InPlace) -> Self {
        match in_place {
            crate::widget::InPlace::Nothing => Self::Nothing,
            crate::widget::InPlace::Document => Self::Document,
            crate::widget::InPlace::List => Self::List,
        }
    }
}

impl From<SearchOffer> for crate::widget::InPlace {
    fn from(offer: SearchOffer) -> Self {
        match offer {
            SearchOffer::Nothing => Self::Nothing,
            SearchOffer::Document => Self::Document,
            SearchOffer::List => Self::List,
        }
    }
}

impl From<crate::widget::FilterCount> for SearchCount {
    fn from(count: crate::widget::FilterCount) -> Self {
        let clamp = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        Self {
            shown: clamp(count.shown),
            total: clamp(count.total),
        }
    }
}

impl From<SearchCount> for crate::widget::FilterCount {
    fn from(count: SearchCount) -> Self {
        Self::new(count.shown as usize, count.total as usize)
    }
}

impl From<crate::widget::HistoryControl> for HistoryOffer {
    fn from(control: crate::widget::HistoryControl) -> Self {
        match control {
            crate::widget::HistoryControl::Closed => Self::Closed,
            crate::widget::HistoryControl::Open => Self::Open,
        }
    }
}

impl From<HistoryOffer> for crate::widget::HistoryControl {
    fn from(offer: HistoryOffer) -> Self {
        match offer {
            HistoryOffer::Closed => Self::Closed,
            HistoryOffer::Open => Self::Open,
        }
    }
}

impl WindowToolbar {
    /// Whether there is anything to show.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sidebar.is_none()
            && self.navigation.is_none()
            && self.location.is_none()
            && self.actions.is_empty()
    }

    /// The declaration as the bar takes it: at most
    /// [`MAX_DECLARED_ACTIONS`] actions, [`MAX_SEGMENTS`] levels and
    /// [`MAX_SUGGESTIONS`] suggestions, every text at most
    /// [`MAX_TEXT_BYTES`], a picture at most [`MAX_IMAGE_BYTES`], no icon
    /// the set does not have.
    #[must_use]
    pub fn bounded(mut self) -> Self {
        self.actions
            .retain(|action| crate::icons::Icon::from_name(&action.icon).is_some());
        self.actions.truncate(MAX_DECLARED_ACTIONS);
        for action in &mut self.actions {
            cut(&mut action.id);
            cut(&mut action.name);
        }
        if let Some(location) = &mut self.location {
            for text in [
                &mut location.label,
                &mut location.value,
                &mut location.placeholder,
                &mut location.name,
                &mut location.go_name,
            ] {
                cut(text);
            }
            if location
                .icon
                .as_deref()
                .is_some_and(|icon| crate::icons::Icon::from_name(icon).is_none())
            {
                location.icon = None;
            }
            location.segments.truncate(MAX_SEGMENTS);
            for segment in &mut location.segments {
                cut(&mut segment.label);
                cut(&mut segment.value);
            }
            location.suggestions.truncate(MAX_SUGGESTIONS);
            for suggestion in &mut location.suggestions {
                cut(&mut suggestion.label);
                cut(&mut suggestion.value);
                if suggestion
                    .icon
                    .as_deref()
                    .is_some_and(|icon| crate::icons::Icon::from_name(icon).is_none())
                {
                    suggestion.icon = None;
                }
            }
            if location
                .image
                .as_ref()
                .is_some_and(|image| image.png.is_empty() || image.png.len() > MAX_IMAGE_BYTES)
            {
                location.image = None;
            }
        }
        self
    }
}

impl ToolbarEvent {
    /// The event as an app takes it: a committed text at most
    /// [`MAX_TEXT_BYTES`].
    #[must_use]
    pub fn bounded(mut self) -> Self {
        match &mut self {
            Self::Commit { text }
            | Self::Edit { text }
            | Self::Action { id: text }
            | Self::Search { query: text } => cut(text),
            Self::ToggleSidebar
            | Self::Back
            | Self::Forward
            | Self::SearchStep { .. }
            | Self::SearchClose
            | Self::History => {}
        }
        self
    }
}

fn cut(text: &mut String) {
    if text.len() > MAX_TEXT_BYTES {
        let mut end = MAX_TEXT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
}

/// Upper bound on events an app has not read yet. A held key repeating
/// faster than the app draws must not queue without end.
const MAX_PENDING: usize = 32;

thread_local! {
    static DECLARED: RefCell<Option<WindowToolbar>> = const { RefCell::new(None) };
    static REVISION: Cell<u64> = const { Cell::new(0) };
    static EVENTS: RefCell<VecDeque<ToolbarEvent>> = const { RefCell::new(VecDeque::new()) };
    static HOSTED: Cell<bool> = const { Cell::new(false) };
    static EDIT_REQUESTED: Cell<bool> = const { Cell::new(false) };
    static SEARCH_REQUESTED: Cell<bool> = const { Cell::new(false) };
}

/// Declare the window's slots (`None`: none). Cheap when nothing changed.
pub fn declare(toolbar: Option<WindowToolbar>) {
    let toolbar = toolbar
        .map(WindowToolbar::bounded)
        .filter(|toolbar| !toolbar.is_empty());
    DECLARED.with(|declared| {
        let mut declared = declared.borrow_mut();
        if *declared != toolbar {
            *declared = toolbar;
            REVISION.with(|revision| revision.set(revision.get().wrapping_add(1)));
        }
    });
}

/// The slots last declared on this thread.
#[must_use]
pub fn declared() -> Option<WindowToolbar> {
    DECLARED.with(|declared| declared.borrow().clone())
}

/// Bumped by every [`declare`] that changed something, so a runner sends
/// a declaration once.
#[must_use]
pub fn revision() -> u64 {
    REVISION.with(Cell::get)
}

/// What the person did in the bar since the last call, oldest first.
#[must_use]
pub fn take_events() -> Vec<ToolbarEvent> {
    EVENTS.with(|events| events.borrow_mut().drain(..).collect())
}

/// Whether the desktop's bar shows this window's slots, so the app draws
/// no toolbar of its own.
#[must_use]
pub fn is_hosted() -> bool {
    HOSTED.with(Cell::get)
}

/// Ask the bar to turn the window's editable location into a field, as a
/// click on it does: the app's Ctrl+L (or F6) while the bar hosts its slots.
pub fn edit_location() {
    EDIT_REQUESTED.with(|cell| cell.set(true));
}

/// Whether the app asked to edit its location since the last call (the
/// runner, to tell the shell).
#[must_use]
pub fn take_edit_request() -> bool {
    EDIT_REQUESTED.with(|cell| cell.replace(false))
}

/// Ask the bar to open the window's search, as a click on its control does:
/// the app's Ctrl+F while the bar hosts its window and it has something to
/// search in place.
pub fn open_search() {
    SEARCH_REQUESTED.with(|cell| cell.set(true));
}

/// Whether the app asked to open the window's search since the last call
/// (the runner, to tell the shell).
#[must_use]
pub fn take_search_request() -> bool {
    SEARCH_REQUESTED.with(|cell| cell.replace(false))
}

/// Hand the app what the person did in the bar (the runner, from the
/// shell's message).
pub fn deliver(event: ToolbarEvent) {
    EVENTS.with(|events| {
        let mut events = events.borrow_mut();
        if events.len() < MAX_PENDING {
            events.push_back(event.bounded());
        }
    });
}

/// Whether a desktop bar shows the slots (the runner, as its shell
/// connection comes and goes). Returns whether that changed.
pub fn set_hosted(hosted: bool) -> bool {
    HOSTED.with(|cell| cell.replace(hosted) != hosted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn browser() -> WindowToolbar {
        WindowToolbar {
            sidebar: None,
            navigation: Some(ToolbarNavigation {
                back: true,
                forward: false,
            }),
            location: Some(ToolbarLocation {
                label: "duckduckgo.com".into(),
                value: "https://duckduckgo.com/?q=rust".into(),
                editable: true,
                ..ToolbarLocation::default()
            }),
            actions: vec![ToolbarAction {
                id: "reload".into(),
                icon: "refresh".into(),
                name: "Reload".into(),
                enabled: true,
                on: false,
            }],
        }
    }

    #[test]
    fn a_declaration_is_bounded_and_sent_once_per_change() {
        let mut many = browser();
        for n in 0..6 {
            many.actions.push(ToolbarAction {
                id: format!("a{n}"),
                icon: "star".into(),
                name: "Bookmark".into(),
                enabled: true,
                on: false,
            });
        }
        many.actions.push(ToolbarAction {
            id: "glyph".into(),
            icon: "not-an-icon".into(),
            name: "Nothing".into(),
            enabled: true,
            on: false,
        });
        for n in 6..20 {
            many.actions.push(ToolbarAction {
                id: format!("a{n}"),
                icon: "star".into(),
                name: "Bookmark".into(),
                enabled: true,
                on: false,
            });
        }
        let bounded = many.clone().bounded();
        assert_eq!(bounded.actions.len(), MAX_DECLARED_ACTIONS);
        assert!(bounded.actions.iter().all(|a| a.icon != "not-an-icon"));

        let before = revision();
        declare(Some(browser()));
        let once = revision();
        assert_ne!(before, once);
        declare(Some(browser()));
        assert_eq!(revision(), once, "an equal declaration changes nothing");
        declare(Some(WindowToolbar::default()));
        assert_eq!(declared(), None, "an empty declaration is none");
    }

    #[test]
    fn events_round_trip_the_wire_and_reach_the_app_in_order() {
        for event in [
            ToolbarEvent::Back,
            ToolbarEvent::Forward,
            ToolbarEvent::Commit {
                text: "example.org".into(),
            },
            ToolbarEvent::Action {
                id: "reload".into(),
            },
            ToolbarEvent::Search {
                query: "cat".into(),
            },
            ToolbarEvent::SearchStep { forward: false },
            ToolbarEvent::SearchClose,
            ToolbarEvent::History,
        ] {
            let wire = serde_json::to_string(&event).unwrap();
            assert_eq!(serde_json::from_str::<ToolbarEvent>(&wire).unwrap(), event);
            deliver(event);
        }
        let taken = take_events();
        assert_eq!(taken.len(), 8);
        assert_eq!(taken[0], ToolbarEvent::Back);
        assert!(take_events().is_empty());
        let wire = serde_json::to_string(&browser()).unwrap();
        assert_eq!(
            serde_json::from_str::<WindowToolbar>(&wire).unwrap(),
            browser()
        );
        assert_eq!(
            browser().location.unwrap().edit_value(),
            "https://duckduckgo.com/?q=rust"
        );
    }

    #[test]
    fn a_window_offer_round_trips_and_an_empty_one_reads_as_nothing() {
        let offer = WindowOffer {
            search: SearchOffer::List,
            count: Some(SearchCount {
                shown: 12,
                total: 40,
            }),
            history: Some(HistoryOffer::Open),
        };
        let wire = serde_json::to_string(&offer).unwrap();
        assert_eq!(serde_json::from_str::<WindowOffer>(&wire).unwrap(), offer);
        assert_eq!(
            serde_json::from_str::<WindowOffer>("{}").unwrap(),
            WindowOffer::default(),
            "an app with nothing to offer sends nothing but the frame"
        );
        assert_eq!(
            crate::widget::FilterCount::from(offer.count.unwrap()),
            crate::widget::FilterCount::new(12, 40)
        );
        assert!(!take_search_request());
        open_search();
        assert!(take_search_request());
        assert!(!take_search_request(), "one ask per Ctrl+F");
    }
}
