//! Tabs of one app, opened and closed by the app itself. Applications have no tab strips: a second
//! page, file or conversation is a second window of the app in a tab of the desktop's window group.
//! An app asks for one over its authenticated `shell-ui` connection ([`ui::shell_tabs`]): another
//! window after its own tab, its own tab closed, the last closed tab back, its group stepped on, or
//! its next window beside its own. The connection vouches for the app and its workspace, so an app
//! can never place, close or step another app's tab, and a refused request changes nothing. The
//! asks become shell commands on the session loop's own pass: the shell is never read from another
//! thread. An app also says here that its windows show a secret right now, which marks it private
//! for the compositor that every capture asks, and every connection receives the appearance of its
//! window's workspace.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, Write};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::platform::surfaces::ShellDesktop;
use launcher::services::{SHELL_UI_LAUNCH_PROTOCOL, SHELL_UI_SERVICE};
use serde::Deserialize;
use tilo_app_launch::{Connects, Launch, Open};
use tilo_compositor::{Compositor, ReadableWatch};

use tilo_desktop_model::apps::app_ids_match;
use tilo_session_core::desktop::launchpad::LaunchpadAction;
use ui::Theme;
use ui::WindowAppearance;
use ui::floating_surface::{
    FLOATING_SURFACE_PROTOCOL_VERSION, FloatingSurfaceClientMessage, FloatingSurfaceHostMessage,
    FloatingSurfaceProtocolRole, HostNetworkStatus, Recipient, protocol_rect,
};
use ui::theme::ThemeDocument;
use ui::workspace_sync::WorkspaceSync;

use crate::Launches;
use crate::apps::Apps;
use crate::assembly::BoxAsk;
use crate::capture_studio::CaptureStudio;
use crate::desktop_services::DesktopServices;
use crate::host_shell::HostShellAsks;
use crate::page_track::{PageTracks, Watch};
use ui::vello::kurbo::Rect;

/// How far a picture an app floats stands from the lower right corner of the window it floats
/// over, and of the display when that window is not on the shell.
const PICTURE_MARGIN: f64 = 16.0;

/// A window the compositor lists, and the rect it draws in, in the layout's coordinates.
type ListedWindow = (tilo_compositor::WindowInfo, (i32, i32, i32, i32));

/// `at` moved no farther than it must to stand on the outputs the person has: a floating picture
/// is clamped to what they can see.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "a place moved by whole logical pixels on a display"
)]
fn on_outputs(at: Rect, outputs: Vec<tilo_compositor::OutputInfo>) -> Rect {
    let mut area: Option<Rect> = None;
    for output in outputs {
        let (true, Some((x, y, width, height))) = (output.enabled, output.rect) else {
            continue;
        };
        let rect = Rect::from_origin_size(
            (f64::from(x), f64::from(y)),
            (f64::from(width), f64::from(height)),
        );
        area = Some(area.map_or(rect, |area| area.union(rect)));
    }
    let Some(area) = area else {
        return at;
    };
    let x = if at.x0 < area.x0 {
        area.x0 - at.x0
    } else if at.x1 > area.x1 {
        area.x1 - at.x1
    } else {
        0.0
    };
    let y = if at.y0 < area.y0 {
        area.y0 - at.y0
    } else if at.y1 > area.y1 {
        area.y1 - at.y1
    } else {
        0.0
    };
    at + ui::vello::kurbo::Vec2::new(x, y)
}

/// The longest frame an app may send: a paint payload in parts, a marker inventory and a whole
/// accessibility tree all fit, as the toolkit's own frame bound says.
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// The longest address an app may ask to open in a new tab: as long as the shell's own link opening
/// takes.
const MAX_TARGET_BYTES: usize = 8 * 1024;

/// How the session makes a window's own connections to its services: the collaboration broker and
/// the shell-ui socket, each of the window's own, so what the window asks is answered for it alone.
pub struct ServiceLinks {
    launches: Arc<Launches>,
}

impl ServiceLinks {
    /// The connections of a window of the apps whose launches `launches` records.
    #[must_use]
    pub fn new(launches: Arc<Launches>) -> Self {
        Self { launches }
    }

    /// The window's collaboration connection, attested as that window: the record is the session's
    /// own, spent by the connection that names the window's instance.
    fn window_collaboration(
        &self,
        app: &str,
        workspace: &str,
        instance: u64,
        launch: Option<u32>,
    ) -> Option<OwnedFd> {
        let mut stream = tilo_files::native::socket::connect(&launcher::services::collab_socket())
            .inspect_err(|error| {
                tracing::warn!(%error, %app, "a window could not reach the collaboration broker");
            })
            .ok()?;
        self.launches.window(app, workspace, instance, launch);
        if let Err(error) =
            launcher::services::write_collab_window_handoff(&mut stream, app, workspace, instance)
        {
            self.launches.window_withdrawn(app, workspace, instance);
            tracing::warn!(%error, %app, "a window's collaboration connection was not named");
            return None;
        }
        Some(OwnedFd::from(stream))
    }

    /// The window's shell-ui connection, attributed to the launch whose process `launcher` is: the
    /// app and the workspace it runs in, without which what it asks reaches no shell. `instance` is
    /// the tab the window takes, which is how its asks find it.
    fn window_shell_ui(&self, app: &str, launcher: Option<u32>, instance: u64) -> Option<OwnedFd> {
        let launcher = launcher?;
        self.launches.window_shell_ui(app, launcher, instance);
        let connected = (|| {
            let mut stream = tilo_files::native::socket::connect(&launcher::services::socket_for(
                SHELL_UI_SERVICE,
            ))?;
            // What the app may know of the network was decided for its launch; the window does not
            // widen it.
            stream.write_all(&launcher::services::shell_ui_handshake(app, false))?;
            stream.flush()?;
            Ok::<_, std::io::Error>(stream)
        })();
        match connected {
            Ok(stream) => Some(OwnedFd::from(stream)),
            Err(error) => {
                self.launches.take_window_shell_ui(app);
                tracing::warn!(%error, %app, "a window could not reach the shell-ui service");
                None
            }
        }
    }
}

impl Connects for ServiceLinks {
    fn collaboration(
        &self,
        app: &str,
        workspace: &str,
        instance: u64,
        launcher_pid: Option<u32>,
    ) -> Option<OwnedFd> {
        self.window_collaboration(app, workspace, instance, launcher_pid)
    }

    fn shell_ui(&self, app: &str, instance: u64, launcher_pid: Option<u32>) -> Option<OwnedFd> {
        self.window_shell_ui(app, launcher_pid, instance)
    }

    /// The collaboration connection of a call of `app` that shows nothing: recorded as that call,
    /// which the broker admits and hands no view to open.
    fn collaboration_with_no_view(&self, app: &str, workspace: &str, call: u64) -> Option<OwnedFd> {
        let mut stream = tilo_files::native::socket::connect(&launcher::services::collab_socket())
            .inspect_err(|error| {
                tracing::warn!(%error, %app, "a call that shows nothing could not reach its workspace");
            })
            .ok()?;
        self.launches.record_no_view(app, workspace, call);
        if let Err(error) =
            launcher::services::write_collab_window_handoff(&mut stream, app, workspace, call)
        {
            self.launches.window_withdrawn(app, workspace, call);
            tracing::warn!(%error, %app, "a call that shows nothing was not named to its workspace");
            return None;
        }
        Some(OwnedFd::from(stream))
    }
}

/// The shell-ui socket this session serves: what an app asks about its own tabs arrives here, and
/// the session's loop answers it.
pub struct AppTabs {
    requests: mpsc::Receiver<FromConnection>,
    /// The connections that asked for their app's windows to be withheld from captures, and the app
    /// each speaks for: the mark lasts while one of them is open.
    withheld: BTreeMap<u64, String>,
    /// The open connections, by the app each vouches for and what it was last told of its window's
    /// appearance.
    links: BTreeMap<u64, Link>,
    /// The cards the apps draw themselves, and where the layer gives them.
    cards: crate::cards::Cards,
    /// The apps this session started, whose launches name each app window: what tells a window of
    /// an app from another window of the same app.
    launches: Arc<Launches>,
    /// The questions the person is answering for Terminal's own shell.
    host_shell: HostShellAsks,
    /// The listener the apps connect to, taken off on the session's own pass,
    /// and the socket's path, which goes when the session ends.
    socket: Socket,
    /// A report's box waiting for the recording it carries to be written.
    report: Option<Preparing>,
}

/// How long a report's box waits for its recording before it opens without one: writing out
/// what the capture kept takes a moment, and the person waits on none past this.
const CLIP_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// A report's box, waiting for what it carries by default: the picture of the window it is about,
/// and the last moments the system's capture kept while that is written out.
struct Preparing {
    to: Vec<Recipient>,
    /// The connection of the window the report is about, settled when it was asked: the picture
    /// and the box are that window's, whatever comes in front while the recording is written.
    about: Option<u64>,
    carried: Vec<tilo_reference::Attached>,
    clip: Option<(PathBuf, mpsc::Receiver<Result<usize, String>>)>,
    since: std::time::Instant,
}

impl Preparing {
    /// What the box carries, once the recording is written or will not be: `Err` with itself
    /// while it is still being written.
    fn ready(mut self, carried: &crate::carried::Carried) -> Result<Self, Self> {
        let Some((path, written)) = self.clip.take() else {
            return Ok(self);
        };
        match written.try_recv() {
            Ok(Ok(_)) => {
                let name = format!("{}.mp4", crate::i18n::t("session-report-recording"));
                match carried.offer(&path, &name, "video/mp4", (0, 0)) {
                    Ok(file) => self.carried.insert(0, file),
                    Err(error) => tracing::warn!(%error, "a report's recording was not carried"),
                }
                Ok(self)
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "a report's recording was not written");
                Ok(self)
            }
            Err(mpsc::TryRecvError::Disconnected) => Ok(self),
            Err(mpsc::TryRecvError::Empty) if self.since.elapsed() >= CLIP_WAIT => {
                tracing::warn!("a report's recording took too long: the box opens without it");
                Ok(self)
            }
            Err(mpsc::TryRecvError::Empty) => {
                self.clip = Some((path, written));
                Err(self)
            }
        }
    }
}

/// An open shell-ui connection: the app it vouches for, the launch whose
/// window it is, whether that launch let the app know the network's state,
/// and what its window was last told.
struct Link {
    app: String,
    /// The launch the window that speaks on this connection belongs to: what tells it apart from
    /// another window of the same app, and so whether the very window that draws an activity is on
    /// the shell.
    launch: u32,
    /// The tab the window takes, when the session connected it itself: what its asks about its own
    /// window are answered for. A window the launcher connected names its tab itself, in each ask.
    instance: Option<u64>,
    /// The window that speaks on a connection its launcher made, as its own asks name it (their
    /// `instance`): such a connection names no tab itself, and an app has several windows.
    named: Option<u64>,
    /// Whether the window on this connection said it hands its page over
    /// ([`FloatingSurfaceClientMessage::PaintCapable`]). The offer names no window, so it is taken
    /// up for the window the connection speaks for, once that is known ([`Answers::window_of`]).
    offers: bool,
    /// The tab whose page this connection's window offered: where the window is asked for that
    /// page, and the only page the frames it sends on this connection are published as.
    page: Option<u64>,
    /// A payload of that page that arrives in parts: how many bytes it has, and the ones that
    /// came ([`ui::floating_surface::PaintPart`]).
    assembling: Option<(usize, Vec<u8>)>,
    /// Whether this app's launch let it know the network's state; a window never widens that for
    /// itself.
    network_allowed: bool,
    /// Whether this window's app said it opens a send-to box
    /// ([`FloatingSurfaceClientMessage::SelectionCapable`]): only a window that answered is asked
    /// to open one.
    selection: bool,
    /// Writing to the connection.
    writer: Arc<Mutex<UnixStream>>,
    /// The appearance the window was last told, so a pass that changes nothing sends nothing.
    told: Option<Delivered>,
}

