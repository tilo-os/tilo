//! Carrying a client window, and the document it shows, to another workspace.
//!
//! A workspace is a sandbox boundary: a running client stays inside the
//! sandbox, grants and collaborators of the workspace that launched it, and
//! its records are scoped to that workspace's room. So a window never
//! crosses by itself. What crosses is its **record** — the CRDT document with
//! its whole history — through the explicit record verb
//! [`ui::record::transfer`], plus the presentation that instance saved and
//! the continuity resume projection it last reported. A new instance opens
//! inside the destination's own sandbox on that record, and only once it has
//! a window does the old one close.
//!
//! Nothing starts without the person at this desktop agreeing on a sheet
//! that says what crosses, what stays and who loses access
//! ([`crate::shell::portal_dialog::WindowTransferRequest`]). Agents reach the
//! same flow through the `tab.move` action, so they get the same sheet.
//!
//! The order is what makes a move lossless:
//!
//! 1. Ask the running old instance for a checkpoint
//!    ([`ui::collaboration::handoff`]): it makes what it shows durable now.
//! 2. Read the source record as durable, rehome it into the destination
//!    under a fresh id, and give the old instance's presentation to the new
//!    one.
//! 3. Stage the continuity resume projection and launch the new instance
//!    into a new tab in the destination. If it never produces a window,
//!    everything rolls back: the new tab goes, the imported record is retired
//!    unopened, and the old window was never touched.
//! 4. Once the new window exists, a **Move** asks the old instance to stamp
//!    its document retired. That stamp is an ordinary change published over
//!    the record's own route, so every collaborator holding the record learns
//!    it moved, by whom and to where; their windows stop editing it and their
//!    desktops say so. Only then is the old window asked to close.
//! 5. When its process has let go of the record, whatever it wrote after
//!    step 2 and before its stamp is merged into the destination (a CRDT
//!    merge, so nothing is lost and the order cannot matter). If the new
//!    instance already holds the destination record, it is restarted around
//!    the merge, and the person is told their last edits came over. An
//!    instance that never answers — it crashed, or predates handoff — is
//!    retired at rest by the shell instead.
//! 6. A **Copy** stops after step 3: the source keeps its window and record,
//!    and the two histories diverge from there.
//!
//! A native app without records keeps one document for itself in the room
//! ([`ui::persist`]); that document is what its window carries, arriving as
//! a record the new instance opens by its saved presentation.
//!
//! An app that keeps its state only in its running process (an X11 or plain
//! Wayland client with no Tilo profile) cannot carry it. Its sheet says so
//! and offers a new, empty window in the destination; the old window is
//! never asked to close.
//!
//! Workspaces hosted by a room owner keep their records with that owner, not
//! in this device's app profiles, so a transfer into or out of one is refused
//! with that explanation.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ChangeHash};
use tokio::sync::oneshot;
use ui::application::NativeProfile;
use ui::application_view::SavedInstanceView;
use ui::collaboration::handoff::{HandoffReport, HandoffRequest, Successor};
use ui::record::transfer::{self, Origin, Retirement, TransferError};
use ui::record::{RecordCheckout, RecordId};

use crate::shell::chrome_state::ChromeState;
use crate::shell::document_import::HostedRooms;
use crate::shell::i18n::{t, t_args};
use crate::shell::personal_continuity::{AppInstanceId, LaunchIntent};
use crate::shell::portal_dialog::{
    PortalDialogQueue, WindowTransferChoice, WindowTransferDecision, WindowTransferRequest,
};
use crate::shell::record_hydration::profile_root;
use crate::shell::state::WorkspaceAuthority as _;
use crate::shell::state::{
    RoomId, StripId, Tab, TabId, TileContent, Workspace, WorkspaceId, WorkspaceTree,
};
use crate::shell::tab_binding::Placement;
use crate::shell::window_actions::MoveTabError;

/// How long a new instance may take to show a window before the transfer
/// rolls back, or before a restart around late edits gives up.
const OPEN_TIMEOUT: Duration = Duration::from_secs(30);

/// How long an old instance may take to answer a handoff step before the
/// transfer goes on without it: reading what it last saved, or stamping its
/// document retired at rest once it has gone.
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(3);

/// How often a transfer waiting on a process is looked at again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// What the person asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferMode {
    /// The document leaves the source workspace.
    Move,
    /// The destination gets its own copy, history included; the source keeps going.
    Copy,
}

/// Starts the destination instance. The production launcher publishes the
/// workspace's capabilities and spawns inside its sandbox; it consumes the
/// continuation staged for `tab`, exactly as a restored window's launch does.
pub trait TransferLauncher {
    /// # Errors
    /// Why nothing was started, in the person's language.
    fn launch(&mut self, app_id: &str, workspace: &Workspace, tab: TabId) -> Result<(), String>;
}

struct SessionLauncher;

impl TransferLauncher for SessionLauncher {
    fn launch(&mut self, app_id: &str, workspace: &Workspace, _tab: TabId) -> Result<(), String> {
        crate::shell::launcher::launch_in_workspace(app_id, workspace)
            .map(|_| ())
            .map_err(|error| error.user_message())
    }
}

/// One side of a transfer.
#[derive(Clone, Debug)]
struct Side {
    workspace: WorkspaceId,
    room: RoomId,
    name: String,
    /// Governed by a room owner: its records live with that owner, reached
    /// through [`HostedRooms`], not in this device's app profiles.
    hosted: bool,
}

/// What a window can carry across.
#[derive(Clone, Debug)]
enum Carry {
    /// A native app's record, the presentation its instance saved, and the
    /// resume projection it last reported.
    Record {
        instance: u64,
        origin: Origin,
        view: Box<SavedInstanceView>,
        resume: Option<(u16, Vec<u8>)>,
        /// Why Move is not offered, when it is not: another window still
        /// shows the record, or it cannot be retired where it is.
        move_blocked: Option<String>,
    },
    /// Nothing that can travel.
    Nothing,
}

/// What a transfer would do, worked out before anything changes.
#[derive(Clone, Debug)]
struct Plan {
    tab: TabId,
    /// The app's launchable package id.
    app_id: String,
    title: String,
    /// The old instance's surface, when it is running.
    surface: Option<u64>,
    source: Side,
    destination: Side,
    carry: Carry,
}

/// The record once it has arrived in the destination.
struct Carried {
    origin: Origin,
    target: RecordCheckout,
    known: Vec<ChangeHash>,
    source_root: PathBuf,
    destination_root: PathBuf,
    instance: u64,
    /// The source document's heads from just before its own app stamped it
    /// retired: the end of what still has to come along.
    retired_at: Option<Vec<ChangeHash>>,
}

impl Carried {
    /// The record the source room's activity names as the one that left:
    /// the record itself, or for an app's own document the record it became.
    fn left(&self) -> &RecordCheckout {
        match &self.origin {
            Origin::Record(record) => record,
            Origin::AppDocument(_) => &self.target,
        }
    }
}

enum Phase {
    Consent(oneshot::Receiver<WindowTransferDecision>),
    /// Waiting for the old instance to make what it shows durable, so the
    /// transfer reads it as the person left it.
    Checkpointing {
        mode: TransferMode,
        token: u64,
        since: Instant,
    },
    /// Waiting for the old instance to stamp its document retired over its
    /// own route, now that the new window exists.
    Retiring {
        tab: TabId,
        carried: Box<Carried>,
        token: u64,
        since: Instant,
    },
    /// Waiting for the destination instance to show a window.
    Opening {
        mode: TransferMode,
        tab: TabId,
        carried: Box<Carried>,
        since: Instant,
    },
    /// A move waiting for the old instance to let go of the record.
    Releasing {
        tab: TabId,
        carried: Box<Carried>,
    },
    /// The destination instance restarting so late edits can be merged in.
    Redelivering {
        tab: TabId,
        carried: Box<Carried>,
        source: Box<AutoCommit>,
        since: Instant,
    },
    /// Waiting for a governed destination's owner to accept the document,
    /// and for the record they accepted to arrive here.
    Proposing {
        mode: TransferMode,
        tab: TabId,
        carried: Box<Carried>,
        view: Box<SavedInstanceView>,
        resume: Option<(u16, Vec<u8>)>,
        wait: hosted::Wait,
        since: Instant,
    },
    /// The destination instance reopening after the merge.
    Reopening {
        tab: TabId,
        carried: Box<Carried>,
        since: Instant,
    },
}

