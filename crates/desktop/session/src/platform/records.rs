//! What a role reads of the workspaces' records, and what changed in them.
//!
//! The workspaces' records are written by their apps, by the person's other devices and by this
//! session's own services. Whoever writes one notes it in the one [`Changes`] stream of this host,
//! and the roles that follow records — the activity, the person's mentions and search — read that
//! stream and keep no watcher of their own. The records are this device's workspace service's
//! ([`Held`]), and the record index notes what the broker receives from the apps and what the
//! session writes.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};

use automerge::AutoCommit;

/// A record a change reached: whose app keeps it, in which workspace, and its route
/// ([`ui::resource::DocumentId::route`]).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Changed {
    pub workspace: String,
    pub app: String,
    /// The record's route; empty for the app's own record of the workspace, the document that
    /// also keeps its list of the app's records.
    pub record: String,
    /// What reached it is what this device already held — the workspace replaying its records
    /// before any role followed them — rather than a live change: no news.
    pub replayed: bool,
}

/// Whether this device holds a workspace, as the stream is told: a change of one it does not hold
/// is nothing any role following the stream can read.
type Holds = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// The one stream of record changes: every write that lands in the workspaces' records is noted
/// here, and every role that follows records takes it.
#[derive(Clone, Default)]
pub struct Changes(Arc<Inner>);

#[derive(Default)]
struct Inner {
    /// The records a change reached since this was last taken, each with whether what reached it
    /// is what the device already held.
    reached: Mutex<BTreeMap<(String, String, String), bool>>,
    /// How often the records of each workspace moved.
    moved_in: Mutex<BTreeMap<String, u64>>,
    /// How often any record moved.
    moved: AtomicU64,
    /// What wakes the session's loop when a record changed, so the pass reads the queue then.
    wake: Mutex<Option<tilo_service::Wake>>,
    /// The workers following the records, told each change as it is noted.
    readers: Mutex<Vec<mpsc::Sender<Changed>>>,
    /// Whether this device holds a workspace, where the session knows: what [`Changes::holding`]
    /// was told.
    holds: Mutex<Option<Holds>>,
    /// What `holds` answered for each workspace, so a change of one asks it nothing until the keys
    /// settle.
    answered: Mutex<BTreeMap<String, bool>>,
}

impl Changes {
    /// Note only what `holds` holds: a change of a workspace this device's service does not hold
    /// is no news here, and is taken when the workspace becomes held.
    pub fn holding(&self, holds: Holds) {
        *self.0.holds.lock().unwrap_or_else(PoisonError::into_inner) = Some(holds);
        self.0
            .answered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// The keys settled: a workspace this device held nothing of may be held now, so what
    /// [`Self::holding`] answered of one it does not hold is asked again.
    pub fn settling(&self) {
        self.0
            .answered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|_, held| *held);
    }

    /// Whether this device holds `workspace`, as [`Self::holding`] says: true where the session
    /// told the stream nothing, as a page's does, which notes only what it keeps.
    fn holds(&self, workspace: &str) -> bool {
        let asked = self
            .0
            .holds
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let Some(asked) = asked else {
            return true;
        };
        if let Some(held) = self
            .0
            .answered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(workspace)
        {
            return *held;
        }
        let held = asked(workspace);
        self.0
            .answered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(workspace.to_owned(), held);
        held
    }

    /// Note that `record` of `app` changed in `workspace` (an empty route: the workspace's list
    /// of `app`'s moved), as `replayed` where it is what this device already held, and wake what
    /// follows it. Whether it is news here: a change of a workspace this device does not hold is
    /// not, and nothing that follows the stream is told of it.
    pub fn note(&self, workspace: &str, app: &str, record: &str, replayed: bool) -> bool {
        if app.is_empty() || !self.holds(workspace) {
            return false;
        }
        // A record noted twice before it is read is read once, and a live change of it is news even
        // where the replay reached it first.
        let route = (workspace.to_owned(), app.to_owned(), record.to_owned());
        let mut reached = self
            .0
            .reached
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let held = reached.entry(route).or_insert(replayed);
        *held &= replayed;
        drop(reached);
        let mut moved_in = self
            .0
            .moved_in
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let moved = moved_in.entry(workspace.to_owned()).or_default();
        *moved = moved.saturating_add(1);
        drop(moved_in);
        self.0.moved.fetch_add(1, Ordering::AcqRel);
        let changed = Changed {
            workspace: workspace.to_owned(),
            app: app.to_owned(),
            record: record.to_owned(),
            replayed,
        };
        self.0
            .readers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|reader| reader.send(changed.clone()).is_ok());
        if let Some(wake) = self
            .0
            .wake
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            wake();
        }
        true
    }

    /// Wake the session's loop through `wake` whenever a record changed.
    pub fn waking(&self, wake: tilo_service::Wake) {
        *self.0.wake.lock().unwrap_or_else(PoisonError::into_inner) = Some(wake);
    }

    /// What reached the records since this was last taken.
    pub fn take(&self) -> Vec<Changed> {
        std::mem::take(
            &mut *self
                .0
                .reached
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
        .into_iter()
        .map(|((workspace, app, record), replayed)| Changed {
            workspace,
            app,
            record,
            replayed,
        })
        .collect()
    }

    /// How often the records of `workspace` moved.
    pub fn moved_in(&self, workspace: &str) -> u64 {
        self.0
            .moved_in
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(workspace)
            .copied()
            .unwrap_or_default()
    }

    /// How often any record moved.
    pub fn moved(&self) -> u64 {
        self.0.moved.load(Ordering::Acquire)
    }

    /// A receiver told every change noted from now on: what a worker following the records waits
    /// on.
    #[cfg(target_os = "linux")]
    pub fn reader(&self) -> mpsc::Receiver<Changed> {
        let (told, reader) = mpsc::channel();
        self.0
            .readers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(told);
        reader
    }
}

