//! Records of the room the agent has seen but not opened.
//!
//! Nothing reads such a record until the agent opens it, except the shared
//! desktop, whose tabs say which records people have open. So:
//!
//! - **The shared desktop** is held as its live Automerge document
//!   ([`SharedDesktop`]): the changes that arrived are admitted together
//!   once the session's events are taken in ([`Unopened::settle`]), and the
//!   planner reads its tabs from the current value, and only when it
//!   changed since the last plan. No plan replays its history.
//! - **Every other record** is held packed: its document as last saved,
//!   and the changes that arrived since, in host order. Taking a change is
//!   a push; once the changes since outweigh the saved document (or
//!   [`TAIL_FLOOR`], for small ones) they are merged into it in one load
//!   and saved again. Merging change by change into a live document would
//!   cost the agent's thread work in proportion to the record's history for
//!   every change someone makes, in records it may never open.
//!
//! **Admission.** Every byte here comes from the room's members, and this
//! is the host's process, beside its device key. So none of it reaches
//! Automerge here before a `tilo-automerge-isolate` worker has made the
//! same call on it ([`admit`]): changes go to the isolate in batches, as
//! one load into a blank document where they can. A change the isolate
//! refuses is dropped, as an open record's replica drops it, and holds back
//! no other. When the isolate cannot be asked at all nothing is decoded
//! here instead: the changes are kept, and the record does not open until
//! they can be admitted.
//!
//! **Retention.** Per unopened record the agent keeps the record's
//! document with its whole history, each change once — what its replica
//! would hold had the agent opened it, and what a later concurrent change
//! or an undo may still build on — plus, packed, at most the size of that
//! document (or [`TAIL_FLOOR`]) of changes not yet merged. A change the host
//! delivers again (a reconnect replays the room) merges into what is held
//! and adds nothing. A change waiting for one it builds on stays unmerged
//! until that one arrives. Opening a record seeds its replica from this
//! document ([`Unopened::seed`]).

use std::collections::{BTreeMap, HashMap, HashSet};

use automerge::{AutoCommit, ReadDoc as _};
use tilo_automerge_isolate::{Dependencies, Rejected};
use tilo_browser_peer::DocumentRoute;

/// The route of a room's shared browser desktop (`tilo-browser-runtime`'s
/// replication): its `desktop` value names each tab's record.
const DESKTOP_APP: &str = "so.tilo.browser-desktop";
const DESKTOP_RECORD: &str = "desktop";

/// Change bytes a record may hold unmerged however small its document.
const TAIL_FLOOR: usize = 64 * 1024;

/// Most bytes of changes admitted in one isolate request: well under its
/// input ceiling, so a long history is admitted in parts.
const BATCH_BYTES: usize = 8 * 1024 * 1024;
/// Most changes admitted in one isolate request: well under its chunk
/// ceiling for changes of a few chunks each.
const BATCH_CHANGES: usize = 4096;

/// The shared desktop's route in `room`.
pub(crate) fn desktop_route(room: &str) -> DocumentRoute {
    DocumentRoute {
        app_id: DESKTOP_APP.to_owned(),
        cluster_id: room.to_owned(),
        record: DESKTOP_RECORD.to_owned(),
    }
}

/// The records bound to a tab of a shared desktop `doc`: what people have
/// open. Reads its current value only.
fn desktop_records(doc: &impl automerge::ReadDoc) -> HashSet<String> {
    tilo_desktop_model::shared_record::read(doc, None)
        .map(|desktop| {
            desktop
                .records
                .into_iter()
                .map(|(_, record)| record)
                .collect()
        })
        .unwrap_or_default()
}

/// `known` ordered for the planner: records open on the desktop first, then
/// the most recently changed, so "the notes" means the notes people see.
fn people_first(
    mut known: Vec<DocumentRoute>,
    on_desktop: &HashSet<String>,
    arrived: &HashMap<DocumentRoute, u64>,
) -> Vec<DocumentRoute> {
    known.sort_by_key(|route| {
        (
            !on_desktop.contains(&route.record),
            std::cmp::Reverse(arrived.get(route).copied().unwrap_or(0)),
        )
    });
    known
}

