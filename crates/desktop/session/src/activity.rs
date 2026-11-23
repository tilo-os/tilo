//! The workspace's activity: the workspaces' layout records and the records its apps keep in the
//! engine, served to the Activity app and to presence.
//! A record is read away from the loop ([`crate::platform::records::Reading`]) and a change asks the session for
//! the frame that lists it; what this device has seen of a workspace is its own ([`HistoryMaps`]) and
//! never reaches a shared record.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// The page runs this too, where std's clock does not exist: time is read
// through the toolkit's.
use ui::time::Instant;

use durable_store::{DurableStore, StorageKey, WriteOp};
use serde_json::Value;
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_lock::MutexRecover as _;
use tilo_session_core::desktop::activity::describer::{Describer, Job};
use tilo_session_core::desktop::activity::{
    ActivityHost, ActivitySnapshot, DetachedRecordHistory, HistoryOrigin, TaskRunDocument,
    TaskRunUndo, WorkspaceDeparture, WorkspaceSeenFrontier, morning,
};
#[cfg(target_os = "linux")]
use tilo_session_core::desktop::history_log;
use tilo_session_core::desktop::session::{ActivityRequests, activity_requests};
use tilo_session_core::desktop::state::HistorySource;

use crate::activity_provider::ActivityWire;

/// The workspace engine's reading of the signed values a workspace's governance keeps, for its
/// history: what each membership operation did, and what each device row names its device.
pub struct GovernanceValues;

impl tilo_session_core::desktop::activity::GovernanceReader for GovernanceValues {
    fn membership(
        &self,
        op: &[u8],
    ) -> Option<tilo_session_core::desktop::activity::MembershipWrite> {
        use tilo_peer_state::members::{MembershipOpCommand, SignedMembershipOp};
        use tilo_session_core::desktop::activity::MembershipWrite;
        let (_, op) = SignedMembershipOp::verified_stored(op).ok()?;
        Some(match &op.command {
            MembershipOpCommand::Project { changes } => MembershipWrite::Changes(changes.clone()),
            MembershipOpCommand::Admit { person, .. } => MembershipWrite::Admitted {
                person: hex::encode(person),
            },
            MembershipOpCommand::Governance { .. } => MembershipWrite::Recorded,
        })
    }

    fn device_label(&self, row: &[u8]) -> Option<String> {
        tilo_peer_state::governance::device_row_label(row)
    }
}
#[cfg(target_os = "linux")]
use crate::device_state::DeviceState;
#[cfg(target_os = "linux")]
use tilo_workspace_service::apps::collab_app_broker::AppBrokerState;

use crate::platform::Store;
use crate::platform::apps::Apps;
use crate::platform::authors::{People, Writers};
use crate::platform::pane::Pane;
use crate::platform::records::{Records as Reads, Source};
#[cfg(target_os = "linux")]
use tilo_actions::remote::workspace_activity as wire;
use ui::resource::{DocumentCheckout, DocumentId, ResourceType};

#[cfg(target_os = "linux")]
use crate::platform::fabric::SharedLink;
#[cfg(target_os = "linux")]
use crate::unlock_history::UnlockHistory;

/// The session's action broker and the generation of its published actions History last learned.
#[cfg(target_os = "linux")]
type LearnedBroker = (Arc<AppBrokerState>, Cell<Option<u64>>);

/// The package this device's view of the activity is kept under: the session's.
const VIEW_PACKAGE: &str = "so.tilo.session.activity";

