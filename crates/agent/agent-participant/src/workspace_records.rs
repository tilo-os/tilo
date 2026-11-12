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

/// The route of a workspace's shared browser desktop (`tilo-browser-runtime`'s
/// replication): its `desktop` value names each tab's record.
const DESKTOP_APP: &str = "so.tilo.browser-desktop";
const DESKTOP_RECORD: &str = "desktop";

pub(crate) fn desktop_route(workspace: &str) -> DocumentRoute {
    DocumentRoute {
        app_id: DESKTOP_APP.to_owned(),
        cluster_id: workspace.to_owned(),
        record: DESKTOP_RECORD.to_owned(),
    }
}

/// The records bound to a tab of a shared desktop `doc`: what people have
/// open. Reads its current value only.
fn desktop_records(doc: &AutoCommit) -> HashSet<String> {
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
pub(crate) struct WorkspaceRecords {
    engine: Arc<Engine>,
    workspace: String,
    /// Records an open replica holds now: not listed here.
    opened: HashSet<DocumentRoute>,
    /// When each record last changed, as a count of landings seen.
    arrived: HashMap<DocumentRoute, u64>,
    landings: u64,
    titles: Read<Option<String>>,
    /// The plain text of each record the planner searches, and of open records
    /// ([`Self::open_text`]).
    texts: HashMap<DocumentRoute, (Stamp, Arc<str>)>,
    /// The messages of each message record news and waits read.
    messages: HashMap<DocumentRoute, (Stamp, Arc<str>)>,
    counts: Read<Option<u64>>,
    tabs: Read<Arc<HashSet<String>>>,
}

impl WorkspaceRecords {
    pub(crate) fn new(workspace: &str, engine: Arc<Engine>) -> Self {
        Self {
            engine,
            workspace: workspace.to_owned(),
            opened: HashSet::new(),
            arrived: HashMap::new(),
            landings: 0,
            titles: HashMap::new(),
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
    }

    /// The workspace's id.
    pub(crate) fn workspace(&self) -> &str {
        &self.workspace
    }

    /// How many writes landed in the workspace: a revision of it as a whole.
    pub(crate) const fn landings(&self) -> u64 {
        self.landings
    }

    /// The records of the workspace not held open.
    pub(crate) fn routes(&self) -> Vec<DocumentRoute> {
        match self.engine.records(&self.workspace) {
            Ok(records) => records
                .into_iter()
                .map(|(app_id, record)| DocumentRoute {
                    app_id,
                    cluster_id: self.workspace.clone(),
                    record,
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
                        .map(|doc| desktop_records(&doc))
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
            .record(&route.cluster_id, &route.app_id, &route.record)
        {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::debug!(%error, record = %route.record, "agent: a record does not read");
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
                route.record
            ))
            .to_string())
        })
    }

    /// `read` of the record at `route` (`T` kept in `slot`), again only once
    /// it changed since. `None` for a record the replica does not hold or
    /// cannot open yet.
    fn cached<T: Clone>(
        &mut self,
        route: &DocumentRoute,
        slot: fn(&mut Self) -> &mut Read<T>,
        read: impl FnOnce(Vec<u8>) -> T,
    ) -> Option<T> {
        let known = slot(self).get(route).map(|(revision, _)| *revision);
        match self
            .engine
            .record_since(&route.cluster_id, &route.app_id, &route.record, known)
        {
            Ok(Some((bytes, revision))) => {
                let value = read(bytes);
                slot(self).insert(route.clone(), (revision, value.clone()));
                Some(value)
            }
            Ok(None) => slot(self).get(route).map(|(_, value)| value.clone()),
            Err(error) => {
                tracing::debug!(%error, record = %route.record, "agent: a record does not read");
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
            .record_since(&route.cluster_id, &route.app_id, &route.record, known)
        {
            Ok(Some((bytes, revision))) => {
                let text = reading.read(&load(&bytes)?);
                self.read_texts(reading)
                    .insert(route.clone(), (Stamp::Replica(revision), Arc::clone(&text)));
                Some(text)
            }
            Ok(None) => known.and(held).map(|(_, text)| text),
            Err(error) => {
                tracing::debug!(%error, record = %route.record, "agent: a record does not read");
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
        let record = ui::record::RecordId::parse(&route.record)?;
        holder.read_record(&record, |doc| self.held_text(route, doc, reading))
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
            record: record.into(),
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
            .map(|route| route.record)
            .collect();
        assert_eq!(ordered, ["c", "b", "a"]);
    }

    #[test]
    fn the_shared_desktop_names_its_tabs_records() {
        use tilo_desktop_model::{Strip, StripId, Tab, TileContent, WindowGroup, Workspace};
        let mut workspace = Workspace::new(tilo_desktop_model::WorkspaceSlot(1), "Workspace");
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
            tree: tilo_desktop_model::DesktopTree {
                workspaces: vec![workspace],
                ..tilo_desktop_model::DesktopTree::default()
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
        tilo_desktop_model::shared_record::write(&mut doc, &desktop).unwrap();
        let records = desktop_records(&doc);
        assert!(records.contains("rec-open"));
        assert_eq!(records.len(), 20);
    }
}