/// What an appearance message carried, to tell whether a window needs another one.
#[derive(PartialEq)]
struct Delivered {
    window: WindowAppearance,
    theme: ThemeDocument,
    theme_bundle: Option<String>,
    network: Option<HostNetworkStatus>,
    sync: Option<WorkspaceSync>,
}

/// What a connection said that this service answers.
enum Ask {
    /// What the app asks of its own tabs.
    Message(Box<FloatingSurfaceClientMessage>),
    /// The window joined: its side of the connection is open, and it is told its window's
    /// appearance from the next pass on.
    Connected { network_allowed: bool },
    /// The connection ended: what it asked about captures goes with it, and its window is told no
    /// more.
    Closed,
}

/// One ask an app made over its own authenticated connection.
struct Request {
    /// The app the launcher vouched for.
    app: String,
    /// The workspace its launch runs it in.
    workspace: String,
    /// The launch the window that speaks on this connection belongs to.
    launch: u32,
    /// The tab the window takes, when the session connected it itself.
    instance: Option<u64>,
    /// The connection the ask came in on, which its answer goes back on and which its capture
    /// protection lasts as long as.
    connection: u64,
    /// Writing to that connection.
    reply: Arc<Mutex<UnixStream>>,
    ask: Ask,
}

/// What the thread serving a connection tells the session's loop, which is woken for either, so a
/// connection the session took off its socket never leaves the loop's wait running.
enum FromConnection {
    /// An ask of a window this session started, answered on the loop.
    Ask(Box<Request>),
    /// The connection ended without naming a window of an app this session started, so nothing was
    /// answered on it. The loop is told it was taken and is over all the same: the connection was
    /// the session's from the moment it came off its socket.
    Unadmitted { connection: u64 },
}

