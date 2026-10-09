//! Records of the room the agent has seen but not opened.
//!
//! Nothing reads such a record until the agent opens it, except the shared
//! desktop, whose tabs say which records people have open. So:
//!
//! - **The shared desktop** is held as its live Automerge document, each
//!   change merged as it arrives; the planner reads its tabs from the
//!   current value, and only when it changed since the last plan. No plan
//!   replays its history.
//! - **Every other record** is held packed: its document as last saved,
//!   and the changes that arrived since, in host order. Taking a change is
//!   a push; once the changes since outweigh the saved document (or
//!   [`TAIL_FLOOR`], for small ones) they are merged into it in one load
//!   and saved again. Merging change by change into a live document would
//!   cost the agent's thread work in proportion to the record's history for
//!   every change someone makes, in records it may never open.
//!
//! **Retention.** Per unopened record the agent keeps the record's
//! document with its whole history, each change once — what its replica
//! would hold had the agent opened it, and what a later concurrent change
//! or an undo may still build on — plus, packed, at most the size of that
//! document (or [`TAIL_FLOOR`]) of changes not yet merged. A change the host
//! delivers again (a reconnect replays the room) merges into what is held
//! and adds nothing. Nothing is dropped: a change waiting for one it builds
//! on stays unmerged until that one arrives, and a change that does not
//! load is merged on its own, as before, so it holds back no other. Opening
//! a record seeds its replica from this document ([`Unopened::seed`]).

use std::collections::{BTreeMap, HashMap, HashSet};

use automerge::{AutoCommit, LoadOptions, OnPartialLoad, ReadDoc as _};
use tilo_automerge::ApplyIncoming as _;
use tilo_browser_peer::DocumentRoute;

/// The route of a room's shared browser desktop (`tilo-browser-runtime`'s
/// replication): its `desktop` value names each tab's record.
const DESKTOP_APP: &str = "so.tilo.browser-desktop";
const DESKTOP_RECORD: &str = "desktop";

