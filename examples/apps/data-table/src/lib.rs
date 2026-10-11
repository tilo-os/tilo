//! Data table: columns over a sparse source of a hundred million rows.
//!
//! A `DataTable` is built from column definitions and a `TableModel`. Text
//! columns format a row into a string (which is also the cell's accessible
//! value); widget columns build a live control per visible cell. The model
//! stands in for a database: it reports a row count and loads the rows in
//! view in the background, discarding answers for windows scrolled past.
//!
//! The rows are not resource state. What the user did with them, which ones
//! they opened, is: stored, synced and undoable. A cell button reaches the
//! resource through `cx.actions()`, because cells are built on demand long
//! after the view function returned.

/// The application id.
pub const APP_ID: &str = "so.tilo.example.data-table";
/// The window and launcher title.
pub const APP_TITLE: &str = "Data Table";

pub mod locales;

fn install_catalog() {
    locales::install();
}

use std::collections::HashMap;
use std::ops::Range;
use ui::i18n::I18nManager;
use ui::prelude::*;

/// How many rows the source reports.
pub const ROW_COUNT: usize = 100_000_000;
/// How many recently opened rows the review keeps.
const RECENT: usize = 5;

/// One row of the external data set. It is not part of the resource: the rows
/// come from a sparse source (a database, a service) and only the visible
/// window is ever loaded.
#[derive(Clone)]
pub struct Row {
    pub id: u64,
    pub status: Status,
}

/// A row's status, shown through the catalog rather than as source text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Open,
    Closed,
}

impl Status {
    fn label(self) -> String {
        I18nManager::lookup_plain(match self {
            Self::Open => "data-status-open",
            Self::Closed => "data-status-closed",
        })
    }
}

/// The app's own state: what the user did with the data. It is stored, synced
/// and undoable; the rows themselves stay in their source.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Review {
    /// Recently opened row ids, newest first.
    pub opened: Vec<u64>,
}

#[resource(kind = "example.review", label = "Results")]
impl Review {
    /// Open a row.
    #[action]
    pub fn open_row(&mut self, #[param("Stable row id")] id: u64) -> Result<(), &'static str> {
        if !usize::try_from(id).is_ok_and(|index| index < ROW_COUNT) {
            return Err("no such row");
        }
        self.opened.retain(|opened| *opened != id);
        self.opened.insert(0, id);
        self.opened.truncate(RECENT);
        Ok(())
    }
}

/// A sparse asynchronous model with one current viewport request.
///
/// Replace the worker's sample row construction with a database or network
/// call. The table-facing API, stale-result suppression, targeted change
/// notification, and application wakeup remain the same.
pub struct Rows {
    cache: HashMap<usize, Row>,
    requested: Range<usize>,
    error: Option<String>,
    request: LatestRequest<Result<Vec<Row>, String>>,
}

impl Rows {
    fn new() -> Self {
        Self {
            cache: HashMap::new(),
            requested: 0..0,
            error: None,
            request: LatestRequest::new(),
        }
    }
}

impl TableModel for Rows {
    type Row = Row;

    fn len(&self) -> usize {
        ROW_COUNT
    }

    fn key_at(&self, index: usize) -> Option<u64> {
        (index < ROW_COUNT)
            .then_some(index)
            .and_then(|index| u64::try_from(index).ok())
    }

    fn index_of(&self, key: u64) -> Option<usize> {
        usize::try_from(key).ok().filter(|index| *index < ROW_COUNT)
    }

    fn row_state(&self, index: usize) -> TableRowState<'_, Row> {
        if let Some(row) = self.cache.get(&index) {
            TableRowState::Ready(row)
        } else if self.requested.contains(&index)
            && let Some(error) = self.error.as_deref()
        {
            TableRowState::Error(error)
        } else {
            TableRowState::Loading
        }
    }

    fn request_range(&mut self, range: Range<usize>) {
        let resident = range.clone().all(|index| self.cache.contains_key(&index));
        if range == self.requested
            && (self.request.is_pending() || resident || self.error.is_some())
        {
            return;
        }
        self.requested = range.clone();
        self.error = None;
        self.cache.retain(|index, _| range.contains(index));

        // A newer request makes this completion stale, so a slow answer for a
        // window the user scrolled past never overwrites a newer one.
        let completion = self.request.begin();
        // A worker where the target has one; the page's executor otherwise.
        let spawned = ui::background::Background::current().spawn("load-rows", move || {
            let rows = Ok(range
                .filter_map(|index| u64::try_from(index).ok())
                .map(|id| Row {
                    id,
                    status: if id % 3 == 0 {
                        Status::Closed
                    } else {
                        Status::Open
                    },
                })
                .collect());
            let _ = completion.complete(rows);
        });
        if spawned.is_err() {
            self.error = Some("Background work is not available here.".into());
        }
    }

    fn poll_changes(&mut self) -> Vec<TableModelChange> {
        let Some(rows) = self.request.try_take() else {
            return Vec::new();
        };
        let mut changed = Vec::new();
        match rows {
            Ok(rows) => {
                for row in rows {
                    changed.push(row.id);
                    if let Ok(index) = usize::try_from(row.id) {
                        self.cache.insert(index, row);
                    }
                }
            }
            Err(error) => {
                self.error = Some(error);
                changed.extend(
                    self.requested
                        .clone()
                        .filter_map(|index| u64::try_from(index).ok()),
                );
            }
        }
        (!changed.is_empty())
            .then_some(TableModelChange::RowsChanged { keys: changed })
            .into_iter()
            .collect()
    }
}

pub struct App;

impl ResourceApp for App {
    type Resource = Review;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn view(cx: &mut ViewCx<Review>) -> Element {
        install_catalog();
        // Rows are built on demand as the table scrolls, long after this view
        // was; they bind their buttons through the resource's actions.
        let actions = cx.actions();
        let columns = vec![
            ColumnDef::<Row>::new("name", "data-name-column", ColumnWidth::Flex(1.0))
                .with_text(|row| I18nManager::format("data-row", &[("id", row.id.into())])),
            ColumnDef::<Row>::new("status", "data-status-column", ColumnWidth::Fixed(120.0))
                .with_text(|row| row.status.label()),
            ColumnDef::<Row>::new("actions", "data-actions-column", ColumnWidth::Fixed(120.0))
                .with_widget(move |row, _| {
                    Box::new(
                        actions.bind_invocation(
                            Button::new(&format!("open-{}", row.id), "data-open")
                                .control_size(ui::widget::ControlSize::Small),
                            Review::OPEN_ROW.with(row.id),
                        ),
                    )
                }),
        ];
        let mut table = DataTable::from_model(columns, Rows::new())
            .with_id("rows-table")
            .with_accessible_name("data-accessible-name")
            .selectable(true);
        table.set_viewport(720.0, 32.0);
        let recent = cx
            .text(|review| {
                if review.opened.is_empty() {
                    return I18nManager::lookup_plain("data-none-opened");
                }
                let rows = review
                    .opened
                    .iter()
                    .map(|id| I18nManager::format("data-row", &[("id", (*id).into())]))
                    .collect::<Vec<_>>()
                    .join(", ");
                I18nManager::format("data-recent", &[("rows", rows.into())])
            })
            .with_id("recently-opened");
        Box::new(ui::column![table, recent])
    }
}

ui::dev_reload_entry!(App);