struct Transfer {
    handle: String,
    plan: Plan,
    phase: Phase,
    /// The old instance went away before it was asked to.
    source_lost: bool,
}

/// Every transfer in flight on this desktop.
pub struct WindowTransfers {
    active: Vec<Transfer>,
    launcher: Box<dyn TransferLauncher>,
    queue: Option<PortalDialogQueue>,
    attended: Box<dyn Fn() -> bool>,
    data_home: Option<PathBuf>,
    handoff: Option<Box<dyn HandoffLink>>,
    /// Reports taken in this round, until a transfer claims them.
    notes: Vec<HandoffNote>,
    /// Documents moved to a workspace this person is in, being brought here
    /// so their window can follow them.
    followers: Vec<Follower>,
    /// Governed rooms, for transfers into and out of them.
    hosted: Option<Box<dyn HostedRooms>>,
    /// Proposals this desktop's person is being asked to accept as owner.
    consent: hosted::Consent,
    next: u64,
    polled: Option<Instant>,
}

/// Reaches one application instance by its primary route with a handoff
/// request ([`ui::collaboration::handoff`]). Its answer comes back through
/// [`report_handoff`].
pub trait HandoffLink {
    fn request(&self, room: RoomId, app_id: &str, instance: u64, request: HandoffRequest);
}

/// The session broker's reach to one instance's primary route.
pub struct BrokerHandoff(crate::shell::collab_broker::SessionFollowDispatcher);

impl BrokerHandoff {
    pub fn new(dispatcher: crate::shell::collab_broker::SessionFollowDispatcher) -> Self {
        Self(dispatcher)
    }
}

impl HandoffLink for BrokerHandoff {
    fn request(&self, room: RoomId, app_id: &str, instance: u64, request: HandoffRequest) {
        self.0
            .request_handoff(&room.to_string(), app_id, instance, request);
    }
}

/// One instance's handoff report, as the broker delivered it.
struct HandoffNote {
    room: RoomId,
    app_id: String,
    instance: u64,
    report: HandoffReport,
}

fn handoff_inbox() -> &'static std::sync::Mutex<std::collections::VecDeque<HandoffNote>> {
    static INBOX: std::sync::Mutex<std::collections::VecDeque<HandoffNote>> =
        std::sync::Mutex::new(std::collections::VecDeque::new());
    &INBOX
}

/// Take in one instance's handoff report. Called from the broker for the
/// instance's own certified primary route, so `room`, `app_id` and
/// `instance` are its own; the report is picked up on the next frame.
///
/// # Errors
/// A report that is not well-formed.
pub fn report_handoff(
    room: RoomId,
    app_id: String,
    instance: u64,
    message: &[u8],
) -> Result<(), String> {
    let report = HandoffReport::decode(message).ok_or("malformed handoff report")?;
    let mut inbox = handoff_inbox()
        .lock()
        .map_err(|_| "handoff inbox unavailable".to_string())?;
    // Bounded: a flood of reports must not grow without limit.
    if inbox.len() >= 256 {
        inbox.pop_front();
    }
    inbox.push_back(HandoffNote {
        room,
        app_id,
        instance,
        report,
    });
    drop(inbox);
    Ok(())
}

fn take_handoff_notes() -> Vec<HandoffNote> {
    handoff_inbox()
        .lock()
        .map(|mut inbox| inbox.drain(..).collect())
        .unwrap_or_default()
}

fn handoff_notes_waiting() -> bool {
    handoff_inbox().lock().is_ok_and(|inbox| !inbox.is_empty())
}

impl std::fmt::Debug for WindowTransfers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowTransfers")
            .field("active", &self.active.len())
            .finish_non_exhaustive()
    }
}

impl Default for WindowTransfers {
    fn default() -> Self {
        Self {
            active: Vec::new(),
            launcher: Box::new(SessionLauncher),
            queue: None,
            attended: Box::new(crate::shell::app_action_consent::session_attended),
            data_home: None,
            handoff: None,
            notes: Vec::new(),
            followers: Vec::new(),
            hosted: None,
            consent: hosted::Consent::default(),
            next: 0,
            polled: None,
        }
    }
}

impl WindowTransfers {
    /// Start destination instances through `launcher` instead of the session's.
    pub fn set_launcher(&mut self, launcher: Box<dyn TransferLauncher>) {
        self.launcher = launcher;
    }

    /// Ask on `queue`'s sheet instead of the session's.
    pub fn set_dialog_queue(&mut self, queue: PortalDialogQueue) {
        self.queue = Some(queue);
    }

    /// Whether someone is at this desktop to answer the sheet.
    pub fn set_attended(&mut self, attended: impl Fn() -> bool + 'static) {
        self.attended = Box::new(attended);
    }

    /// Where the per-room app profiles live, instead of the launcher's data home.
    pub fn set_data_home(&mut self, data_home: PathBuf) {
        self.data_home = Some(data_home);
    }

    /// Reach application instances through `link` for the handoff steps.
    /// Without one, a transfer reads what an instance last saved and leaves
    /// the retirement to the shell.
    pub fn set_handoff_link(&mut self, link: Box<dyn HandoffLink>) {
        self.handoff = Some(link);
    }

    /// Reach governed rooms through `hosted`, so windows can be carried into
    /// and out of them and this desktop's person can answer as an owner.
    pub fn set_hosted_rooms(&mut self, hosted: Box<dyn HostedRooms>) {
        self.hosted = Some(hosted);
    }

    /// Whether any transfer is in flight, including one waiting on its
    /// sheet, or an instance has reported something to act on.
    pub fn is_pending(&self) -> bool {
        !self.active.is_empty() || !self.followers.is_empty() || handoff_notes_waiting()
    }

    fn ask(&self, room: RoomId, app_id: &str, instance: u64, request: HandoffRequest) {
        if let Some(link) = self.handoff.as_ref() {
            link.request(room, app_id, instance, request);
        }
    }

    fn next_token(&mut self) -> u64 {
        self.next = self.next.wrapping_add(1);
        self.next
    }

    /// Claim the report answering `token` from `instance` of `app_id` in `room`.
    fn claim(
        &mut self,
        room: RoomId,
        app_id: &str,
        instance: u64,
        token: u64,
    ) -> Option<HandoffReport> {
        let index = self.notes.iter().position(|note| {
            note.room == room
                && note.app_id == app_id
                && note.instance == instance
                && matches!(
                    &note.report,
                    HandoffReport::Done { token: answered, .. }
                        | HandoffReport::Failed { token: answered, .. }
                        if *answered == token
                )
        })?;
        Some(self.notes.remove(index).report)
    }

    /// Whether a transfer is waiting on something no input announces — a
    /// window appearing, or a process letting go of its record — so the
    /// frame loop has to keep polling.
    pub fn is_busy(&self) -> bool {
        !self.followers.is_empty()
            || self.consent.is_busy()
            || self
                .active
                .iter()
                .any(|transfer| !matches!(transfer.phase, Phase::Consent(_)))
    }

    /// When the frame loop should run [`tick`] again, while a transfer is busy.
    pub fn next_poll(&self) -> Option<Instant> {
        if self.is_busy() || crate::shell::record_hydration::any_waiting() {
            return Some(self.polled.map_or_else(Instant::now, |polled| {
                polled.checked_add(POLL_INTERVAL).unwrap_or(polled)
            }));
        }
        // Proposals for this person to answer arrive with the room's state.
        self.hosted.as_ref().and_then(|_| self.consent.next_scan())
    }

