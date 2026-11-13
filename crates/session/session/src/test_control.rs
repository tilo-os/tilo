//! The control sockets: `tilo-control` drives a session it was started for, and
//! the device admin drives their own desktop, over one protocol — one JSON request
//! per line, tagged by `op`, and one answer per line: `{"ok": ..}`,
//! `{"answer": ..}`, `{"state": ..}`, `{"targets": ..}` or
//! `{"err": {"code": .., "message": ..}}`.
//! The development test-control socket (`--test-control PATH` or
//! `$TILO_TEST_CONTROL`, development builds only) answers every op below,
//! including injected input, screenshots and fixtures. The owner's own door,
//! `admin-control.sock`, admits only programs of the account whose desktop this
//! is, in its own namespaces, and serves only the action model, the state it shows
//! and the session's diagnostics ([`Op::owner_serves`]).
//! Each connection is read on a thread of its own; what a request asks of the
//! compositor, the desk or the hosted surfaces is answered on the loop's pass, and
//! the loop is woken for it.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value as Json, json};
use tilo_actions::remote::{
    ActionHost as _, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvokeArgument, InvokeRequest, MAX_CATALOGUE_PAGE, ReceiptStatus, RemoteActionTarget,
    settle_with,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};
use tilo_compositor::{Compositor, InputEvent};

use tilo_desktop::overlays::{Overlays, Slot};
use tilo_desktop_surface::DesktopHandle;
use tilo_lock::MutexRecover as _;
use tilo_service::Wake;
use tilo_session_core::door::Doors;

use crate::agent_door::{self, PeerRefusal};
use crate::engine::Engine;

mod gesture_input;
mod state;

use gesture_input::asked_of_gestures;
use state::{presence_stamp, shown, state_dump, window_rows, within};

/// The variable `tilo-control` reads the test-control socket's path from: what a development
/// session binds the socket at when its command line names none and the default is not wanted,
/// and what it prints with the path it opened.
pub const ENV: &str = "TILO_TEST_CONTROL";

/// The variable a session prints the owner's control socket's path with, as it prints [`ENV`]'s:
/// the socket [`admin_path`] opened.
pub const ADMIN_ENV: &str = "TILO_ADMIN_CONTROL";

/// Whether this build opens the test-control socket it is given: development builds, and the dev
/// image's, whose binaries are release ones built with `dev-overrides` . A production build leaves
/// the flag without effect, so nothing it ships drives a session as the person through a socket.
pub const OPENED_BY_THIS_BUILD: bool = cfg!(any(debug_assertions, feature = "dev-overrides"));

/// The owner's control socket's file name in a headless session's directory: a headless session
/// serves no seat, so it never takes the seat's own `admin-control.sock` , which a login's session
/// binds.
pub const HEADLESS_OWNER_CONTROL_SOCKET_FILE: &str = "headless-owner-control.sock";

/// Where the development socket is opened, given whether this build opens one (`opens`,
/// [`OPENED_BY_THIS_BUILD`]) and the path the command line named: `None` in a build that opens
/// none, whatever was named; the named path, else `$TILO_TEST_CONTROL`'s, else the default
/// `$XDG_RUNTIME_DIR/tilo-session/test-control.sock` the tools that drive a desktop go to —
/// what a dev image's login binds with nothing named, so `tilo-vmd` finds it where it looks.
#[must_use]
pub fn path(opens: bool, named: Option<&str>) -> Option<PathBuf> {
    path_in(
        opens,
        named,
        std::env::var_os(ENV),
        std::env::var_os("XDG_RUNTIME_DIR"),
    )
}

/// [`path`] for the values it reads, so what it decides can be checked without the environment.
fn path_in(
    opens: bool,
    named: Option<&str>,
    env: Option<OsString>,
    runtime_dir: Option<OsString>,
) -> Option<PathBuf> {
    if !opens {
        return None;
    }
    let named = named
        .map(PathBuf::from)
        .or_else(|| env.map(PathBuf::from))
        .filter(|path| !path.as_os_str().is_empty());
    named.or_else(|| {
        tilo_paths::login::session_dir(runtime_dir)
            .map(|directory| directory.join(tilo_paths::login::TEST_CONTROL_SOCKET_FILE))
    })
}

/// Where the owner's control socket opens: the path named on the command line, else the seat's
/// `admin-control.sock` in `$XDG_RUNTIME_DIR/tilo-session` for a session that owns the machine,
/// else `headless-owner-control.sock` there for a headless one, whose machine this is but whose
/// seat it is not. `None` for a session that owns no machine and named no path.
///
/// A path the door is served at is not yet the account's right to be served: the seat's own
/// socket is the device's, and [`owner_decision`] decides whether this session is the account
/// that owns the device.
#[must_use]
pub fn admin_path(owned: bool, headless: bool, named: Option<PathBuf>) -> Option<PathBuf> {
    admin_path_in(owned, headless, named, std::env::var_os("XDG_RUNTIME_DIR"))
}

/// [`admin_path`] for the values it reads, so what it decides can be checked without the
/// environment.
fn admin_path_in(
    owned: bool,
    headless: bool,
    named: Option<PathBuf>,
    runtime_dir: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(named) = named.filter(|path| !path.as_os_str().is_empty()) {
        return Some(named);
    }
    if !owned {
        return None;
    }
    let file = if headless {
        HEADLESS_OWNER_CONTROL_SOCKET_FILE
    } else {
        tilo_paths::login::ADMIN_CONTROL_SOCKET_FILE
    };
    tilo_paths::login::session_dir(runtime_dir).map(|directory| directory.join(file))
}

/// Why a session serves no owner's door.
#[derive(Debug, PartialEq, Eq)]
pub enum NotServed {
    /// The account does not administer the machine: it is not a member of `wheel` .
    NotAdministrator,
    /// The account's identity does not own the device, or it has none yet.
    NotDeviceOwner,
    /// The device's ownership could not be read; reported, never guessed.
    OwnershipUnreadable(String),
}

impl std::fmt::Display for NotServed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAdministrator => f.write_str(
                "this account is not in the `wheel` group, which administers this machine",
            ),
            Self::NotDeviceOwner => f.write_str("this account's identity does not own this device"),
            Self::OwnershipUnreadable(error) => {
                write!(f, "the device's ownership could not be read: {error}")
            }
        }
    }
}

/// Whether this session serves the owner's door, from the two facts that make its account the
/// device's own: a member of the machine's administrator group (`wheel`, which the image's
/// `sudo` and polkit rules both name), whose identity is the one that claimed the device at
/// its first login. A session in another desktop's window, whose machine this is not, serves
/// none.
///
/// # Errors
///
/// [`NotServed`], saying which of the two the account is not.
pub fn owner_decision(
    in_admin_group: bool,
    owns_device: io::Result<bool>,
) -> Result<(), NotServed> {
    if !in_admin_group {
        return Err(NotServed::NotAdministrator);
    }
    match owns_device {
        Ok(true) => Ok(()),
        Ok(false) => Err(NotServed::NotDeviceOwner),
        Err(error) => Err(NotServed::OwnershipUnreadable(error.to_string())),
    }
}

/// What the session is, for the answers that read it: the doors every hosted surface's actions go
/// through, the desk's tree, what stands over it, the workspaces this computer holds, whether the
/// person locked the screen, whether the lock screen's own frame is on it, and the agents seated at
/// the person's door.
#[derive(Clone, Copy)]
pub(crate) struct Desk<'a> {
    pub(crate) doors: &'a Doors,
    pub(crate) handle: &'a DesktopHandle,
    pub(crate) overlays: &'a Overlays,
    /// Whether the overview the person sees is up: its pictures' own flag, which the chrome writes
    /// as it draws and the shell writes as its views show it, so the state says the one overview
    /// either way.
    pub(crate) overview: &'a std::cell::Cell<bool>,
    pub(crate) engine: Option<&'a Engine>,
    /// The agents the person admitted at its door, as the desk's bar shows them.
    pub(crate) seats: &'a std::sync::Arc<agent_door::Seats>,
    /// Who is here, as the collaboration plane says it: this session's membership of each
    /// workspace, when it takes part in one.
    pub(crate) sync: Option<&'a crate::sync::Client>,
    pub(crate) locked: bool,
    pub(crate) lock_screen_drawn: bool,
    /// The lock's own state: what unlocking it is, the same one an accepted password sets, so a
    /// tool that unlocks asks the session to come down the one way it does — the screen dissolves
    /// and the compositor lifts the lock once it has.
    pub(crate) lock: &'a crate::lock_service::Lock,
    /// Where this device's pointer is, and whether it shows.
    pub(crate) pointer: (f64, f64),
    pub(crate) pointer_shown: bool,
    /// The clipboard's history and the selection it reads.
    pub(crate) clipboard: &'a crate::clipboard::Clipboard,
    /// This device's share of the person's one keyboard and pointer.
    pub(crate) shared_input: Option<&'a crate::shared_input::SharedInput>,
}

/// One request a connection sent the loop, and where its answer goes.
struct Asked {
    /// The op it asks, which the socket it came in on serves.
    op: Op,
    request: Json,
    /// What it acts as: the socket it came in on, and the app and workspace it was admitted for.
    ctx: Context,
    answer: Sender<Json>,
}

/// The sockets the session serves, and what they answer from.
pub(crate) struct Control {
    /// Accepting connections for as long as the session serves them: dropped, they stop.
    _sockets: Vec<Socket>,
    /// What every connection asks the actions and the change signal through.
    door: Arc<Door>,
    asked: Receiver<Asked>,
    /// The keys a `key_down` holds, so `key_up` releases them and `modifiers` knows what to press
    /// and release to reach the mask it names.
    held: RefCell<BTreeSet<u32>>,
    /// The fingers a `swipe` with `release: false` left on the touchpad, so the next `swipe`
    /// carries the same gesture on and its release lifts them.
    fingers_down: std::cell::Cell<Option<u32>>,
    /// The desk state the loop last published, as its stamp.
    published: std::cell::Cell<Option<u64>>,
    changes: Arc<Changes>,
    /// What asked anything but the desk's input since the last pass: it is answered after the
    /// frame, where it sees the desk the frame shows.
    waiting: RefCell<Vec<Asked>>,
}

/// One socket: where it listens, whose it is, and what its connections answer from.
struct Socket {
    path: PathBuf,
    /// Tells the thread accepting its connections to stop at the next one.
    stopped: Arc<AtomicBool>,
}

/// Who a socket serves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Gate {
    /// The developer's own socket: whoever the developer started it for.
    Dev,
    /// The owner's door: this account's own programs, and only theirs.
    Owner,
    /// A program inside an app this session started: it acts in that app's workspace, with the
    /// app's own permissions, never the owner's.
    App,
}

/// The workspace a connection of the app door acts in, and the app it acts as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Scope {
    app: String,
    workspace: String,
}

/// What a connection is: the socket it came in on, and, at the app door, the app and workspace it
/// was admitted for.
#[derive(Clone)]
struct Context {
    gate: Gate,
    scope: Option<Arc<Scope>>,
}

impl Context {
    /// A connection of the socket `gate` serves: no app narrower than it.
    const fn at(gate: Gate) -> Self {
        Self { gate, scope: None }
    }
}

/// One op of the control protocol, as a request's `op` names it.
///
/// [`Op::ALL`] is every op the protocol answers. The development socket answers all of them;
/// the owner's door serves the semantic half only ([`Op::owner_serves`], [`served`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    ActionsCancel,
    ActionsEnumerate,
    ActionsInvoke,
    ActionsStatus,
    ClickAt,
    ClickNamed,
    Drag,
    Key,
    KeyDown,
    KeyUp,
    ListTargets,
    Modifiers,
    Pinch,
    PointerButton,
    PointerMove,
    ProfileStart,
    ProfileStatus,
    ProfileStop,
    ProvisionTestIdentity,
    ProcessMemory,
    RecordRead,
    Scroll,
    SearchRows,
    Snapshot,
    StateDump,
    Swipe,
    Type,
    Unlock,
    Watch,
    Windows,
    WorkspaceHosts,
}

