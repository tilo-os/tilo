//! The Library page: history, bookmarks, downloads and extensions.
//!
//! The page is a title, a section switcher and one list of rows. Finding an
//! item is the platform search, narrowed to the section on screen by the
//! magnifier button in the header; the page draws no field of its own.

use ui::icons::Icon;
use ui::search::{ScopeDecl, SearchRequest, search_button};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Button, Column, Element, EmptyState, ListRow, Row, Section, SegmentedStyle, Spacer, Text,
    ToggleGroup, Widget,
};

use super::{BrowserWidget, LibrarySection, library_filter_matches};
use crate::actions::{BOOKMARKS_SCOPE, DOWNLOADS_SCOPE, HISTORY_SCOPE, history_resource_id};
use crate::downloads::DownloadStatus;
use crate::resource::{Browser, bookmark_resource_id};

/// How many rows a section materialises. The rest stay reachable through
/// search, which is bounded by its own pages.
const ROW_LIMIT: usize = 50;

/// The library's sections in tab order, with the resource each one shows.
const SECTIONS: [(LibrarySection, &str, &str); 4] = [
    (LibrarySection::History, "History", HISTORY_SCOPE),
    (LibrarySection::Bookmarks, "Bookmarks", BOOKMARKS_SCOPE),
    (LibrarySection::Downloads, "Downloads", DOWNLOADS_SCOPE),
    (
        LibrarySection::Extensions,
        "Extensions",
        "browser.extensions",
    ),
];

impl BrowserWidget {
    fn history_rows(&self) -> Vec<Element> {
        self.history
            .entries()
            .iter()
            .rev()
            .filter(|entry| {
                library_filter_matches(&self.library_filter.get(), &[&entry.title, &entry.url])
            })
            .take(ROW_LIMIT)
            .map(|entry| {
                let id = history_resource_id(&entry.url);
                let title = if entry.title.trim().is_empty() {
                    entry.url.as_str()
                } else {
                    entry.title.as_str()
                };
                let mut row = ListRow::new(format!("{id}.open"), title)
                    .secondary(&entry.url)
                    .disclosure();
                if let Some(binding) = self.unit_binding(&id, "open") {
                    row = row.on_action(binding);
                }
                Box::new(row) as Element
            })
            .collect()
    }

    fn bookmark_rows(&self) -> Vec<Element> {
        self.bookmarks
            .entries()
            .iter()
            .filter(|entry| {
                library_filter_matches(&self.library_filter.get(), &[&entry.title, &entry.url])
            })
            .take(ROW_LIMIT)
            .map(|entry| {
                // The bookmark's item resource, which this window's row joins.
                let id = bookmark_resource_id(&entry.id);
                let title = if entry.title.trim().is_empty() {
                    entry.url.as_str()
                } else {
                    entry.title.as_str()
                };
                let mut row = ListRow::new(format!("{id}.open"), title).secondary(&entry.url);
                if let Some(binding) = self.unit_binding(&id, "open") {
                    row = row.on_action(binding);
                }
                let remove = self.route_document_button(
                    Button::icon_only(
                        &format!("{id}.remove"),
                        Icon::Trash2,
                        &format!("Remove bookmark {title}"),
                    ),
                    &id,
                    "remove-bookmark",
                    self.is_framework_hosted()
                        .then(|| Browser::REMOVE_BOOKMARK.with(entry.id.clone())),
                );
                Box::new(crate::rows::with_buttons(row, vec![remove])) as Element
            })
            .collect()
    }

    fn download_rows(&self, theme: &Theme) -> Vec<Element> {
        self.downloads
            .entries()
            .iter()
            .rev()
            .filter(|entry| {
                library_filter_matches(
                    &self.library_filter.get(),
                    &[
                        entry.display_name(),
                        &entry.source_url,
                        &super::download_status_text(entry),
                    ],
                )
            })
            .take(ROW_LIMIT)
            .map(|entry| {
                let id = entry.resource_id();
                let name = entry.display_name();
                let mut row = ListRow::new(format!("{id}.row"), name)
                    .secondary(super::download_status_text(entry));
                let mut actions = Vec::new();
                let mut progress = None;
                match entry.status {
                    DownloadStatus::AwaitingDestination | DownloadStatus::Downloading => {
                        progress = (entry.status == DownloadStatus::Downloading).then(|| {
                            crate::rows::transfer_bar(entry.received_bytes, entry.total_bytes)
                        });
                        actions.push(
                            self.bind_button(
                                Button::new(&format!("{id}.cancel"), "Cancel")
                                    .with_accessible_name(&format!("Cancel download {name}")),
                                &id,
                                "cancel",
                            ),
                        );
                    }
                    DownloadStatus::Completed => {
                        if let Some(binding) = self.unit_binding(&id, "open") {
                            row = row.on_action(binding);
                        }
                        actions.push(
                            self.bind_button(
                                Button::new(&format!("{id}.reveal"), "Show in Files")
                                    .with_accessible_name(&format!("Show {name} in Files")),
                                &id,
                                "reveal",
                            ),
                        );
                    }
                    DownloadStatus::Failed | DownloadStatus::Cancelled => {
                        row = row.error();
                        actions.push(
                            self.bind_button(
                                Button::new(&format!("{id}.retry"), "Retry")
                                    .with_accessible_name(&format!("Retry download {name}")),
                                &id,
                                "retry",
                            ),
                        );
                    }
                }
                let line = crate::rows::with_buttons(row, actions);
                match progress {
                    Some(bar) => crate::rows::with_progress(
                        line,
                        bar.with_label(format!("Downloading {name}")),
                        theme,
                    ),
                    None => Box::new(line) as Element,
                }
            })
            .collect()
    }