    fn data_home(&self) -> Option<PathBuf> {
        self.data_home
            .clone()
            .or_else(::launcher::appdir::host_data_home)
    }
}
fn find_tab(tree: &WorkspaceTree, tab: TabId) -> Option<(&Workspace, &Tab)> {
    tree.desktop.workspaces.iter().find_map(|workspace| {
        workspace
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find(|candidate| candidate.id == tab)
            .map(|found| (workspace, found))
    })
}

/// The window's launchable app id and running surface, for a client tile.
fn client(tile: &TileContent) -> Option<(String, String, Option<u64>)> {
    match tile {
        TileContent::Foreign { app_id, surface_id } => Some((
            crate::shell::persist::durable_app_id(*surface_id, app_id),
            app_id.clone(),
            Some(*surface_id),
        )),
        TileContent::LocalApp { app_id, .. } => Some((app_id.clone(), app_id.clone(), None)),
        _ => None,
    }
}

/// The continuity instance a tab's process runs as.
fn instance_of(
    tree: &WorkspaceTree,
    tab: TabId,
    room: RoomId,
    tile_app: &str,
    app_id: &str,
) -> Option<u64> {
    tree.local_app_record(tab, room, tile_app).or_else(|| {
        let launch = LaunchIntent::app(app_id.to_owned()).ok()?;
        crate::shell::personal_continuity_registry::global_registry()
            .lock()
            .ok()?
            .record_instance_for_tab(room, AppInstanceId(tab), &launch)
            .map(|instance| instance.0)
    })
}

/// What `instance` of `app_id` shows in the profile at `directory`, when it
/// can travel: the record its saved presentation names, else the document
/// the app keeps for itself in the room ([`ui::persist`]), with the
/// presentation it carries along.
fn shown(
    directory: &Path,
    room: RoomId,
    app_id: &str,
    instance: u64,
    hosted: Option<&dyn HostedRooms>,
) -> Option<(Origin, SavedInstanceView)> {
    if !directory.is_dir() {
        return None;
    }
    let profile = NativeProfile::open(directory, Some(&room.to_string())).ok()?;
    // A governed room's records live with its owner; the app's own
    // document there never leaves it.
    let readable = |origin: &Origin| match (hosted, origin) {
        (Some(hosted), Origin::Record(checkout)) => hosted.read(room, app_id, &checkout.id).is_ok(),
        (Some(_), Origin::AppDocument(_)) => false,
        (None, origin) => {
            transfer::read(profile.store(), app_id, profile.workspace_id(), origin).is_ok()
        }
    };
    if let Some(view) = profile.saved_view(instance).ok().flatten()
        && view.app_id == app_id
        && let Some(id) = RecordId::parse(&view.record_id).filter(|id| !id.is_package_primary())
    {
        let origin = Origin::Record(RecordCheckout::new(
            id,
            ui::record::RecordKind::new(view.record_kind.clone()),
        ));
        if readable(&origin) {
            return Some((origin, view));
        }
    }
    let kind = ui::record::RecordKind::new(transfer::APP_DOCUMENT_KIND);
    let origin = Origin::AppDocument(kind.clone());
    readable(&origin).then(|| {
        let view = SavedInstanceView {
            version: 2,
            app_id: app_id.to_owned(),
            device_id: profile.device_id().to_owned(),
            workspace_id: profile.workspace_id().to_owned(),
            instance_id: instance,
            record_id: RecordId::for_tab(instance).as_str().to_owned(),
            record_kind: kind.as_str().to_owned(),
            view: ui::view::View::default(),
            application: None,
        };
        (origin, view)
    })
}

fn plan(
    tree: &WorkspaceTree,
    tab: TabId,
    target: WorkspaceId,
    data_home: Option<&Path>,
    hosted: Option<&dyn HostedRooms>,
) -> Result<Plan, MoveTabError> {
    let (source_ws, found) = find_tab(tree, tab).ok_or(MoveTabError::UnknownTab)?;
    let (app_id, tile_app, surface) = client(&found.tile).ok_or(MoveTabError::Pinned)?;
    let destination_ws = tree
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.id == target)
        .filter(|workspace| !workspace.strips.is_empty())
        .ok_or(MoveTabError::UnknownWorkspace)?;
    if source_ws.id == target {
        return Err(MoveTabError::SameWorkspace);
    }
    let governed = |room: RoomId| {
        tree.requires_native_owner(room) || hosted.is_some_and(|hosted| hosted.hosts(room))
    };
    let source_hosted = governed(source_ws.room);
    if (source_hosted || governed(destination_ws.room)) && hosted.is_none() {
        return Err(MoveTabError::HostedWorkspace);
    }
    // The person carrying the window must be allowed to open this app in
    // the destination; the destination's own authority governs it there.
    if destination_ws.owner.is_some() {
        let me = crate::shell::state::local_user_id_for_room(&destination_ws.room);
        let actor = destination_ws.actor_for_person(&me);
        if !destination_ws.allows(
            &actor,
            &crate::shell::state::ResourceRef::App(app_id.clone()),
            crate::shell::state::Verb::Write,
        ) {
            return Err(MoveTabError::NotPermitted);
        }
    }
    let side = |workspace: &Workspace| Side {
        workspace: workspace.id,
        room: workspace.room,
        name: workspace.name.clone(),
        hosted: governed(workspace.room),
    };
    let source = side(source_ws);
    let carry = data_home
        .and_then(|home| {
            let instance = instance_of(tree, tab, source.room, &tile_app, &app_id)?;
            let root = profile_root(home, source.room, &app_id);
            let source_reader = hosted.filter(|_| source_hosted);
            let (origin, view) = shown(&root, source.room, &app_id, instance, source_reader)?;
            let launch = LaunchIntent::app(app_id.clone()).ok()?;
            let resume = crate::shell::personal_continuity_registry::global_registry()
                .lock()
                .ok()?
                .continuation_for_tab(source.room, AppInstanceId(tab), &launch)
                .map(|found| (found.resume.schema_version(), found.resume.bytes().to_vec()));
            let move_blocked = source_ws
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .filter(|other| other.id != tab)
                .filter_map(|other| {
                    let (other_app, other_tile_app, _) = client(&other.tile)?;
                    (other_app == app_id).then_some((other.id, other_tile_app))
                })
                .any(|(other, other_tile_app)| {
                    match instance_of(tree, other, source.room, &other_tile_app, &app_id) {
                        Some(other) => shown(&root, source.room, &app_id, other, source_reader)
                            .is_some_and(|(other, _)| other == origin),
                        // An instance nothing is known about shares the app's
                        // own document, the one thing every window opens.
                        None => matches!(origin, Origin::AppDocument(_)),
                    }
                });
            let from = source.name.as_str();
            let move_blocked = if move_blocked {
                Some(t_args(
                    "window-transfer-move-blocked",
                    &[("from", from.into())],
                ))
            } else if source_hosted && surface.is_none() {
                // Only a running instance can mark a governed record moved,
                // through its own route; this desktop cannot write there.
                Some(t_args(
                    "window-transfer-move-needs-window",
                    &[("from", from.into())],
                ))
            } else {
                None
            };
            Some(Carry::Record {
                instance,
                origin,
                view: Box::new(view),
                resume,
                move_blocked,
            })
        })
        .unwrap_or(Carry::Nothing);
    Ok(Plan {
        tab,
        app_id,
        title: found.title.clone(),
        surface,
        source,
        destination: side(destination_ws),
        carry,
    })
}

/// The people other than this desktop's person who can open things in `workspace`.
fn others_with_access(workspace: &Workspace, tree: &WorkspaceTree) -> Vec<String> {
    let me = crate::shell::state::local_user_id_for_room(&workspace.room);
    let mut people: Vec<String> = tilo_desktop::overview::room_people(
        workspace,
        &tree.desktop,
        &crate::shell::overview::ShellOverview::for_user(&me),
        tilo_clock::unix_now(),
    )
    .into_iter()
    .filter(|person| !person.is_you)
    .map(|person| person.name)
    .collect();
    people.sort();
    people.dedup();
    people
}

