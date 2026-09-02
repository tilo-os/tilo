use std::collections::HashMap;
use std::ops::Range;
use ui::LatestRequest;
use ui::qos_actions;
use ui::widget::data_table::{
    ColumnDef, ColumnWidth, DataTable, TableModel, TableModelChange, TableRowState,
};
use ui::widget::{Button, Widget};

const ROW_COUNT: usize = 100_000_000;

#[derive(Clone)]
pub struct Row {
    pub id: u64,
    pub name: String,
}

fn row_resource() -> qos_actions::Resource {
    let mut resource = ui::widget::resource_actions::typed_resource(
        "generated.table",
        "Results",
        "open",
        "Open row",
        vec![qos_actions::ActionParam::required_owned(
            "id",
            qos_actions::ParamType::Integer,
            "Stable row id",
        )],
    );
    if let Some(action) = resource.actions.first_mut() {
        action.contract.result = qos_actions::ResultSpec::Resource {
            resource_kind: "generated.table.row".into(),
        };
    }
    resource
}

fn open_row(id: u64) -> qos_actions::ActionOutput {
    qos_actions::ActionOutput {
        value: None,
        resources: vec![format!("generated.table.row:{id}")],
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
#[expect(
    clippy::expect_used,
    reason = "the binding uses the sole action constructed immediately above"
)]
pub fn app_root() -> Box<dyn Widget> {
    crate::install_catalog();
    let columns = vec![
        ColumnDef::<Row>::new("name", "data-name-column", ColumnWidth::Flex(1.0))
            .with_text(|row| row.name.clone()),
        ColumnDef::<Row>::new("actions", "data-actions-column", ColumnWidth::Fixed(120.0))
            .with_widget(|row, _| {
                let id = row.id;
                let resource = row_resource();
                let binding = qos_actions::ActionBinding::static_json_for(
                    &resource,
                    "open",
                    serde_json::Map::from_iter([("id".into(), serde_json::json!(id))]),
                )
                .expect("generated row arguments match the exported action");
                Box::new(
                    Button::new(&format!("open-{}", row.id), "data-open", None)
                        .on_action(binding)
                        .fit_content(28.0),
                )
            }),
    ];
    let mut table = DataTable::from_model(columns, Rows::new())
        .with_id("rows-table")
        .with_accessible_name("data-accessible-name")
        .selectable(true);
    table.set_viewport(720.0, 32.0);
    ui::widget::resource_actions::immediate(Box::new(table), row_resource(), |payload| {
        let args: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        let id = args
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .ok_or("missing id")?;
        Ok(open_row(id))
    })
}
