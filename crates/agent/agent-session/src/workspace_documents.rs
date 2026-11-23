//! A workspace's records as this device's service holds them.
//!
//! The device's workspace service holds every record of the workspace the
//! session joined, opened with this member's channel keys. Nothing here keeps
//! a second copy: a record is read from the service when asked, and what is
//! read of it (its text, its title, how many changes it holds) is kept only
//! with the heads it was read at, so it is read again only once the record
//! changed.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use automerge::AutoCommit;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_peer_state::workspace_documents::{DeviceDocuments, DocumentRead};
use ui::search::index_publication::IndexEntry;

/// The documents of the workspaces this device's service holds, as the
/// session's own roles read them: the service on a computer, the page's
/// replicated copy on the web.
#[derive(Clone)]
pub(crate) struct Reads(Arc<dyn DeviceDocuments>);

impl Reads {
    pub(crate) fn new(reads: Arc<dyn DeviceDocuments>) -> Self {
        Self(reads)
    }

    /// The record whole, saved, when the service holds it and can open it
    /// now.
    pub(crate) fn record(&self, route: &DocumentRoute) -> Result<Option<Vec<u8>>, String> {
        Ok(match self.read(route, None)? {
            DocumentRead::Whole { bytes, .. } => Some(bytes),
            DocumentRead::Changes { .. }
            | DocumentRead::Unchanged(_)
            | DocumentRead::Absent
            | DocumentRead::Pending => None,
        })
    }

    /// The revision the record stands at for this device, or `None` while it
    /// is not here yet.
    pub(crate) fn document_revision(
        &self,
        route: &DocumentRoute,
    ) -> Result<Option<[u8; 32]>, String> {
        Ok(match self.read(route, None)? {
            DocumentRead::Whole { revision, .. } | DocumentRead::Changes { revision, .. } => {
                Some(revision)
            }
            DocumentRead::Unchanged(revision) => Some(revision),
            DocumentRead::Absent | DocumentRead::Pending => None,
        })
    }

    /// Whether the document can be read now: here, and its keys arrived. Decided without
    /// reading it out: a sealed document's first read after a start opens its whole history.
    pub(crate) fn readable(&self, route: &DocumentRoute) -> bool {
        #[cfg(feature = "test-support")]
        if crate::workspace_actions::held_back::hidden(&route.cluster_id, &route.document) {
            return false;
        }
        self.0
            .readable(&route.cluster_id, &route.app_id, &route.document)
    }

    /// What the workspace holds of the record, readable or still sealed.
    pub(crate) fn holds(&self, route: &DocumentRoute) -> bool {
        #[cfg(feature = "test-support")]
        if crate::workspace_actions::held_back::hidden(&route.cluster_id, &route.document) {
            return false;
        }
        self.0
            .holds(&route.cluster_id, &route.app_id, &route.document)
    }

    /// The records the workspace holds, by app and route.
    pub(crate) fn documents(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
        let listed = self.0.listing(workspace);
        #[cfg(feature = "test-support")]
        let listed = listed.map(|mut listed| {
            listed.retain(|(_, document)| {
                !crate::workspace_actions::held_back::unlisted(workspace, document)
            });
            listed
        });
        listed
    }

    /// The workspaces the service holds.
    pub(crate) fn workspaces(&self) -> Vec<String> {
        self.0.workspaces()
    }

    pub(crate) fn read(
        &self,
        route: &DocumentRoute,
        since: Option<&[[u8; 32]]>,
    ) -> Result<DocumentRead, String> {
        #[cfg(feature = "test-support")]
        if crate::workspace_actions::held_back::hidden(&route.cluster_id, &route.document) {
            // A part held back is on its way, not one the workspace holds none of: a request
            // reading it waits, as it does for one the workspace has still to send.
            return Ok(DocumentRead::Pending);
        }
        self.0
            .read(&route.cluster_id, &route.app_id, &route.document, since)
    }
}

/// The route of a workspace's layout: its `desktop` value names each tab's record.
pub(crate) fn desktop_route(workspace: &str) -> DocumentRoute {
    DocumentRoute {
        app_id: tilo_desktop_model::ownership::DESKTOP_APP.to_owned(),
        cluster_id: workspace.to_owned(),
        document: tilo_desktop_model::ownership::DESKTOP_DOCUMENT.to_owned(),
    }
}

