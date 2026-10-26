//! The room's tilolang record as a framework resource.
//!
//! A tilolang app keeps its resources in the room record, under `tl/<app>/<id>`, and changes them
//! with [`DynAction`]s: the edits a wasm action planned, replayable by any peer without the
//! module. [`Room`] is that record as a `tilo_resource::Resource`, so the framework's root hosts
//! it like every app's resource: persistence and sync, attributed history, per-author undo,
//! permissions, the palette and agent door, and search.
//!
//! What the record holds is read by the runtime, not by this type: its state is only the
//! record's heads, which tell the framework that something changed. What the resource publishes
//! (every record and item with its actions) and what search finds come from the live window's
//! runtime, which registers itself here ([`Live`]), and are kept up to date change by change:
//! a keystroke in one note republishes and re-indexes that note, not the room. A past version
//! previewed in History builds its own runtime over the old record and publishes nothing.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use tilolang_tilo::action::{DynAction, EDIT_TEXT_ACTION};
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::Host;
use tilolang_tilo::publish::{
    Caller, Committer, Planner, Publications, host_resource, publish_package,
};
use tilolang_tilo::search::{Hit, SearchIndex};
use ui::tilo_actions::{ActionInvoker, ActorKind};
use ui::tilo_resource::{Doc, ModelError, Resource, SearchEntry, SearchWeight};

/// The resource a tilolang app's window shows: the room record of its packages.
pub struct Room;

/// What the framework keeps of the record: its heads, so a change is a new state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoomState {
    heads: Vec<ui::automerge::ChangeHash>,
}

/// What a live window publishes of the room, kept up to date with the record change by change:
/// the records with their items and actions, and what search finds.
struct Published {
    hold: u64,
    host: Host,
    planner: Arc<dyn Planner>,
    committer: Arc<dyn Committer>,
    publications: Publications,
    search: SearchIndex,
    /// What search finds, by the published id of the resource each entry opens.
    entries: BTreeMap<String, SearchEntry>,
}

impl Published {
    /// Bring what is published up to date with the record, waiting for records still being
    /// read off the frame when `wait`.
    fn update(&mut self, wait: bool) {
        if wait {
            self.publications.settle(&self.host, &self.planner);
        } else {
            self.publications.update(&self.host, &self.planner);
        }
        let delta = if wait {
            self.search.settle(&self.host)
        } else {
            self.search.update(&self.host)
        };
        for gone in delta.removed {
            self.entries.remove(&gone);
        }
        for hit in delta.upserted {
            self.entries.insert(hit.resource.clone(), entry(&hit));
        }
    }

    /// Every resource the room's packages hold, each with its actions; the host's own resource;
    /// and each package's actions that take no record.
    fn resources(&self) -> Vec<ui::tilo_actions::Resource> {
        let mut out = vec![host_resource(&self.committer)];
        out.extend(self.publications.trees().values().cloned());
        for package in self.host.packages() {
            let resource = publish_package(&package.meta, &self.planner);
            if !resource.actions.is_empty() {
                out.push(resource);
            }
        }
        out
    }
}

