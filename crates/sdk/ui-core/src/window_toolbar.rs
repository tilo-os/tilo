//! An app's toolbar is slots in the window's bar, never a row it draws. The desktop draws
//! one bar per window; what an app adds it declares here, as slots the bar lays out and
//! draws with the system's own controls.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// The most actions the row shows as buttons; the rest go in its overflow menu.
pub const MAX_ACTIONS: usize = 3;

/// The most actions one window declares, buttons and overflow together.
pub const MAX_DECLARED_ACTIONS: usize = 12;

/// The most leading actions the row shows; a declaration's others are dropped.
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

/// Largest encoded location picture, in bytes.
pub const MAX_IMAGE_BYTES: usize = crate::icons::MAX_PNG_BYTES;

/// Largest location picture, in pixels (edge squared).
pub const MAX_IMAGE_PIXELS: u32 = crate::icons::MAX_PNG_PIXELS;

/// Longest label, value or name the bar takes, in bytes. Longer text is cut at a
/// character boundary: the bar elides far sooner, and the wire never carries a page's
/// worth of text per frame.
pub const MAX_TEXT_BYTES: usize = 8 * 1024;

/// What an app shows in its window's bar.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowToolbar {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar: Option<bool>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sidebar_unavailable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub navigation: Option<ToolbarNavigation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<ToolbarLocation>,
    /// The app's most used actions, at most [`MAX_ACTIONS`] in the row; the rest in its
    /// overflow menu.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ToolbarAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub leading: Vec<ToolbarAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub views: Option<ToolbarViews>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<ToolbarNotice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reactions: Option<ToolbarReactions>,
}

/// The reactions to the one thing a window shows (a shared document), as its bar shows
/// them: a pill per emoji, the person's own marked, and an add button where they may
/// react. Whoever shows them takes the presses ([`take_reaction_presses`]); the app's
/// slot events never carry them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolbarReactions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pills: Vec<ToolbarReaction>,
    #[serde(default)]
    pub can_react: bool,
}

/// Everyone who reacted with one emoji.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolbarReaction {
    pub emoji: String,
    pub count: u32,
    #[serde(default)]
    pub mine: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

/// The most pills a window's reactions carry: as many emoji as one thing holds reactions
/// with.
pub const MAX_REACTION_PILLS: usize = 20;

/// The most names a pill's tooltip carries; the rest are counted.
pub const MAX_REACTOR_NAMES: usize = 12;

/// The longest emoji a pill carries, in bytes.
const MAX_REACTION_EMOJI_BYTES: usize = 64;

/// The longest name a pill's tooltip carries, in bytes.
const MAX_REACTOR_NAME_BYTES: usize = 128;

/// What a press on the pill of an emoji, or a pick of it in the picker, sends as
/// [`ToolbarEvent::Action`]: a window's reactions own every id with this prefix.
pub const REACTION_ACTION_PREFIX: &str = "window-reaction:";

/// The action id a press on `emoji`'s pill sends.
#[must_use]
pub fn reaction_action(emoji: &str) -> String {
    format!("{REACTION_ACTION_PREFIX}{emoji}")
}

impl ToolbarReactions {
    /// Whether the bar shows nothing of them: no pill, and no add button.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pills.is_empty() && !self.can_react
    }

    fn bound(&mut self) {
        self.pills.retain(|pill| {
            !pill.emoji.is_empty() && pill.emoji.len() <= MAX_REACTION_EMOJI_BYTES && pill.count > 0
        });
        self.pills.truncate(MAX_REACTION_PILLS);
        for pill in &mut self.pills {
            pill.names.truncate(MAX_REACTOR_NAMES);
            for name in &mut pill.names {
                cut_to(name, MAX_REACTOR_NAME_BYTES);
            }
        }
    }
}

/// What just happened, said in the window's bar for a while: an operation's result or
/// failure ("Page 1 captured"). A notice that asks the person to act carries what they
/// can do: at most [`MAX_NOTICE_ACTIONS`] actions ("Sync again", "Open Settings"),
/// standing in order right after its text, each a button that names itself where the row
/// has room and its icon where not (the last first), and never in the overflow menu.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolbarNotice {
    pub text: String,
    #[serde(default)]
    pub kind: crate::widget::toast::ToastKind,
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
    pub name: String,
    pub choices: Vec<ToolbarView>,
    pub chosen: String,
}