impl AppTabs {
    /// Serve the shell-ui socket, waking the session's loop with each ask.
    ///
    /// # Errors
    /// The service's directory cannot be made, or its socket cannot be bound
    /// or served.
    pub(crate) fn start(
        launches: Arc<Launches>,
        wake: tilo_service::Wake,
        cards: crate::cards::Cards,
    ) -> Result<Self, String> {
        let root = launcher::services::ensure_services_root()
            .map_err(|error| format!("create the shell-ui service directory: {error}"))?;
        let path = root.join(format!("{SHELL_UI_SERVICE}.sock"));
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|error| format!("remove the stale shell-ui socket: {error}"))?;
        }
        let listener = tilo_files::native::socket::bind(&path)
            .map_err(|error| format!("bind the shell-ui socket {}: {error}", path.display()))?;
        let (requests, answers) = mpsc::channel();
        let socket = Socket::serve(listener, path, Arc::clone(&launches), requests, wake)?;
        Ok(Self {
            requests: answers,
            withheld: BTreeMap::new(),
            links: BTreeMap::new(),
            cards,
            launches,
            host_shell: HostShellAsks::default(),
            socket,
            report: None,
        })
    }

    /// Write what the person just pressed in a window's bar or content to the app that declared
    /// it, before the loop serves anything else: a press a control socket made is in its app
    /// ahead of the input the person makes after it, so a bar's New event whose field the typing
    /// goes into has run before the first key is served, never a frame behind it.
    #[cfg(target_os = "linux")]
    pub(crate) fn deliver_presses_now(&self, desktop: &ShellDesktop, compositor: &Compositor) {
        write_bar_events(desktop, compositor, &self.links);
        write_target_presses(desktop, compositor, &self.links);
    }

    /// Write what a client of the session's AT-SPI export asked of an app window's node to the
    /// window that drew the tree, which answers as its own assistive client's action. A window
    /// whose connection is gone is asked nothing.
    pub(crate) fn deliver_accessibility_now(
        &self,
        desktop: &ShellDesktop,
        compositor: &Compositor,
        windows: &Rc<RefCell<crate::a11y::WindowExports>>,
    ) {
        let actions = windows.borrow_mut().take_actions();
        for action in actions {
            // The connection the window sent its tree on, which is the window's own even when
            // its process hosts several windows.
            let Some(connection) = windows.borrow().connection_of(action.instance) else {
                continue;
            };
            let Some(link) = self.links.get(&connection) else {
                continue;
            };
            tell(
                &link.writer,
                &FloatingSurfaceHostMessage::AccessibilityAction {
                    request: action.request,
                    // In its place among the window's own input: what the person typed before
                    // the action is read before it.
                    after_input: input_before(desktop, compositor, action.instance),
                },
                &link.app,
            );
        }
    }

    /// End the loop's wait when an app connects, so its window's first ask is
    /// answered on the pass that follows.
    ///
    /// # Errors
    /// The loop took no more sources.
    pub(crate) fn wake_on(&mut self, compositor: &mut Compositor) -> std::io::Result<()> {
        self.socket.watch = Some(crate::sockets::watch(compositor, &self.socket.listener)?);
        Ok(())
    }

    /// The app's end of a connection for a window of `app` in `workspace` that the launcher made
    /// before the app started, as every app's first window is connected: the process at the other
    /// end is the app's own, so the connection names no tab, and what the window asks about itself
    /// is answered for the app's window on the shell. It is served as every admitted connection is;
    /// only the kernel's word for who is at the other end, which a test's own process cannot stand
    /// in for, is left out.
    ///
    /// # Errors
    /// The pair of sockets cannot be made, or no thread serves the connection.
    #[cfg(test)]
    pub(crate) fn launched_window(
        &self,
        app: &str,
        workspace: &str,
    ) -> std::io::Result<UnixStream> {
        self.window_connected(app, workspace, None)
    }

    /// The app's end of a connection for one more window of `app` in `workspace`, as the session
    /// makes it for a window it opens in the app's running process: the connection is the
    /// session's own, so it names the tab the window takes, `instance`.
    ///
    /// # Errors
    /// As [`Self::launched_window`].
    #[cfg(test)]
    pub(crate) fn opened_window(
        &self,
        app: &str,
        workspace: &str,
        instance: u64,
    ) -> std::io::Result<UnixStream> {
        self.window_connected(app, workspace, Some(instance))
    }

    #[cfg(test)]
    fn window_connected(
        &self,
        app: &str,
        workspace: &str,
        instance: Option<u64>,
    ) -> std::io::Result<UnixStream> {
        let (theirs, ours) = UnixStream::pair()?;
        let writer = ours.try_clone()?;
        let connection = self.socket.next.fetch_add(1, Ordering::Relaxed);
        let window = Window {
            connection,
            app: app.to_owned(),
            workspace: workspace.to_owned(),
            launch: std::process::id(),
            instance,
            network_allowed: false,
        };
        let (requests, wake) = (self.socket.requests.clone(), Arc::clone(&self.socket.wake));
        std::thread::Builder::new()
            .name(format!("tilo-shell-ui-{connection}"))
            .spawn(move || serve_window(window, BufReader::new(ours), writer, &requests, &wake))?;
        Ok(theirs)
    }

    /// Answer what the apps asked since the last pass: each ask is a shell command, so the shell is
    /// read and changed on this thread alone.
    pub(crate) fn follow(
        &mut self,
        shared: &crate::Shared,
        desktop: &ShellDesktop,
        apps: Option<&Apps>,
        compositor: &mut Compositor,
        services: Option<&DesktopServices>,
        studio: Option<&CaptureStudio>,
    ) {
        // The windows that connected since the last pass join here, so an ask they write is
        // answered on this pass and no thread of the session's waits on a client.
        self.socket.take();
        let mut redials = Vec::new();
        {
            let mut windows = shared.windows.borrow_mut();
            let mut answered = Answers {
                desktop,
                apps,
                compositor,
                services,
                studio,
                withheld: &mut self.withheld,
                links: &mut self.links,
                cards: &self.cards,
                launches: &self.launches,
                pages: &shared.pages,
                windows: &mut windows,
                redials: &mut redials,
            };
            for arrived in self.requests.try_iter() {
                match arrived {
                    FromConnection::Ask(request) => {
                        answered.answer(&mut self.host_shell, *request);
                    }
                    // A connection this session took ended without a window on it: nothing of it
                    // is left to answer, and the pass that hears it is the one it woke.
                    FromConnection::Unadmitted { connection } => {
                        tracing::debug!(connection, "a shell-ui connection ended unadmitted");
                    }
                }
            }
            // What the person did in the windows' bars goes back to the app whose connection
            // declared the parts there.
            answered.deliver_bar_events();
            // A control of a window's own content, pressed where the shell lists it, goes to the app
            // drawing that window.
            answered.deliver_target_presses();
        }
        // A window whose workspace could not arrive asked to dial it again: its workspace's holders
        // are dialled now, and the window hears how it went in its next appearance.
        if let Some(sync) = &shared.sync {
            for (app, instance) in redials {
                let ws = shared.handle.read(|tree| {
                    crate::window_warmth::window_workspace(tree, &app, instance)
                        .map(tilo_desktop_model::Workspace::id)
                });
                if let Some(ws) = ws {
                    sync.redial(ws);
                }
            }
        }
        // What the members' sessions show of these windows' pages, and what the
        // apps sent of them since the last pass.
        self.follow_pages(shared);
        self.deliver_box(shared, desktop, compositor, studio);
        // Where each app's own card stands this pass: the floating layer's place, which the shell
        // lays the app's window out by.
        self.follow_cards(desktop, compositor);
        // The person may have changed their answer about the device's own shell, here or on
        // another of their devices.
        self.host_shell.follow(services);
    }

    /// Publish the pages someone shows, and tell the apps how their windows are watched: a window
    /// that gained a viewer is asked for a whole frame and starts sending, one that lost its last
    /// viewer is told to stop, and a window's whole frame the live session needs again (a viewer joined, or
    /// lost the chain of changes) is asked for as the app can only answer. The app is asked on the
    /// connection it offered the page on, which for an app's first window is the one its launcher
    /// made and names no tab; an app that hands nothing over is asked again
    /// ([`crate::page_track::HAND_OVER_BOUND`]).
    fn follow_pages(&mut self, shared: &crate::Shared) {
        let live_sessions = shared
            .sync
            .as_deref()
            .map(crate::calls::live_sessions_of)
            .unwrap_or_default();
        let now_us = tilo_clock::unix_now_ms().saturating_mul(1000);
        // Only a window shared as its page goes out, and only in the workspace it is shared
        // with: a member's shell that asks for the page of a window nobody shared is sent
        // nothing, and its app is never asked.
        let shared_pages = shared.window_shares.borrow().paged();
        let asks = shared
            .pages
            .borrow_mut()
            .step(&live_sessions, &shared_pages, now_us);
        for (tab, watch) in asks {
            let offered = self
                .links
                .iter()
                .find(|(_, link)| link.page == Some(tab))
                .or_else(|| {
                    self.links
                        .iter()
                        .find(|(_, link)| link.instance == Some(tab))
                })
                .map(|(connection, _)| *connection);
            let Some(link) = offered.and_then(|connection| self.links.get_mut(&connection)) else {
                if watch == Watch::Whole {
                    tracing::warn!(
                        tab,
                        "a window's page is asked for and no connection of its app takes the ask"
                    );
                }
                continue;
            };
            let message = match watch {
                Watch::Whole => FloatingSurfaceHostMessage::PaintKeyframe,
                Watch::Stop => {
                    // Nobody takes the rest of a payload the window was sending in parts.
                    link.assembling = None;
                    FloatingSurfaceHostMessage::PaintStop
                }
            };
            tell(&link.writer, &message, &link.app);
        }
    }

    /// Ask the window in front to open its box, as the shell's send keys do
    /// ([`FloatingSurfaceHostMessage::ShareSelection`]): the app draws the box alone, with no menu
    /// around it, addressed to the people here or to the agent it makes by being sent to. Saying
    /// something to the people here addresses them; a report asks the window for the thing it is
    /// about first, and the box opens about what the person points at, to the one place this
    /// computer sends reports — or saying it is told nowhere, and still sending to an agent or a
    /// person. A window whose app cannot take a box — it never said it opens one, or it is gone —
    /// leaves the gesture to Chat, which a send has always opened.
    ///
    /// A report's box carries what happened by default: the picture of the window in front, and
    /// the last moments the system's capture kept when it keeps them, each a chip the person may
    /// take off before sending ([`crate::carried`]). It opens once the recording is written.
    fn deliver_box(
        &mut self,
        shared: &crate::Shared,
        desktop: &ShellDesktop,
        compositor: &mut Compositor,
        studio: Option<&CaptureStudio>,
    ) {
        if let Some(ask) = shared.take_box_ask() {
            match ask {
                BoxAsk::Agent => {
                    let about = self.link_in_front(desktop, compositor);
                    self.ask_box(shared, about, Vec::new(), Vec::new());
                }
                BoxAsk::People => {
                    let about = self.link_in_front(desktop, compositor);
                    self.ask_box(shared, about, shared.people_recipients(), Vec::new());
                }
                BoxAsk::Report => {
                    let about = self.link_in_front(desktop, compositor);
                    let window = about
                        .and_then(|connection| {
                            let link = self.links.get(&connection)?;
                            window_of(&self.links, desktop, connection, &link.app)
                        })
                        .and_then(|tab| desktop.with_policy(|policy| policy.window_of_tab(tab)))
                        .or_else(|| desktop.with_policy(|policy| policy.focused()));
                    self.report = Some(prepare_report(
                        shared.feedback_row(),
                        about,
                        window,
                        &shared.box_files,
                        compositor,
                        studio,
                    ));
                }
            }
        }
        if let Some(preparing) = self.report.take() {
            match preparing.ready(&shared.box_files) {
                Ok(ready) => self.ask_box(shared, ready.about, ready.to, ready.carried),
                Err(waiting) => self.report = Some(waiting),
            }
        }
    }

    /// Ask the window of connection `about` to open its box to `to`, carrying `carried`: a
    /// report's box asks the window for what it is about first.
    fn ask_box(
        &self,
        shared: &crate::Shared,
        about: Option<u64>,
        to: Vec<Recipient>,
        carried: Vec<tilo_reference::Attached>,
    ) {
        let pointing = to
            .iter()
            .any(|recipient| recipient.kind == ui::floating_surface::RecipientKind::Feedback);
        let asked = about
            .and_then(|connection| self.links.get(&connection))
            .is_some_and(|link| {
                tell(
                    &link.writer,
                    &FloatingSurfaceHostMessage::ShareSelection {
                        to,
                        pointing,
                        carried,
                    },
                    &link.app,
                )
            });
        if !asked {
            tracing::debug!("no window in front opens a box: the gesture opens Chat");
            shared.launchpad(LaunchpadAction::Chat);
        }
    }

    /// The connection of the window in front, among the windows whose app opens a box: one that
    /// never said it does is never sent a box it would not draw. A session that cannot tell which
    /// window is in front answers with its one window that takes boxes, and none when it holds
    /// more.
    fn link_in_front(&self, desktop: &ShellDesktop, compositor: &Compositor) -> Option<u64> {
        let ready = || self.links.iter().filter(|(_, link)| link.selection);
        // The window in front is the tab exactly one connection speaks for: another window of
        // the same app — one parked after its tab closed, one on another strip — is another
        // window, and never the one the person works in.
        let front = desktop.with_policy(|policy| (policy.focused(), policy.focused_tab()));
        let (focused, tab) = front;
        if let Some(tab) = tab
            && let Some((connection, _)) = ready().find(|(connection, link)| {
                window_of(&self.links, desktop, **connection, &link.app) == Some(tab)
            })
        {
            return Some(*connection);
        }
        // A window whose app has not named its tab yet: the launch of the window in front, when
        // it has one window that opens a box.
        let launch = focused
            .and_then(|window| {
                compositor
                    .windows()
                    .into_iter()
                    .find(|(held, _)| held.id == window)
            })
            .and_then(|(held, _)| held.pid)
            .and_then(|pid| self.launches.launch_of(pid));
        if let Some(launch) = launch {
            let mut of_launch = ready().filter(|(_, link)| link.launch == launch);
            if let Some((connection, _)) = of_launch.next()
                && of_launch.next().is_none()
            {
                return Some(*connection);
            }
        }
        let mut only = ready();
        let (first, _) = only.next()?;
        only.next().is_none().then_some(*first)
    }

    /// Stand each app's own card where the floating layer put it this pass,
    /// and stand it down while the window that draws it is on the shell: there
    /// the app draws the activity itself, and its card waits out of sight.
    ///
    /// The layer decides the place; the app's own geometry is a request it
    /// made when presenting, and its window is laid out by what this says.
    fn follow_cards(&self, desktop: &ShellDesktop, compositor: &mut Compositor) {
        let placements = self.cards.placements();
        // The windows on the shell this pass, and the launch each belongs to: what tells whether the
        // very window that draws an activity is on screen, rather than another window of the same
        // app.
        let shown: BTreeMap<u64, Rect> =
            desktop.with_policy(|policy| policy.placement().shown.into_iter().collect());
        let windows = compositor.windows();
        let shown_launches: BTreeSet<u32> = windows
            .iter()
            .filter(|(window, _)| shown.contains_key(&window.id))
            .filter_map(|(window, _)| window.pid)
            .map(|pid| self.launches.launch_of(pid).unwrap_or(pid))
            .collect();
        let mut moved = false;
        for card in self.cards.presented() {
            let in_view = match self.links.get(&card.connection) {
                // The launch that draws the activity has a window on the shell: the app draws it
                // there, and another window of the same app elsewhere leaves the card to this one.
                Some(link) => shown_launches.contains(&link.launch),
                // A connection that has not said whose launch it is is no window of its own yet:
                // the app's windows stand for it.
                None => desktop.with_policy(|policy| policy.shows(&card.app)),
            };
            // A picture floats above its app's window: the person keeps watching it while that
            // window is where they work, so being shown does not stand it down.
            let floats_above_its_window =
                card.role == FloatingSurfaceProtocolRole::PictureInPicture;
            let slot = self
                .cards
                .slot(&card.app, &card.activity)
                .or_else(|| self.picture_place(&card, &shown, &windows, compositor));
            let wanted = if (in_view && !floats_above_its_window) || card.stood_down {
                None
            } else {
                slot.map(|window| (window, card.body(window)))
            };
            let Some(link) = self.links.get(&card.connection) else {
                continue;
            };
            match (wanted, card.told) {
                (Some((_, body)), Some(told)) if body == told => {}
                (Some((window, body)), _) => {
                    self.cards.told(&card.app, &card.activity, Some(body));
                    tell(
                        &link.writer,
                        &FloatingSurfaceHostMessage::PrepareSurface {
                            id: card.activity.clone(),
                            token: card.token.clone(),
                            rect: ui::floating_surface::protocol_rect_array(window),
                            content_rect: ui::floating_surface::protocol_rect_array(body),
                            transfer_covered: false,
                            coordinate_offset: [body.x0 - card.local.x, body.y0 - card.local.y],
                        },
                        &card.app,
                    );
                    placements.place(
                        &card.token,
                        (whole(window.width()), whole(window.height())),
                        Some(window),
                    );
                    moved = true;
                }
                (None, None) => {}
                (None, Some(_)) => {
                    self.cards.told(&card.app, &card.activity, None);
                    placements.suspend(&card.token);
                    tell(
                        &link.writer,
                        &FloatingSurfaceHostMessage::Suspended {
                            id: card.activity.clone(),
                        },
                        &card.app,
                    );
                    moved = true;
                }
            }
            // The card's window is up and draws the activity: the app records the card as adopted,
            // so it knows the shell shows it.
            if let Some((_, body)) = wanted
                && placements.drawn(&card.token)
                && self.cards.adopt(&card.app, &card.activity)
            {
                tell(
                    &link.writer,
                    &FloatingSurfaceHostMessage::Adopted {
                        id: card.activity.clone(),
                        rect: ui::floating_surface::protocol_rect_array(body),
                    },
                    &card.app,
                );
            }
        }
        moved |= self.home_lost_cards();
        if moved {
            compositor.rearrange();
        }
    }

    /// Where an app's picture floats when the shell's cards hold no block for it: over the lower
    /// right corner of the window that presented it, or, while no window of that app is on the
    /// shell, over the same corner of the display the person works on. The place is the picture's
    /// for as long as it floats, so the window behind it moving never carries it away.
    fn picture_place(
        &self,
        card: &crate::cards::Presented,
        shown: &BTreeMap<u64, Rect>,
        windows: &[ListedWindow],
        compositor: &Compositor,
    ) -> Option<Rect> {
        if card.role != FloatingSurfaceProtocolRole::PictureInPicture {
            return None;
        }
        if let Some(seat) = self.cards.picture_seat(&card.app, &card.activity) {
            return Some(seat);
        }
        let launch = self.links.get(&card.connection).map(|link| link.launch)?;
        let over = windows.iter().find_map(|(window, _)| {
            let at = shown.get(&window.id)?;
            let own = window
                .pid
                .map(|pid| self.launches.launch_of(pid).unwrap_or(pid));
            (own == Some(launch)).then_some(*at)
        });
        let outputs = compositor.outputs();
        let area = over.or_else(|| {
            outputs
                .iter()
                .find(|output| output.enabled && output.primary)
                .or_else(|| outputs.iter().find(|output| output.enabled))
                .and_then(|output| output.rect)
                .map(|(x, y, width, height)| {
                    Rect::from_origin_size(
                        (f64::from(x), f64::from(y)),
                        (f64::from(width), f64::from(height)),
                    )
                })
        })?;
        let size = card.window_size();
        let x = (area.x1 - PICTURE_MARGIN - size.width).max(area.x0 + PICTURE_MARGIN);
        let y = (area.y1 - PICTURE_MARGIN - size.height).max(area.y0 + PICTURE_MARGIN);
        let seat = Rect::from_origin_size((x, y), (size.width, size.height));
        self.cards.seat(&card.app, &card.activity, seat);
        Some(seat)
    }

    /// A card's window that went away leaves the app drawing it on the shell with nothing to draw it
    /// in: send it home, and it presents the card again. Says whether the shell's cards changed.
    fn home_lost_cards(&self) -> bool {
        let placements = self.cards.placements();
        let mut moved = false;
        for token in self.cards.lost() {
            let Some(card) = self.cards.by_token(&token) else {
                continue;
            };
            placements.suspend(&token);
            self.cards.told(&card.app, &card.activity, None);
            if let Some(link) = self.links.get(&card.connection) {
                tell(
                    &link.writer,
                    &FloatingSurfaceHostMessage::Reattach {
                        id: card.activity.clone(),
                    },
                    &card.app,
                );
            }
            moved = true;
        }
        moved
    }

    /// Give every open window the appearance of the shell it stands on: the temperature of its
    /// workspace, the person's colour, the theme, and its workspace's sync state, each when it
    /// differs from what that window was last told. A window opens with one, and gets another
    /// whenever any of it changes; a pass that changes nothing writes nothing.
    ///
    /// `sync_of` is told the instance (the tab) the window names as well as its app: an app may
    /// have a window in a workspace its person writes and another where they only read, and
    /// every window is told what its own workspace says, never another window of the app.
    pub fn publish_window_warmth(
        &mut self,
        appearance_of: impl Fn(&str) -> WindowAppearance,
        sync_of: impl Fn(&str, Option<u64>) -> Option<WorkspaceSync>,
        theme: &Theme,
        theme_bundle: Option<&str>,
        network: HostNetworkStatus,
    ) {
        for link in self.links.values_mut() {
            let network = link.network_allowed.then_some(network);
            let window = appearance_of(&link.app);
            let sync = sync_of(&link.app, link.instance.or(link.named));
            let unchanged = link.told.as_ref().is_some_and(|last| {
                last.window == window
                    && last.theme == theme.document
                    && last.theme_bundle.as_deref() == theme_bundle
                    && last.network == network
                    && last.sync == sync
            });
            if unchanged {
                continue;
            }
            let mut message =
                FloatingSurfaceHostMessage::appearance_with_theme(&window, theme, theme_bundle);
            if let FloatingSurfaceHostMessage::Appearance {
                network_status,
                workspace_sync,
                ..
            } = &mut message
            {
                *network_status = network;
                workspace_sync.clone_from(&sync);
            }
            let written = link
                .writer
                .lock()
                .map_err(|_| "the connection's writer is gone".to_string())
                .and_then(|mut stream| {
                    write_message(&mut stream, &message).map_err(|error| error.to_string())
                });
            if let Err(error) = written {
                tracing::warn!(%error, app = %link.app, "a window's appearance was not written");
                continue;
            }
            link.told = Some(Delivered {
                window,
                theme: theme.document.clone(),
                theme_bundle: theme_bundle.map(str::to_owned),
                network,
                sync,
            });
        }
    }
}

