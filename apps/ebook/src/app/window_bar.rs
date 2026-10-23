//! Books' controls, as slots in the window's bar ([`ui::window_toolbar`]).
//!
//! - Library: "Library" as the location, the sort order as the views, Open
//!   book as the action, Hide finished and Settings in the bar's menu.
//! - Reader: the contents as the sidebar, Back to the library, the book's
//!   title as the location, and reading appearance, the page's bookmark and
//!   the annotations as actions.
//! - Settings: Back, "Settings" as the location, its tabs as the views.
//! - Search: Back, and what is searched for as the location.
//!
//! The window's search is the bar's own: typing turns the window into the
//! search page ([`ui::widget::Queryable`]). The view draws no header row.

use ui::window_toolbar::{
    ToolbarAction, ToolbarEvent, ToolbarLocation, ToolbarNavigation, ToolbarView, ToolbarViews,
    WindowToolbar,
};

use super::settings_page::SETTINGS_TABS;
use super::{Action, BooksApp, SettingsTab, View};
use crate::model::LibrarySort;

/// The bar action that opens a book from a file.
pub(crate) const OPEN_BOOK: &str = "books.library.open";
/// The bar's menu item that hides and shows finished books.
pub(crate) const HIDE_FINISHED: &str = "books.library.hide-finished";
/// The bar's menu item that opens the settings page.
pub(crate) const SETTINGS: &str = "books.settings.open";
/// The bar action that opens and closes the reading-appearance popover.
pub(crate) const APPEARANCE: &str = "books.reader.appearance";
/// The bar action that bookmarks the page, or removes its bookmark.
pub(crate) const BOOKMARK: &str = "books.reader.bookmark";
/// The bar action that shows and hides the annotations.
pub(crate) const ANNOTATIONS: &str = "books.reader.annotations";

const SORT: &str = "books.library.sort:";
const TAB: &str = "books.settings.tab:";

/// The bar's view id of a sort order.
pub(crate) fn sort_id(sort: LibrarySort) -> String {
    format!("{SORT}{}", sort.as_str())
}

/// The bar's view id of a settings tab.
pub(crate) fn tab_id(tab: SettingsTab) -> String {
    let name = match tab {
        SettingsTab::Library => "library",
        SettingsTab::Fonts => "fonts",
        SettingsTab::Colours => "colours",
        SettingsTab::Shortcuts => "shortcuts",
    };
    format!("{TAB}{name}")
}

