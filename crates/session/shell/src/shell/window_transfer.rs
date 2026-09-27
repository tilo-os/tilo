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
//! 1. Read the source record as its writer last made it durable, rehome it
//!    into the destination under a fresh id, and give the old instance's
//!    presentation to the new one.
//! 2. Stage the continuity resume projection and launch the new instance
//!    into a new tab in the destination. If it never produces a window,
//!    everything rolls back: the new tab goes, the imported record is retired
//!    unopened, and the old window was never touched.
//! 3. Once the new window exists, a **Move** asks the old one to close. When
//!    its process has let go of the record, whatever it wrote after step 1 is
//!    merged into the destination (a CRDT merge, so nothing is lost and the
//!    order cannot matter) and the source record is retired. If the new
//!    instance already holds the destination record, it is restarted around
//!    the merge, and the person is told their last edits came over.
//! 4. A **Copy** stops after step 2: the source keeps its window and record,
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
use ui::record::transfer::{self, Origin, Retirement, TransferError};
use ui::record::{RecordCheckout, RecordId};

use crate::shell::chrome_state::ChromeState;
use crate::shell::i18n::{t, t_args};
use crate::shell::personal_continuity::{AppInstanceId, LaunchIntent};
use crate::shell::portal_dialog::{
    PortalDialogQueue, WindowTransferChoice, WindowTransferDecision, WindowTransferRequest,
};
use crate::shell::state::WorkspaceAuthority as _;
use crate::shell::state::{
    RoomId, StripId, Tab, TabId, TileContent, Workspace, WorkspaceId, WorkspaceTree,
};
use crate::shell::tab_binding::Placement;
use crate::shell::window_actions::MoveTabError;

/// How long a new instance may take to show a window before the transfer
/// rolls back, or before a restart around late edits gives up.
const OPEN_TIMEOUT: Duration = Duration::from_secs(30);

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
        /// Why Move is not offered, when another window still shows the record.
        move_blocked: bool,
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
    next: u64,
    polled: Option<Instant>,
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

    /// Whether any transfer is in flight, including one waiting on its sheet.
    pub fn is_pending(&self) -> bool {
        !self.active.is_empty()
    }

    /// Whether a transfer is waiting on something no input announces — a
    /// window appearing, or a process letting go of its record — so the
    /// frame loop has to keep polling.
    pub fn is_busy(&self) -> bool {
        self.active
            .iter()
            .any(|transfer| !matches!(transfer.phase, Phase::Consent(_)))
    }

    /// When the frame loop should run [`tick`] again, while a transfer is busy.
    pub fn next_poll(&self) -> Option<Instant> {
        self.is_busy().then(|| {
            self.polled.map_or_else(Instant::now, |polled| {
                polled.checked_add(POLL_INTERVAL).unwrap_or(polled)
            })
        })
    }

    fn data_home(&self) -> Option<PathBuf> {
        self.data_home
            .clone()
            .or_else(::launcher::appdir::host_data_home)
    }
}