/// Who can open the document on each side, and what stays behind.
fn access(tree: &WorkspaceTree, source: &Workspace, plan: &Plan) -> Vec<String> {
    let from = plan.source.name.as_str();
    let to = plan.destination.name.as_str();
    let people = others_with_access(source, tree);
    let mut lines = vec![
        if people.is_empty() {
            t_args("window-transfer-access-only-you", &[("from", from.into())])
        } else {
            t_args(
                "window-transfer-access-others",
                &[("from", from.into()), ("people", people.join(", ").into())],
            )
        },
        t_args("window-transfer-access-destination", &[("to", to.into())]),
    ];
    let grants: Vec<String> = source
        .app_capabilities(&plan.app_id)
        .into_iter()
        .map(|cap| {
            tilo_desktop::workspace_settings::scope_label(
                &cap.scope,
                &crate::shell::launcher::ShellApps,
            )
        })
        .collect();
    if !grants.is_empty() {
        lines.push(t_args(
            "window-transfer-grants-stay",
            &[("from", from.into()), ("grants", grants.join(", ").into())],
        ));
    }
    lines
}

fn sheet(
    tree: &WorkspaceTree,
    plan: &Plan,
    handle: String,
    preferred: TransferMode,
) -> WindowTransferRequest {
    let workspace = |id: WorkspaceId| tree.desktop.workspaces.iter().find(|ws| ws.id == id);
    let window = plan.title.as_str();
    let from = plan.source.name.as_str();
    let to = plan.destination.name.as_str();
    match &plan.carry {
        Carry::Record { move_blocked, .. } => {
            let mut detail = vec![
                t_args(
                    "window-transfer-carries",
                    &[("window", window.into()), ("to", to.into())],
                ),
                t_args(
                    "window-transfer-move-detail",
                    &[("from", from.into()), ("to", to.into())],
                ),
                t_args(
                    "window-transfer-copy-detail",
                    &[("from", from.into()), ("to", to.into())],
                ),
            ];
            if let Some(source) = workspace(plan.source.workspace) {
                detail.extend(access(tree, source, plan));
            }
            if plan.destination.hosted {
                detail.push(t_args(
                    "window-transfer-owner-accepts",
                    &[("to", to.into())],
                ));
            }
            let mut choices = Vec::new();
            if let Some(reason) = move_blocked {
                detail.push(reason.clone());
            } else {
                choices.push(WindowTransferChoice {
                    decision: WindowTransferDecision::Move,
                    label: t_args("window-transfer-choice-move", &[("from", from.into())]),
                });
            }
            choices.push(WindowTransferChoice {
                decision: WindowTransferDecision::Copy,
                label: t_args("window-transfer-choice-copy", &[("from", from.into())]),
            });
            if preferred == TransferMode::Copy {
                choices.reverse();
            }
            WindowTransferRequest {
                handle,
                app_id: plan.app_id.clone(),
                title: t_args(
                    "window-transfer-title",
                    &[("window", window.into()), ("to", to.into())],
                ),
                detail: detail.join("\n"),
                choices,
                confirm_label: t("window-transfer-confirm"),
            }
        }
        Carry::Nothing => WindowTransferRequest {
            handle,
            app_id: plan.app_id.clone(),
            title: t_args(
                "window-transfer-fresh-title",
                &[("window", window.into()), ("to", to.into())],
            ),
            detail: t_args(
                "window-transfer-fresh-detail",
                &[
                    ("window", window.into()),
                    ("from", from.into()),
                    ("to", to.into()),
                ],
            ),
            choices: vec![WindowTransferChoice {
                decision: WindowTransferDecision::OpenFresh,
                label: t_args("window-transfer-choice-fresh", &[("to", to.into())]),
            }],
            confirm_label: t("window-transfer-fresh-confirm"),
        },
    }
}

/// Ask to carry `tab` into `target`. Nothing changes until the person
/// answers the sheet this queues.
///
/// # Errors
/// Why the window cannot be carried at all.
pub fn request(
    st: &mut ChromeState,
    tab: TabId,
    target: WorkspaceId,
    preferred: TransferMode,
) -> Result<(), MoveTabError> {
    if st
        .window_transfers
        .active
        .iter()
        .any(|transfer| transfer.plan.tab == tab)
    {
        return Err(MoveTabError::AlreadyMoving);
    }
    if !(st.window_transfers.attended)() {
        return Err(MoveTabError::NobodyToConfirm);
    }
    let data_home = st.window_transfers.data_home();
    st.window_transfers.next = st.window_transfers.next.wrapping_add(1);
    let handle = format!("window-transfer:{}:{tab}", st.window_transfers.next);
    let tree = st.tree();
    let plan = plan(
        &tree,
        tab,
        target,
        data_home.as_deref(),
        st.window_transfers.hosted.as_deref(),
    )?;
    let request = sheet(&tree, &plan, handle, preferred);
    drop(tree);
    let handle = request.handle.clone();
    let receiver = st
        .window_transfers
        .queue
        .clone()
        .unwrap_or_else(PortalDialogQueue::global)
        .enqueue_window_transfer(request)
        .map_err(|_| MoveTabError::NobodyToConfirm)?;
    st.window_transfers.active.push(Transfer {
        handle,
        plan,
        phase: Phase::Consent(receiver),
        source_lost: false,
    });
    st.bump();
    Ok(())
}

/// Advance every transfer in flight. The frame loop calls this each round.
pub fn tick(st: &mut ChromeState) {
    let notes = take_handoff_notes();
    let consent_due = st.window_transfers.hosted.is_some()
        && (st.window_transfers.consent.due() || st.window_transfers.consent.is_busy());
    if st.window_transfers.active.is_empty()
        && st.window_transfers.followers.is_empty()
        && notes.is_empty()
        && !consent_due
    {
        return;
    }
    let mut transfers = std::mem::take(&mut st.window_transfers);
    transfers.polled = Some(Instant::now());
    transfers.notes.extend(notes);
    let active = std::mem::take(&mut transfers.active);
    for transfer in active {
        if let Some(next) = advance(st, &mut transfers, transfer) {
            transfers.active.push(next);
        }
    }
    // Reports no transfer here was waiting for: a document someone else
    // moved, arriving in a window of this desktop.
    for note in std::mem::take(&mut transfers.notes) {
        if let HandoffReport::Retired { successor } = &note.report {
            moved_away(st, &mut transfers, &note, successor);
        }
    }
    for follower in std::mem::take(&mut transfers.followers) {
        if let Some(waiting) = follow(st, &transfers, follower) {
            transfers.followers.push(waiting);
        }
    }
    hosted::consent(st, &mut transfers);
    // Anything queued while this ran (a nested request) is kept.
    transfers.active.append(&mut st.window_transfers.active);
    st.window_transfers = transfers;
}

/// A moved document being brought here so its window can follow it.
struct Follower {
    address: crate::shell::record_hydration::RecordAddress,
    workspace: WorkspaceId,
    title: String,
}

/// Bring a moved document here and, once it is kept, open it in its new
/// workspace as a window of this person's own, without taking them there.
fn follow(
    st: &mut ChromeState,
    transfers: &WindowTransfers,
    follower: Follower,
) -> Option<Follower> {
    use crate::shell::record_hydration::{Hydration, forget, hydrate, kept_here, reopen_on};
    let data_home = transfers.data_home()?;
    let record = if kept_here(&follower.address, &data_home) {
        None
    } else {
        match hydrate(&follower.address, &data_home) {
            Hydration::Waiting => return Some(follower),
            Hydration::Kept(record) => Some(record),
            Hydration::Failed(reason) => {
                tracing::warn!(%reason, app = %follower.address.app_id, "a moved document could not be brought here");
                forget(&follower.address);
                return None;
            }
        }
    };
    forget(&follower.address);
    let instance = crate::shell::state::next_id();
    reopen_on(&follower.address, &data_home, instance, record.as_ref());
    let strip = destination_strip(st, follower.workspace)?;
    let tab = place_in(
        st,
        follower.workspace,
        strip,
        Tab {
            id: crate::shell::state::next_id(),
            title: follower.title.clone(),
            tile: TileContent::LocalApp {
                app_id: follower.address.app_id.clone(),
                label: follower.title,
                note: String::new(),
            },
        },
    );
    // Its launch opens this instance, which reopens on the record brought.
    let mut tree = st.tree_mut();
    tree.remember_local_app_record(tab, instance);
    tree.queue_save();
    drop(tree);
    None
}