thread_local! {
    /// What the live windows on this thread publish, each under its hold, the latest last: the
    /// record is published from the latest.
    static LIVE: RefCell<Vec<Published>> = const { RefCell::new(Vec::new()) };
    static NEXT_HOLD: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The live window's hold on publication: while it lives, the room's records are published
/// from its runtime, unless a window registered after it still lives. Dropping a hold takes
/// back only its own registration, so a window the framework built and dropped again never
/// leaves the one that stays unpublished.
pub struct Live(u64);

impl Live {
    /// Publish from `host` until the returned hold is dropped. The records are read off the
    /// frame; `wake` is called when they are ready for [`Self::update`] to take in.
    #[must_use]
    pub fn register(host: &Host, wake: Waker) -> Self {
        let hold = NEXT_HOLD.with(|next| {
            let hold = next.get();
            next.set(hold.wrapping_add(1));
            hold
        });
        let (planner, committer) = door_handles(host);
        let mut publications = Publications::new(host);
        publications.set_waker(Arc::clone(&wake));
        let mut search = SearchIndex::new(host);
        search.set_waker(wake);
        let mut published = Published {
            hold,
            host: host.clone(),
            planner,
            committer,
            publications,
            search,
            entries: BTreeMap::new(),
        };
        published.update(false);
        LIVE.with(|live| live.borrow_mut().push(published));
        Self(hold)
    }

    /// Bring what this window publishes up to date with the record: what changed since, and
    /// the records read off the frame when they are ready.
    pub fn update(&self) {
        let hold = self.0;
        LIVE.with(|live| {
            if let Some(published) = live
                .borrow_mut()
                .iter_mut()
                .find(|published| published.hold == hold)
            {
                published.update(false);
            }
        });
    }
}

impl Live {
    /// Whether records are still being read off the frame: the update after they are ready
    /// takes them in.
    #[must_use]
    pub fn pending(&self) -> bool {
        let hold = self.0;
        LIVE.with(|live| {
            live.borrow().iter().any(|published| {
                published.hold == hold
                    && (published.publications.is_building() || published.search.is_reading())
            })
        })
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let hold = self.0;
        let _ = LIVE.try_with(|live| live.borrow_mut().retain(|published| published.hold != hold));
    }
}

/// `read` of what the latest live window publishes, brought up to date with the record first.
fn with_live<T>(read: impl FnOnce(&Published) -> T) -> Option<T> {
    LIVE.with(|live| {
        let mut live = live.borrow_mut();
        let published = live.last_mut()?;
        published.update(true);
        Some(read(published))
    })
}

fn live_host() -> Option<Host> {
    LIVE.with(|live| live.borrow().last().map(|published| published.host.clone()))
}

/// What the door admitted of an invoker, as the runtime reads it.
#[must_use]
pub fn caller_of(invoker: &ActionInvoker) -> Caller {
    Caller {
        actor: invoker.actor.clone(),
        agent: invoker.kind != ActorKind::Person,
        on_behalf_of: invoker.on_behalf_of.clone(),
    }
}

/// The planner and committer the published actions build with: the host's, reading the caller
/// of the invocation being built from the door.
fn door_handles(host: &Host) -> (Arc<dyn Planner>, Arc<dyn Committer>) {
    let handle = Arc::new(host.handle().with_caller(|| {
        ui::invocation_scope::current_invoker().map(|invoker| caller_of(&invoker))
    }));
    (handle.clone(), handle)
}

/// Every resource the live window's room publishes, each with its actions.
#[must_use]
pub fn published() -> Vec<ui::tilo_actions::Resource> {
    with_live(Published::resources).unwrap_or_default()
}

/// The longest text the persistent index keeps of a resource as a field: a name, a subject.
const SHORT: usize = 80;

/// What search finds of a resource with text: found by it, opening the app at the resource.
fn entry(hit: &Hit) -> SearchEntry {
    let mut entry = SearchEntry::new(hit.title.clone(), hit.texts.join("\n"))
        .anchored(hit.resource.clone())
        .grouped(hit.app_title.clone())
        .subtitled(hit.type_name.clone());
    for text in hit.texts.iter().skip(1) {
        if text.chars().count() <= SHORT {
            entry = entry.field(text.clone(), SearchWeight::Metadata);
        }
    }
    entry
}

/// What search finds of the live window's room: every record and item with text.
#[must_use]
pub fn search_entries() -> Vec<SearchEntry> {
    with_live(|published| published.entries.values().cloned().collect()).unwrap_or_default()
}

impl Resource for Room {
    const KIND: &'static str = crate::RECORD_KIND;
    const LABEL: &'static str = "Apps";
    const ACTION_TYPE: &'static str = tilolang_tilo::action::ACTION_TYPE;

    type State = RoomState;
    type Action = DynAction;

    /// The heads, and the live runtime's mirror brought up to date with the record first: what
    /// the room publishes is then read from the record this state is of, not from the one
    /// before a peer's change arrived.
    fn read(doc: &Doc) -> Result<RoomState, ModelError> {
        if let Some(host) = live_host() {
            host.sync_from(doc);
        }
        Ok(RoomState {
            heads: doc.committed_document().get_heads(),
        })
    }

    /// Keystrokes in one bound text field continue one another: one word, one undo step.
    fn coalesces(previous: &DynAction, next: &DynAction) -> bool {
        previous.action_id == EDIT_TEXT_ACTION
            && next.action_id == EDIT_TEXT_ACTION
            && previous.app == next.app
            && previous.resources() == next.resources()
            && previous.args.get("field") == next.args.get("field")
    }

    fn children(_state: &RoomState) -> Vec<ui::tilo_actions::Resource> {
        published()
    }

    fn search(_state: &RoomState) -> Vec<SearchEntry> {
        search_entries()
    }
}
