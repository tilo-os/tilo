//! The control sockets: `tilo-control` drives a session it was started for, and
//! the device admin drives their own desktop, over one protocol — one JSON request
//! per line, tagged by `op`, and one answer per line: `{"ok": ..}`,
//! `{"answer": ..}`, `{"state": ..}`, `{"targets": ..}` or
//! `{"err": {"code": .., "message": ..}}`.
//! The development test-control socket (`--test-control PATH` or
//! `$TILO_TEST_CONTROL`, development builds only) answers every op below,
//! including injected input, screenshots and fixtures. The owner's own socket,
//! `admin-control.sock`, admits only programs of the account whose desktop this
//! is, in its own namespaces, and serves only the action model, the state it shows
//! and the session's diagnostics ([`Op::owner_serves`]).
//! Each connection is read on a thread of its own; what a request asks of the
//! compositor, the session or the hosted surfaces is answered on the loop's pass, and
//! the loop is woken for it.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value as Json, json};
use tilo_actions::remote::{self, ActionCallError, Caller, RemoteActionFrame, RemoteActionMessage};
use tilo_compositor::{Compositor, InputEvent};

use tilo_session_core::desktop::overlays::Overlays;

use tilo_lock::MutexRecover as _;
use tilo_service::Wake;
use tilo_session_core::desktop::overlays::Part;
use tilo_session_core::desktop::state::DesktopHandle;
use tilo_session_core::surfaces::Surfaces;

use crate::mcp_server::{self, PeerRefusal};
use crate::workspace_view::WorkspaceView;

mod gesture_input;
mod state;
mod wheel_input;

use gesture_input::asked_of_gestures;
use state::{presence_stamp, shown, state_dump, window_rows, within};
use wheel_input::wheel;

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
/// serves no device, so it never takes the device's own `admin-control.sock` , which a login's session
/// binds.
pub const HEADLESS_OWNER_CONTROL_SOCKET_FILE: &str = "headless-owner-control.sock";

/// Where the development socket is opened, given whether this build opens one (`opens`,
/// [`OPENED_BY_THIS_BUILD`]) and the path the command line named: `None` in a build that opens
/// none, whatever was named; the named path, else `$TILO_TEST_CONTROL`'s, else the default
/// `$XDG_RUNTIME_DIR/tilo-session/test-control.sock` the tools that drive a desktop go to —
/// what a dev image's login binds with nothing named, so `tilo-device-step` finds it where it looks.
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

/// Where the owner's control socket opens: the path named on the command line, else this device's
/// `admin-control.sock` in `$XDG_RUNTIME_DIR/tilo-session` for a session that owns the machine,
/// else `headless-owner-control.sock` there for a headless one, whose machine this is but whose
/// seat it is not. `None` for a session that owns no machine and named no path.
///
/// A path the socket is served at is not yet the account's right to be served: the device's own
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

/// Why a session serves no owner's socket.
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

/// Whether this session serves the owner's socket, from the two facts that make its account the
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