/// Tell the person that the document a window of theirs shows was moved to
/// another workspace, and by whom. Where it went is named only when they
/// are in that workspace too.
fn moved_away(
    st: &mut ChromeState,
    transfers: &mut WindowTransfers,
    note: &HandoffNote,
    successor: &Successor,
) {
    let (window, person, destination) = {
        let tree = st.tree();
        let source = tree
            .desktop
            .workspaces
            .iter()
            .find(|ws| ws.room == note.room);
        let window = source
            .and_then(|ws| {
                ws.strips
                    .iter()
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .find(|tab| {
                        client(&tab.tile).is_some_and(|(app, tile_app, _)| {
                            app == note.app_id
                                && instance_of(&tree, tab.id, ws.room, &tile_app, &app)
                                    == Some(note.instance)
                        })
                    })
                    .map(|tab| tab.title.clone())
            })
            .unwrap_or_else(|| crate::shell::tab_binding::tab_label_for_foreign(&note.app_id));
        let person = source
            .and_then(|ws| {
                let me = crate::shell::state::local_user_id_for_room(&ws.room);
                tilo_desktop::overview::room_people(
                    ws,
                    &tree.desktop,
                    &crate::shell::overview::ShellOverview::for_user(&me),
                    tilo_clock::unix_now(),
                )
                .into_iter()
                .find(|person| person.id == successor.by)
                .map(|person| person.name)
            })
            .unwrap_or_else(|| successor.by.clone());
        let destination = tree
            .desktop
            .workspaces
            .iter()
            .find(|ws| ws.room.to_string() == successor.room)
            .map(|ws| (ws.id, ws.room, ws.name.clone()));
        drop(tree);
        (window, person, destination)
    };
    let body = match destination {
        Some((workspace, room, to)) => {
            // The person can see where it went: their window follows it.
            if let Some(record) = RecordId::parse(&successor.record) {
                transfers.followers.push(Follower {
                    address: crate::shell::record_hydration::RecordAddress {
                        room,
                        app_id: note.app_id.clone(),
                        record,
                    },
                    workspace,
                    title: window.clone(),
                });
            }
            t_args(
                "window-transfer-moved-away-to",
                &[
                    ("person", person.as_str().into()),
                    ("to", to.as_str().into()),
                ],
            )
        }
        None => t_args(
            "window-transfer-moved-away-elsewhere",
            &[("person", person.as_str().into())],
        ),
    };
    st.push_notification(
        t_args(
            "window-transfer-moved-away-title",
            &[("window", window.as_str().into())],
        ),
        body,
    );
}

fn tab_tile(st: &ChromeState, tab: TabId) -> Option<TileContent> {
    let tree = st.tree();
    find_tab(&tree, tab).map(|(_, found)| found.tile.clone())
}

#[expect(
    clippy::too_many_lines,
    reason = "one match over the transfer phases, each arm a single step"
)]
fn advance(
    st: &mut ChromeState,
    transfers: &mut WindowTransfers,
    mut transfer: Transfer,
) -> Option<Transfer> {
    if let Some(surface) = transfer.plan.surface
        && !matches!(
            transfer.phase,
            Phase::Releasing { .. } | Phase::Redelivering { .. } | Phase::Reopening { .. }
        )
        && !matches!(
            tab_tile(st, transfer.plan.tab),
            Some(TileContent::Foreign { surface_id, .. }) if surface_id == surface
        )
    {
        transfer.source_lost = true;
    }
    let Transfer {
        handle,
        plan,
        phase,
        source_lost,
    } = transfer;
    let phase = match phase {
        Phase::Consent(mut receiver) => match receiver.try_recv() {
            Err(oneshot::error::TryRecvError::Empty) => Some(Phase::Consent(receiver)),
            Err(oneshot::error::TryRecvError::Closed) | Ok(WindowTransferDecision::Cancelled) => {
                None
            }
            Ok(WindowTransferDecision::OpenFresh) => {
                open_fresh(st, &plan);
                None
            }
            Ok(WindowTransferDecision::Move) => {
                checkpoint(st, transfers, &plan, TransferMode::Move, source_lost)
            }
            Ok(WindowTransferDecision::Copy) => {
                checkpoint(st, transfers, &plan, TransferMode::Copy, source_lost)
            }
        },
        Phase::Checkpointing { mode, token, since } => {
            let answered = transfers
                .claim(plan.source.room, &plan.app_id, instance(&plan), token)
                .is_some();
            // An answer either way, the old instance gone, or no answer in
            // time: read what is durable now. Anything written after that
            // still comes along once the old instance lets go.
            if answered || source_lost || since.elapsed() >= HANDOFF_TIMEOUT {
                begin(st, transfers, &plan, mode)
            } else {
                Some(Phase::Checkpointing { mode, token, since })
            }
        }
        Phase::Retiring {
            tab,
            mut carried,
            token,
            since,
        } => match transfers.claim(plan.source.room, &plan.app_id, carried.instance, token) {
            Some(report) => {
                carried.retired_at = report.heads();
                close_source(st, transfers, &plan, tab, carried, source_lost)
            }
            None if source_lost || since.elapsed() >= HANDOFF_TIMEOUT => {
                close_source(st, transfers, &plan, tab, carried, source_lost)
            }
            None => Some(Phase::Retiring {
                tab,
                carried,
                token,
                since,
            }),
        },
        Phase::Opening {
            mode,
            tab,
            carried,
            since,
        } => match tab_tile(st, tab) {
            Some(TileContent::Foreign { .. }) => {
                opened(st, transfers, &plan, mode, tab, carried, source_lost)
            }
            Some(TileContent::LocalApp { note, .. })
                if note == crate::shell::peer_windows::OPENING
                    && since.elapsed() < OPEN_TIMEOUT =>
            {
                Some(Phase::Opening {
                    mode,
                    tab,
                    carried,
                    since,
                })
            }
            Some(TileContent::LocalApp { note, .. })
                if note != crate::shell::peer_windows::OPENING =>
            {
                roll_back(st, &plan, Some(tab), &carried, &note);
                None
            }
            Some(_) => {
                roll_back(
                    st,
                    &plan,
                    Some(tab),
                    &carried,
                    &t("window-transfer-failed-timeout"),
                );
                None
            }
            None => {
                roll_back(
                    st,
                    &plan,
                    None,
                    &carried,
                    &t("window-transfer-failed-closed"),
                );
                None
            }
        },
        Phase::Releasing { tab, carried } => {
            release(st, transfers, &plan, tab, carried, source_lost)
        }
        Phase::Proposing {
            mode,
            tab,
            carried,
            view,
            resume,
            wait,
            since,
        } => hosted::proposing(
            st,
            transfers,
            &plan,
            hosted::Waiting {
                mode,
                tab,
                carried,
                view,
                resume,
                wait,
                since,
            },
        ),
        Phase::Redelivering {
            tab,
            carried,
            source,
            since,
        } => redeliver(
            st,
            transfers,
            &plan,
            tab,
            carried,
            source,
            since,
            source_lost,
        ),
        Phase::Reopening {
            tab,
            carried,
            since,
        } => match tab_tile(st, tab) {
            Some(TileContent::Foreign { .. }) => {
                finish_move(st, &plan, &carried, true, source_lost);
                None
            }
            Some(TileContent::LocalApp { note, .. })
                if note == crate::shell::peer_windows::OPENING
                    && since.elapsed() < OPEN_TIMEOUT =>
            {
                Some(Phase::Reopening {
                    tab,
                    carried,
                    since,
                })
            }
            _ => {
                // The document is already whole in the destination; only its
                // window failed. Leave the card so it can be reopened.
                set_note(st, tab, t("window-transfer-reopen-failed"));
                finish_move(st, &plan, &carried, true, source_lost);
                None
            }
        },
    };
    phase.map(|phase| Transfer {
        handle,
        plan,
        phase,
        source_lost,
    })
}

