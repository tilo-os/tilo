//! The editable key/value tables of a request: query parameters, headers and
//! form fields.
//!
//! Each table is a [`DataTable`] with real controls in its cells: a checkbox
//! that includes or excludes the row, a field for the key, a field for the
//! value and a button that removes the row. The last row is always blank and
//! is where the next pair is typed; it holds only the two fields, and gains
//! the checkbox and the remove button once it becomes a pair.
//!
//! Every cell is bound to process-local state held by its row, so a cell never
//! has to be rebuilt for an edit and keeps its caret while the person types.
//! The table learns of structural changes (a row appended, a row removed)
//! through a queue its model drains.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use ui::LocalState;
use ui::icons::Icon;
use ui::tokens::{Metrics, Spacing, SpacingStep};
use ui::widget::data_table::{
    ColumnDef, ColumnWidth, DataTable, TableModel, TableModelChange, TableRowState,
};
use ui::widget::{Button, ButtonRole, Checkbox, Column, Element, Spacer, TextInput, TextInputKind};

use crate::commands::{Cmd, RowEdit, Sink, Table};
use crate::model::KeyValue;
use crate::record::Row;

/// One row as the table shows it. The states are shared with the cells.
#[derive(Clone)]
pub(crate) struct KvRow {
    pub(crate) id: String,
    pub(crate) key: LocalState<String>,
    pub(crate) value: LocalState<String>,
    pub(crate) enabled: LocalState<bool>,
    /// The row is the blank one: nothing typed yet, so no checkbox and no
    /// remove button.
    pending: LocalState<bool>,
    /// Where the row is in its table: the blank row's first text is
    /// inserted there.
    at: usize,
}

impl KvRow {
    pub(crate) fn of(row: &KeyValue) -> Self {
        Self {
            id: row.id.clone(),
            key: LocalState::new(row.key.clone()),
            value: LocalState::new(row.value.clone()),
            enabled: LocalState::new(row.enabled),
            pending: LocalState::new(false),
            at: 0,
        }
    }

    /// The trailing row at `at`: empty, and without controls until something
    /// is typed.
    pub(crate) fn blank(id: String, at: usize) -> Self {
        Self {
            id,
            key: LocalState::new(String::new()),
            value: LocalState::new(String::new()),
            enabled: LocalState::new(false),
            pending: LocalState::new(true),
            at,
        }
    }
}

/// A change the app makes to a table's rows while it is on screen.
#[derive(Clone)]
pub(crate) enum KvChange {
    /// A row removed.
    Remove(String),
}

/// The queue the app pushes [`KvChange`]s onto and the table's model drains.
#[derive(Clone, Default)]
pub(crate) struct KvQueue {
    changes: Rc<RefCell<VecDeque<KvChange>>>,
}

impl KvQueue {
    pub(crate) fn push(&self, change: KvChange) {
        self.changes.borrow_mut().push_back(change);
    }
}

fn identity(id: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

struct KvModel {
    rows: Vec<KvRow>,
    queue: KvQueue,
}

impl TableModel for KvModel {
    type Row = KvRow;

    fn len(&self) -> usize {
        self.rows.len()
    }

    fn key_at(&self, index: usize) -> Option<u64> {
        self.rows.get(index).map(|row| identity(&row.id))
    }

    fn index_of(&self, key: u64) -> Option<usize> {
        self.rows.iter().position(|row| identity(&row.id) == key)
    }

    fn row_state(&self, index: usize) -> TableRowState<'_, KvRow> {
        self.rows
            .get(index)
            .map_or(TableRowState::Loading, TableRowState::Ready)
    }

    fn poll_changes(&mut self) -> Vec<TableModelChange> {
        let drained: Vec<KvChange> = self.queue.changes.borrow_mut().drain(..).collect();
        let mut out = Vec::new();
        for change in drained {
            let KvChange::Remove(id) = change;
            if let Some(index) = self.rows.iter().position(|row| row.id == id) {
                self.rows.remove(index);
                out.push(TableModelChange::RowsRemoved { index, count: 1 });
            }
        }
        out
    }

    fn needs_poll(&self) -> bool {
        !self.queue.changes.borrow().is_empty()
    }
}