impl Op {
    /// Every op the protocol answers, in the order the refusal names them.
    const ALL: [Self; 31] = [
        Self::ActionsCancel,
        Self::ActionsEnumerate,
        Self::ActionsInvoke,
        Self::ActionsStatus,
        Self::ClickAt,
        Self::ClickNamed,
        Self::Drag,
        Self::Key,
        Self::KeyDown,
        Self::KeyUp,
        Self::ListTargets,
        Self::Modifiers,
        Self::Pinch,
        Self::PointerButton,
        Self::PointerMove,
        Self::ProcessMemory,
        Self::ProfileStart,
        Self::ProfileStatus,
        Self::ProfileStop,
        Self::ProvisionTestIdentity,
        Self::RecordRead,
        Self::Scroll,
        Self::SearchRows,
        Self::Snapshot,
        Self::StateDump,
        Self::Swipe,
        Self::Type,
        Self::Unlock,
        Self::Watch,
        Self::Windows,
        Self::WorkspaceHosts,
    ];

    /// The op `request` names, or `None` for one the protocol does not answer at all.
    fn of(request: &Json) -> Option<Self> {
        Some(match request.get("op").and_then(Json::as_str)? {
            "actions_cancel" => Self::ActionsCancel,
            "actions_enumerate" => Self::ActionsEnumerate,
            "actions_invoke" => Self::ActionsInvoke,
            "actions_status" => Self::ActionsStatus,
            "click_at" => Self::ClickAt,
            "click_named" => Self::ClickNamed,
            "drag" => Self::Drag,
            "key" => Self::Key,
            "key_down" => Self::KeyDown,
            "key_up" => Self::KeyUp,
            "list_targets" => Self::ListTargets,
            "modifiers" => Self::Modifiers,
            "pinch" => Self::Pinch,
            "pointer_button" => Self::PointerButton,
            "pointer_move" => Self::PointerMove,
            "process_memory" => Self::ProcessMemory,
            "profile_start" => Self::ProfileStart,
            "profile_status" => Self::ProfileStatus,
            "profile_stop" => Self::ProfileStop,
            "provision_test_identity" => Self::ProvisionTestIdentity,
            "record_read" => Self::RecordRead,
            "scroll" => Self::Scroll,
            "search_rows" => Self::SearchRows,
            "snapshot" => Self::Snapshot,
            "state_dump" => Self::StateDump,
            "swipe" => Self::Swipe,
            "type" => Self::Type,
            "unlock" => Self::Unlock,
            "watch" => Self::Watch,
            "windows" => Self::Windows,
            "workspace_hosts" => Self::WorkspaceHosts,
            _ => return None,
        })
    }

    /// The name the protocol writes it as.
    const fn name(self) -> &'static str {
        match self {
            Self::ActionsCancel => "actions_cancel",
            Self::ActionsEnumerate => "actions_enumerate",
            Self::ActionsInvoke => "actions_invoke",
            Self::ActionsStatus => "actions_status",
            Self::ClickAt => "click_at",
            Self::ClickNamed => "click_named",
            Self::Drag => "drag",
            Self::Key => "key",
            Self::KeyDown => "key_down",
            Self::KeyUp => "key_up",
            Self::ListTargets => "list_targets",
            Self::Modifiers => "modifiers",
            Self::Pinch => "pinch",
            Self::PointerButton => "pointer_button",
            Self::PointerMove => "pointer_move",
            Self::ProcessMemory => "process_memory",
            Self::ProfileStart => "profile_start",
            Self::ProfileStatus => "profile_status",
            Self::ProfileStop => "profile_stop",
            Self::ProvisionTestIdentity => "provision_test_identity",
            Self::RecordRead => "record_read",
            Self::Scroll => "scroll",
            Self::SearchRows => "search_rows",
            Self::Snapshot => "snapshot",
            Self::StateDump => "state_dump",
            Self::Swipe => "swipe",
            Self::Type => "type",
            Self::Unlock => "unlock",
            Self::Watch => "watch",
            Self::Windows => "windows",
            Self::WorkspaceHosts => "workspace_hosts",
        }
    }

    /// Whether the owner's door serves it: the action model and the state it shows, which is what a
    /// person's own scripts drive their desktop through. Synthetic input, screenshots of the
    /// person's screen, the hosted surfaces' controls, the fixtures and the test identity are
    /// nobody's but a development session's.
    const fn owner_serves(self) -> bool {
        matches!(
            self,
            Self::ActionsCancel
                | Self::ActionsEnumerate
                | Self::ActionsInvoke
                | Self::ActionsStatus
                | Self::ProcessMemory
                | Self::ProfileStart
                | Self::ProfileStatus
                | Self::ProfileStop
                | Self::RecordRead
                | Self::StateDump
                | Self::Watch
                | Self::Windows
                | Self::WorkspaceHosts
        )
    }
}

impl Op {
    /// Whether what `request` asks of this op starts work the broker answers, which needs no desk
    /// the frame shows: an action a tool or an agent invokes starts now, in this pass, and the
    /// answer it waits on comes back beside the frame instead of after it. An invocation the
    /// session's own actions answer is the loop's to run later in the pass, so it waits for the
    /// frame with everything else.
    fn starts_an_action(self, request: &Json) -> bool {
        self == Self::ActionsInvoke
            && request
                .get("provider")
                .and_then(Json::as_str)
                .is_some_and(|provider| provider != tilo_actions::remote::SESSION_ACTIONS_PROVIDER)
    }

    /// Whether it hands the desk input, which the loop serves where a device's own input reaches
    /// it: in the pass it arrives in, before that pass draws its frame.
    const fn hands_input(self) -> bool {
        matches!(
            self,
            Self::ClickAt
                | Self::Drag
                | Self::Key
                | Self::KeyDown
                | Self::KeyUp
                | Self::Modifiers
                | Self::Pinch
                | Self::PointerButton
                | Self::PointerMove
                | Self::Scroll
                | Self::Swipe
                | Self::Type
        )
    }
}

/// Whether the socket at `gate` answers `op` : the one place the protocol's ops are divided between
/// the doors.
fn served(gate: Gate, op: Op) -> bool {
    match gate {
        Gate::Dev => true,
        Gate::Owner | Gate::App => op.owner_serves(),
    }
}

/// The answer a door gives what it does not serve.
fn not_this_doors(gate: Gate, op: Op) -> Json {
    let _ = gate;
    refusal(
        "not_permitted",
        &format!(
            "this control door serves the action model, the state it shows and the \
             session's own diagnostics (`actions_cancel`, `actions_enumerate`, \
             `actions_invoke`, `actions_status`, `process_memory`, `profile_start`, \
             `profile_status`, `profile_stop`, `state_dump`, `watch` and `windows`), not \
             `{}`; synthetic input and screenshots exist only on a development session's \
             test-control socket",
            op.name()
        ),
    )
}

/// The answer for an `op` the protocol does not answer at all, naming the ones the socket `gate`
/// serves.
fn unsupported(ctx: &Context, request: &Json) -> Json {
    let answers = Op::ALL
        .iter()
        .filter(|op| served(ctx.gate, **op))
        .map(|op| format!("`{}`", op.name()))
        .collect::<Vec<_>>()
        .join(", ");
    refusal(
        "unsupported",
        &format!(
            "this socket answers {answers}, not `{}`",
            request.get("op").and_then(Json::as_str).unwrap_or_default()
        ),
    )
}

/// What a connection asks on: the loop's queue and its wake, and the actions and change signal it
/// may read without the loop.
pub(crate) struct Door {
    asks: Mutex<Sender<Asked>>,
    wake: Wake,
    actions: Arc<Actions>,
    changes: Arc<Changes>,
}

impl Control {
    /// The sockets `options` name, opened. The actions and the change signal exist whether or not
    /// one does: the app door answers the programs it admits through them too.
    pub(crate) fn open(options: &crate::Options, host: Arc<agent_door::Host>) -> Self {
        let (asks, asked) = channel();
        let changes = Arc::new(Changes::new(options.wake.handle()));
        let actions = Arc::new(Actions {
            host,
            caller: Caller::local_person(options.person.clone()).in_new_execution(),
            front: Front::default(),
            ledger: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
            locked: AtomicBool::new(false),
        });
        let door = Arc::new(Door {
            asks: Mutex::new(asks),
            wake: options.wake.handle(),
            actions,
            changes: Arc::clone(&changes),
        });
        let mut sockets = Vec::new();
        for (path, gate) in [
            (options.test_control.as_deref(), Gate::Dev),
            (options.admin_control.as_deref(), Gate::Owner),
        ] {
            let Some(path) = path else {
                continue;
            };
            match Socket::open(path, gate, Arc::clone(&door)) {
                Ok(socket) => {
                    tracing::info!(path = %path.display(), "control: serving a socket");
                    sockets.push(socket);
                }
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "a control socket does not open");
                }
            }
        }
        Self {
            _sockets: sockets,
            door,
            asked,
            held: RefCell::new(BTreeSet::new()),
            fingers_down: std::cell::Cell::new(None),
            published: std::cell::Cell::new(None),
            changes,
            waiting: RefCell::new(Vec::new()),
        }
    }

    /// The door a program inside an app this session started is served at.
    pub(crate) fn door(&self) -> Arc<Door> {
        Arc::clone(&self.door)
    }

    /// What a tool handed the desk's input since the last pass, answered where a device's input
    /// reaches the desk: a key or a press a tool injects takes effect in the frame this pass draws,
    /// as the same input from a device does. Everything else asked waits for the frame
    /// ([`Self::serve`]), where it reads the desk the frame shows.
    pub(crate) fn serve_input(&self, compositor: &mut Compositor, desk: &Desk<'_>) {
        self.door.actions.set_workspace(desk.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        }));
        self.door.actions.set_locked(desk.locked);
        self.take();
        let mut waiting = self.waiting.borrow_mut();
        let mut later = Vec::new();
        for Asked {
            op,
            request,
            ctx,
            answer,
        } in waiting.drain(..)
        {
            if op.hands_input() || op.starts_an_action(&request) {
                let _ = answer.send(loop_answer(
                    op,
                    &request,
                    &ctx,
                    compositor,
                    desk,
                    &self.held,
                    &self.fingers_down,
                ));
            } else {
                later.push(Asked {
                    op,
                    request,
                    ctx,
                    answer,
                });
            }
        }
        *waiting = later;
    }

    /// Take the connections waiting, publish the desk its watchers read, and answer what was asked
    /// of the loop since the last pass.
    pub(crate) fn serve(&self, compositor: &mut Compositor, desk: &Desk<'_>) {
        self.door.actions.set_workspace(desk.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        }));
        self.door.actions.set_locked(desk.locked);
        self.take();
        let ready: Vec<Asked> = self.waiting.borrow_mut().drain(..).collect();
        for Asked {
            op,
            request,
            ctx,
            answer,
        } in ready
        {
            let _ = answer.send(loop_answer(
                op,
                &request,
                &ctx,
                compositor,
                desk,
                &self.held,
                &self.fingers_down,
            ));
        }
    }

    /// Everything the connections sent the loop since the last look, oldest first. What asks for
    /// the desk's input, or starts an action the broker answers, is served in the pass it arrives
    /// in; the rest waits for the frame.
    fn take(&self) {
        while let Ok(asked) = self.asked.try_recv() {
            self.waiting.borrow_mut().push(asked);
        }
    }

    /// Tell the watchers where the desk stands, when it moved since the last pass: the state dump
    /// is only built when someone reads it.
    pub(crate) fn publish(&self, desk: &Desk<'_>) {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::hash::DefaultHasher::new();
        desk.handle.navigation_revision().hash(&mut hasher);
        desk.locked.hash(&mut hasher);
        for slot in Slot::ALL {
            desk.overlays.is_open(slot).hash(&mut hasher);
        }
        desk.overview.get().hash(&mut hasher);
        presence_stamp(desk, &mut hasher);
        let stamp = hasher.finish();
        let moved = self.published.get() != Some(stamp);
        if moved {
            self.published.set(Some(stamp));
        }
        self.changes
            .follow(moved, || desk.handle.read(|tree| state_dump(tree, desk)));
    }
}

impl Socket {
    /// Listen at `path` for this account alone; `gate` is who is served there. Connections are
    /// accepted on a thread of the socket's own as they arrive, each read on a thread of its own:
    /// what a program asks of the actions is answered there, never after a pass of the loop, and
    /// what it asks of the loop wakes the loop itself.
    fn open(path: &Path, gate: Gate, door: Arc<Door>) -> io::Result<Self> {
        let listener = agent_door::bind_private(path)?;
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let at = path.to_path_buf();
        std::thread::Builder::new()
            .name("tilo-control-accept".into())
            .spawn(move || accept(&listener, &at, gate, &door, &stop))?;
        Ok(Self {
            path: path.to_path_buf(),
            stopped,
        })
    }
}

