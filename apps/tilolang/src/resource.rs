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
//! runtime, which registers itself here ([`Live`]). A past version previewed in History builds
//! its own runtime over the old record and publishes nothing.

use std::cell::RefCell;
use std::sync::Arc;

use tilolang_tilo::action::{DynAction, EDIT_TEXT_ACTION};
use tilolang_tilo::host::Host;
use tilolang_tilo::publish::{
    Caller, Committer, Planner, host_resource, publish_package, publish_record, publish_tree,
};
use ui::tilo_actions::{ActionInvoker, ActorKind};
use ui::tilo_resource::{Doc, ModelError, Resource, SearchEntry, SearchWeight};

/// The resource a tilolang app's window shows: the room record of its packages.
pub struct Room;

/// What the framework keeps of the record: its heads, so a change is a new state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoomState {
    heads: Vec<ui::automerge::ChangeHash>,
}

thread_local! {
    /// The runtime of the live window on this thread, which the record is published from.
    static LIVE: RefCell<Option<Host>> = const { RefCell::new(None) };
}

/// The live window's hold on publication: while it lives, the room's records are published
/// from its runtime.
pub struct Live(());

impl Live {
    /// Publish from `host` until the returned hold is dropped.
    #[must_use]
    pub fn register(host: &Host) -> Self {
        LIVE.with(|live| *live.borrow_mut() = Some(host.clone()));
        Self(())
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        LIVE.with(|live| live.borrow_mut().take());
    }
}

fn live() -> Option<Host> {
    LIVE.with(|live| live.borrow().clone())
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

/// Every resource the room's packages hold, each with its actions; the host's own resource; and
/// each package's actions that take no record.
#[must_use]
pub fn published(host: &Host) -> Vec<ui::tilo_actions::Resource> {
    let (planner, committer) = door_handles(host);
    let mut out = vec![host_resource(&committer)];
    for (package, type_name, doc_id) in host.records() {
        let label = host.label_of(&doc_id);
        out.push(match host.store().record(&doc_id) {
            Ok(record) => publish_tree(
                &package.meta,
                &record,
                &type_name,
                &doc_id,
                &label,
                &planner,
            ),
            Err(_) => publish_record(&package.meta, &type_name, &doc_id, &label, &planner),
        });
    }
    for package in host.packages() {
        let resource = publish_package(&package.meta, &planner);
        if !resource.actions.is_empty() {
            out.push(resource);
        }
    }
    out
}

/// The longest text the persistent index keeps of a resource as a field: a name, a subject.
const SHORT: usize = 80;

/// What search finds of the room: every record and item with text, found by it, opening the
/// app at that resource.
#[must_use]
pub fn search_entries(host: &Host) -> Vec<SearchEntry> {
    let mut entries = Vec::new();
    for (package, _, doc) in host.records() {
        let Ok(record) = host.store().record(&doc) else {
            continue;
        };
        for hit in tilolang_tilo::search::hits(&package.meta, &record, &doc) {
            let mut entry = SearchEntry::new(hit.title.clone(), hit.texts.join("\n"))
                .anchored(hit.resource.clone())
                .grouped(hit.app_title.clone())
                .subtitled(hit.type_name.clone());
            for text in hit.texts.iter().skip(1) {
                if text.chars().count() <= SHORT {
                    entry = entry.field(text.clone(), SearchWeight::Metadata);
                }
            }
            entries.push(entry);
        }
    }
    entries
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
        if let Some(host) = live() {
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
        live().map(|host| published(&host)).unwrap_or_default()
    }

    fn search(_state: &RoomState) -> Vec<SearchEntry> {
        live().map(|host| search_entries(&host)).unwrap_or_default()
    }
}
