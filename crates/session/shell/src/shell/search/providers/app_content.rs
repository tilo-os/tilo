//! What apps hold, found whether or not they run.
//!
//! Answers from the session's persistent index of apps' search entries
//! ([`crate::shell::search::app_content_index`]): a note, a message, a
//! login, by its title, its secondary line and its short fields. A hit opens
//! the app at the entry, through the same door a live hit takes: the
//! entry's published action, or the app's `open` action at the entry's
//! resource, invoked on the running app or, when it is closed, on the
//! catalogue the broker retained for it, which launches it first. With the
//! app running, its live search rows for an indexed resource give way to
//! this provider's ([`AppContentProvider::covers`]), so one resource is one
//! row.
//!
//! Secret-bearing entries (Passwords' logins) are found only while their
//! app last said its secrets are unlocked. While they are locked, a query
//! one of them would match gets one row instead, "Unlock Passwords to
//! search for …", whose Enter opens the app to unlock it.
//!
//! Rows are "your content", the third intent tier: ranked as
//! [`ResourceKind::Record`] on the provider scale ([`MAX_SCORE`]).

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use tilo_actions::remote::{InvokeArgument, InvokeRequest, PendingPoll, RemoteActionTarget};
use tilo_search::{Document, Matching, Page, SearchIndex};
use ui::search::index_publication::IndexEntry;

use crate::shell::certified_action_session::ActionSessionSource;
use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::collab_broker::AppActionBroker;
use crate::shell::collab_broker_capnp::{
    PublishedAction, PublishedInput, PublishedSurface, launchable_provider,
};
use crate::shell::i18n::t_args;
use crate::shell::search::action_api::ActionError;
use crate::shell::search::app_content_index::{
    AppContentIndex, Contents, IndexedEntry, SlotKey, record_slot,
};
use crate::shell::search::{
    ResourceKind, SearchBudget, SearchContext, SearchCursor, SearchPage, SearchProvider,
    SearchProximity, SearchResult,
};

/// This provider's id.
pub const PROVIDER_ID: &str = "app-content";

/// Top of this provider's scale: a title named exactly. The registry
/// divides by it to compare providers.
pub const MAX_SCORE: i32 = ui::search::system::MAX_PROVIDER_SCORE;

/// The action every framework app publishes to show the item at an anchor
/// (`tilo_app::framework`'s `open`), and its parameter.
const OPEN_ACTION: &str = "open";
const OPEN_ANCHOR: &str = "anchor";

/// Separates the parts of a row id; an entry's key, which may hold it, is
/// last.
const ID_SEPARATOR: char = '\u{1f}';
/// The id prefix of an app's unlock row.
const UNLOCK: &str = "unlock";

/// The index at one generation, ready to search.
struct Snapshot {
    generation: u64,
    visible: Vec<IndexedEntry>,
    index: SearchIndex,
    /// Locked secret-bearing entries, matched only to offer unlocking.
    locked: Vec<IndexedEntry>,
    locked_index: SearchIndex,
    /// `(app, room, resource)` of every visible entry.
    covered: HashSet<(String, String, String)>,
}

impl Snapshot {
    fn new(generation: u64, contents: Contents) -> Self {
        let Contents { visible, locked } = contents;
        let covered = visible
            .iter()
            .map(|indexed| {
                (
                    indexed.slot.app.clone(),
                    indexed.slot.room.clone(),
                    indexed.entry.resource.clone(),
                )
            })
            .collect();
        Self {
            generation,
            index: text_index(&visible),
            locked_index: text_index(&locked),
            visible,
            locked,
            covered,
        }
    }
}

/// The entries' texts, each found by its position.
fn text_index(entries: &[IndexedEntry]) -> SearchIndex {
    // Names a person half remembers: words across fields and typos.
    let mut index = SearchIndex::new(Matching::LOOSE);
    index.replace_all(entries.iter().enumerate().map(|(at, indexed)| {
        indexed
            .entry
            .searchable()
            .fold(Document::new(at as u64), |document, (text, weight)| {
                document.field(text, weight.field_weight())
            })
    }));
    if let Err(error) = index.commit() {
        tracing::error!(%error, "could not index apps' content");
    }
    index
}

/// What opening a hit goes through: the broker's published surfaces, live
/// and retained, and the session's action door. Absent where no broker is
/// attached; a hit then opens the app itself.
#[derive(Clone)]
struct OpenDoor {
    broker: AppActionBroker,
    sessions: ActionSessionSource,
}