    fn extension_rows(&self) -> Vec<Element> {
        if self.headless {
            return Vec::new();
        }
        let filter = self.library_filter.get();
        self.extensions
            .library_rows(|fields| library_filter_matches(&filter, fields))
    }

    /// What the section shows when it has no rows: one sentence.
    fn library_empty_state(&self) -> EmptyState {
        if !self.library_filter.get().is_empty() {
            return EmptyState::new("No matching items").with_icon(Icon::Search);
        }
        match self.library_section {
            LibrarySection::History if self.history_paused() => {
                EmptyState::new("History is paused in this window").with_icon(Icon::Pause)
            }
            LibrarySection::History => {
                EmptyState::new("No browsing history yet").with_icon(Icon::History)
            }
            LibrarySection::Bookmarks => EmptyState::new("No bookmarks yet")
                .with_icon(Icon::Star)
                .with_message("Select the star in the address bar to keep a page."),
            LibrarySection::Downloads => {
                EmptyState::new("No downloads yet").with_icon(Icon::Download)
            }
            LibrarySection::Extensions => {
                EmptyState::new("No extensions installed").with_icon(Icon::Layers)
            }
        }
    }

    fn library_body(&self, theme: &Theme) -> Element {
        if self.library_section == LibrarySection::Downloads
            && self.capabilities.owns_download_presentation()
        {
            return self.capabilities.downloads_filtered(
                &self.engine,
                theme,
                &self.library_filter.get(),
            );
        }
        // Extensions always show their header: how to add one, and the way to.
        if self.library_section == LibrarySection::Extensions && !self.headless {
            let rows = self.extension_rows();
            let mut body = Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_rhythm(ui::tokens::Rhythm::Sibling);
            if let Some(notice) = self.extensions.notice() {
                body = body.push(notice);
            }
            body = body.push(self.extensions.library_header());
            body = body.push(if rows.is_empty() {
                Box::new(self.library_empty_state()) as Element
            } else {
                crate::rows::separated(rows, theme)
            });
            return Box::new(body);
        }
        let list = match self.library_section {
            LibrarySection::History => {
                let rows = self.history_rows();
                (!rows.is_empty()).then(|| Box::new(Section::new(rows)) as Element)
            }
            LibrarySection::Bookmarks => {
                let rows = self.bookmark_rows();
                (!rows.is_empty()).then(|| crate::rows::separated(rows, theme))
            }
            LibrarySection::Downloads => {
                let rows = self.download_rows(theme);
                (!rows.is_empty()).then(|| crate::rows::separated(rows, theme))
            }
            LibrarySection::Extensions => {
                let rows = self.extension_rows();
                (!rows.is_empty()).then(|| crate::rows::separated(rows, theme))
            }
        };
        let Some(list) = list else {
            return Box::new(self.library_empty_state());
        };
        let body = Column::new().with_no_padding().with_intrinsic_height();
        Box::new(body.push(list))
    }

    pub(super) fn build_library_composition(
        &self,
        theme: &Theme,
        notices: &[crate::notice::Notice],
    ) -> Box<dyn Widget> {
        let current = SECTIONS
            .iter()
            .position(|(section, ..)| *section == self.library_section)
            .unwrap_or(0);

        let mut header = Row::new().no_wrap().push(Box::new(
            Text::new("Library")
                .with_role(TextRole::LargeTitle)
                .with_heading_level(1),
        ));
        header = header.push(Box::new(Spacer::new()));
        if self.library_section == LibrarySection::History {
            let label = if self.history_paused() {
                "Resume history"
            } else {
                "Pause history"
            };
            header = header.push(Box::new(self.bind_button(
                Button::new("browser.library.history-pause", label),
                "browser.session",
                "toggle-pause",
            )));
        }
        if !self.library_filter.get().is_empty() {
            header = header.push(Box::new(self.bind_button(
                Button::icon_only(
                    "browser.library.clear-filter",
                    Icon::X,
                    "Clear library filter",
                ),
                "browser.library",
                "clear-library-filter",
            )));
        }
        // Search is the platform's: the button opens it narrowed to the
        // section on screen. Extensions are not a search scope, so that page
        // has no button.
        if let Some((_, label, scope)) = SECTIONS.get(current)
            && self.library_section != LibrarySection::Extensions
        {
            header = header.push(Box::new(search_button(SearchRequest::within(
                ScopeDecl::new(*scope, *label),
            ))));
        }
        header = header.push(Box::new(self.bind_button(
            Button::new("browser.library.close", "Done"),
            "browser",
            "show-page",
        )));

        let queue = self.pending_commands.clone();
        let tabs = ToggleGroup::new(
            SECTIONS
                .iter()
                .map(|(_, label, _)| Segment::new(*label))
                .collect(),
            current,
        )
        .with_style(SegmentedStyle::Tabs)
        .on_view("browser.library.sections", move |index| {
            if let Some((_, _, resource)) = SECTIONS.get(index) {
                queue.update(|commands| commands.push(((*resource).to_owned(), "show".to_owned())));
            }
        });

        let mut parts: Vec<Element> = notices.iter().map(|notice| notice.widget(false)).collect();
        parts.extend([
            Box::new(header) as Element,
            Box::new(
                Row::new()
                    .no_wrap()
                    .with_padding(0.0, 0.0)
                    .push(Box::new(tabs)),
            ),
            self.library_body(theme),
        ]);
        crate::settings::page(parts)
    }
}