/// The records bound to a tab of a shared desktop `doc`: what people have
/// open. Reads its current value only.
fn desktop_documents(doc: &AutoCommit) -> HashSet<String> {
    tilo_desktop_model::shared_resource::read(doc, None)
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
            !on_desktop.contains(&route.document),
            std::cmp::Reverse(arrived.get(route).copied().unwrap_or(0)),
        )
    });
    known
}

/// How a record's text is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reading {
    /// Its words, as search reads them ([`crate::search::plain_text`]).
    Plain,
}

impl Reading {
    fn read(self, doc: &AutoCommit) -> Arc<str> {
        match self {
            Self::Plain => crate::search::plain_text(doc).into(),
        }
    }
}

/// Something read of a record, and how many writes had landed in it then:
/// a record is read again only once another write lands in it.
type Read<T> = HashMap<DocumentRoute, (u64, T)>;

/// One workspace's records in this device's service, minus those a replica
/// of this member holds open.
pub(crate) struct WorkspaceDocuments {
    reads: Reads,
    workspace: String,
    /// Records an open replica holds now: not listed here.
    opened: HashSet<DocumentRoute>,
    /// When each record last changed, as a count of landings seen.
    arrived: HashMap<DocumentRoute, u64>,
    /// The documents search has no entries for, as of the last listing and
    /// every write since: what an index pass asks for.
    unindexed: HashSet<DocumentRoute>,
    /// Whether [`Self::unindexed`] has been filled from the replica: what it
    /// already held, whose entries no pass read yet.
    listed: bool,
    landings: u64,
    /// How many writes landed in each app's documents: a revision of one app's documents, which
    /// another app's writes leave where it is.
    app_landings: Arc<HashMap<String, u64>>,
    /// The documents of the workspace not held open, as last listed, with the
    /// same set for asking whether one is in it. Thrown away when a document
    /// that was not here lands, or an open replica takes one.
    known: Option<(Arc<[DocumentRoute]>, HashSet<DocumentRoute>)>,
    /// Where a test held a document back or let one go when [`Self::known`] was listed: a
    /// document let go arrives, and is listed again ([`crate::workspace_actions::held_back`]).
    #[cfg(feature = "test-support")]
    held_back: u64,
    /// What search finds in each record, as its app reads it: the entries it
    /// publishes to the search index, one per resource of the record.
    entries: Read<Arc<[IndexEntry]>>,
    /// The documents search keeps no entries for because they could not be read, by the
    /// landings they stood at when it gave up: a landing reads them again.
    unreadable: HashMap<DocumentRoute, u64>,
    /// How many writes landed in each document: what says whether what the index read of it
    /// is still what the workspace holds, without reading it again.
    moved: HashMap<DocumentRoute, u64>,
    /// The plain text of each record search reads.
    texts: HashMap<DocumentRoute, (u64, Option<Arc<str>>)>,
    tabs: Read<Arc<HashSet<String>>>,
}

impl WorkspaceDocuments {
    pub(crate) fn new(workspace: &str, reads: Reads) -> Self {
        Self {
            reads,
            workspace: workspace.to_owned(),
            opened: HashSet::new(),
            arrived: HashMap::new(),
            unindexed: HashSet::new(),
            listed: false,
            landings: 0,
            app_landings: Arc::default(),
            known: None,
            #[cfg(feature = "test-support")]
            held_back: 0,
            entries: HashMap::new(),
            unreadable: HashMap::new(),
            moved: HashMap::new(),
            texts: HashMap::new(),
            tabs: HashMap::new(),
        }
    }