/// The window that speaks on `connection`: the tab the session gave the connection, else the tab
/// the window names in its asks, else the app's only window. A connection its launcher made that
/// named no window yet, in an app with several, speaks for none of them until it does: which one
/// stands in front says nothing about who is talking.
fn window_of(
    links: &BTreeMap<u64, Link>,
    desktop: &ShellDesktop,
    connection: u64,
    app: &str,
) -> Option<u64> {
    let link = links.get(&connection)?;
    if let Some(instance) = link.instance.or(link.named) {
        return desktop.with_policy(|policy| policy.tab_of_instance(app, instance));
    }
    desktop.with_policy(|policy| {
        let mut windows = policy
            .app_tabs_in_front()
            .into_iter()
            .filter(|(_, id)| app_ids_match(id, app))
            .map(|(tab, _)| tab);
        match (windows.next(), windows.next()) {
            (Some(only), None) => Some(only),
            _ => None,
        }
    })
}

/// What one pass of answers reaches: the shell, the services the person answers through, and the
/// connections the answers go back on.
struct Answers<'a> {
    desktop: &'a ShellDesktop,
    /// The launches of this session, which a window an app asks for starts through: it is planned
    /// here and started once the shell asks back for the tab it takes.
    apps: Option<&'a Apps>,
    compositor: &'a mut Compositor,
    services: Option<&'a DesktopServices>,
    studio: Option<&'a CaptureStudio>,
    withheld: &'a mut BTreeMap<u64, String>,
    links: &'a mut BTreeMap<u64, Link>,
    cards: &'a crate::cards::Cards,
    launches: &'a Launches,
    /// The windows whose apps hand their page over as a paint stream.
    pages: &'a Rc<RefCell<PageTracks>>,
    /// The windows' trees, as assistive technology reads them, and the actions clients asked of
    /// them: what the session's AT-SPI export publishes.
    windows: &'a mut crate::a11y::WindowExports,
    /// The windows, by app and tab, whose workspace the person asked to dial again this pass.
    redials: &'a mut Vec<(String, Option<u64>)>,
}

