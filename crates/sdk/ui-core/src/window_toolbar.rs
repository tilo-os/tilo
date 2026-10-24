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
//!   app, Esc puts the location back (and the app hears the field hold its
//!   value again, as an [`ToolbarEvent::Edit`]). A location with a hierarchy (a
//!   folder's path) declares its levels as [`LocationSegment`]s: the bar
//!   draws them as a breadcrumb with drawn chevrons, and a click on a level
//!   commits that level's value. Its own picture ([`ToolbarImage`], a page's
//!   favicon) stands at the bar's leading edge in place of the app's icon.
//!   While the field is edited the app hears each change of its text
//!   ([`ToolbarEvent::Edit`]) and may answer with [`LocationSuggestion`]s,
//!   which the bar lists under the field. A location that is one of a few
//!   places (where a terminal's shell runs) declares them as
//!   [`LocationChoice`]s instead: the label reads as a picker, with a chevron,
//!   and a click opens the system's menu of them; choosing one commits its
//!   value;
//! - **leading actions:** icon buttons that act on where the window is,
//!   standing right after the navigation (a calendar's previous, next and
//!   today), at most [`MAX_LEADING`] of them;
//! - **views:** tabs switching between the app's views of the same
//!   thing (Month, Week, Day), trailing the location; where the row has no
//!   room for it, it folds into a views menu that names the chosen view and
//!   lists them all, the chosen one checked (only a row too narrow even for
//!   that lists them in the overflow menu). Choosing one sends its id as an
//!   [`ToolbarEvent::Action`];
//! - **actions:** icon buttons (reload, bookmark…), at most [`MAX_ACTIONS`]
//!   of them in the row; the rest, those the row has no room for and those
//!   declared for the menu ([`ToolbarAction::menu`]) are in the menu of a
//!   trailing overflow button. An action with items
//!   ([`ToolbarAction::items`]) is a menu button: a press lists its items
//!   (a document's Format or Insert commands), and in the overflow menu its
//!   items follow its name.
//!
//! Search is the bar's own control, never a slot: it finds in place what the
//! app's widgets offer ([`WindowOffer`]), or opens the platform search
//! narrowed to the app.
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
//!
//! An app its host runs in the host's own process (every app of the browser
//! desktop) makes the same calls: while the host runs it ([`enter`]), they
//! act on the window the host shows it in
//! ([`crate::hosted_window::HostedWindow`]) instead of the thread's, so apps
//! sharing a thread never see each other's slots.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// The most actions the row shows as buttons; the rest go in its overflow
/// menu.
pub const MAX_ACTIONS: usize = 3;

/// The most actions one window declares, buttons and overflow together.
pub const MAX_DECLARED_ACTIONS: usize = 12;

/// The most leading actions the row shows; a declaration's others are
/// dropped.
pub const MAX_LEADING: usize = 3;

/// The most choices a view switcher carries.
pub const MAX_VIEWS: usize = 8;

/// The most items one menu button lists.
pub const MAX_ITEMS: usize = 24;

/// The most levels a location's breadcrumb carries.
pub const MAX_SEGMENTS: usize = 64;

/// The most suggestions the bar lists under an edited location.
pub const MAX_SUGGESTIONS: usize = 8;

/// The most choices a location's picker lists.
pub const MAX_CHOICES: usize = 32;

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
    /// The sidebar toggle holds its place but cannot act: a view of the
    /// window that has no sidebar, so the row stands still as the views
    /// change. Only with [`Self::sidebar`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sidebar_unavailable: bool,
    /// Back and forward, for an app that keeps a trail of places.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub navigation: Option<ToolbarNavigation>,
    /// Where the window is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<ToolbarLocation>,
    /// The app's most used actions, at most [`MAX_ACTIONS`] in the row; the
    /// rest in its overflow menu.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ToolbarAction>,
    /// Actions on where the window is, right after the navigation (previous,
    /// next, today), at most [`MAX_LEADING`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub leading: Vec<ToolbarAction>,
    /// The app's views of what the window shows, one of them chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub views: Option<ToolbarViews>,
    /// A transient notice ([`notify`]), shown in the location's place while
    /// it is up. The window's notice, not a declaration's: [`shown`] puts
    /// it here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<ToolbarNotice>,
}

