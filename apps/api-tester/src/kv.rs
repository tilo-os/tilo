//! The editable key/value tables of a request: query parameters, headers and
//! form fields.
//!
//! Each table is a [`DataTable`] with real controls in its cells: a checkbox
//! that includes or excludes the row, a field for the key, a field for the
//! value and a button that removes the row. The last row is always blank and
//! is where the next pair is typed.
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
use ui::widget::data_table::{
    ColumnDef, ColumnWidth, DataTable, TableModel, TableModelChange, TableRowState,
};
use ui::widget::{Button, ButtonRole, Checkbox, Element, TextInput, TextInputKind};

use crate::commands::{Cmd, RowEdit, Sink, Table};
use crate::model::KeyValue;

/// One row as the table shows it. The states are shared with the cells.
#[derive(Clone)]
pub(crate) struct KvRow {
    pub(crate) id: String,
    pub(crate) key: LocalState<String>,
    pub(crate) value: LocalState<String>,
    pub(crate) enabled: LocalState<bool>,
}

impl KvRow {
    pub(crate) fn of(row: &KeyValue) -> Self {
        Self {
            id: row.id.clone(),
            key: LocalState::new(row.key.clone()),
            value: LocalState::new(row.value.clone()),
            enabled: LocalState::new(row.enabled),
        }
    }

    /// The trailing row: empty, and unchecked until something is typed.
    pub(crate) fn blank(id: String) -> Self {
        Self {
            id,
            key: LocalState::new(String::new()),
            value: LocalState::new(String::new()),
            enabled: LocalState::new(false),
        }
    }
}

/// A change the app makes to a table's rows while it is on screen.
#[derive(Clone)]
pub(crate) enum KvChange {
    /// A blank row appended at the end.
    Append(String),
    /// The blank row became a pair: it is checked from now on.
    Activate(String),
    /// A row emptied (the blank row's remove control).
    Clear(String),
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

    /// How many changes the table has not taken yet.
    #[cfg(test)]
    pub(crate) fn pending(&self) -> usize {
        self.changes.borrow().len()
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
            match change {
                KvChange::Append(id) => {
                    self.rows.push(KvRow::blank(id));
                    out.push(TableModelChange::RowsInserted {
                        index: self.rows.len().saturating_sub(1),
                        count: 1,
                    });
                }
                KvChange::Activate(id) => {
                    if let Some(row) = self.rows.iter().find(|row| row.id == id) {
                        row.enabled.set(true);
                    }
                }
                KvChange::Clear(id) => {
                    if let Some(row) = self.rows.iter().find(|row| row.id == id) {
                        row.key.set(String::new());
                        row.value.set(String::new());
                    }
                }
                KvChange::Remove(id) => {
                    if let Some(index) = self.rows.iter().position(|row| row.id == id) {
                        self.rows.remove(index);
                        out.push(TableModelChange::RowsRemoved { index, count: 1 });
                    }
                }
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
    edit: fn(String) -> RowEdit,
) -> Element {
    let sink = sink.clone();
    let id = row.id.clone();
    let input = TextInput::from_state(state)
        .with_kind(TextInputKind::Code)
        .with_plain_chrome()
        .with_placeholder(name)
        .with_accessible_name(name)
        .on_change(move |text| {
            sink.borrow_mut().push(Cmd::Edit {
                table,
                row: id.clone(),
                edit: edit(text.to_owned()),
            });
        });
    Box::new(input)
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
    shown.push(KvRow::blank(blank_id.to_owned()));

    let enabled_sink = sink.clone();
    let key_sink = sink.clone();
    let value_sink = sink.clone();
    let remove_sink = sink.clone();
    let columns = vec![
        ColumnDef::new("enabled", "", ColumnWidth::Fixed(48.0)).with_widget(
            move |row: &KvRow, _| -> Element {
                let sink = enabled_sink.clone();
                let id = row.id.clone();
                Box::new(
                    Checkbox::from_state(&row.enabled)
                        .with_accessible_name("Use this row")
                        .on_change(move |enabled| {
                            sink.borrow_mut().push(Cmd::Edit {
                                table,
                                row: id.clone(),
                                edit: RowEdit::Enabled(enabled),
                            });
                        }),
                )
            },
        ),
        ColumnDef::new("key", "Key", ColumnWidth::Flex(1.0)).with_widget(
            move |row: &KvRow, _| -> Element {
                text_cell(&key_sink, table, row, &row.key, "Key", RowEdit::Key)
            },
        ),
        ColumnDef::new("value", "Value", ColumnWidth::Flex(2.0)).with_widget(
            move |row: &KvRow, _| -> Element {
                text_cell(&value_sink, table, row, &row.value, "Value", RowEdit::Value)
            },
        ),
        ColumnDef::new("remove", "", ColumnWidth::Fixed(48.0)).with_widget(
            move |row: &KvRow, _| -> Element {
                let sink = remove_sink.clone();
                let id = row.id.clone();
                Box::new(
                    Button::icon_only(&format!("api.kv-remove.{}", row.id), Icon::X, "Remove row")
                        .role(ButtonRole::Plain)
                        .on_effect(move || {
                            sink.borrow_mut().push(Cmd::Edit {
                                table,
                                row: id.clone(),
                                edit: RowEdit::Remove,
                            });
                        }),
                )
            },
        ),
    ];
    let model = KvModel {
        rows: shown,
        queue: queue.clone(),
    };
    let element = DataTable::from_model(columns, model).with_accessible_name(label);
    KvTable {
        element: Box::new(element),
        queue,
    }
}