    /// A write to `route` landed in the replica.
    pub(crate) fn landed(&mut self, route: &DocumentRoute) {
        self.landings = self.landings.wrapping_add(1);
        let app = Arc::make_mut(&mut self.app_landings)
            .entry(route.app_id.clone())
            .or_insert(0);
        *app = app.wrapping_add(1);
        self.arrived.insert(route.clone(), self.landings);
        if !self.opened.contains(route) {
            self.unindexed.insert(route.clone());
        }
        // A document that was not here is one the listing has to name too.
        if self
            .known
            .as_ref()
            .is_some_and(|(_, set)| !set.contains(route))
        {
            self.known = None;
        }
        // What search answers with is read again from the document as it stands now: the write
        // landed, so the entries kept at the old revision describe a document that is gone.
        self.entries.remove(route);
        // A document the index could not read is read again now that it moved.
        self.unreadable.remove(route);
        *self.moved.entry(route.clone()).or_insert(0) =
            self.moved.get(route).copied().unwrap_or(0).wrapping_add(1);
    }

    /// The document at `route` search could not read for now: asked for again once a write
    /// lands in it, never in a pass that would ask the same thing for ever.
    pub(crate) fn unreadable(&mut self, route: &DocumentRoute) {
        self.unreadable.insert(route.clone(), self.landings);
        self.unindexed.remove(route);
    }

    /// What search finds in the document at `route`, read off the serving thread by the
    /// index's own: kept at the heads it was read from, and given back to the app's next
    /// read of it without opening the document again.
    pub(crate) fn keep_entries(&mut self, route: &DocumentRoute, entries: Arc<[IndexEntry]>) {
        let at = self.moved(route);
        self.entries.insert(route.clone(), (at, entries));
        self.unindexed.remove(route);
    }

    /// How many writes have landed in the document at `route`: the index reads it again when
    /// that moved since it read it.
    pub(crate) fn moved(&self, route: &DocumentRoute) -> u64 {
        self.moved.get(route).copied().unwrap_or(0)
    }

    /// The reads the workspace's documents are made from, for the index's own thread.
    pub(crate) fn reads(&self) -> Reads {
        self.reads.clone()
    }

    /// The workspace's id.
    pub(crate) fn workspace(&self) -> &str {
        &self.workspace
    }

    /// How many writes landed in the workspace: a revision of it as a whole.
    pub(crate) const fn landings(&self) -> u64 {
        self.landings
    }

    /// How many writes landed in each app's documents ([`Self::landings`], by app).
    pub(crate) fn app_landings(&self) -> Arc<HashMap<String, u64>> {
        Arc::clone(&self.app_landings)
    }

    /// Whether what search finds in the document at `route` is kept: a query answers from it
    /// without opening the document at all.
    pub(crate) fn indexed(&self, route: &DocumentRoute) -> bool {
        self.entries.contains_key(route)
    }

    /// What search finds in the document at `route`, as its app last told it.
    pub(crate) fn kept_entries(&self, route: &DocumentRoute) -> Option<Arc<[IndexEntry]>> {
        self.entries
            .get(route)
            .map(|(_, entries)| Arc::clone(entries))
    }

    /// Up to `limit` of the workspace's documents search keeps no entries for, but those
    /// `later` keeps for later (being read now, or read again only after a while): the warm
    /// thread reads them, so a query never has to.
    pub(crate) fn unindexed(
        &mut self,
        limit: usize,
        later: impl Fn(&DocumentRoute) -> bool,
    ) -> Vec<DocumentRoute> {
        self.list_unindexed();
        self.unindexed
            .iter()
            .filter(|route| !later(route))
            .take(limit)
            .cloned()
            .collect()
    }

    /// Whether any of the workspace's documents waits to be read for the index.
    pub(crate) fn has_unindexed(&mut self) -> bool {
        self.list_unindexed();
        !self.unindexed.is_empty()
    }

    /// Fill [`Self::unindexed`] with what the replica already held when this
    /// was made, once: from then on the writes keep it current, and a document
    /// the index gave up on stays out until a write lands in it.
    fn list_unindexed(&mut self) {
        if self.listed {
            return;
        }
        self.listed = true;
        for route in self.routes() {
            if self.indexes(&route) {
                self.unindexed.insert(route);
            }
        }
    }

    /// Whether the document at `route` still wants entries: it has none, and the index has not
    /// given up on it since it last moved.
    fn indexes(&self, route: &DocumentRoute) -> bool {
        !self.indexed(route)
            && self
                .unreadable
                .get(route)
                .is_none_or(|gave_up| *gave_up != self.landings)
    }