impl BooksApp {
    /// The window bar's slots for the page the window shows now.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        match self.view {
            View::Library => self.library_toolbar(),
            View::Reader => self.reader_toolbar(),
            View::Settings => WindowToolbar {
                navigation: Some(back()),
                location: Some(location("Settings")),
                views: Some(ToolbarViews {
                    name: "Settings".into(),
                    choices: SETTINGS_TABS
                        .iter()
                        .map(|(tab, label)| ToolbarView {
                            id: tab_id(*tab),
                            label: (*label).into(),
                        })
                        .collect(),
                    chosen: tab_id(self.settings_tab),
                }),
                ..WindowToolbar::default()
            },
            View::Search => WindowToolbar {
                navigation: Some(back()),
                location: Some(location(&self.query_title())),
                ..WindowToolbar::default()
            },
        }
    }

    fn library_toolbar(&self) -> WindowToolbar {
        let settings = ToolbarAction::new(SETTINGS, "settings", "Settings").in_menu();
        // An empty shelf offers "Open a book" as its one action and has
        // nothing to sort or hide, so the bar repeats none of it.
        if self.library.is_empty() && self.elsewhere.is_empty() {
            return WindowToolbar {
                location: Some(location("Library")),
                actions: vec![settings],
                ..WindowToolbar::default()
            };
        }
        WindowToolbar {
            location: Some(location("Library")),
            views: Some(ToolbarViews {
                name: "Sort".into(),
                choices: LibrarySort::ALL
                    .iter()
                    .map(|sort| ToolbarView {
                        id: sort_id(*sort),
                        label: sort.label().into(),
                    })
                    .collect(),
                chosen: sort_id(self.library_prefs.sort),
            }),
            actions: vec![
                ToolbarAction::new(OPEN_BOOK, "plus", "Open book"),
                ToolbarAction::new(HIDE_FINISHED, "eye-off", "Hide finished")
                    .on(self.library_prefs.hide_read)
                    .in_menu(),
                settings,
            ],
            ..WindowToolbar::default()
        }
    }

    fn reader_toolbar(&self) -> WindowToolbar {
        WindowToolbar {
            sidebar: Some(self.toc_open()),
            navigation: Some(back()),
            location: self.open_book().map(|book| location(&book.title)),
            actions: vec![
                ToolbarAction::new(APPEARANCE, "type", "Reading appearance")
                    .on(self.reading_settings.is_open()),
                ToolbarAction::new(BOOKMARK, "bookmark", "Bookmark this page")
                    .on(self.page_bookmarked()),
                ToolbarAction::new(ANNOTATIONS, "panel-right", "Annotations").on(self.notes_open()),
            ],
            ..WindowToolbar::default()
        }
    }

    /// Run what the person did in the window's bar, then declare the slots
    /// for the page the window shows now.
    pub(super) fn run_window_toolbar(&mut self, doc: &mut ui::widget::Edits<'_>) {
        for event in ui::window_toolbar::take_events() {
            self.run_window_bar_event(&event, doc);
        }
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }

    /// Run one thing the person did in the window's bar.
    pub(crate) fn run_window_bar_event(
        &mut self,
        event: &ToolbarEvent,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        let Some(action) = self.bar_action(event) else {
            return;
        };
        self.dispatch(action, doc);
        self.refresh_from_doc(doc);
        self.dirty = true;
    }

    /// What `event` asks of the page the window shows now, if anything.
    fn bar_action(&self, event: &ToolbarEvent) -> Option<Action> {
        match (event, self.view) {
            (ToolbarEvent::ToggleSidebar, View::Reader) => Some(Action::ToggleToc),
            (ToolbarEvent::Back, View::Reader) => Some(Action::Back),
            (ToolbarEvent::Back, View::Settings) => Some(Action::CloseSettings),
            (ToolbarEvent::Back, View::Search) => Some(Action::CloseSearch),
            (ToolbarEvent::Action { id }, view) => Self::bar_action_named(id, view),
            _ => None,
        }
    }

    fn bar_action_named(id: &str, view: View) -> Option<Action> {
        match (id, view) {
            (OPEN_BOOK, View::Library) => Some(Action::OpenViaPortal),
            (HIDE_FINISHED, View::Library) => Some(Action::ToggleHideFinished),
            (SETTINGS, View::Library) => Some(Action::OpenSettings),
            (APPEARANCE, View::Reader) => Some(Action::ToggleTypePopover),
            (BOOKMARK, View::Reader) => Some(Action::ToggleBookmark),
            (ANNOTATIONS, View::Reader) => Some(Action::ToggleNotes),
            (id, View::Library) => LibrarySort::ALL
                .into_iter()
                .find(|sort| sort_id(*sort) == id)
                .map(Action::SetSort),
            (id, View::Settings) => SETTINGS_TABS
                .iter()
                .map(|(tab, _)| *tab)
                .find(|tab| tab_id(*tab) == id)
                .map(Action::SetSettingsTab),
            _ => None,
        }
    }
}

fn back() -> ToolbarNavigation {
    ToolbarNavigation {
        back: true,
        forward: false,
    }
}

fn location(label: &str) -> ToolbarLocation {
    ToolbarLocation {
        label: label.into(),
        ..ToolbarLocation::default()
    }
}

#[cfg(test)]
#[path = "window_bar_tests.rs"]
mod tests;