/// Admit untrusted `changes` (documents or changes), in order, to `doc`
/// through the isolate, in as few requests as their size allows. A batch
/// the isolate refuses is halved until each refused change stands alone;
/// it is dropped and holds back no other. Changes waiting for a dependency
/// stay in `doc`'s queue. Returns the indices of the refused changes.
///
/// # Errors
///
/// Why the isolate could not be asked. Nothing is decoded in this process
/// instead; `doc` holds what was admitted before.
fn admit(doc: &mut AutoCommit, changes: &[&[u8]]) -> Result<Vec<usize>, String> {
    let mut refused = Vec::new();
    let mut rest = changes;
    let mut first = 0_usize;
    while !rest.is_empty() {
        let mut count = 0_usize;
        let mut bytes = 0_usize;
        for change in rest.iter().take(BATCH_CHANGES) {
            if count > 0 && bytes.saturating_add(change.len()) > BATCH_BYTES {
                break;
            }
            bytes = bytes.saturating_add(change.len());
            count = count.saturating_add(1);
        }
        let (batch, after) = rest.split_at(count.min(rest.len()));
        admit_run(doc, batch, first, &mut refused)?;
        first = first.saturating_add(batch.len());
        rest = after;
    }
    Ok(refused)
}

/// [`admit`] one batch, `run`, whose first change has index `first`.
fn admit_run(
    doc: &mut AutoCommit,
    run: &[&[u8]],
    first: usize,
    refused: &mut Vec<usize>,
) -> Result<(), String> {
    if run.iter().all(|change| change.is_empty()) {
        return Ok(());
    }
    match tilo_automerge_isolate::apply(doc, &run.concat(), Dependencies::RetainOrphans) {
        Ok(_) => Ok(()),
        Err(Rejected::Unavailable(reason)) => Err(reason),
        Err(error) if run.len() == 1 => {
            tracing::warn!(%error, "agent: a change of an unopened record was not admitted");
            refused.push(first);
            Ok(())
        }
        Err(_) => {
            let (left, right) = run.split_at(run.len() / 2);
            admit_run(doc, left, first, refused)?;
            admit_run(doc, right, first.saturating_add(left.len()), refused)
        }
    }
}

/// Why a record cannot be opened yet: its changes wait for the isolate.
fn not_admitted(reason: String) -> String {
    crate::apps::AppError::Merge(reason).to_string()
}

/// A record nothing reads until it is opened.
#[derive(Default)]
struct Packed {
    /// The document as last merged (`AutoCommit::save` of a document built
    /// only through the isolate); empty before.
    saved: Vec<u8>,
    /// Changes since, in host order, and their bytes.
    tail: Vec<Vec<u8>>,
    tail_bytes: usize,
    /// Tail bytes past which the tail is merged.
    limit: usize,
}

impl Packed {
    fn receive(&mut self, bytes: Vec<u8>) {
        self.tail_bytes = self.tail_bytes.saturating_add(bytes.len());
        self.tail.push(bytes);
        if self.tail_bytes > self.limit.max(TAIL_FLOOR) {
            self.compact();
        }
    }

    /// The document: the saved one with the tail merged, admitted together
    /// through the isolate. Into a blank document that is one load, which
    /// merges the whole tail at about the cost of one change. Changes the
    /// isolate refuses leave the tail.
    ///
    /// # Errors
    ///
    /// The isolate could not be asked; the tail is kept.
    fn document(&mut self) -> Result<AutoCommit, String> {
        let saved = (!self.saved.is_empty()).then_some(self.saved.as_slice());
        let changes: Vec<&[u8]> = saved
            .into_iter()
            .chain(self.tail.iter().map(Vec::as_slice))
            .collect();
        let mut doc = AutoCommit::new();
        let refused = admit(&mut doc, &changes)?;
        if !refused.is_empty() {
            let offset = usize::from(saved.is_some());
            let mut index = offset;
            self.tail.retain(|_| {
                let keep = !refused.contains(&index);
                index = index.saturating_add(1);
                keep
            });
            self.tail_bytes = self.tail.iter().map(Vec::len).sum();
        }
        Ok(doc)
    }