pub struct AppContentProvider {
    index: Arc<AppContentIndex>,
    door: Option<OpenDoor>,
    snapshot: RefCell<Option<Rc<Snapshot>>>,
    /// Whether the index wakes the compositor loop on a change yet.
    wakes: Cell<bool>,
}

impl AppContentProvider {
    /// A provider over `index` whose hits open through `broker`.
    pub fn new(index: Arc<AppContentIndex>, broker: AppActionBroker) -> Self {
        Self {
            index,
            door: Some(OpenDoor {
                sessions: ActionSessionSource::new(broker.clone()),
                broker,
            }),
            snapshot: RefCell::new(None),
            wakes: Cell::new(false),
        }
    }

    /// A provider over `index` with no broker: a hit opens its app.
    pub fn without_broker(index: Arc<AppContentIndex>) -> Self {
        Self {
            index,
            door: None,
            snapshot: RefCell::new(None),
            wakes: Cell::new(false),
        }
    }

    /// The snapshot of the index as it is now, built if it moved.
    fn current(&self) -> Rc<Snapshot> {
        let generation = self.index.generation();
        let mut slot = self.snapshot.borrow_mut();
        match slot.as_ref() {
            Some(snapshot) if snapshot.generation == generation => Rc::clone(snapshot),
            _ => {
                // Read the generation before the contents: a change landing
                // between them leaves this snapshot stale, never stamped
                // current.
                let snapshot = Rc::new(Snapshot::new(generation, self.index.contents()));
                *slot = Some(Rc::clone(&snapshot));
                snapshot
            }
        }
    }

    /// Whether this provider has a row for `resource` of `app` in `room`:
    /// the live provider's row for it gives way, so one resource is one
    /// row.
    pub fn covers(&self, app: &str, room: &str, resource: &str) -> bool {
        self.snapshot.borrow().as_ref().is_some_and(|snapshot| {
            snapshot
                .covered
                .contains(&(app.to_owned(), room.to_owned(), resource.to_owned()))
        })
    }

    fn row(&self, indexed: &IndexedEntry, score: i32) -> SearchResult {
        let IndexedEntry { slot, entry } = indexed;
        let subtitle = entry
            .subtitle
            .clone()
            .or_else(|| entry.group.clone())
            .unwrap_or_else(|| app_name(&slot.app));
        let door = self.door.clone();
        let opened_slot = slot.clone();
        let opened = entry.clone();
        SearchResult::nullary_fallible(
            ResourceKind::Record,
            PROVIDER_ID,
            entry_id(slot, &entry.key),
            entry.title.clone(),
            Some(subtitle),
            score,
            move |_doc, _root, chrome| open_entry(door.as_ref(), chrome, &opened_slot, &opened),
        )
    }

    /// One row offering to unlock `app`, for the `needle` its locked
    /// entries match.
    fn unlock_row(app: &str, needle: &str, score: i32) -> SearchResult {
        let name = app_name(app);
        let launched = app.to_owned();
        let launched_name = name.clone();
        SearchResult::nullary_fallible(
            ResourceKind::Record,
            PROVIDER_ID,
            format!("{UNLOCK}{ID_SEPARATOR}{app}"),
            t_args(
                "search-unlock-app",
                &[
                    ("app", name.as_str().into()),
                    ("query", needle.trim().into()),
                ],
            ),
            Some(t_args(
                "search-unlock-app-detail",
                &[("app", name.as_str().into())],
            )),
            score,
            move |_doc, _root, chrome| {
                super::applications::launch_installed_app(chrome, &launched, &launched_name)
            },
        )
    }

    /// One unlock row per app whose locked entries match `needle`, scored by
    /// its best match.
    fn unlock_rows(snapshot: &Snapshot, needle: &str) -> Vec<SearchResult> {
        let mut best: Vec<(String, i32)> = Vec::new();
        for hit in snapshot
            .locked_index
            .search(needle, Page::first(SearchBudget::PALETTE.max_work))
            .hits
        {
            let Some(indexed) = usize::try_from(hit.key)
                .ok()
                .and_then(|at| snapshot.locked.get(at))
            else {
                continue;
            };
            let score = crate::shell::search::provider_score(hit.rank.unwrap_or(0));
            match best.iter_mut().find(|(app, _)| *app == indexed.slot.app) {
                Some((_, held)) => *held = (*held).max(score),
                None => best.push((indexed.slot.app.clone(), score)),
            }
        }
        best.into_iter()
            .map(|(app, score)| Self::unlock_row(&app, needle, score.max(1)))
            .collect()
    }
}

