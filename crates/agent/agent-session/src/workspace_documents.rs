//! A workspace's records as the session's replica holds them.
//!
//! The session keeps every record of the workspace it joined in its replica
//! (`tilo_session_client::engine::Engine`), opened with this member's channel
//! keys. Nothing here keeps a second copy: a record is read from the replica
//! when asked, and what is read of it (its text, its title, how many changes
//! it holds) is kept only with the revision it was read at, so it is read
//! again only once the record changed.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use automerge::{AutoCommit, ReadDoc as _};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_session_client::engine::Engine;
use ui::search::index_publication::IndexEntry;

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
    /// Its messages ([`crate::message_lines::message_text`]).
    Messages,
}

impl Reading {
    fn read(self, doc: &AutoCommit) -> Arc<str> {
        match self {
            Self::Plain => crate::search::plain_text(doc).into(),
            Self::Messages => crate::message_lines::message_text(doc).into(),
        }
    }
}

/// When a record's text was read: at a revision of the replica's record, or
/// at an open record's change and operation counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stamp {
    Replica([u8; 32]),
    Open(u64, u64),
}

/// Something read of a record, and the revision it was read at.
type Read<T> = HashMap<DocumentRoute, ([u8; 32], T)>;

/// One workspace's records in the session's replica, minus those a replica
/// of this member holds open.
pub(crate) struct WorkspaceDocuments {
    engine: Arc<Engine>,
    workspace: String,
    /// Records an open replica holds now: not listed here.
    opened: HashSet<DocumentRoute>,
    /// When each record last changed, as a count of landings seen.
    arrived: HashMap<DocumentRoute, u64>,
    landings: u64,
    titles: Read<Option<String>>,
    /// What search finds in each record, as its app reads it: the entries it
    /// publishes to the search index, one per resource of the record.
    entries: Read<Arc<[IndexEntry]>>,
    /// The documents search keeps no entries for because they could not be read, by the
    /// landings they stood at when it gave up: a landing reads them again.
    unreadable: HashMap<DocumentRoute, u64>,
    /// How many writes landed in each document: what says whether what the index read of it
    /// is still what the workspace holds, without reading it again.
    moved: HashMap<DocumentRoute, u64>,
    /// The plain text of each record the planner searches, and of open records
    /// ([`Self::open_text`]).
    texts: HashMap<DocumentRoute, (Stamp, Arc<str>)>,
    /// The messages of each message record news and waits read.
    messages: HashMap<DocumentRoute, (Stamp, Arc<str>)>,
    counts: Read<Option<u64>>,
    tabs: Read<Arc<HashSet<String>>>,
}

impl WorkspaceDocuments {
    pub(crate) fn new(workspace: &str, engine: Arc<Engine>) -> Self {
        Self {
            engine,
            workspace: workspace.to_owned(),
            opened: HashSet::new(),
            arrived: HashMap::new(),
            landings: 0,
            titles: HashMap::new(),
            entries: HashMap::new(),
            unreadable: HashMap::new(),
            moved: HashMap::new(),
            texts: HashMap::new(),
            messages: HashMap::new(),
            counts: HashMap::new(),
            tabs: HashMap::new(),
        }
    }

    /// A write to `route` landed in the replica.
    pub(crate) fn landed(&mut self, route: &DocumentRoute) {
        self.landings = self.landings.wrapping_add(1);
        self.arrived.insert(route.clone(), self.landings);
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
    }

    /// What search finds in the document at `route`, read off the serving thread by the
    /// index's own: kept at the revision it was read from, and given back to the app's next
    /// read of it without opening the document again.
    pub(crate) fn keep_entries(
        &mut self,
        route: &DocumentRoute,
        revision: [u8; 32],
        entries: Arc<[IndexEntry]>,
    ) {
        self.entries.insert(route.clone(), (revision, entries));
    }

    /// How many writes have landed in the document at `route`: the index reads it again when
    /// that moved since it read it.
    pub(crate) fn moved(&self, route: &DocumentRoute) -> u64 {
        self.moved.get(route).copied().unwrap_or(0)
    }

