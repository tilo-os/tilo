use std::collections::HashMap;
use std::ops::Range;
use ui::prelude::*;

const ITEM_COUNT: usize = 100_000_000;
/// How many recently opened activities the feed keeps.
const RECENT: usize = 5;

/// One activity from the external feed. Only the visible window is loaded.
pub struct Activity {
    id: u64,
    label: String,
}

/// The app's own state: what the user opened. Stored, synced and undoable.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Feed {
    /// Recently opened activity ids, newest first.
    pub opened: Vec<u64>,
}

#[resource(kind = "generated.activity-list", label = "Activity")]
impl Feed {
    /// Open an activity.
    #[action]
    pub fn open(&mut self, #[param("Stable activity id")] id: u64) -> Result<(), &'static str> {
        if !usize::try_from(id).is_ok_and(|index| index < ITEM_COUNT) {
            return Err("no such activity");
        }
        self.opened.retain(|opened| *opened != id);
        self.opened.insert(0, id);
        self.opened.truncate(RECENT);
        Ok(())
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
pub struct App;

impl ResourceApp for App {
    type Resource = Feed;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn view(cx: &mut ViewCx<Feed>) -> Element {
        crate::install_catalog();
        let actions = cx.actions();
        let list = VirtualList::new_async(
            ActivityModel::new(ITEM_COUNT),
            48.0,
            move |model, index, key, state| {
                let height = if index % 5 == 0 { 72.0 } else { 44.0 };
                match state {
                    ListRowState::Ready => {
                        let label = model.item(index).map_or("", |item| item.label.as_str());
                        let open = actions.bind_invocation(
                            Button::new(&format!("open-{key}"), "list-open", None)
                                .fit_content(36.0),
                            Feed::OPEN.with(*key),
                        );
                        Box::new(Container::new().with_height(height).with_child(Box::new(
                            ui::row![ui::column![Text::new(label)].with_no_padding(), open,],
                        )))
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
        let recent = cx
            .text(|feed| {
                feed.opened.first().map_or_else(
                    || ui::i18n::I18nManager::lookup_plain("list-none-opened"),
                    |id| {
                        ui::i18n::I18nManager::format(
                            "list-last-opened",
                            &[("activity", format!("Activity {id}").as_str().into())],
                        )
                    },
                )
            })
            .with_id("last-opened");
        Box::new(ui::column![list, recent])
    }
}