/// What the session is, for the answers that read it: the published side every hosted surface's
/// actions go through, the layout, what stands over it, the workspaces this computer holds, whether
/// the person locked the screen, whether the lock screen's own frame is on it, and the agents admitted
/// at the person's API.
#[derive(Clone)]
pub(crate) struct Session<'a> {
    pub(crate) published: &'a Surfaces,
    /// The shell's bar and the parts it shows, when its package started.
    pub(crate) shell: Option<&'a crate::shell::Shell>,
    /// Whether the resources of the apps the session runs itself are kept in the workspace the
    /// person is in, rather than where the session started them.
    pub(crate) apps_kept: bool,
    pub(crate) handle: &'a DesktopHandle,
    pub(crate) overlays: &'a Overlays,
    /// Whether the overview the person sees is up, as the shell's surfaces turn it.
    pub(crate) overview: &'a std::cell::Cell<bool>,
    pub(crate) view: Option<&'a WorkspaceView>,
    /// The agents the person admitted at its socket, as the session's bar shows them.
    pub(crate) agent_sessions: &'a std::sync::Arc<crate::agent_sessions::AgentSessions>,
    /// Who is here, as the peer network says it: this session's membership of each
    /// workspace, when it takes part in one.
    pub(crate) sync: Option<&'a crate::sync::Client>,
    /// The windows this session shares and the ones of other devices it shows: what its
    /// window-share resource reads.
    pub(crate) shares: &'a std::cell::RefCell<crate::window_share::WindowShares>,
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
    /// This device's share of the person's one keyboard and pointer: where the pointer is here, the
    /// device it drives, and what the devices driving this one sent.
    pub(crate) shared_input: Option<&'a crate::shared_input::SharedInput>,
    /// The person's layout, as the shell's views read it: the resource each place shows.
    pub(crate) layout: &'a std::cell::RefCell<tilo_session_core::layout::Layout>,
    /// What the person was told, as the session's own notifications service keeps it: the banners
    /// up and the centre behind them, whichever shell draws them.
    pub(crate) notifications: tilo_session_core::desktop::state::DesktopNotifications,
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
    shared: Arc<Shared>,
    asked: Receiver<Asked>,
    /// The keys a `key_down` holds, so `key_up` releases them and `modifiers` knows what to press
    /// and release to reach the mask it names.
    held: RefCell<BTreeSet<u32>>,
    /// The fingers a `swipe` with `release: false` left on the touchpad, so the next `swipe`
    /// carries the same gesture on and its release lifts them.
    fingers_down: std::cell::Cell<Option<u32>>,
    /// The session state the loop last published, as its stamp.
    published: std::cell::Cell<Option<u64>>,
    changes: Arc<Changes>,
    /// What asked anything but the session's input since the last pass: it is answered after the
    /// frame, where it sees the session the frame shows.
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
enum Audience {
    /// The developer's own socket: whoever the developer started it for.
    Dev,
    /// The owner's socket: this account's own programs, and only theirs.
    Owner,
    /// A program inside an app this session started: it acts in that app's workspace, with the
    /// app's own permissions, never the owner's.
    App,
}

/// The workspace a connection of the app socket acts in, and the app it acts as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Scope {
    app: String,
    workspace: String,
}

/// What a connection is: the socket it came in on, and, at the app socket, the app and workspace it
/// was admitted for.
#[derive(Clone)]
struct Context {
    who: Audience,
    scope: Option<Arc<Scope>>,
}

impl Context {
    /// A connection of the socket `who` serves: no app narrower than it.
    const fn at(who: Audience) -> Self {
        Self { who, scope: None }
    }
}

/// One op of the control protocol, as a request's `op` names it.
///
/// [`Op::ALL`] is every op the protocol answers. The development socket answers all of them;
/// the owner's socket serves the semantic half only ([`Op::owner_serves`], [`served`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
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
    Scroll,
    /// Whether the shell shows `text` now, as assistive technology reads it: a label, a
    /// description or a value of any of its parts on screen.
    Says,
    /// The person's and the device's settings as this session reads them.
    Settings,
    /// Run the shell's own action `action` with `arguments`, as pressing its control does.
    ShellAction,
    /// The shell's own field `field` as the shell draws it now.
    ShellSetting,
    SearchRows,
    Snapshot,
    StateDump,
    Swipe,
    Type,
    Unlock,
    Watch,
    Windows,
    KeepingDevices,
}

impl Op {
    /// Every op the protocol answers, in the order the refusal names them.
    const ALL: [Self; 30] = [
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
        Self::Scroll,
        Self::Says,
        Self::Settings,
        Self::ShellAction,
        Self::ShellSetting,
        Self::SearchRows,
        Self::Snapshot,
        Self::StateDump,
        Self::Swipe,
        Self::Type,
        Self::Unlock,
        Self::Watch,
        Self::Windows,
        Self::KeepingDevices,
    ];

    /// The op `request` names, or `None` for one the protocol does not answer at all.
    fn of(request: &Json) -> Option<Self> {
        Some(match request.get("op").and_then(Json::as_str)? {
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
            "settings" => Self::Settings,
            "shell_action" => Self::ShellAction,
            "shell_setting" => Self::ShellSetting,
            "scroll" => Self::Scroll,
            "says" => Self::Says,
            "search_rows" => Self::SearchRows,
            "snapshot" => Self::Snapshot,
            "state_dump" => Self::StateDump,
            "swipe" => Self::Swipe,
            "type" => Self::Type,
            "unlock" => Self::Unlock,
            "watch" => Self::Watch,
            "windows" => Self::Windows,
            "keeping_devices" => Self::KeepingDevices,
            _ => return None,
        })
    }