#[path = "window_transfer_hosted.rs"]
mod hosted;

/// A legacy window stays where it is; the destination gets a new, empty one.
fn open_fresh(st: &mut ChromeState, plan: &Plan) {
    let Some(strip) = destination_strip(st, plan.destination.workspace) else {
        failed(st, plan, &t("window-move-unknown-workspace"));
        return;
    };
    place_in(
        st,
        plan.destination.workspace,
        strip,
        Tab {
            id: crate::shell::state::next_id(),
            title: plan.title.clone(),
            tile: TileContent::LocalApp {
                app_id: plan.app_id.clone(),
                label: plan.title.clone(),
                note: String::new(),
            },
        },
    );
}

fn destination_strip(st: &ChromeState, workspace: WorkspaceId) -> Option<StripId> {
    let tree = st.tree();
    let current = tree.desktop.active_strip.get(&workspace).copied();
    tree.desktop
        .workspaces
        .iter()
        .find(|ws| ws.id == workspace)
        .and_then(|ws| {
            ws.strips
                .iter()
                .find(|strip| Some(strip.id) == current)
                .or_else(|| ws.strips.first())
                .map(|strip| strip.id)
        })
}

/// Put `tab` in a new group at the end of `strip` in `workspace` without
/// taking the person there.
fn place_in(st: &mut ChromeState, workspace: WorkspaceId, strip: StripId, tab: Tab) -> TabId {
    let id = {
        let mut tree = st.tree_mut();
        let gap = tree
            .desktop
            .workspaces
            .iter()
            .flat_map(|ws| &ws.strips)
            .find(|candidate| candidate.id == strip)
            .map_or(0, |candidate| candidate.window_groups.len());
        let previous = tree.desktop.active;
        tree.desktop.active = workspace;
        let id =
            crate::shell::tab_binding::place_tab(&mut tree, tab, Placement::GroupAt(strip, gap));
        tree.desktop.active = previous;
        // The card is the destination room's to see, and closable like any
        // tab, so it enters the room document as well as the cache.
        let groups = tree
            .desktop
            .workspaces
            .iter()
            .flat_map(|ws| &ws.strips)
            .find(|candidate| candidate.id == strip)
            .map(|candidate| candidate.window_groups.clone())
            .unwrap_or_default();
        tilo_desktop_model::desktop_doc::rewrite_strip_groups(&mut tree.doc, strip, &groups);
        tree.desktop.external_revision = tree.desktop.external_revision.wrapping_add(1);
        tree.queue_save();
        id
    };
    st.bump();
    id
}

/// Replace the tile of `tab`, wherever it is.
fn set_tile(st: &ChromeState, tab: TabId, tile: impl FnOnce(&mut TileContent)) {
    let mut tree = st.tree_mut();
    let found = tree
        .desktop
        .workspaces
        .iter_mut()
        .flat_map(|ws| ws.strips.iter_mut())
        .flat_map(|strip| strip.window_groups.iter_mut())
        .flat_map(|group| group.tabs.iter_mut())
        .find(|candidate| candidate.id == tab);
    if let Some(found) = found {
        tile(&mut found.tile);
        tree.desktop.external_revision = tree.desktop.external_revision.wrapping_add(1);
    }
    drop(tree);
}

fn set_note(st: &ChromeState, tab: TabId, text: String) {
    set_tile(st, tab, |tile| {
        if let TileContent::LocalApp { note, .. } = tile {
            *note = text;
        }
    });
}

fn failed(st: &mut ChromeState, plan: &Plan, reason: &str) {
    st.push_error_notification(
        t_args(
            "window-transfer-failed-title",
            &[("window", plan.title.as_str().into())],
        ),
        t_args(
            "window-transfer-failed-detail",
            &[
                ("reason", reason.into()),
                ("from", plan.source.name.as_str().into()),
            ],
        ),
    );
}

fn transfer_reason(error: &TransferError) -> String {
    match error {
        TransferError::Busy => t("window-transfer-failed-busy"),
        TransferError::NotTransferable(_) | TransferError::Open(_) => {
            t("window-transfer-failed-record")
        }
        TransferError::Store(_) | TransferError::Automerge(_) => {
            t("window-transfer-failed-storage")
        }
    }
}

/// Read the source record as it was last made durable, rehome it into the
/// destination under a fresh id, and give `instance` there the presentation
/// it had. Returns the arrived record and the source heads it was read at.
fn import(
    plan: &Plan,
    origin: &Origin,
    snapshot: &AutoCommit,
    view: &SavedInstanceView,
    instance: u64,
    destination_root: &Path,
) -> Result<RecordCheckout, String> {
    let storage = |_| t("window-transfer-failed-storage");
    std::fs::create_dir_all(destination_root).map_err(|_| t("window-transfer-failed-storage"))?;
    let destination =
        NativeProfile::open(destination_root, Some(&plan.destination.room.to_string()))
            .map_err(storage)?;
    let mut rehomed = transfer::rehome(
        snapshot,
        origin,
        &plan.source.room.to_string(),
        destination.workspace_id(),
        &RecordId::minted(),
    )
    .map_err(|error| transfer_reason(&error))?;
    let target = transfer::install(
        destination.store(),
        &plan.app_id,
        destination.workspace_id(),
        &mut rehomed,
    )
    .map_err(|error| transfer_reason(&error))?;
    destination
        .adopt_view(view, instance, &target)
        .map_err(storage)?;
    Ok(target)
}

/// The continuity instance a plan carries, or 0 when it carries nothing.
fn instance(plan: &Plan) -> u64 {
    match &plan.carry {
        Carry::Record { instance, .. } => *instance,
        Carry::Nothing => 0,
    }
}

/// Ask a running old instance to make what it shows durable before the
/// record is read; without one, read what it last saved.
fn checkpoint(
    st: &mut ChromeState,
    transfers: &mut WindowTransfers,
    plan: &Plan,
    mode: TransferMode,
    source_lost: bool,
) -> Option<Phase> {
    if matches!(plan.carry, Carry::Record { .. })
        && plan.surface.is_some()
        && !source_lost
        && transfers.handoff.is_some()
    {
        let token = transfers.next_token();
        transfers.ask(
            plan.source.room,
            &plan.app_id,
            instance(plan),
            HandoffRequest::Checkpoint { token },
        );
        return Some(Phase::Checkpointing {
            mode,
            token,
            since: Instant::now(),
        });
    }
    begin(st, transfers, plan, mode)
}

/// Bring the record over and start the destination instance.
fn begin(
    st: &mut ChromeState,
    transfers: &mut WindowTransfers,
    plan: &Plan,
    mode: TransferMode,
) -> Option<Phase> {
    let Carry::Record {
        instance,
        origin,
        view,
        resume,
        move_blocked,
    } = &plan.carry
    else {
        return None;
    };
    if mode == TransferMode::Move
        && let Some(reason) = move_blocked
    {
        failed(st, plan, reason);
        return None;
    }
    let Some(data_home) = transfers.data_home() else {
        failed(st, plan, &t("window-transfer-failed-storage"));
        return None;
    };
    let source_root = profile_root(&data_home, plan.source.room, &plan.app_id);
    let destination_root = profile_root(&data_home, plan.destination.room, &plan.app_id);
    // A moved window is the same instance continuing elsewhere; a copy is a
    // new window beside the one that stays, so it is an instance of its own.
    let instance = match mode {
        TransferMode::Move => *instance,
        TransferMode::Copy => crate::shell::state::next_id(),
    };
    let snapshot = match hosted::read_source(transfers, plan, origin, &source_root) {
        Ok(snapshot) => snapshot,
        Err(reason) => {
            failed(st, plan, &reason);
            return None;
        }
    };
    let known = snapshot.clone().get_heads();
    if plan.destination.hosted {
        return hosted::propose(
            st,
            transfers,
            plan,
            mode,
            hosted::Proposal {
                origin,
                snapshot: &snapshot,
                known,
                view,
                resume: resume.clone(),
                instance,
                source_root,
                destination_root,
            },
        );
    }
    let target = match import(plan, origin, &snapshot, view, instance, &destination_root) {
        Ok(target) => target,
        Err(reason) => {
            failed(st, plan, &reason);
            return None;
        }
    };
    let carried = Box::new(Carried {
        origin: origin.clone(),
        target,
        known,
        source_root,
        destination_root,
        instance,
        retired_at: None,
    });
    let Some(strip) = destination_strip(st, plan.destination.workspace) else {
        roll_back(
            st,
            plan,
            None,
            &carried,
            &t("window-move-unknown-workspace"),
        );
        return None;
    };
    let tab = place_in(
        st,
        plan.destination.workspace,
        strip,
        Tab {
            id: crate::shell::state::next_id(),
            title: plan.title.clone(),
            tile: TileContent::LocalApp {
                app_id: plan.app_id.clone(),
                label: plan.title.clone(),
                note: crate::shell::peer_windows::OPENING.to_string(),
            },
        },
    );
    start(st, transfers, plan, mode, tab, carried, resume.clone())
}