/// What just happened, said in the window's bar for a while: an operation's
/// result or failure ("Page 1 captured"). The bar shows it in the
/// location's place, its kind's icon first, and announces it. A notice that
/// asks the person to act carries what they can do: at most
/// [`MAX_NOTICE_ACTIONS`] actions ("Sync again", "Open Settings"), standing
/// in order right after its text, each a button that names itself where the
/// row has room and its icon where not (the last first), and never in the
/// overflow menu.
/// Notices replace each other; none stack. A message the person must act
/// on before it goes is not a notice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolbarNotice {
    /// What happened, in a few words.
    pub text: String,
    /// Whether it is a confirmation, information, a warning or a failure.
    #[serde(default)]
    pub kind: crate::widget::toast::ToastKind,
    /// What the person can do about it, in order, at most
    /// [`MAX_NOTICE_ACTIONS`]; each is sent as [`ToolbarEvent::Action`] when
    /// pressed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ToolbarAction>,
}

/// The most actions a notice carries.
pub const MAX_NOTICE_ACTIONS: usize = 2;

impl ToolbarNotice {
    #[must_use]
    pub fn new(text: impl Into<String>, kind: crate::widget::toast::ToastKind) -> Self {
        Self {
            text: text.into(),
            kind,
            actions: Vec::new(),
        }
    }

    /// The notice with `action` ("Undo") beside it, after any it has.
    #[must_use]
    pub fn action(mut self, action: ToolbarAction) -> Self {
        self.actions.push(action);
        self
    }

    /// As [`WindowToolbar::bounded`] takes it: its texts cut, an action
    /// without an icon of the set dropped, at most [`MAX_NOTICE_ACTIONS`].
    fn bound(&mut self) {
        cut(&mut self.text);
        self.actions
            .retain(|action| crate::icons::Icon::from_name(&action.icon).is_some());
        self.actions.truncate(MAX_NOTICE_ACTIONS);
        for action in &mut self.actions {
            cut(&mut action.id);
            cut(&mut action.name);
            action.items.clear();
        }
    }
}

/// Tabs switching between an app's views (Month, Week, Day).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolbarViews {
    /// The switcher's accessible name ("View").
    pub name: String,
    /// The choices, in order. Choosing one sends its id as an
    /// [`ToolbarEvent::Action`].
    pub choices: Vec<ToolbarView>,
    /// The id of the chosen one.
    pub chosen: String,
}

/// One choice of a [`ToolbarViews`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolbarView {
    /// What the app gets back in [`ToolbarEvent::Action`].
    pub id: String,
    /// What the segment says.
    pub label: String,
}

impl ToolbarViews {
    /// The index of the chosen view, if it is one of the choices.
    #[must_use]
    pub fn chosen_index(&self) -> Option<usize> {
        self.choices.iter().position(|view| view.id == self.chosen)
    }
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
    /// Narrower forms of the label, widest first, each saying the same (a
    /// period's `Jun 29 – Jul 5` for `Mon, Jun 29 – Sun, Jul 5`): where the
    /// label does not fit, the bar shows the widest of them that fits the
    /// room the location has once everything else it can fold is in the
    /// menu, and keeps that room for it, so a wider window never shows a
    /// narrower form. Only the last is ever elided.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shorter: Vec<String>,
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
    /// The places the location can be switched to, for a location that is
    /// one of a few (where a terminal's shell runs): the label reads as a
    /// picker and a click opens the menu of them. Choosing one commits its
    /// value ([`ToolbarEvent::Commit`]). Empty: no picker. Not for an
    /// editable location, which edits instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<LocationChoice>,
    /// Where in the app the window is, as the app's `open` action takes it
    /// (a search entry's anchor): the desktop reopens it there, from the
    /// launcher's Continue. `None`: the app cannot be opened at it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    /// The document the location names has changes not yet saved: the bar
    /// says so after the label, in words, and tells assistive technology
    /// the same. Every editor marks its unsaved document this one way.
    #[serde(default)]
    pub edited: bool,
}

/// One row of a location's picker.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocationChoice {
    /// What the row says ("Your shell on this device").
    pub label: String,
    /// What choosing it commits ([`ToolbarEvent::Commit`]).
    pub value: String,
    /// It is where the window is now: the row carries the menu's check.
    #[serde(default)]
    pub current: bool,
    /// A separator stands above it: it begins another group of rows.
    #[serde(default)]
    pub separated: bool,
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
    /// The line under the label: a visited page's address under its title,
    /// which search a search runs on. Empty: the label alone.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

impl ToolbarLocation {
    /// A location naming `period` ([`crate::format::Period`]), in its
    /// widest form, with its narrower ones for a narrow bar.
    #[must_use]
    pub fn period(period: crate::format::Period) -> Self {
        let mut forms = period.forms().into_iter();
        Self {
            label: forms.next().unwrap_or_default(),
            shorter: forms.collect(),
            ..Self::default()
        }
    }

