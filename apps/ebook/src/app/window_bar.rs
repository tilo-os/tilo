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

use ui::framework::{BarActions, BarCommands, Dispatcher};
use ui::icons::Icon;
use ui::theme::Theme;
use ui::widget::ControlSize;
use ui::window_toolbar::{
    ToolbarAction, ToolbarEvent, ToolbarLocation, ToolbarNavigation, WindowToolbar,
};
use vello::kurbo::Rect;

use super::{Action, BooksApp, View};
use crate::model::LibrarySort;
use crate::resource::Ebook;

/// The bar's entries: the library's edits, and the window's actions.
pub(super) struct Bar {
    edits: BarActions<Ebook>,
    actions: BarCommands<Action>,
}

impl Bar {
    /// Entries submitted through `dispatcher`, the window's.
    pub(super) fn new(dispatcher: Dispatcher<Ebook>) -> Self {
        Self {
            edits: BarActions::new(dispatcher),
            actions: BarCommands::new(),
        }
    }
}

impl BooksApp {
    /// The window bar's slots for what the window shows now.
    pub(crate) fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = WindowToolbar {
            location: Some(self.location()),
            ..WindowToolbar::default()
        };
        let Bar { edits, actions } = &self.bar;
        let prefs = Ebook::paths().prefs;
        match self.view {
            View::Library => {
                let sort = LibrarySort::ALL
                    .iter()
                    .map(|sort| {
                        ToolbarAction {
                            name: sort.label().into(),
                            icon: Icon::Check.name().into(),
                            ..edits.action(prefs.sort.set(*sort))
                        }
                        .on(*sort == self.library_prefs.sort)
                    })
                    .collect();
                let hide_read = self.library_prefs.hide_read;
                toolbar.actions = vec![
                    actions.entry(Icon::Plus.name(), "Open book", Action::OpenViaPortal),
                    ToolbarAction::new("books.sort", Icon::ArrowUpDown.name(), "Sort books")
                        .with_items(sort),
                    ToolbarAction {
                        name: "Hide finished books".into(),
                        icon: Icon::Eye.name().into(),
                        ..edits.action(prefs.hide_read.set(!hide_read))
                    }
                    .on(hide_read)
                    .in_menu(),
                    actions.entry(Icon::Settings.name(), "Settings", Action::OpenSettings),
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
                let bookmark =
                    Ebook::TOGGLE_BOOKMARK.with(self.bookmark_params(self.page_top_block));
                toolbar.actions = vec![
                    ToolbarAction {
                        name: "Bookmark this page".into(),
                        icon: Icon::Bookmark.name().into(),
                        ..edits.action(bookmark)
                    }
                    .on(self.page_bookmarked()),
                    actions
                        .entry(Icon::PanelRight.name(), "Annotations", Action::ToggleNotes)
                        .on(self.notes_open()),
                    actions
                        .entry(
                            Icon::Type.name(),
                            "Reading appearance",
                            Action::ToggleTypePopover,
                        )
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
                event => {
                    if !self.bar.edits.press(&event)
                        && let Some(action) = self.bar.actions.pressed(&event)
                    {
                        self.dispatch(action, edits);
                    }
                }
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
