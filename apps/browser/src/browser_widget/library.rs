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
    Button, Column, Element, EmptyState, ListRow, Row, Section, SectionHeader, Spacer, Text, Widget,
};

use super::{BrowserWidget, LibrarySection, library_filter_matches};
use crate::actions::{BOOKMARKS_SCOPE, DOWNLOADS_SCOPE, HISTORY_SCOPE, history_resource_id};
use crate::downloads::DownloadStatus;
use crate::resource::{Browser, CATALOG, bookmark_resource_id};

/// How many rows a section materialises. The rest stay reachable through
/// search, which is bounded by its own pages.
const ROW_LIMIT: usize = 50;

/// The library's sections in tab order, with the copy that names each one and
/// the resource it shows.
const SECTIONS: [(LibrarySection, &str, &str); 4] = [
    (
        LibrarySection::History,
        "browser-library-section-history",
        HISTORY_SCOPE,
    ),
    (
        LibrarySection::Bookmarks,
        "browser-library-section-bookmarks",
        BOOKMARKS_SCOPE,
    ),
    (
        LibrarySection::Downloads,
        "browser-library-section-downloads",
        DOWNLOADS_SCOPE,
    ),
    (
        LibrarySection::Extensions,
        "browser-library-section-extensions",
        "browser.extensions",
    ),
];

/// A row's name for assistive technology: a sentence from the catalogue with
/// the thing it acts on interpolated.
fn named(key: &str, title: &str) -> String {
    CATALOG.t_args(
        key,
        &[("title", ui::i18n::ArgValue::Text(title.to_owned()))],
    )
}

/// A download row's name for assistive technology.
fn named_download(key: &str, name: &str) -> String {
    CATALOG.t_args(key, &[("name", ui::i18n::ArgValue::Text(name.to_owned()))])
}

/// What the window bar's location commits for the Library itself.
pub(super) const LIBRARY_LOCATION: &str = "browser:library";

impl BrowserWidget {
    /// Where the window bar puts the Library: the Library, then the section
    /// on screen, each committing its place (the section, its resource).
    pub(super) fn library_segments(&self) -> Vec<(String, &'static str)> {
        let mut segments = vec![(CATALOG.t("browser-library-title"), LIBRARY_LOCATION)];
        segments.extend(
            SECTIONS
                .iter()
                .find(|(section, ..)| *section == self.library_section)
                .map(|(_, label, resource)| (CATALOG.t(label), *resource)),
        );
        segments
    }