    /// The label (`0`) or its narrower form `form`, the narrowest past the
    /// last.
    #[must_use]
    pub fn form(&self, form: usize) -> &str {
        form.checked_sub(1)
            .map_or(Some(&self.label), |shorter| {
                self.shorter.get(shorter).or_else(|| self.shorter.last())
            })
            .map_or(self.label.as_str(), String::as_str)
    }

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
    /// It is a toggle that is on (a bookmarked page's star, an open panel).
    #[serde(default)]
    pub on: bool,
    /// It is a toggle, on or off ([`Self::on`]): assistive technology
    /// hears it as pressed or not.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub toggles: bool,
    /// It is in the overflow menu only, never a button in the row: an
    /// occasional action (import, export, the app's settings).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub menu: bool,
    /// What a press lists, for a menu button: each item is chosen by its
    /// id ([`ToolbarEvent::Action`]); the button's own id is never sent.
    /// At most [`MAX_ITEMS`], one level deep.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<ToolbarAction>,
    /// It moves the person through the content (previous, next, back): a
    /// leading action that stays in a narrow row after the others and the
    /// views have gone into the overflow menu.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub navigates: bool,
    /// It ends, removes or discards something (leaving a call, deleting):
    /// its button has the destructive role's danger ink, as every other
    /// destructive button does.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub destructive: bool,
    /// Where the row has room it is a text button saying its name, as a
    /// calendar's Today; narrower it is its icon, and then an overflow
    /// row, in the row's fold order.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub labelled: bool,
}

fn enabled() -> bool {
    true
}

impl ToolbarAction {
    /// An enabled action button: `id` comes back in
    /// [`ToolbarEvent::Action`], `icon` is its icon-set name, `name` the
    /// outcome it names.
    #[must_use]
    pub fn new(id: impl Into<String>, icon: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new_owned(id.into(), icon.into(), name.into())
    }

    /// [`Self::new`] once its text is owned: one copy in the toolkit
    /// rather than one in every app that calls the generic constructor.
    fn new_owned(id: String, icon: String, name: String) -> Self {
        Self {
            id,
            icon,
            name,
            enabled: true,
            on: false,
            menu: false,
            items: Vec::new(),
            navigates: false,
            toggles: false,
            destructive: false,
            labelled: false,
        }
    }

    /// It ends, removes or discards something ([`Self::destructive`]).
    #[must_use]
    pub fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }

    /// A menu button listing `items`.
    #[must_use]
    pub fn with_items(mut self, items: Vec<ToolbarAction>) -> Self {
        self.items = items;
        self
    }

    /// Whether it can act now.
    #[must_use]
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// A toggle, on or off.
    #[must_use]
    pub fn on(mut self, on: bool) -> Self {
        self.on = on;
        self.toggles = true;
        self
    }

    /// In the overflow menu only ([`Self::menu`]).
    #[must_use]
    pub fn in_menu(mut self) -> Self {
        self.menu = true;
        self
    }

    /// It moves through the content ([`Self::navigates`]).
    #[must_use]
    pub fn navigating(mut self) -> Self {
        self.navigates = true;
        self
    }

    /// Its name as a text button where the row has room
    /// ([`Self::labelled`]).
    #[must_use]
    pub fn labelled(mut self) -> Self {
        self.labelled = true;
        self
    }
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
    /// The document's find options changed: find again under them. Sent
    /// before the query when they differ from the defaults a search opens
    /// with.
    SearchOptions {
        options: crate::widget::FindOptions,
    },
    /// Replace the current match in the document with `with`, or every
    /// match (`all`), and find again.
    SearchReplace {
        with: String,
        all: bool,
    },
    /// The search's options stand open over this much of the app's window,
    /// in its coordinates (`None`: they closed): the current match is shown
    /// clear of them ([`crate::widget::Findable::set_find_cover`]).
    SearchCover {
        cover: Option<WholeRect>,
    },
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
    /// The app's views: what is typed turns the window's view into a query
    /// view of the app's resources, and closing returns to the view before.
    Query,
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowOffer {
    #[serde(default)]
    pub search: SearchOffer,
    /// The open search's count; none before anything is typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<SearchCount>,
    /// The History control; none where the app has no History.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<HistoryOffer>,
    /// What the document's find offers beyond matching (replacing, a regular
    /// expression, hex), when the search acts on a document.
    #[serde(
        default,
        skip_serializing_if = "crate::widget::FindAbilities::is_plain"
    )]
    pub find: crate::widget::FindAbilities,
    /// The last find the app's document ran on its own that its search took
    /// over ([`crate::widget::Findable::take_own_find`]): the bar's field
    /// shows its query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adopted: Option<AdoptedQuery>,
}