/// Seconds since the epoch, as a change of the session names its time.
#[cfg(target_os = "linux")]
pub(crate) fn now_seconds() -> i64 {
    ui::time::wall()
        .duration_since(ui::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

/// One device's lasting refusal: its workspace, the device, the class of its reason.
#[cfg(target_os = "linux")]
type ToldRefusal = (String, [u8; 32], neighbour_wire::RefusalReason);

/// Where the pane reads the history of a layout, and writes what a model says of its changes: the
/// workspaces' records.
pub trait History {
    /// The history of the layout in the workspace `workspace_id` of `tree`.
    fn history(&self, tree: &Layout, workspace_id: WorkspaceId) -> Option<ActivitySnapshot>;
    /// The layout of each workspace `tree` shows, as this host holds it.
    fn documents(&self, tree: &Layout) -> Vec<(WorkspaceId, automerge::AutoCommit)>;
    /// Write what `write` changes in the layout of `workspace_id`. Whether anything was written.
    fn describe(
        &self,
        tree: &Layout,
        workspace_id: WorkspaceId,
        write: &mut dyn FnMut(&mut automerge::AutoCommit) -> bool,
    ) -> bool;
    /// A number that moves whenever the session's layout does.
    fn moved(&self) -> u64;
    /// Read the layouts through `reads`, and write them through `writes`, once this session
    /// opened both: the documents of the workspaces the session holds. Only a device's session
    /// has them; a page keeps its layout in its own document.
    #[cfg(target_os = "linux")]
    fn attach(
        &self,
        certificate: &tilo_user_identity::DeviceCert,
        view: &std::sync::Arc<crate::workspace_view::WorkspaceView>,
        reads: &std::sync::Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
        writes: &std::sync::Arc<dyn tilo_peer_state::workspace_documents::DeviceWrites>,
    );
}

pub struct Given {
    /// Where the session's own history is read from and written to.
    pub layouts: Box<dyn History>,
    /// Who sits here.
    pub person: String,
    pub store: Option<Store>,
    /// What the workspaces hold; a native session reads it through its engine once it opened it.
    pub held: Option<Source>,
    pub state: Option<PathBuf>,
    pub apps: Option<Arc<dyn Apps>>,
    /// Who an actor is, as presence shows them.
    pub people: People,
    pub writers: Writers,
    pub pane: Rc<dyn Pane>,
    /// The model the session's changes are described by, while this host answers one.
    pub models: Option<Arc<dyn crate::platform::Models>>,
    /// The one stream of record changes this host feeds.
    pub changes: crate::platform::records::Changes,
    pub wake: Option<tilo_service::Wake>,
}

/// A workspace's activity, as this session serves it.
#[derive(Clone)]
pub struct Activity(Rc<Inner>);

struct Inner {
    /// Where the session's own history is read from and written to.
    layouts: Box<dyn History>,
    /// What this device shows: the workspace in front, and the displays' partition.
    #[cfg(target_os = "linux")]
    shown: DeviceState,
    /// What the devices answered when one asked to be unlocked.
    #[cfg(target_os = "linux")]
    unlock: UnlockHistory,
    /// Who sits here.
    person: String,
    maps: HistoryMaps,
    apps: Option<Arc<dyn Apps>>,
    /// Who an actor is, as presence shows them: the names the devices that answered a boot go
    /// by.
    #[cfg(target_os = "linux")]
    people: People,
    writers: Writers,
    /// Where the activity model's code is.
    pane: Rc<dyn Pane>,
    /// The model the session's changes are described by, while this host answers one.
    models: Option<Arc<dyn crate::platform::Models>>,
    /// The workspaces this device holds, once the session opened them.
    #[cfg(target_os = "linux")]
    view: RefCell<Option<Arc<crate::workspace_view::WorkspaceView>>>,
    /// The members of each workspace, by workspace: its people named as the bar and People name
    /// them, its agents as its record of them does, and the actors each person's devices write as.
    people_of: RefCell<HashMap<WorkspaceId, Members>>,
    /// Moves whenever [`Self::people_of`] does, so an open history names its authors again.
    people_moved: Cell<u64>,
    /// The session's action API, once its apps run here.
    #[cfg(target_os = "linux")]
    api: RefCell<Option<Arc<dyn crate::task_undo::UndoApi>>>,
    /// The records' content this device keeps.
    #[cfg(target_os = "linux")]
    content: RefCell<Option<Arc<crate::content::ContentService>>>,
    /// The session's action broker, whose apps' published actions say in their own words what
    /// each of their changes did, and the generation of them History last learned.
    #[cfg(target_os = "linux")]
    broker: RefCell<Option<LearnedBroker>>,
    #[cfg(target_os = "linux")]
    undos: crate::task_undo::Undos,
    /// The records each workspace holds, read on a worker of its own, told by the change stream
    /// when one of them moved.
    records:
        RefCell<HashMap<String, Rc<crate::platform::records::Reading<Vec<DetachedRecordHistory>>>>>,
    /// The records the session's services keep themselves, rather than an app's process or the
    /// store: the workspace record a service publishes holds a root per package it runs.
    services: RefCell<Option<Rc<RefCell<tilo_service::Services>>>>,
    /// What the workspaces hold, as this device reads it.
    held: RefCell<Option<Source>>,
    /// The one stream of record changes: what tells this session a workspace's records moved.
    changes: crate::platform::records::Changes,
    /// The stream's count and the reads that landed, as the last frame saw them: a change the
    /// person has not been shown asks for the frame that shows it.
    seen: Cell<(u64, u64)>,
    /// The session a change the model wrote back leaves, for the next pass.
    restored: RefCell<Option<Layout>>,
    /// The resources the person opened from the history.
    openings: RefCell<Vec<(WorkspaceId, ui::HistoryResourceTarget)>>,
    /// The windows' end of what they ask of the activity, and what they follow.
    requests: ActivityRequests,
    /// Raised when a window asked, or what the history reads moved.
    woken: Arc<AtomicBool>,
    /// The broker's end of the same activity, once this session serves the apps it runs.
    wire: RefCell<Option<Arc<ActivityWire>>>,
    /// How many new results the model's reads had sent when the apps following them were last told.
    served: Cell<u64>,
    /// One notice per device and reason, never one per write or per pass.
    #[cfg(target_os = "linux")]
    told_refusals: RefCell<BTreeSet<ToldRefusal>>,
    /// What the session's history says its changes were, as a model answered it.
    describer: RefCell<Describer>,
    /// Answers the model gave, waiting for the session's thread to write them, each with the workspace
    /// whose layout it was asked about.
    described: Arc<Mutex<Vec<(WorkspaceId, Job, String)>>>,
    /// One request at a time keeps a long history from spending the person's provider on a burst.
    describing: Arc<AtomicBool>,
    /// The session's changes the describer last read, so a pass with nothing new asks nothing.
    described_heads: Cell<u64>,
    /// When a model was last looked for.
    looked_for_model: Cell<Option<Instant>>,
    /// What wakes the session's loop when a model answered off this thread.
    wake: Option<tilo_service::Wake>,
}

#[cfg(target_os = "linux")]
impl Inner {
    /// Learn, on the thread that words History, the words of every action the apps publish,
    /// once they published anything new: a change an app made reads as that app says it.
    fn learn_published_words(&self) {
        let broker = self.broker.borrow();
        let Some((broker, learned)) = broker.as_ref() else {
            return;
        };
        let generation = broker.published_surfaces_generation();
        if learned.get() == Some(generation) {
            return;
        }
        learned.set(Some(generation));
        // The workspace's agents are a record the session holds, published by no app's surface.
        ui::history_source::sentence::learn_declared::<
            tilo_app::tilo_workspace_agent::WorkspaceAgents,
        >();
        // A live surface is listed before the one kept from before its app closed: learned last,
        // its words are the ones kept.
        for surface in broker.published_surfaces().iter().rev() {
            ui::history_source::sentence::learn_published(&surface.app_id, &surface.actions);
        }
    }
}

/// How often a session with nothing new looks again for a model the person may have configured.
const LOOK_FOR_MODEL: Duration = Duration::from_secs(30);

/// Something the history reads moved: the session draws a frame, and the loop wakes.
fn ask_for_frame(woken: &AtomicBool, wake: Option<&tilo_service::Wake>) {
    woken.store(true, Ordering::Release);
    if let Some(wake) = wake {
        wake();
    }
}

/// Who a workspace's history names: its members (people and agents), and the actors each
/// member's devices write as, by member, in hex.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Members {
    pub(crate) members: Vec<collab_types::Member>,
    pub(crate) writers: Vec<(collab_types::MemberId, Vec<String>)>,
}

impl Activity {
    /// This session's activity: the session under `state` , the records, the person.
    #[cfg(target_os = "linux")]
    pub(crate) fn new(
        state: Option<&std::path::Path>,
        person: String,
        store: Option<Store>,
        changes: crate::platform::records::Changes,
        apps: Option<Arc<dyn Apps>>,
        wake: Option<tilo_service::Wake>,
        layouts: Box<dyn History>,
    ) -> Self {
        let unlock = UnlockHistory::new(state, &person);
        let given = Given {
            layouts,
            person,
            store,
            held: None,
            state: state.map(std::path::Path::to_path_buf),
            apps,
            people: crate::platform::authors::unnamed(),
            writers: Writers::untracked(),
            pane: Rc::new(crate::platform::pane::Linked),
            models: None,
            changes,
            wake,
        };
        Self::assemble(given, unlock)
    }

    /// This session's activity over what `given` names: every host's.
    pub fn over(given: Given) -> Self {
        #[cfg(target_os = "linux")]
        let unlock = UnlockHistory::new(None, &given.person);
        Self::assemble(
            given,
            #[cfg(target_os = "linux")]
            unlock,
        )
    }

    fn assemble(given: Given, #[cfg(target_os = "linux")] unlock: UnlockHistory) -> Self {
        let Given {
            layouts,
            person,
            store,
            held,
            #[cfg(target_os = "linux")]
            state,
            #[cfg(not(target_os = "linux"))]
                state: _,
            apps,
            #[cfg(target_os = "linux")]
            people,
            #[cfg(not(target_os = "linux"))]
                people: _,
            writers,
            pane,
            models,
            changes,
            wake,
        } = given;
        let woken = Arc::new(AtomicBool::new(false));
        let requests = activity_requests(Arc::clone(&woken));
        let seen = (changes.moved(), 0);
        #[cfg(target_os = "linux")]
        let shown = DeviceState::load(state.as_deref());
        Self(Rc::new(Inner {
            layouts,
            #[cfg(target_os = "linux")]
            shown,
            #[cfg(target_os = "linux")]
            unlock,
            maps: HistoryMaps::load(store),
            apps,
            #[cfg(target_os = "linux")]
            people,
            writers,
            pane,
            models,
            #[cfg(target_os = "linux")]
            view: RefCell::default(),
            people_of: RefCell::default(),
            people_moved: Cell::default(),
            #[cfg(target_os = "linux")]
            api: RefCell::default(),
            #[cfg(target_os = "linux")]
            content: RefCell::default(),
            #[cfg(target_os = "linux")]
            broker: RefCell::default(),
            #[cfg(target_os = "linux")]
            undos: crate::task_undo::Undos::default(),
            person,
            records: RefCell::default(),
            services: RefCell::default(),
            held: RefCell::new(held),
            changes,
            seen: Cell::new(seen),
            restored: RefCell::default(),
            openings: RefCell::default(),
            requests,
            woken,
            wire: RefCell::default(),
            served: Cell::default(),
            #[cfg(target_os = "linux")]
            told_refusals: RefCell::default(),
            describer: RefCell::default(),
            described: Arc::default(),
            describing: Arc::default(),
            described_heads: Cell::default(),
            looked_for_model: Cell::default(),
            wake,
        }))
    }

    /// Read and write the person's workspaces through the device's daemon: `view` of how they
    /// stand (where a workspace's writes were refused included), and `reads` and `writes` of
    /// their documents, which the layouts and the history are read from and written to.
    #[cfg(target_os = "linux")]
    pub(crate) fn attach_workspaces(
        &self,
        view: Arc<crate::workspace_view::WorkspaceView>,
        reads: Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
        writes: &Arc<dyn tilo_peer_state::workspace_documents::DeviceWrites>,
    ) {
        if let Some(identity) = tilo_session_client::identity::device_identity() {
            self.0
                .layouts
                .attach(&identity.device_cert, &view, &reads, writes);
        }
        *self.0.view.borrow_mut() = Some(view);
        self.attach_documents(reads);
    }

    /// Read what the workspaces hold through `reads`, the session's own read of its device's
    /// workspace service.
    #[cfg(target_os = "linux")]
    fn attach_documents(
        &self,
        reads: Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
    ) {
        *self.0.held.borrow_mut() = Some(Arc::new(crate::platform::records::Held::over(
            reads,
            self.0.changes.clone(),
        )));
    }

    /// Tell the person about the refusals waiting cannot change, once each until the verdict goes.
    #[cfg(target_os = "linux")]
    fn tell_refusals(&self, notifications: &crate::notifications::SessionNotifications) {
        let Some(view) = self.0.view.borrow().as_ref().map(Arc::clone) else {
            return;
        };
        let mut told = self.0.told_refusals.borrow_mut();
        let mut lasting = Vec::new();
        for (workspace, refusal) in view.lasting_refusals() {
            let key = (workspace.clone(), refusal.by, refusal.reason);
            lasting.push(key.clone());
            if !told.insert(key) {
                continue;
            }
            let label = view
                .device_labels(&workspace)
                .get(&refusal.by)
                .filter(|label| !label.is_empty())
                .cloned()
                .unwrap_or_else(|| hex::encode(&refusal.by[..4]));
            notifications.tell(&crate::notices::refused_write_notice(
                &label,
                refusal.reason,
            ));
        }
        told.retain(|key| lasting.contains(key));
    }

    /// Name `workspace` 's members as `members` do: who a change in its history was made by.
    #[cfg(target_os = "linux")]
    pub(crate) fn set_people(&self, workspace: &str, members: Members) {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return;
        };
        let mut held = self.0.people_of.borrow_mut();
        if held.get(&workspace) != Some(&members) {
            held.insert(workspace, members);
            self.0
                .people_moved
                .set(self.0.people_moved.get().wrapping_add(1));
        }
    }

    /// What the windows ask of the activity and follow.
    pub fn requests(&self) -> ActivityRequests {
        self.0.requests.clone()
    }

    /// Serve the apps this session runs through `wire` .
    #[cfg(target_os = "linux")]
    pub(crate) fn serve_apps(&self, wire: Arc<ActivityWire>) {
        *self.0.wire.borrow_mut() = Some(wire);
    }

    /// Undo an agent's run in the records it changed, as the person at the screen.
    #[cfg(target_os = "linux")]
    pub(crate) fn attach_api(&self, api: Arc<dyn crate::task_undo::UndoApi>) {
        *self.0.api.borrow_mut() = Some(api);
    }

    /// Take back the work of the run `run` in `workspace`, as the person at the screen: each
    /// document `steps` names is asked through the session's action API, on a thread of its own, as the
    /// history's own Undo run asks. Answers whether it started; what came of it is in the
    /// documents, which a person reads there.
    ///
    /// # Errors
    /// The session serves no action API to ask, so nothing was undone.
    #[cfg(target_os = "linux")]
    pub(crate) fn undo_invocation(
        &self,
        workspace: &str,
        run: &str,
        steps: Vec<(String, String)>,
    ) -> Result<(), String> {
        let Some(api) = self.0.api.borrow().as_ref().map(Arc::clone) else {
            return Err(tilo_session_core::desktop::i18n::t(
                "activity-undo-task-unavailable",
            ));
        };
        let woken = Arc::clone(&self.0.woken);
        let wake = self.0.wake.clone();
        self.0.undos.start(
            api,
            self.0.person.clone(),
            workspace.to_owned(),
            uuid::Uuid::new_v4(),
            run.to_owned(),
            steps,
            move || ask_for_frame(&woken, wake.as_ref()),
        )
    }

    /// Read the output of command runs from `content` .
    #[cfg(target_os = "linux")]
    pub(crate) fn attach_content(&self, content: Arc<crate::content::ContentService>) {
        *self.0.content.borrow_mut() = Some(content);
    }

    /// Word the apps' changes by what `broker` says they publish: each action's label and what
    /// the app says it did, as the apps publish them now.
    #[cfg(target_os = "linux")]
    pub(crate) fn attach_broker(&self, broker: Arc<AppBrokerState>) {
        *self.0.broker.borrow_mut() = Some((broker, Cell::new(None)));
    }

    /// Read the records the session's own services keep from `services` : what History lists and
    /// a run is undone in, where no app's process holds the record. The desktop mounts the
    /// services, so only it attaches them.
    #[cfg(target_os = "linux")]
    pub(crate) fn attach_services(&self, services: Rc<RefCell<tilo_service::Services>>) {
        *self.0.services.borrow_mut() = Some(services);
    }

    /// Take part in `link` 's fabric.
    #[cfg(target_os = "linux")]
    pub(crate) fn watch_fabric(&self, link: SharedLink) {
        self.0.unlock.watch(link);
    }

    /// Write what the fabric holds now into the person's own record.
    #[cfg(target_os = "linux")]
    pub(crate) fn keep_unlock(&self) {
        if self.0.unlock.absorb() {
            ask_for_frame(&self.0.woken, self.0.wake.as_ref());
        }
    }

    /// Serve what the activity holds on this pass: the apps following it, and the refusals owed.
    #[cfg(target_os = "linux")]
    pub(crate) fn serve(&self, notifications: &crate::notifications::SessionNotifications) {
        self.serve_following();
        self.tell_refusals(notifications);
    }

    pub fn serve_following(&self) {
        let served = self.0.requests.served_revision();
        if served != self.0.served.get() {
            self.0.served.set(served);
            if let Some(wire) = self.0.wire.borrow().as_ref() {
                wire.changed();
            }
        }
    }

    /// Show what this device left in `tree`: the workspace in front and the displays' partition.
    /// Whether anything changed.
    #[cfg(target_os = "linux")]
    pub fn show(&self, tree: &mut Layout) -> bool {
        self.0.shown.applied(tree)
    }

    /// Whether this device kept no workspace to show, so the shell shows only the one it started
    /// with. The shell is the Linux session's ([`crate::platform::session`]).
    #[cfg(target_os = "linux")]
    pub(crate) fn keeps_no_workspace(&self) -> bool {
        self.0.shown.keeps_no_workspace()
    }

    /// Keep what `tree` shows of this device: the workspace in front and the displays' partition.
    /// Read at `revision` of the session, so a session that did not move is not read.
    #[cfg(target_os = "linux")]
    pub fn keep_shown(&self, revision: Option<u64>, tree: impl FnOnce() -> Layout) -> bool {
        self.0.shown.keep(revision, tree)
    }

    /// The workspace a change the model wrote back leaves the session at, if one is waiting.
    pub fn take_restored(&self) -> Option<Layout> {
        self.0.restored.borrow_mut().take()
    }

    /// The resources the person opened from the history, each with its workspace.
    pub fn take_openings(&self) -> Vec<(WorkspaceId, ui::HistoryResourceTarget)> {
        std::mem::take(&mut *self.0.openings.borrow_mut())
    }

    /// Whether a window asked, or a record moved and was read again, since last taken.
    pub fn take_woken(&self) -> bool {
        let now = (self.0.changes.moved(), self.reads());
        let moved = self.0.seen.replace(now) != now;
        self.0.woken.swap(false, Ordering::AcqRel) || moved
    }

    /// When this session next looks for a model.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.0
            .looked_for_model
            .get()
            .and_then(|at| at.checked_add(LOOK_FOR_MODEL))
    }

    /// Say what the session's changes were when the session moved: one request at a time, off the shell's
    /// thread. Whether a layout changed.
    pub fn describe(&self, tree: &Layout) -> bool {
        let mut wrote = false;
        let answers = std::mem::take(&mut *self.0.described.lock_recover());
        {
            let mut describer = self.0.describer.borrow_mut();
            for (workspace_id, job, answer) in answers {
                wrote |= if answer.is_empty() {
                    // No model answered: nothing is described in its place.
                    describer.failed(&job);
                    false
                } else {
                    self.0.layouts.describe(tree, workspace_id, &mut |doc| {
                        describer.apply(doc, &job, &answer)
                    })
                };
            }
        }
        if self.0.describing.load(Ordering::Acquire) {
            return wrote;
        }
        let moved = self.0.layouts.moved();
        let look_again = self
            .0
            .looked_for_model
            .get()
            .is_none_or(|at| at.elapsed() >= LOOK_FOR_MODEL);
        if moved == self.0.described_heads.get() && !look_again {
            return wrote;
        }
        self.0.described_heads.set(moved);
        let mut describer = self.0.describer.borrow_mut();
        describer.set_enabled(self.0.models.is_some());
        let mut asked = None;
        for (workspace_id, mut doc) in self.0.layouts.documents(tree) {
            if let Some(job) = describer.next(&mut doc) {
                asked = Some((workspace_id, job));
                break;
            }
        }
        let Some((workspace_id, job)) = asked else {
            self.0.looked_for_model.set(Some(ui::time::now()));
            return wrote;
        };
        drop(describer);
        self.0.looked_for_model.set(Some(ui::time::now()));
        self.0.describing.store(true, Ordering::Release);
        let answers = Arc::clone(&self.0.described);
        let describing = Arc::clone(&self.0.describing);
        let woken = Arc::clone(&self.0.woken);
        let wake = self.0.wake.clone();
        let models = self.0.models.clone();
        let ask = move || async move {
            let answer = crate::activity_models::answer(models.as_deref(), &job).await;
            answers.lock_recover().push((workspace_id, job, answer));
            describing.store(false, Ordering::Release);
            // The session draws a frame to write the answer where it belongs.
            ask_for_frame(&woken, wake.as_ref());
        };
        // A page has one thread and nothing may block on it: the answer is a task of its own turn.
        #[cfg(target_arch = "wasm32")]
        tilo_rt::spawn_task(ask());
        #[cfg(not(target_arch = "wasm32"))]
        std::thread::spawn(move || {
            if let Ok(runtime) = tilo_rt::try_app() {
                let _ = runtime.block_on(ask());
            }
        });
        wrote
    }

    /// The `WorkspaceId` of `workspace` , as `tree` holds it.
    fn id_of(tree: &Layout, workspace: WorkspaceId) -> Option<WorkspaceId> {
        tree.workspaces
            .iter()
            .find(|held| held.id() == workspace)
            .map(Workspace::id)
    }

    /// The records `workspace_id` holds: read when the change stream says one of them moved, on a
    /// worker where this host has threads, so a workspace's records never hold up a frame.
    fn resources_of(&self, workspace_id: WorkspaceId) -> Vec<DetachedRecordHistory> {
        self.records_held(workspace_id).as_ref().clone()
    }

    /// The services this session runs, once it has started them.
    fn services_held(&self) -> Option<Rc<RefCell<tilo_service::Services>>> {
        self.0
            .services
            .try_borrow()
            .ok()
            .and_then(|held| held.clone())
    }

    /// The records `workspace_id` holds as last read, shared rather than copied.
    fn records_held(&self, workspace_id: WorkspaceId) -> Arc<Vec<DetachedRecordHistory>> {
        let Some(held) = self.0.held.borrow().clone() else {
            return Arc::default();
        };
        let key = workspace_id.to_string();
        let watch = crate::platform::records::Watch::Workspace(key.clone());
        if let Some(known) = self.0.records.borrow().get(&key) {
            let known = Rc::clone(known);
            return known.following(&self.0.changes, &watch);
        }
        let by_workspace = key.clone();
        let noted = crate::platform::authors::new_noted({
            let noted = self.0.writers.noted.clone();
            move |actor| noted(actor)
        });
        let read = move || read_workspace_records(held.as_ref(), &by_workspace, &noted);
        #[cfg(target_os = "linux")]
        let known = Rc::new(crate::platform::records::Reading::start(
            "tilo-activity-records",
            self.0.changes.clone(),
            crate::platform::records::Watch::Workspace(key.clone()),
            read,
            {
                let woken = Arc::clone(&self.0.woken);
                let wake = self.0.wake.clone();
                move || ask_for_frame(&woken, wake.as_ref())
            },
        ));
        #[cfg(not(target_os = "linux"))]
        let known = Rc::new(crate::platform::records::Reading::read(
            &self.0.changes,
            &watch,
            read,
        ));
        let value = known.get();
        self.0.records.borrow_mut().insert(key, Rc::clone(&known));
        value
    }

    /// The reads of the workspaces' records that landed: their count tells a frame that what the
    /// history reads changed since it was last served.
    fn reads(&self) -> u64 {
        self.0
            .records
            .borrow()
            .values()
            .map(|known| known.reads())
            .sum()
    }
}

