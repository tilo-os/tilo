//! The Library page: history, bookmarks, downloads and extensions.
//!
//! The page is a title, a section switcher and one list of rows. Finding an
//! item is the platform search, narrowed to the section on screen by the
//! magnifier button in the header; the page draws no field of its own.

use ui::icons::Icon;
use ui::search::{ScopeDecl, SearchRequest, search_button};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::webview::WebViewCommand;
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Banner, Button, Column, Container, Element, EmptyState, ListRow, Row, ScrollAxis, Scrollable,
    Section, SegmentedStyle, Spacer, Text, ToggleGroup, Widget,
};

use super::{BrowserWidget, LibrarySection};
use crate::actions::{
    BOOKMARKS_SCOPE, BrowserAction, DOWNLOADS_SCOPE, HISTORY_SCOPE, bookmark_resource_id,
    history_resource_id,
};
use crate::downloads::DownloadStatus;

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

    /// A row with the commands that apply to it beside it. Each command is a
    /// button, so it is reachable by keyboard and assistive technology.
    fn row_with_actions(row: ListRow, actions: Vec<Button>) -> Element {
        let mut line = Row::new().no_wrap().push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(Box::new(row)),
        ));
        for action in actions {
            line = line.push(Box::new(action));
        }
        Box::new(line)
    }

    fn bookmark_rows(&self) -> Vec<Element> {
        self.bookmarks
            .entries()
            .iter()
            .take(ROW_LIMIT)
            .map(|entry| {
                // The same normalized identity as the resource surface: legacy
                // JSON can hold a URL without its canonical trailing slash.
                let id = bookmark_resource_id(&entry.url);
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
                    "remove",
                    self.is_framework_hosted()
                        .then(|| BrowserAction::RemoveBookmark {
                            url: entry.url.clone(),
                            observed_tags: entry.tags.clone(),
                        }),
                );
                Self::row_with_actions(row, vec![remove])
            })
            .collect()
    }

    fn download_rows(&self) -> Vec<Element> {
        self.downloads
            .entries()
            .iter()
            .rev()
            .take(ROW_LIMIT)
            .map(|entry| {
                let id = entry.resource_id();
                let name = entry.display_name();
                let mut row = ListRow::new(format!("{id}.row"), name)
                    .secondary(super::download_status_text(entry));
                let mut actions = Vec::new();
                match entry.status {
                    DownloadStatus::AwaitingDestination | DownloadStatus::Downloading => {
                        row = row.meta(format!("{:.0}%", entry.progress * 100.0));
                        actions.push(
                            self.bind_button(
                                Button::new(&format!("{id}.cancel"), "Cancel", None)
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
                                Button::new(&format!("{id}.reveal"), "Show in Files", None)
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
                                Button::new(&format!("{id}.retry"), "Retry", None)
                                    .with_accessible_name(&format!("Retry download {name}")),
                                &id,
                                "retry",
                            ),
                        );
                    }
                }
                Self::row_with_actions(row, actions)
            })
            .collect()
    }

    fn extension_rows(&self) -> Vec<Element> {
        let installed = if self.headless {
            Vec::new()
        } else {
            crate::extensions::list_installed()
        };
        let mut rows: Vec<Element> = Vec::new();
        for extension in installed.into_iter().take(100) {
            let approved = crate::extensions::is_approved(&extension);
            let loaded = extension
                .path
                .canonicalize()
                .ok()
                .is_some_and(|path| self.startup_extensions.contains(&path));
            let status = if loaded {
                "Load requested at startup"
            } else if approved {
                "Approved; restart Browser to load"
            } else {
                "Inactive; permission review required"
            };
            let permissions = crate::extensions::permission_summary(&extension)
                .unwrap_or_else(|error| error.to_string());
            let info = extension.clone();
            let feedback = self.extension_feedback.clone();
            let toggle = Button::new(
                &format!("browser.extension.grant.{}", extension.id),
                if approved { "Revoke" } else { "Approve" },
                None,
            )
            .on_effect(move || {
                let message = match crate::extensions::set_approved(&info, !approved) {
                    Ok(()) => "Extension preference saved. Restart Browser to apply.".to_owned(),
                    Err(error) => error.to_string(),
                };
                if let Ok(mut slot) = feedback.lock() {
                    *slot = Some(message);
                }
            });
            let mut actions = Row::new().no_wrap().push(Box::new(toggle));
            if loaded
                && approved
                && let Some(url) = crate::extensions::action_url(&extension)
            {
                let engine = self.engine.clone();
                actions = actions.push(Box::new(
                    Button::new(
                        &format!("browser.extension.action.{}", extension.id),
                        "Open action page",
                        None,
                    )
                    .on_effect(move || engine.send(WebViewCommand::Navigate(url.clone()))),
                ));
            }
            rows.push(Box::new(
                Row::new()
                    .no_wrap()
                    .push(Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_flex_grow()
                            .with_child(Box::new(
                                ListRow::new(
                                    format!("browser.extension.{}", extension.id),
                                    extension.name.clone(),
                                )
                                .secondary(format!("{status}. {permissions}"))
                                .two_line_secondary()
                                .meta(extension.version.clone()),
                            )),
                    ))
                    .push(Box::new(actions)),
            ));
        }
        rows
    }

    /// What the section shows when it has no rows.
    fn library_empty_state(&self) -> EmptyState {
        match self.library_section {
            LibrarySection::History if self.history_paused() => {
                EmptyState::new("History is paused in this window")
                    .with_icon(Icon::Pause)
                    .with_message("New pages here are not saved to history.")
            }
            LibrarySection::History => EmptyState::new("No browsing history yet")
                .with_icon(Icon::History)
                .with_message("Pages you visit appear here. History stays on this device."),
            LibrarySection::Bookmarks => EmptyState::new("No bookmarks yet")
                .with_icon(Icon::Star)
                .with_message("Use the star in the address bar to keep a page."),
            LibrarySection::Downloads => EmptyState::new("No downloads yet")
                .with_icon(Icon::Download)
                .with_message("Files you download appear here. You choose where each one is saved."),
            LibrarySection::Extensions => EmptyState::new("No active extensions")
                .with_icon(Icon::Layers)
                .with_message(
                    "Install an unpacked Manifest V3 package in the browser extension folder, then approve its permissions here.",
                ),
        }
    }

    fn library_body(&self) -> Element {
        if self.library_section == LibrarySection::Downloads
            && self.capabilities.owns_download_presentation()
        {
            return self.capabilities.downloads(&self.engine);
        }
        let rows = match self.library_section {
            LibrarySection::History => self.history_rows(),
            LibrarySection::Bookmarks => self.bookmark_rows(),
            LibrarySection::Downloads => self.download_rows(),
            LibrarySection::Extensions => self.extension_rows(),
        };
        if rows.is_empty() {
            return Box::new(self.library_empty_state());
        }
        let mut body = Column::new().with_no_padding().with_intrinsic_height();
        if self.library_section == LibrarySection::Extensions
            && let Some(message) = &self.extension_notice
        {
            body = body.push(Box::new(Banner::info(message)));
        }
        Box::new(body.push(Box::new(Section::new(rows))))
    }

    pub(super) fn build_library_composition(&self, theme: &Theme) -> Box<dyn Widget> {
        let metrics = &theme.tokens.metrics;
        // The widest a list reads comfortably; a narrower window uses all it has.
        let measure = metrics.panel_width_sheet + metrics.panel_width_list;
        let current = SECTIONS
            .iter()
            .position(|(section, ..)| *section == self.library_section)
            .unwrap_or(0);

        // Search is the platform's: the button opens it narrowed to the
        // section on screen.
        let search = match SECTIONS.get(current) {
            Some((LibrarySection::Extensions, ..)) | None => {
                search_button(SearchRequest::this_app())
            }
            Some((_, label, scope)) => {
                search_button(SearchRequest::within(ScopeDecl::new(*scope, *label)))
            }
        };
        // The header and the tabs line up with the text inside the rows below.
        let inset = theme.tokens.spacing.s3;
        let mut header = Row::new()
            .no_wrap()
            .with_padding(inset, 0.0)
            .push(Box::new(
                Text::new("Library")
                    .with_role(TextRole::LargeTitle)
                    .with_heading_level(1),
            ))
            .push(Box::new(Spacer::new()));
        if self.library_section == LibrarySection::History {
            let label = if self.history_paused() {
                "Resume history"
            } else {
                "Pause history"
            };
            header = header.push(Box::new(self.bind_button(
                Button::new("browser.library.history-pause", label, None),
                "browser.session",
                "toggle-history-pause",
            )));
        }
        header = header
            .push(Box::new(search))
            .push(Box::new(self.bind_button(
                Button::new("browser.library.close", "Done", None),
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

        let page = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(header))
            .push(Box::new(Row::new().no_wrap().push(Box::new(tabs))))
            .push(self.library_body());
        Box::new(
            Scrollable::new(ScrollAxis::Vertical)
                .fill_parent_height()
                .with_child(Box::new(
                    Container::new()
                        .with_centered_max_width(measure)
                        .with_child(Box::new(page)),
                )),
        )
    }
}