impl Answers<'_> {
    /// The app draws its activity in its own window again: the shell hides the card's window, the
    /// app is told it stands down, and the window that draws it comes forward — the person asked
    /// for the activity where it lives, not for a card that vanishes.
    fn stand_down(&mut self, card: &crate::cards::Presented, reply: &Arc<Mutex<UnixStream>>) {
        self.cards.told(&card.app, &card.activity, None);
        self.cards.placements().suspend(&card.token);
        self.compositor.rearrange();
        tell(
            reply,
            &FloatingSurfaceHostMessage::Suspended {
                id: card.activity.clone(),
            },
            &card.app,
        );
        let Some(launch) = self.links.get(&card.connection).map(|link| link.launch) else {
            return;
        };
        let window = self
            .compositor
            .windows()
            .into_iter()
            .find(|(window, _)| {
                window.pid.and_then(|pid| self.launches.launch_of(pid)) == Some(launch)
                    && window
                        .app_id
                        .as_deref()
                        .is_some_and(|app_id| app_ids_match(app_id, &card.app))
            })
            .map(|(window, _)| window.id);
        if let Some(window) = window {
            self.desktop.with_policy(|policy| policy.focus(window));
        }
    }

    /// The window that speaks on `connection` ([`window_of`]).
    fn window_of(&self, connection: u64, app: &str) -> Option<u64> {
        window_of(self.links, self.desktop, connection, app)
    }

    /// Take up the offer the window on `connection` made of its page, for the window the
    /// connection speaks for ([`Self::window_of`]); `None` while it made none or that window is
    /// not known. The page is asked for where it was offered, and of the window's newest
    /// connection alone. An offer taken up for another tab before the window named itself moves
    /// with it, and the tab it stood under offers no page unless a connection of its own does.
    fn settle_offer(&mut self, connection: u64, app: &str, workspace: &str) -> Option<u64> {
        let link = self.links.get(&connection)?;
        if !link.offers {
            return None;
        }
        let earlier = link.page;
        let tab = self.window_of(connection, app)?;
        if earlier == Some(tab) {
            return Some(tab);
        }
        self.pages.borrow_mut().capable(tab, workspace);
        for (other, link) in &mut *self.links {
            if *other == connection {
                link.page = Some(tab);
                link.assembling = None;
            } else if link.page == Some(tab) {
                link.page = None;
            }
        }
        if let Some(earlier) = earlier
            && !self.links.values().any(|link| link.page == Some(earlier))
        {
            self.pages.borrow_mut().closed(earlier);
        }
        Some(tab)
    }

    /// The payload a frame of the page of `tab` completes: the frame's own, or the one whose
    /// last part it brings ([`ui::floating_surface::PaintPart`]); `None` while parts are still
    /// to come. A payload that says it is larger than the live session carries is not waited for: its
    /// first part is enough to say so ([`PageTracks::too_large`]). Parts that do not follow
    /// each other join nothing, and the window is asked for a whole frame again.
    fn page_payload(
        &mut self,
        connection: u64,
        tab: u64,
        payload: &str,
        part: Option<ui::floating_surface::PaintPart>,
    ) -> Result<Option<Vec<u8>>, &'static str> {
        use base64::Engine as _;
        let decode = |text: &str| {
            base64::engine::general_purpose::STANDARD
                .decode(text)
                .map_err(|_| "it is not base64")
        };
        let link = self
            .links
            .get_mut(&connection)
            .ok_or("its connection is gone")?;
        let Some(part) = part else {
            link.assembling = None;
            return decode(payload).map(Some);
        };
        let bytes = decode(&part.bytes)?;
        let of = usize::try_from(part.of).unwrap_or(usize::MAX);
        let at = usize::try_from(part.at).unwrap_or(usize::MAX);
        if at == 0 {
            link.assembling = None;
            if of > crate::page_track::MAX_PAYLOAD {
                let whole = ui::webview::paint::stream::wire::Payload::is_key(&bytes);
                self.pages.borrow_mut().too_large(tab, whole, of);
                return Ok(None);
            }
            return match bytes.len().cmp(&of) {
                std::cmp::Ordering::Less => {
                    link.assembling = Some((of, bytes));
                    Ok(None)
                }
                std::cmp::Ordering::Equal => Ok(Some(bytes)),
                std::cmp::Ordering::Greater => Err("a part is longer than its payload"),
            };
        }
        match link.assembling.as_mut() {
            Some((expected, held))
                if *expected == of && held.len() == at && at.saturating_add(bytes.len()) <= of =>
            {
                held.extend_from_slice(&bytes);
                if held.len() < of {
                    return Ok(None);
                }
            }
            _ => {
                link.assembling = None;
                self.pages.borrow_mut().lost(tab);
                return Err("a part does not follow the one before it");
            }
        }
        Ok(link.assembling.take().map(|(_, held)| held))
    }

    /// Answer one ask of the app it came from: an ask never changes another app's tab, and an app
    /// whose ask this session cannot answer is told so rather than left waiting.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per message, so every one of them is answered here"
    )]
    fn answer(&mut self, host_shell: &mut HostShellAsks, request: Request) {
        let Request {
            app,
            workspace,
            launch,
            connection,
            reply,
            ask,
            instance,
        } = request;
        let message = match ask {
            Ask::Closed => {
                withhold(
                    self.withheld,
                    self.desktop,
                    self.compositor,
                    connection,
                    &app,
                    false,
                );
                if let Some(link) = self.links.remove(&connection) {
                    // A page nothing runs any more is published nowhere, and what its window said
                    // it shows goes with it.
                    for tab in [link.instance, link.page].into_iter().flatten() {
                        self.pages.borrow_mut().closed(tab);
                    }
                    if let Some(tab) = link.instance {
                        self.launches.notes().forget(tab);
                    }
                    // The window's tree went with the connection a client of the export read it
                    // through.
                    self.windows.remove_connection(connection);
                }
                // The cards it presented, and the windows the shell stood
                // them in, go with it.
                let mut gone = false;
                for card in self.cards.connection_closed(connection) {
                    self.cards.placements().withdraw(&card.token);
                    gone = true;
                }
                if gone {
                    self.compositor.rearrange();
                }
                // What this connection declared for its window's bar goes with it, as does the
                // question it waits on.
                self.desktop
                    .with_policy(|policy| policy.handle().bar_connection_closed(connection));
                host_shell.closed(connection);
                return;
            }
            Ask::Connected { network_allowed } => {
                self.links.insert(
                    connection,
                    Link {
                        app,
                        launch,
                        instance,
                        named: None,
                        offers: false,
                        page: None,
                        assembling: None,
                        network_allowed,
                        selection: false,
                        writer: reply,
                        told: None,
                    },
                );
                return;
            }
            Ask::Message(message) => *message,
        };
        // The window the ask is about: the tab the session gave the connection, else the tab its
        // own ask names, and only a window of neither is the app's window in front — never
        // another window of the same app.
        let instance = self
            .links
            .get(&connection)
            .and_then(|link| link.instance)
            .or_else(|| instance_of(&message));
        // A window its launcher connected says which window it is in what it asks: from then on
        // that is the window its connection speaks for, and an offer of its page that waited for
        // that is taken up. It says so once: an ask that names another window later moves
        // nothing.
        if let Some(named) = instance_of(&message)
            && let Some(link) = self.links.get_mut(&connection)
            && link.instance.is_none()
            && link.named.is_none()
        {
            link.named = Some(named);
            self.settle_offer(connection, &app, &workspace);
        }
        let own = self.desktop.with_policy(|policy| match instance {
            Some(instance) => policy.tab_of_instance(&app, instance),
            None => policy.anchor_for(&app).map(|(tab, ..)| tab),
        });
        let name = name_of(&message);
        match message {
            FloatingSurfaceClientMessage::OpenTabBeside {
                target, background, ..
            } => self.answer_open_beside(&app, target, background, own),
            FloatingSurfaceClientMessage::CloseTab { .. } => {
                // The tab's window goes: assistive technology stops finding it.
                if let Some(tab) = own {
                    self.windows.remove(tab);
                }
                self.desktop.with_policy(|policy| policy.close_tab_of(&app));
            }
            FloatingSurfaceClientMessage::RetryWorkspace { .. } => {
                self.redials.push((app.clone(), instance));
            }
            FloatingSurfaceClientMessage::ReopenClosedTab => {
                self.desktop
                    .with_policy(|policy| policy.reopen_closed_of(&app));
            }
            FloatingSurfaceClientMessage::CycleTab { forward, .. } => {
                self.desktop
                    .with_policy(|policy| policy.cycle_tab(&app, forward));
            }
            FloatingSurfaceClientMessage::PlaceNextWindowBeside { background, .. } => {
                self.desktop
                    .with_policy(|policy| policy.place_next_beside(&app, own, background));
            }
            FloatingSurfaceClientMessage::SetCaptureProtection {
                protected,
                generation,
            } => self.answer_protection(connection, &app, &reply, protected, generation),
            FloatingSurfaceClientMessage::Toolbar { toolbar, .. } => {
                // The parts this app shows in its window's bar, where its window is a tab of the
                // desktop's.
                match own {
                    Some(tab) => self.desktop.with_policy(|policy| {
                        policy.handle().declare_bar(tab, connection, toolbar);
                    }),
                    None => refuse(&reply, &app, &name, NO_WINDOW),
                }
            }
            FloatingSurfaceClientMessage::WindowTargets { targets, .. } => {
                // The controls of the window's own content, where its window is a tab of the
                // desktop's, so a session names and presses them as it names the bar's.
                match own {
                    Some(tab) => self.desktop.with_policy(|policy| {
                        policy.handle().declare_targets(tab, connection, targets);
                    }),
                    None => refuse(&reply, &app, &name, NO_WINDOW),
                }
            }
            FloatingSurfaceClientMessage::AccessibilityTree {
                tree, name: window, ..
            } => {
                // What assistive technology reads of the window, published by the session for
                // it: only the window a connection speaks for may say it, so a window of this
                // app is never another window's tree.
                let speaks_for = self
                    .links
                    .get(&connection)
                    .and_then(|link| link.instance.or(link.named));
                // The tab of the window, when it is on the shell: `own` resolved it for this
                // app, so a window of one app never says another app's tree. A window that
                // sends its tree before the shell mapped it is attributed to the instance this
                // very ask names, and its tab joins on the next pass; it is not lost until its
                // tree changes again.
                let attributed =
                    own.or_else(|| speaks_for.filter(|named| instance == Some(*named)));
                match attributed {
                    Some(tab) => self.windows.set(tab, connection, window, tree),
                    None => refuse(&reply, &app, &name, NO_WINDOW),
                }
            }
            FloatingSurfaceClientMessage::EditLocation { .. } => match own {
                Some(tab) => self
                    .desktop
                    .with_policy(|policy| policy.handle().edit_bar_location(tab)),
                None => refuse(&reply, &app, &name, NO_WINDOW),
            },
            FloatingSurfaceClientMessage::WindowOffer { offer, .. } => match own {
                Some(tab) => self.desktop.with_policy(|policy| {
                    policy.handle().offer_bar(tab, connection, offer);
                }),
                None => refuse(&reply, &app, &name, NO_WINDOW),
            },
            FloatingSurfaceClientMessage::OpenSearch { opening, .. } => match own {
                Some(tab) => self
                    .desktop
                    .with_policy(|policy| policy.handle().open_bar_search(tab, opening)),
                None => refuse(&reply, &app, &name, NO_WINDOW),
            },
            FloatingSurfaceClientMessage::OpenAppSearch { request, .. } => match own {
                Some(tab) => self
                    .desktop
                    .with_policy(|policy| policy.handle().search_app(tab, request)),
                None => refuse(&reply, &app, &name, NO_WINDOW),
            },
            FloatingSurfaceClientMessage::AllowHostShell => {
                // The person allows the device's own shell, and the runtime (`ush`) records their
                // answer; the app hears it here. Only an app whose launches are handed `ush` has a
                // use for it, so no other app puts the question in front of the person.
                if !host_shell.ask(self.services, &app, connection, Arc::clone(&reply)) {
                    refuse(&reply, &app, &name, NO_USH);
                }
            }
            // The person picks the area in the session's own studio, so the app opens a chooser
            // and is given nothing it did not pick.
            FloatingSurfaceClientMessage::PickPictureInPictureArea => match self.studio {
                Some(studio) => studio.open_asked(ui::widget::CaptureMode::Pip),
                None => refuse(&reply, &app, &name, NO_CAPTURE),
            },
            // A window paints its own action markers, at the control an agent would press, so
            // nothing here draws a second one; and the session names none of them to assistive
            // technology or opens what one points at, so the inventory is read and adds nothing. A
            // surface cleared or a destination presented is the app's own business.
            FloatingSurfaceClientMessage::ActionMarkers { .. }
            | FloatingSurfaceClientMessage::SourceCleared { .. }
            | FloatingSurfaceClientMessage::DestinationPresented { .. } => {}
            // The window opens a send-to box when the shell asks: from now on it is the one the
            // shell's send key asks, and no window that never said so is ever sent a box it would
            // not draw.
            FloatingSurfaceClientMessage::SelectionCapable => {
                if let Some(link) = self.links.get_mut(&connection) {
                    link.selection = true;
                    tracing::debug!(app = %link.app, "a window opens a send-to box");
                }
            }
            // An app's own card — a call, a route — is a window of its own: the app presents it,
            // the floating layer decides where it stands, and the app draws the activity inline
            // whenever its own window is on the shell.
            FloatingSurfaceClientMessage::Present { request } => {
                if let Err(reason) = self.cards.present(&app, connection, &request) {
                    tracing::warn!(%app, %reason, "card refused");
                    refuse(&reply, &app, &name, &reason);
                }
            }
            // The app's own geometry is a request: the layer's place stands, but a picture the
            // person dragged stands where they put it, and the next float of it comes back
            // there. A card the app presents again after bringing it down draws the activity
            // there once more.
            FloatingSurfaceClientMessage::SurfaceGeometry { id, rect, .. } => {
                self.cards.presented_again(&app, connection, &id);
                if let Ok(at) = protocol_rect(rect) {
                    let at = on_outputs(at, self.compositor.outputs());
                    self.cards.moved(&app, connection, &id, at);
                }
            }
            // The app draws the activity in its own window again: its card stands down until it
            // presents one again.
            FloatingSurfaceClientMessage::Suspend { id, .. } => {
                if let Some(card) = self.cards.stood_down(&app, connection, &id) {
                    self.stand_down(&card, &reply);
                }
            }
            // The app ended the activity: its card goes, and the window the shell stood there goes
            // with it.
            FloatingSurfaceClientMessage::Dismiss { id } => {
                if let Some(card) = self.cards.dismissed(connection, &id) {
                    self.cards.placements().withdraw(&card.token);
                    self.compositor.rearrange();
                }
                tell(&reply, &FloatingSurfaceHostMessage::Closed { id }, &app);
            }
            // The picture an app floats moved in its own window — the page scrolled, the window
            // resized — so the surface mirrors where it stands now. Only the app that presented
            // it, on the window that presented it, may move it.
            FloatingSurfaceClientMessage::UpdateRegion { id, region } => {
                match protocol_rect(region) {
                    Ok(region) if self.cards.update_region(&app, connection, &id, region) => {}
                    Ok(_) => refuse(&reply, &app, &name, NO_REGION),
                    Err(reason) => refuse(&reply, &app, &name, &reason),
                }
            }
            // The record a window shows, as its app holds it. An app that keeps one document for
            // the whole workspace has no record id to name it by (its route is empty), so the
            // document's kind names it: the same on every device, and the same for every window
            // showing it.
            FloatingSurfaceClientMessage::WindowRecord { record, kind, .. } => {
                let record = if record.trim().is_empty() {
                    &kind
                } else {
                    &record
                };
                if let Some(tab) = own {
                    self.launches.notes().note(tab, &app, &workspace, record);
                }
            }
            // What else a window shares live, the embeds it draws of other apps' resources, and
            // the selection an agent works from: the session serves none of them over this
            // connection yet.
            FloatingSurfaceClientMessage::Embeds { .. }
            | FloatingSurfaceClientMessage::SharedWindows { .. } => {
                refuse(&reply, &app, &name, NO_WINDOW_SHARE);
            }
            // The resource a window shows is what the composer asks the agent about it, so it goes
            // with the selection it seeds.
            FloatingSurfaceClientMessage::WindowResource { .. }
            | FloatingSurfaceClientMessage::Pointed { .. }
            | FloatingSurfaceClientMessage::PickAnswered { .. } => {
                refuse(&reply, &app, &name, NO_AGENT);
            }
            // The app's send landed on `anchor`: a window of the shell showing it, beside the
            // window that sent.
            FloatingSurfaceClientMessage::OpenFloated { anchor, .. } => {
                self.answer_open_after_send(&app, &anchor);
            }
            // A window that runs a page engine offers its page as the thing the
            // workspace sees of it: what it draws is published on the live
            // live session as its window's page track, and only while a member shows
            // it. Tilo apps' own views are never streamed, and neither is
            // anything but a page.
            //
            // The offer names no window. It is taken up for the window its connection speaks
            // for, which is never guessed from which window is in front: an app has several
            // windows, each with a page and a connection of its own, and a page offered as
            // another window's would be shown to whoever watches that one.
            //
            // What the offer does say at once is that the app draws its windows' pages: from
            // here on none of them is shared as pictures, also not one whose page is not taken
            // up yet.
            FloatingSurfaceClientMessage::PaintCapable => {
                self.pages.borrow_mut().engine_of(&app);
                if let Some(link) = self.links.get_mut(&connection) {
                    link.offers = true;
                }
                if self.settle_offer(connection, &app, &workspace).is_none() && own.is_none() {
                    refuse(&reply, &app, &name, NO_WINDOW);
                }
            }
            // A frame is of the page its connection offered, and of no other: one that names
            // another window of the app, or comes where no page was offered, is published as
            // nobody's. `page` and `scale` place a viewer's input in the page, which this
            // session does not take: the page is drawn, not typed into, by the members that
            // show it.
            FloatingSurfaceClientMessage::PaintStream { payload, part, .. } => {
                match self.links.get(&connection).and_then(|link| link.page) {
                    Some(tab) if instance.is_none() || own == Some(tab) => {
                        match self.page_payload(connection, tab, &payload, part) {
                            Ok(Some(bytes)) => {
                                let whole =
                                    ui::webview::paint::stream::wire::Payload::is_key(&bytes);
                                self.pages.borrow_mut().received(tab, whole, bytes);
                            }
                            Ok(None) => {}
                            Err(reason) => {
                                tracing::warn!(%app, reason, "a window's page frame was not taken");
                            }
                        }
                    }
                    Some(_) => refuse(&reply, &app, &name, OTHER_WINDOW),
                    None => refuse(&reply, &app, &name, NO_PAGE),
                }
            }
            // The device's own shell is what a window of Terminal asks about: not asked here.
            FloatingSurfaceClientMessage::DeviceShellOutcome { .. } => {
                refuse(&reply, &app, &name, NO_DEVICE);
            }
            // The app under the pointer did not scroll with a sideways wheel: the strip its window
            // stands in pans by it, as a wheel over the strip's own canvas does. Nothing to answer
            // — the app took what it scrolled with, and this is what it left.
            FloatingSurfaceClientMessage::SidewaysWheel { travel } => {
                if let Some(tab) = own {
                    self.desktop
                        .with_policy(|policy| policy.handle().sideways_wheel(tab, travel));
                }
            }
            // The connection said its version when it opened; a second one is answered with the one
            // this session speaks.
            FloatingSurfaceClientMessage::Hello { .. } => refuse(&reply, &app, &name, SAID),
        }
    }

    /// Open the place a window of `app` asked for: beside the one that asked, and a launch that
    /// starts nothing takes the place back.
    fn answer_open_beside(
        &self,
        app: &str,
        target: Option<String>,
        background: bool,
        anchor: Option<u64>,
    ) {
        let Some(apps) = self.apps else {
            return;
        };
        let open = if let Some(target) = target {
            let Some(open) = target_open(app, &target) else {
                tracing::info!(%app, "an app asked to open an address it does not claim");
                return;
            };
            Some(open)
        } else {
            None
        };
        // The tab is there from this frame and the window starts once the shell asks back, so the
        // tab is the window's instance.
        apps.ask_window(
            &Launch::Package {
                app: app.to_owned(),
                open,
            },
            anchor,
            background,
            false,
            None,
        );
    }

    /// Open a window of `app` beside the one that sent from its box, showing
    /// the item `anchor` names: the conversation a message from that box
    /// landed in. It opens in a window of its own, over the shell as
    /// the chord floats a window — over the place its strip gave it, the strip
    /// sharing out the card it draws at, so it stands beside what the person
    /// was reading instead of over it. The workspace's record keeps it there
    /// beside its sender, for every member, and the window that sent it goes on
    /// showing what it showed.
    fn answer_open_after_send(&self, app: &str, anchor: &str) {
        let Some(apps) = self.apps else {
            return;
        };
        // One window per item: the conversation a window already shows is that window, so a
        // second send into it raises the window it stands in instead of opening another on the
        // same conversation.
        if self.desktop.with_policy(|policy| {
            policy
                .tab_showing(app, anchor)
                .is_some_and(|tab| policy.focus_tab(tab))
        }) {
            return;
        }
        // The tab is there from this frame and the window starts once the shell asks back, so the
        // tab is the window's instance.
        apps.ask_window(
            &Launch::Package {
                app: app.to_owned(),
                open: None,
            },
            None,
            false,
            true,
            Some(anchor),
        );
    }

    /// Withhold `app`'s windows from captures, as its own ask narrows what
    /// leaves the display and never widens it, and confirm the change.
    fn answer_protection(
        &mut self,
        connection: u64,
        app: &str,
        reply: &Arc<Mutex<UnixStream>>,
        protected: bool,
        generation: Option<u64>,
    ) {
        withhold(
            self.withheld,
            self.desktop,
            self.compositor,
            connection,
            app,
            protected,
        );
        if let Some(generation) = generation {
            tell(
                reply,
                &FloatingSurfaceHostMessage::CaptureProtectionApplied {
                    generation,
                    protected,
                },
                app,
            );
        }
        tracing::debug!(connection, %app, protected, "capture protection asked");
    }

    /// Write what the person did in the windows' bars to the app whose connection declared the
    /// parts there.
    fn deliver_bar_events(&self) {
        write_bar_events(self.desktop, &*self.compositor, self.links);
    }

    /// Write the controls pressed in the windows' own content to the apps
    /// whose connections described them
    /// ([`ui::floating_surface::FloatingSurfaceClientMessage::WindowTargets`]),
    /// which answer as an assistive technology's activation does.
    fn deliver_target_presses(&self) {
        write_target_presses(self.desktop, &*self.compositor, self.links);
    }
}