impl HistorySource for Activity {
    fn workspace_history(
        &mut self,
        tree: &Layout,
        workspace: WorkspaceId,
    ) -> Option<ActivitySnapshot> {
        Self::id_of(tree, workspace)?;
        let mut snapshot = self.0.layouts.history(tree, workspace)?;
        snapshot.desktop.active = workspace;
        let restored = Rc::downgrade(&self.0);
        let commit = Rc::new(move |tree: &Layout| {
            let Some(inner) = restored.upgrade() else {
                return;
            };
            *inner.restored.borrow_mut() = Some(tree.clone());
        });
        Some(snapshot.committing(commit))
    }

    fn record_histories(
        &mut self,
        tree: &Layout,
        workspace: WorkspaceId,
    ) -> Vec<DetachedRecordHistory> {
        #[cfg(target_os = "linux")]
        self.0.learn_published_words();
        let mut records = Vec::new();
        if let Some(workspace_id) = Self::id_of(tree, workspace) {
            records.extend(self.resources_of(workspace_id));
        }
        // What the person's devices answered when one of them asked to be unlocked is theirs and
        // not any one workspace's: it reads the same in every workspace's history.
        #[cfg(target_os = "linux")]
        if let Some(record) = self.0.unlock.history() {
            records.push(record);
        }
        records
    }