    /// The replica the workspace's documents are read from, for the index's own thread.
    pub(crate) fn engine(&self) -> Arc<Engine> {
        Arc::clone(&self.engine)
    }

    /// The workspace's id.
    pub(crate) fn workspace(&self) -> &str {
        &self.workspace
    }

    /// How many writes landed in the workspace: a revision of it as a whole.
    pub(crate) const fn landings(&self) -> u64 {
        self.landings
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

    /// Up to `limit` of the workspace's documents search keeps no entries for: the warm thread
    /// reads them, so a query never has to.
    pub(crate) fn unindexed(&self, limit: usize) -> Vec<DocumentRoute> {
        self.routes()
            .into_iter()
            .filter(|route| self.indexes(route))
            .take(limit)
            .collect()
    }

    /// Whether any of the workspace's documents waits to be read for the index.
    pub(crate) fn has_unindexed(&self) -> bool {
        self.routes().into_iter().any(|route| self.indexes(&route))
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
        match self.engine.records(&self.workspace) {
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

    /// The records, those people have open on the desktop first, then the
    /// most recently changed.
    pub(crate) fn people_first(&mut self) -> Vec<DocumentRoute> {
        let known = self.routes();
        let tabs = self.desktop_tabs();
        people_first(known, &tabs, &self.arrived)
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

    /// The record at `route`, saved, as the replica holds it now. `None` when
    /// the replica holds no such record or cannot open it yet.
    pub(crate) fn document(&self, route: &DocumentRoute) -> Option<Vec<u8>> {
        match self
            .engine
            .record(&route.cluster_id, &route.app_id, &route.document)
        {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::debug!(%error, record = %route.document, "agent: a record does not read");
                None
            }
        }
    }

    /// What a replica of `route` is seeded from: its document, saved. `None`
    /// when the workspace has no such record; `Err` when it has one this
    /// member cannot open yet.
    pub(crate) fn seed(&self, route: &DocumentRoute) -> Option<Result<Vec<u8>, String>> {
        if let Some(bytes) = self.document(route) {
            return Some(Ok(bytes));
        }
        self.routes().contains(route).then(|| {
            Err(crate::apps::AppError::Merge(format!(
                "the keys to record {} have not arrived yet",
                route.document
            ))
            .to_string())
        })
    }

    /// `read` of the record at `route` (`T` kept in `field`), again only once
    /// it changed since. `None` for a record the replica does not hold or
    /// cannot open yet.
    fn cached<T: Clone>(
        &mut self,
        route: &DocumentRoute,
        field: fn(&mut Self) -> &mut Read<T>,
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
        field: fn(&mut Self) -> &mut Read<T>,
        read: impl FnOnce(Vec<u8>) -> T,
    ) -> Option<(T, bool)> {
        let known = field(self).get(route).map(|(revision, _)| *revision);
        match self
            .engine
            .record_since(&route.cluster_id, &route.app_id, &route.document, known)
        {
            Ok(Some((bytes, revision))) => {
                let value = read(bytes);
                field(self).insert(route.clone(), (revision, value.clone()));
                Some((value, true))
            }
            Ok(None) => field(self)
                .get(route)
                .map(|(_, value)| (value.clone(), false)),
            Err(error) => {
                tracing::debug!(%error, record = %route.document, "agent: a record does not read");
                None
            }
        }
    }

    /// What the record at `route` is called, as `read` finds it in the
    /// record's saved document. Read once per change of the record.
    pub(crate) fn title(
        &mut self,
        route: &DocumentRoute,
        read: impl FnOnce(Vec<u8>) -> Result<Option<String>, String>,
    ) -> Option<String> {
        self.cached(
            route,
            |records| &mut records.titles,
            |bytes| {
                read(bytes).unwrap_or_else(|reason| {
                    tracing::warn!(%reason, "agent: a record's title could not be read");
                    None
                })
            },
        )
        .flatten()
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
        self.cached_telling(
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
        .map_or((None, false), |(entries, read)| (Some(entries), read))
    }

    fn read_texts(&mut self, reading: Reading) -> &mut HashMap<DocumentRoute, (Stamp, Arc<str>)> {
        match reading {
            Reading::Plain => &mut self.texts,
            Reading::Messages => &mut self.messages,
        }
    }

    /// The text of the record at `route`, read as `reading` says, once per
    /// change of it.
    pub(crate) fn text(&mut self, route: &DocumentRoute, reading: Reading) -> Option<Arc<str>> {
        let held = self.read_texts(reading).get(route).cloned();
        let known = match held {
            Some((Stamp::Replica(revision), _)) => Some(revision),
            _ => None,
        };
        match self
            .engine
            .record_since(&route.cluster_id, &route.app_id, &route.document, known)
        {
            Ok(Some((bytes, revision))) => {
                let text = reading.read(&load(&bytes)?);
                self.read_texts(reading)
                    .insert(route.clone(), (Stamp::Replica(revision), Arc::clone(&text)));
                Some(text)
            }
            Ok(None) => known.and(held).map(|(_, text)| text),
            Err(error) => {
                tracing::debug!(%error, record = %route.document, "agent: a record does not read");
                None
            }
        }
    }

    /// The text of the record at `route` as `reading` last read it, however
    /// it changed since.
    pub(crate) fn last_read(
        &mut self,
        route: &DocumentRoute,
        reading: Reading,
    ) -> Option<Arc<str>> {
        self.read_texts(reading)
            .get(route)
            .map(|(_, text)| Arc::clone(text))
    }

    /// `read` of the document of the record at `route`. `None` for a record
    /// the replica does not hold or cannot open yet.
    pub(crate) fn read<T>(
        &self,
        route: &DocumentRoute,
        read: impl FnOnce(&AutoCommit) -> T,
    ) -> Option<T> {
        load(&self.document(route)?).map(|doc| read(&doc))
    }

    /// The text of `replica`, the open record at `route`, read as `reading`
    /// says, again only once it changed.
    pub(crate) fn open_text(
        &mut self,
        route: &DocumentRoute,
        replica: &crate::worker::ConfinedReplica,
        reading: Reading,
    ) -> Option<Arc<str>> {
        replica.read(|doc| self.held_text(route, doc, reading))
    }

    /// The text of the part at `route`, held beside its open record in
    /// `holder`, read as `reading` says, again only once it changed. `None`
    /// when `holder` does not hold it.
    pub(crate) fn part_text(
        &mut self,
        route: &DocumentRoute,
        holder: &crate::worker::ConfinedReplica,
        reading: Reading,
    ) -> Option<Arc<str>> {
        let record = ui::resource::DocumentId::parse(&route.document)?;
        holder.read_resource(&record, |doc| self.held_text(route, doc, reading))
    }

    fn held_text(&mut self, route: &DocumentRoute, doc: &AutoCommit, reading: Reading) -> Arc<str> {
        let stats = doc.stats();
        let stamp = Stamp::Open(stats.num_changes, stats.num_ops);
        let texts = self.read_texts(reading);
        match texts.get(route) {
            Some((at, text)) if *at == stamp => Arc::clone(text),
            _ => {
                let text = reading.read(doc);
                texts.insert(route.clone(), (stamp, Arc::clone(&text)));
                text
            }
        }
    }

    /// How many changes the record at `route` holds: whether anyone wrote in
    /// it since its genesis.
    pub(crate) fn changes(&mut self, route: &DocumentRoute) -> Option<u64> {
        self.cached(
            route,
            |records| &mut records.counts,
            |bytes| load(&bytes).map(|doc| doc.stats().num_changes),
        )
        .flatten()
    }

    /// Drop the texts of records `keep` no longer holds.
    pub(crate) fn keep_texts(&mut self, keep: impl Fn(&DocumentRoute) -> bool) {
        self.texts.retain(|route, _| keep(route));
    }

    /// When the record at `route` last changed, as a count of landings
    /// (higher is later); `None` for a record nothing landed in since this
    /// session started.
    pub(crate) fn changed(&self, route: &DocumentRoute) -> Option<u64> {
        self.arrived.get(route).copied()
    }

    /// Stop listing `route`: an open replica holds it now.
    pub(crate) fn forget(&mut self, route: &DocumentRoute) {
        self.titles.remove(route);
        self.entries.remove(route);
        self.counts.remove(route);
        self.opened.insert(route.clone());
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