/// One choice of a [`ToolbarViews`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolbarView {
    pub id: String,
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
    pub label: String,
    /// Narrower forms of the label, widest first, each saying the same (a period's
    /// `Jun 29 – Jul 5` for `Mon, Jun 29 – Sun, Jul 5`): where the label does not fit,
    /// the bar shows the widest of them that fits the room the location has once
    /// everything else it can fold is in the menu, and keeps that room for it, so a wider
    /// window never shows a narrower form.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shorter: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub placeholder: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default)]
    pub editable: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub go_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<LocationSegment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ToolbarImage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<LocationAvatar>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<LocationSuggestion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<LocationChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    #[serde(default)]
    pub edited: bool,
    /// The person may only look at what the location names: the bar says "View only"
    /// after the label, in place of [`Self::edited`], and tells assistive technology the
    /// same.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub view_only: bool,
}

/// One row of a location's picker.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocationChoice {
    pub label: String,
    pub value: String,
    #[serde(default)]
    pub current: bool,
    #[serde(default)]
    pub separated: bool,
}

/// One level of a location's hierarchy.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocationSegment {
    pub label: String,
    pub value: String,
}

/// The person or agent a location is ([`ToolbarLocation::avatar`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocationAvatar {
    pub name: String,
    pub identity: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub agent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hue: Option<crate::tokens::PresenceHue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<LocationPresence>,
}

/// Whether the person a location is, is here ([`LocationAvatar::presence`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocationPresence {
    Online,
    Away,
    Busy,
    Offline,
}

impl LocationPresence {
    /// The badge their face wears.
    #[must_use]
    pub fn status(self) -> crate::widget::PresenceStatus {
        match self {
            Self::Online => crate::widget::PresenceStatus::Online,
            Self::Away => crate::widget::PresenceStatus::Away,
            Self::Busy => crate::widget::PresenceStatus::Busy,
            Self::Offline => crate::widget::PresenceStatus::Offline,
        }
    }
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
    /// The picture as the bar paints it: decoded once, at most [`MAX_IMAGE_PIXELS`].
    #[must_use]
    pub fn app_image(&self) -> Option<crate::icons::AppImage> {
        let key = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            self.png.hash(&mut hasher);
            format!("window-toolbar-image:{:016x}", hasher.finish())
        };
        crate::icons::AppImage::from_png(key, &self.png)
    }
}

/// One suggestion under an edited location.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocationSuggestion {
    pub label: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

impl ToolbarLocation {
    /// A location naming `period` ([`crate::format::Period`]), in its widest form, with
    /// its narrower ones for a narrow bar.
    #[must_use]
    pub fn period(period: crate::format::Period) -> Self {
        let mut forms = period.forms().into_iter();
        Self {
            label: forms.next().unwrap_or_default(),
            shorter: forms.collect(),
            ..Self::default()
        }
    }

    /// The label (`0`) or its narrower form `form`, the narrowest past the last.
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolbarAction {
    pub id: String,
    pub icon: String,
    pub name: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub on: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub toggles: bool,
    /// It is in the overflow menu only, never a button in the row: an occasional action
    /// (import, export, the app's settings).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub menu: bool,
    /// What a press lists, for a menu button: each item is chosen by its id
    /// ([`ToolbarEvent::Action`]); the button's own id is never sent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<ToolbarAction>,
    /// It moves the person through the content (previous, next, back): a leading action
    /// that stays in a narrow row after the others and the views have gone into the
    /// overflow menu.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub navigates: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub destructive: bool,
    /// Where the row has room it is a text button saying its name, as a calendar's Today;
    /// narrower it is its icon, and then an overflow row, in the row's fold order.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub labelled: bool,
    /// It is the app's one main action for what the window shows ([`Self::primary`]): the
    /// bar draws it as the primary button, its words or its icon on the accent, so a
    /// person sees at once what the window is for. It leads the actions and stands in the
    /// row at every width, never behind the bar's door.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub primary: bool,
}

fn enabled() -> bool {
    true
}

impl ToolbarAction {
    /// An enabled action button: `id` comes back in [`ToolbarEvent::Action`], `icon` is
    /// its icon-set name, `name` the outcome it names.
    #[must_use]
    pub fn new(id: impl Into<String>, icon: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new_owned(id.into(), icon.into(), name.into())
    }

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
            primary: false,
        }
    }

    /// It ends, removes or discards something ([`Self::destructive`]).
    #[must_use]
    pub fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }

    /// It ends, removes or discards something, when `when` ([`Self::destructive`]).
    #[must_use]
    pub fn destructive_if(mut self, when: bool) -> Self {
        self.destructive = when;
        self
    }

    /// A menu button listing `items`.
    #[must_use]
    pub fn with_items(mut self, items: Vec<ToolbarAction>) -> Self {
        self.items = items;
        self
    }

    #[must_use]
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

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

    /// Its name as a text button where the row has room ([`Self::labelled`]).
    #[must_use]
    pub fn labelled(mut self) -> Self {
        self.labelled = true;
        self
    }

    /// The app's one main action for what the window shows ([`Self::primary`]).
    #[must_use]
    pub fn primary(mut self) -> Self {
        self.primary = true;
        self
    }
}