/// Change bytes a record may hold unmerged however small its document.
const TAIL_FLOOR: usize = 64 * 1024;

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
pub(crate) fn desktop_records(doc: &impl automerge::ReadDoc) -> HashSet<String> {
    let Ok(Some((automerge::Value::Scalar(value), _))) = doc.get(automerge::ROOT, "desktop") else {
        return HashSet::new();
    };
    let automerge::ScalarValue::Str(text) = value.as_ref() else {
        return HashSet::new();
    };
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|desktop| desktop.get("records").and_then(|r| r.as_array()).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|pair| pair.get(1).and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect()
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

/// Merge `bytes` into `doc`; the first failure is why the record refuses
/// to open, as it was when its changes were merged only then.
fn merge(doc: &mut AutoCommit, bytes: &[u8], refused: &mut Option<String>) {
    if let Err(error) = doc.load_incoming(bytes)
        && refused.is_none()
    {
        *refused = Some(error.to_string());
    }
}

/// A record nothing reads until it is opened.
#[derive(Default)]
struct Packed {
    /// The document as last merged (`AutoCommit::save`); empty before.
    saved: Vec<u8>,
    /// Changes since, in host order, and their bytes.
    tail: Vec<Vec<u8>>,
    tail_bytes: usize,
    /// Tail bytes past which the tail is merged.
    limit: usize,
    refused: Option<String>,
}

impl Packed {
    fn receive(&mut self, bytes: Vec<u8>) {
        self.tail_bytes = self.tail_bytes.saturating_add(bytes.len());
        self.tail.push(bytes);
        if self.tail_bytes > self.limit.max(TAIL_FLOOR) {
            self.compact();
        }
    }

    /// The document: the saved one with the tail merged.
    fn document(&mut self) -> AutoCommit {
        let mut all = Vec::with_capacity(self.saved.len().saturating_add(self.tail_bytes));
        all.extend_from_slice(&self.saved);
        for bytes in &self.tail {
            all.extend_from_slice(bytes);
        }
        // One load merges the whole tail at about the cost of one change.
        let strict = LoadOptions::new().on_partial_load(OnPartialLoad::Error);
        if let Ok(doc) = AutoCommit::load_with_options(&all, strict) {
            return doc;
        }
        // A change that does not load, or waits for one not yet here:
        // merge them one by one, as they arrived.
        let mut doc = AutoCommit::new();
        merge(&mut doc, &self.saved, &mut self.refused);
        for bytes in &self.tail {
            merge(&mut doc, bytes, &mut self.refused);
        }
        doc
    }

    /// Merge the tail into the saved document, unless a change in it waits
    /// for one not yet here (saving would drop it) or the record refuses.
    fn compact(&mut self) {
        let mut doc = self.document();
        if self.refused.is_none() && doc.get_missing_deps(&[]).is_empty() {
            self.saved = doc.save();
            self.tail = Vec::new();
            self.tail_bytes = 0;
            self.limit = self.saved.len();
        } else {
            // Try again once the tail has doubled.
            self.limit = self.tail_bytes.saturating_mul(2);
        }
    }

    fn seed(&mut self) -> Result<Vec<u8>, String> {
        let mut doc = self.document();
        match &self.refused {
            Some(reason) => Err(crate::apps::AppError::Merge(reason.clone()).to_string()),
            None => Ok(doc.save()),
        }
    }

    /// Bytes held.
    #[cfg(test)]
    fn retained(&self) -> usize {
        self.saved.len().saturating_add(self.tail_bytes)
    }
}

/// The shared desktop, kept merged.
struct Live {
    doc: AutoCommit,
    /// Change bytes merged since the document was last reloaded, and past
    /// how many it is reloaded: a document built change by change holds
    /// each change's bytes besides its state, a reloaded one only the
    /// state.
    since: usize,
    limit: usize,
    refused: Option<String>,
    /// The tabs' records as of the last change; `None` until read after it.
    tabs: Option<HashSet<String>>,
}

impl Live {
    fn new() -> Self {
        Self {
            doc: AutoCommit::new(),
            since: 0,
            limit: 0,
            refused: None,
            tabs: None,
        }
    }

    fn receive(&mut self, bytes: &[u8]) {
        merge(&mut self.doc, bytes, &mut self.refused);
        self.tabs = None;
        self.since = self.since.saturating_add(bytes.len());
        if self.since > self.limit.max(TAIL_FLOOR)
            && self.refused.is_none()
            && self.doc.get_missing_deps(&[]).is_empty()
        {
            let saved = self.doc.save();
            if let Ok(doc) = AutoCommit::load(&saved) {
                self.doc = doc;
            }
            self.since = 0;
            self.limit = saved.len();
        }
    }

    fn tabs(&mut self) -> &HashSet<String> {
        self.tabs.get_or_insert_with(|| desktop_records(&self.doc))
    }

    fn seed(&mut self) -> Result<Vec<u8>, String> {
        match &self.refused {
            Some(reason) => Err(crate::apps::AppError::Merge(reason.clone()).to_string()),
            None => Ok(self.doc.save()),
        }
    }
}

/// Every record of one room the agent has seen but not opened.
pub(super) struct Unopened {
    desktop_route: DocumentRoute,
    desktop: Option<Live>,
    held: BTreeMap<DocumentRoute, Packed>,
    /// When each record last changed, as a count of changes seen.
    arrived: HashMap<DocumentRoute, u64>,
    changes_seen: u64,
}

impl Unopened {
    pub(super) fn new(room: &str) -> Self {
        Self {
            desktop_route: desktop_route(room),
            desktop: None,
            held: BTreeMap::new(),
            arrived: HashMap::new(),
            changes_seen: 0,
        }
    }

    /// Take one durable change (or snapshot) of `route`, in host order.
    pub(super) fn receive(&mut self, route: DocumentRoute, bytes: Vec<u8>) {
        self.changes_seen = self.changes_seen.wrapping_add(1);
        self.arrived.insert(route.clone(), self.changes_seen);
        if route == self.desktop_route {
            self.desktop.get_or_insert_with(Live::new).receive(&bytes);
        } else {
            self.held.entry(route).or_default().receive(bytes);
        }
    }

    /// The records, those people have open on the desktop first, then the
    /// most recently changed. Reads the desktop only when it changed since
    /// the last call.
    pub(super) fn people_first(&mut self) -> Vec<DocumentRoute> {
        let known = self.routes();
        let none = HashSet::new();
        let on_desktop = self.desktop.as_mut().map_or(&none, Live::tabs);
        people_first(known, on_desktop, &self.arrived)
    }

    /// The shared desktop as merged, while no replica holds it.
    pub(super) fn desktop(&self) -> Option<&AutoCommit> {
        self.desktop.as_ref().map(|live| &live.doc)
    }

    /// The records.
    pub(super) fn routes(&self) -> Vec<DocumentRoute> {
        let mut routes: Vec<DocumentRoute> = self.held.keys().cloned().collect();
        if self.desktop.is_some() {
            routes.push(self.desktop_route.clone());
        }
        routes
    }

    /// What a replica of `route` is seeded from: its document, saved.
    /// `None` when the room has no such record; `Err` when one of its
    /// changes did not merge.
    pub(super) fn seed(&mut self, route: &DocumentRoute) -> Option<Result<Vec<u8>, String>> {
        if *route == self.desktop_route {
            return self.desktop.as_mut().map(Live::seed);
        }
        self.held.get_mut(route).map(Packed::seed)
    }

    /// Stop holding `route`: its replica holds it now.
    pub(super) fn forget(&mut self, route: &DocumentRoute) {
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
    use automerge::transaction::Transactable as _;

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

    impl Desktop {
        /// One change, its last tab bound to `last`.
        fn change(&mut self, last: &str) -> Vec<u8> {
            let index = self.changes;
            self.changes = index.saturating_add(1);
            let mut records: Vec<String> = (0..19)
                .map(|tab| format!("[{tab},\"rec-{index}-{tab}\"]"))
                .collect();
            records.push(format!("[19,\"{last}\"]"));
            self.doc
                .put(
                    automerge::ROOT,
                    "desktop",
                    format!("{{\"tree\":{{}},\"records\":[{}]}}", records.join(",")),
                )
                .unwrap();
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
        doc.put(
            automerge::ROOT,
            "desktop",
            r#"{"tree":{},"records":[[7,"rec-open"]]}"#,
        )
        .unwrap();
        let records = desktop_records(&doc);
        assert!(records.contains("rec-open"));
        assert_eq!(records.len(), 1);
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
}