/// A row's id: where its entry was published from, then its key.
fn entry_id(slot: &SlotKey, key: &str) -> String {
    format!(
        "{}{ID_SEPARATOR}{}{ID_SEPARATOR}{}{ID_SEPARATOR}{key}",
        slot.app, slot.room, slot.record
    )
}

/// The app an unlock row's id names.
fn unlock_app(id: &str) -> Option<&str> {
    id.strip_prefix(UNLOCK)?.strip_prefix(ID_SEPARATOR)
}

/// The slot and key a row id names.
fn parse_entry_id(id: &str) -> Option<(SlotKey, &str)> {
    let mut parts = id.splitn(4, ID_SEPARATOR);
    let app = parts.next()?;
    let room = parts.next()?;
    let record = parts.next()?;
    let key = parts.next()?;
    Some((
        SlotKey {
            app: app.to_owned(),
            room: room.to_owned(),
            record: record.to_owned(),
        },
        key,
    ))
}

/// The app's name as the person knows it.
fn app_name(app: &str) -> String {
    crate::shell::launcher::display_name_for_app_id_without_waiting(app)
        .unwrap_or_else(|| app.to_owned())
}

/// The published action that opens `entry` and what to invoke it with:
/// the entry's own action, else the app's `open` at the entry's resource.
/// The app's surfaces in the entry's room are asked, live ones first, the
/// window record the entry came from before others.
fn opening(
    surfaces: Vec<PublishedSurface>,
    slot: &SlotKey,
    entry: &IndexEntry,
) -> Option<(PublishedSurface, PublishedAction, InvokeArgument)> {
    let mut candidates: Vec<PublishedSurface> = surfaces
        .into_iter()
        .filter(|surface| surface.app_id == slot.app && surface.route.cluster_id == slot.room)
        .collect();
    candidates.sort_by_key(|surface| {
        (
            !surface.live,
            record_slot(&surface.route.record_id) != slot.record,
        )
    });
    if let Some(wanted) = &entry.action {
        let found = candidates.iter().find_map(|surface| {
            surface
                .actions
                .iter()
                .find(|action| {
                    action.resource == wanted.resource
                        && action.action == wanted.action
                        && action.params.is_empty()
                        && matches!(action.input, PublishedInput::None)
                })
                .map(|action| (surface.clone(), action.clone()))
        });
        if let Some((surface, action)) = found {
            return Some((surface, action, InvokeArgument::None));
        }
    }
    candidates.iter().find_map(|surface| {
        let action = surface.actions.iter().find(|action| {
            action.action == OPEN_ACTION
                && action
                    .params
                    .iter()
                    .any(|param| param.name.as_ref() == OPEN_ANCHOR)
        })?;
        let mut argument = serde_json::Map::new();
        argument.insert(
            OPEN_ANCHOR.into(),
            serde_json::Value::String(entry.resource.clone()),
        );
        Some((
            surface.clone(),
            action.clone(),
            InvokeArgument::Json(argument),
        ))
    })
}

/// Open the app at `entry`, as its person in the entry's room: the running
/// app takes the invocation; a closed one is launched by the broker first.
/// With nothing published that opens it, the app opens.
fn open_entry(
    door: Option<&OpenDoor>,
    chrome: &Chrome,
    slot: &SlotKey,
    entry: &IndexEntry,
) -> Result<(), ActionError> {
    let person = {
        let state = chrome.borrow();
        let room = state
            .tree()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == slot.room)
            .map(|workspace| workspace.room);
        room.map(|room| state.local_user_for_room(&room))
    };
    let found = door.zip(person).and_then(|(door, person)| {
        Some((door, person, opening(door.broker.surfaces(), slot, entry)?))
    });
    let Some((door, person, (surface, action, argument))) = found else {
        return super::applications::launch_installed_app(chrome, &slot.app, &app_name(&slot.app));
    };
    let provider = if surface.live {
        surface.peer_id.to_string()
    } else {
        launchable_provider(&surface)
    };
    let session = door
        .sessions
        .open(&slot.room, Some(&person))
        .map_err(ActionError::Refused)?;
    let mut admission = session.invoke_job(InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider,
            address: tilo_actions::ActionAddress::new(&action.resource, &action.action),
        },
        argument,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
    });
    match admission.poll_ready() {
        PendingPoll::Ready(result) => result
            .map(|(job, _)| tracing::debug!(job, "indexed app content opened"))
            .map_err(|error| ActionError::Refused(error.to_string())),
        PendingPoll::Waiting => {
            // A closed app launches first; the host delivers once it is up.
            // Keep listening so the delivery is not abandoned.
            admission.then(|answer| match answer {
                Some(Ok((job, _))) => tracing::debug!(job, "launched app opened indexed content"),
                Some(Err(error)) => {
                    tracing::warn!(%error, "opening indexed app content was refused");
                }
                None => {}
            });
            Ok(())
        }
        PendingPoll::Abandoned => Err(ActionError::Refused(
            "opening the app was not admitted".into(),
        )),
    }
}