/// Accept the connections of `listener` until `stopped`, each read on a thread of its own.
fn accept(
    listener: &UnixListener,
    path: &Path,
    gate: Gate,
    door: &Arc<Door>,
    stopped: &AtomicBool,
) {
    for stream in listener.incoming() {
        if stopped.load(Ordering::Acquire) {
            return;
        }
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                tracing::warn!(%error, "control: accepting a connection failed");
                continue;
            }
        };
        if gate == Gate::Owner
            && let Err(refusal) =
                agent_door::identify(&stream).and_then(|(_, peer)| agent_door::peer_decision(&peer))
        {
            tracing::info!(?refusal, path = %path.display(), "control: a connection is refused");
            refuse_owner(stream, &refusal);
            continue;
        }
        let door = Arc::clone(door);
        let ctx = Context::at(gate);
        let spawned = std::thread::Builder::new()
            .name("tilo-control".into())
            .spawn(move || serve_connection(stream, &ctx, &door, None));
        if let Err(error) = spawned {
            tracing::warn!(%error, "control: a connection could not be served");
        }
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        // The accepting thread ends at the next connection, which this one is.
        self.stopped.store(true, Ordering::Release);
        if let Err(error) = tilo_files::native::socket::connect(&self.path) {
            tracing::warn!(
                %error,
                path = %self.path.display(),
                "control: the thread accepting connections was not woken to stop"
            );
        }
        // Nothing answers there once the session is gone.
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        // A watcher waiting on a state that will never change answers.
        self.changes.end();
    }
}

/// Tell a connection of the owner's door it is not this desktop's owner, in the control protocol's
/// own framing, then let it go.
fn refuse_owner(mut stream: UnixStream, refusal: &PeerRefusal) {
    let message = match refusal {
        PeerRefusal::Unidentified => "this program couldn't be identified".to_owned(),
        PeerRefusal::OtherAccount => {
            "this works only for the account whose desktop this is; sign in as that account"
                .to_owned()
        }
        PeerRefusal::Sandboxed => {
            "programs inside a sandboxed app can't use tilo-control; the app acts with its own \
             permissions"
                .to_owned()
        }
        PeerRefusal::Unavailable | PeerRefusal::Busy | PeerRefusal::OutsideEnvironment => {
            refusal.message().to_owned()
        }
    };
    let reply = refusal_json("not_permitted", &message);
    if let Err(error) = stream.set_write_timeout(Some(Duration::from_secs(1))) {
        tracing::warn!(%error, "a control refusal has no write timeout");
    }
    if let Err(error) = stream.write_all(format!("{reply}\n").as_bytes()) {
        tracing::warn!(%error, "a control refusal did not reach its program");
    }
}

/// Serve one connection of a program inside an app this session started: the control protocol,
/// as that app, in the workspace the session started it into. `first` is the line the agent
/// door already read.
pub(crate) fn serve_app(
    stream: UnixStream,
    first: String,
    app: String,
    workspace: String,
    door: &Door,
) {
    let ctx = Context {
        gate: Gate::App,
        scope: Some(Arc::new(Scope { app, workspace })),
    };
    serve_connection(stream, &ctx, door, Some(first));
}

/// A connection's writer: shared with the thread that streams the desk's state, so a state line and
/// an answer never interleave.
type Writer = Arc<Mutex<UnixStream>>;

/// Read `stream` 's requests one line at a time, answer each, and write its answer, until the
/// client closes it or the session ends. `gate` is the socket it came in on: an op that socket does
/// not serve is refused here, before it reaches anything ([`served`]).
fn serve_connection(stream: UnixStream, ctx: &Context, door: &Door, first: Option<String>) {
    if let Err(error) = stream.set_nonblocking(false) {
        tracing::warn!(%error, "control: a connection cannot block");
        return;
    }
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let writer: Writer = Arc::new(Mutex::new(writer));
    let mut watching: Option<Watching> = None;
    // The app door reads a client's first line to know what it asks before it hands the connection
    // over.
    let reader: Box<dyn io::BufRead + Send> = match first {
        Some(first) => Box::new(BufReader::new(io::Read::chain(
            io::Cursor::new(format!("{first}\n").into_bytes()),
            stream,
        ))),
        None => Box::new(BufReader::new(stream)),
    };
    for line in reader.lines() {
        let Ok(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        let answer = match serde_json::from_str::<Json>(&line) {
            Err(error) => refusal("parse_error", &error.to_string()),
            Ok(request) => match Op::of(&request) {
                None => unsupported(ctx, &request),
                Some(op) if !served(ctx.gate, op) => not_this_doors(ctx.gate, op),
                Some(op) => match asked_here(op, &request, &writer, &mut watching, ctx, door) {
                    Some(answer) => answer,
                    // The loop answers every request it takes; a session that ends drops it
                    // unanswered.
                    None => match ask_loop(op, &request, ctx, door) {
                        Some(answer) => answer,
                        None => return,
                    },
                },
            },
        };
        if talk(&writer, &answer.to_string()).is_err() {
            break;
        }
    }
    if let Some(watching) = watching.take() {
        watching.stop(&door.changes);
    }
}

/// Hand `request` to the loop and wait for its answer; `None` when the session ended.
fn ask_loop(op: Op, request: &Json, ctx: &Context, door: &Door) -> Option<Json> {
    let (answer, answered) = channel();
    door.asks
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .send(Asked {
            op,
            request: request.clone(),
            ctx: ctx.clone(),
            answer,
        })
        .ok()?;
    (door.wake)();
    answered.recv().ok()
}

/// Write one answer line.
fn talk(writer: &Writer, line: &str) -> io::Result<()> {
    let mut stream = writer.lock().unwrap_or_else(PoisonError::into_inner);
    stream.write_all(format!("{line}\n").as_bytes())
}

/// A watching connection's stop flag, and what it takes to end it.
struct Watching {
    stop: Arc<AtomicBool>,
}

impl Watching {
    fn stop(&self, changes: &Arc<Changes>) {
        self.stop.store(true, Ordering::Relaxed);
        changes.poke();
    }
}

/// Stream the desk's state to `writer` until it stops watching, off the change signal the loop
/// publishes on: nothing here polls, and a change wakes the wait. What the connection may see of it
/// is what it is sent ([`shown`]).
fn stream_state(writer: &Writer, ctx: &Context, door: &Door) -> Watching {
    let changes = Arc::clone(&door.changes);
    // Read before attaching: the state attached for is published after this.
    let mut seen = changes.generation();
    changes.attach();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    let writer = Arc::clone(writer);
    let watched = Arc::clone(&changes);
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new()
        .name("tilo-control-watch".into())
        .spawn(move || {
            while let Some((generation, dump)) = watched.wait(seen, Some(&stopped), None) {
                seen = generation;
                let Some(dump) = dump else {
                    continue;
                };
                if talk(&writer, &shown(&ctx, dump).to_string()).is_err() {
                    break;
                }
            }
            watched.detach();
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "control: a state watcher could not be served");
        stop.store(true, Ordering::Relaxed);
        changes.detach();
    }
    Watching { stop }
}

/// The answer to what `op` asks only of the actions and the change signal, when it asks of them:
/// answered where the connection asked, so a wait holds no frame.
fn asked_here(
    op: Op,
    request: &Json,
    writer: &Writer,
    watching: &mut Option<Watching>,
    ctx: &Context,
    door: &Door,
) -> Option<Json> {
    Some(match op {
        Op::ActionsEnumerate => actions_enumerate(request, &door.actions, &door.changes, ctx),
        Op::ActionsInvoke => actions_invoke(request, &door.actions, ctx),
        Op::ActionsStatus => actions_status(request, &door.actions, ctx),
        Op::ActionsCancel => actions_cancel(request, &door.actions, ctx),
        Op::Watch => watch(request, writer, watching, ctx, door),
        Op::RecordRead => record_read(request, &door.actions, ctx),
        _ => return None,
    })
}

/// The answer to `record_read`: the records `records` of `app` in `workspace` (the one in front
/// when unnamed), read through the record door as this person reads them, once they differ from
/// `after` (the `revision` an earlier answer gave) or `wait_ms` passed: as every reader of a
/// record is answered ([`tilo_actions::remote::RecordReadings::readable`]). Each record is watched
/// before it is read, so a reader holding a copy is woken by the next write to any of them.
fn record_read(request: &Json, actions: &Actions, ctx: &Context) -> Json {
    use tilo_actions::remote::{RecordAddress, RecordAsk};
    let Some(app) = request.get("app").and_then(Json::as_str) else {
        return refusal("parse_error", "`record_read` takes `app` and `records`");
    };
    let asks: Vec<RecordAsk> = request
        .get("records")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(Json::as_str)
        .map(|record| RecordAsk {
            address: RecordAddress {
                app: app.to_owned(),
                record: record.to_owned(),
            },
            joined: false,
        })
        .collect();
    let workspace = request.get("workspace").and_then(Json::as_str).map_or_else(
        || {
            ctx.scope.as_ref().map_or_else(
                || actions.front.known(HOST_WAIT).unwrap_or_default(),
                |scope| scope.workspace.clone(),
            )
        },
        str::to_owned,
    );
    let after = request.get("after").and_then(Json::as_str);
    let deadline = wait_deadline(request.get("wait_ms").and_then(Json::as_u64));
    let Some(reads) = actions.host.record_reads() else {
        return refusal(
            "unavailable",
            tilo_actions::remote::RECORD_READS_UNAVAILABLE,
        );
    };
    let caller = actions.caller(ctx).in_workspace(workspace);
    match tilo_actions::remote::read_records(reads.as_ref(), &caller, &asks, after, deadline)
        .map_err(|error| action_refusal(&error))
        .and_then(|readings| {
            readings
                .readable()
                .map_err(|error| refusal("unreadable", &error))
        }) {
        Ok(answer) => json!({ "answer": answer }),
        Err(refused) => refused,
    }
}

/// Start or stop streaming the desk's state to a connection (`watch`).
fn watch(
    request: &Json,
    writer: &Writer,
    watching: &mut Option<Watching>,
    ctx: &Context,
    door: &Door,
) -> Json {
    let wanted = request
        .get("watching")
        .and_then(Json::as_bool)
        .unwrap_or(true);
    if !wanted {
        if let Some(watching) = watching.take() {
            watching.stop(&door.changes);
        }
        return json!({"ok": {}});
    }
    if watching.is_none() {
        *watching = Some(stream_state(writer, ctx, door));
    }
    json!({"ok": {}})
}

/// The answer to what `op` asks of the desk, the pointer, the gestures and the keyboard, on the
/// loop's pass.
fn loop_answer(
    op: Op,
    request: &Json,
    ctx: &Context,
    compositor: &mut Compositor,
    desk: &Desk<'_>,
    held: &RefCell<BTreeSet<u32>>,
    fingers_down: &std::cell::Cell<Option<u32>>,
) -> Json {
    if let Some(answer) = asked_of_desk(ctx, op, request, compositor, desk) {
        return answer;
    }
    if let Some(answer) = asked_of_pointer(op, request, compositor) {
        return answer;
    }
    if let Some(answer) = asked_of_gestures(op, request, compositor, fingers_down) {
        return answer;
    }
    if let Some(answer) = asked_of_keys(op, request, compositor, held) {
        return answer;
    }
    // A served op always has an answer above.
    refusal(
        "unsupported",
        &format!("`{}` answers nothing here", op.name()),
    )
}

/// The answer to what `op` asks of the desk and its hosted surfaces, when it asks of them.
fn asked_of_desk(
    ctx: &Context,
    op: Op,
    request: &Json,
    compositor: &mut Compositor,
    desk: &Desk<'_>,
) -> Option<Json> {
    Some(match op {
        Op::Snapshot => {
            let path = request
                .get("path")
                .and_then(Json::as_str)
                .unwrap_or_default();
            match snapshot(compositor, path) {
                // Whether the picture is of the lock screen, which is all a locked display shows: a
                // tool reads it, never a blank frame.
                Ok(()) => json!({"ok": {"frame": 0, "locked": desk.locked}}),
                Err((code, message)) => refusal(code, &message),
            }
        }
        // The session's lock comes down as it does for a password that unlocks: the screen
        // dissolves, and the compositor lifts the lock once it has.
        Op::Unlock => {
            let was_locked = desk.locked;
            if was_locked {
                desk.lock.unlocked();
            }
            json!({"ok": {"locked": false, "was_locked": was_locked}})
        }
        Op::ProvisionTestIdentity => provision_test_identity(),
        Op::ListTargets => json!({"targets": listed_targets(desk)}),
        Op::ClickNamed => return Some(clicked_named(request, desk)),
        Op::SearchRows => {
            let (query, rows) = desk.handle.search_rows();
            let rows: Vec<Json> = rows
                .iter()
                .map(|row| {
                    json!({
                        "title": row.title,
                        "subtitle": row.subtitle,
                        "kind": row.kind,
                        "facts": row.facts,
                    })
                })
                .collect();
            // Whether the search still owes rows: a tool timing what a person waits for stops on
            // the rows being final, not on rows that are about to change under the same query.
            json!({"answer": {
                "query": query,
                "rows": rows,
                "searching": desk.handle.search_searching(),
            }})
        }
        Op::ProcessMemory => json!({"answer": tilo_compositor::memory_report::report()}),
        Op::ProfileStart => profile_start(request),
        Op::ProfileStatus => json!({"answer": crate::diagnostics::reading()}),
        Op::ProfileStop => json!({"answer": {"profile": tilo_frame_metrics::profile::stop()}}),
        Op::StateDump => shown(ctx, desk.handle.read(|tree| state_dump(tree, desk))),
        Op::WorkspaceHosts => desk
            .handle
            .read(|tree| state::workspace_hosts(tree, desk, ctx)),
        Op::Windows => {
            // Nothing shows a window while the screen is locked.
            let rows = if ctx.gate != Gate::Dev && desk.locked {
                Vec::new()
            } else {
                // Where each window is drawn, read off the frame's own layout: a row says what a
                // tool aiming at that window aims at. How many frames it drew is read off the
                // compositor, where the client's commits arrive.
                let rect_of = |id: u64| compositor.window_rect(id);
                let frames_of = |id: u64| compositor.window_frames(id);
                match &ctx.scope {
                    None => desk
                        .handle
                        .read(|tree| window_rows(tree, rect_of, frames_of)),
                    Some(scope) => desk
                        .handle
                        .read(|tree| window_rows(&within(tree, scope), rect_of, frames_of)),
                }
            };
            json!({"answer": {"windows": rows}})
        }
        _ => return None,
    })
}

/// Start profiling the session for `seconds` (default [`profile::DEFAULT_SECONDS`], at most
/// [`profile::MAX_SECONDS`]) into the session's profile directory: the loop records its own frame
/// rows, phases and threads there, and the summary is written last.
fn profile_start(request: &Json) -> Json {
    use tilo_frame_metrics::profile;
    let Some(root) = profile::default_root() else {
        return refusal(
            "unavailable",
            "there is no state directory to keep the profile in",
        );
    };
    let seconds = match request.get("seconds").map(Json::as_u64) {
        None => profile::DEFAULT_SECONDS,
        Some(Some(seconds)) if (1..=profile::MAX_SECONDS).contains(&seconds) => seconds,
        Some(_) => {
            return refusal(
                "invalid_arguments",
                &format!(
                    "`profile_start` takes `seconds` from 1 to {}",
                    profile::MAX_SECONDS
                ),
            );
        }
    };
    match profile::start(&root, std::time::Duration::from_secs(seconds)) {
        Ok(status) => json!({"answer": {"profile": status}}),
        Err(profile::StartError::AlreadyRecording(path)) => refusal(
            "action_failed",
            &format!("a profile is already recording into {}", path.display()),
        ),
        Err(profile::StartError::Io(error)) => refusal(
            "action_failed",
            &format!("the profile directory could not be created: {error}"),
        ),
    }
}

/// The answer to what `op` asks of the pointer, when it asks of it.
fn asked_of_pointer(op: Op, request: &Json, compositor: &mut Compositor) -> Option<Json> {
    Some(match op {
        Op::ClickAt => {
            let (Some(x), Some(y)) = (number(request, "x"), number(request, "y")) else {
                return Some(refusal("parse_error", "`click_at` takes `x` and `y`"));
            };
            let button = match button(request, "button") {
                Ok(button) => button,
                Err(refusal) => return Some(refusal),
            };
            if let Err(error) = compositor.input(InputEvent::PointerMotion { x, y }) {
                return Some(refusal("input_failed", &error.to_string()));
            }
            tap(compositor, button)
        }
        Op::PointerMove => {
            let (Some(x), Some(y)) = (number(request, "x"), number(request, "y")) else {
                return Some(refusal("parse_error", "`pointer_move` takes `x` and `y`"));
            };
            answer(compositor.input(InputEvent::PointerMotion { x, y }))
        }
        Op::PointerButton => {
            let Some(pressed) = request.get("pressed").and_then(Json::as_bool) else {
                return Some(refusal("parse_error", "`pointer_button` takes `pressed`"));
            };
            let button = match button(request, "button") {
                Ok(button) => button,
                Err(refusal) => return Some(refusal),
            };
            answer(compositor.input(InputEvent::PointerButton { button, pressed }))
        }
        Op::Drag => {
            let (Some(x_from), Some(y_from), Some(x_to), Some(y_to)) = (
                number(request, "x_from"),
                number(request, "y_from"),
                number(request, "x_to"),
                number(request, "y_to"),
            ) else {
                return Some(refusal(
                    "parse_error",
                    "`drag` takes `x_from`, `y_from`, `x_to` and `y_to`",
                ));
            };
            let steps = request
                .get("steps")
                .and_then(Json::as_u64)
                .unwrap_or(8)
                .clamp(1, 1_000);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a drag's steps are clamped to a thousand"
            )]
            let steps = steps as u32;
            let button = match button(request, "button") {
                Ok(button) => button,
                Err(refusal) => return Some(refusal),
            };
            match drag(compositor, button, (x_from, y_from), (x_to, y_to), steps) {
                Ok(()) => json!({"ok": {}}),
                Err(error) => refusal("input_failed", &error),
            }
        }
        Op::Scroll => {
            let dx = number(request, "dx").unwrap_or(0.0);
            let dy = number(request, "dy").unwrap_or(0.0);
            // A notch up is the old wire's positive `dy` ; the compositor speaks the protocol's
            // axis, which counts down as positive.
            answer(compositor.input(InputEvent::PointerAxis {
                horizontal: -dx,
                vertical: -dy,
            }))
        }
        _ => return None,
    })
}