fn text_cell(
    sink: &Sink,
    table: Table,
    row: &KvRow,
    state: &LocalState<String>,
    name: &str,
    field: &'static str,
) -> Element {
    let id = ui::tilo_resource::Id::from_raw(row.id.clone());
    // The blank row under the pairs invites a new one; repeating the
    // column headers there read as a second header row.
    let placeholder = if row.pending.get() {
        format!("Add {}", name.to_lowercase())
    } else {
        name.to_owned()
    };
    let input = TextInput::from_state(state)
        .with_id(&format!("api.kv.{table:?}.{id}.{field}"))
        .with_kind(TextInputKind::Code)
        .with_plain_chrome()
        .with_placeholder(&placeholder)
        .with_accessible_name(name);
    let rows = table.rows();
    let item = rows.item(&id);
    let path = if field == "key" { item.key } else { item.value };
    let input = if row.pending.get() {
        // The blank row becomes a pair with its first text.
        let at = row.at;
        sink.bind_field(input, path, move |_, text| {
            let row = if field == "key" {
                Row::new(text, "")
            } else {
                Row::new("", text)
            };
            rows.insert_as(&id, at, &row)
        })
    } else {
        sink.bind_field(input, path, ui::tilo_resource::Path::replace)
    };
    // The plain field draws its text at its own edge; the inset lines the
    // text up under the column's header.
    Box::new(
        Column::new()
            .with_horizontal_padding(Spacing::BASE.s2, Spacing::BASE.s2)
            .with_vertical_padding(0.0)
            .with_intrinsic_height()
            .push(Box::new(input)),
    )
}

/// A control that sits at the top of a field-high row, moved down to the
/// field's middle.
fn centred(control: Element) -> Element {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_extra_top_padding(Spacing::BASE.s1)
            .with_intrinsic_height()
            .push(control),
    )
}

/// The trailing blank has no checkbox or removal control. Canonical state
/// presentation rebuilds it as a durable pair after its first text edit.
fn gated(row: &KvRow, control: impl Fn() -> Element + 'static) -> Element {
    if row.pending.get() {
        Box::new(Spacer::token(SpacingStep::S0))
    } else {
        centred(control())
    }
}

/// A built table and the queue its rows change through.
pub(crate) struct KvTable {
    pub(crate) element: Element,
    pub(crate) queue: KvQueue,
}

/// A table of `rows` followed by the blank row `blank_id`.
pub(crate) fn build(
    table: Table,
    label: &str,
    rows: &[KeyValue],
    blank_id: &str,
    sink: &Sink,
) -> KvTable {
    let queue = KvQueue::default();
    let mut shown: Vec<KvRow> = rows.iter().map(KvRow::of).collect();
    shown.push(KvRow::blank(blank_id.to_owned(), rows.len()));

    let enabled_sink = sink.clone();
    let key_sink = sink.clone();
    let value_sink = sink.clone();
    let remove_sink = sink.clone();
    // The two control columns are as wide as a control's pointer target.
    let control = Metrics::BASE.min_pointer_target;
    let columns = vec![
        ColumnDef::new("enabled", "", ColumnWidth::Fixed(control)).with_widget(
            move |row: &KvRow, _| -> Element {
                let sink = enabled_sink.clone();
                let id = ui::tilo_resource::Id::from_raw(row.id.clone());
                let state = row.enabled.clone();
                gated(row, move || -> Element {
                    let control = Checkbox::from_state(&state).with_accessible_name("Use this row");
                    Box::new(sink.bind_field(
                        control,
                        table.rows().item(&id).enabled,
                        ui::tilo_resource::Path::set,
                    ))
                })
            },
        ),
        ColumnDef::new("key", "Key", ColumnWidth::Flex(1.0)).with_widget(
            move |row: &KvRow, _| -> Element {
                text_cell(&key_sink, table, row, &row.key, "Key", "key")
            },
        ),
        ColumnDef::new("value", "Value", ColumnWidth::Flex(2.0)).with_widget(
            move |row: &KvRow, _| -> Element {
                text_cell(&value_sink, table, row, &row.value, "Value", "value")
            },
        ),
        ColumnDef::new("remove", "", ColumnWidth::Fixed(control)).with_widget(
            move |row: &KvRow, _| -> Element {
                let sink = remove_sink.clone();
                let id = row.id.clone();
                gated(row, move || -> Element {
                    let sink = sink.clone();
                    let row = id.clone();
                    Box::new(
                        Button::icon_only(&format!("api.kv-remove.{id}"), Icon::X, "Remove row")
                            .role(ButtonRole::Plain)
                            .on_effect(move || {
                                sink.borrow_mut().push(Cmd::Edit {
                                    table,
                                    row: row.clone(),
                                    edit: RowEdit::Remove,
                                });
                            }),
                    )
                })
            },
        ),
    ];
    let model = KvModel {
        rows: shown,
        queue: queue.clone(),
    };
    let table = DataTable::from_model(columns, model).with_accessible_name(label);
    // A pair reads across: on a wide window the value would sit a screen's
    // width from its key, so the table keeps to the readable measure.
    let element = ui::widget::Container::new()
        .with_no_padding()
        .readable()
        .with_child(Box::new(table));
    KvTable {
        element: Box::new(element),
        queue,
    }
}