    fn host(&self) -> Rc<dyn ActivityHost> {
        Rc::new(Host(Rc::clone(&self.0))) as Rc<dyn ActivityHost>
    }

    fn register_authors(&self, workspace: WorkspaceId, authors: &mut ui::AuthorDirectory) {
        // Who this host's own mounts write as: a record's changes by the person who made them.
        (self.0.writers.register)(authors);
        // The workspace's people and agents: a change another member made reads as theirs, by
        // name, and so does one a person's device signed, such as the workspace's governance.
        if let Some(held) = self.0.people_of.borrow().get(&workspace) {
            authors.register_roster(
                held.members
                    .iter()
                    .map(|member| (&member.id, member.name.as_str())),
            );
            for (member, writers) in &held.writers {
                authors.register_writers(member, writers.iter().map(String::as_str));
            }
        }
        // A change of a workspace's layout is the person's: the record is written as them, so it
        // reads as presence shows them here and never as another member.
        authors.register_local_identity(
            &automerge::ActorId::from(self.0.person.as_bytes()).to_hex_string(),
        );
        // The devices that answered a boot are named as their own snapshots name them, and this one
        // of them is the person here.

        #[cfg(target_os = "linux")]
        {
            let unlock = &self.0.unlock;
            history_log::register_authors(authors, &unlock.authors(), &*self.0.people);
            if let Some(actor) = unlock.local_actor() {
                authors.register_local_identity(&actor);
            }
        }
    }

    fn revision(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        #[cfg(target_os = "linux")]
        self.0.unlock.absorb();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.0.layouts.moved().hash(&mut hasher);
        #[cfg(target_os = "linux")]
        self.0.unlock.heads().hash(&mut hasher);
        // What the records say moved, and the reads of them that landed since: a history open on
        // this device is served again.
        self.0.changes.moved().hash(&mut hasher);
        self.reads().hash(&mut hasher);
        self.0.people_moved.get().hash(&mut hasher);
        // What the session's own services keep moves without a store's change stream: an agent's
        // step in a record the session holds is one of its runs, and asks for the frame listing it.
        if let Some(services) = self.services_held()
            && let Ok(services) = services.try_borrow()
        {
            services.revision().hash(&mut hasher);
        }
        hasher.finish()
    }

    fn activity_pane(&self) -> tilo_session_core::desktop::state::ActivityPaneCode {
        self.0.pane.pane_code()
    }

    /// The person opened a resource from the history: its app opens on the next pass.
    fn resource_tab(
        &self,
        tree: &Layout,
        target: &ui::HistoryResourceTarget,
    ) -> Option<tilo_desktop_model::TabId> {
        // No app holds the person's own record of what their devices answered: its rows are read in
        // the history, and opened nowhere.
        #[cfg(target_os = "linux")]
        if self.0.unlock.holds(&target.id) {
            return None;
        }
        let workspace_id = tree.active_workspace().map(Workspace::id)?;
        self.0
            .openings
            .borrow_mut()
            .push((workspace_id, target.clone()));
        None
    }
}