/// The answer to what `op` asks of the keyboard, when it asks of it.
fn asked_of_keys(
    op: Op,
    request: &Json,
    compositor: &mut Compositor,
    held: &RefCell<BTreeSet<u32>>,
) -> Option<Json> {
    Some(match op {
        Op::Key => {
            let pressed = match typed_char(request) {
                // A character is pressed as the keyboard types it, with the Shift or AltGr its layout
                // needs held: `/` on a German keyboard is Shift+7.
                Some(ch) => compositor.type_char(ch).map_err(|error| error.to_string()),
                None => match key_code(request, compositor) {
                    Some(code) => [true, false]
                        .into_iter()
                        .try_for_each(|pressed| {
                            compositor.input(InputEvent::Key {
                                keycode: code,
                                pressed,
                            })
                        })
                        .map(|()| true)
                        .map_err(|error| error.to_string()),
                    None => Ok(false),
                },
            };
            match pressed {
                Ok(true) => json!({"ok": {}}),
                Ok(false) => refusal(
                    "parse_error",
                    "`key` takes a named key or one character the keyboard types",
                ),
                Err(error) => refusal("input_failed", &error),
            }
        }
        Op::KeyDown => match key_code(request, compositor) {
            Some(code) => {
                if let Err(error) = compositor.input(InputEvent::Key {
                    keycode: code,
                    pressed: true,
                }) {
                    return Some(refusal("input_failed", &error.to_string()));
                }
                held.borrow_mut().insert(code);
                json!({"ok": {}})
            }
            None => refusal("parse_error", "`key_down` takes a named key"),
        },
        Op::KeyUp => match key_code(request, compositor) {
            Some(code) => {
                if let Err(error) = compositor.input(InputEvent::Key {
                    keycode: code,
                    pressed: false,
                }) {
                    return Some(refusal("input_failed", &error.to_string()));
                }
                held.borrow_mut().remove(&code);
                json!({"ok": {}})
            }
            None => refusal("parse_error", "`key_up` takes a named key"),
        },
        Op::Modifiers => {
            let wanted = [
                (KEY_CTRL, request.get("ctrl").and_then(Json::as_bool)),
                (KEY_SHIFT, request.get("shift").and_then(Json::as_bool)),
                (KEY_ALT, request.get("alt").and_then(Json::as_bool)),
                (KEY_SUPER, request.get("logo").and_then(Json::as_bool)),
            ];
            for (code, pressed) in wanted {
                let pressed = pressed.unwrap_or(false);
                let was = held.borrow().contains(&code);
                if pressed == was {
                    continue;
                }
                if let Err(error) = compositor.input(InputEvent::Key {
                    keycode: code,
                    pressed,
                }) {
                    return Some(refusal("input_failed", &error.to_string()));
                }
                if pressed {
                    held.borrow_mut().insert(code);
                } else {
                    held.borrow_mut().remove(&code);
                }
            }
            json!({"ok": {}})
        }
        Op::Type => {
            let Some(text) = request.get("text").and_then(Json::as_str) else {
                return Some(refusal("parse_error", "`type` takes `text`"));
            };
            match type_text(compositor, text) {
                Ok(()) => json!({"ok": {}}),
                Err(error) => refusal("input_failed", &error),
            }
        }
        _ => return None,
    })
}

/// The actions the control sockets invoke, and how they are asked as.
struct Actions {
    /// The broker, with the session's own doors beside it ([`crate::agent_door::Host`]).
    host: Arc<agent_door::Host>,
    /// The person's own caller, made once and cloned: the broker remembers an invocation under the
    /// caller that admitted it, so a status or a cancel reads it back as the same caller.
    caller: Caller,
    /// The workspace in front, as the loop last served it: what an invocation opens into.
    front: Front,
    /// The number a tool was answered with, and the invocation it names.
    ledger: Mutex<BTreeMap<u64, InvocationId>>,
    next: AtomicU64,
    /// Whether the screen is locked, as the loop last read it: the owner's door invokes nothing
    /// there.
    locked: AtomicBool,
}

/// How long a host may take to answer a request at all.
const HOST_WAIT: Duration = Duration::from_secs(30);

/// The longest a control request waits: what `tilo-control` says a desktop caps a wait at.
const MAX_WAIT: Duration = Duration::from_secs(30);

/// How long a settled wait stands by the host's own answer once it gave one.
const SETTLE_WAIT: Duration = Duration::from_secs(35);

/// The workspace in front, as the loop last served the desk, and the wait for a request that
/// arrives before the desk holds one.
///
/// A session adopts the person's workspaces in its first passes, so a desk holds none for the
/// moment between its first frame and them: what a request asks in that moment is where the person
/// is, which is not yet known, and waiting for it is the answer — the person's own authority over
/// their own desk is not in question, so a refusal would be a lie.
#[derive(Default)]
struct Front {
    workspace: Mutex<String>,
    /// The loop served a workspace: a request waiting for one looks again.
    arrived: Condvar,
}

impl Front {
    /// The loop served the desk: `workspace` is the one in front now.
    fn served(&self, workspace: String) {
        let mut held = self.workspace.lock_recover();
        if *held != workspace {
            *held = workspace;
            drop(held);
            self.arrived.notify_all();
        }
    }

    /// The workspace in front, once the desk holds one, waiting up to `wait` for a desk that
    /// started before it had any: `None` when there is none to name yet.
    fn known(&self, wait: Duration) -> Option<String> {
        let until = Instant::now().checked_add(wait)?;
        let mut held = self.workspace.lock_recover();
        loop {
            if !held.is_empty() {
                return Some(held.clone());
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            let (next, timeout) = self
                .arrived
                .wait_timeout(held, left)
                .unwrap_or_else(PoisonError::into_inner);
            if timeout.timed_out() && next.is_empty() {
                return None;
            }
            held = next;
        }
    }
}

impl Actions {
    /// Who a request here is made as: the person at this desktop, in the workspace in front,
    /// or the app a program inside it runs as, in the workspace the session started it into.
    fn caller(&self, ctx: &Context) -> Caller {
        if let Some(scope) = &ctx.scope {
            return Caller::local_app(scope.app.clone()).in_workspace(scope.workspace.clone());
        }
        let caller = self.caller.clone();
        match self.front.known(HOST_WAIT) {
            Some(workspace) => caller.in_workspace(workspace),
            None => caller,
        }
    }