/// A rect in whole logical pixels, as the bar tells an app in its own
/// process where something of the bar stands over it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WholeRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl WholeRect {
    /// The whole pixels `rect` touches.
    #[must_use]
    pub fn around(rect: vello::kurbo::Rect) -> Self {
        let rect = rect.expand();
        // A window's coordinates are a few thousand logical pixels.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "window coordinates are far inside i32 once rounded outward"
        )]
        let whole = |v: f64| v as i32;
        Self {
            x0: whole(rect.x0),
            y0: whole(rect.y0),
            x1: whole(rect.x1),
            y1: whole(rect.y1),
        }
    }

    /// The rect in logical pixels.
    #[must_use]
    pub fn rect(self) -> vello::kurbo::Rect {
        vello::kurbo::Rect::new(
            f64::from(self.x0),
            f64::from(self.y0),
            f64::from(self.x1),
            f64::from(self.y1),
        )
    }
}

/// A find an app's document ran on its own (an action went to a match) that
/// the open search took over, for the bar's field to show. `serial` tells one
/// such find from the next, so a find already shown never overwrites what the
/// person typed since.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdoptedQuery {
    pub serial: u64,
    pub query: String,
    #[serde(default)]
    pub options: crate::widget::FindOptions,
}

/// What an app's chord asks of its window's search: Find (Ctrl+F, ⌘F), or
/// Find and Replace (Ctrl+H, ⌘⌥F), which opens it with the replacement
/// field holding the keyboard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchOpening {
    #[default]
    Find,
    Replace,
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
            crate::widget::InPlace::Query => Self::Query,
        }
    }
}