    /// Merge the tail into the saved document, unless a change in it waits
    /// for one not yet here (saving would drop it) or the isolate cannot be
    /// asked.
    fn compact(&mut self) {
        match self.document() {
            Ok(mut doc) if doc.get_missing_deps(&[]).is_empty() => {
                self.saved = doc.save();
                self.tail = Vec::new();
                self.tail_bytes = 0;
                self.limit = self.saved.len();
            }
            // Try again once the tail has doubled.
            _ => self.limit = self.tail_bytes.saturating_mul(2),
        }
    }

    fn seed(&mut self) -> Result<Vec<u8>, String> {
        self.document()
            .map(|mut doc| doc.save())
            .map_err(not_admitted)
    }

    /// Bytes held.
    #[cfg(test)]
    fn retained(&self) -> usize {
        self.saved.len().saturating_add(self.tail_bytes)
    }
}

/// A room's shared desktop, kept merged: which records people have open.
/// What it takes is admitted through the isolate, a batch per
/// [`SharedDesktop::admit`].
pub(crate) struct SharedDesktop {
    doc: AutoCommit,
    /// Changes taken and not yet admitted, in order.
    waiting: Vec<Vec<u8>>,
    /// Change bytes merged since the document was last reloaded, and past
    /// how many it is reloaded: a document built change by change holds
    /// each change's bytes besides its state, a reloaded one only the
    /// state.
    since: usize,
    limit: usize,
    /// Why the isolate could not be asked, while changes wait for it.
    unavailable: Option<String>,
    /// The tabs' records as of the last change; `None` until read after it.
    tabs: Option<HashSet<String>>,
}

impl SharedDesktop {
    pub(crate) fn new() -> Self {
        Self {
            doc: AutoCommit::new(),
            waiting: Vec::new(),
            since: 0,
            limit: 0,
            unavailable: None,
            tabs: None,
        }
    }

    /// Take one change, in host order, for the next [`Self::admit`].
    pub(crate) fn receive(&mut self, bytes: Vec<u8>) {
        self.waiting.push(bytes);
        self.tabs = None;
    }

    /// Admit every change taken since the last call through the isolate.
    /// When the isolate cannot be asked they wait for the next call.
    pub(crate) fn admit(&mut self) {
        if self.waiting.is_empty() {
            return;
        }
        let waiting = std::mem::take(&mut self.waiting);
        let changes: Vec<&[u8]> = waiting.iter().map(Vec::as_slice).collect();
        let admitted = admit(&mut self.doc, &changes);
        drop(changes);
        self.tabs = None;
        if let Err(reason) = admitted {
            tracing::warn!(%reason, "agent: the shared desktop's changes wait for the isolate");
            self.unavailable = Some(reason);
            // Those admitted before it merge again then, adding nothing.
            self.waiting = waiting;
            return;
        }
        self.unavailable = None;
        let bytes: usize = waiting.iter().map(Vec::len).sum();
        self.since = self.since.saturating_add(bytes);
        if self.since > self.limit.max(TAIL_FLOOR) && self.doc.get_missing_deps(&[]).is_empty() {
            let saved = self.doc.save();
            if let Ok(doc) = tilo_automerge_isolate::load(&saved) {
                self.doc = doc;
            }
            self.since = 0;
            self.limit = saved.len();
        }
    }

    /// The records bound to the desktop's tabs, admitting what waits first.
    pub(crate) fn tabs(&mut self) -> &HashSet<String> {
        self.admit();
        self.tabs.get_or_insert_with(|| desktop_records(&self.doc))
    }

    fn seed(&mut self) -> Result<Vec<u8>, String> {
        self.admit();
        match &self.unavailable {
            Some(reason) => Err(not_admitted(reason.clone())),
            None => Ok(self.doc.save()),
        }
    }
}

/// Every record of one room the agent has seen but not opened.
pub(crate) struct Unopened {
    desktop_route: DocumentRoute,
    desktop: Option<SharedDesktop>,
    held: BTreeMap<DocumentRoute, Packed>,
    /// When each record last changed, as a count of changes seen.
    arrived: HashMap<DocumentRoute, u64>,
    changes_seen: u64,
}