/// What a value read of the records follows: the records of one workspace, or every record.
#[derive(Clone, Debug)]
pub enum Watch {
    /// The records of this workspace, by its id.
    Workspace(String),
    /// Every record of every workspace: a value read across the device's records.
    Any,
}

impl Watch {
    /// Whether a change noted in `workspace` is one this watch follows: what the worker that reads
    /// it again listens for.
    #[cfg(target_os = "linux")]
    fn follows(&self, workspace: &str) -> bool {
        match self {
            Self::Workspace(watched) => watched == workspace,
            Self::Any => true,
        }
    }

    /// How often what this watch follows moved.
    fn moved(&self, changes: &Changes) -> u64 {
        match self {
            Self::Workspace(workspace) => changes.moved_in(workspace),
            Self::Any => changes.moved(),
        }
    }
}

/// A value read of the workspaces' records, kept current by the change stream: read when it is
/// started, and again whenever what it follows moves — on a worker of its own where this host has
/// threads, so a read never holds up a frame, and on the one thread the page has where it has one.
pub struct Reading<T> {
    #[cfg(target_os = "linux")]
    held: Arc<Mutex<(u64, Arc<T>)>>,
    #[cfg(target_os = "linux")]
    landed: Arc<AtomicU64>,
    #[cfg(not(target_os = "linux"))]
    held: std::cell::RefCell<(u64, Arc<T>)>,
    #[cfg(not(target_os = "linux"))]
    read: Box<dyn Fn() -> T>,
}

#[cfg(target_os = "linux")]
impl<T: Send + Sync + 'static> Reading<T> {
    /// Read `read` now, and again on a worker whenever `changes` says what `watch` follows moved,
    /// telling `moved` so the frame that shows it is drawn.
    pub fn start(
        name: &str,
        changes: Changes,
        watch: Watch,
        read: impl Fn() -> T + Send + Sync + 'static,
        moved: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let at = watch.moved(&changes);
        let held = Arc::new(Mutex::new((at, Arc::new(read()))));
        let landed = Arc::new(AtomicU64::new(1));
        let reader = changes.reader();
        let (shared, counted) = (Arc::clone(&held), Arc::clone(&landed));
        let worker = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                // Ends when the stream does: the session is gone.
                while let Ok(changed) = reader.recv() {
                    // A burst of changes is read once.
                    let mut mine = watch.follows(&changed.workspace);
                    while let Ok(other) = reader.try_recv() {
                        mine |= watch.follows(&other.workspace);
                    }
                    if !mine {
                        continue;
                    }
                    let at = watch.moved(&changes);
                    let fresh = Arc::new((read)());
                    *shared.lock().unwrap_or_else(PoisonError::into_inner) = (at, fresh);
                    counted.fetch_add(1, Ordering::AcqRel);
                    moved();
                }
            });
        if let Err(error) = worker {
            tracing::warn!(%error, "a value read of the records is not followed");
        }
        Self { held, landed }
    }

    /// The value as this host last read it: the worker keeps it current.
    pub fn get(&self) -> Arc<T> {
        Arc::clone(&self.held.lock().unwrap_or_else(PoisonError::into_inner).1)
    }

    /// [`Self::get`] for a caller that also reads a thread with one: the same value here.
    pub fn following(&self, _changes: &Changes, _watch: &Watch) -> Arc<T> {
        self.get()
    }

    /// How many reads landed: what a caller hashes to hear that a read changed the value.
    pub fn reads(&self) -> u64 {
        self.landed.load(Ordering::Acquire)
    }
}