impl From<SearchOffer> for crate::widget::InPlace {
    fn from(offer: SearchOffer) -> Self {
        match offer {
            SearchOffer::Nothing => Self::Nothing,
            SearchOffer::Document => Self::Document,
            SearchOffer::List => Self::List,
            SearchOffer::Query => Self::Query,
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
            && self.leading.is_empty()
            && self
                .views
                .as_ref()
                .is_none_or(|views| views.choices.is_empty())
            && self.notice.is_none()
    }

    /// The slots as the bar lays them out: the notice up, if any, in the
    /// location's place (its kind's icon, then its text). Its action is the
    /// notice's own, laid out right after its text, never among the
    /// actions.
    #[must_use]
    pub fn presented(&self) -> Self {
        let mut shown = self.clone();
        let Some(notice) = self.notice.as_ref() else {
            return shown;
        };
        shown.location = Some(ToolbarLocation {
            label: notice.text.clone(),
            icon: Some(
                crate::widget::toast::kind_icon(notice.kind)
                    .name()
                    .to_owned(),
            ),
            ..ToolbarLocation::default()
        });
        shown
    }

    /// How many of the actions may stand in the row: those not declared for
    /// the menu, which [`Self::bounded`] puts first.
    #[must_use]
    pub fn row_actions(&self) -> usize {
        self.actions
            .iter()
            .take_while(|action| !action.menu)
            .count()
    }

    /// The declaration as the bar takes it: at most
    /// [`MAX_DECLARED_ACTIONS`] actions, the row's before the menu's,
    /// [`MAX_LEADING`] leading actions, [`MAX_VIEWS`] views,
    /// [`MAX_SEGMENTS`] levels and [`MAX_SUGGESTIONS`] suggestions, every
    /// text at most [`MAX_TEXT_BYTES`], a picture at most
    /// [`MAX_IMAGE_BYTES`], no icon the set does not have. A button in the
    /// row needs an icon; a menu entry is named by its text, so one with no
    /// icon stays ([`ToolbarAction::menu`], a menu button's items).
    #[must_use]
    pub fn bounded(mut self) -> Self {
        self.actions
            .retain(|action| shown_icon(&action.icon, action.menu));
        // The row's actions first, in their order, then the menu's: the
        // row shows a prefix of the declared actions.
        self.actions.sort_by_key(|action| action.menu);
        self.actions.truncate(MAX_DECLARED_ACTIONS);
        self.leading
            .retain(|action| crate::icons::Icon::from_name(&action.icon).is_some());
        self.leading.truncate(MAX_LEADING);
        for action in self.actions.iter_mut().chain(self.leading.iter_mut()) {
            cut(&mut action.id);
            cut(&mut action.name);
            action.items.retain(|item| shown_icon(&item.icon, true));
            action.items.truncate(MAX_ITEMS);
            for item in &mut action.items {
                cut(&mut item.id);
                cut(&mut item.name);
                item.items.clear();
                item.menu = false;
            }
        }
        // Leading actions stand in the row whatever they say.
        for action in &mut self.leading {
            action.menu = false;
        }
        if let Some(notice) = &mut self.notice {
            notice.bound();
        }
        if let Some(views) = &mut self.views {
            views.choices.truncate(MAX_VIEWS);
            cut(&mut views.name);
            cut(&mut views.chosen);
            for view in &mut views.choices {
                cut(&mut view.id);
                cut(&mut view.label);
            }
            if views.choices.is_empty() {
                self.views = None;
            }
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
            if let Some(anchor) = &mut location.anchor {
                cut(anchor);
            }
            location.segments.truncate(MAX_SEGMENTS);
            for segment in &mut location.segments {
                cut(&mut segment.label);
                cut(&mut segment.value);
            }
            location.choices.truncate(MAX_CHOICES);
            for choice in &mut location.choices {
                cut(&mut choice.label);
                cut(&mut choice.value);
            }
            location.suggestions.truncate(MAX_SUGGESTIONS);
            for suggestion in &mut location.suggestions {
                cut(&mut suggestion.label);
                cut(&mut suggestion.value);
                cut(&mut suggestion.detail);
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

/// Whether an action with `icon` can be shown: an icon the set has, or
/// none at all for an entry of a menu (`in_menu`), which its text names.
fn shown_icon(icon: &str, in_menu: bool) -> bool {
    crate::icons::Icon::from_name(icon).is_some() || (in_menu && icon.is_empty())
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
            | Self::Search { query: text }
            | Self::SearchReplace { with: text, .. } => cut(text),
            Self::ToggleSidebar
            | Self::Back
            | Self::Forward
            | Self::SearchStep { .. }
            | Self::SearchClose
            | Self::SearchOptions { .. }
            | Self::SearchCover { .. }
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
    static NOTICE: RefCell<Option<ToolbarNotice>> = const { RefCell::new(None) };
    static REVISION: Cell<u64> = const { Cell::new(0) };
    static EVENTS: RefCell<VecDeque<ToolbarEvent>> = const { RefCell::new(VecDeque::new()) };
    static NOTICE_PRESSES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static HOSTED: Cell<bool> = const { Cell::new(false) };
    static EDIT_REQUESTED: Cell<bool> = const { Cell::new(false) };
    static SEARCH_REQUESTED: Cell<Option<SearchOpening>> = const { Cell::new(None) };
    /// The window of the app its host is running, while it runs one.
    static WINDOW: RefCell<Option<crate::hosted_window::HostedWindow>> =
        const { RefCell::new(None) };
}

/// While the returned guard lives, this module's calls act on `window`: the
/// window a host shows the app it is about to run in. `None` keeps the
/// enclosing window. Guards nest.
#[must_use = "the window is current until the guard is dropped"]
pub fn enter(window: Option<&crate::hosted_window::HostedWindow>) -> WindowScope {
    match window {
        Some(window) => WindowScope {
            changed: true,
            earlier: WINDOW.with(|current| current.replace(Some(window.clone()))),
        },
        None => WindowScope {
            changed: false,
            earlier: None,
        },
    }
}

/// Puts back the enclosing window when dropped: see [`enter`].
pub struct WindowScope {
    /// Whether this guard changed the window.
    changed: bool,
    /// The window current before it did.
    earlier: Option<crate::hosted_window::HostedWindow>,
}

impl Drop for WindowScope {
    fn drop(&mut self) {
        if self.changed {
            let earlier = self.earlier.take();
            WINDOW.with(|current| *current.borrow_mut() = earlier);
        }
    }
}

/// The window of the app being run, if its host shows it in one.
pub(crate) fn window() -> Option<crate::hosted_window::HostedWindow> {
    WINDOW.with(|current| current.borrow().clone())
}

/// Which window this module's calls act on now: the one a host lent the app
/// it is running, or `None` for the process's own window.
pub(crate) fn current_window() -> Option<usize> {
    WINDOW.with(|current| {
        current
            .borrow()
            .as_ref()
            .map(crate::hosted_window::HostedWindow::identity)
    })
}

/// Declare the window's slots (`None`: none). Cheap when nothing changed.
pub fn declare(toolbar: Option<WindowToolbar>) {
    if let Some(window) = window() {
        window.declare(toolbar);
        return;
    }
    let toolbar = toolbar
        .map(declaration)
        .filter(|toolbar| !toolbar.is_empty());
    DECLARED.with(|declared| {
        let mut declared = declared.borrow_mut();
        if *declared != toolbar {
            *declared = toolbar;
            REVISION.with(|revision| revision.set(revision.get().wrapping_add(1)));
        }
    });
}

/// `toolbar` as a declaration: bounded, without a notice, which is the
/// window's ([`notify`]) rather than any declaration's.
pub(crate) fn declaration(toolbar: WindowToolbar) -> WindowToolbar {
    WindowToolbar {
        notice: None,
        ..toolbar.bounded()
    }
}

/// Take the slots back, as [`declare`] with `None` does, unless the thread
/// is ending and they are gone already (a widget dropped with its thread).
pub(crate) fn retract() {
    let changed = DECLARED
        .try_with(|declared| declared.borrow_mut().take().is_some())
        .unwrap_or(false);
    if changed {
        let _ = REVISION.try_with(|revision| revision.set(revision.get().wrapping_add(1)));
    }
}

/// The slots last declared on this thread.
#[must_use]
pub fn declared() -> Option<WindowToolbar> {
    if let Some(window) = window() {
        return window.declared();
    }
    DECLARED.with(|declared| declared.borrow().clone())
}

/// Show `notice` in the window's bar, replacing the notice up (`None`: take
/// it down). Cheap when nothing changed. Whoever shows a notice takes it
/// down with [`withdraw`], so a later notice from elsewhere stays.
pub fn notify(notice: Option<ToolbarNotice>) {
    let notice = notice.map(|mut notice| {
        notice.bound();
        notice
    });
    if let Some(window) = window() {
        window.notify(notice);
        return;
    }
    NOTICE.with(|current| {
        let mut current = current.borrow_mut();
        if *current != notice {
            *current = notice;
            REVISION.with(|revision| revision.set(revision.get().wrapping_add(1)));
        }
    });
}

/// Take `notice` down if it is the one up; a notice that replaced it stays.
/// Nothing once the thread is ending: its window is gone with it.
pub fn withdraw(notice: &ToolbarNotice) {
    let mut bounded = notice.clone();
    bounded.bound();
    let Ok(window) = WINDOW.try_with(|current| current.borrow().clone()) else {
        return;
    };
    if let Some(window) = window {
        if window.notice().as_ref() == Some(&bounded) {
            window.notify(None);
        }
        return;
    }
    let _ = NOTICE.try_with(|current| {
        let mut current = current.borrow_mut();
        if current.as_ref() == Some(&bounded) {
            *current = None;
            let _ = REVISION.try_with(|revision| revision.set(revision.get().wrapping_add(1)));
        }
    });
}

/// The notice up in the window's bar.
#[must_use]
pub fn notice() -> Option<ToolbarNotice> {
    if let Some(window) = window() {
        return window.notice();
    }
    NOTICE.with(|notice| notice.borrow().clone())
}

/// What the window's bar shows: the slots declared, with the notice up.
#[must_use]
pub fn shown() -> Option<WindowToolbar> {
    with_notice(declared(), notice())
}

/// `toolbar` with `notice` up in it.
pub(crate) fn with_notice(
    toolbar: Option<WindowToolbar>,
    notice: Option<ToolbarNotice>,
) -> Option<WindowToolbar> {
    match (toolbar, notice) {
        (toolbar, None) => toolbar,
        (toolbar, Some(notice)) => Some(WindowToolbar {
            notice: Some(notice),
            ..toolbar.unwrap_or_default()
        }),
    }
}

/// Bumped by every [`declare`] and [`notify`] that changed something, so a
/// runner sends what the bar shows once.
#[must_use]
pub fn revision() -> u64 {
    if let Some(window) = window() {
        return window.revision();
    }
    REVISION.with(Cell::get)
}

/// Whether `event` is a press on the app's own slots (a mode, an action,
/// the sidebar toggle), after which the app takes the keyboard if nothing
/// in its window holds it. Typing into the bar's location or search, and
/// its History, keep the keyboard where it is.
#[must_use]
pub fn hands_keyboard_to_app(event: &ToolbarEvent) -> bool {
    matches!(
        event,
        ToolbarEvent::Action { .. } | ToolbarEvent::ToggleSidebar
    )
}

/// The id of the notice's button `event` presses, if it presses one: such a
/// press is the notice's poster's ([`take_notice_press`]), never among the
/// app's slot events.
pub(crate) fn notice_press(notice: Option<&ToolbarNotice>, event: &ToolbarEvent) -> Option<String> {
    let ToolbarEvent::Action { id } = event else {
        return None;
    };
    notice
        .filter(|notice| notice.actions.iter().any(|action| action.id == *id))
        .map(|_| id.clone())
}

/// Whether the button `id` of a notice was pressed in the bar since the
/// last call: the notice's poster takes it, whatever else the window does
/// with its bar's events.
#[must_use]
pub fn take_notice_press(id: &str) -> bool {
    if let Some(window) = window() {
        return window.take_notice_press(id);
    }
    NOTICE_PRESSES.with(|presses| {
        let mut presses = presses.borrow_mut();
        let before = presses.len();
        presses.retain(|pressed| pressed != id);
        presses.len() != before
    })
}

/// What the person did in the bar since the last call, oldest first.
#[must_use]
pub fn take_events() -> Vec<ToolbarEvent> {
    if let Some(window) = window() {
        return window.take_events();
    }
    EVENTS.with(|events| events.borrow_mut().drain(..).collect())
}

/// Whether the desktop's bar shows this window's slots, so the app draws
/// no toolbar of its own.
#[must_use]
pub fn is_hosted() -> bool {
    if window().is_some() {
        return true;
    }
    HOSTED.with(Cell::get)
}

/// Ask the bar to turn the window's editable location into a field, as a
/// click on it does: the app's Ctrl+L (or F6) while the bar hosts its slots.
pub fn edit_location() {
    if let Some(window) = window() {
        window.edit_location();
        return;
    }
    EDIT_REQUESTED.with(|cell| cell.set(true));
}

/// Whether the app asked to edit its location since the last call (the
/// runner, to tell the shell).
#[must_use]
pub fn take_edit_request() -> bool {
    if let Some(window) = window() {
        return window.take_edit_request();
    }
    EDIT_REQUESTED.with(|cell| cell.replace(false))
}

/// Ask the bar to open the window's search, as a click on its control does:
/// the app's Ctrl+F (or, for `SearchOpening::Replace`, its Ctrl+H) while the
/// bar hosts its window and it has something to search in place.
pub fn open_search(opening: SearchOpening) {
    if let Some(window) = window() {
        window.open_search(opening);
        return;
    }
    SEARCH_REQUESTED.with(|cell| cell.set(Some(opening)));
}

/// How the app asked to open the window's search since the last call, if it
/// did (the runner, to tell the shell).
#[must_use]
pub fn take_search_request() -> Option<SearchOpening> {
    if let Some(window) = window() {
        return window.take_search_request();
    }
    SEARCH_REQUESTED.with(Cell::take)
}

/// Hand the app what the person did in the bar (the runner, from the
/// shell's message).
pub fn deliver(event: ToolbarEvent) {
    if let Some(window) = window() {
        window.deliver(event);
        return;
    }
    if let Some(id) = NOTICE.with(|notice| notice_press(notice.borrow().as_ref(), &event)) {
        NOTICE_PRESSES.with(|presses| {
            let mut presses = presses.borrow_mut();
            if presses.len() < MAX_PENDING {
                presses.push(id);
            }
        });
        return;
    }
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
                menu: false,
                items: Vec::new(),
                navigates: false,
                toggles: false,
                destructive: false,
                labelled: false,
            }],
            ..WindowToolbar::default()
        }
    }

    #[test]
    fn menu_actions_follow_the_rows_and_leading_and_views_are_bounded() {
        let toolbar = WindowToolbar {
            actions: vec![
                ToolbarAction::new("import", "download", "Import").in_menu(),
                ToolbarAction::new("new", "plus", "New event"),
                ToolbarAction::new("settings", "settings", "Settings").in_menu(),
                ToolbarAction::new("share", "upload", "Share"),
            ],
            leading: (0..5)
                .map(|n| ToolbarAction::new(format!("l{n}"), "chevron-left", "Previous").in_menu())
                .collect(),
            views: Some(ToolbarViews {
                name: "View".into(),
                choices: (0..12)
                    .map(|n| ToolbarView {
                        id: format!("v{n}"),
                        label: format!("View {n}"),
                    })
                    .collect(),
                chosen: "v2".into(),
            }),
            ..WindowToolbar::default()
        }
        .bounded();
        let ids: Vec<&str> = toolbar.actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            ["new", "share", "import", "settings"],
            "the row's first, in order"
        );
        assert_eq!(toolbar.row_actions(), 2);
        assert_eq!(toolbar.leading.len(), MAX_LEADING);
        assert!(
            toolbar.leading.iter().all(|a| !a.menu),
            "leading stands in the row"
        );
        let views = toolbar.views.as_ref().unwrap();
        assert_eq!(views.choices.len(), MAX_VIEWS);
        assert_eq!(views.chosen_index(), Some(2));
        let wire = serde_json::to_string(&toolbar).unwrap();
        assert_eq!(
            serde_json::from_str::<WindowToolbar>(&wire).unwrap(),
            toolbar
        );
        let empty_views = WindowToolbar {
            views: Some(ToolbarViews::default()),
            ..WindowToolbar::default()
        };
        assert!(
            empty_views.is_empty(),
            "a switcher with no choices is nothing"
        );
        assert_eq!(
            serde_json::from_str::<ToolbarAction>(r#"{"id":"a","icon":"plus","name":"Add"}"#)
                .unwrap(),
            ToolbarAction::new("a", "plus", "Add"),
            "an older app's action is enabled and in the row"
        );
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
                menu: false,
                items: Vec::new(),
                navigates: false,
                toggles: false,
                destructive: false,
                labelled: false,
            });
        }
        many.actions.push(ToolbarAction {
            id: "glyph".into(),
            icon: "not-an-icon".into(),
            name: "Nothing".into(),
            enabled: true,
            on: false,
            menu: false,
            items: Vec::new(),
            navigates: false,
            toggles: false,
            destructive: false,
            labelled: false,
        });
        for n in 6..20 {
            many.actions.push(ToolbarAction {
                id: format!("a{n}"),
                icon: "star".into(),
                name: "Bookmark".into(),
                enabled: true,
                on: false,
                menu: false,
                items: Vec::new(),
                navigates: false,
                toggles: false,
                destructive: false,
                labelled: false,
            });
        }
        let bounded = many.clone().bounded();
        assert_eq!(bounded.actions.len(), MAX_DECLARED_ACTIONS);
        assert!(bounded.actions.iter().all(|a| a.icon != "not-an-icon"));

        // A menu entry is named by its text: one with no icon stays, and a
        // button in the row with none does not.
        let texts = WindowToolbar {
            actions: vec![
                ToolbarAction::new("import", "", "Import report").in_menu(),
                ToolbarAction::new("bare", "", "No icon in the row"),
                ToolbarAction::new("more", "more-horizontal", "More")
                    .with_items(vec![ToolbarAction::new("export", "", "Export report")]),
            ],
            ..WindowToolbar::default()
        }
        .bounded();
        let ids: Vec<&str> = texts.actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["more", "import"]);
        assert_eq!(
            texts.actions[0].items.first().map(|item| item.id.as_str()),
            Some("export")
        );

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
            ToolbarEvent::SearchOptions {
                options: crate::widget::FindOptions {
                    match_case: true,
                    whole_word: false,
                    mode: crate::widget::FindMode::Regex,
                },
            },
            ToolbarEvent::SearchCover {
                cover: Some(WholeRect {
                    x0: 120,
                    y0: 0,
                    x1: 300,
                    y1: 168,
                }),
            },
            ToolbarEvent::SearchReplace {
                with: "dog".into(),
                all: true,
            },
            ToolbarEvent::History,
        ] {
            let wire = serde_json::to_string(&event).unwrap();
            assert_eq!(serde_json::from_str::<ToolbarEvent>(&wire).unwrap(), event);
            deliver(event);
        }
        let taken = take_events();
        assert_eq!(taken.len(), 11, "every event reaches the app");
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
            find: crate::widget::FindAbilities {
                replace: true,
                regex: true,
                hex: false,
            },
            adopted: Some(AdoptedQuery {
                serial: 2,
                query: "relay".into(),
                options: crate::widget::FindOptions::default(),
            }),
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
        assert_eq!(take_search_request(), None);
        open_search(SearchOpening::Find);
        assert_eq!(take_search_request(), Some(SearchOpening::Find));
        assert_eq!(take_search_request(), None, "one ask per Ctrl+F");
        open_search(SearchOpening::Replace);
        assert_eq!(
            take_search_request(),
            Some(SearchOpening::Replace),
            "Ctrl+H asks for the replacement field"
        );
    }
}