/// Where the launcher keeps `app_id`'s profile in `room`: the directory the
/// app sees as `XDG_DATA_HOME`.
fn profile_root(data_home: &Path, room: RoomId, app_id: &str) -> PathBuf {
    let room = room.to_string();
    ::launcher::appdir::host_app_dir(
        data_home,
        &::launcher::appdir::room_slug(Some(&room)),
        app_id,
    )
    .join(".local/share")
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
) -> Option<(Origin, SavedInstanceView)> {
    if !directory.is_dir() {
        return None;
    }
    let profile = NativeProfile::open(directory, Some(&room.to_string())).ok()?;
    let readable = |origin: &Origin| {
        transfer::read(profile.store(), app_id, profile.workspace_id(), origin).is_ok()
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
    if tree.requires_native_owner(source_ws.room) || tree.requires_native_owner(destination_ws.room)
    {
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
    };
    let source = side(source_ws);
    let carry = data_home
        .and_then(|home| {
            let instance = instance_of(tree, tab, source.room, &tile_app, &app_id)?;
            let root = profile_root(home, source.room, &app_id);
            let (origin, view) = shown(&root, source.room, &app_id, instance)?;
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
                        Some(other) => shown(&root, source.room, &app_id, other)
                            .is_some_and(|(other, _)| other == origin),
                        // An instance nothing is known about shares the app's
                        // own document, the one thing every window opens.
                        None => matches!(origin, Origin::AppDocument(_)),
                    }
                });
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
            let mut choices = Vec::new();
            if *move_blocked {
                detail.push(t_args(
                    "window-transfer-move-blocked",
                    &[("from", from.into())],
                ));
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
    let plan = plan(&tree, tab, target, data_home.as_deref())?;
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
    if st.window_transfers.active.is_empty() {
        return;
    }
    let mut transfers = std::mem::take(&mut st.window_transfers);
    transfers.polled = Some(Instant::now());
    let active = std::mem::take(&mut transfers.active);
    for transfer in active {
        if let Some(next) = advance(st, &mut transfers, transfer) {
            transfers.active.push(next);
        }
    }
    // Anything queued while this ran (a nested request) is kept.
    transfers.active.append(&mut st.window_transfers.active);
    st.window_transfers = transfers;
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
            Ok(WindowTransferDecision::Move) => begin(st, transfers, &plan, TransferMode::Move),
            Ok(WindowTransferDecision::Copy) => begin(st, transfers, &plan, TransferMode::Copy),
        },
        Phase::Opening {
            mode,
            tab,
            carried,
            since,
        } => match tab_tile(st, tab) {
            Some(TileContent::Foreign { .. }) => opened(st, &plan, mode, tab, carried, source_lost),
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
        Phase::Releasing { tab, carried } => release(st, &plan, tab, carried, source_lost),
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
        tree.flush_to_disk();
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
    view: &SavedInstanceView,
    instance: u64,
    source_root: &Path,
    destination_root: &Path,
) -> Result<(RecordCheckout, Vec<ChangeHash>), String> {
    let storage = |_| t("window-transfer-failed-storage");
    let source =
        NativeProfile::open(source_root, Some(&plan.source.room.to_string())).map_err(storage)?;
    std::fs::create_dir_all(destination_root).map_err(|_| t("window-transfer-failed-storage"))?;
    let destination =
        NativeProfile::open(destination_root, Some(&plan.destination.room.to_string()))
            .map_err(storage)?;
    let snapshot = transfer::read(source.store(), &plan.app_id, source.workspace_id(), origin)
        .map_err(|error| transfer_reason(&error))?;
    let known = snapshot.clone().get_heads();
    let mut rehomed = transfer::rehome(
        &snapshot,
        origin,
        source.workspace_id(),
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
    Ok((target, known))
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
    if mode == TransferMode::Move && *move_blocked {
        failed(
            st,
            plan,
            &t_args(
                "window-transfer-move-blocked",
                &[("from", plan.source.name.as_str().into())],
            ),
        );
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
    let arrived = import(
        plan,
        origin,
        view,
        instance,
        &source_root,
        &destination_root,
    );
    let (target, known) = match arrived {
        Ok(arrived) => arrived,
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
    if let Err(reason) = launch(st, transfers, plan, tab, instance, resume.clone()) {
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
                tree.flush_to_disk();
            }
            removed
        };
        if removed {
            st.bump();
        }
    }
    // Nothing ever opened the imported record; retire it so no later launch
    // can take it for live state. An instance that started without a window
    // may still hold it, in which case it stays unreferenced.
    if let Ok(destination) = NativeProfile::open(
        &carried.destination_root,
        Some(&plan.destination.room.to_string()),
    ) {
        let _ = transfer::retire(
            destination.store(),
            &plan.app_id,
            destination.workspace_id(),
            &Origin::Record(carried.target.clone()),
            Retirement::Abandoned,
        );
    }
    failed(st, plan, reason);
}

/// The destination window exists. A copy is done; a move asks the old
/// window to close and waits for it to let go of the record.
fn opened(
    st: &mut ChromeState,
    plan: &Plan,
    mode: TransferMode,
    tab: TabId,
    carried: Box<Carried>,
    source_lost: bool,
) -> Option<Phase> {
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
        TransferMode::Move => {
            // The old tab's continuity binding names the same instance the
            // new tab now runs as; closing it must not take the new
            // instance's reports with it.
            if let Ok(mut registry) =
                crate::shell::personal_continuity_registry::global_registry().lock()
            {
                registry.forget_local_binding(AppInstanceId(plan.tab));
            }
            match plan.surface {
                Some(surface) if !source_lost => st.request_surface_close(surface),
                _ => {
                    let mut tree = st.tree_mut();
                    if crate::shell::tab_binding::remove_tab(&mut tree, plan.tab) {
                        tree.flush_to_disk();
                    }
                }
            }
            st.bump();
            release(st, plan, tab, carried, source_lost)
        }
    }
}

fn profiles(plan: &Plan, carried: &Carried) -> Option<(NativeProfile, NativeProfile)> {
    let source =
        NativeProfile::open(&carried.source_root, Some(&plan.source.room.to_string())).ok()?;
    let destination = NativeProfile::open(
        &carried.destination_root,
        Some(&plan.destination.room.to_string()),
    )
    .ok()?;
    Some((source, destination))
}

/// Once the old instance has let go of the record, bring over whatever it
/// wrote after the transfer read it, and retire the source.
fn release(
    st: &mut ChromeState,
    plan: &Plan,
    tab: TabId,
    carried: Box<Carried>,
    source_lost: bool,
) -> Option<Phase> {
    let Some((source, destination)) = profiles(plan, &carried) else {
        failed(st, plan, &t("window-transfer-failed-storage"));
        return None;
    };
    match transfer::writer_released(
        source.store(),
        &plan.app_id,
        source.workspace_id(),
        &carried.origin,
    ) {
        Ok(false) => return Some(Phase::Releasing { tab, carried }),
        Ok(true) => {}
        Err(error) => {
            failed(st, plan, &transfer_reason(&error));
            return None;
        }
    }
    let last = match transfer::read(
        source.store(),
        &plan.app_id,
        source.workspace_id(),
        &carried.origin,
    ) {
        Ok(last) => last,
        Err(error) => {
            failed(st, plan, &transfer_reason(&error));
            return None;
        }
    };
    if transfer::changes_since(&last, &carried.known) == 0 {
        finish_move(st, plan, &carried, false, source_lost);
        return None;
    }
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

#[expect(
    clippy::too_many_arguments,
    reason = "one phase of the transfer state machine, passed through unchanged"
)]
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
    let Some((_, destination)) = profiles(plan, &carried) else {
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
    if let Some((source, _)) = profiles(plan, carried)
        && let Err(error) = transfer::retire(
            source.store(),
            &plan.app_id,
            source.workspace_id(),
            &carried.origin,
            Retirement::Moved,
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