/// Start the destination instance in `tab`, or roll the transfer back.
fn start(
    st: &mut ChromeState,
    transfers: &mut WindowTransfers,
    plan: &Plan,
    mode: TransferMode,
    tab: TabId,
    carried: Box<Carried>,
    resume: Option<(u16, Vec<u8>)>,
) -> Option<Phase> {
    if let Err(reason) = launch(st, transfers, plan, tab, carried.instance, resume) {
        roll_back(st, plan, Some(tab), &carried, &reason);
        return None;
    }
    Some(Phase::Opening {
        mode,
        tab,
        carried,
        since: Instant::now(),
    })
}

/// Stage the continuation for `tab` and start the destination instance.
fn launch(
    st: &ChromeState,
    transfers: &mut WindowTransfers,
    plan: &Plan,
    tab: TabId,
    instance: u64,
    resume: Option<(u16, Vec<u8>)>,
) -> Result<(), String> {
    let room = plan.destination.room;
    crate::shell::launcher::set_pending_placement(&plan.app_id, Placement::IntoTab(tab));
    match resume {
        Some((schema, bytes)) => {
            crate::shell::launcher::stage_continuation_resume(room, tab, schema, bytes, instance);
        }
        None => crate::shell::launcher::stage_continuation_record(room, tab, instance),
    }
    let workspace = {
        let tree = st.tree();
        tree.desktop
            .workspaces
            .iter()
            .find(|ws| ws.id == plan.destination.workspace)
            .cloned()
    };
    let result = workspace
        .ok_or_else(|| t("window-move-unknown-workspace"))
        .and_then(|workspace| transfers.launcher.launch(&plan.app_id, &workspace, tab));
    if result.is_err() {
        crate::shell::launcher::clear_staged_continuation(room, tab);
        crate::shell::launcher::clear_pending_placement(&plan.app_id);
    }
    result
}

/// Undo a transfer whose destination never showed a window. The old window
/// was never touched, so it simply stays.
fn roll_back(
    st: &mut ChromeState,
    plan: &Plan,
    tab: Option<TabId>,
    carried: &Carried,
    reason: &str,
) {
    if let Some(tab) = tab {
        crate::shell::launcher::clear_staged_continuation(plan.destination.room, tab);
        let removed = {
            let mut tree = st.tree_mut();
            let removed = crate::shell::tab_binding::remove_tab(&mut tree, tab);
            if removed {
                tree.queue_save();
            }
            removed
        };
        if removed {
            st.bump();
        }
    }
    // Nothing ever opened the imported record; retire it so no later launch
    // can take it for live state. An instance that started without a window
    // may still hold it, in which case it stays unreferenced. A governed
    // destination's owner accepted it into their room; it stays there.
    if !plan.destination.hosted
        && let Some(destination) = destination_profile(plan, carried)
    {
        let _ = transfer::retire(
            destination.store(),
            &plan.app_id,
            destination.workspace_id(),
            &Origin::Record(carried.target.clone()),
            &Retirement::Abandoned,
        );
    }
    failed(st, plan, reason);
}

/// The destination window exists. A copy is done; a move asks the old
/// window to close and waits for it to let go of the record.
fn opened(
    st: &mut ChromeState,
    transfers: &mut WindowTransfers,
    plan: &Plan,
    mode: TransferMode,
    tab: TabId,
    carried: Box<Carried>,
    source_lost: bool,
) -> Option<Phase> {
    // A governed destination took the document as it was when proposed;
    // edits made in the old window while its owner decided would be lost
    // by retiring it, so the old one stays and both are kept.
    if mode == TransferMode::Move
        && plan.destination.hosted
        && hosted::changed_since_read(transfers, plan, &carried)
    {
        hosted::kept_as_copy(st, plan, &carried);
        return None;
    }
    // A running old instance stamps its own document retired, over the
    // route everyone holding it listens on, before it is asked to close.
    if mode == TransferMode::Move
        && plan.surface.is_some()
        && !source_lost
        && transfers.handoff.is_some()
    {
        let token = transfers.next_token();
        let successor = successor(plan, &carried);
        transfers.ask(
            plan.source.room,
            &plan.app_id,
            carried.instance,
            HandoffRequest::Retire { token, successor },
        );
        return Some(Phase::Retiring {
            tab,
            carried,
            token,
            since: Instant::now(),
        });
    }
    match mode {
        TransferMode::Copy => {
            receipt(
                st,
                plan.destination.workspace,
                &plan.app_id,
                &plan.title,
                &carried.target,
                t_args(
                    "window-transfer-receipt-copied",
                    &[
                        ("window", plan.title.as_str().into()),
                        ("from", plan.source.name.as_str().into()),
                    ],
                ),
            );
            None
        }
        TransferMode::Move => close_source(st, transfers, plan, tab, carried, source_lost),
    }
}

/// Where the moved document went and who moved it, as its retirement says.
fn successor(plan: &Plan, carried: &Carried) -> Successor {
    Successor {
        room: plan.destination.room.to_string(),
        record: carried.target.id.as_str().to_owned(),
        by: crate::shell::state::local_user_id_for_room(&plan.source.room),
    }
}

/// Ask the old window to close, now that the new one exists and the old
/// document says where it went, and wait for it to let go of the record.
fn close_source(
    st: &mut ChromeState,
    transfers: &WindowTransfers,
    plan: &Plan,
    tab: TabId,
    carried: Box<Carried>,
    source_lost: bool,
) -> Option<Phase> {
    // The old tab's continuity binding names the same instance the new tab
    // now runs as; closing it must not take the new instance's reports with it.
    if let Ok(mut registry) = crate::shell::personal_continuity_registry::global_registry().lock() {
        registry.forget_local_binding(AppInstanceId(plan.tab));
    }
    match plan.surface {
        Some(surface) if !source_lost => st.request_surface_close(surface),
        _ => {
            let mut tree = st.tree_mut();
            if crate::shell::tab_binding::remove_tab(&mut tree, plan.tab) {
                tree.queue_save();
            }
        }
    }
    st.bump();
    release(st, transfers, plan, tab, carried, source_lost)
}

fn source_profile(plan: &Plan, carried: &Carried) -> Option<NativeProfile> {
    NativeProfile::open(&carried.source_root, Some(&plan.source.room.to_string())).ok()
}

fn destination_profile(plan: &Plan, carried: &Carried) -> Option<NativeProfile> {
    NativeProfile::open(
        &carried.destination_root,
        Some(&plan.destination.room.to_string()),
    )
    .ok()
}