/// What the person did with the app's slots in the bar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolbarEvent {
    ToggleSidebar,
    Back,
    Forward,
    /// The location field was committed (Enter or the go button) with this text, exactly
    /// as typed; or a breadcrumb level or a suggestion was chosen, with its value.
    Commit {
        text: String,
    },
    Edit {
        text: String,
    },
    Action {
        id: String,
    },
    Search {
        query: String,
    },
    SearchStep {
        forward: bool,
    },
    SearchClose,
    SearchOptions {
        options: crate::widget::FindOptions,
    },
    SearchReplace {
        with: String,
        all: bool,
    },
    SearchCover {
        cover: Option<WholeRect>,
    },
    History,
    Floating {
        floating: bool,
    },
}

/// What a window's search acts on in place, as its app offers it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchOffer {
    #[default]
    Nothing,
    /// A document: matches are highlighted and stepped through ("3 of 12").
    Document,
    /// A list: it narrows to what matches ("12 of 40").
    List,
    Query,
}

/// The search's count: in a document the current match of all of them, in a list the rows
/// shown of all rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchCount {
    pub shown: u32,
    pub total: u32,
}

/// The window's History control, as its app offers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryOffer {
    Closed,
    Open,
}

/// What an app's window offers the bar's own controls: its search in place and the count
/// of the open search, and its History control.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowOffer {
    #[serde(default)]
    pub search: SearchOffer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<SearchCount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<HistoryOffer>,
    #[serde(
        default,
        skip_serializing_if = "crate::widget::FindAbilities::is_plain"
    )]
    pub find: crate::widget::FindAbilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adopted: Option<AdoptedQuery>,
}

/// A rect in whole logical pixels, as the bar tells an app in its own process where
/// something of the bar stands over it.
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

/// A find an app's document ran on its own (an action went to a match) that the open
/// search took over, for the bar's field to show. `serial` tells one such find from the
/// next, so a find already shown never overwrites what the person typed since.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdoptedQuery {
    pub serial: u64,
    pub query: String,
    #[serde(default)]
    pub options: crate::widget::FindOptions,
}

