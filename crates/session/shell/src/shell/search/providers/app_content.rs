//! What apps hold, found whether or not they run.
//!
//! Answers from the session's persistent index of apps' search entries
//! ([`tilo_desktop_search::content_index`]): a note, a message, a
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
//! [`ResourceKind::Record`] on the provider scale ([`MAX_SCORE`]). They are
//! desktop search's ([`IndexedContent`]), the same every desktop finds;
//! this provider opens them through the broker.

use std::cell::Cell;
use std::collections::HashSet;
use std::sync::Arc;

use tilo_actions::remote::{InvokeArgument, InvokeRequest, PendingPoll, RemoteActionTarget};
use ui::search::index_publication::IndexEntry;

use crate::shell::certified_action_session::ActionSessionSource;
use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::collab_app_broker::{
    PublishedAction, PublishedInput, PublishedSurface, launchable_provider,
};
use crate::shell::collab_broker::AppActionBroker;
use crate::shell::search::action_api::ActionError;
use crate::shell::search::{
    ResourceKind, SearchBudget, SearchContext, SearchCursor, SearchPage, SearchProvider,
    SearchProximity, SearchResult,
};
use tilo_desktop_search::content_index::{AppContentIndex, SlotKey, record_slot};
use tilo_desktop_search::indexed::{
    IndexedContent, OPEN_ACTION, OPEN_ANCHOR, parse_entry_id, unlock_app,
};

/// This provider's id.
pub const PROVIDER_ID: &str = "app-content";

/// Top of this provider's scale: a title named exactly. The registry
/// divides by it to compare providers.
pub const MAX_SCORE: i32 = ui::search::system::MAX_PROVIDER_SCORE;

/// What opening a hit goes through: the broker's published surfaces, live
/// and retained, and the session's action door. Absent where no broker is
/// attached; a hit then opens the app itself.
#[derive(Clone)]
pub(crate) struct OpenDoor {
    broker: AppActionBroker,
    sessions: ActionSessionSource,
}

impl OpenDoor {
    pub(crate) fn new(broker: AppActionBroker) -> Self {
        Self {
            sessions: ActionSessionSource::new(broker.clone()),
            broker,
        }
    }

    /// A value that changes whenever the windows publishing actions do.
    pub(crate) fn surfaces_generation(&self) -> u64 {
        self.broker.surfaces_generation()
    }
}

/// Open `app` in the workspace of `room` at `anchor`, through its `open`
/// as a search hit at that anchor opens: the running app takes it, a closed
/// one is launched first; with nothing published that opens it, the app
/// opens.
pub(crate) fn open_at(
    door: Option<&OpenDoor>,
    chrome: &Chrome,
    app: &str,
    room: &str,
    anchor: &str,
) -> Result<(), ActionError> {
    let slot = SlotKey {
        app: app.to_owned(),
        room: room.to_owned(),
        record: String::new(),
    };
    let entry = IndexEntry {
        key: anchor.to_owned(),
        resource: anchor.to_owned(),
        action: None,
        title: String::new(),
        subtitle: None,
        group: None,
        icon: None,
        boost: 0,
        fields: Vec::new(),
        secret: false,
        references: Vec::new(),
    };
    open_entry(door, chrome, &slot, &entry)
}

pub struct AppContentProvider {
    content: IndexedContent,
    door: Option<OpenDoor>,
    /// Whether the index wakes the compositor loop on a change yet.
    wakes: Cell<bool>,
}

impl AppContentProvider {
    /// A provider over `index` whose hits open through `broker`.
    pub fn new(index: Arc<AppContentIndex>, broker: AppActionBroker) -> Self {
        Self {
            content: IndexedContent::new(index, app_name),
            door: Some(OpenDoor::new(broker)),
            wakes: Cell::new(false),
        }
    }

    /// A provider over `index` with no broker: a hit opens its app.
    pub fn without_broker(index: Arc<AppContentIndex>) -> Self {
        Self {
            content: IndexedContent::new(index, app_name),
            door: None,
            wakes: Cell::new(false),
        }
    }

    /// Whether this provider finds `resource` of `app` in `room`
    /// ([`IndexedContent::covers`]).
    pub fn covers(&self, app: &str, room: &str, resource: &str) -> bool {
        self.content.covers(app, room, resource)
    }

    /// A desktop-search row as the palette's: an entry opens through the
    /// broker, an unlock row opens its app.
    fn row(&self, hit: tilo_desktop_search::Hit) -> SearchResult {
        let door = self.door.clone();
        let opened = self.content.entry(&hit.id);
        let unlocked = unlock_app(&hit.id).map(str::to_owned);
        SearchResult::nullary_fallible(
            hit.kind,
            PROVIDER_ID,
            hit.id,
            hit.title,
            hit.subtitle,
            hit.score,
            move |_doc, _root, chrome| match (&opened, &unlocked) {
                (Some(opened), _) => open_entry(door.as_ref(), chrome, &opened.slot, &opened.entry),
                (None, Some(app)) => {
                    super::applications::launch_installed_app(chrome, app, &app_name(app))
                }
                (None, None) => Err(ActionError::Refused("that entry is gone".into())),
            },
        )
    }
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
#[allow(
    clippy::significant_drop_tightening,
    reason = "the action session stays open while its admission is polled"
)]
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
            .find(|workspace| workspace.room().to_string() == slot.room)
            .map(crate::shell::state::Workspace::room);
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
    invoke_opening(door, &slot.room, &person, provider, &action, argument)
}