impl Unopened {
    pub(crate) fn new(room: &str) -> Self {
        Self {
            desktop_route: desktop_route(room),
            desktop: None,
            held: BTreeMap::new(),
            arrived: HashMap::new(),
            changes_seen: 0,
        }
    }

    /// Take one durable change (or snapshot) of `route`, in host order.
    pub(crate) fn receive(&mut self, route: DocumentRoute, bytes: Vec<u8>) {
        self.changes_seen = self.changes_seen.wrapping_add(1);
        self.arrived.insert(route.clone(), self.changes_seen);
        if route == self.desktop_route {
            self.desktop
                .get_or_insert_with(SharedDesktop::new)
                .receive(bytes);
        } else {
            self.held.entry(route).or_default().receive(bytes);
        }
    }

    /// Admit the shared desktop's changes taken since the last call, in one
    /// batch. Called once the session's events are taken in, so a plan
    /// finds them admitted.
    pub(crate) fn settle(&mut self) {
        if let Some(desktop) = &mut self.desktop {
            desktop.admit();
        }
    }

    /// The records, those people have open on the desktop first, then the
    /// most recently changed. Reads the desktop only when it changed since
    /// the last call.
    pub(crate) fn people_first(&mut self) -> Vec<DocumentRoute> {
        let known = self.routes();
        let none = HashSet::new();
        let on_desktop = self.desktop.as_mut().map_or(&none, SharedDesktop::tabs);
        people_first(known, on_desktop, &self.arrived)
    }

    /// The shared desktop as merged, while no replica holds it.
    pub(crate) fn desktop(&self) -> Option<&AutoCommit> {
        self.desktop.as_ref().map(|live| &live.doc)
    }

    /// The records.
    pub(crate) fn routes(&self) -> Vec<DocumentRoute> {
        let mut routes: Vec<DocumentRoute> = self.held.keys().cloned().collect();
        if self.desktop.is_some() {
            routes.push(self.desktop_route.clone());
        }
        routes
    }

    /// What a replica of `route` is seeded from: its document, saved.
    /// `None` when the room has no such record; `Err` when the isolate
    /// could not be asked to admit its changes.
    pub(crate) fn seed(&mut self, route: &DocumentRoute) -> Option<Result<Vec<u8>, String>> {
        if *route == self.desktop_route {
            return self.desktop.as_mut().map(SharedDesktop::seed);
        }
        self.held.get_mut(route).map(Packed::seed)
    }