    /// The Library's section whose resource is `place`, as a bar segment
    /// commits it.
    pub(super) fn library_section_at(place: &str) -> Option<&'static str> {
        SECTIONS
            .iter()
            .find(|(_, _, resource)| *resource == place)
            .map(|(_, _, resource)| *resource)
    }

    /// The visited pages, newest first, in one group per day: Today,
    /// Yesterday, the days of the past week by name, then dates. Each row
    /// says where the page is and when it was last open.
    fn history_groups(&self) -> Vec<Element> {
        let today = ui::clock::today();
        let mut entries: Vec<&crate::history::HistoryEntry> = self
            .history
            .entries()
            .iter()
            .filter(|entry| {
                library_filter_matches(&self.library_filter.get(), &[&entry.title, &entry.url])
            })
            .collect();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.last_visit));
        let mut groups: Vec<(String, Vec<Element>)> = Vec::new();
        for entry in entries.into_iter().take(ROW_LIMIT) {
            let when = visit_time(entry.last_visit);
            let heading =
                when.map_or_else(String::new, |when| day_heading(when.date_naive(), today));
            let id = history_resource_id(&entry.url);
            let site = crate::omnibox::display_url(&entry.url);
            let host = site
                .split(['/', '?', '#'])
                .next()
                .unwrap_or(&site)
                .to_owned();
            let title = if entry.title.trim().is_empty() {
                site.as_str()
            } else {
                entry.title.as_str()
            };
            let detail = match when {
                Some(when) => format!("{host} — {}", ui::clock::time_of_day(&when)),
                None => host,
            };
            let mut row = ListRow::new(format!("{id}.open"), title)
                .secondary(detail)
                .disclosure();
            if let Some(binding) = self.unit_binding(&id, "open") {
                row = row.on_action(binding);
            }
            // One page can be forgotten without losing the rest, through the
            // same verb that clears all of them. A window that keeps no
            // history record of its own publishes no such row, and offers no
            // removal.
            let forget: Vec<Button> = self
                .unit_binding(&id, "forget-visit")
                .map(|binding| {
                    Button::icon_only(
                        &format!("{id}.forget"),
                        Icon::Trash2,
                        &named("browser-library-forget-visit", title),
                    )
                    .on_action(binding)
                })
                .into_iter()
                .collect();
            let row = crate::rows::with_buttons(row, forget);
            match groups.last_mut() {
                Some((current, rows)) if *current == heading => rows.push(Box::new(row)),
                _ => groups.push((heading, vec![Box::new(row) as Element])),
            }
        }
        groups
            .into_iter()
            .map(|(heading, rows)| {
                let section = Section::new(rows);
                Box::new(if heading.is_empty() {
                    section
                } else {
                    section.with_header(SectionHeader::group(heading))
                }) as Element
            })
            .collect()
    }

    /// Forget one visited page in the person's record, and show the history
    /// it leaves: the same verb the Settings row clears all of them with,
    /// for one page. A window that keeps no record of its own has nothing to
    /// forget from.
    pub(super) fn forget_visit(&mut self, url: &str) -> Result<(), String> {
        let record = self
            .person_history
            .as_ref()
            .ok_or("This window keeps no browsing history to forget from")?;
        let url = url.trim();
        if url.is_empty() {
            return Err("a page is forgotten by its address".into());
        }
        record
            .invoke(&crate::history::BrowsingHistory::FORGET.with(url.to_owned()))
            .map_err(|error| {
                tracing::warn!(%error, "browser visit was not forgotten");
                "Couldn’t forget this page because Browser could not save the change. It is still in your history."
                    .to_string()
            })?;
        self.follow_history();
        self.suggestions.clear();
        self.last_query = None;
        self.structure_dirty = true;
        Ok(())
    }

    pub(super) fn bookmark_rows(&self) -> Vec<Element> {
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
                        &named("browser-library-remove-bookmark", title),
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
                                Button::new(
                                    &format!("{id}.cancel"),
                                    &CATALOG.t("browser-download-cancel"),
                                )
                                .with_accessible_name(
                                    &named_download("browser-download-cancel-name", name),
                                ),
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
                                Button::new(
                                    &format!("{id}.reveal"),
                                    &CATALOG.t("browser-download-reveal"),
                                )
                                .with_accessible_name(
                                    &named_download("browser-download-reveal-name", name),
                                ),
                                &id,
                                "reveal",
                            ),
                        );
                    }
                    DownloadStatus::Failed | DownloadStatus::Cancelled => {
                        row = row.error();
                        actions.push(
                            self.bind_button(
                                Button::new(
                                    &format!("{id}.retry"),
                                    &CATALOG.t("browser-download-retry"),
                                )
                                .with_accessible_name(
                                    &named_download("browser-download-retry-name", name),
                                ),
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
                        bar.with_label(named_download("browser-download-downloading", name)),
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
            return EmptyState::new(CATALOG.t("browser-library-no-match")).with_icon(Icon::Search);
        }
        match self.library_section {
            LibrarySection::History if self.history_paused() => {
                EmptyState::new(CATALOG.t("browser-library-history-paused")).with_icon(Icon::Pause)
            }
            LibrarySection::History => {
                EmptyState::new(CATALOG.t("browser-library-history-empty")).with_icon(Icon::History)
            }
            LibrarySection::Bookmarks => {
                EmptyState::new(CATALOG.t("browser-library-bookmarks-empty"))
                    .with_icon(Icon::Star)
                    .with_message(CATALOG.t("browser-library-bookmarks-empty-hint"))
            }
            LibrarySection::Downloads => {
                EmptyState::new(CATALOG.t("browser-downloads-empty")).with_icon(Icon::Download)
            }
            LibrarySection::Extensions => {
                EmptyState::new(CATALOG.t("browser-library-extensions-empty"))
                    .with_icon(Icon::Layers)
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
        if self.library_section == LibrarySection::Bookmarks && self.person_bookmarks.is_some() {
            return self.personal_bookmark_body(theme);
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
                let groups = self.history_groups();
                (!groups.is_empty()).then(|| {
                    Box::new(
                        groups.into_iter().fold(
                            Column::new()
                                .with_no_padding()
                                .with_intrinsic_height()
                                .with_rhythm(ui::tokens::Rhythm::Group),
                            Column::push,
                        ),
                    ) as Element
                })
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

    /// The Library's own header outside a Tilo session: its title, Pause
    /// history, Clear filter, search and Done. While the desktop's bar
    /// hosts the window, the bar's location names the Library, Clear filter
    /// is its action, Pause history is in its menu, its search is the bar's
    /// and its Library button closes it.
    fn library_header(&self, current: usize) -> Row {
        let mut header = Row::new().no_wrap().push(Box::new(
            Text::new(&CATALOG.t("browser-library-title"))
                .with_role(TextRole::LargeTitle)
                .with_heading_level(1),
        ));
        header = header.push(Box::new(Spacer::new()));
        if self.library_section == LibrarySection::History {
            let label = CATALOG.t(if self.history_paused() {
                "browser-library-resume"
            } else {
                "browser-library-pause"
            });
            header = header.push(Box::new(self.bind_button(
                Button::new("browser.library.history-pause", &label),
                "browser.session",
                "toggle-pause",
            )));
        }
        if !self.library_filter.get().is_empty() {
            header = header.push(Box::new(self.bind_button(
                Button::icon_only(
                    "browser.library.clear-filter",
                    Icon::X,
                    &CATALOG.t("browser-library-clear-filter"),
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
            // window-bar: fallback, the bar's search while it hosts the window
            header = header.push(Box::new(search_button(SearchRequest::within(
                ScopeDecl::new(*scope, CATALOG.t(label)),
            ))));
        }
        header = header.push(Box::new(self.bind_button(
            Button::new("browser.library.close", &CATALOG.t("browser-library-done")),
            "browser",
            "show-page",
        )));

        header
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
        let header = (!ui::window_toolbar::is_hosted()).then(|| self.library_header(current));

        let queue = self.pending_commands.clone();
        let tabs = ui::widget::ViewTabs::new(
            SECTIONS
                .iter()
                .map(|(_, label, _)| Segment::new(CATALOG.t(label)))
                .collect(),
            current,
        )
        .on_view("browser.library.sections", move |index| {
            if let Some((_, _, resource)) = SECTIONS.get(index) {
                queue.update(|commands| commands.push(((*resource).to_owned(), "show".to_owned())));
            }
        });

        let mut parts: Vec<Element> = notices.iter().map(|notice| notice.widget(false)).collect();
        if let Some(header) = header {
            parts.push(Box::new(header));
        }
        parts.push(Box::new(
            Row::new()
                .no_wrap()
                .with_padding(0.0, 0.0)
                .push(Box::new(tabs)),
        ));
        if matches!(
            self.library_section,
            LibrarySection::Bookmarks | LibrarySection::History
        ) && let Some(notice) = self.continuity_notice()
        {
            parts.push(notice);
        }
        parts.push(self.library_body(theme));
        crate::settings::page(parts)
    }
}

fn visit_time(unix_secs: u64) -> Option<chrono::DateTime<chrono::Local>> {
    let secs = i64::try_from(unix_secs).ok()?;
    chrono::DateTime::from_timestamp(secs, 0).map(|utc| utc.with_timezone(&chrono::Local))
}

/// The heading of the day a visit falls on, seen from `today`: Today and
/// Yesterday, a weekday within the past week, otherwise the date (with the
/// year once it is not this year's).
fn day_heading(day: chrono::NaiveDate, today: chrono::NaiveDate) -> String {
    use chrono::Datelike as _;
    let days = (day - today).num_days();
    // A visit stamped ahead of this clock (another device's) is today's.
    if days >= 0 {
        return ui::format::near_day(0).unwrap_or_default();
    }
    if let Some(near) = ui::format::near_day(days) {
        return near;
    }
    if days > -7 {
        return ui::format::weekday_name(day.weekday());
    }
    ui::format::day_heading(day, day.year() != today.year())
}

#[cfg(test)]
mod history_day_tests {
    use super::day_heading;
    use chrono::NaiveDate;

    #[test]
    fn history_days_read_as_a_person_names_them() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 30).unwrap();
        let day = |m, d| NaiveDate::from_ymd_opt(2026, m, d).unwrap();
        assert_eq!(day_heading(today, today), "Today");
        assert_eq!(day_heading(day(7, 1), today), "Today");
        assert_eq!(day_heading(day(6, 29), today), "Yesterday");
        assert_eq!(day_heading(day(6, 26), today), "Friday");
        assert_eq!(day_heading(day(6, 1), today), "Monday, June 1");
        assert_eq!(
            day_heading(NaiveDate::from_ymd_opt(2025, 12, 24).unwrap(), today),
            "Wednesday, December 24, 2025"
        );
    }
}