    /// The records of the workspace not held open.
    pub(crate) fn routes(&self) -> Vec<DocumentRoute> {
        match self.reads.documents(&self.workspace) {
            Ok(records) => records
                .into_iter()
                .map(|(app_id, record)| DocumentRoute {
                    app_id,
                    cluster_id: self.workspace.clone(),
                    document: record,
                })
                .filter(|route| !self.opened.contains(route))
                .collect(),
            Err(error) => {
                tracing::debug!(%error, "agent: the workspace's records are not listed");
                Vec::new()
            }
        }
    }

    /// `routes` as [`Self::people_first`] orders them.
    pub(crate) fn people_first_of(&mut self, routes: Vec<DocumentRoute>) -> Vec<DocumentRoute> {
        let tabs = self.desktop_tabs();
        people_first(routes, &tabs, &self.arrived)
    }

    /// The documents of the workspace not held open, as last listed: what a
    /// request reads the workspace's documents from without listing them again.
    /// Listed once, and again only once a document that was not here lands or
    /// an open replica takes one.
    pub(crate) fn known(&mut self) -> Arc<[DocumentRoute]> {
        #[cfg(feature = "test-support")]
        {
            let moved = crate::workspace_actions::held_back::moved();
            if moved != self.held_back {
                self.held_back = moved;
                self.known = None;
                self.landings = self.landings.wrapping_add(1);
            }
        }
        if let Some((known, _)) = &self.known {
            return Arc::clone(known);
        }
        let known: Arc<[DocumentRoute]> = self.routes().into();
        let set: HashSet<DocumentRoute> = known.iter().cloned().collect();
        self.known = Some((Arc::clone(&known), set));
        known
    }

    /// The records bound to the shared desktop's tabs.
    pub(crate) fn desktop_tabs(&mut self) -> Arc<HashSet<String>> {
        let desktop = desktop_route(&self.workspace);
        self.cached(
            &desktop,
            |records| &mut records.tabs,
            |bytes| {
                Arc::new(
                    load(&bytes)
                        .map(|doc| desktop_documents(&doc))
                        .unwrap_or_default(),
                )
            },
        )
        .unwrap_or_default()
    }

    /// The shared desktop as the replica holds it, while no open replica does.
    pub(crate) fn desktop(&self) -> Option<AutoCommit> {
        let desktop = desktop_route(&self.workspace);
        if self.opened.contains(&desktop) {
            return None;
        }
        load(&self.document(&desktop)?)
    }