impl SearchProvider for AppContentProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "App content"
    }

    fn display_name(&self) -> String {
        crate::shell::i18n::t("search-provider-app-content")
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Record]
    }

    fn max_score(&self) -> i32 {
        MAX_SCORE
    }

    fn row_icon(&self, result: &SearchResult) -> Option<ui::icons::Icon> {
        if unlock_app(&result.id).is_some() {
            return Some(ui::icons::Icon::Lock);
        }
        let (slot, key) = parse_entry_id(&result.id)?;
        let snapshot = self.snapshot.borrow();
        snapshot
            .as_ref()?
            .visible
            .iter()
            .find(|indexed| indexed.slot == slot && indexed.entry.key == key)?
            .entry
            .icon
            .as_deref()
            .and_then(ui::icons::Icon::from_name)
    }

    fn proximity(&self, result: &SearchResult, chrome: &ChromeState) -> SearchProximity {
        let Some((slot, _)) = parse_entry_id(&result.id) else {
            return SearchProximity::Global;
        };
        let here = chrome
            .tree()
            .desktop
            .active_workspace()
            .map(|workspace| workspace.room.to_string());
        if here.as_deref() == Some(slot.room.as_str()) {
            SearchProximity::CurrentWorkspace
        } else {
            SearchProximity::Global
        }
    }

    fn refresh_search_index(&self, _ctx: &SearchContext<'_>) {
        if !self.wakes.replace(true) {
            self.index
                .set_waker(crate::shell::search::invocation_wake::waker());
        }
        let _ = self.current();
    }

    fn search_revision(&self, _ctx: &SearchContext<'_>) -> String {
        self.index.generation().to_string()
    }

    fn resolve(&self, id: &str, ctx: SearchContext<'_>) -> Option<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Record) {
            return None;
        }
        let snapshot = self.current();
        if let Some(app) = unlock_app(id) {
            return snapshot
                .locked
                .iter()
                .any(|indexed| indexed.slot.app == app)
                .then(|| Self::unlock_row(app, ctx.needle, MAX_SCORE));
        }
        let (slot, key) = parse_entry_id(id)?;
        let indexed = snapshot
            .visible
            .iter()
            .find(|indexed| indexed.slot == slot && indexed.entry.key == key)?;
        Some(self.row(indexed, MAX_SCORE))
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        self.query_page(ctx, None, SearchBudget::PALETTE).results
    }

    fn query_page(
        &self,
        ctx: SearchContext<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage {
        let needle = ctx.needle.trim();
        if !ctx.wanted.contains(&ResourceKind::Record)
            || needle.is_empty()
            || budget.max_results == 0
            || budget.max_work == 0
        {
            return SearchPage::exhausted("empty", Vec::new());
        }
        let snapshot = self.current();
        let revision = snapshot.generation.to_string();
        // Two rooms' copies of one app's entry (a vault opened in each) are
        // one row.
        let mut shown: HashSet<(String, String)> = HashSet::new();
        let mut page = crate::shell::search::index_page(
            revision,
            cursor,
            needle,
            budget,
            |page| snapshot.index.search(needle, page),
            |hit| {
                let indexed = usize::try_from(hit.key)
                    .ok()
                    .and_then(|at| snapshot.visible.get(at))?;
                if !shown.insert((indexed.slot.app.clone(), indexed.entry.key.clone())) {
                    return None;
                }
                let score = crate::shell::search::provider_score(hit.rank.unwrap_or(0))
                    .saturating_add(indexed.entry.boost.clamp(0, 2))
                    .clamp(1, MAX_SCORE);
                Some(self.row(indexed, score))
            },
        );
        if cursor.is_none() {
            let room = budget.max_results.saturating_sub(page.results.len());
            page.results
                .extend(Self::unlock_rows(&snapshot, needle).into_iter().take(room));
        }
        page
    }
}

#[cfg(test)]
#[path = "app_content_tests.rs"]
mod tests;