    /// Stop holding `route`: its replica holds it now.
    pub(crate) fn forget(&mut self, route: &DocumentRoute) {
        if *route == self.desktop_route {
            self.desktop = None;
        } else {
            self.held.remove(route);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(app: &str, record: &str) -> DocumentRoute {
        DocumentRoute {
            app_id: app.into(),
            cluster_id: "room".into(),
            record: record.into(),
        }
    }

    /// A browser's shared desktop: each change rewrites its value naming
    /// twenty tabs' records, as the browser desktop stores it.
    #[derive(Default)]
    struct Desktop {
        doc: AutoCommit,
        changes: usize,
    }

    /// A desktop of twenty windows, `records` naming theirs.
    fn twenty(records: impl Fn(u64) -> String) -> tilo_desktop_model::ownership::SharedDesktop {
        use tilo_desktop_model::{
            Strip, StripId, Tab, TileContent, WindowGroup, Workspace, WorkspaceId,
        };
        let mut workspace = Workspace::new(WorkspaceId(1), "Room");
        let mut strip = Strip::new(StripId(2), "Main");
        let tabs: Vec<Tab> = (0..20)
            .map(|id| Tab {
                id: 100 + id,
                title: "Notes".into(),
                tile: TileContent::LocalApp {
                    app_id: "so.tilo.notes".into(),
                    label: "Notes".into(),
                    note: String::new(),
                },
            })
            .collect();
        strip.window_groups.push(WindowGroup {
            id: 3,
            active_tab: 100,
            tabs,
            width: tilo_desktop_model::WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        workspace.strips.push(strip);
        tilo_desktop_model::ownership::SharedDesktop {
            tree: tilo_desktop_model::DesktopTree {
                workspaces: vec![workspace],
                ..tilo_desktop_model::DesktopTree::default()
            },
            records: (0..20).map(|id| (100 + id, records(id))).collect(),
            authors: std::collections::BTreeMap::new(),
        }
    }

    impl Desktop {
        /// One change, its last tab bound to `last`.
        fn change(&mut self, last: &str) -> Vec<u8> {
            let index = self.changes;
            self.changes = index.saturating_add(1);
            let desktop = twenty(|tab| {
                if tab == 19 {
                    last.to_owned()
                } else {
                    format!("rec-{index}-{tab}")
                }
            });
            tilo_desktop_model::shared_record::write(&mut self.doc, &desktop).unwrap();
            self.doc.save_incremental()
        }
    }

    /// `count` changes of one document, each rewriting a value as the
    /// desktop does; and the document they make.
    fn history(count: usize) -> (Vec<Vec<u8>>, AutoCommit) {
        let mut desktop = Desktop::default();
        let changes = (0..count)
            .map(|index| desktop.change(&format!("rec-{index}")))
            .collect();
        (changes, desktop.doc)
    }

    fn json(doc: &AutoCommit) -> String {
        serde_json::to_string(&automerge::AutoSerde::from(doc)).unwrap()
    }

    /// `seed` is the document `expected` is: same state, same history.
    fn assert_seeds(seed: &[u8], expected: &mut AutoCommit) {
        let mut seeded = AutoCommit::load(seed).unwrap();
        assert_eq!(json(&seeded), json(expected));
        assert_eq!(seeded.get_heads(), expected.get_heads());
    }

    #[test]
    fn records_people_have_open_come_first_then_the_latest() {
        let known = vec![
            route("sheets", "a"),
            route("sheets", "b"),
            route("sheets", "c"),
        ];
        let on_desktop = std::iter::once("c".to_owned()).collect();
        let arrived = [(route("sheets", "a"), 1), (route("sheets", "b"), 2)]
            .into_iter()
            .collect();
        let ordered: Vec<String> = people_first(known, &on_desktop, &arrived)
            .into_iter()
            .map(|route| route.record)
            .collect();
        assert_eq!(ordered, ["c", "b", "a"]);
    }

    #[test]
    fn the_shared_desktop_names_its_tabs_records() {
        let mut doc = AutoCommit::new();
        let desktop = twenty(|tab| {
            if tab == 7 {
                "rec-open".into()
            } else {
                format!("rec-{tab}")
            }
        });
        tilo_desktop_model::shared_record::write(&mut doc, &desktop).unwrap();
        let records = desktop_records(&doc);
        assert!(records.contains("rec-open"));
        assert_eq!(records.len(), 20);
    }

    /// The desktop's tabs come from its document as merged when its
    /// changes arrived: asking reads its current value, and asking again
    /// with nothing new reads nothing.
    #[test]
    fn desktop_records_cost_is_independent_of_history() {
        let desktop_route = route(DESKTOP_APP, DESKTOP_RECORD);
        let mut unopened = Unopened::new("room");
        unopened.receive(route("sheets", "seen"), AutoCommit::new().save());
        unopened.receive(route("sheets", "rec-open"), AutoCommit::new().save());
        let mut desktop = Desktop::default();
        for index in 0..2_000 {
            let last = if index == 1_999 {
                "rec-open"
            } else {
                "elsewhere"
            };
            unopened.receive(desktop_route.clone(), desktop.change(last));
        }
        let live = unopened.desktop.as_ref().unwrap();
        assert!(live.tabs.is_none(), "a desktop change is read when asked");
        let first = unopened.people_first();
        assert_eq!(first[0].record, "rec-open", "the latest desktop names it");
        let read = unopened.desktop.as_ref().unwrap().tabs.clone().unwrap();
        assert_eq!(read.len(), 20);
        // No desktop change since: the same order without reading the
        // desktop again (with its document gone, a read would find no tabs).
        let live = unopened.desktop.as_mut().unwrap();
        let kept = std::mem::take(&mut live.doc);
        assert_eq!(unopened.people_first(), first);
        unopened.desktop.as_mut().unwrap().doc = kept;
        // Another record changing leaves the desktop's read standing; the
        // desktop changing has it read again.
        unopened.receive(route("sheets", "rec-open"), AutoCommit::new().save());
        assert_eq!(
            unopened.desktop.as_ref().unwrap().tabs.as_ref(),
            Some(&read)
        );
        unopened.receive(desktop_route.clone(), desktop.change("seen"));
        assert_eq!(unopened.people_first()[0].record, "seen");
        // Reloaded along the way, the desktop is still every change it took.
        let seed = unopened.seed(&desktop_route).unwrap().unwrap();
        assert_seeds(&seed, &mut desktop.doc);
    }

    /// A record held across many changes, and a reconnect's replay of all
    /// of them, holds its document and at most as much again in changes
    /// not yet merged: never the log of everything that arrived.
    #[test]
    fn unmounted_record_bytes_are_bounded_by_its_document() {
        let (changes, mut doc) = history(3_000);
        let document = doc.save().len();
        assert!(document > TAIL_FLOOR, "the document outweighs the floor");
        let received: usize = changes
            .iter()
            .map(Vec::len)
            .sum::<usize>()
            .saturating_mul(2);
        let largest = changes.iter().map(Vec::len).max().unwrap();
        let notes = route("so.tilo.notes", "rec-notes");
        let mut unopened = Unopened::new("room");
        let mut most = 0;
        for change in changes.iter().chain(&changes) {
            unopened.receive(notes.clone(), change.clone());
            most = most.max(unopened.held[&notes].retained());
        }
        // The tail is merged once a change takes it past the document.
        assert!(
            most <= document.saturating_mul(2).saturating_add(largest),
            "held up to {most} bytes for a {document}-byte document"
        );
        assert!(most.saturating_mul(4) < received, "{most} of {received}");
        let seed = unopened.seed(&notes).unwrap().unwrap();
        assert_seeds(&seed, &mut doc);
    }

    /// Opening a record after many changes seeds its replica with the
    /// state those changes make, history included.
    #[test]
    fn open_seeds_from_the_maintained_doc() {
        let (changes, mut doc) = history(500);
        let mut unopened = Unopened::new("room");
        let notes = route("so.tilo.notes", "rec-notes");
        for change in changes {
            unopened.receive(notes.clone(), change);
        }
        let seed = unopened.seed(&notes).unwrap().unwrap();
        assert_seeds(&seed, &mut doc);
        unopened.forget(&notes);
        assert!(unopened.seed(&notes).is_none());
        assert!(unopened.routes().is_empty());
    }

    /// A change that arrives before one it builds on is kept until that one
    /// arrives, merges included.
    #[test]
    fn a_change_waiting_for_its_dependency_is_kept() {
        let (changes, mut doc) = history(1_000);
        let notes = route("so.tilo.notes", "rec-notes");
        let mut unopened = Unopened::new("room");
        let (early, late) = changes.split_at(10);
        let (missing, rest) = late.split_first().unwrap();
        for change in early.iter().chain(rest) {
            unopened.receive(notes.clone(), change.clone());
        }
        unopened.receive(notes.clone(), missing.clone());
        let seed = unopened.seed(&notes).unwrap().unwrap();
        assert_seeds(&seed, &mut doc);
    }

    /// Bytes that are no change hold back none of the changes around them.
    #[test]
    fn a_change_that_does_not_load_holds_back_no_other() {
        let (changes, mut doc) = history(1_000);
        let notes = route("so.tilo.notes", "rec-notes");
        let mut unopened = Unopened::new("room");
        for (index, change) in changes.into_iter().enumerate() {
            if index == 500 {
                unopened.receive(notes.clone(), b"not a change".to_vec());
            }
            unopened.receive(notes.clone(), change);
        }
        let seed = unopened.seed(&notes).unwrap().unwrap();
        assert_seeds(&seed, &mut doc);
    }

    /// A checksum-valid document whose op actor index is past its actor
    /// table: stock Automerge panics on it (`tilo-automerge-isolate`'s
    /// decoder cases).
    #[cfg(target_os = "linux")]
    const DECODER_CRASH: &str = "856f4a833d51cb7a00f0010110d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1011f0242f07c77fbf3d3aac425927bb392f9d579080452645fdb50bf7e4097564b07010203021303230240034302560210010402061108130d15092102230e3403420d560f5708800109810102830103940103a50107020002017e0a0302007e00017f000207000309010003070102090005050000017f00000302007f0202017c00017b0a7d016b016c017400090c007b0b7e78067b02017b047d0104020308017c0102040703017f0703017f057f1602007f0203167f0002167e18147668656c6c6f010205007f0104007e010002007e0d7f07010400037f0162000801";

    /// Feed `hostile` among a record's changes and the shared desktop's,
    /// through every path that merges them: the record's compactions and
    /// its seed, the desktop's settle, tabs and seed, and room actions'
    /// desktop holder on its own. Each hostile input is refused and
    /// dropped; what the valid changes make is intact.
    fn hostile_changes_are_dropped(hostile: &[Vec<u8>]) {
        let (changes, mut doc) = history(3_000);
        let notes = route("so.tilo.notes", "rec-notes");
        let desktop_route = route(DESKTOP_APP, DESKTOP_RECORD);
        let mut desktop = Desktop::default();
        let mut unopened = Unopened::new("room");
        for (index, change) in changes.iter().enumerate() {
            if index % 1_000 == 10 {
                for bytes in hostile {
                    unopened.receive(notes.clone(), bytes.clone());
                    unopened.receive(desktop_route.clone(), bytes.clone());
                }
            }
            unopened.receive(notes.clone(), change.clone());
            if index % 100 == 0 {
                let last = format!("tab-{index}");
                unopened.receive(desktop_route.clone(), desktop.change(&last));
                unopened.settle();
            }
        }
        assert!(
            !unopened.held[&notes].saved.is_empty(),
            "the record compacted around the hostile changes"
        );
        let seed = unopened.seed(&notes).unwrap().unwrap();
        assert_seeds(&seed, &mut doc);
        assert!(
            !unopened.held[&notes]
                .tail
                .iter()
                .any(|change| hostile.contains(change)),
            "a refused change is not kept"
        );
        let tabs = unopened.desktop.as_mut().unwrap().tabs();
        assert!(tabs.contains("tab-2900"), "{tabs:?}");
        let seed = unopened.seed(&desktop_route).unwrap().unwrap();
        assert_seeds(&seed, &mut desktop.doc);

        // Room actions' holder of a room's desktop.
        let mut shared = SharedDesktop::new();
        let mut desktop = Desktop::default();
        shared.receive(desktop.change("first"));
        for bytes in hostile {
            shared.receive(bytes.clone());
        }
        shared.receive(desktop.change("open"));
        assert!(shared.tabs().contains("open"));
        assert_seeds(&shared.seed().unwrap(), &mut desktop.doc);
    }

    /// Input whose framing is not Automerge — a chunk declaring a body of
    /// 2^40 bytes that never arrives, and bytes that are no chunk at all —
    /// is refused before any decode and holds back no other change.
    #[test]
    fn malformed_changes_are_refused_and_dropped() {
        let mut huge = vec![0x85, 0x6f, 0x4a, 0x83, 0, 0, 0, 0, 1];
        huge.extend([0x80, 0x80, 0x80, 0x80, 0x80, 0x20]);
        huge.extend([0xff; 16]);
        hostile_changes_are_dropped(&[huge, b"not automerge".to_vec()]);
    }

    /// Room members' bytes reach Automerge only in an isolate worker: input
    /// that panics stock Automerge costs a worker, and would panic this
    /// test had any path decoded it here. Linux only, as the isolate's
    /// workers are: elsewhere it decodes in the calling process by design.
    #[cfg(target_os = "linux")]
    #[test]
    fn changes_that_crash_automerge_are_decoded_only_in_the_isolate() {
        let crash = hex::decode(DECODER_CRASH).unwrap();
        assert!(
            std::panic::catch_unwind(|| AutoCommit::load(&crash)).is_err(),
            "the input must panic Automerge in process, or this test proves nothing"
        );
        hostile_changes_are_dropped(&[crash]);
    }
}