/// What an app's chord asks of its window's search: Find (Ctrl+F, ⌘F), or Find and
/// Replace (Ctrl+H, ⌘⌥F), which opens it with the replacement field holding the keyboard.
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
    /// What a window floating as a card shows of these in its header: whom or what it is
    /// about, alone, as a card's header names its activity — the app's own controls are
    /// the window's in its strip.
    #[must_use]
    pub fn card_form(&self) -> Self {
        Self {
            location: self.location.clone().map(|location| ToolbarLocation {
                editable: false,
                ..location
            }),
            ..Self::default()
        }
    }

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
            && self
                .reactions
                .as_ref()
                .is_none_or(ToolbarReactions::is_empty)
    }

    /// Whether the slots are an overflow menu alone: actions, every one of them in the
    /// menu, and nothing else.
    #[must_use]
    pub fn menu_only(&self) -> bool {
        !self.actions.is_empty()
            && self.actions.iter().all(|action| action.menu)
            && self.sidebar.is_none()
            && self.navigation.is_none()
            && self.location.is_none()
            && self.leading.is_empty()
            && self
                .views
                .as_ref()
                .is_none_or(|views| views.choices.is_empty())
            && self.notice.is_none()
            && self
                .reactions
                .as_ref()
                .is_none_or(ToolbarReactions::is_empty)
    }

    /// The slots as the bar lays them out: the notice up, if any, in the location's place
    /// (its kind's icon, then its text). Its action is the notice's own, laid out right
    /// after its text, never among the actions.
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

    /// How many of the actions may stand in the row: those not declared for the menu,
    /// which [`Self::bounded`] puts first.
    #[must_use]
    pub fn row_actions(&self) -> usize {
        self.actions
            .iter()
            .take_while(|action| !action.menu)
            .count()
    }

    /// The declaration as the bar takes it: at most [`MAX_DECLARED_ACTIONS`] actions, the
    /// row's before the menu's, [`MAX_LEADING`] leading actions, [`MAX_VIEWS`] views,
    /// [`MAX_SEGMENTS`] levels and [`MAX_SUGGESTIONS`] suggestions, every text at most
    /// [`MAX_TEXT_BYTES`], a picture at most [`MAX_IMAGE_BYTES`], no icon the set does
    /// not have.
    #[must_use]
    pub fn bounded(mut self) -> Self {
        // A window's main action never falls out of its bar for having no glyph: its
        // words say what it does wherever the row has room for them.
        self.actions
            .retain(|action| action.primary || shown_icon(&action.icon, action.menu));
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
        for action in &mut self.leading {
            action.menu = false;
        }
        if let Some(notice) = &mut self.notice {
            notice.bound();
        }
        if let Some(reactions) = &mut self.reactions {
            reactions.bound();
            if reactions.is_empty() {
                self.reactions = None;
            }
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
            if let Some(avatar) = &mut location.avatar {
                cut(&mut avatar.name);
                cut(&mut avatar.identity);
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

fn shown_icon(icon: &str, in_menu: bool) -> bool {
    crate::icons::Icon::from_name(icon).is_some() || (in_menu && icon.is_empty())
}

impl ToolbarEvent {
    /// The event as an app takes it: a committed text at most [`MAX_TEXT_BYTES`].
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
            | Self::History
            | Self::Floating { .. } => {}
        }
        self
    }
}

fn cut(text: &mut String) {
    cut_to(text, MAX_TEXT_BYTES);
}

fn cut_to(text: &mut String, most: usize) {
    if text.len() > most {
        let mut end = most;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
}

/// Upper bound on events an app has not read yet. A held key repeating faster than the
/// app draws must not queue without end.
const MAX_PENDING: usize = 32;

thread_local! {
    static DECLARED: RefCell<Option<WindowToolbar>> = const { RefCell::new(None) };
    static NOTICE: RefCell<Option<ToolbarNotice>> = const { RefCell::new(None) };
    static REACTIONS: RefCell<Option<ToolbarReactions>> = const { RefCell::new(None) };
    static VIEW_ONLY: Cell<bool> = const { Cell::new(false) };
    static EDITED: Cell<bool> = const { Cell::new(false) };
    static REACTION_PRESSES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static REVISION: Cell<u64> = const { Cell::new(0) };
    static EVENTS: RefCell<VecDeque<ToolbarEvent>> = const { RefCell::new(VecDeque::new()) };
    static NOTICE_PRESSES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static HOSTED: Cell<bool> = const { Cell::new(false) };
    static FLOATS: Cell<bool> = const { Cell::new(false) };
    static EDIT_REQUESTED: Cell<bool> = const { Cell::new(false) };
    static SEARCH_REQUESTED: Cell<Option<SearchOpening>> = const { Cell::new(None) };
    static WINDOWS: RefCell<Vec<crate::hosted_window::HostedWindow>> =
        const { RefCell::new(Vec::new()) };
}

/// While the returned guard lives, this module's calls act on `window`: the window a host
/// shows the app it is about to run in.
#[must_use = "the window is current until the guard is dropped"]
pub fn enter(window: Option<&crate::hosted_window::HostedWindow>) -> WindowScope {
    let Some(window) = window else {
        return WindowScope { entered: None };
    };
    WINDOWS.with(|windows| windows.borrow_mut().push(window.clone()));
    WindowScope {
        entered: Some(window.identity()),
    }
}

/// Leaves the window it entered when dropped: see [`enter`].
pub struct WindowScope {
    entered: Option<u64>,
}

impl Drop for WindowScope {
    fn drop(&mut self) {
        let Some(entered) = self.entered else {
            return;
        };
        // The guard leaves its own window from wherever it stands: a host that runs
        // several windows in one thread leaves them in any order, and the ones still
        // running stay current in the order they were entered.
        let _ = WINDOWS.try_with(|windows| {
            let mut windows = windows.borrow_mut();
            if let Some(at) = windows
                .iter()
                .rposition(|window| window.identity() == entered)
            {
                windows.remove(at);
            }
        });
    }
}

/// The window of the app being run, if its host shows it in one.
pub(crate) fn window() -> Option<crate::hosted_window::HostedWindow> {
    WINDOWS
        .try_with(|windows| windows.borrow().last().cloned())
        .ok()
        .flatten()
}

/// The workspace of the window the app runs in, when its host put the window in one
/// ([`crate::hosted_window::HostedWindow::set_workspace`]).
#[must_use]
pub fn workspace() -> Option<String> {
    window().and_then(|window| window.workspace())
}

/// Which window this module's calls act on now: the one a host lent the app it runs
/// ([`crate::hosted_window::HostedWindow::identity`]), or `None` for the process's own
/// window.
#[must_use]
pub fn current_window() -> Option<u64> {
    window()
        .as_ref()
        .map(crate::hosted_window::HostedWindow::identity)
}

/// Declare the window's slots (`None`: none).
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

/// `toolbar` as a declaration: bounded, without a notice or reactions, which are the
/// window's ([`notify`], [`show_reactions`]) rather than any declaration's.
pub(crate) fn declaration(toolbar: WindowToolbar) -> WindowToolbar {
    WindowToolbar {
        notice: None,
        reactions: None,
        ..toolbar.bounded()
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

/// Show `notice` in the window's bar, replacing the notice up (`None`: take it down).
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

/// Take `notice` down if it is the one up; a notice that replaced it stays. Nothing once
/// the thread is ending: its window is gone with it.
pub fn withdraw(notice: &ToolbarNotice) {
    let mut bounded = notice.clone();
    bounded.bound();
    let Ok(window) = WINDOWS.try_with(|windows| windows.borrow().last().cloned()) else {
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

/// What the window's bar shows: the slots declared, with the notice up and the reactions
/// shown.
#[must_use]
pub fn shown() -> Option<WindowToolbar> {
    with_window_owned(declared(), notice(), reactions(), view_only(), edited())
}

/// `toolbar` with `notice` up in it, `reactions` at its end and its location marked
/// [`ToolbarLocation::view_only`] when `view_only`, and [`ToolbarLocation::edited`] when
/// `edited`.
pub(crate) fn with_window_owned(
    mut toolbar: Option<WindowToolbar>,
    notice: Option<ToolbarNotice>,
    reactions: Option<ToolbarReactions>,
    view_only: bool,
    edited: bool,
) -> Option<WindowToolbar> {
    if let Some(location) = toolbar
        .as_mut()
        .and_then(|toolbar| toolbar.location.as_mut())
    {
        location.view_only |= view_only;
        location.edited |= edited;
    }
    if notice.is_none() && reactions.is_none() {
        return toolbar;
    }
    Some(WindowToolbar {
        notice,
        reactions,
        ..toolbar.unwrap_or_default()
    })
}

/// Say in the window's bar whether its person may only look at what it shows
/// ([`ToolbarLocation::view_only`]).
pub fn show_view_only(view_only: bool) {
    if let Some(window) = window() {
        window.show_view_only(view_only);
        return;
    }
    VIEW_ONLY.with(|current| {
        if current.replace(view_only) != view_only {
            REVISION.with(|revision| revision.set(revision.get().wrapping_add(1)));
        }
    });
}

/// Whether the window's bar says its person may only look.
#[must_use]
pub fn view_only() -> bool {
    if let Some(window) = window() {
        return window.view_only();
    }
    VIEW_ONLY.with(Cell::get)
}

/// Mark the window's document as having work not yet saved ([`ToolbarLocation::edited`]),
/// as the window's framework follows its record's saves: every window marks its record
/// this one way, and no app marks it from its own copy of the save status.
pub fn show_edited(edited: bool) {
    if let Some(window) = window() {
        window.show_edited(edited);
        return;
    }
    EDITED.with(|current| {
        if current.replace(edited) != edited {
            REVISION.with(|revision| revision.set(revision.get().wrapping_add(1)));
        }
    });
}

/// Whether the window's bar marks its document as not yet saved ([`show_edited`]).
#[must_use]
pub fn edited() -> bool {
    if let Some(window) = window() {
        return window.edited();
    }
    EDITED.with(Cell::get)
}

/// Show `reactions` at the end of the window's bar (`None`: none), as the reactions to
/// the one thing the window shows.
pub fn show_reactions(reactions: Option<ToolbarReactions>) {
    let reactions = reactions
        .map(|mut reactions| {
            reactions.bound();
            reactions
        })
        .filter(|reactions| !reactions.is_empty());
    if let Some(window) = window() {
        window.show_reactions(reactions);
        return;
    }
    REACTIONS.with(|current| {
        let mut current = current.borrow_mut();
        if *current != reactions {
            *current = reactions;
            REVISION.with(|revision| revision.set(revision.get().wrapping_add(1)));
        }
    });
}

/// The reactions shown in the window's bar.
#[must_use]
pub fn reactions() -> Option<ToolbarReactions> {
    if let Some(window) = window() {
        return window.reactions();
    }
    REACTIONS.with(|reactions| reactions.borrow().clone())
}

/// A press on the window's reactions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReactionPress {
    Inert,
    Emoji(String),
}

/// What `event` is to the window's `reactions`: `None` where it is no press on them (an
/// event of the app's). Every press on them is the reactions' shower's
/// ([`take_reaction_presses`]), never among the app's slot events.
pub(crate) fn reaction_press(
    reactions: Option<&ToolbarReactions>,
    event: &ToolbarEvent,
) -> Option<ReactionPress> {
    let ToolbarEvent::Action { id } = event else {
        return None;
    };
    let emoji = id.strip_prefix(REACTION_ACTION_PREFIX)?;
    let reacts = reactions.is_some_and(|reactions| reactions.can_react)
        && !emoji.is_empty()
        && emoji.len() <= MAX_REACTION_EMOJI_BYTES;
    Some(if reacts {
        ReactionPress::Emoji(emoji.to_owned())
    } else {
        ReactionPress::Inert
    })
}

/// The emoji pressed (or picked) among the window's reactions since the last call, oldest
/// first.
#[must_use]
pub fn take_reaction_presses() -> Vec<String> {
    if let Some(window) = window() {
        return window.take_reaction_presses();
    }
    REACTION_PRESSES.with(|presses| std::mem::take(&mut *presses.borrow_mut()))
}

/// Bumped by every [`declare`] and [`notify`] that changed something, so a runner sends
/// what the bar shows once.
#[must_use]
pub fn revision() -> u64 {
    if let Some(window) = window() {
        return window.revision();
    }
    REVISION.with(Cell::get)
}

/// The id of the notice's button `event` presses, if it presses one: such a press is the
/// notice's poster's ([`take_notice_press`]), never among the app's slot events.
pub(crate) fn notice_press(notice: Option<&ToolbarNotice>, event: &ToolbarEvent) -> Option<String> {
    let ToolbarEvent::Action { id } = event else {
        return None;
    };
    notice
        .filter(|notice| notice.actions.iter().any(|action| action.id == *id))
        .map(|_| id.clone())
}

/// Whether the button `id` of a notice was pressed in the bar since the last call: the
/// notice's poster takes it, whatever else the window does with its bar's events.
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

/// Whether the desktop's bar shows this window's slots, so the app draws no toolbar of
/// its own.
#[must_use]
pub fn is_hosted() -> bool {
    if window().is_some() {
        return true;
    }
    HOSTED.with(Cell::get)
}

/// Why a control of the app's own stays where it is although the bar has a slot for it:
/// the closed set of reasons `tools/policy/check_app_toolbars.py` accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Exemption {
    Picker,
    Form,
    Service,
    Fullscreen,
    /// Drawn only when no window bar hosts the window.
    Fallback,
    Wizard,
    ListNew,
}

/// `body` with the reason it keeps a control of its own although the bar has
/// a slot for it ([`Exemption`]).
///
/// A reason outside [`Exemption`] does not compile, and the policy check
/// reads this call rather than a comment, so tidying the comments around the
/// control never takes the reason away from it. Wrap the expression that
/// draws the control, not a statement that binds it: the body is a block.
///
/// ```
/// use tilo_ui::window_toolbar::exempt;
///
/// let field = exempt!(Picker, { "type-ahead" });
/// assert_eq!(field, "type-ahead");
/// ```
#[macro_export]
macro_rules! window_bar_exempt {
    ($reason:ident, $body:block) => {{
        const _: $crate::window_toolbar::Exemption = $crate::window_toolbar::Exemption::$reason;
        $body
    }};
}

pub use crate::window_bar_exempt as exempt;

/// Ask the bar to turn the window's editable location into a field, as a click on it
/// does: the app's Ctrl+L (or F6) while the bar hosts its slots.
pub fn edit_location() {
    if let Some(window) = window() {
        window.edit_location();
        return;
    }
    EDIT_REQUESTED.with(|cell| cell.set(true));
}

/// Whether the app asked to edit its location since the last call (the runner, to tell
/// the shell).
#[must_use]
pub fn take_edit_request() -> bool {
    if let Some(window) = window() {
        return window.take_edit_request();
    }
    EDIT_REQUESTED.with(|cell| cell.replace(false))
}

/// Ask the bar to open the window's search, as a click on its control does: the app's
/// Ctrl+F (or, for `SearchOpening::Replace`, its Ctrl+H) while the bar hosts its window
/// and it has something to search in place.
pub fn open_search(opening: SearchOpening) {
    if let Some(window) = window() {
        window.open_search(opening);
        return;
    }
    SEARCH_REQUESTED.with(|cell| cell.set(Some(opening)));
}

/// How the app asked to open the window's search since the last call, if it did (the
/// runner, to tell the shell).
#[must_use]
pub fn take_search_request() -> Option<SearchOpening> {
    if let Some(window) = window() {
        return window.take_search_request();
    }
    SEARCH_REQUESTED.with(Cell::take)
}

/// Hand the app what the person did in the bar, answering whether it is a press on the
/// app's own slots: the host hands the app the keyboard then, if nothing in its window
/// holds it.
pub(crate) fn deliver_to_app(event: ToolbarEvent) -> bool {
    let hands_keyboard = matches!(
        event,
        ToolbarEvent::Action { .. } | ToolbarEvent::ToggleSidebar
    );
    deliver(event);
    hands_keyboard
}

/// Whether the window floats over the desk as a card rather than standing in its strip
/// ([`ToolbarEvent::Floating`]).
#[must_use]
pub fn floats() -> bool {
    if let Some(window) = window() {
        return window.floats();
    }
    FLOATS.with(Cell::get)
}

/// Hand the app what the person did in the bar (the runner, from the shell's message).
pub fn deliver(event: ToolbarEvent) {
    if let Some(window) = window() {
        window.deliver(event);
        return;
    }
    if let ToolbarEvent::Floating { floating } = event {
        FLOATS.with(|cell| cell.set(floating));
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
    if let Some(press) =
        REACTIONS.with(|reactions| reaction_press(reactions.borrow().as_ref(), &event))
    {
        if let ReactionPress::Emoji(emoji) = press {
            REACTION_PRESSES.with(|presses| {
                let mut presses = presses.borrow_mut();
                if presses.len() < MAX_PENDING {
                    presses.push(emoji);
                }
            });
        }
        return;
    }
    EVENTS.with(|events| {
        let mut events = events.borrow_mut();
        if events.len() < MAX_PENDING {
            events.push_back(event.bounded());
        }
    });
}

/// Whether a desktop bar shows the slots (the runner, as its shell connection comes and
/// goes).
pub fn set_hosted(hosted: bool) -> bool {
    HOSTED.with(|cell| cell.replace(hosted) != hosted)
}

/// One control a window's bar lays out, as the desktop draws it: what it is, what it does
/// and where it stands.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BarControl {
    /// The slot it stands in: `sidebar`, `back`, `forward`, `leading`, `views`,
    /// `views-menu`, `action`, `overflow`, `notice`, `reaction`, `search`, `history`,
    /// `minimize`, `maximize`, `close`.
    pub slot: &'static str,
    pub id: String,
    pub name: String,
    /// Its icon, by the icon set's name (none: it shows words only).
    pub icon: Option<String>,
    /// It opens a menu: the bar's own overflow, an app's menu button, a views menu.
    pub menu: bool,
    pub labelled: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub furniture: bool,
    pub primary: bool,
    /// The width its words need, where it has words the bar shows only when it has room
    /// for them (`0.0`: it asks for no words, or has none).
    pub words: f64,
    pub rect: [f64; 4],
}

impl BarControl {
    /// The control as one of `slot`, at `rect`.
    #[must_use]
    pub fn new(slot: &'static str, id: impl Into<String>, rect: crate::kurbo::Rect) -> Self {
        Self {
            slot,
            id: id.into(),
            name: String::new(),
            icon: None,
            menu: false,
            labelled: false,
            furniture: false,
            primary: false,
            words: 0.0,
            rect: [rect.x0, rect.y0, rect.width(), rect.height()],
        }
    }

    /// It says `name` for itself.
    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Its icon is `icon`.
    #[must_use]
    pub fn icon(mut self, icon: Option<String>) -> Self {
        self.icon = icon;
        self
    }

    #[must_use]
    pub fn menu(mut self) -> Self {
        self.menu = true;
        self
    }

    /// The bar shows it with its name beside its icon.
    #[must_use]
    pub fn labelled(mut self) -> Self {
        self.labelled = true;
        self
    }

    /// It is the window's own furniture, not an action of the app's.
    #[must_use]
    pub fn furniture(mut self) -> Self {
        self.furniture = true;
        self
    }

    /// It is drawn as the primary button.
    #[must_use]
    pub fn primary(mut self) -> Self {
        self.primary = true;
        self
    }

    /// Its words need `words` wide, where the bar shows them only when it has room for
    /// them.
    #[must_use]
    pub fn words(mut self, words: f64) -> Self {
        self.words = words;
        self
    }
}

/// A word the bar paints itself, at the place its ink stands: no widget lays it out, so
/// whoever reads the bar reports it — a layout check reads these as text, and finds two
/// labels drawn on top of each other that no widget tree would show.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BarLabel {
    pub widget: &'static str,
    pub text: String,
    /// `[x, y, width, height]` of its ink, in logical pixels in the bar's own
    /// coordinates.
    pub rect: [f64; 4],
}

impl BarLabel {
    /// The words `widget` paints as `text`, standing at `rect`.
    #[must_use]
    pub fn new(widget: &'static str, text: impl Into<String>, rect: crate::kurbo::Rect) -> Self {
        Self {
            widget,
            text: text.into(),
            rect: [rect.x0, rect.y0, rect.width(), rect.height()],
        }
    }
}

/// One window's bar, as it lays itself out across the window: what a person sees in it,
/// and what room it leaves empty.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BarLayout {
    pub spare: f64,
    pub controls: Vec<BarControl>,
    pub labels: Vec<BarLabel>,
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
                primary: false,
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
                primary: false,
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
            primary: false,
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
                primary: false,
            });
        }
        let bounded = many.clone().bounded();
        assert_eq!(bounded.actions.len(), MAX_DECLARED_ACTIONS);
        assert!(bounded.actions.iter().all(|a| a.icon != "not-an-icon"));

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
    fn the_windows_reactions_are_its_own_and_their_presses_go_to_their_shower() {
        let pill = |emoji: &str| ToolbarReaction {
            emoji: emoji.into(),
            count: 2,
            mine: true,
            names: (0..40).map(|n| format!("Person {n}")).collect(),
        };
        let mut toolbar = browser();
        toolbar.reactions = Some(ToolbarReactions {
            pills: vec![pill("👍")],
            can_react: true,
        });
        declare(Some(toolbar));
        assert_eq!(
            declared().and_then(|toolbar| toolbar.reactions),
            None,
            "a declaration carries no reactions: they are the window's"
        );
        let before = revision();
        show_reactions(Some(ToolbarReactions {
            pills: (0..30)
                .map(|n| pill(&format!("{}", char::from_u32(0x1F600 + n).unwrap())))
                .collect(),
            can_react: true,
        }));
        assert_ne!(revision(), before, "the bar is told");
        let shown = shown()
            .and_then(|toolbar| toolbar.reactions)
            .expect("shown");
        assert_eq!(shown.pills.len(), MAX_REACTION_PILLS);
        assert!(
            shown
                .pills
                .iter()
                .all(|pill| pill.names.len() == MAX_REACTOR_NAMES)
        );
        let wire = serde_json::to_string(&shown).unwrap();
        assert_eq!(
            serde_json::from_str::<ToolbarReactions>(&wire).unwrap(),
            shown
        );

        let _ = take_events();
        deliver(ToolbarEvent::Action {
            id: reaction_action("🎉"),
        });
        deliver(ToolbarEvent::Action {
            id: "reload".into(),
        });
        assert_eq!(take_reaction_presses(), vec!["🎉".to_owned()]);
        assert_eq!(
            take_events(),
            vec![ToolbarEvent::Action {
                id: "reload".into()
            }],
            "the app hears its own slots only"
        );

        show_reactions(Some(ToolbarReactions {
            pills: vec![pill("👍")],
            can_react: false,
        }));
        deliver(ToolbarEvent::Action {
            id: reaction_action("👍"),
        });
        assert!(
            take_reaction_presses().is_empty(),
            "someone who may not react presses nothing"
        );
        assert!(take_events().is_empty(), "nor does the app hear it");
        show_reactions(Some(ToolbarReactions::default()));
        assert_eq!(reactions(), None, "nothing to show is none");
        declare(None);
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