/// Once the old instance has let go of the record, bring over whatever it
/// wrote after the transfer read it where that can be merged, and finish the
/// move.
fn release(
    st: &mut ChromeState,
    transfers: &WindowTransfers,
    plan: &Plan,
    tab: TabId,
    carried: Box<Carried>,
    source_lost: bool,
) -> Option<Phase> {
    let released = if plan.source.hosted {
        // Its writes go through the room's owner; it has let go once its
        // window is gone.
        Ok(source_lost || plan.surface.is_none() || tab_tile(st, plan.tab).is_none())
    } else {
        source_profile(plan, &carried)
            .ok_or_else(|| t("window-transfer-failed-storage"))
            .and_then(|source| {
                transfer::writer_released(
                    source.store(),
                    &plan.app_id,
                    source.workspace_id(),
                    &carried.origin,
                )
                .map_err(|error| transfer_reason(&error))
            })
    };
    match released {
        Ok(true) => {}
        Ok(false) => return Some(Phase::Releasing { tab, carried }),
        Err(reason) => {
            failed(st, plan, &reason);
            return None;
        }
    }
    let last = match hosted::read_last(transfers, plan, &carried) {
        Ok(last) => last,
        Err(reason) => {
            failed(st, plan, &reason);
            return None;
        }
    };
    if transfer::changes_since(&last, &carried.known) == 0 {
        finish_move(st, plan, &carried, false, source_lost);
        return None;
    }
    if plan.destination.hosted {
        // Only its owner's route writes a governed record; what the old
        // window wrote in its last moments stays in the source's history.
        finish_move(st, plan, &carried, false, source_lost);
        hosted::late_edits_stay(st, plan);
        return None;
    }
    merge_late(st, plan, tab, carried, last, source_lost)
}

/// Merge what the old instance wrote after the transfer read it into the
/// destination record, restarting the destination instance around the
/// merge when it already has the record open, then finish the move.
fn merge_late(
    st: &mut ChromeState,
    plan: &Plan,
    tab: TabId,
    carried: Box<Carried>,
    last: AutoCommit,
    source_lost: bool,
) -> Option<Phase> {
    let Some(destination) = destination_profile(plan, &carried) else {
        failed(st, plan, &t("window-transfer-failed-storage"));
        return None;
    };
    match transfer::catch_up(
        destination.store(),
        &plan.app_id,
        destination.workspace_id(),
        &carried.target,
        &last,
    ) {
        Ok(_) => {
            finish_move(st, plan, &carried, true, source_lost);
            None
        }
        Err(TransferError::Busy) => {
            // The destination instance already has the record open. Restart
            // it around the merge rather than race its writer.
            let surface = match tab_tile(st, tab) {
                Some(TileContent::Foreign { surface_id, .. }) => Some(surface_id),
                _ => None,
            };
            // A card while it restarts, so the window closing does not take
            // the tab with it.
            set_tile(st, tab, |tile| {
                *tile = TileContent::LocalApp {
                    app_id: plan.app_id.clone(),
                    label: plan.title.clone(),
                    note: t("window-transfer-bringing-edits"),
                };
            });
            if let Some(surface) = surface {
                st.request_surface_close(surface);
            }
            st.bump();
            Some(Phase::Redelivering {
                tab,
                carried,
                source: Box::new(last),
                since: Instant::now(),
            })
        }
        Err(error) => {
            // The source record is untouched and still holds those edits.
            failed(st, plan, &transfer_reason(&error));
            None
        }
    }
}

fn redeliver(
    st: &mut ChromeState,
    transfers: &mut WindowTransfers,
    plan: &Plan,
    tab: TabId,
    carried: Box<Carried>,
    source: Box<AutoCommit>,
    since: Instant,
    source_lost: bool,
) -> Option<Phase> {
    let Some(destination) = destination_profile(plan, &carried) else {
        failed(st, plan, &t("window-transfer-failed-storage"));
        return None;
    };
    match transfer::catch_up(
        destination.store(),
        &plan.app_id,
        destination.workspace_id(),
        &carried.target,
        &source,
    ) {
        Err(TransferError::Busy) if since.elapsed() < OPEN_TIMEOUT => {
            return Some(Phase::Redelivering {
                tab,
                carried,
                source,
                since,
            });
        }
        Ok(_) => {}
        Err(error) => {
            // Keep the source record: it still holds the late edits.
            set_note(st, tab, String::new());
            failed(st, plan, &transfer_reason(&error));
            return None;
        }
    }
    // The new instance's latest resume projection, reported under its tab.
    let resume = LaunchIntent::app(plan.app_id.clone())
        .ok()
        .and_then(|launch| {
            crate::shell::personal_continuity_registry::global_registry()
                .lock()
                .ok()?
                .continuation_for_tab(plan.destination.room, AppInstanceId(tab), &launch)
                .map(|found| (found.resume.schema_version(), found.resume.bytes().to_vec()))
        });
    set_note(st, tab, crate::shell::peer_windows::OPENING.to_string());
    if let Err(reason) = launch(st, transfers, plan, tab, carried.instance, resume) {
        set_note(st, tab, t("window-transfer-reopen-failed"));
        finish_move(st, plan, &carried, true, source_lost);
        failed(st, plan, &reason);
        return None;
    }
    Some(Phase::Reopening {
        tab,
        carried,
        since: Instant::now(),
    })
}

/// Retire the source record, note the move in both workspaces' activity,
/// and tell the person about anything they should know.
fn finish_move(
    st: &mut ChromeState,
    plan: &Plan,
    carried: &Carried,
    late_edits: bool,
    source_lost: bool,
) {
    if plan.source.hosted {
        // The running instance marked it moved through its own route; one
        // that went away first could not, and the original stays live.
        if carried.retired_at.is_none() {
            hosted::not_retired(st, plan);
        }
    } else if let Some(source) = source_profile(plan, carried)
        && let Err(error) = transfer::retire(
            source.store(),
            &plan.app_id,
            source.workspace_id(),
            &carried.origin,
            &Retirement::Moved(Some(successor(plan, carried))),
        )
    {
        tracing::warn!(%error, app = %plan.app_id, "moved record could not be retired in its source room");
    }
    let window = plan.title.as_str();
    receipt(
        st,
        plan.source.workspace,
        &plan.app_id,
        &plan.title,
        carried.left(),
        t_args(
            "window-transfer-receipt-moved-out",
            &[
                ("window", window.into()),
                ("to", plan.destination.name.as_str().into()),
            ],
        ),
    );
    receipt(
        st,
        plan.destination.workspace,
        &plan.app_id,
        &plan.title,
        &carried.target,
        t_args(
            "window-transfer-receipt-moved-in",
            &[
                ("window", window.into()),
                ("from", plan.source.name.as_str().into()),
            ],
        ),
    );
    if source_lost {
        st.push_warning_notification(
            t_args("window-transfer-lost-title", &[("window", window.into())]),
            t_args(
                "window-transfer-lost-detail",
                &[
                    ("window", window.into()),
                    ("to", plan.destination.name.as_str().into()),
                ],
            ),
        );
    } else if late_edits {
        st.push_notification(
            t_args(
                "window-transfer-late-edits-title",
                &[("window", window.into())],
            ),
            t_args(
                "window-transfer-late-edits-detail",
                &[("from", plan.source.name.as_str().into())],
            ),
        );
    }
}

/// Record a transfer in `workspace`'s activity, where consequential effects are kept.
fn receipt(
    st: &ChromeState,
    workspace: WorkspaceId,
    app_id: &str,
    title: &str,
    record: &RecordCheckout,
    label: String,
) {
    let mut context = ui::tilo_actions::ActionContext::default().with_app(app_id, title);
    context.action_label = label;
    context.resource = Some(ui::tilo_actions::ActionResource {
        id: record.id.as_str().to_owned(),
        kind: record.kind.as_str().to_owned(),
        label: title.to_owned(),
    });
    let actor = {
        let tree = st.tree();
        tree.desktop
            .workspaces
            .iter()
            .find(|ws| ws.id == workspace)
            .map(|ws| crate::shell::state::local_user_id_for_room(&ws.room))
            .unwrap_or_default()
    };
    let mut tree = st.tree_mut();
    let _ = tree.append_workspace_effect_receipt(
        workspace,
        ui::tilo_actions::InvocationId::new(),
        &actor,
        context,
        ui::tilo_actions::EffectReceiptStatus::Succeeded,
    );
}