/// Every record a workspace holds, with the document its app last wrote.
fn read_workspace_records(
    held: &dyn Reads,
    workspace: &str,
    noted: &crate::platform::authors::Noted,
) -> Vec<DetachedRecordHistory> {
    let mut read = Vec::new();
    let mut seen = BTreeSet::new();
    // The workspace's folder is a record of its own, and its changes belong in
    // the history beside the apps': a page holds it without listing it among
    // the packages it opened, and a computer's folder writes are its versions.
    let tree_id = collab_types::person_document_id(fs_protocol::tree::TREE_KIND);
    if let Some(document_id) = DocumentId::parse(&tree_id)
        && let Some(document) =
            held.document(fs_protocol::tree::FILES_APP, workspace, document_id.route())
    {
        noted(document.get_actor());
        seen.insert((
            fs_protocol::tree::FILES_APP.to_owned(),
            document_id.route().to_owned(),
        ));
        // The chunks the tree lists, each read on its own: the folder's rows are read from
        // them beside it. A chunk this device holds nothing of comes with a later read.
        let tree_chunks = fs_protocol::tree::listed_chunks(&document)
            .into_iter()
            .filter_map(|chunk| {
                let chunk_id = fs_protocol::tree::chunk_document_id(chunk);
                seen.insert((fs_protocol::tree::FILES_APP.to_owned(), chunk_id.clone()));
                held.document(fs_protocol::tree::FILES_APP, workspace, &chunk_id)
                    .map(|doc| (chunk, doc))
            })
            .collect();
        read.push(DetachedRecordHistory {
            app: fs_protocol::tree::FILES_APP.to_owned(),
            checkout: DocumentCheckout::new(
                document_id,
                ResourceType::new(fs_protocol::tree::TREE_KIND),
            ),
            tree_chunks,
            document: Some(document),
        });
    }
    // Every resource the workspace holds, as the workspace lists them: an app keeps them whether
    // or not its own document indexes them.
    for (package, route) in held.held(workspace) {
        if !seen.insert((package.clone(), route.clone())) {
            continue;
        }
        let Some(document) = held.document(&package, workspace, &route) else {
            continue;
        };
        // The app's own document of the workspace keeps its index. It is a resource of its own
        // (Notes' notebook, Chat's chat) once anything was done in it: keeping the index alone is
        // no one's work. Only the changes' messages are read.
        let checkout = if route.is_empty() {
            let acted = document
                .committed_document()
                .get_changes_meta(&[])
                .iter()
                .any(|change| {
                    change.message.as_deref().is_some_and(|message| {
                        message.starts_with(tilo_actions::CHANGE_MESSAGE_PREFIX)
                            || message.starts_with(tilo_actions::CONTINUATION_PREFIX)
                    })
                });
            acted.then(|| DocumentCheckout::package_primary(ResourceType::new(package.as_str())))
        } else {
            ui::resource::identity(&document)
        };
        let Some(checkout) = checkout else {
            continue;
        };
        noted(document.get_actor());
        read.push(DetachedRecordHistory {
            app: package,
            checkout,
            tree_chunks: Vec::new(),
            document: Some(document),
        });
    }
    read
}

/// What the model asks of the session, over the one state this activity keeps.
struct Host(Rc<Inner>);

impl ActivityHost for Host {
    fn local_user(&self) -> String {
        self.0.person.clone()
    }

    fn local_author(&self, authors: &mut ui::AuthorDirectory, actor: &str) -> Option<String> {
        if authors.register_local_identity(actor) {
            return None;
        }
        (!self.0.person.is_empty()).then(|| self.0.person.clone())
    }

    fn load_morning_sources(&self) -> HashMap<WorkspaceId, morning::MorningPreferences> {
        self.0.maps.map("morning_sources")
    }

    fn save_morning_sources(&self, sources: &HashMap<WorkspaceId, morning::MorningPreferences>) {
        self.0.maps.save("morning_sources", sources);
    }

    fn load_frontiers(&self) -> HashMap<WorkspaceId, WorkspaceSeenFrontier> {
        self.0.maps.map("seen_frontiers")
    }

    fn save_frontiers(&self, frontiers: &HashMap<WorkspaceId, WorkspaceSeenFrontier>) {
        self.0.maps.save("seen_frontiers", frontiers);
    }

    fn load_departures(&self) -> HashMap<WorkspaceId, WorkspaceDeparture> {
        self.0.maps.map("departures")
    }

    fn save_departures(&self, departures: &HashMap<WorkspaceId, WorkspaceDeparture>) {
        self.0.maps.save("departures", departures);
    }

    fn mutations_available(&self) -> bool {
        true
    }

    /// The session's own layout is this session's to write; a record's history is its app's.
    fn writable(&self, origin: &HistoryOrigin) -> bool {
        matches!(origin, HistoryOrigin::Layout)
    }

    #[cfg(target_os = "linux")]
    fn document_content(&self, workspace: &str, app: &str, hash: &str) -> Option<Vec<u8>> {
        self.0.content.borrow().as_ref()?.read(workspace, app, hash)
    }

    fn app_display_name(&self, app: &str) -> Option<String> {
        let name = self.0.apps.as_ref()?.name(app);
        (!name.is_empty()).then_some(name)
    }

