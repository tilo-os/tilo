use std::collections::HashMap;
use std::ops::Range;
use ui::LatestRequest;
use ui::tilo_actions;
use ui::widget::virtual_list::{ListModel, ListModelChange, ListRowState, VirtualList};
use ui::widget::{Button, Container, Text, Widget};

const ITEM_COUNT: usize = 100_000_000;

pub struct Activity {
    id: u64,
    label: String,
}

fn activity_resource() -> tilo_actions::Resource {
    let mut resource = ui::widget::resource_actions::typed_resource(
        "generated.activity-list",
        "Activity",
        "open",
        "Open activity",
        vec![tilo_actions::ActionParam::required_owned(
            "id",
            tilo_actions::ParamType::Integer,
            "Stable activity id",
        )],
    );
    if let Some(action) = resource.actions.first_mut() {
        action.contract.result = tilo_actions::ResultSpec::Resource {
            resource_kind: "generated.activity".into(),
        };
    }
    resource
}

fn open_activity(id: u64) -> tilo_actions::ActionOutput {
    tilo_actions::ActionOutput {
        value: None,
        resources: vec![format!("generated.activity:{id}")],
    }
}

/// A sparse asynchronous model: the generated app never allocates
/// `ITEM_COUNT` rows and only the newest virtual window may publish.
pub struct ActivityModel {
    count: usize,
    cache: HashMap<usize, Activity>,
    requested: Range<usize>,
    error: Option<String>,
    request: LatestRequest<Result<Vec<Activity>, String>>,
}

impl ActivityModel {
    fn new(count: usize) -> Self {
        Self {
            count,
            cache: HashMap::new(),
            requested: 0..0,
            error: None,
            request: LatestRequest::new(),
        }
    }

    fn item(&self, index: usize) -> Option<&Activity> {
        self.cache.get(&index)
    }
}

impl ListModel for ActivityModel {
    type Key = u64;

    fn len(&self) -> usize {
        self.count
    }

    fn key_at(&self, index: usize) -> Option<Self::Key> {
        (index < self.count)
            .then_some(index)
            .and_then(|index| u64::try_from(index).ok())
    }

    fn index_of(&self, key: &Self::Key) -> Option<usize> {
        usize::try_from(*key)
            .ok()
            .filter(|index| *index < self.count)
    }

    fn row_state(&self, index: usize) -> ListRowState<'_> {
        if self.cache.contains_key(&index) {
            ListRowState::Ready
        } else if self.requested.contains(&index)
            && let Some(error) = self.error.as_deref()
        {
            ListRowState::Error(error)
        } else {
            ListRowState::Loading
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
                .map(|index| Activity {
                    id: u64::try_from(index).unwrap_or(u64::MAX),
                    label: format!("Activity {index}"),
                })
                .collect());
            let _ = completion.complete(rows);
        });
    }

    fn poll_changes(&mut self) -> Vec<ListModelChange<Self::Key>> {
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
            .then_some(ListModelChange::RowsChanged { keys: changed })
            .into_iter()
            .collect()
    }
}

/// A hundred-million-item list with a bounded retained widget window.
///
/// Rows have stable keys and measured variable heights. Each delegate is an
/// ordinary widget subtree, so its button remains independently focusable and
/// accessible without turning a button press into a row-selection gesture.
///
/// # Panics
///
/// Panics if a generated row binding no longer matches the resource action
/// declared by this template.
#[expect(
    clippy::expect_used,
    reason = "the binding uses the sole action constructed immediately above"
)]
pub fn app_root() -> Box<dyn Widget> {
    crate::install_catalog();
    let list = VirtualList::new_async(
        ActivityModel::new(ITEM_COUNT),
        48.0,
        |model, index, key, state| {
            let height = if index % 5 == 0 { 72.0 } else { 44.0 };
            match state {
                ListRowState::Ready => {
                    let label = model.item(index).map_or("", |item| item.label.as_str());
                    let open_key = *key;
                    let resource = activity_resource();
                    let binding = tilo_actions::ActionBinding::static_json_for(
                        &resource,
                        "open",
                        serde_json::Map::from_iter([("id".into(), serde_json::json!(open_key))]),
                    )
                    .expect("generated activity id matches the exported action");
                    Box::new(
                        Container::new()
                            .with_height(height)
                            .with_child(Box::new(ui::row![
                                ui::column![Text::new(label)].with_no_padding(),
                                Button::new(&format!("open-{key}"), "list-open", None)
                                    .on_action(binding)
                                    .fit_content(36.0),
                            ])),
                    )
                }
                ListRowState::Loading => Box::new(
                    Container::new()
                        .with_height(height)
                        .with_child(Box::new(Text::new("list-loading"))),
                ),
                ListRowState::Error(message) => Box::new(
                    Container::new()
                        .with_height(height)
                        .with_child(Box::new(Text::new(message))),
                ),
            }
        },
    )
    .with_id("activity-list")
    .with_accessible_name("list-accessible-name")
    .with_semantic_key(u64::to_string)
    .with_viewport_height(420.0)
    .multi_select(true);
    ui::widget::resource_actions::immediate(Box::new(list), activity_resource(), |payload| {
        let args: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        let id = args
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .ok_or("missing id")?;
        Ok(open_activity(id))
    })
}
