//! An app's toolbar is slots in the window's bar, never a row it draws.
//!
//! The desktop draws one bar per window (`Workspace → Strip → WindowGroup →
//! Tab`): the app's icon, its tabs, the window's controls. What an app adds
//! to it it declares here, as slots the bar lays out and draws with the
//! system's own controls:
//!
//! - **navigation:** back and forward, and whether each can go;
//! - **location:** where the window is and can go, the label the bar shows
//!   at rest (a page's domain, a folder's path, a document's name), and
//!   whether it can be edited. An editable location turns into a field
//!   holding the full value, all of it selected, on a click or Ctrl+L, with a
//!   go button at its end; Enter or the go button commits the text to the
//!   app, Esc puts the location back;
//! - **actions:** up to [`MAX_ACTIONS`] icon buttons (reload, bookmark…);
//!   the rest of an app's actions reach people through the action registry
//!   (palette, menus), not through the bar.
//!
//! The tab label (what tells two tabs apart) stays the window's title; the
//! location is where you are. A window whose app declares no slots shows
//! its title, as before.
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

/// The most actions the bar shows for one window.
pub const MAX_ACTIONS: usize = 3;

/// Longest label, value or name the bar takes, in bytes. Longer text is cut
/// at a character boundary: the bar elides far sooner, and the wire never
/// carries a page's worth of text per frame.
pub const MAX_TEXT_BYTES: usize = 8 * 1024;

/// What an app shows in its window's bar.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowToolbar {
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
    Back,
    Forward,
    /// The location field was committed (Enter or the go button) with this
    /// text, exactly as typed.
    Commit {
        text: String,
    },
    /// An action button was pressed.
    Action {
        id: String,
    },
}

impl WindowToolbar {
    /// Whether there is anything to show.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.navigation.is_none() && self.location.is_none() && self.actions.is_empty()
    }

    /// The declaration as the bar takes it: at most [`MAX_ACTIONS`]
    /// actions, every text at most [`MAX_TEXT_BYTES`], no action with an
    /// icon the set does not have.
    #[must_use]
    pub fn bounded(mut self) -> Self {
        self.actions
            .retain(|action| crate::icons::Icon::from_name(&action.icon).is_some());
        self.actions.truncate(MAX_ACTIONS);
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
            Self::Commit { text } | Self::Action { id: text } => cut(text),
            Self::Back | Self::Forward => {}
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
        let bounded = many.clone().bounded();
        assert_eq!(bounded.actions.len(), MAX_ACTIONS);
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
        ] {
            let wire = serde_json::to_string(&event).unwrap();
            assert_eq!(serde_json::from_str::<ToolbarEvent>(&wire).unwrap(), event);
            deliver(event);
        }
        let taken = take_events();
        assert_eq!(taken.len(), 4);
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
}