    #[cfg(target_os = "linux")]
    fn refusals(&self, workspace: &str) -> Vec<wire::Refusal> {
        let Some(view) = self.0.view.borrow().as_ref().map(Arc::clone) else {
            return Vec::new();
        };
        let labels = view.device_labels(workspace);
        view.write_refusals(workspace)
            .into_iter()
            .flat_map(|refusal| {
                let label = labels
                    .get(&refusal.by)
                    .filter(|label| !label.is_empty())
                    .cloned()
                    .unwrap_or_else(|| hex::encode(&refusal.by[..4]));
                let reason = crate::notices::reason_in_words(refusal.reason);
                let permanent = refusal.reason.permanent();
                refusal
                    .changes
                    .into_iter()
                    .map(|change| wire::Refusal {
                        change: wire::change_hex(&change),
                        device: hex::encode(refusal.by),
                        label: label.clone(),
                        reason: reason.clone(),
                        permanent,
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn undo_task_run(&self, undo: TaskRunUndo) -> Result<(), String> {
        // Undone by each changed document's own history, as the person at the screen.
        #[cfg(target_os = "linux")]
        {
            let Some(api) = self.0.api.borrow().as_ref().map(Arc::clone) else {
                return Err(tilo_session_core::desktop::i18n::t(
                    "activity-undo-task-unavailable",
                ));
            };
            let woken = Arc::clone(&self.0.woken);
            let wake = self.0.wake.clone();
            self.0.undos.start(
                api,
                self.0.person.clone(),
                undo.workspace,
                undo.invocation,
                undo.task_id,
                undo.steps,
                move || ask_for_frame(&woken, wake.as_ref()),
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = undo;
            Err(tilo_session_core::desktop::i18n::t(
                "activity-undo-task-unavailable",
            ))
        }
    }

    fn poll_task_run_undo(
        &self,
        invocation: uuid::Uuid,
    ) -> Option<Result<Vec<TaskRunDocument>, String>> {
        #[cfg(target_os = "linux")]
        {
            match self.0.undos.poll(invocation)? {
                crate::task_undo::UndoState::Running => None,
                crate::task_undo::UndoState::Ended(outcome) => Some(outcome),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = invocation;
            None
        }
    }
}

struct HistoryMaps {
    known: RefCell<serde_json::Map<String, Value>>,
    store: Option<Store>,
    /// The claim on the record, held for as long as this session writes it.
    lease: RefCell<Option<durable_store::Lease>>,
}

impl HistoryMaps {
    fn load(store: Option<Store>) -> Self {
        let known = store
            .as_ref()
            .and_then(|store| Self::read(store.as_ref()))
            .unwrap_or_default();
        Self {
            known: RefCell::new(known),
            store,
            lease: RefCell::default(),
        }
    }

    /// What this device's store holds of the view.
    fn read(store: &dyn DurableStore) -> Option<serde_json::Map<String, Value>> {
        let key = StorageKey::app(VIEW_PACKAGE);
        let stored = match store.read(&key) {
            Ok(stored) => stored?,
            Err(error) => {
                tracing::warn!(%error, "the remembered history view is not read");
                return None;
            }
        };
        serde_json::from_slice(&stored.bytes)
            .inspect_err(
                |error| tracing::warn!(%error, "the remembered history view does not read back"),
            )
            .ok()
    }

    /// `key` 's per-workspace map, as this session last kept it.
    fn map<T: serde::de::DeserializeOwned>(&self, key: &str) -> HashMap<WorkspaceId, T> {
        self.known
            .borrow()
            .get(key)
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(serde_json::Map::iter)
            .filter_map(|(id, value)| {
                Some((
                    id.parse().ok()?,
                    serde_json::from_value(value.clone()).ok()?,
                ))
            })
            .collect()
    }

    /// Keep `values` as `key` 's map, for this session and the next.
    fn save<T: serde::Serialize>(&self, key: &str, values: &HashMap<WorkspaceId, T>) {
        let map = Value::Object(
            values
                .iter()
                .filter_map(|(id, value)| Some((id.to_string(), serde_json::to_value(value).ok()?)))
                .collect(),
        );
        self.known.borrow_mut().insert(key.to_owned(), map);
        self.keep();
    }

    /// Write the view to this device's store, as it stands now.
    fn keep(&self) {
        let Some(store) = &self.store else {
            return;
        };
        let key = StorageKey::app(VIEW_PACKAGE);
        let Ok(bytes) = serde_json::to_vec(&*self.known.borrow()) else {
            return;
        };
        let mut lease = self.lease.borrow_mut();
        if lease.is_none() {
            match store.lease(&key) {
                Ok(held) => *lease = Some(held),
                Err(error) => {
                    tracing::warn!(%error, "the remembered history view is not written");
                    return;
                }
            }
        }
        if let Err(error) = store.write(&key, WriteOp::Replace(bytes), None) {
            tracing::warn!(%error, "the remembered history view is not written");
        }
    }
}

#[cfg(test)]
mod tests {
    //! A native session's history, as the session serves it: the activity a person opens from its
    //! command is the workspace's own changes, and what they restore from it reaches the session.

    use super::*;
    use crate::platform::records::Kept as InStore;
    use automerge::transaction::Transactable as _;

    /// A window's record is read where its app keeps it, under its storage key: a document a file
    /// opened in Word Processor, named by the file's path, has its history like any other.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_record_named_by_a_files_path_is_read_for_its_history() {
        let dir = tempfile::tempdir().unwrap();
        let store = app_state::FsStore::new(dir.path());
        let workspace = "7c307c8d-bfa5-46fa-ae08-fa2fe4790bd4";
        let app = "so.tilo.word-processor";
        let id = DocumentId::for_file("/run/tilo/documents/Budget for Friday.txt");
        let kind = ResourceType::new("word-processor.document");
        let write = |key: &StorageKey, doc: &mut ui::automerge::AutoCommit| {
            let _lease = store.lease(key).unwrap();
            store
                .write(key, WriteOp::Replace(doc.save()), None)
                .unwrap();
            store.flush(Some(key)).unwrap();
        };
        let mut index = ui::automerge::AutoCommit::new();
        assert!(ui::resource::index::reference(&mut index, &id, &kind));
        index.commit();
        write(&StorageKey::workspace_index(app, workspace), &mut index);
        let mut document = ui::automerge::AutoCommit::new();
        ui::resource::init(&mut document, &id, &kind, workspace);
        write(
            &StorageKey::document(app, workspace, id.storage_key()),
            &mut document,
        );

        let noted: crate::platform::authors::Noted = Arc::new(|_| {});
        let kept = InStore::over(
            Arc::new(store),
            crate::platform::records::Changes::default(),
        );
        let read = read_workspace_records(&kept, workspace, &noted);
        let budget = read
            .iter()
            .find(|record| record.checkout.id == id)
            .expect("the index names the record");
        assert!(budget.document.is_some(), "and its document is read");
    }

    use tilo_actions::InvocationId;
    use tilo_actions::remote::workspace_activity as wire;
    use tilo_actions::remote::{Caller, InvokeArgument, PendingPoll};
    use tilo_desktop_model::commands::DesktopCommand;
    use tilo_desktop_model::{IdMinter, Tab, TileContent};
    use tilo_session_core::desktop::navigation::{ActivityView, ChromeEffect};
    use tilo_session_core::desktop::seed_tree;
    use tilo_session_core::desktop::state::{DesktopHandle, DesktopPass, TileSource};
    use ui::widget::TileHost;

    /// What a test session's tabs say. Nothing the session would start.
    struct Tiles;

    impl TileSource for Tiles {
        fn tile(&mut self, _workspace: &Workspace, tab: &Tab) -> TileHost {
            TileHost::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            })
        }
    }

    /// The person at this session.
    fn person() -> Caller {
        Caller::local_person("ada").in_workspace("workspace")
    }

    /// The workspaces as a test session keeps them: each one's layout record and its own document,
    /// written from the session on each frame as the person here.
    #[derive(Clone, Default)]
    struct Records(std::rc::Rc<RefCell<HashMap<WorkspaceId, Kept>>>);

    #[derive(Clone)]
    struct Kept {
        layout: ui::automerge::AutoCommit,
        governance: ui::automerge::AutoCommit,
    }

    impl Records {
        /// Write `workspace` of `tree` into its layout record and its name into its own document,
        /// as a pass does. What founds either is the session restored from an earlier session, nobody's
        /// change.
        fn keep(&self, workspace: &tilo_desktop_model::Workspace) {
            use tilo_desktop_model::projection::{WORKSPACE_ID_KEY, WORKSPACE_NAME_KEY};
            use tilo_desktop_model::shared_resource;
            use ui::automerge::ReadDoc as _;
            let layout = tilo_desktop_model::ownership::SharedDesktop {
                tree: Layout {
                    workspaces: vec![workspace.clone()],
                    active: workspace.id(),
                    ..Layout::default()
                },
                ..Default::default()
            };
            let commit = |doc: &mut ui::automerge::AutoCommit, housekeeping: bool| {
                let message = if housekeeping {
                    shared_resource::housekeeping_message("ada")
                } else {
                    shared_resource::step_message("ada")
                };
                doc.commit_with(
                    automerge::transaction::CommitOptions::default()
                        .with_message(message)
                        .with_time(now_seconds()),
                );
            };
            let mut held = self.0.borrow_mut();
            let kept = held.entry(workspace.id()).or_insert_with(|| {
                let doc = || {
                    let mut doc = ui::automerge::AutoCommit::new();
                    doc.set_actor(tilo_session_core::layout::actor_of_person("ada"));
                    doc
                };
                Kept {
                    layout: doc(),
                    governance: doc(),
                }
            });
            // What arranges nothing (the record's copy of the workspace's name) is no step either.
            let id = workspace.id();
            let was = crate::layout::arranged(&kept.layout, id);
            let founding = kept.layout.get_heads().is_empty();
            if shared_resource::write(&mut kept.layout, &layout).unwrap_or(false) {
                let step = !founding && crate::layout::arranged(&kept.layout, id) != was;
                commit(&mut kept.layout, !step);
            }
            let governance = &mut kept.governance;
            let founding = governance.get_heads().is_empty();
            let named = governance
                .get(automerge::ROOT, WORKSPACE_NAME_KEY)
                .ok()
                .flatten()
                .and_then(|(value, _)| value.into_string().ok());
            if named.as_deref() != Some(workspace.name.as_str()) {
                let _ = governance.put(automerge::ROOT, WORKSPACE_ID_KEY, id.to_string());
                let _ = governance.put(automerge::ROOT, WORKSPACE_NAME_KEY, workspace.name.clone());
                if founding {
                    commit(governance, true);
                } else {
                    let _ = tilo_resource::history::commit_stamped(governance);
                }
            }
        }

        fn of(&self, workspace_id: WorkspaceId) -> Option<ui::automerge::AutoCommit> {
            self.0
                .borrow()
                .get(&workspace_id)
                .map(|kept| kept.layout.clone())
        }
    }

    impl History for Records {
        fn history(&self, tree: &Layout, workspace_id: WorkspaceId) -> Option<ActivitySnapshot> {
            let kept = self.0.borrow().get(&workspace_id)?.clone();
            Some(
                ActivitySnapshot::of_layout(tree.clone(), workspace_id, kept.layout)
                    .governing(kept.governance)
                    .reading_governance(Rc::new(GovernanceValues)),
            )
        }

        fn documents(&self, tree: &Layout) -> Vec<(WorkspaceId, ui::automerge::AutoCommit)> {
            tree.workspaces
                .iter()
                .filter_map(|workspace| Some((workspace.id(), self.of(workspace.id())?)))
                .collect()
        }

        fn describe(
            &self,
            _tree: &Layout,
            workspace_id: WorkspaceId,
            write: &mut dyn FnMut(&mut ui::automerge::AutoCommit) -> bool,
        ) -> bool {
            let Some(mut record) = self.of(workspace_id) else {
                return false;
            };
            if !write(&mut record) {
                return false;
            }
            let _ = tilo_resource::history::commit_stamped(&mut record);
            if let Some(kept) = self.0.borrow_mut().get_mut(&workspace_id) {
                kept.layout = record;
            }
            true
        }

        fn moved(&self) -> u64 {
            use std::hash::{Hash as _, Hasher as _};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            for kept in self.0.borrow().values() {
                kept.layout.base_heads().hash(&mut hasher);
                kept.governance.base_heads().hash(&mut hasher);
            }
            hasher.finish()
        }

        fn attach(
            &self,
            _certificate: &tilo_user_identity::DeviceCert,
            _view: &std::sync::Arc<crate::workspace_view::WorkspaceView>,
            _reads: &std::sync::Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
            _writes: &std::sync::Arc<dyn tilo_peer_state::workspace_documents::DeviceWrites>,
        ) {
        }
    }

    /// A session of one workspace with Notes open, its activity served as this session serves it.
    struct Session {
        pass: DesktopPass,
        handle: DesktopHandle,
        activity: Activity,
        workspace: WorkspaceId,
        records: Records,
    }

    impl Session {
        fn new() -> Self {
            Self::seeded(|minter, notes| seed_tree(minter, "Studio", vec![notes]))
        }

        /// A session seeded by `seed` , given its minter and the Notes window it shows.
        fn seeded(seed: impl FnOnce(&mut IdMinter, Tab) -> Layout) -> Self {
            let mut minter = IdMinter::new(7);
            let notes = Tab {
                id: minter.mint(),
                title: "Notes".into(),
                tile: TileContent::LocalApp {
                    app_id: "so.tilo.notes".into(),
                    label: "Notes".into(),
                    note: String::new(),
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            };
            let tree = seed(&mut minter, notes);
            let workspace = tree.active;
            let handle = DesktopHandle::new(tree, 7);
            let mut pass = DesktopPass::new(handle.clone());
            let records = Records::default();
            let activity = Activity::new(
                None,
                "ada".to_string(),
                None,
                crate::platform::records::Changes::default(),
                None,
                None,
                Box::new(records.clone()),
            );
            pass.set_history_source(Box::new(activity.clone()));
            pass.serve_activity(activity.requests());
            Self {
                pass,
                handle,
                activity,
                workspace,
                records,
            }
        }

        /// One pass of the session: the session as it stood is written into the workspaces' layouts, then
        /// what the windows asked is answered.
        fn frame(&mut self) {
            let workspaces = self.handle.read(|tree| tree.workspaces.clone());
            for workspace in &workspaces {
                self.records.keep(workspace);
            }
            self.pass.step(&mut Tiles);
        }

        /// The person asks for this workspace's history, and it opens.
        fn open_history(&mut self) {
            self.handle
                .dispatch(ChromeEffect::Activity {
                    workspace: self.workspace.to_string(),
                    view: ActivityView::Explore,
                    author: None,
                })
                .expect("the history is admitted");
            self.frame();
        }

        /// The person names the first strip of the workspace in front.
        fn name_strip(&self, name: &str) {
            let strip = self.handle.read(|tree| {
                tree.active_workspace()
                    .and_then(|workspace| workspace.strips.first())
                    .map(|strip| strip.id.0)
                    .expect("a strip")
            });
            self.handle
                .apply_command(&DesktopCommand::RenameStrip {
                    strip,
                    name: name.to_owned(),
                })
                .expect("the strip takes the name");
        }

        /// Follow the history as the person's app does.
        fn follow(&self) -> InvocationId {
            let invocation = InvocationId::new();
            let InvokeArgument::Json(query) = wire::argument(&wire::FeedQuery::default()) else {
                panic!("the feed takes a query");
            };
            self.activity
                .requests()
                .follow(&person(), invocation, wire::FEED, query)
                .expect("the feed is followed");
            invocation
        }

        /// What the app following `invocation` last read.
        fn feed(&self, invocation: &InvocationId) -> wire::Feed {
            let read = self
                .activity
                .requests()
                .served(invocation)
                .expect("the history is served")
                .expect("a feed");
            serde_json::from_value(read).expect("a feed")
        }
    }

    /// Lines an agent appends to `body` of `notebook` as one run, each a change of its own a second
    /// after the last.
    #[cfg(target_os = "linux")]
    fn agent_appends(
        notebook: &mut ui::automerge::AutoCommit,
        body: &ui::automerge::ObjId,
        time: &mut i64,
        lines: usize,
    ) {
        use ui::automerge::ReadDoc as _;
        use ui::automerge::transaction::Transactable as _;
        for line in 0..lines {
            *time = time.saturating_add(1);
            let wrote = tilo_actions::ActionContext {
                invoker: Some(
                    tilo_actions::ActionInvoker::agent("Claude Code").acting_for("ada".to_owned()),
                ),
                what: Some(tilo_actions::Did::Verb(tilo_actions::VerbCall::new(
                    tilo_actions::ActionKey::new(
                        "so.tilo.notes",
                        "notes.notebook",
                        "append-paragraph",
                    ),
                    std::iter::empty(),
                ))),
                ..tilo_actions::ActionContext::default()
            }
            .with_group("run-7", "Claude Code", "agent-session");
            let end = notebook.length(body);
            notebook
                .splice_text(body, end, 0, &format!("Launch notes, line {line}.\n"))
                .unwrap();
            notebook.commit_with(
                ui::automerge::transaction::CommitOptions::default()
                    .with_message(wrote.change_message().expect("a message"))
                    .with_time(*time),
            );
        }
    }

    /// An agent's work in an app's own record (Notes' notebook is the workspace's Notes document,
    /// which also keeps its index) is one of its runs, with what it did: the record a host opens
    /// for an agent is that one. Its card lists the run, and the notice that it finished counts
    /// the record it changed. Each time it writes again, the history reads what it added and
    /// lists that too.
    ///
    /// The session is seeded in the workspace as a page in it seeds its own, and what the host
    /// delivers is kept by the workspace's id, as a page's replica keeps it: the session's workspace
    /// is that one, so its history finds the agent's writes.
    /// A session whose one workspace is `workspace`'s, seeded with the notebook as a page seeds
    /// its own.
    fn seeded(workspace: &str) -> Session {
        Session::seeded(|minter, notes| {
            tilo_session_core::desktop::seed_tree_in(
                minter,
                WorkspaceId::parse(workspace).expect("a workspace id"),
                "Workspace",
                vec![notes],
            )
        })
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_agents_run_in_an_apps_own_record_is_listed_with_its_changes() {
        use ui::automerge::transaction::Transactable as _;
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(app_state::FsStore::new(dir.path())) as Store;
        let workspace = "7c307c8d-bfa5-46fa-ae08-fa2fe4790bd3";
        let mut session = seeded(workspace);
        // The notebook as a host keeps it: what the agent wrote, for the person, in its run.
        let mut notebook = ui::automerge::AutoCommit::new();
        let body = notebook
            .put_object(ui::automerge::ROOT, "body", ui::automerge::ObjType::Text)
            .unwrap();
        notebook.commit();
        let key = StorageKey::workspace_index("so.tilo.notes", workspace);
        let _lease = store.lease(&key).unwrap();
        // Each write is noted on the one stream of record changes, as the store's writers note it.
        let changes = crate::platform::records::Changes::default();
        let mut time = 0;
        let mut write = |notebook: &mut ui::automerge::AutoCommit, lines: usize| {
            agent_appends(notebook, &body, &mut time, lines);
            store
                .write(&key, WriteOp::Replace(notebook.save()), None)
                .unwrap();
            changes.note(workspace, "so.tilo.notes", "", false);
        };
        write(&mut notebook, 2);

        session.activity = Activity::over(Given {
            layouts: Box::new(session.records.clone()),
            person: "ada".into(),
            store: Some(Arc::clone(&store)),
            held: Some(Arc::new(InStore::over(Arc::clone(&store), changes.clone()))),
            state: None,
            apps: None,
            people: crate::platform::authors::unnamed(),
            writers: Writers::untracked(),
            pane: Rc::new(crate::platform::pane::Linked),
            models: None,
            changes: changes.clone(),
            wake: None,
        });
        session
            .pass
            .set_history_source(Box::new(session.activity.clone()));
        session.pass.serve_activity(session.activity.requests());
        session.open_history();
        let invocation = InvocationId::new();
        let InvokeArgument::Json(query) = wire::argument(&wire::SessionsQuery::default()) else {
            panic!("the runs take a query");
        };
        session
            .activity
            .requests()
            .follow(&person(), invocation, wire::SESSIONS, query)
            .expect("the runs are followed");
        // The records are read again on a worker once the stream says they moved: a pass lists
        // what that read found once it landed.
        let mut runs_until = |landed: &dyn Fn(&wire::AgentSession) -> bool| -> wire::Sessions {
            for _ in 0..300 {
                session.frame();
                let runs: wire::Sessions = serde_json::from_value(
                    session
                        .activity
                        .requests()
                        .served(&invocation)
                        .expect("the runs are served")
                        .expect("runs"),
                )
                .expect("runs");
                if runs.sessions.iter().any(landed) {
                    return runs;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("the agent's run is never listed");
        };
        let first = runs_until(&|run| run.last_ms == 2_000);
        let [run] = first.sessions.as_slice() else {
            panic!("the agent's one run: {first:?}");
        };
        assert_eq!(run.agent.name, "Claude Code", "{run:?}");
        let [action] = run.actions.as_slice() else {
            panic!("one action, in the notebook: {run:?}");
        };
        assert_eq!(action.app_id, "so.tilo.notes", "{action:?}");
        assert_eq!(action.changes, 2, "its two writes: {action:?}");
        assert_eq!(
            action.record,
            ui::resource::DocumentId::package_primary().as_str(),
            "{action:?}"
        );
        assert_eq!(
            run.changes(),
            2,
            "the run counts the two changes it made in the notebook"
        );

        // It writes again: the run ends at what it added, its fifth change.
        write(&mut notebook, 3);
        let again = runs_until(&|run| run.last_ms == 5_000);
        assert_eq!(
            again.sessions.len(),
            1,
            "still the agent's one run: {again:?}"
        );
    }

    /// A workspace's history names each member as the workspace does: a change one of Ada's
    /// devices signed (naming the workspace, adding a member) reads as Ada, and one her agent
    /// made reads under the name the workspace's record of its agents gives it, not its id.
    #[cfg(target_os = "linux")]
    #[test]
    fn changes_read_under_the_names_the_workspace_gives_its_members() {
        let ada = collab_types::MemberId::person("ada");
        let agent = "claude-code";
        let device = hex::encode([7_u8; 32]);
        let session = Session::new();
        session.activity.set_people(
            &session.workspace.to_string(),
            Members {
                members: vec![
                    collab_types::Member::named(ada.clone(), "Ada"),
                    collab_types::Member::of_software(
                        collab_types::MemberId::agent(agent),
                        "Claude Code",
                    ),
                ],
                writers: vec![(ada, vec![device.clone()])],
            },
        );
        let mut authors = ui::AuthorDirectory::new();
        session
            .activity
            .register_authors(session.workspace, &mut authors);

        assert_eq!(authors.resolve_entry(&device, None).name, "Ada");
        let invoker = tilo_actions::ActionInvoker::admitted(
            &tilo_actions::Acting::agent(agent).acting_for(tilo_actions::Acting::person("ada")),
        );
        let context = tilo_actions::ActionContext::default().with_invoker(invoker);
        let writer = hex::encode([9_u8; 32]);
        assert_eq!(
            authors.resolve_entry(&writer, Some(&context)).name,
            "Claude Code"
        );
    }

    /// The activity opens from its command and lists the workspace's real changes.
    #[test]
    fn the_activity_pane_opens_from_its_command_and_lists_a_real_edit() {
        let mut session = Session::new();
        session.open_history();
        let following = session.follow();
        session.frame();
        assert_eq!(
            session.feed(&following).entries.len(),
            0,
            "the session as it was opened is not a change"
        );

        // A workspace's name is its governance's, so naming it changes no record of it;
        // naming a strip does.
        session.name_strip("Plans");
        session.frame();
        let feed = session.feed(&following);
        let [named] = feed.entries.as_slice() else {
            panic!("naming the strip is one row: {:?}", feed.entries);
        };
        // Whoever wrote it, a row names the person reading it as such: the wire says the change is
        // the reader's own, and words it as every history row does.
        assert_eq!(
            named.author.relation,
            wire::Relation::Reader,
            "and who did it: the person here"
        );
        assert_eq!(
            named.author.name, "ada",
            "the row carries the account's name, what its face and the search read"
        );
        assert_eq!(
            ui::wire_author_line(&named.author),
            "You",
            "and the line words the person reading it as such, never by their account's name"
        );
        assert!(
            feed.mutations,
            "the session's own layout is the person's to undo"
        );
    }

    /// Nothing moved, so no pass asks for a frame: a change says when the model is read again.
    #[test]
    fn a_pass_with_nothing_new_asks_for_no_frame() {
        let mut session = Session::new();
        session.open_history();
        let _following = session.follow();
        assert!(
            session.activity.take_woken(),
            "the window asking for its history asks for the frame that serves it"
        );
        for _ in 0..3 {
            session.frame();
        }
        assert!(
            !session.activity.take_woken(),
            "nothing moved, so nothing asks for a frame"
        );
    }

    /// A change of a workspace's layout is one step of the person's in its own history: the person
    /// takes it back by their name, and another person's undo never reaches it.
    #[test]
    fn a_change_of_the_layout_is_one_step_of_the_persons_in_its_history() {
        use tilo_desktop_model::shared_resource;
        use ui::permissions::MemberId;
        use ui::state::UndoLog;

        let mut doc = ui::automerge::AutoCommit::new();
        doc.set_actor(tilo_session_core::layout::actor_of_person("ada"));
        let mut log = UndoLog::new(&mut doc);
        let before = doc.base_heads();
        // The person opens a window: one change of the workspace's layout.
        let tree = Session::new();
        let opened = tree.handle.read(Clone::clone);
        let layout = tilo_desktop_model::ownership::SharedDesktop {
            tree: opened,
            ..Default::default()
        };
        assert!(
            shared_resource::write(&mut doc, &layout).expect("a record that takes the layout"),
            "the layout holds the window"
        );
        shared_resource::commit_change(&mut doc, "ada", now_seconds(), None);
        assert!(log.record_committed_action_authored(
            &mut doc,
            before,
            None,
            None,
            Some(MemberId::person("ada"))
        ));

        let ada = MemberId::person("ada");
        let bob = MemberId::person("bob");
        assert!(
            log.can_undo_as(&doc, Some(&ada)),
            "the person takes it back"
        );
        assert!(
            !log.can_undo_as(&doc, Some(&bob)),
            "another person's undo never reaches it"
        );
        assert!(
            log.undo_as(&mut doc, Some(&ada), None, None).is_some(),
            "the step is one change of the layout"
        );
        // Undoing the change that founded the layout leaves none: an empty one.
        let left = shared_resource::read(&doc, None).map_or(0, |undone| windows(&undone.tree));
        assert_eq!(left, 0, "the window the step opened is gone");
    }

    /// Every window `tree` holds.
    fn windows(tree: &Layout) -> usize {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .map(|group| group.tabs.len())
            .sum()
    }

    /// A version restored from the history reaches the session, as a change of it.
    #[test]
    fn a_restore_from_the_history_writes_the_shell_back() {
        let mut session = Session::new();
        session.open_history();
        let following = session.follow();
        session.frame();
        let strip = |session: &Session| {
            session.handle.read(|tree| {
                tree.active_workspace()
                    .and_then(|workspace| workspace.strips.first())
                    .map(|strip| strip.name.clone())
                    .unwrap_or_default()
            })
        };
        let before: Vec<String> = session
            .feed(&following)
            .entries
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        session.name_strip("Plans");
        session.frame();
        let feed = session.feed(&following);
        let named: Vec<_> = feed
            .entries
            .iter()
            .filter(|entry| !before.contains(&entry.id))
            .collect();
        let [plans] = named.as_slice() else {
            panic!("naming the strip is one change: {:?}", feed.entries);
        };
        let plans = plans.id.clone();
        session.name_strip("Lounge");
        session.frame();
        assert_eq!(strip(&session), "Lounge");

        let mut restored = session.activity.requests().act(
            wire::RESTORE,
            match serde_json::json!(wire::Range::one(&plans)).as_object() {
                Some(range) => range.clone(),
                None => panic!("a range"),
            },
        );
        session.frame();
        assert!(
            matches!(restored.poll_ready(), PendingPoll::Ready(Ok(()))),
            "the history takes the restore"
        );
        let restored = session
            .activity
            .take_restored()
            .expect("the model writes the session back");
        session.handle.replace_tree(restored);
        assert_eq!(
            strip(&session),
            "Plans",
            "the session is at the version restored"
        );

        // The restored session is recorded like any other change of it.
        session.frame();
        let feed = session.feed(&following);
        assert!(
            feed.entries.len() > 2,
            "the restore is a change on top: {:?}",
            feed.entries
        );
    }
}
