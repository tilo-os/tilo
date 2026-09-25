use std::collections::HashMap;
use std::ops::Range;
use ui::prelude::*;

const ROW_COUNT: usize = 100_000_000;
/// How many recently opened rows the review keeps.
const RECENT: usize = 5;

/// One row of the external data set. It is not part of the resource: the rows
/// come from a sparse source (a database, a service) and only the visible
/// window is ever loaded.
#[derive(Clone)]
pub struct Row {
    pub id: u64,
    pub name: String,
}

/// The app's own state: what the user did with the data. It is stored, synced
/// and undoable; the rows themselves stay in their source.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Review {
    /// Recently opened row ids, newest first.
    pub opened: Vec<u64>,
}

#[resource(kind = "generated.table", label = "Results")]
impl Review {
    /// Open a row.
    #[action]
    pub fn open(&mut self, #[param("Stable row id")] id: u64) -> Result<(), &'static str> {
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

        let completion = self.request.begin();
        std::thread::spawn(move || {
            let rows = Ok(range
                .map(|index| Row {
                    id: u64::try_from(index).unwrap_or(u64::MAX),
                    name: format!("Row {index}"),
                })
                .collect());
            let _ = completion.complete(rows);
        });
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

/// A hundred-million-row table with bounded data and retained-widget windows.
pub struct App;

impl ResourceApp for App {
    type Resource = Review;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn view(cx: &mut ViewCx<Review>) -> Element {
        crate::install_catalog();
        // Rows are built on demand as the table scrolls, long after this view
        // was; they bind their buttons through the resource's actions.
        let actions = cx.actions();
        let columns = vec![
            ColumnDef::<Row>::new("name", "data-name-column", ColumnWidth::Flex(1.0))
                .with_text(|row| row.name.clone()),
            ColumnDef::<Row>::new("actions", "data-actions-column", ColumnWidth::Fixed(120.0))
                .with_widget(move |row, _| {
                    Box::new(
                        actions.bind_invocation(
                            Button::new(&format!("open-{}", row.id), "data-open", None)
                                .fit_content(28.0),
                            Review::OPEN.with(row.id),
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
                    return ui::i18n::I18nManager::lookup_plain("data-none-opened");
                }
                let rows = review
                    .opened
                    .iter()
                    .map(|id| format!("Row {id}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                ui::i18n::I18nManager::format("data-recent", &[("rows", rows.as_str().into())])
            })
            .with_id("recently-opened");
        Box::new(ui::column![table, recent])
    }
}