/// Write what the person did in the windows' bars to the app whose connection declared the parts
/// there.
fn write_bar_events(desktop: &ShellDesktop, compositor: &Compositor, links: &BTreeMap<u64, Link>) {
    let events = desktop.with_policy(|policy| policy.handle().take_bar_events());
    for (tab, connection, event) in events {
        let Some(link) = links.get(&connection) else {
            continue;
        };
        // In its place among the window's own input: what the person typed before using the
        // bar is read before what the bar did.
        tell(
            &link.writer,
            &FloatingSurfaceHostMessage::ToolbarEvent {
                event,
                after_input: input_before(desktop, compositor, tab),
            },
            &link.app,
        );
    }
}

/// Write the controls pressed in the windows' own content to the apps whose connections described
/// them, which answer as an assistive technology's activation does.
fn write_target_presses(
    desktop: &ShellDesktop,
    compositor: &Compositor,
    links: &BTreeMap<u64, Link>,
) {
    let presses = desktop.with_policy(|policy| policy.handle().take_target_presses());
    for press in presses {
        let Some(link) = links.get(&press.connection) else {
            continue;
        };
        tell(
            &link.writer,
            &FloatingSurfaceHostMessage::PressTarget {
                name: press.name,
                role: press.role,
                identity: press.identity,
                action: press.action,
                // In its place among the window's own input: a save pressed as the person's
                // typing is still on its way reads every character typed before it.
                after_input: input_before(desktop, compositor, press.tab),
            },
            &link.app,
        );
    }
}

/// The serial of the newest input the shell sent the window drawing `tab`'s content, which a
/// press in that window comes after: `None` while the window received none, or has none.
fn input_before(desktop: &ShellDesktop, compositor: &Compositor, tab: u64) -> Option<u32> {
    let window = desktop.with_policy(|policy| policy.window_of_tab(tab));
    compositor.last_input_serial(window?)
}

/// An app asked of a window this session shows in no tab: it has none open,
/// or it is not the app of the tab that asked.
const NO_WINDOW: &str = "this session shows no window of that app";

/// A window sent a frame of its page on a connection that offered none.
const NO_PAGE: &str = "this connection offered no page";

/// A window sent a frame that names another window than the one its connection offered the
/// page of.
const OTHER_WINDOW: &str = "this connection offered another window's page";

/// An app asked for the capture studio where the session has none.
const NO_CAPTURE: &str = "this session has no capture studio";

/// An app that is handed no `ush` asked for the device's own shell.
const NO_USH: &str = "only an app handed the device's shell (`ush`) asks for it";

/// An app asked to float a surface over the desktop, which this session has
/// no layer for.
const NO_REGION: &str = "this session mirrors no floating surface's region";

/// An app asked to share a window live or embed another's resource.
const NO_WINDOW_SHARE: &str = "this session shares no window live";

/// An app asked about what a person selected, and for an agent to work from it.
const NO_AGENT: &str = "this session asks no window about a selection";

/// An app answered a device-shell operation.
const NO_DEVICE: &str = "this session runs no device-shell operation here";

/// A second greeting on a connection whose version was read when it opened.
const SAID: &str = "this connection said its version when it opened";

/// `value` as a whole logical pixel.
#[allow(clippy::cast_possible_truncation, reason = "a card's size on a screen")]
fn whole(value: f64) -> i32 {
    value.round() as i32
}

/// The tab the window sending `message` names, when it names one: a window the
/// launcher connected says which tab it takes in each ask.
fn instance_of(message: &FloatingSurfaceClientMessage) -> Option<u64> {
    match message {
        FloatingSurfaceClientMessage::WindowTargets { instance, .. }
        | FloatingSurfaceClientMessage::AccessibilityTree { instance, .. }
        | FloatingSurfaceClientMessage::Toolbar { instance, .. }
        | FloatingSurfaceClientMessage::WindowOffer { instance, .. }
        | FloatingSurfaceClientMessage::EditLocation { instance }
        | FloatingSurfaceClientMessage::RetryWorkspace { instance }
        | FloatingSurfaceClientMessage::OpenSearch { instance, .. }
        | FloatingSurfaceClientMessage::OpenAppSearch { instance, .. }
        | FloatingSurfaceClientMessage::OpenFloated { instance, .. }
        | FloatingSurfaceClientMessage::WindowRecord { instance, .. }
        | FloatingSurfaceClientMessage::PaintStream { instance, .. }
        | FloatingSurfaceClientMessage::Embeds { instance, .. } => *instance,
        _ => None,
    }
}

/// The name `message` is sent under, as an app reads it in
/// [`FloatingSurfaceHostMessage::Unanswered`].
fn name_of(message: &FloatingSurfaceClientMessage) -> String {
    match serde_json::to_value(message) {
        Ok(serde_json::Value::Object(fields)) => fields
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    }
}

/// Tell the app on `reply` that this session does not answer `name` , and why, so nothing of the
/// app's waits on an answer that will not come.
fn refuse(reply: &Arc<Mutex<UnixStream>>, app: &str, name: &str, reason: &str) {
    tell(
        reply,
        &FloatingSurfaceHostMessage::Unanswered {
            message: name.to_owned(),
            reason: reason.to_owned(),
        },
        app,
    );
}

/// Write `message` to the app on its connection, saying so when it cannot be written.
fn tell(reply: &Arc<Mutex<UnixStream>>, message: &FloatingSurfaceHostMessage, app: &str) -> bool {
    let written = reply
        .lock()
        .map_err(|_| "the connection's writer is gone".to_string())
        .and_then(|mut stream| {
            write_message(&mut stream, message).map_err(|error| error.to_string())
        });
    match written {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, %app, "a shell-ui answer was not written");
            false
        }
    }
}