#[cfg(not(target_os = "linux"))]
impl<T: 'static> Reading<T> {
    /// Read `read` now: this host has one thread, and reads the records on it.
    pub fn read(changes: &Changes, watch: &Watch, read: impl Fn() -> T + 'static) -> Self {
        Self {
            held: std::cell::RefCell::new((watch.moved(changes), Arc::new(read()))),
            read: Box::new(read),
        }
    }

    /// The value, read again where the stream says what `watch` follows moved: this host has one
    /// thread, and nothing may hold it.
    pub fn following(&self, changes: &Changes, watch: &Watch) -> Arc<T> {
        let at = watch.moved(changes);
        let mut held = self.held.borrow_mut();
        if held.0 != at {
            held.0 = at;
            held.1 = Arc::new((self.read)());
        }
        Arc::clone(&held.1)
    }

    /// The value as last read.
    pub fn get(&self) -> Arc<T> {
        Arc::clone(&self.held.borrow().1)
    }

    /// How often what `watch` follows moved when this was last read.
    pub fn reads(&self) -> u64 {
        self.held.borrow().0
    }
}

/// What changed of a record since the heads a reader holds it at, as a host reads it without the
/// record whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Since {
    /// It stands where the reader last read it.
    Unchanged,
    /// These encoded changes, after which it stands at `heads`.
    Changes {
        heads: Vec<automerge::ChangeHash>,
        bytes: Vec<u8>,
    },
    /// Only the record whole answers: the host does not hold the reader's heads, or reads no
    /// changes apart.
    Whole,
}

/// What a host reads of its records.
pub trait Records {
    /// The records a change reached since this was last taken.
    fn take_changed(&self) -> Vec<Changed>;
    /// The document of `app`'s `record` (a route) in `workspace`, as this host holds it now.
    fn document(&self, app: &str, workspace: &str, record: &str) -> Option<AutoCommit>;
    /// What changed of `app`'s `record` in `workspace` since `heads`, read without the record
    /// whole where this host can.
    fn since(
        &self,
        app: &str,
        workspace: &str,
        record: &str,
        heads: &[automerge::ChangeHash],
    ) -> Since {
        let _ = (app, workspace, record, heads);
        Since::Whole
    }
    /// The heads `app`'s `record` in `workspace` stands at here, where this host knows them
    /// without reading the record.
    fn heads(
        &self,
        app: &str,
        workspace: &str,
        record: &str,
    ) -> Option<Vec<automerge::ChangeHash>> {
        let _ = (app, workspace, record);
        None
    }
    /// Everything `workspace` holds here, by app and route.
    fn held(&self, workspace: &str) -> Vec<(String, String)> {
        let _ = workspace;
        Vec::new()
    }
    /// The workspaces this host holds records of.
    fn workspaces(&self) -> Vec<String> {
        Vec::new()
    }
}

/// What a role that reads on a worker of its own reads the workspaces through.
#[cfg(target_os = "linux")]
pub type Source = Arc<dyn Records + Send + Sync>;
/// What a role reads the workspaces through, where the session has one thread.
#[cfg(not(target_os = "linux"))]
pub type Source = std::rc::Rc<dyn Records>;

/// Records held in memory, for a test that writes a workspace's records as its apps do and notes
/// each change itself.
/// A clone holds the same records.
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct Memory {
    /// Each record by its app, workspace and route; the empty route is the app's own record of
    /// the workspace, its index.
    held: Arc<Mutex<BTreeMap<(String, String, String), Vec<u8>>>>,
    changes: Changes,
}

#[cfg(test)]
impl Memory {
    /// Records whose changes `changes` notes.
    pub(crate) fn over(changes: Changes) -> Self {
        Self {
            held: Arc::default(),
            changes,
        }
    }

    /// Hold `doc` as `app`'s record `route` of `workspace`, as its app last wrote it.
    pub(crate) fn keep(&self, app: &str, workspace: &str, route: &str, doc: &mut AutoCommit) {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                (app.to_owned(), workspace.to_owned(), own_route(route)),
                doc.save(),
            );
    }
}

/// The route a record is held under here: the empty one for the app's own record.
#[cfg(test)]
fn own_route(route: &str) -> String {
    match ui::resource::DocumentId::from_route(route) {
        Some(id) if id.is_package_primary() => String::new(),
        _ => route.to_owned(),
    }
}

#[cfg(test)]
impl Records for Memory {
    fn take_changed(&self) -> Vec<Changed> {
        self.changes.take()
    }

    fn document(&self, app: &str, workspace: &str, route: &str) -> Option<AutoCommit> {
        let held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let bytes = held.get(&(app.to_owned(), workspace.to_owned(), own_route(route)))?;
        AutoCommit::load(bytes).ok()
    }

    fn held(&self, workspace: &str) -> Vec<(String, String)> {
        let held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        held.keys()
            .filter(|(_, held_in, _)| held_in == workspace)
            .map(|(app, _, route)| (app.clone(), route.clone()))
            .collect()
    }