    /// The name the protocol writes it as.
    const fn name(self) -> &'static str {
        match self {
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
            Self::Settings => "settings",
            Self::ShellAction => "shell_action",
            Self::ShellSetting => "shell_setting",
            Self::Scroll => "scroll",
            Self::Says => "says",
            Self::SearchRows => "search_rows",
            Self::Snapshot => "snapshot",
            Self::StateDump => "state_dump",
            Self::Swipe => "swipe",
            Self::Type => "type",
            Self::Unlock => "unlock",
            Self::Watch => "watch",
            Self::Windows => "windows",
            Self::KeepingDevices => "keeping_devices",
        }
    }

    /// Whether the owner's socket serves it: the action model and the state it shows, which is what a
    /// person's own scripts drive their desktop through. Synthetic input, screenshots of the
    /// person's screen, the hosted surfaces' controls, the fixtures and the test identity are
    /// nobody's but a development session's.
    const fn owner_serves(self) -> bool {
        matches!(
            self,
            Self::ProcessMemory
                | Self::ProfileStart
                | Self::ProfileStatus
                | Self::ProfileStop
                | Self::Settings
                | Self::StateDump
                | Self::Watch
                | Self::Windows
                | Self::KeepingDevices
        )
    }
}

impl Op {
    /// Whether it hands the session input, which the loop serves where a device's own input reaches
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

/// Whether the socket at `who` answers `op` : the one place the protocol's ops are divided between
/// the APIs.
fn served(who: Audience, op: Op) -> bool {
    match who {
        Audience::Dev => true,
        Audience::Owner | Audience::App => op.owner_serves(),
    }
}

/// The answer an API gives what it does not serve.
fn not_this_api(who: Audience, op: Op) -> Json {
    let _ = who;
    refusal(
        "not_permitted",
        &format!(
            "this control socket serves action frames, the state the session shows and its \
             own diagnostics (`process_memory`, `profile_start`, `profile_status`, \
             `profile_stop`, `state_dump`, `watch` and `windows`), not \
             `{}`; synthetic input and screenshots exist only on a development session's \
             test-control socket",
            op.name()
        ),
    )
}

/// The answer for an `op` the protocol does not answer at all, naming the ones the socket `who`
/// serves.
fn unsupported(ctx: &Context, request: &Json) -> Json {
    let answers = Op::ALL
        .iter()
        .filter(|op| served(ctx.who, **op))
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
pub(crate) struct Shared {
    asks: Mutex<Sender<Asked>>,
    wake: Wake,
    actions: Arc<Actions>,
    changes: Arc<Changes>,
}

impl Control {
    /// The sockets `options` name, opened. The actions and the change signal exist whether or not
    /// one does: the app socket answers the programs it admits through them too.
    pub(crate) fn open(options: &crate::Options, host: Arc<mcp_server::Host>) -> Self {
        let (asks, asked) = channel();
        let changes = Arc::new(Changes::new(options.wake.handle()));
        let actions = Arc::new(Actions {
            person: host.as_ref().clone(),
            host,
            caller: Caller::local_person(options.person.clone()).in_new_execution(),
            front: Front::default(),
            locked: AtomicBool::new(false),
        });
        let shared = Arc::new(Shared {
            asks: Mutex::new(asks),
            wake: options.wake.handle(),
            actions,
            changes: Arc::clone(&changes),
        });
        let mut sockets = Vec::new();
        for (path, who) in [
            (options.test_control.as_deref(), Audience::Dev),
            (options.admin_control.as_deref(), Audience::Owner),
        ] {
            let Some(path) = path else {
                continue;
            };
            match Socket::open(path, who, Arc::clone(&shared)) {
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
            shared,
            asked,
            held: RefCell::new(BTreeSet::new()),
            fingers_down: std::cell::Cell::new(None),
            published: std::cell::Cell::new(None),
            changes,
            waiting: RefCell::new(Vec::new()),
        }
    }

    /// What a program inside an app this session started is served with.
    pub(crate) fn shared(&self) -> Arc<Shared> {
        Arc::clone(&self.shared)
    }

    /// What a tool handed the session's input since the last pass, answered where a device's input
    /// reaches the session: a key or a press a tool injects takes effect in the frame this pass draws,
    /// as the same input from a device does. Everything else asked waits for the frame
    /// ([`Self::serve`]), where it reads the session the frame shows.
    pub(crate) fn serve_input(&self, compositor: &mut Compositor, session: &Session<'_>) {
        self.shared
            .actions
            .set_workspace(session.handle.read(|tree| {
                tree.active_workspace()
                    .map(|workspace| workspace.id().to_string())
            }));
        self.shared.actions.set_locked(session.locked);
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
            if op.hands_input() {
                let _ = answer.send(loop_answer(
                    op,
                    &request,
                    &ctx,
                    compositor,
                    session,
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

    /// Take the connections waiting, publish the session its watchers read, and answer what was asked
    /// of the loop since the last pass.
    pub(crate) fn serve(&self, compositor: &mut Compositor, session: &Session<'_>) {
        self.shared
            .actions
            .set_workspace(session.handle.read(|tree| {
                tree.active_workspace()
                    .map(|workspace| workspace.id().to_string())
            }));
        self.shared.actions.set_locked(session.locked);
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
                session,
                &self.held,
                &self.fingers_down,
            ));
        }
    }

    /// Everything the connections sent the loop since the last look, oldest first. What asks for
    /// the session's input, or starts an action the broker answers, is served in the pass it arrives
    /// in; the rest waits for the frame.
    fn take(&self) {
        while let Ok(asked) = self.asked.try_recv() {
            self.waiting.borrow_mut().push(asked);
        }
    }

    /// Tell the watchers where the session stands, when it moved since the last pass: the state dump
    /// is only built when someone reads it.
    pub(crate) fn publish(&self, session: &Session<'_>) {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::hash::DefaultHasher::new();
        session.handle.navigation_revision().hash(&mut hasher);
        session.locked.hash(&mut hasher);
        for part in Part::ALL {
            session.overlays.is_open(part).hash(&mut hasher);
        }
        session.overview.get().hash(&mut hasher);
        presence_stamp(session, &mut hasher);
        let stamp = hasher.finish();
        let moved = self.published.get() != Some(stamp);
        if moved {
            self.published.set(Some(stamp));
        }
        self.changes.follow(moved, || {
            session.handle.read(|tree| state_dump(tree, session))
        });
    }
}

impl Socket {
    /// Listen at `path` for this account alone; `who` is served there. Connections are
    /// accepted on a thread of the socket's own as they arrive, each read on a thread of its own:
    /// what a program asks of the actions is answered there, never after a pass of the loop, and
    /// what it asks of the loop wakes the loop itself.
    fn open(path: &Path, who: Audience, shared: Arc<Shared>) -> io::Result<Self> {
        let listener = mcp_server::bind_private(path)?;
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let at = path.to_path_buf();
        std::thread::Builder::new()
            .name("tilo-control-accept".into())
            .spawn(move || accept(&listener, &at, who, &shared, &stop))?;
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
    who: Audience,
    shared: &Arc<Shared>,
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
        if who == Audience::Owner
            && let Err(refusal) =
                mcp_server::identify(&stream).and_then(|(_, peer)| mcp_server::peer_decision(&peer))
        {
            tracing::info!(?refusal, path = %path.display(), "control: a connection is refused");
            refuse_owner(stream, &refusal);
            continue;
        }
        let shared = Arc::clone(shared);
        let ctx = Context::at(who);
        let spawned = std::thread::Builder::new()
            .name("tilo-control".into())
            .spawn(move || serve_connection(stream, &ctx, &shared, None));
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

/// Tell a connection of the owner's socket it is not this desktop's owner, in the control protocol's
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
/// socket already read.
pub(crate) fn serve_app(
    stream: UnixStream,
    first: String,
    app: String,
    workspace: String,
    shared: &Shared,
) {
    let ctx = Context {
        who: Audience::App,
        scope: Some(Arc::new(Scope { app, workspace })),
    };
    serve_connection(stream, &ctx, shared, Some(first));
}

/// A connection's writer: shared with the thread that streams the session's state, so a state line and
/// an answer never interleave.
type Writer = Arc<Mutex<UnixStream>>;

/// Read `stream` 's requests one line at a time, answer each, and write its answer, until the
/// client closes it or the session ends. `who` is the socket it came in on: an op that socket does
/// not serve is refused here, before it reaches anything ([`served`]).
fn serve_connection(stream: UnixStream, ctx: &Context, shared: &Shared, first: Option<String>) {
    if let Err(error) = stream.set_nonblocking(false) {
        tracing::warn!(%error, "control: a connection cannot block");
        return;
    }
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let writer: Writer = Arc::new(Mutex::new(writer));
    let mut watching: Option<Watching> = None;
    // The app socket reads a client's first line to know what it asks before it hands the connection
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
            // A frame of the action model's own wire: the one call, as every other caller makes it.
            Ok(request) if request.get("op").is_none() && request.get("version").is_some() => {
                if talk(&writer, &frame_answer(&line, &shared.actions, ctx)).is_err() {
                    break;
                }
                continue;
            }
            Ok(request) => match Op::of(&request) {
                None => unsupported(ctx, &request),
                Some(op) if !served(ctx.who, op) => not_this_api(ctx.who, op),
                Some(op) => match asked_here(op, &request, &writer, &mut watching, ctx, shared) {
                    Some(answer) => answer,
                    // The loop answers every request it takes; a session that ends drops it
                    // unanswered.
                    None => match ask_loop(op, &request, ctx, shared) {
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
        watching.stop(&shared.changes);
    }
}

/// Hand `request` to the loop and wait for its answer; `None` when the session ended.
fn ask_loop(op: Op, request: &Json, ctx: &Context, shared: &Shared) -> Option<Json> {
    let (answer, answered) = channel();
    shared
        .asks
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .send(Asked {
            op,
            request: request.clone(),
            ctx: ctx.clone(),
            answer,
        })
        .ok()?;
    (shared.wake)();
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

/// Stream the session's state to `writer` until it stops watching, off the change signal the loop
/// publishes on: nothing here polls, and a change wakes the wait. What the connection may see of it
/// is what it is sent ([`shown`]).
fn stream_state(writer: &Writer, ctx: &Context, shared: &Shared) -> Watching {
    let changes = Arc::clone(&shared.changes);
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

/// The answer to `watch`, which asks only of the change signal: answered where the connection
/// asked, so the stream holds no frame.
fn asked_here(
    op: Op,
    request: &Json,
    writer: &Writer,
    watching: &mut Option<Watching>,
    ctx: &Context,
    shared: &Shared,
) -> Option<Json> {
    (op == Op::Watch).then(|| watch(request, writer, watching, ctx, shared))
}

/// Start or stop streaming the session's state to a connection (`watch`).
fn watch(
    request: &Json,
    writer: &Writer,
    watching: &mut Option<Watching>,
    ctx: &Context,
    shared: &Shared,
) -> Json {
    let wanted = request
        .get("watching")
        .and_then(Json::as_bool)
        .unwrap_or(true);
    if !wanted {
        if let Some(watching) = watching.take() {
            watching.stop(&shared.changes);
        }
        return json!({"ok": {}});
    }
    if watching.is_none() {
        *watching = Some(stream_state(writer, ctx, shared));
    }
    json!({"ok": {}})
}

/// The answer to what `op` asks of the session, the pointer, the gestures and the keyboard, on the
/// loop's pass.
fn loop_answer(
    op: Op,
    request: &Json,
    ctx: &Context,
    compositor: &mut Compositor,
    session: &Session<'_>,
    held: &RefCell<BTreeSet<u32>>,
    fingers_down: &std::cell::Cell<Option<u32>>,
) -> Json {
    if let Some(answer) = asked_of_shell(ctx, op, request, compositor, session) {
        return answer;
    }
    if op == Op::Scroll {
        return wheel(request, compositor, held);
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

/// The answer to what `op` asks of the session and its hosted surfaces, when it asks of them.
/// The windows a tool reads: where each is drawn, how many frames it drew, and the resource it
/// shows. Nothing shows a window while the screen is locked, to anyone but the developer.
fn windows_answer(ctx: &Context, compositor: &Compositor, session: &Session<'_>) -> Json {
    let rows = if ctx.who != Audience::Dev && session.locked {
        Vec::new()
    } else {
        // Where each window is drawn, read off the frame's own layout: a row says what a tool
        // aiming at that window aims at. How many frames it drew is read off the compositor, where
        // the client's commits arrive. The resource it shows is the one the shell's own views read
        // of its place: what the window's bar carries.
        let rect_of = |id: u64| compositor.window_rect(id);
        let frames_of = |id: u64| compositor.window_frames(id);
        let places = session.layout.borrow().reading().places;
        let resource_of = |tab: u64| {
            places
                .iter()
                .find(|place| place.key == tab.to_string())
                .map(|place| place.resource.clone())
                .filter(|resource| !resource.is_empty())
        };
        // The item a window was opened on, which a restart opens it on again: the document of
        // the app's library the window shows.
        let items = session.handle.window_items();
        let item_of = |tab: u64| items.get(&tab).cloned();
        let shown = (rect_of, frames_of, resource_of, item_of);
        match &ctx.scope {
            None => session.handle.read(|tree| window_rows(tree, shown)),
            Some(scope) => session
                .handle
                .read(|tree| window_rows(&within(tree, scope), shown)),
        }
    };
    json!({"answer": {"windows": rows}})
}

fn asked_of_shell(
    ctx: &Context,
    op: Op,
    request: &Json,
    compositor: &mut Compositor,
    session: &Session<'_>,
) -> Option<Json> {
    Some(match op {
        // No picture leaves a locked session, to anyone: a socket client that reads the screen
        // could otherwise watch whatever the lock hides the moment it lifts or flickers, and a
        // caller that cannot see the display learns it is locked from the refusal.
        Op::Snapshot if session.locked => refusal(
            "session_locked",
            "the session is locked; unlock it at the device, then take the picture again",
        ),
        Op::Snapshot => {
            let path = request
                .get("path")
                .and_then(Json::as_str)
                .unwrap_or_default();
            match snapshot(compositor, path) {
                Ok(()) => json!({"ok": {"frame": 0}}),
                Err((code, message)) => refusal(code, &message),
            }
        }
        // The session's lock comes down as it does for a password that unlocks: the screen
        // dissolves, and the compositor lifts the lock once it has.
        Op::Unlock => {
            let was_locked = session.locked;
            if was_locked {
                session.lock.unlocked();
            }
            json!({"ok": {"locked": false, "was_locked": was_locked}})
        }
        Op::ProvisionTestIdentity => provision_test_identity(),
        Op::Says => {
            let Some(text) = request.get("text").and_then(Json::as_str) else {
                return Some(refusal("parse_error", "`says` takes `text`"));
            };
            let says = session.shell.is_some_and(|shell| shell.says(text));
            json!({"answer": {"says": says}})
        }
        Op::ListTargets => json!({"targets": listed_targets(session)}),
        Op::ClickNamed => return Some(clicked_named(request, session)),
        Op::Type if request.get("clipboard").and_then(Json::as_bool) == Some(true) => {
            let Some(text) = request.get("text").and_then(Json::as_str) else {
                return Some(refusal("parse_error", "`type` takes `text`"));
            };
            // Set an ephemeral offer so the harness can exercise the actual
            // Paste shortcut without leaving a copy in clipboard history.
            if session.clipboard.copy_for_person(
                compositor,
                tilo_compositor::clipboard_history::TEXT_MIME,
                text.as_bytes().to_vec(),
                Some(Duration::from_secs(5)),
            ) {
                json!({"ok": {}})
            } else {
                refusal("input_failed", "could not set the clipboard")
            }
        }
        Op::SearchRows => {
            let (query, rows) = session.handle.search_rows();
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
                "searching": session.handle.search_searching(),
            }})
        }
        Op::ProcessMemory => json!({"answer": tilo_compositor::memory_report::report()}),
        Op::ProfileStart => profile_start(request),
        Op::ProfileStatus => json!({"answer": crate::diagnostics::reading()}),
        Op::ProfileStop => json!({"answer": {"profile": tilo_frame_metrics::profile::stop()}}),
        Op::StateDump => shown(ctx, session.handle.read(|tree| state_dump(tree, session))),
        Op::Settings => json!({"answer": settings_store::load()}),
        Op::ShellAction => {
            let Some(action) = request.get("action").and_then(Json::as_str) else {
                return Some(refusal("parse_error", "`shell_action` takes `action`"));
            };
            let arguments = request
                .get("arguments")
                .and_then(Json::as_array)
                .cloned()
                .unwrap_or_default();
            match session
                .shell
                .map(|shell| shell.run_action(action, arguments))
            {
                Some(Ok(())) => json!({"ok": {}}),
                Some(Err(why)) => return Some(refusal("refused", &why)),
                None => return Some(refusal("unavailable", "this session shows no shell")),
            }
        }
        Op::ShellSetting => {
            let Some(field) = request.get("field").and_then(Json::as_str) else {
                return Some(refusal("parse_error", "`shell_setting` takes `field`"));
            };
            json!({"answer": session.shell.and_then(|shell| shell.record_setting(field))})
        }
        Op::KeepingDevices => session
            .handle
            .read(|tree| state::keeping_devices(tree, session, ctx)),
        Op::Windows => windows_answer(ctx, compositor, session),
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
    /// The broker, with the session's own APIs beside it ([`crate::mcp_server::Host`]).
    host: Arc<mcp_server::Host>,
    /// The person's own caller, made once and cloned: the broker remembers an invocation under the
    /// caller that admitted it, so a status or a cancel reads it back as the same caller.
    caller: Caller,
    /// The workspace in front, as the loop last served it: what an invocation opens into.
    front: Front,
    /// What the person at this device invokes through: the device's dispatch, which decides a
    /// call as the person the session's link was made for. What a frame on the owner's or the
    /// development socket is answered by.
    person: mcp_server::Host,
    /// Whether the screen is locked, as the loop last read it: the owner's socket invokes nothing
    /// there.
    locked: AtomicBool,
}

/// How long a host may take to answer a request at all.
const HOST_WAIT: Duration = Duration::from_secs(30);

/// The workspace in front, as the loop last served the session, and the wait for a request that
/// arrives before the session holds one.
///
/// A session adopts the person's workspaces in its first passes, so a session holds none for the
/// moment between its first frame and them: what a request asks in that moment is where the person
/// is, which is not yet known, and waiting for it is the answer — the person's own authority over
/// their own session is not in question, so a refusal would be a lie.
#[derive(Default)]
struct Front {
    workspace: Mutex<String>,
    /// The loop served a workspace: a request waiting for one looks again.
    arrived: Condvar,
}

impl Front {
    /// The loop served the session: `workspace` is the one in front now.
    fn served(&self, workspace: String) {
        let mut held = self.workspace.lock_recover();
        if *held != workspace {
            *held = workspace;
            drop(held);
            self.arrived.notify_all();
        }
    }

    /// The workspace in front, once the session holds one, waiting up to `wait` for a session that
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

    /// Whose workspace is in front, as the loop read it: a session that holds none names none.
    fn set_workspace(&self, workspace: Option<String>) {
        if let Some(workspace) = workspace {
            self.front.served(workspace);
        }
    }

    /// Whether the screen is locked, as the loop last read it, and where it stands now.
    fn set_locked(&self, locked: bool) {
        self.locked.store(locked, Ordering::Relaxed);
    }

    fn locked(&self) -> bool {
        self.locked.load(Ordering::Relaxed)
    }
}

/// The answer to one [`RemoteActionFrame`] line: the frame [`remote::serve`] answers it with, as
/// the person at this device (or, at the app socket, as the app), in the workspace in front unless
/// the connection was admitted to another. A caller names no workspace: the session decides where
/// the person is, waiting for one when it has none yet ([`Front::known`]). The owner's socket
/// invokes nothing while the screen is locked: what a person's own scripts run is what the person
/// at the device could run.
fn frame_answer(line: &str, actions: &Actions, ctx: &Context) -> String {
    let message = match RemoteActionFrame::decode(line.as_bytes()) {
        Ok(frame) => frame.message,
        Err(error) => return refusal("parse_error", &error.to_string()).to_string(),
    };
    let locked = ctx.who != Audience::Dev
        && actions.locked()
        && matches!(message, RemoteActionMessage::Invoke(_));
    if locked {
        let error = ActionCallError::Unavailable {
            reason: "the session is locked; unlock it at the device, then invoke again".into(),
        };
        return remote::refuse(&message, error).map_or_else(
            || refusal("parse_error", "a frame sent here asks something").to_string(),
            remote::frame_line,
        );
    }
    let caller = actions.caller(ctx);
    let provider = if ctx.scope.is_some() {
        actions.host.as_ref()
    } else {
        &actions.person
    };
    remote::answer_message(provider, &caller, message, HOST_WAIT)
}

/// The controls the session shows now, as the tools read them: each one's name, what it does where it
/// does something the app publishes, and where it stands.
fn listed_targets(session: &Session<'_>) -> Vec<Json> {
    session
        .published
        .controls()
        .into_iter()
        .map(|control| {
            let (x, y, w, h) = control.rect;
            let level = control.level;
            let selected = control.selected;
            match control.identity {
                Some(identity) => json!({
                    "name": control.name,
                    "identity": identity,
                    "rect": [x, y, w, h],
                    "level": level,
                    "selected": selected,
                }),
                None => json!({
                    "name": control.name,
                    "rect": [x, y, w, h],
                    "level": level,
                    "selected": selected,
                }),
            }
        })
        .collect()
}

/// The answer to `click_named`: the control the caller names, by its name or by what it does, is
/// pressed as assistive technology presses it — or the session says it shows none.
fn clicked_named(request: &Json, session: &Session<'_>) -> Json {
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
        session.published.press_identity(identity, action)
    } else {
        session.published.press_action(name, action)
    };
    match pressed {
        Ok(()) => json!({"ok": {}}),
        Err(error) => refusal("unknown_target", &error),
    }
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

/// Give this session's user their identity, as a session started after it opens its session
/// with: what `provision_test_identity` answers on the shell's socket, in the same shape.
///
/// The account's person is made the one way a person is made, this computer's daemon founding
/// them ([`crate::startup::found_and_certify`]), and its key certified with the root the daemon
/// keeps for the account. A real identity is never replaced; a release build mints none. A
/// session is served by a daemon: one that no daemon answers is refused rather than given an
/// identity of its own.
fn provision_test_identity() -> Json {
    let identity_path = match tilo_identity::store::active_identity_path() {
        Ok(path) => path,
        Err(error) => return refusal("provision_failed", &format!("no identity store: {error}")),
    };
    let Some(identity_dir) = identity_path.parent() else {
        return refusal("provision_failed", "the identity store has no directory");
    };
    if !tilo_onboarding_session::identity_provisioning::needs_first_run_flow(&identity_path) {
        // This session made its identity as it started, the one way a person is made: there is
        // none left to make here.
        return json!({"answer": {
            "created": false,
            "display_name": tilo_identity::provisioning::login_account_display_name(),
            "throwaway": true,
        }});
    }
    if tilo_api::daemon::who::who().is_err() {
        return refusal(
            "no_daemon",
            "this computer's daemon is not answering: an identity is made through it",
        );
    }
    match crate::startup::found_and_certify(identity_dir) {
        Ok(true) => json!({"answer": {
            "created": true,
            "display_name": tilo_identity::provisioning::login_account_display_name(),
            "throwaway": true,
        }}),
        Ok(false) => refusal(
            "not_signed_in",
            "this account is not signed in on this computer, so no person can be made for it",
        ),
        Err(error) => refusal("provision_failed", &error),
    }
}

fn refusal(code: &str, message: &str) -> Json {
    refusal_json(code, message)
}

fn refusal_json(code: &str, message: &str) -> Json {
    json!({"err": {"code": code, "message": message}})
}

/// The session's change signal, as its watchers and the held action requests read it: what the session
/// published, generation by generation, and a condvar that wakes everyone waiting on one.
struct Changes {
    state: Mutex<Changed>,
    settled: Condvar,
    wake: Option<Wake>,
}

/// What [`Changes`] keeps.
struct Changed {
    /// How many state dumps the session published: a waiter has seen one of these.
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

    /// Publish a pass: `moved` says the session's state changed, and the dump is only built when a
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
#[path = "test_control/tests.rs"]
mod tests;
