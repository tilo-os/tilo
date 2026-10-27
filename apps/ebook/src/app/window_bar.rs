//! Books' controls, as slots in the window's bar ([`ui::window_toolbar`]):
//! the shelf's Open book, sort and Hide finished and the Settings entry;
//! the reader's contents rail as the window's sidebar, Back, the book as the
//! location (its author after the title, where the bar elides), and the
//! reading controls — bookmark, annotations and the reading-appearance
//! ("Aa") popover. The window draws no header row of its own: the library,
//! the reader, the settings page and the search results all begin where the
//! bar ends, as every other app's content does.
//!
//! What a slot stands for is what the control it replaced stood for: the same
//! actions, the same persisted preferences, the same popover. The settings
//! page's tabs are that page's own views of one document and stay in the
//! body, under its title, where the page put them.

use ui::icons::Icon;
use ui::theme::Theme;
use ui::widget::ControlSize;
use ui::window_toolbar::{
    ToolbarAction, ToolbarEvent, ToolbarLocation, ToolbarNavigation, WindowToolbar,
};
use vello::kurbo::Rect;

use super::{Action, BooksApp, View};
use crate::model::LibrarySort;

/// The library's primary way in: a book through the host picker.
pub(crate) const OPEN_BOOK: &str = "books.open";
/// The library's sort order, as a menu of the orders it can be in.
pub(crate) const SORT: &str = "books.sort";
/// One sort order, by the value the shelf's preference is stored under.
const SORT_CHOICE: &str = "books.sort.";
/// Whether finished books are hidden from the shelf.
pub(crate) const HIDE_FINISHED: &str = "books.hide-finished";
/// The font and colour-scheme managers.
pub(crate) const SETTINGS: &str = "books.settings";
/// The reading controls: bookmark the page, show the annotations rail, and
/// the reading-appearance popover.
pub(crate) const BOOKMARK: &str = "books.bookmark";
pub(crate) const ANNOTATIONS: &str = "books.annotations";
pub(crate) const APPEARANCE: &str = "books.appearance";

impl BooksApp {
    /// The window bar's slots for what the window shows now.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = WindowToolbar {
            location: Some(self.location()),
            ..WindowToolbar::default()
        };
        match self.view {
            View::Library => {
                let sort = LibrarySort::ALL
                    .iter()
                    .map(|sort| {
                        ToolbarAction::new(
                            format!("{SORT_CHOICE}{}", sort.as_str()),
                            Icon::Check.name(),
                            sort.label(),
                        )
                        .on(*sort == self.library_prefs.sort)
                    })
                    .collect();
                toolbar.actions = vec![
                    ToolbarAction::new(OPEN_BOOK, Icon::Plus.name(), "Open book"),
                    ToolbarAction::new(SORT, Icon::ArrowUpDown.name(), "Sort books")
                        .with_items(sort),
                    ToolbarAction::new(HIDE_FINISHED, Icon::Eye.name(), "Hide finished books")
                        .on(self.library_prefs.hide_read)
                        .in_menu(),
                    ToolbarAction::new(SETTINGS, Icon::Settings.name(), "Settings"),
                ];
            }
            View::Reader => {
                // The contents rail is the window's sidebar, as a list of
                // chapters beside a document is in every other reader.
                toolbar.sidebar = Some(self.toc_open());
                toolbar.navigation = Some(ToolbarNavigation {
                    back: true,
                    forward: false,
                });
                toolbar.actions = vec![
                    ToolbarAction::new(BOOKMARK, Icon::Bookmark.name(), "Bookmark this page")
                        .on(self.page_bookmarked()),
                    ToolbarAction::new(ANNOTATIONS, Icon::PanelRight.name(), "Annotations")
                        .on(self.notes_open()),
                    ToolbarAction::new(APPEARANCE, Icon::Type.name(), "Reading appearance")
                        .on(self.reading_settings.is_open()),
                ];
            }
            View::Settings | View::Search => {
                toolbar.navigation = Some(ToolbarNavigation {
                    back: true,
                    forward: false,
                });
            }
        }
        toolbar
    }

    /// Where the window is: the open book, by title and author, or the page
    /// of the app it shows. The author is the elided detail the bar drops
    /// before it shortens the title.
    fn location(&self) -> ToolbarLocation {
        match self.view {
            View::Library => ToolbarLocation {
                label: "Library".into(),
                ..ToolbarLocation::default()
            },
            View::Settings => ToolbarLocation {
                label: "Settings".into(),
                ..ToolbarLocation::default()
            },
            View::Search => ToolbarLocation {
                label: "Search".into(),
                ..ToolbarLocation::default()
            },
            View::Reader => {
                let Some(book) = self.library.get(self.book_idx) else {
                    return ToolbarLocation {
                        label: "Library".into(),
                        ..ToolbarLocation::default()
                    };
                };
                let title = book.title.clone();
                // The title, with the author after it in words: where the
                // row has room the bar shows both, and where it has not it
                // falls back to the title alone.
                let label = if book.author.is_empty() {
                    title.clone()
                } else {
                    format!("{title} by {}", book.author)
                };
                ToolbarLocation {
                    shorter: vec![title],
                    label,
                    ..ToolbarLocation::default()
                }
            }
        }
    }

    /// Take what the person did in the bar since the last pass, and carry it
    /// out through the same actions the controls it replaced ran.
    pub(crate) fn take_bar_events(&mut self, edits: &mut ui::widget::Edits<'_>) {
        for event in ui::window_toolbar::take_events() {
            match event {
                ToolbarEvent::ToggleSidebar => self.dispatch(Action::ToggleToc, edits),
                ToolbarEvent::Back => {
                    let back = match self.view {
                        View::Settings => Some(Action::CloseSettings),
                        View::Search => Some(Action::CloseSearch),
                        View::Reader => Some(Action::Back),
                        View::Library => None,
                    };
                    if let Some(back) = back {
                        self.dispatch(back, edits);
                    }
                }
                ToolbarEvent::Action { id } => {
                    if let Some(sort) = id.strip_prefix(SORT_CHOICE) {
                        self.choose_sort(LibrarySort::from_str(sort), edits);
                    } else if id == HIDE_FINISHED {
                        self.toggle_finished_hidden(edits);
                    } else if let Some(action) = match id.as_str() {
                        OPEN_BOOK => Some(Action::OpenViaPortal),
                        SETTINGS => Some(Action::OpenSettings),
                        BOOKMARK => Some(Action::ToggleBookmark),
                        ANNOTATIONS => Some(Action::ToggleNotes),
                        APPEARANCE => Some(Action::ToggleTypePopover),
                        _ => None,
                    } {
                        self.dispatch(action, edits);
                    }
                }
                _ => {}
            }
        }
    }

    /// Declare the slots the bar shows: an equal declaration is free, and a
    /// changed one moves the bar without a frame of the app.
    pub(crate) fn declare_window_bar(&self) {
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }

    /// Where the reading-appearance popover hangs from: the Aa control's
    /// place in the window's bar, but never past the reading column — the
    /// card is the app's own, a settings panel over the page, and it must not
    /// cover the docked annotations rail beside it.
    pub(crate) fn appearance_anchor(&self, theme: &Theme) -> Rect {
        let side = ControlSize::Regular.height(theme);
        let inset = f64::from(theme.tokens.spacing.s3);
        let x1 = self.notes_drawer.content_rect().x1.min(self.rect.x1) - inset;
        Rect::new(x1 - side, self.rect.y0, x1, self.rect.y0)
    }
}