    fn workspaces(&self) -> Vec<String> {
        let held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let mut workspaces: Vec<String> = held
            .keys()
            .map(|(_, workspace, _)| workspace.clone())
            .collect();
        workspaces.sort();
        workspaces.dedup();
        workspaces
    }
}

#[cfg(target_os = "linux")]
pub struct Held {
    reads: Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
    changes: Changes,
}

#[cfg(target_os = "linux")]
impl Held {
    /// What `reads` answers, with `changes` saying what moved in it.
    #[must_use]
    pub fn over(
        reads: Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
        changes: Changes,
    ) -> Self {
        Self { reads, changes }
    }

    /// What `app`'s document at `route` in `workspace` holds since `heads`: all of it without
    /// heads. Nothing while its keys are not here yet, and nothing of a workspace this device's
    /// service does not hold: this device's own, where an app launched outside every workspace
    /// keeps what it writes, stays with that app.
    fn read(
        &self,
        (app, workspace, route): (&str, &str, &str),
        heads: Option<&[automerge::ChangeHash]>,
    ) -> Option<tilo_peer_state::workspace_documents::DocumentRead> {
        if !self.reads.workspaces().iter().any(|held| held == workspace) {
            return None;
        }
        let heads: Option<Vec<[u8; 32]>> =
            heads.map(|heads| heads.iter().map(|head| head.0).collect());
        self.reads
            .read(workspace, app, route, heads.as_deref())
            .inspect_err(
                |error| tracing::warn!(%error, %app, %workspace, %route, "a document is not read"),
            )
            .ok()
    }
}

#[cfg(target_os = "linux")]
impl Records for Held {
    fn take_changed(&self) -> Vec<Changed> {
        self.changes.take()
    }

    fn document(&self, app: &str, workspace: &str, route: &str) -> Option<AutoCommit> {
        let Some(tilo_peer_state::workspace_documents::DocumentRead::Whole { bytes, .. }) =
            self.read((app, workspace, route), None)
        else {
            return None;
        };
        AutoCommit::load(&bytes)
            .inspect_err(|error| tracing::warn!(%error, %app, %route, "a document does not load"))
            .ok()
    }

    fn since(
        &self,
        app: &str,
        workspace: &str,
        route: &str,
        heads: &[automerge::ChangeHash],
    ) -> Since {
        use tilo_peer_state::workspace_documents::DocumentRead;
        match self.read((app, workspace, route), Some(heads)) {
            Some(DocumentRead::Unchanged(_)) => Since::Unchanged,
            Some(DocumentRead::Changes { heads, bytes, .. }) => Since::Changes {
                heads: heads.into_iter().map(automerge::ChangeHash).collect(),
                bytes,
            },
            _ => Since::Whole,
        }
    }

    fn held(&self, workspace: &str) -> Vec<(String, String)> {
        self.reads
            .listing(workspace)
            .inspect_err(|error| tracing::warn!(%error, %workspace, "a workspace is not listed"))
            .unwrap_or_default()
    }

    fn workspaces(&self) -> Vec<String> {
        self.reads.workspaces()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A change of a workspace this device's service does not hold is nothing a role that
    /// follows records can read: it is not noted, moves nothing, tells no reader and wakes no
    /// loop. The workspaces the service holds are noted as before.
    #[test]
    fn a_change_of_a_workspace_this_device_does_not_hold_is_noted_to_nobody() {
        let changes = Changes::default();
        let asked = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&asked);
        changes.holding(Arc::new(move |workspace: &str| {
            flag.store(true, Ordering::SeqCst);
            workspace == "held"
        }));
        let woke = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&woke);
        changes.waking(Arc::new(move || flag.store(true, Ordering::SeqCst)));
        #[cfg(target_os = "linux")]
        let reader = changes.reader();

        assert!(
            !changes.note("unheld", "so.tilo.chat", "messages", false),
            "a change of an unheld workspace is no news here"
        );
        assert!(asked.load(Ordering::SeqCst), "the stream asks what is held");
        assert_eq!(changes.moved(), 0, "nothing moved");
        assert_eq!(changes.moved_in("unheld"), 0);
        assert!(changes.take().is_empty(), "no role has anything to take");
        assert!(
            !woke.load(Ordering::SeqCst),
            "the session's loop is not woken"
        );
        #[cfg(target_os = "linux")]
        assert!(
            reader.try_recv().is_err(),
            "no reader following the records is told"
        );

        assert!(changes.note("held", "so.tilo.chat", "messages", false));
        assert_eq!(changes.moved(), 1);
        assert_eq!(changes.moved_in("held"), 1);
        assert!(woke.load(Ordering::SeqCst), "the loop reads the change");
        #[cfg(target_os = "linux")]
        assert_eq!(
            reader
                .recv()
                .map(|changed| changed.workspace)
                .ok()
                .as_deref(),
            Some("held")
        );
    }
}