/// Mark the windows of `app` private (`private`) or showable again, as `connection` asks: the mark
/// lasts while one of the app's connections asks for it, so it goes when the last one stops asking
/// or closes.
fn withhold(
    withheld: &mut BTreeMap<u64, String>,
    desktop: &ShellDesktop,
    compositor: &mut Compositor,
    connection: u64,
    app: &str,
    private: bool,
) {
    let asked = withheld.remove(&connection).is_some();
    if private {
        withheld.insert(connection, app.to_owned());
        compositor.protect_app(app, true);
    } else if asked && !withheld.values().any(|named| named == app) {
        compositor.protect_app(app, false);
    }
    // The shell knows the same apps, so nothing it names for the compositor to draw is named for an
    // app that keeps its content, and a card in the switcher or the overview says so instead of
    // showing what the window holds back.
    desktop.set_withheld_apps(withheld.values().cloned().collect());
}

/// The address `app` asks to open, when its manifest claims it: a link it declares the scheme of,
/// as its own menu would open one.
fn target_open(app: &str, target: &str) -> Option<Open> {
    if target.is_empty()
        || target.len() > MAX_TARGET_BYTES
        || target.trim() != target
        || target.chars().any(char::is_control)
    {
        return None;
    }
    let scheme = tilo_actions::remote::link_scheme(target)?;
    let installed = app_catalogue::apps::read_one_from_env(app).ok().flatten()?;
    installed
        .opens_uri_schemes
        .iter()
        .any(|claims| claims.eq_ignore_ascii_case(&scheme))
        .then(|| Open::Uri(target.to_owned()))
}

/// The listener the apps connect to, its path, and what a connection taken
/// off it is served with.
///
/// The connections are taken on the session's own pass ([`Socket::take`]): a
/// thread waiting in `accept` could only be joined once a client woke it, and
/// a session another one took the path from, or one that outlives its clients,
/// would never be woken at all.
struct Socket {
    listener: UnixListener,
    /// What ends the loop's wait while an app connects or writes.
    watch: Option<ReadableWatch>,
    path: PathBuf,
    /// The socket at `path` when it was bound: a session that takes the path later binds
    /// one of its own, which this one leaves in place.
    inode: Option<(u64, u64)>,
    /// The connection number the next connection taken is given.
    next: AtomicU64,
    /// The launches a connection is checked against, the channel its asks arrive on, and the wake
    /// that brings the session to answer them.
    launches: Arc<Launches>,
    requests: mpsc::Sender<FromConnection>,
    wake: tilo_service::Wake,
}

impl Socket {
    fn serve(
        listener: UnixListener,
        path: PathBuf,
        launches: Arc<Launches>,
        requests: mpsc::Sender<FromConnection>,
        wake: tilo_service::Wake,
    ) -> Result<Self, String> {
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("serve the shell-ui socket: {error}"))?;
        Ok(Self {
            inode: inode_of(&path),
            listener,
            watch: None,
            path,
            next: AtomicU64::new(1),
            launches,
            requests,
            wake,
        })
    }

    /// Take every connection waiting, each served on a thread of its own: what it asks arrives on
    /// the session's channel, for the session to answer on this pass.
    fn take(&self) {
        loop {
            let stream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    tracing::warn!(%error, "the shell-ui service could not accept");
                    return;
                }
            };
            let connection = self.next.fetch_add(1, Ordering::Relaxed);
            let (launches, requests, wake) = (
                Arc::clone(&self.launches),
                self.requests.clone(),
                Arc::clone(&self.wake),
            );
            if let Err(error) = std::thread::Builder::new()
                .name(format!("tilo-shell-ui-{connection}"))
                .spawn(move || serve_connection(connection, stream, &launches, &requests, &wake))
            {
                // Nobody serves this connection: the app it belongs to waits.
                tracing::warn!(%error, connection, "a shell-ui connection worker could not start");
            }
        }
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        // A session started while this one ran takes the path for its own: the socket
        // there is that session's, and stays.
        if inode_of(&self.path) == self.inode {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// The socket at `path` , as the file system names it: the device and inode of the node
/// there. A bound socket's own file descriptor names an inode of the socket layer, not this one.
fn inode_of(path: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::symlink_metadata(path).ok()?;
    Some((meta.dev(), meta.ino()))
}

/// The launcher's word for a connection, written before the sandbox starts: which app it is, as the
/// launcher knows it, and whether that launch let the app know the network's state.
#[derive(Deserialize)]
struct LauncherHandshake {
    protocol: u16,
    app_id: String,
    /// Absent from a launcher that predates the field: such an app is told nothing of the network.
    #[serde(default)]
    network_status_allowed: bool,
}

/// The app's first frame as every shell-ui version reads it: `{"type": "hello", "version": N}` ,
/// whatever else a later version adds.
#[derive(Deserialize)]
struct Hello {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    version: Option<u16>,
}

/// The shell-ui version `frame` says, when it is a hello.
fn hello_version(frame: &[u8]) -> Option<u16> {
    tilo_failure::ignored!(
        "a frame that does not read back is not a hello",
        serde_json::from_slice::<Hello>(frame)
            .ok()
            .filter(|hello| hello.kind == "hello")
            .and_then(|hello| hello.version)
    )
}

// The channel handles are deliberately owned by the per-connection worker,
// which outlives the pass that took the connection off the socket.
fn serve_connection(
    connection: u64,
    stream: UnixStream,
    launches: &Launches,
    requests: &mpsc::Sender<FromConnection>,
    wake: &tilo_service::Wake,
) {
    if admit(connection, stream, launches, requests, wake) {
        return;
    }
    // The session took this connection off its socket and answered nothing on it: its end is still
    // the session's to hear, so its loop is woken rather than sleeping on a connection it took.
    tracing::debug!(
        connection,
        "a shell-ui connection named no window this session serves"
    );
    let _ = requests.send(FromConnection::Unadmitted { connection });
    wake();
}

/// Serve `stream` as the window of a launch this session started, answering whether it named one:
/// what it does not admit is dropped here, having said nothing the session answers.
fn admit(
    connection: u64,
    stream: UnixStream,
    launches: &Launches,
    requests: &mpsc::Sender<FromConnection>,
    wake: &tilo_service::Wake,
) -> bool {
    // The process at the other end, as the socket says it: a window the app opened, or the
    // session's own connection for one it opened in the app.
    let peer = peer_pid(&stream);
    let Ok(mut writer) = stream.try_clone() else {
        return false;
    };
    let mut reader = BufReader::new(stream);
    let Some(handshake_frame) = wire_frame::read_line(&mut reader, MAX_FRAME_BYTES)
        .ok()
        .flatten()
    else {
        return false;
    };
    // A client that does not speak this protocol is dropped.
    let Ok(handshake) = tilo_failure::ignored!(
        "a handshake that does not read back is not one of ours",
        serde_json::from_slice::<LauncherHandshake>(&handshake_frame)
    ) else {
        return false;
    };
    if handshake.protocol != SHELL_UI_LAUNCH_PROTOCOL
        || handshake.app_id.trim().is_empty()
        || handshake.app_id.len() > 255
    {
        return false;
    }
    // The app's first frame says the version it speaks; nothing else is read from an app at another
    // one.
    let Ok(Some(hello)) = wire_frame::read_line(&mut reader, MAX_FRAME_BYTES) else {
        return false;
    };
    let app_version = hello_version(&hello);
    if app_version != Some(FLOATING_SURFACE_PROTOCOL_VERSION) {
        refuse_version(&mut writer, &handshake.app_id, app_version);
        return false;
    }
    let app = handshake.app_id;
    let Some((workspace, pid, instance)) = launched(launches, peer, &app) else {
        tracing::info!(%app, "a shell-ui connection names no app this session started");
        return false;
    };
    // The launch the window belongs to: an app runs one process per window tree, and a window that
    // connects from below it names that tree.
    let launch = launches.launch_of(pid).unwrap_or(pid);
    serve_window(
        Window {
            connection,
            app,
            workspace,
            launch,
            instance,
            network_allowed: handshake.network_status_allowed,
        },
        reader,
        writer,
        requests,
        wake,
    );
    true
}

/// A window whose connection this session admitted: the app the launcher vouched for, the
/// workspace its launch runs it in, the launch it belongs to and, for a window the session
/// connected itself, the tab it takes.
struct Window {
    connection: u64,
    app: String,
    workspace: String,
    launch: u32,
    instance: Option<u64>,
    network_allowed: bool,
}

/// Serve an admitted window's connection until it ends: every frame it writes becomes an ask the
/// session answers on its pass.
fn serve_window(
    window: Window,
    mut reader: BufReader<UnixStream>,
    writer: UnixStream,
    requests: &mpsc::Sender<FromConnection>,
    wake: &tilo_service::Wake,
) {
    let Window {
        connection,
        app,
        workspace,
        launch,
        instance,
        network_allowed,
    } = window;
    let reply = Arc::new(Mutex::new(writer));
    // From here the window is told the appearance of the shell it stands on,
    // and it is told once more whenever that changes.
    if requests
        .send(FromConnection::Ask(Box::new(Request {
            app: app.clone(),
            workspace: workspace.clone(),
            launch,
            instance,
            connection,
            reply: Arc::clone(&reply),
            ask: Ask::Connected { network_allowed },
        })))
        .is_err()
    {
        return;
    }
    wake();
    loop {
        let frame = match wire_frame::read_line(&mut reader, MAX_FRAME_BYTES) {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(%error, %app, "a shell-ui frame was refused");
                break;
            }
        };
        let Ok(message) = serde_json::from_slice::<FloatingSurfaceClientMessage>(&frame) else {
            tracing::warn!(%app, "a shell-ui message was refused");
            break;
        };
        // Every message is answered: what this session acts on it acts on,
        // and what it cannot is told to the app
        // ([`FloatingSurfaceHostMessage::Unanswered`]), never left waiting.
        if requests
            .send(FromConnection::Ask(Box::new(Request {
                app: app.clone(),
                workspace: workspace.clone(),
                launch,
                instance,
                connection,
                reply: Arc::clone(&reply),
                ask: Ask::Message(Box::new(message)),
            })))
            .is_err()
        {
            break;
        }
        wake();
    }
    // What this connection asked of what a capture may show goes with it.
    let _ = requests.send(FromConnection::Ask(Box::new(Request {
        app: app.clone(),
        workspace,
        launch,
        instance,
        connection,
        reply,
        ask: Ask::Closed,
    })));
    wake();
    tracing::debug!(connection, %app, "shell-ui connection closed");
}

/// The process at the other end of `stream` , as the kernel names it: the app's own process, or the
/// session itself for a window it connected.
fn peer_pid(stream: &UnixStream) -> Option<u32> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).ok()?;
    // SAFETY: both pointers name initialized storage of `length` bytes and
    // the socket stays open for the duration of `getsockopt`.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::from_mut(&mut credentials).cast(),
            std::ptr::from_mut(&mut length),
        )
    };
    if result != 0 || usize::try_from(length).ok()? < std::mem::size_of::<libc::ucred>() {
        return None;
    }
    u32::try_from(credentials.pid).ok().filter(|pid| *pid > 0)
}