    /// The record at `route`, saved, as the service holds it now. `None` when
    /// the service holds no such record or cannot open it yet.
    pub(crate) fn document(&self, route: &DocumentRoute) -> Option<Vec<u8>> {
        match self.reads.record(route) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::debug!(%error, record = %route.document, "agent: a record does not read");
                None
            }
        }
    }

    /// `read` of the record at `route` (`T` kept in `field`), again only once
    /// it changed since. `None` for a record the replica does not hold or
    /// cannot open yet.
    fn cached<T: Clone>(
        &mut self,
        route: &DocumentRoute,
        field: impl Fn(&mut Self) -> &mut Read<T>,
        read: impl FnOnce(Vec<u8>) -> T,
    ) -> Option<T> {
        self.cached_telling(route, field, read)
            .map(|(value, _)| value)
    }

    /// [`Self::cached`], saying whether the document was opened for it: `false` when what is
    /// kept answered without reading it at all.
    fn cached_telling<T: Clone>(
        &mut self,
        route: &DocumentRoute,
        field: impl Fn(&mut Self) -> &mut Read<T>,
        read: impl FnOnce(Vec<u8>) -> T,
    ) -> Option<(T, bool)> {
        // Only a write landing in the record changes it, so what was read of
        // it is kept until one does: a query never opens a document that no
        // write touched since the last one.
        let at = self.moved(route);
        if let Some((kept_at, value)) = field(self).get(route)
            && *kept_at == at
        {
            return Some((value.clone(), false));
        }
        match self.reads.record(route) {
            Ok(Some(bytes)) => {
                let value = read(bytes);
                field(self).insert(route.clone(), (at, value.clone()));
                Some((value, true))
            }
            Ok(None) => None,
            Err(error) => {
                tracing::debug!(%error, record = %route.document, "agent: a record does not read");
                None
            }
        }
    }

    /// What search finds in the document at `route`, as `read` finds it in
    /// what was saved of it: the entries its app publishes to the search
    /// index. Read once per change of it, and whether it was opened for this
    /// answer.
    pub(crate) fn entries(
        &mut self,
        route: &DocumentRoute,
        read: impl FnOnce(Vec<u8>) -> Result<Vec<IndexEntry>, String>,
    ) -> (Option<Arc<[IndexEntry]>>, bool) {
        let answer = self
            .cached_telling(
                route,
                |records| &mut records.entries,
                |bytes| {
                    read(bytes)
                        .unwrap_or_else(|reason| {
                            tracing::warn!(%reason, "agent: a record is not read for search");
                            Vec::new()
                        })
                        .into()
                },
            )
            .map_or((None, false), |(entries, read)| (Some(entries), read));
        if answer.0.is_some() {
            self.unindexed.remove(route);
        }
        answer
    }

    fn read_texts(
        &mut self,
        reading: Reading,
    ) -> &mut HashMap<DocumentRoute, (u64, Option<Arc<str>>)> {
        match reading {
            Reading::Plain => &mut self.texts,
        }
    }

    /// The text of the record at `route`, read as `reading` says, once per
    /// change of it.
    pub(crate) fn text(&mut self, route: &DocumentRoute, reading: Reading) -> Option<Arc<str>> {
        self.cached(
            route,
            |records| records.read_texts(reading),
            |bytes| load(&bytes).map(|document| reading.read(&document)),
        )
        .flatten()
    }

    /// When the record at `route` last changed, as a count of landings
    /// (higher is later); `None` for a record nothing landed in since this
    /// session started.
    pub(crate) fn changed(&self, route: &DocumentRoute) -> Option<u64> {
        self.arrived.get(route).copied()
    }
}

/// The document `bytes` the replica saved.
fn load(bytes: &[u8]) -> Option<AutoCommit> {
    AutoCommit::load(bytes)
        .inspect_err(
            |error| tracing::warn!(%error, "agent: a record from the replica does not load"),
        )
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(app: &str, record: &str) -> DocumentRoute {
        DocumentRoute {
            app_id: app.into(),
            cluster_id: "workspace".into(),
            document: record.into(),
        }
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
            .map(|route| route.document)
            .collect();
        assert_eq!(ordered, ["c", "b", "a"]);
    }

    #[test]
    fn the_shared_desktop_names_its_tabs_records() {
        use tilo_desktop_model::{Strip, StripId, Tab, TileContent, WindowGroup, Workspace};
        let mut workspace =
            Workspace::new(tilo_desktop_model::WorkspaceId::from_u128(1), "Workspace");
        let mut strip = Strip::new(StripId(2), "Main");
        strip.window_groups.push(WindowGroup {
            id: 3,
            active_tab: 100,
            tabs: (0..20_u64)
                .map(|id| Tab {
                    id: id.saturating_add(100),
                    title: "Notes".into(),
                    tile: TileContent::LocalApp {
                        app_id: "so.tilo.notes".into(),
                        label: "Notes".into(),
                        note: String::new(),
                    },
                    app_title: None,
                    view: String::new(),
                    pinned: false,
                })
                .collect(),
            width: tilo_desktop_model::WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        workspace.strips.push(strip);
        let desktop = tilo_desktop_model::ownership::SharedDesktop {
            tree: tilo_desktop_model::Layout {
                workspaces: vec![workspace],
                ..tilo_desktop_model::Layout::default()
            },
            records: (0..20_u64)
                .map(|id| {
                    let record = if id == 7 {
                        "rec-open".into()
                    } else {
                        format!("rec-{id}")
                    };
                    (id.saturating_add(100), record)
                })
                .collect(),
            items: Vec::new(),
            authors: std::collections::BTreeMap::new(),
            opened_by: std::collections::BTreeMap::new(),
        };
        let mut doc = AutoCommit::new();
        tilo_desktop_model::shared_resource::write(&mut doc, &desktop).unwrap();
        let records = desktop_documents(&doc);
        assert!(records.contains("rec-open"));
        assert_eq!(records.len(), 20);
    }
}