/// Open the window of `app` in the workspace of `room` that the shell
/// launched for window instance `instance` at `anchor`, through that
/// window's own `open`, as a search hit at that anchor opens it: the one
/// window, not any of the app's. `Ok(false)` while that window has not
/// published an `open` yet; nothing is asked then.
pub(crate) fn open_in_window(
    door: &OpenDoor,
    chrome: &Chrome,
    (app, room): (&str, &str),
    instance: u64,
    anchor: &str,
) -> Result<bool, ActionError> {
    let Some(peer) = door.broker.window_peer(instance) else {
        return Ok(false);
    };
    let person = {
        let state = chrome.borrow();
        let room = state
            .tree()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room().to_string() == room)
            .map(crate::shell::state::Workspace::room);
        room.map(|room| state.local_user_for_room(&room))
    };
    let Some(person) = person else {
        return Err(ActionError::Refused("that workspace is not here".into()));
    };
    let slot = SlotKey {
        app: app.to_owned(),
        room: room.to_owned(),
        record: String::new(),
    };
    let entry = IndexEntry {
        key: anchor.to_owned(),
        resource: anchor.to_owned(),
        action: None,
        title: String::new(),
        subtitle: None,
        group: None,
        icon: None,
        boost: 0,
        fields: Vec::new(),
        secret: false,
        references: Vec::new(),
    };
    let window: Vec<PublishedSurface> = door
        .broker
        .surfaces()
        .into_iter()
        .filter(|surface| surface.live && surface.peer_id == peer)
        .collect();
    let Some((surface, action, argument)) = opening(window, &slot, &entry) else {
        return Ok(false);
    };
    invoke_opening(
        door,
        room,
        &person,
        surface.peer_id.to_string(),
        &action,
        argument,
    )
    .map(|()| true)
}

/// Invoke `action` with `argument` on `provider` in the workspace of
/// `room`, as `person`, through the session's action door: an opening, so
/// it is admitted now or, for a closed app, once the broker launched it.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the action session stays open while its admission is polled"
)]
fn invoke_opening(
    door: &OpenDoor,
    room: &str,
    person: &str,
    provider: String,
    action: &PublishedAction,
    argument: InvokeArgument,
) -> Result<(), ActionError> {
    let session = door
        .sessions
        .open(room, Some(person))
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
        self.content
            .entry(&result.id)?
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
            .map(|workspace| workspace.room().to_string());
        if here.as_deref() == Some(slot.room.as_str()) {
            SearchProximity::CurrentWorkspace
        } else {
            SearchProximity::Global
        }
    }

    /// Continue: what the person opened from here lately, newest first,
    /// each opening where it is through its app's `open` at its anchor, as
    /// a search hit does. Scoped to a workspace, only what lives there.
    fn refresh_search_index(&self, _ctx: &SearchContext<'_>) {
        if !self.wakes.replace(true) {
            self.content
                .index()
                .set_waker(crate::shell::search::invocation_wake::waker());
        }
        let _ = self.content.current();
    }

    fn search_revision(&self, _ctx: &SearchContext<'_>) -> String {
        self.content.index().generation().to_string()
    }

    fn resolve(&self, id: &str, ctx: SearchContext<'_>) -> Option<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Record) {
            return None;
        }
        self.content
            .resolve(id, ctx.needle)
            .map(|hit| self.row(hit))
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        self.query_page(ctx, None, SearchBudget::PALETTE).results
    }

    /// An app ranks its own rows as its own search does: how well each
    /// matched plus the boost it gave it. Tube's row that searches
    /// `YouTube` leads the videos it suggests, which follow in `YouTube`'s
    /// order. The boost stays out of the row's score, so no app lifts its
    /// rows over other providers'.
    fn source_order(&self, result: &SearchResult) -> Option<(String, i64)> {
        let (slot, _) = parse_entry_id(&result.id)?;
        let boost = self.content.boost(&result.id);
        Some((
            slot.app,
            i64::from(result.score).saturating_add(i64::from(boost)),
        ))
    }

    fn pages_itself(&self) -> bool {
        true
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
        let snapshot = self.content.current();
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
                // How well it matched; its app's boost orders it among the
                // app's own rows only ([`SearchProvider::source_order`]).
                let score =
                    crate::shell::search::provider_score(hit.rank.unwrap_or(0)).clamp(1, MAX_SCORE);
                Some(self.row(self.content.row(indexed, score)))
            },
        );
        if cursor.is_none() {
            let room = budget.max_results.saturating_sub(page.results.len());
            page.results.extend(
                self.content
                    .unlock_rows(needle)
                    .into_iter()
                    .take(room)
                    .map(|hit| self.row(hit)),
            );
        }
        page
    }
}

#[cfg(test)]
#[path = "app_content_tests.rs"]
mod tests;
