//! Virtual list: a hundred million rows, a few dozen widgets.
//!
//! A `VirtualList` asks its `ListModel` only for the rows in view, plus a
//! little overscan, and retains one widget subtree per visible row. The model
//! here stands in for a database or a service: it knows how many rows exist
//! and loads a window of them in the background when asked. Only what the
//! user did with the rows, which ones they opened, is resource state.
//!
//! Rows have stable keys and measured, variable heights. Each row is an
//! ordinary widget subtree, so its button is focusable and accessible on its
//! own, and pressing it does not select the row.

/// The application id.
pub const APP_ID: &str = "so.tilo.example.virtual-list";
/// The window and launcher title.
pub const APP_TITLE: &str = "Virtual List";

pub mod locales;

fn install_catalog() {
    locales::install();
}

use std::collections::HashMap;
use std::ops::Range;
use ui::i18n::I18nManager;
use ui::prelude::*;

/// How many rows the source reports. None is allocated until it is in view.
pub const ITEM_COUNT: usize = 100_000_000;
/// How many recently opened activities the feed keeps.
const RECENT: usize = 5;

/// One activity from the external source. Only the visible window is loaded.
pub struct Activity {
    id: u64,
}

/// The app's own state: what the user opened. Stored, synced and undoable.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Feed {
    /// Recently opened activity ids, newest first.
    pub opened: Vec<u64>,
}

#[resource(kind = "example.activity-feed", label = "Activity")]
impl Feed {
    /// Open an activity.
    #[action]
    pub fn open_activity(
        &mut self,
        #[param("Stable activity id")] id: u64,
    ) -> Result<(), &'static str> {
        if !usize::try_from(id).is_ok_and(|index| index < ITEM_COUNT) {
            return Err("no such activity");
        }
        self.opened.retain(|opened| *opened != id);
        self.opened.insert(0, id);
        self.opened.truncate(RECENT);
        Ok(())
    }
}

/// A sparse asynchronous model: it never allocates `ITEM_COUNT` rows, and
/// only the newest requested window may publish. Replace the worker in
/// `request_range` with a query against a real source.
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

    fn is_loaded(&self, index: usize) -> bool {
        self.cache.contains_key(&index)
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

        // A newer request makes this completion stale, so a slow answer for a
        // window the user scrolled past never overwrites a newer one.
        let completion = self.request.begin();
        // A worker where the target has one; the page's executor otherwise.
        let spawned = ui::background::Background::current().spawn("load-rows", move || {
            let rows = Ok(range
                .filter_map(|index| u64::try_from(index).ok())
                .map(|id| Activity { id })
                .collect());
            let _ = completion.complete(rows);
        });
        if spawned.is_err() {
            self.error = Some("Background work is not available here.".into());
        }
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

pub struct App;

impl ResourceApp for App {
    type Resource = Feed;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn view(cx: &mut ViewCx<Feed>) -> Element {
        install_catalog();
        let actions = cx.actions();
        let list = VirtualList::new_async(
            ActivityModel::new(ITEM_COUNT),
            48.0,
            move |model, index, key, state| {
                let height = if index % 5 == 0 { 72.0 } else { 44.0 };
                match state {
                    ListRowState::Ready if model.is_loaded(index) => {
                        let label = I18nManager::format("list-row", &[("id", (*key).into())]);
                        let open = actions.bind_invocation(
                            Button::new(&format!("open-{key}"), "list-open"),
                            Feed::OPEN_ACTIVITY.with(*key),
                        );
                        Box::new(Container::new().with_height(height).with_child(Box::new(
                            ui::row![ui::column![Text::new(&label)].with_no_padding(), open,],
                        )))
                    }
                    ListRowState::Ready | ListRowState::Loading => Box::new(
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
                    || I18nManager::lookup_plain("list-none-opened"),
                    |id| I18nManager::format("list-last-opened", &[("id", (*id).into())]),
                )
            })
            .with_id("last-opened");
        Box::new(ui::column![list, recent])
    }
}

ui::dev_reload_entry!(App);