/// The workspace the app the connection names runs in and the process the window that speaks for it
/// runs in, when this session started it: the launch the peer's process belongs to, which is the
/// process the window's activity draws in.
fn launched(
    launches: &Launches,
    peer: Option<u32>,
    app: &str,
) -> Option<(String, u32, Option<u64>)> {
    let (pid, instance) = match peer {
        // The session connects a window it opens in an app's running process itself: what it
        // recorded for that connection names the launch and the tab the window takes.
        Some(pid) if pid == std::process::id() => {
            let (started, instance) = launches.take_window_shell_ui(app)?;
            (started, Some(instance))
        }
        peer => (peer?, None),
    };
    launches
        .workspace_of(pid, app)
        .map(|workspace| (workspace, pid, instance))
}

/// Tell an app speaking another shell-ui version which one this session speaks, so the one behind
/// can be updated.
fn refuse_version(writer: &mut UnixStream, app: &str, app_version: Option<u16>) {
    if let Err(error) = write_message(
        writer,
        &FloatingSurfaceHostMessage::VersionMismatch {
            shell: FLOATING_SURFACE_PROTOCOL_VERSION,
        },
    ) {
        tracing::warn!(%error, app, "an app was not told this session speaks another shell-ui version");
    }
    tracing::info!(
        %app,
        ?app_version,
        shell = FLOATING_SURFACE_PROTOCOL_VERSION,
        "shell-ui version mismatch"
    );
}

/// Write one message to an app, as the one-line frame every shell-ui version reads.
fn write_message(
    writer: &mut UnixStream,
    message: &FloatingSurfaceHostMessage,
) -> std::io::Result<()> {
    let mut frame = serde_json::to_vec(message).map_err(std::io::Error::other)?;
    frame.push(b'\n');
    writer.write_all(&frame).and_then(|()| writer.flush())
}

/// A report's box to `to` in the window of connection `about`, with what it carries by default
/// being made: the picture of `window`, the window it is about, now, and the last moments the
/// system's capture kept, written out to be carried when it keeps them. A window held back from
/// captures is pictured as captures picture it.
fn prepare_report(
    to: Recipient,
    about: Option<u64>,
    window: Option<u64>,
    carried: &crate::carried::Carried,
    compositor: &mut Compositor,
    studio: Option<&CaptureStudio>,
) -> Preparing {
    static MADE: AtomicU64 = AtomicU64::new(0);
    let folder = crate::carried::folder();
    if let Err(error) = std::fs::create_dir_all(&folder) {
        tracing::warn!(%error, "a report's files have nowhere to be written");
    }
    let made = MADE.fetch_add(1, Ordering::Relaxed);
    let mut files = Vec::new();
    if let Some(identifier) = window.and_then(|window| compositor.capture_identifier(window)) {
        let pictured = compositor
            .read_back(&tilo_compositor::capture::Target::window(identifier))
            .map_err(|error| error.to_string())
            .and_then(|shot| {
                let png = ui::png::encode_rgba8(shot.width, shot.height, &shot.rgba)
                    .map_err(|error| error.to_string())?;
                let path = folder.join(format!("{made}-picture.png"));
                std::fs::write(&path, png).map_err(|error| error.to_string())?;
                let name = format!("{}.png", crate::i18n::t("session-report-picture"));
                carried.offer(&path, &name, "image/png", (shot.width, shot.height))
            });
        match pictured {
            Ok(file) => files.push(file),
            Err(error) => tracing::warn!(%error, "a report's picture of its window was not taken"),
        }
    }
    let clip = studio.and_then(|studio| {
        let path = folder.join(format!("{made}-recording.mp4"));
        studio
            .clip_for_report(path.clone())
            .map(|written| (path, written))
    });
    Preparing {
        to: vec![to],
        about,
        carried: files,
        clip,
        since: std::time::Instant::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the launcher writes before an app's sandbox starts.
    fn handshake(app: &str) -> Vec<u8> {
        launcher::services::shell_ui_handshake(app, false)
    }

    /// A socket on a path of the test's own, with the channel its asks land on and a wake that does
    /// nothing.
    fn socket_at(path: &std::path::Path) -> (Socket, mpsc::Receiver<FromConnection>) {
        socket_waking(path, Arc::new(|| {}))
    }

    /// [`socket_at`] with `wake` told of each connection that ends, as the session's loop is.
    fn socket_waking(
        path: &std::path::Path,
        wake: tilo_service::Wake,
    ) -> (Socket, mpsc::Receiver<FromConnection>) {
        let (requests, answers) = mpsc::channel();
        let socket = Socket::serve(
            tilo_files::native::socket::bind(path).unwrap(),
            path.to_path_buf(),
            Arc::new(Launches::default()),
            requests,
            wake,
        )
        .unwrap();
        (socket, answers)
    }

    /// A session started while another ran takes the shell-ui path for itself. The one that lost it
    /// ends without a client waking a thread for it, and leaves the socket in place there alone.
    #[test]
    fn a_session_ending_leaves_the_socket_another_one_took() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shell-ui.sock");
        let (first, _first_answers) = socket_at(&path);
        std::fs::remove_file(&path).unwrap();
        let (second, _second_answers) = socket_at(&path);
        // Nothing connects to the socket the first one bound: ending it returns all the same, on
        // the thread that serves it.
        drop(first);
        assert!(path.exists(), "the second session's socket stands");
        drop(second);
        assert!(
            !path.exists(),
            "the socket goes with the session that bound it"
        );
    }

    /// A window that connected before the pass is taken off the socket by it,
    /// and is served from there on.
    #[test]
    fn a_connection_waiting_is_taken_on_the_sessions_pass() {
        use std::io::{Read as _, Write as _};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shell-ui.sock");
        let (socket, answers) = socket_at(&path);
        let mut client = tilo_files::native::socket::connect(&path).unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        socket.take();
        let mut frames = handshake("so.tilo.notes");
        let mut hello = serde_json::to_vec(&FloatingSurfaceClientMessage::Hello {
            version: FLOATING_SURFACE_PROTOCOL_VERSION,
        })
        .unwrap();
        hello.push(b'\n');
        frames.extend_from_slice(&hello);
        client.write_all(&frames).unwrap();
        // An app this session started nothing of is told nothing, and the connection closes.
        let mut rest = Vec::new();
        client.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty(), "nothing is written to that connection");
        // What the loop hears of it is that it is over, never an ask of an app this session never
        // started.
        let arrived = answers
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the loop is told the connection it took is over");
        assert!(
            matches!(arrived, FromConnection::Unadmitted { .. }),
            "no ask stands on a connection this session started nothing of"
        );
    }

    /// A connection the session takes off its socket and admits nothing on ends as one it serves
    /// does: the loop is told it is over and woken for it, so the wait it took the connection in
    /// never goes on for a connection the session is done with.
    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the guard is handed to the variable's wait, which is how it is released while waiting"
    )]
    fn a_connection_no_launch_names_ends_for_the_woken_loop() {
        use std::io::Write as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shell-ui.sock");
        let woken = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let ringing = Arc::clone(&woken);
        let (socket, answers) = socket_waking(
            &path,
            Arc::new(move || {
                let (woken, ready) = &*ringing;
                *woken.lock().unwrap() = true;
                ready.notify_all();
            }),
        );
        let mut client = tilo_files::native::socket::connect(&path).unwrap();
        socket.take();
        let mut frames = handshake("so.tilo.notes");
        let mut hello = serde_json::to_vec(&FloatingSurfaceClientMessage::Hello {
            version: FLOATING_SURFACE_PROTOCOL_VERSION,
        })
        .unwrap();
        hello.push(b'\n');
        frames.extend_from_slice(&hello);
        client.write_all(&frames).unwrap();
        let arrived = answers
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the loop is told the connection it took is over");
        assert!(
            matches!(arrived, FromConnection::Unadmitted { .. }),
            "a connection that named no launch is over and asks nothing"
        );
        let (woken, ready) = &*woken;
        let held = woken.lock().unwrap();
        let (held, waited) = ready
            .wait_timeout_while(held, std::time::Duration::from_secs(10), |woken| !*woken)
            .unwrap();
        assert!(
            !waited.timed_out(),
            "the loop is woken for the connection it took"
        );
        assert!(*held, "the loop is woken for the connection it took");
    }

    /// What the session cannot do of an ask is written back on the app's own connection, naming the
    /// message it sent, so nothing of the app's waits on an answer that never comes.
    #[test]
    fn an_ask_the_session_does_not_answer_is_told_so() {
        use std::io::BufRead as _;

        let (app, session) = UnixStream::pair().unwrap();
        app.set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let message = FloatingSurfaceClientMessage::Embeds {
            embeds: Vec::new(),
            instance: Some(1),
        };
        refuse(
            &Arc::new(Mutex::new(session)),
            "so.tilo.notes",
            &name_of(&message),
            NO_WINDOW_SHARE,
        );
        let mut line = String::new();
        std::io::BufReader::new(&app).read_line(&mut line).unwrap();
        assert_eq!(
            serde_json::from_str::<FloatingSurfaceHostMessage>(line.trim()).unwrap(),
            FloatingSurfaceHostMessage::Unanswered {
                message: "embeds".to_owned(),
                reason: NO_WINDOW_SHARE.to_owned(),
            }
        );
    }

    tilo_test_env::scenario! {
        fn what_an_app_and_its_launcher_send_is_read_as_they_sent_it;

        phase an_ask_is_named_as_the_app_sent_it;
        // An ask is named to the app the way the app sent it, so the reply says which of its messages
        // this session could not answer.
        {
            assert_eq!(
                name_of(&FloatingSurfaceClientMessage::Hello { version: 21 }),
                "hello"
            );
            assert_eq!(
                name_of(&FloatingSurfaceClientMessage::AllowHostShell),
                "allow_host_shell"
            );
            assert_eq!(
                name_of(&FloatingSurfaceClientMessage::PickPictureInPictureArea),
                "pick_picture_in_picture_area"
            );
        }

        phase the_launcher_s_handshake_and_the_app_s_hello_are_read;
        {
            let frame = handshake("so.tilo.notes");
            let parsed: LauncherHandshake = serde_json::from_slice(&frame[..frame.len() - 1]).unwrap();
            assert_eq!(parsed.protocol, SHELL_UI_LAUNCH_PROTOCOL);
            assert_eq!(parsed.app_id, "so.tilo.notes");
            assert!(!parsed.network_status_allowed);
            // What the launcher says of the network reaches the window; one that predates the field
            // leaves it out.
            let allowed: LauncherHandshake = serde_json::from_value(serde_json::json!({
                "protocol": SHELL_UI_LAUNCH_PROTOCOL,
                "app_id": "so.tilo.notes",
                "network_status_allowed": true,
            }))
            .unwrap();
            assert!(allowed.network_status_allowed);
            let old: LauncherHandshake = serde_json::from_value(serde_json::json!({
                "protocol": SHELL_UI_LAUNCH_PROTOCOL,
                "app_id": "so.tilo.notes",
            }))
            .unwrap();
            assert!(!old.network_status_allowed);
            let mut hello = serde_json::to_vec(&FloatingSurfaceClientMessage::Hello {
                version: FLOATING_SURFACE_PROTOCOL_VERSION,
            })
            .unwrap();
            hello.push(b'\n');
            assert_eq!(
                hello_version(&hello[..hello.len() - 1]),
                Some(FLOATING_SURFACE_PROTOCOL_VERSION)
            );
            assert_eq!(
                hello_version(b"{\"type\":\"hello\",\"version\":0}"),
                Some(0)
            );
            assert_eq!(hello_version(b"{\"type\":\"other\"}"), None);
        }
    }
}