    /// Whose workspace is in front, as the loop read it: a desk that holds none names none.
    fn set_workspace(&self, workspace: Option<String>) {
        if let Some(workspace) = workspace {
            self.front.served(workspace);
        }
    }

    /// Remember `invocation` under a fresh number for the tools.
    fn remember(&self, invocation: InvocationId) -> u64 {
        let number = self.next.fetch_add(1, Ordering::Relaxed);
        self.ledger.lock_recover().insert(number, invocation);
        number
    }

    /// Whether the screen is locked, as the loop last read it, and where it stands now.
    fn set_locked(&self, locked: bool) {
        self.locked.store(locked, Ordering::Relaxed);
    }

    fn locked(&self) -> bool {
        self.locked.load(Ordering::Relaxed)
    }

    /// The invocation a tool's number names, while this session remembers it.
    fn invocation(&self, number: u64) -> Option<InvocationId> {
        self.ledger.lock_recover().get(&number).copied()
    }
}

/// The answer to `actions_enumerate` : the rows the actions answer with now, in the shape the tools
/// that drive a desktop read, held until they move past `after_revision` or the listing is
/// complete.
fn actions_enumerate(
    request: &Json,
    actions: &Actions,
    changes: &Arc<Changes>,
    ctx: &Context,
) -> Json {
    let compact = request.get("detail").and_then(Json::as_str) == Some("compact");
    let kinds: Vec<String> = request
        .get("kinds")
        .and_then(Json::as_array)
        .map(|kinds| {
            kinds
                .iter()
                .filter_map(Json::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let wait_ms = request.get("wait_ms").and_then(Json::as_u64);
    let after = request
        .get("after_revision")
        .and_then(Json::as_str)
        .map(str::to_owned);
    // A query of several words is read as the tools read it: the host narrows to the first word,
    // and the rest narrow what it answered ([`legacy_rows`]).
    let words: Vec<String> = request
        .get("query")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    let paging = request
        .get("cursor")
        .and_then(Json::as_str)
        .is_some_and(|cursor| !cursor.is_empty());
    let files = request
        .get("files")
        .and_then(Json::as_bool)
        .unwrap_or(false);
    let deadline = wait_deadline(wait_ms);
    let mut seen = changes.generation();
    loop {
        let page = match catalogue_page(request, actions, &words, ctx) {
            Ok(page) => page,
            Err(answer) => return answer,
        };
        if listing_settled(&page, after.as_deref(), paging, wait_ms.is_some()) {
            return listing(&page, &words, compact, &kinds, files);
        }
        let Some((generation, _)) = changes.wait(seen, None, Some(deadline)) else {
            return listing(&page, &words, compact, &kinds, files);
        };
        seen = generation;
    }
}

/// Whether the listing `page` is the one to answer: the one after `after` , or, without one, a
/// complete listing — the whole of it, or its first page when more pages follow.
fn listing_settled(page: &CataloguePage, after: Option<&str>, paging: bool, waits: bool) -> bool {
    if let Some(after) = after {
        return page.revision != after;
    }
    !waits || paging || page.exhausted || !page.next_cursor.is_empty()
}

/// One page of what the caller is offered, as the broker and the session's own doors answer it.
fn catalogue_page(
    request: &Json,
    actions: &Actions,
    words: &[String],
    ctx: &Context,
) -> Result<CataloguePage, Json> {
    let limit = request.get("limit").and_then(Json::as_u64);
    let mut query = CatalogueQuery {
        needle: words.first().cloned().unwrap_or_default(),
        max_results: limit.map_or(MAX_CATALOGUE_PAGE, |limit| {
            u32::try_from(limit)
                .unwrap_or(u32::MAX)
                .clamp(1, MAX_CATALOGUE_PAGE)
        }),
        ..CatalogueQuery::default()
    };
    if let Some(token) = request
        .get("cursor")
        .and_then(Json::as_str)
        .filter(|token| !token.is_empty())
    {
        let Some((revision, cursor)) = decode_cursor(token) else {
            return Err(refusal(
                "invalid_arguments",
                "the cursor is not one this desktop printed",
            ));
        };
        query.revision = Some(revision);
        query.cursor = cursor;
    }
    let caller = actions.caller(ctx);
    match actions.host.query(&caller, query).wait_timeout(HOST_WAIT) {
        Ok(Some(Ok(page))) => Ok(page),
        Ok(Some(Err(error))) => Err(action_refusal(&error)),
        Ok(None) | Err(_) => Err(refusal("unavailable", "the listing did not answer")),
    }
}

/// The answer to a page: its rows as the tools know them, and the cursor of the next one.
fn listing(
    page: &CataloguePage,
    words: &[String],
    compact: bool,
    kinds: &[String],
    files: bool,
) -> Json {
    if page.stale_cursor {
        return refusal("stale_cursor", "the listing moved on; list it again");
    }
    // Files and folders are the resources behind actions, left out of a listing unless they were
    // asked for — by `files` , or by a kind filter that may name them.
    let resources = files || !kinds.is_empty();
    let mut rows = legacy_rows(page, words, compact, resources);
    if !kinds.is_empty() {
        rows.retain(|row| {
            row.get("kind")
                .and_then(Json::as_str)
                .is_some_and(|kind| kinds.iter().any(|wanted| kind_matches(kind, wanted)))
        });
    }
    let truncated = !page.exhausted || page.budget_exhausted;
    let mut answer = json!({
        "count": rows.len(),
        "actions": rows,
        "revision": page.revision,
        "complete": !truncated,
        "truncated": truncated,
        // This desktop holds a listing request until the listing settles or moves on.
        "settled": true,
    });
    if !page.next_cursor.is_empty()
        && let Some(fields) = answer.as_object_mut()
    {
        fields.insert(
            "next_cursor".into(),
            encode_cursor(&page.revision, &page.next_cursor).into(),
        );
    }
    json!({"answer": answer})
}

/// Whether a row of `kind` answers a `--kind` filter: its kind, in any case, and the names the
/// tools use for an app or an action.
fn kind_matches(kind: &str, wanted: &str) -> bool {
    let wanted = wanted.to_lowercase();
    kind.eq_ignore_ascii_case(&wanted)
        || (kind == KIND_APPS
            && matches!(
                wanted.as_str(),
                "app" | "apps" | "application" | "applications"
            ))
        || (kind == KIND_ACTIONS && matches!(wanted.as_str(), "action" | "actions"))
}

/// The kind a row that opens an app is listed as, as the tools group and filter it.
const KIND_APPS: &str = "Apps";
/// The kind a row that runs an action is listed as.
const KIND_ACTIONS: &str = "Actions";

/// The page's rows as the tools that drive a desktop address them.
/// An app the workspace can open is `applications:<app id>` ; an action an app publishes while it
/// runs is `app-actions:<provider>␟<address>` ; any other action is `<provider>:<address>` . The
/// address is the action model's own — resource, then action ([`ActionAddress::encode`]) — which
/// the tools print escaped and take back.
fn legacy_rows(
    page: &CataloguePage,
    words: &[String],
    compact: bool,
    resources: bool,
) -> Vec<Json> {
    page.rows
        .iter()
        .filter(|row| resources || row.action.is_some())
        .filter_map(legacy_row)
        .filter(|row| row_mentions(row, words))
        .map(|mut row| {
            if compact && let Some(fields) = row.as_object_mut() {
                fields.remove("contract");
            }
            row
        })
        .collect()
}

/// The controls the desk shows now, as the tools read them: each one's name, what it does where it
/// does something the app publishes, and where it stands.
fn listed_targets(desk: &Desk<'_>) -> Vec<Json> {
    desk.doors
        .controls()
        .into_iter()
        .map(|control| {
            let (x, y, w, h) = control.rect;
            match control.identity {
                Some(identity) => json!({
                    "name": control.name,
                    "identity": identity,
                    "rect": [x, y, w, h],
                }),
                None => json!({"name": control.name, "rect": [x, y, w, h]}),
            }
        })
        .collect()
}

/// The answer to `click_named`: the control the caller names, by its name or by what it does, is
/// pressed as assistive technology presses it — or the desk says it shows none.
fn clicked_named(request: &Json, desk: &Desk<'_>) -> Json {
    // What the control does, where the caller names it that way: the identity it keeps wherever
    // the window's composition draws it, in whatever language.
    let identity = request.get("identity").and_then(Json::as_str);
    let Some(name) = request.get("name").and_then(Json::as_str).or(identity) else {
        return refusal("parse_error", "`click_named` takes `name` or `identity`");
    };
    // The verb names the action to take on the control: a press (`activate`, the default) or the
    // context menu it opens without a point aimed at where it is drawn.
    let action = match request.get("verb").and_then(Json::as_str) {
        None | Some("activate") => ui::window_targets::TargetAction::Activate,
        Some("show-context-menu") => ui::window_targets::TargetAction::ShowContextMenu,
        Some(other) => {
            return refusal(
                "parse_error",
                &format!("`click_named` does not know the verb {other}"),
            );
        }
    };
    let pressed = if let Some(identity) = identity {
        desk.doors.press_identity(identity, action)
    } else {
        desk.doors.press_action(name, action)
    };
    match pressed {
        Ok(()) => json!({"ok": {}}),
        Err(error) => refusal("unknown_target", &error),
    }
}

/// One row in the tools' shape, or `None` for a row that only holds more rows.
fn legacy_row(row: &tilo_actions::remote::CatalogueRow) -> Option<Json> {
    let action = row.action.as_ref();
    let mut described = json!({
        "provider": row.provider,
        "kind": action.map_or_else(|| row.resource.kind.clone(), |_| KIND_ACTIONS.to_owned()),
        "operation_kind": action.map(|action| {
            serde_json::to_value(action.kind).unwrap_or(Json::Null)
        }),
        "title": action.map_or_else(
            || row.resource.label.clone(),
            |action| action.action_label.clone(),
        ),
        "subtitle": row.resource.label,
        "params": action.map_or_else(Vec::new, |action| {
            action.params.iter().map(param_row).collect()
        }),
        "contract": action.map(|action| {
            serde_json::to_value(&action.contract).unwrap_or(Json::Null)
        }),
        "application": row.app_id,
        "record": "",
        "resource": row.resource.id,
        "semantic_identity": action.map(|action| action.semantic_identity.clone()),
        "operation": action.map(|action| action.address.action.clone()),
    });
    let fields = described.as_object_mut()?;
    match action {
        None => {
            fields.insert("id".into(), row.resource.id.clone().into());
        }
        Some(action) => {
            if opens_app(row, action) {
                fields.insert("provider".into(), "applications".into());
                fields.insert("id".into(), row.app_id.clone().into());
                fields.insert("kind".into(), KIND_APPS.into());
                fields.insert("title".into(), row.resource.label.clone().into());
            } else if is_app_action(&row.provider) {
                fields.insert("provider".into(), "app-actions".into());
                fields.insert(
                    "id".into(),
                    format!("{}\u{1f}{}", row.provider, action.address.encode()).into(),
                );
            } else {
                fields.insert("id".into(), action.address.encode().into());
            }
        }
    }
    Some(described)
}

/// One parameter, as the tools read it.
fn param_row(param: &tilo_actions::ActionParam) -> Json {
    json!({
        "name": param.name,
        "type": param.ty.label(),
        "required": param.required,
        "description": param.description,
        "choices": param.choices,
        "nullable": param.nullable,
    })
}

/// Whether `row` is the workspace's open of an app: the action a person picks an app by.
fn opens_app(
    row: &tilo_actions::remote::CatalogueRow,
    action: &tilo_actions::ActionDescriptor,
) -> bool {
    action.address.action == tilo_actions::remote::APPLICATION_OPEN_ACTION
        && action
            .address
            .resource
            .starts_with(tilo_actions::remote::APPLICATION_RESOURCE_PREFIX)
        && tilo_actions::remote::provider_workspace(&row.provider).is_some()
}

/// Whether `provider` names an app's own actions: the instance an app runs in, or an app the
/// desktop launches.
fn is_app_action(provider: &str) -> bool {
    provider.starts_with("application:") || provider.starts_with("launch:")
}

/// Whether `row` mentions every word the query asked for, over what the tools see of it.
fn row_mentions(row: &Json, words: &[String]) -> bool {
    if words.len() < 2 {
        return true;
    }
    let mut fields = Vec::new();
    for key in [
        "id",
        "title",
        "subtitle",
        "kind",
        "application",
        "operation",
        "resource",
    ] {
        if let Some(text) = row.get(key).and_then(Json::as_str) {
            fields.push(text.to_lowercase());
        }
    }
    words
        .iter()
        .all(|word| word.is_empty() || fields.iter().any(|field| field.contains(word.as_str())))
}

/// A cursor as the tools carry it: the revision it was cut at and the actions' continuation after
/// it, so the next page reads on in the listing it was taken from.
fn encode_cursor(revision: &str, cursor: &[u8]) -> String {
    format!("{revision}~{}", hex::encode(cursor))
}

/// [`encode_cursor`] undone, `None` for a token this desktop did not print.
fn decode_cursor(token: &str) -> Option<(String, Vec<u8>)> {
    let (revision, cursor) = token.rsplit_once('~')?;
    Some((revision.to_owned(), hex::decode(cursor).ok()?))
}

/// The answer to `actions_invoke` : the receipt, by a number the tools follow it with. The owner's
/// door invokes nothing while the screen is locked: what a person's own scripts run is what the
/// person at the device could run, and a locked desktop runs nothing. `confirmed` stands for the
/// person's own answer to an action that asks first, as pressing Allow in its prompt does.
fn actions_invoke(request: &Json, actions: &Actions, ctx: &Context) -> Json {
    if ctx.gate != Gate::Dev && actions.locked() {
        return refusal(
            "locked",
            "the session is locked; unlock it at the device, then invoke again",
        );
    }
    let Some(provider) = request.get("provider").and_then(Json::as_str) else {
        return refusal("parse_error", "`actions_invoke` takes `provider`");
    };
    let Some(id) = request.get("id").and_then(Json::as_str) else {
        return refusal("parse_error", "`actions_invoke` takes `id`");
    };
    if request.get("device").and_then(Json::as_str).is_some() {
        return refusal(
            "unsupported",
            "this desktop runs an action here; sending one to another device is not served at \
             this door",
        );
    }
    // Which workspace the action is asked in is the desk's answer, not a copy of it: a request
    // that lands while the session is still adopting the person's workspaces waits for the desk
    // rather than naming none and being refused as if the person's own desk were not theirs.
    let workspace = match &ctx.scope {
        Some(scope) => scope.workspace.clone(),
        None => match actions.front.known(HOST_WAIT) {
            Some(workspace) => workspace,
            None => {
                return refusal(
                    "unavailable",
                    "this desk holds no workspace yet; there is nothing in front to act in",
                );
            }
        },
    };
    let target = match legacy_target(provider, id, &workspace) {
        Ok(target) => target,
        Err(answer) => return answer,
    };
    let argument = match request.get("args") {
        None | Some(Json::Null) => InvokeArgument::None,
        Some(Json::Object(object)) if object.is_empty() => InvokeArgument::None,
        Some(Json::Object(object)) => InvokeArgument::Json(object.clone()),
        Some(other) => {
            return refusal(
                "parse_error",
                &format!("`args` is an object of arguments, not {other}"),
            );
        }
    };
    let invocation = InvocationId::new();
    let caller = actions.caller(ctx);
    let confirmation = match request.get("confirmed").and_then(Json::as_bool) {
        Some(true) => Some(tilo_actions::remote::Confirmation::Confirmed),
        _ => None,
    };
    let invoke = InvokeRequest {
        invocation,
        target,
        argument,
        context: None,
        confirmation,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    };
    // The person's own actions are the seat's; a program inside an app acts as that app, and the
    // broker's per-action permissions decide for it.
    let started = match ctx.scope {
        Some(_) => actions.host.invoke(&caller, invoke),
        None => actions.host.invoke_at_seat(&caller, invoke),
    };
    let receipt = match started.wait_timeout(HOST_WAIT) {
        Ok(Some(Ok(receipt))) => receipt,
        Ok(Some(Err(error))) => return action_refusal(&error),
        Ok(None) | Err(_) => return refusal("unavailable", "the action did not start"),
    };
    receipt_answer(
        actions,
        &caller,
        &receipt,
        request.get("wait_ms").and_then(Json::as_u64),
    )
}

/// The answer to `actions_status` : where an invocation a tool started stands now.
fn actions_status(request: &Json, actions: &Actions, ctx: &Context) -> Json {
    let Some(number) = request.get("invocation_id").and_then(Json::as_u64) else {
        return refusal("parse_error", "`actions_status` takes `invocation_id`");
    };
    let Some(invocation) = actions.invocation(number) else {
        return refusal(
            "unknown_invocation",
            &format!("no invocation {number} is known here"),
        );
    };
    let caller = actions.caller(ctx);
    let current = actions
        .host
        .observe(&caller, &invocation)
        .map_or(InvocationUpdate::Accepted { invocation }, |state| {
            state.update
        });
    let update = match request.get("wait_ms").and_then(Json::as_u64) {
        Some(wait_ms) if wait_ms > 0 && !current.is_terminal() => {
            settled(actions, &caller, invocation, current, wait_ms)
        }
        _ => current,
    };
    json!({"answer": {"receipt": {
        "broker_invocation_id": number,
        "invocation": invocation,
        "update": update,
    }}})
}

/// The answer to `actions_cancel` : the invocation asked to stop.
fn actions_cancel(request: &Json, actions: &Actions, ctx: &Context) -> Json {
    let Some(number) = request.get("invocation_id").and_then(Json::as_u64) else {
        return refusal("parse_error", "`actions_cancel` takes `invocation_id`");
    };
    let Some(invocation) = actions.invocation(number) else {
        return refusal(
            "unknown_invocation",
            &format!("no invocation {number} is known here"),
        );
    };
    let caller = actions.caller(ctx);
    match actions
        .host
        .cancel(&caller, &invocation)
        .wait_timeout(HOST_WAIT)
    {
        Ok(Some(Ok(()))) => json!({"answer": {}}),
        Ok(Some(Err(error))) => action_refusal(&error),
        Ok(None) | Err(_) => refusal("unavailable", "the action did not answer the cancel"),
    }
}

/// A receipt's answer, held while the invocation runs when the tool asked to wait for it.
fn receipt_answer(
    actions: &Actions,
    caller: &Caller,
    receipt: &InvocationReceipt,
    wait_ms: Option<u64>,
) -> Json {
    if let ReceiptStatus::Rejected { error } = &receipt.status {
        return action_refusal(error);
    }
    let invocation = receipt.invocation;
    let number = actions.remember(invocation);
    let mut update = crate::surfaces::outcome(invocation, &receipt.status)
        .unwrap_or(InvocationUpdate::Accepted { invocation });
    if let Some(wait_ms) = wait_ms.filter(|wait_ms| *wait_ms > 0)
        && !update.is_terminal()
    {
        update = settled(actions, caller, invocation, update, wait_ms);
    }
    json!({"answer": {"receipt": {
        "broker_invocation_id": number,
        "invocation": invocation,
        "update": update,
    }}})
}

/// The invocation's state once it settles, or what it has got to by `wait_ms` from now.
fn settled(
    actions: &Actions,
    caller: &Caller,
    invocation: InvocationId,
    current: InvocationUpdate,
    wait_ms: u64,
) -> InvocationUpdate {
    let deadline = wait_deadline(Some(wait_ms));
    match settle_with(actions.host.as_ref(), caller, invocation, None, deadline)
        .wait_timeout(SETTLE_WAIT)
    {
        Ok(Some(Some(state))) => state.update,
        _ => actions
            .host
            .observe(caller, &invocation)
            .map_or(current, |state| state.update),
    }
}

/// When a wait of `wait_ms` started now ends, capped at what a control request waits.
fn wait_deadline(wait_ms: Option<u64>) -> Instant {
    let wait = Duration::from_millis(wait_ms.unwrap_or(0)).min(MAX_WAIT);
    Instant::now()
        .checked_add(wait)
        .unwrap_or_else(Instant::now)
}

/// The target a tool's `(provider, id)` names, in the action model's own addressing.
fn legacy_target(provider: &str, id: &str, workspace: &str) -> Result<RemoteActionTarget, Json> {
    // The workspace verbs by the names tools spell them (`workspace.create`, `workspace.branch`):
    // the desk's own actions, as the workspace menu invokes them.
    if provider == "actions"
        && let Some(action) = match id {
            "workspace.create" => Some("new-workspace"),
            "workspace.branch" => Some("branch-workspace"),
            _ => None,
        }
    {
        return Ok(RemoteActionTarget {
            provider: tilo_actions::remote::SESSION_ACTIONS_PROVIDER.to_owned(),
            address: ActionAddress::new(tilo_desktop::desk_verbs::CHROME_RESOURCE, action),
        });
    }
    if provider == "applications" {
        if id.is_empty() {
            return Err(refusal("unknown_action", "`id` is the app's id"));
        }
        return Ok(tilo_actions::remote::application_open_target(workspace, id));
    }
    let (provider, address) = if provider == "app-actions" {
        let Some((provider, address)) = id.split_once('\u{1f}') else {
            return Err(refusal(
                "unknown_action",
                "an app-actions id is `provider␟resource␟action`, as `tilo-control actions list` \
                 printed it",
            ));
        };
        (provider, address)
    } else {
        (provider, id)
    };
    let Some(address) = ActionAddress::parse(address) else {
        return Err(refusal(
            "unknown_action",
            "an action's id is `resource␟action`, as `tilo-control actions list` printed it",
        ));
    };
    Ok(RemoteActionTarget {
        provider: provider.to_owned(),
        address,
    })
}

/// A host's refusal as the tools read it: the codes they know, and what they mean here.
fn action_refusal(error: &ActionHostError) -> Json {
    let code = match error {
        // This door's own refusal is `not_permitted` , which a tool reads as the socket not serving
        // it at all; an action the person may not run is another answer.
        ActionHostError::NotPermitted => "not_allowed",
        ActionHostError::StaleRevision => "stale_cursor",
        ActionHostError::ConfirmationRequired { .. } => "needs_confirmation",
        other => other.code(),
    };
    refusal(code, &error.to_string())
}

/// `request` 's `name` field, when it is a number.
fn number(request: &Json, name: &str) -> Option<f64> {
    request.get(name).and_then(Json::as_f64)
}

/// The button `request` 's `name` field names: `left` unless it says `right` or `middle` .
fn button(request: &Json, name: &str) -> Result<u32, Json> {
    match request.get(name).and_then(Json::as_str) {
        None | Some("left") => Ok(BUTTON_LEFT),
        Some("right") => Ok(BUTTON_RIGHT),
        Some("middle") => Ok(BUTTON_MIDDLE),
        Some(other) => Err(refusal(
            "parse_error",
            &format!("`{name}` is `left`, `right` or `middle`, not `{other}`"),
        )),
    }
}

/// The evdev codes the pointer's buttons go by.
const BUTTON_LEFT: u32 = 0x110;
const BUTTON_RIGHT: u32 = 0x111;
const BUTTON_MIDDLE: u32 = 0x112;

/// The evdev codes of the keys a `modifiers` request holds.
const KEY_CTRL: u32 = 29;
const KEY_SHIFT: u32 = 42;
const KEY_ALT: u32 = 56;
const KEY_SUPER: u32 = 125;

/// `ok` when the input took `result` , its refusal otherwise.
fn answer(result: Result<(), tilo_compositor::Error>) -> Json {
    match result {
        Ok(()) => json!({"ok": {}}),
        Err(error) => refusal("input_failed", &error.to_string()),
    }
}

/// One place of a drag: `from` to `to` , `at` of the way.
fn drag_step(from: (f64, f64), to: (f64, f64), at: f64) -> (f64, f64) {
    (from.0 + (to.0 - from.0) * at, from.1 + (to.1 - from.1) * at)
}

/// Press and release `button` where the pointer is, as the person clicks there.
fn tap(compositor: &mut Compositor, button: u32) -> Json {
    for pressed in [true, false] {
        if let Err(error) = compositor.input(InputEvent::PointerButton { button, pressed }) {
            return refusal("input_failed", &error.to_string());
        }
    }
    json!({"ok": {}})
}

/// A press at `from` , `steps` motions spread evenly to `to` , and the release there: the gesture
/// that moves a tab into another group, or a file into a window.
fn drag(
    compositor: &mut Compositor,
    button: u32,
    from: (f64, f64),
    to: (f64, f64),
    steps: u32,
) -> Result<(), String> {
    // The press lands where the pointer is, so it is taken to where the gesture starts.
    compositor
        .input(InputEvent::PointerMotion {
            x: from.0,
            y: from.1,
        })
        .map_err(|error| error.to_string())?;
    compositor
        .input(InputEvent::PointerButton {
            button,
            pressed: true,
        })
        .map_err(|error| error.to_string())?;
    for step in 1..=steps {
        let at = f64::from(step) / f64::from(steps);
        let (x, y) = drag_step(from, to, at);
        compositor
            .input(InputEvent::PointerMotion { x, y })
            .map_err(|error| error.to_string())?;
    }
    compositor
        .input(InputEvent::PointerButton {
            button,
            pressed: false,
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// The one character a request's `key` names, when it names no named key.
fn typed_char(request: &Json) -> Option<char> {
    let key = request.get("key").and_then(Json::as_str)?;
    if named_key(key).is_some() {
        return None;
    }
    let mut chars = key.chars();
    let first = chars.next()?;
    chars.next().is_none().then_some(first)
}

/// The evdev code a request's `key` means: a named key's, or, for one character, the key this
/// seat's layout puts it on.
fn key_code(request: &Json, compositor: &mut Compositor) -> Option<u32> {
    let key = request.get("key").and_then(Json::as_str)?;
    if let Some(code) = named_key(key) {
        return Some(code);
    }
    let mut chars = key.chars();
    let (first, rest) = (chars.next()?, chars.next());
    if rest.is_some() {
        return None;
    }
    let keysym = xkeysym::Keysym::from_char(first);
    compositor.keycode_for_keysym(keysym.raw())
}

/// The evdev code a named key stands on every keyboard.
fn named_key(name: &str) -> Option<u32> {
    Some(match name {
        "Enter" => 28,
        "Escape" => 1,
        "Tab" => 15,
        "ArrowLeft" => 105,
        "ArrowRight" => 106,
        "ArrowUp" => 103,
        "ArrowDown" => 108,
        "Backspace" => 14,
        "Delete" => 111,
        "Home" => 102,
        "End" => 107,
        "PageUp" => 104,
        "PageDown" => 109,
        "F1" => 59,
        "F2" => 60,
        "F3" => 61,
        "F4" => 62,
        "F5" => 63,
        "F6" => 64,
        "F7" => 65,
        "F8" => 66,
        "F9" => 67,
        "F10" => 68,
        "F11" => 87,
        "F12" => 88,
        "PrintScreen" => 99,
        "Space" => 57,
        "Shift" => 42,
        "Control" => 29,
        "Alt" => 56,
        "Super" => 125,
        "AudioVolumeUp" => 115,
        "AudioVolumeDown" => 114,
        "AudioVolumeMute" => 113,
        "MicrophoneVolumeMute" => 248,
        "BrightnessUp" => 225,
        "BrightnessDown" => 224,
        "MediaPlayPause" => 164,
        "MediaTrackNext" => 163,
        "MediaTrackPrevious" => 165,
        _ => return None,
    })
}

/// Type `text` as the keyboard attached to the machine would: each character by the key this seat's
/// layout puts it on, or, where the layout has no key for it, written as text into the field that
/// takes it.
fn type_text(compositor: &mut Compositor, text: &str) -> Result<(), String> {
    for ch in text.chars() {
        let typed = compositor
            .type_char(ch)
            .map_err(|error| error.to_string())?;
        if !typed {
            let _ = compositor.insert_active_text(&ch.to_string());
        }
    }
    Ok(())
}

/// Write a PNG of every display, each at its place in the layout, as the person in front of them
/// sees it, to `path` .
fn snapshot(compositor: &mut Compositor, path: &str) -> Result<(), (&'static str, String)> {
    if path.is_empty() {
        return Err(("invalid_path", "snapshot path must not be empty".to_owned()));
    }
    let picture = crate::capture::shown(compositor).map_err(|why| ("snapshot_failed", why))?;
    picture
        .save_with_format(path, image::ImageFormat::Png)
        .map_err(|error| ("snapshot_failed", format!("{path}: {error}")))
}

/// Mint a throwaway identity for this session's user, the one a session started after it opens its
/// desk with: what `provision_test_identity` answers on the shell's socket, in the same shape. A
/// real identity is never replaced; a release build mints none.
fn provision_test_identity() -> Json {
    use tilo_identity_host::provisioning::{ProvisionOutcome, THROWAWAY_TEST_IDENTITY_NAME};
    let store = match tilo_onboarding_host::identity_provisioning::DiskIdentityStore::from_env() {
        Ok(store) => store,
        Err(error) => return refusal("provision_failed", &format!("no identity store: {error}")),
    };
    let Some(identity_dir) = store.path().parent() else {
        return refusal("provision_failed", "the identity store has no directory");
    };
    match tilo_onboarding_host::firstboot::ensure_throwaway_test_identity(
        identity_dir,
        &tilo_onboarding_host::firstboot::collaboration_identity_paths(),
    ) {
        Ok(outcome) => json!({"answer": {
            "created": matches!(outcome, ProvisionOutcome::Created { .. }),
            "display_name": THROWAWAY_TEST_IDENTITY_NAME,
            "throwaway": true,
        }}),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            refusal("identity_exists", &error.to_string())
        }
        Err(error) if error.kind() == io::ErrorKind::Unsupported => {
            refusal("dev_image_only", &error.to_string())
        }
        Err(error) => refusal("provision_failed", &error.to_string()),
    }
}

fn refusal(code: &str, message: &str) -> Json {
    refusal_json(code, message)
}

fn refusal_json(code: &str, message: &str) -> Json {
    json!({"err": {"code": code, "message": message}})
}

/// The session's change signal, as its watchers and the held action requests read it: what the desk
/// published, generation by generation, and a condvar that wakes everyone waiting on one.
struct Changes {
    state: Mutex<Changed>,
    settled: Condvar,
    wake: Option<Wake>,
}

/// What [`Changes`] keeps.
struct Changed {
    /// How many state dumps the desk published: a waiter has seen one of these.
    generation: u64,
    /// The latest dump, and the generation it was published at.
    dump: Option<(u64, Json)>,
    /// How many connections watch now: the state is only built while someone reads it.
    watchers: usize,
    /// A watcher asked for the state as it is now, which the next pass publishes.
    refresh: bool,
    /// The session ended: every wait answers.
    ended: bool,
}

impl Changes {
    fn new(wake: Wake) -> Self {
        Self {
            state: Mutex::new(Changed {
                generation: 0,
                dump: None,
                watchers: 0,
                refresh: false,
                ended: false,
            }),
            settled: Condvar::new(),
            wake: Some(wake),
        }
    }

    /// Where the signal stands now, for a waiter that asks before it looks.
    fn generation(&self) -> u64 {
        self.state.lock_recover().generation
    }

    /// A connection started watching: it is sent the state as it is now, and on every change.
    fn attach(&self) {
        {
            let mut state = self.state.lock_recover();
            state.watchers = state.watchers.saturating_add(1);
            state.refresh = true;
        }
        self.request_refresh();
    }

    /// A watcher stopped, or went away.
    fn detach(&self) {
        let mut state = self.state.lock_recover();
        state.watchers = state.watchers.saturating_sub(1);
    }

    /// Wake the loop: the state as it is now is worth a pass.
    fn request_refresh(&self) {
        if let Some(wake) = &self.wake {
            wake();
        }
    }

    /// Publish a pass: `moved` says the desk's state changed, and the dump is only built when a
    /// watcher reads it.
    fn follow(&self, moved: bool, dump: impl FnOnce() -> Json) {
        let mut state = self.state.lock_recover();
        let refresh = std::mem::take(&mut state.refresh);
        if !moved && !refresh {
            return;
        }
        state.generation = state.generation.wrapping_add(1);
        let generation = state.generation;
        if state.watchers > 0 || refresh {
            state.dump = Some((generation, dump()));
        }
        drop(state);
        self.settled.notify_all();
    }

    /// The session ended: nothing follows what was published.
    fn end(&self) {
        self.state.lock_recover().ended = true;
        self.settled.notify_all();
    }

    /// Wake every waiter without publishing: what a waiter's own stop asks.
    fn poke(&self) {
        self.settled.notify_all();
    }

    /// The signal after `seen` , and the state dumped then, or `None` when `stop` was set, the
    /// session ended, or `until` passed.
    fn wait(
        &self,
        seen: u64,
        stop: Option<&AtomicBool>,
        until: Option<Instant>,
    ) -> Option<(u64, Option<Json>)> {
        let mut state = self.state.lock_recover();
        loop {
            if state.ended || stop.is_some_and(|stop| stop.load(Ordering::Relaxed)) {
                return None;
            }
            if state.generation != seen {
                let generation = state.generation;
                let dump = state
                    .dump
                    .as_ref()
                    .filter(|(dumped, _)| *dumped == generation)
                    .map(|(_, dump)| dump.clone());
                return Some((generation, dump));
            }
            state = match until {
                None => self
                    .settled
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner),
                Some(until) => {
                    let left = until.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return None;
                    }
                    let (waited, timeout) = self
                        .settled
                        .wait_timeout(state, left)
                        .unwrap_or_else(PoisonError::into_inner);
                    if timeout.timed_out() && waited.generation == seen {
                        return None;
                    }
                    waited
                }
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::state::owner_shown;
    use super::*;

    #[test]
    fn a_request_before_the_desk_holds_a_workspace_waits_for_the_one_in_front() {
        let front = Arc::new(Front::default());
        let waiting = {
            let front = Arc::clone(&front);
            std::thread::spawn(move || front.known(Duration::from_secs(5)))
        };
        // The loop's pass serves the desk while the request waits: it is answered for the
        // workspace in front, not for none.
        front.served("workspace-1".to_owned());
        assert_eq!(
            waiting.join().expect("the waiting request answers"),
            Some("workspace-1".to_owned())
        );
        assert_eq!(
            front.known(Duration::from_millis(1)),
            Some("workspace-1".to_owned())
        );
    }

    #[test]
    fn a_desk_that_never_holds_a_workspace_names_none() {
        let front = Front::default();
        assert_eq!(front.known(Duration::from_millis(10)), None);
    }

    #[test]
    fn only_a_development_build_opens_the_socket_it_is_named() {
        assert_eq!(
            path_in(true, Some("/run/user/1000/control.sock"), None, None),
            Some(PathBuf::from("/run/user/1000/control.sock"))
        );
        assert_eq!(
            path_in(false, Some("/run/user/1000/control.sock"), None, None),
            None
        );
        assert_eq!(
            OPENED_BY_THIS_BUILD,
            cfg!(any(debug_assertions, feature = "dev-overrides"))
        );
    }

    #[test]
    fn a_socket_nothing_names_opens_where_the_tools_look() {
        let runtime = Some(OsString::from("/run/user/1000"));
        assert_eq!(
            path_in(true, None, None, runtime),
            Some(PathBuf::from(
                "/run/user/1000/tilo-session/test-control.sock"
            ))
        );
        // A tool that names one keeps it, and the environment names the next.
        assert_eq!(
            path_in(
                true,
                Some("/tmp/named.sock"),
                Some(OsString::from("/tmp/env.sock")),
                None
            ),
            Some(PathBuf::from("/tmp/named.sock"))
        );
        assert_eq!(
            path_in(true, None, Some(OsString::from("/tmp/env.sock")), None),
            Some(PathBuf::from("/tmp/env.sock"))
        );
        assert_eq!(path_in(true, None, None, None), None);
        assert_eq!(path_in(true, None, Some(OsString::new()), None), None);
    }

    #[test]
    fn the_owners_door_is_the_seats_own_and_a_headless_session_keeps_its_own() {
        let runtime = Some(OsString::from("/run/user/1000"));
        assert_eq!(
            admin_path_in(true, false, None, runtime.clone()),
            Some(PathBuf::from(
                "/run/user/1000/tilo-session/admin-control.sock"
            ))
        );
        assert_eq!(
            admin_path_in(true, true, None, runtime.clone()),
            Some(PathBuf::from(
                "/run/user/1000/tilo-session/headless-owner-control.sock"
            ))
        );
        // A session in another desktop's window owns no machine, and serves no owner's door.
        assert_eq!(admin_path_in(false, false, None, runtime.clone()), None);
        // A path a developer named is theirs, and tells where it is.
        assert_eq!(
            admin_path_in(
                false,
                false,
                Some(PathBuf::from("/run/somewhere.sock")),
                runtime
            ),
            Some(PathBuf::from("/run/somewhere.sock"))
        );
        assert_eq!(admin_path_in(true, false, None, None), None);
    }

    #[test]
    fn the_owner_door_serves_the_action_model_and_the_state_it_shows() {
        // Every op of the protocol, and the door that serves it: the owner's door is for a person's
        // own scripts, which drive the desktop through the model it publishes and read the state it
        // shows — never through the pointer, the keyboard or a picture of the screen.
        let owner = [
            Op::ActionsCancel,
            Op::ActionsEnumerate,
            Op::ActionsInvoke,
            Op::ActionsStatus,
            Op::ProcessMemory,
            Op::ProfileStart,
            Op::ProfileStatus,
            Op::ProfileStop,
            Op::RecordRead,
            Op::StateDump,
            Op::Watch,
            Op::Windows,
            Op::WorkspaceHosts,
        ];
        for op in Op::ALL {
            let served_there = owner.contains(&op);
            assert!(served(Gate::Dev, op), "a development socket answers {op:?}");
            assert_eq!(
                served(Gate::Owner, op),
                served_there,
                "the owner's door and {op:?}"
            );
            assert_eq!(
                Op::of(&json!({"op": op.name()})),
                Some(op),
                "{op:?} reads back as {}",
                op.name()
            );
        }
        // Every op the protocol answers is in the list, each once.
        let mut names: Vec<&str> = Op::ALL.iter().map(|op| op.name()).collect();
        assert_eq!(names.len(), 31, "the protocol's ops");
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Op::ALL.len(), "{names:?}");
        assert_eq!(Op::of(&json!({"op": "nonsense"})), None);
    }

    #[test]
    fn a_window_row_says_the_window_and_where_its_frame_draws_it() {
        use tilo_desktop_model::{
            DesktopTree, Strip, Tab, TileContent, WindowGroup, Workspace, WorkspaceSlot,
            ids::StripId, width::WindowWidth,
        };
        let tab = |id: u64, title: &str, tile: TileContent| Tab {
            id,
            title: title.to_owned(),
            tile,
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        let mut strip = Strip::new(StripId(1), "Windows");
        strip.window_groups = vec![WindowGroup {
            id: 10,
            tabs: vec![
                tab(
                    100,
                    "Notes",
                    TileContent::Foreign {
                        app_id: "so.tilo.notes".into(),
                        surface_id: 7,
                    },
                ),
                tab(
                    101,
                    "Files",
                    TileContent::LocalApp {
                        app_id: "so.tilo.files".into(),
                        label: "Files".into(),
                        note: String::new(),
                    },
                ),
            ],
            active_tab: 100,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }];
        let mut home = Workspace::new(WorkspaceSlot(1), "Home");
        home.strips = vec![strip];
        let mut tree = DesktopTree {
            active: home.slot,
            workspaces: vec![home],
            ..DesktopTree::default()
        };
        tree.active_strip.insert(WorkspaceSlot(1), StripId(1));
        tree.set_active_group(StripId(1), 10);
        // The frame draws the one window it has mapped; the other tab is an app to open again, with
        // no window of its own yet.
        let frames = |id: u64| if id == 7 { 41 } else { 0 };
        let rows = window_rows(&tree, |id| (id == 7).then_some((14, 96, 946, 984)), frames);
        assert_eq!(rows.len(), 2, "one row per tab: {rows:?}");
        assert_eq!(rows[0]["window"], json!(7));
        assert_eq!(rows[0]["rect"], json!([14, 96, 946, 984]));
        assert_eq!(rows[0]["active"], json!(true));
        assert_eq!(rows[0]["frames"], json!(41));
        assert_eq!(rows[1]["window"], Json::Null);
        assert_eq!(rows[1]["rect"], Json::Null);
        assert_eq!(rows[1]["frames"], json!(0));
        // A frame that no longer maps the window leaves the row without a rectangle rather than one
        // that is no longer where it is drawn.
        let closed = window_rows(&tree, |_| None, frames);
        assert_eq!(closed[0]["window"], json!(7));
        assert_eq!(closed[0]["rect"], Json::Null);
    }

    #[test]
    fn a_locked_desk_shows_the_owner_door_nothing_the_lock_hides() {
        let dump = |locked: bool| {
            json!({"state": {
                "workspace_tree": {"workspaces": [{"name": "Work"}]},
                "held_workspaces": [{"workspace": 1, "room": "room-1"}],
                "chrome": {
                    "session_locked": locked,
                    "in_transition": false,
                    "overlays": {"quick_settings": true, "overview": false},
                },
                "peers": Json::Null,
                "sharing": Json::Null,
            }})
        };
        // Unlocked, the owner's door sees the desk as the development socket does.
        assert_eq!(
            owner_shown(Gate::Owner, dump(false)),
            dump(false),
            "an unlocked desk is shown whole"
        );
        // Locked, it learns that the screen is locked, and nothing of what the lock covers.
        let locked = owner_shown(Gate::Owner, dump(true));
        assert_eq!(locked["state"]["chrome"]["session_locked"], true);
        assert_eq!(locked["state"]["workspace_tree"], Json::Null);
        assert_eq!(locked["state"]["held_workspaces"], json!([]));
        assert_eq!(
            locked["state"]["chrome"]["overlays"],
            json!({"quick_settings": false, "overview": false})
        );
        // A development socket sees the desk it drives, locked or not.
        assert_eq!(owner_shown(Gate::Dev, dump(true)), dump(true));
    }

    #[test]
    fn the_owner_door_is_the_account_that_owns_the_device() {
        assert_eq!(owner_decision(true, Ok(true)), Ok(()));
        assert_eq!(
            owner_decision(true, Ok(false)),
            Err(NotServed::NotDeviceOwner)
        );
        // A member of `wheel` whose device ownership cannot be read is refused, not guessed at.
        assert!(matches!(
            owner_decision(true, Err(io::Error::other("no key"))),
            Err(NotServed::OwnershipUnreadable(_))
        ));
        assert_eq!(
            owner_decision(false, Ok(true)),
            Err(NotServed::NotAdministrator)
        );
    }

    #[test]
    fn a_named_key_and_a_character_are_told_apart() {
        assert_eq!(named_key("Enter"), Some(28));
        assert_eq!(named_key("Super"), Some(125));
        assert_eq!(named_key("enter"), None);
        assert_eq!(named_key("a"), None);
    }

    #[test]
    fn a_tools_address_names_the_action_the_model_routes_to() {
        let open = legacy_target("applications", "so.tilo.notes", "room-1")
            .expect("an app the workspace can open");
        assert_eq!(open.provider, "workspace:room-1");
        assert_eq!(open.address.resource, "app.so.tilo.notes");
        assert_eq!(
            open.address.action,
            tilo_actions::remote::APPLICATION_OPEN_ACTION
        );

        let app = legacy_target(
            "app-actions",
            "application:room-1:so.tilo.notes:abc\u{1f}notes.page:abc\u{1f}new-page",
            "room-1",
        )
        .expect("an app's own action");
        assert_eq!(app.provider, "application:room-1:so.tilo.notes:abc");
        assert_eq!(app.address.resource, "notes.page:abc");
        assert_eq!(app.address.action, "new-page");

        let session = legacy_target("so.tilo.session", "desktop:tab:5\u{1f}close-tab", "room-1")
            .expect("the session's own action");
        assert_eq!(session.provider, "so.tilo.session");
        assert_eq!(session.address.resource, "desktop:tab:5");
        assert_eq!(session.address.action, "close-tab");

        // The workspace verbs by the names tools spell them are the desk's own.
        for (spelled, action) in [
            ("workspace.create", "new-workspace"),
            ("workspace.branch", "branch-workspace"),
        ] {
            let verb = legacy_target("actions", spelled, "room-1").expect("the desk's verb");
            assert_eq!(verb.provider, "so.tilo.session");
            assert_eq!(verb.address.resource, "desktop:chrome");
            assert_eq!(verb.address.action, action);
        }
        assert!(legacy_target("actions", "workspace.nothing", "room-1").is_err());

        assert!(legacy_target("app-actions", "half-an-id", "room-1").is_err());
        assert!(legacy_target("applications", "", "room-1").is_err());
        assert!(legacy_target("so.tilo.session", "no-action", "room-1").is_err());
    }

    #[test]
    fn a_cursor_carries_the_listing_it_was_cut_from() {
        let cursor = encode_cursor("12.34", &[1, 2, 255]);
        assert_eq!(cursor, "12.34~0102ff");
        assert_eq!(
            decode_cursor(&cursor),
            Some(("12.34".to_owned(), vec![1, 2, 255]))
        );
        assert_eq!(decode_cursor("nonsense"), None);
        assert_eq!(decode_cursor("12.34~zz"), None);
    }

    #[test]
    fn the_input_a_tool_injects_is_answered_where_a_device_hands_its_own() {
        let injected: Vec<&str> = Op::ALL
            .iter()
            .filter(|op| op.hands_input())
            .map(|op| op.name())
            .collect();
        assert_eq!(
            injected,
            [
                "click_at",
                "drag",
                "key",
                "key_down",
                "key_up",
                "modifiers",
                "pinch",
                "pointer_button",
                "pointer_move",
                "scroll",
                "swipe",
                "type",
            ],
            "the input a tool injects is served before the pass lays its frame \
             out, so that frame carries it"
        );
        // Everything else reads or changes what a frame already shows, and is answered once the
        // pass has drawn it.
        for op in [
            Op::ClickNamed,
            Op::ActionsInvoke,
            Op::StateDump,
            Op::Snapshot,
            Op::Watch,
            Op::Windows,
        ] {
            assert!(!op.hands_input(), "{} waits for the frame", op.name());
        }
    }

    #[test]
    fn a_watcher_is_sent_the_state_and_nothing_is_built_while_none_is() {
        let changes = Changes::new(Arc::new(|| ()));
        changes.follow(true, || json!({"state": {"unwatched": true}}));
        assert_eq!(changes.generation(), 1, "a change wakes a held request");
        assert!(
            changes.state.lock_recover().dump.is_none(),
            "nobody watched; no state was built"
        );

        changes.attach();
        assert!(
            changes.state.lock_recover().refresh,
            "the state as it is now is asked for"
        );
        changes.follow(false, || json!({"state": {"watched": true}}));
        let (generation, dump) = changes.wait(1, None, None).expect("the published state");
        assert_eq!(generation, 2);
        assert_eq!(dump, Some(json!({"state": {"watched": true}})));

        changes.detach();
        changes.follow(true, || json!({"state": {"unwatched": true}}));
        assert_eq!(changes.generation(), 3);
        assert!(
            changes
                .state
                .lock_recover()
                .dump
                .as_ref()
                .is_some_and(|(dumped, _)| *dumped == 2),
            "the dump a watcher read stays the last one built"
        );
    }

    #[test]
    fn a_wait_answers_when_the_state_changes_or_the_session_ends() {
        let changes = Changes::new(Arc::new(|| ()));
        changes.follow(true, Json::default);
        assert_eq!(changes.generation(), 1);
        assert_eq!(
            changes.wait(1, None, Some(Instant::now())),
            None,
            "a wait past its deadline answers with what it has"
        );

        changes.end();
        assert_eq!(changes.wait(0, None, None), None, "the session ended");
    }

    #[test]
    fn an_action_the_person_may_not_run_is_not_a_refused_socket() {
        assert_eq!(
            action_refusal(&ActionHostError::NotPermitted)["err"]["code"],
            "not_allowed"
        );
        assert_eq!(
            action_refusal(&ActionHostError::StaleRevision)["err"]["code"],
            "stale_cursor"
        );
        assert_eq!(
            action_refusal(&ActionHostError::UnknownAction)["err"]["code"],
            "unknown_action"
        );
        assert_eq!(
            action_refusal(&ActionHostError::InvalidRequest {
                reason: "bad".into()
            })["err"]["code"],
            "invalid_arguments"
        );
    }
}
