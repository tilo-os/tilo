//! The portal's side of the desktop: the screens and windows it shares,
//! the input a remote desktop drives, and the selection.
//!
//! The portal runs on the desktop services' runtime and the compositor on
//! the session's loop. So the portal's side ([`PortalSide`]: what
//! `ScreenCast`, `RemoteDesktop`, `InputCapture` and the remote clipboards
//! ask through) only sends a request and wakes the loop; the session
//! answers it on its next pass ([`Session::serve`]) with the compositor's
//! frames, its seat and its selection. What the portal publishes, the
//! session feeds: where the screens are, which apps run, when the session
//! locks and which apps exited.

use std::collections::{BTreeMap, HashMap};
use std::io::Read as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_compositor::capture::Target;
use tilo_compositor::{Compositor, InputEvent, ObservedInput, Selection};
use tilo_desktop_model::capability::Capability;
use tilo_desktop_model::layout::TileContent;
use tilo_desktop_surface::DesktopHandle;
use tilo_lock::MutexRecover as _;
use tilo_portal::WorkspaceGrants;
use tilo_portal::clipboard::Clipboard;
use tilo_portal::input_capture::{CaptureZone, CapturedInput, InputCaptures};
use tilo_portal::pipewire::Producer;
use tilo_portal::remote_desktop::{InputInjector, RemoteInput};
use tilo_portal::streams::{
    CaptureSource, CaptureStreams, SourceKind, Stream, StreamRequest, cursor_mode,
};
use tilo_service::Wake;

/// The most of a selection a portal app is handed, as the portal bounds it.
const MAX_CLIPBOARD_BYTES: usize = 4 * 1024 * 1024;
/// The frames a share stream is fed a second.
const FRAMERATE: u32 = 30;
/// A pointer step of a scroll wheel notch, in logical pixels.
const NOTCH: f64 = 15.0;

type Reply<T> = tokio::sync::oneshot::Sender<Result<T, String>>;

/// What the portal asks the session for.
enum Ask {
    Start(StreamRequest, Reply<Stream>),
    Stop(u32),
    Inject(RemoteInput),
    /// The desktop's selection as `mime`.
    Read(String, Reply<Vec<u8>>),
    /// The portal's app owns the selection, offering `mimes`; their bytes
    /// follow through [`Ask::Write`].
    Offer(Vec<String>),
    Write(String, Vec<u8>),
    Clear,
}

/// What both sides share: the session's answers to the portal's questions
/// about itself, which need no round trip.
struct Shared {
    /// The session answered at least once: the compositor is running.
    serving: AtomicBool,
    /// The outputs and windows that can be shared now.
    sources: Mutex<Vec<CaptureSource>>,
    /// Whether the outputs and windows changed since the portal last read
    /// them.
    sources_changed: AtomicBool,
    /// The desktop's selection: how often it changed hands and its types.
    selection: Mutex<(u64, Vec<String>)>,
    /// Wakes the desktop services' watcher when the selection changed.
    selection_changed: tokio::sync::Notify,
    /// The workspace each app runs in, as the desk has it.
    workspaces: Mutex<BTreeMap<String, String>>,
    /// The device scopes each `(workspace, app)` is granted.
    grants: Mutex<BTreeMap<(String, String), Vec<String>>>,
    /// What the portal asked for since the session's last pass.
    asks: Mutex<Vec<Ask>>,
    wake: Wake,
}

impl Shared {
    fn ask(&self, ask: Ask) {
        self.asks.lock_recover().push(ask);
        (self.wake)();
    }
}

/// The portal's side: the handles the backend is given, and what the
/// desktop services watch.
#[derive(Clone)]
pub struct PortalSide {
    shared: Arc<Shared>,
}

impl PortalSide {
    /// What shares the session's screens.
    #[must_use]
    pub fn streams(&self) -> Arc<dyn CaptureStreams> {
        Arc::new(Streams {
            shared: Arc::clone(&self.shared),
        })
    }

    /// What drives the session's seat.
    #[must_use]
    pub fn input(&self) -> Arc<dyn InputInjector> {
        Arc::new(Injection {
            shared: Arc::clone(&self.shared),
        })
    }

    /// What shares the desktop's selection.
    #[must_use]
    pub fn clipboard(&self) -> Arc<dyn Clipboard> {
        Arc::new(ClipboardLink {
            shared: Arc::clone(&self.shared),
        })
    }

    /// The workspace app `app_id` runs in, as the desk knows it.
    #[must_use]
    pub fn workspace_of(&self, app_id: &str) -> Option<String> {
        self.shared.workspaces.lock_recover().get(app_id).cloned()
    }

    /// What the desk's workspaces grant standing in for the person's
    /// answer: a workspace already granting an app what a portal asks of
    /// it needs no sheet.
    #[must_use]
    pub fn grants(&self) -> WorkspaceGrants {
        let shared = Arc::clone(&self.shared);
        Arc::new(
            move |caller: &tilo_portal::PortalCaller, capability: &str| {
                let (Some(workspace), Some(scope)) = (&caller.workspace, desk_scope(capability))
                else {
                    return false;
                };
                let grants = shared.grants.lock_recover();
                grants
                    .get(&(workspace.clone(), caller.app_id.clone()))
                    .is_some_and(|scopes| scopes.iter().any(|granted| granted == scope))
            },
        )
    }

    /// Wake `watch` with the desktop's selection types whenever it changes
    /// hands, so the apps holding a portal clipboard hear of it.
    pub fn follow_selection(&self, watch: impl Fn(Vec<String>) + Send + 'static) {
        let shared = Arc::clone(&self.shared);
        let mut seen = shared.selection.lock_recover().0;
        tokio::spawn(async move {
            loop {
                shared.selection_changed.notified().await;
                let (changes, mimes) = shared.selection.lock_recover().clone();
                if changes == seen {
                    continue;
                }
                seen = changes;
                watch(mimes);
            }
        });
    }
}

/// The session's side: answers what the portal asked for and feeds it what
/// it publishes.
pub struct SessionLink {
    shared: Arc<Shared>,
    producer: Option<Producer>,
    /// The streams feeding a node, by node.
    live: HashMap<u32, Live>,
    /// The selection a portal app offered, by type.
    offered: BTreeMap<String, Vec<u8>>,
    /// Whether the desktop's selection is that offer.
    offering: bool,
    /// The selection change the session made itself: the portal is not
    /// told about its own.
    own_change: Option<u64>,
    /// The inputs the session's screens are fed to.
    captures: Option<InputCaptures>,
    /// When the streams were last fed.
    pushed: Option<Instant>,
}

/// A stream the session feeds: what it shows, and the node it goes to.
struct Live {
    target: Target,
    size: (u32, u32),
}

/// A portal side and the session side that answers it; `wake` wakes the
/// session's loop.
#[must_use]
pub fn channel(wake: Wake) -> (PortalSide, SessionLink) {
    let shared = Arc::new(Shared {
        serving: AtomicBool::new(false),
        sources: Mutex::new(Vec::new()),
        sources_changed: AtomicBool::new(false),
        selection: Mutex::new((0, Vec::new())),
        selection_changed: tokio::sync::Notify::new(),
        workspaces: Mutex::new(BTreeMap::new()),
        grants: Mutex::new(BTreeMap::new()),
        asks: Mutex::new(Vec::new()),
        wake,
    });
    (
        PortalSide {
            shared: Arc::clone(&shared),
        },
        SessionLink {
            shared,
            producer: None,
            live: HashMap::new(),
            offered: BTreeMap::new(),
            offering: false,
            own_change: None,
            captures: None,
            pushed: None,
        },
    )
}

impl SessionLink {
    /// The input captures the session offers its input to, and where its
    /// screens are.
    #[must_use]
    pub fn with_captures(mut self, captures: InputCaptures) -> Self {
        self.captures = Some(captures);
        self
    }

    /// What the compositor offers every input before the desktop sees it:
    /// an input capture takes it while it holds the person's keyboard and
    /// pointer.
    #[must_use]
    pub fn input_observer(&self) -> Arc<dyn Fn(ObservedInput) -> bool + Send + Sync> {
        let Some(captures) = self.captures.clone() else {
            return Arc::new(|_| false);
        };
        Arc::new(move |input| {
            let input = match input {
                ObservedInput::PointerMotion { x, y, dx, dy } => {
                    CapturedInput::PointerMotion { x, y, dx, dy }
                }
                ObservedInput::PointerButton { button, pressed } => {
                    CapturedInput::PointerButton { button, pressed }
                }
                ObservedInput::Scroll { dx, dy } => CapturedInput::Scroll {
                    dx: magnitude(dx),
                    dy: magnitude(dy),
                },
                ObservedInput::Key { keycode, pressed } => {
                    CapturedInput::Keyboard { keycode, pressed }
                }
            };
            captures.observe(input)
        })
    }

    /// Answer everything the portal asked for since the last pass, feed it
    /// what changed, and push the frames of the streams.
    pub fn serve(&mut self, compositor: &mut Compositor, desk: &DesktopHandle) {
        self.shared.serving.store(true, Ordering::Release);
        self.publish_sources(compositor);
        self.publish_desk(desk);
        self.feed_zones(compositor);
        let asks = std::mem::take(&mut *self.shared.asks.lock_recover());
        for ask in asks {
            self.answer(compositor, ask);
        }
        self.push_frames(compositor);
        self.watch_selection(compositor);
    }

    /// Tell the portal where the outputs and windows are, and which can be
    /// shared.
    fn publish_sources(&self, compositor: &Compositor) {
        let mut sources: Vec<CaptureSource> = compositor
            .outputs()
            .into_iter()
            .filter(|output| output.enabled)
            .map(|output| CaptureSource {
                kind: SourceKind::Output,
                id: output.name.clone(),
                app_id: None,
                label: output.description.unwrap_or(output.name),
            })
            .collect();
        sources.extend(
            compositor
                .shareable_windows()
                .into_iter()
                .map(|(id, app_id, title)| CaptureSource {
                    // A window with neither title nor app is named by its
                    // identifier.
                    label: if title.is_empty() {
                        app_id.clone().unwrap_or_else(|| id.clone())
                    } else {
                        title
                    },
                    kind: SourceKind::Window,
                    id,
                    app_id,
                }),
        );
        let changed = {
            let mut shared = self.shared.sources.lock_recover();
            let changed = *shared != sources;
            if changed {
                *shared = sources;
            }
            changed
        };
        if changed {
            self.shared.sources_changed.store(true, Ordering::Release);
        }
    }

    /// Tell the portal which workspace each app runs in, and what those
    /// workspaces grant their apps: the desk's own answer stands for the
    /// person's where it already covers what a portal asks.
    fn publish_desk(&self, desk: &DesktopHandle) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let (workspaces, grants) = desk.read(|tree| {
            let mut workspaces = BTreeMap::new();
            let mut grants = BTreeMap::new();
            for workspace in &tree.workspaces {
                for strip in &workspace.strips {
                    for group in &strip.window_groups {
                        for tab in &group.tabs {
                            let tile = &tab.tile;
                            let (TileContent::Foreign { app_id, .. }
                            | TileContent::LocalApp { app_id, .. }) = tile
                            else {
                                continue;
                            };
                            // A workspace is named to the portal by its
                            // room: the same workspace on every device.
                            workspaces.insert(app_id.clone(), workspace.room().to_string());
                        }
                    }
                }
                for (app_id, capabilities) in workspace.app_grants_snapshot_at(now) {
                    let scopes: Vec<String> = capabilities
                        .iter()
                        .filter_map(Capability::device_scope)
                        .map(str::to_owned)
                        .collect();
                    grants.insert((workspace.room().to_string(), app_id), scopes);
                }
            }
            (workspaces, grants)
        });
        *self.shared.workspaces.lock_recover() = workspaces;
        *self.shared.grants.lock_recover() = grants;
    }

    /// Where the screens are, as input captures see them.
    fn feed_zones(&self, compositor: &Compositor) {
        let Some(captures) = &self.captures else {
            return;
        };
        let zones: Vec<CaptureZone> = compositor
            .outputs()
            .into_iter()
            .filter_map(|output| output.rect)
            .map(|(x, y, width, height)| CaptureZone {
                width: u32::try_from(width.max(0)).unwrap_or(0),
                height: u32::try_from(height.max(0)).unwrap_or(0),
                x,
                y,
            })
            .collect();
        captures.set_zones(zones);
    }

    fn answer(&mut self, compositor: &mut Compositor, ask: Ask) {
        match ask {
            Ask::Start(request, reply) => {
                let _ = reply.send(self.start(compositor, &request));
            }
            Ask::Stop(node) => self.stop(node),
            Ask::Inject(input) => {
                if let Some(event) = self.event(compositor, input)
                    && let Err(error) = compositor.input(event)
                {
                    tracing::debug!(%error, "the portal's input was not delivered");
                }
            }
            Ask::Read(mime, reply) => Self::read_selection(compositor, &mime, reply),
            Ask::Offer(mimes) => {
                self.offered = mimes.into_iter().map(|mime| (mime, Vec::new())).collect();
                self.offering = true;
                self.offer_selection(compositor);
            }
            Ask::Write(mime, bytes) => {
                self.offered.insert(mime, bytes);
                self.offering = true;
                self.offer_selection(compositor);
            }
            Ask::Clear => {
                self.offered.clear();
                self.offering = false;
                self.offer_selection(compositor);
            }
        }
    }

    /// Make the portal's app's offer the desktop's selection.
    fn offer_selection(&mut self, compositor: &mut Compositor) {
        let offer: Vec<(String, Vec<u8>)> = self
            .offered
            .iter()
            .map(|(mime, bytes)| (mime.clone(), bytes.clone()))
            .collect();
        match compositor.set_selection(Selection::Clipboard, offer) {
            Ok(()) => {
                self.own_change = Some(compositor.selection_changes(Selection::Clipboard));
            }
            Err(error) => tracing::warn!(%error, "the portal's selection was not taken"),
        }
    }

    /// The desktop's selection as `mime`, read off the loop: a client
    /// writes its bytes while the session runs.
    fn read_selection(compositor: &mut Compositor, mime: &str, reply: Reply<Vec<u8>>) {
        let reader = match compositor.receive_selection(Selection::Clipboard, mime) {
            Ok(reader) => reader,
            Err(error) => {
                let _ = reply.send(Err(error.to_string()));
                return;
            }
        };
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let read = reader
                .take(u64::try_from(MAX_CLIPBOARD_BYTES).unwrap_or(u64::MAX))
                .read_to_end(&mut bytes);
            let _ = reply.send(match read {
                Ok(_) => Ok(bytes),
                Err(error) => Err(error.to_string()),
            });
        });
    }

    /// Start streaming what `request` names.
    fn start(
        &mut self,
        compositor: &mut Compositor,
        request: &StreamRequest,
    ) -> Result<Stream, String> {
        let target = match request.source.kind {
            SourceKind::Output => Target::output(&request.source.id),
            SourceKind::Window => Target::window(&request.source.id),
        };
        let shot = compositor
            .read_back(&target)
            .map_err(|error| error.to_string())?;
        if self.producer.is_none() {
            self.producer = Some(Producer::new()?);
        }
        let producer = self.producer.as_ref().ok_or("no stream producer")?;
        let node = producer.start(&request.app_id, shot.width, shot.height, FRAMERATE)?;
        let position = match request.source.kind {
            SourceKind::Output => compositor
                .outputs()
                .into_iter()
                .find(|output| output.name == request.source.id)
                .and_then(|output| output.rect)
                .map(|(x, y, _, _)| (x, y)),
            SourceKind::Window => None,
        };
        let size = (shot.width, shot.height);
        self.live.insert(node, Live { target, size });
        self.pushed = None;
        producer.push(node, shot.width, shot.height, shot.rgba.into());
        Ok(Stream {
            node_id: node,
            size,
            position,
        })
    }

    fn stop(&mut self, node: u32) {
        self.live.remove(&node);
        if let Some(producer) = &self.producer {
            producer.stop(node);
        }
    }

    /// Feed every stream the frame the desktop shows, at most
    /// [`FRAMERATE`] a second. A screen being shared is never the lock
    /// screen: nothing is pushed while the session is locked.
    fn push_frames(&mut self, compositor: &mut Compositor) {
        let Some(producer) = &self.producer else {
            return;
        };
        if self.live.is_empty() || compositor.locked() {
            return;
        }
        let interval = Duration::from_secs(1)
            .checked_div(FRAMERATE)
            .unwrap_or(Duration::ZERO);
        let now = Instant::now();
        if self
            .pushed
            .is_some_and(|pushed| now.duration_since(pushed) < interval)
        {
            return;
        }
        self.pushed = Some(now);
        let mut gone = Vec::new();
        for (node, live) in &self.live {
            match compositor.read_back(&live.target) {
                Ok(shot) => producer.push(*node, shot.width, shot.height, shot.rgba.into()),
                // The stream waits for a session that is locked or an
                // output that is off.
                Err(tilo_compositor::Error::Backend(_)) if !compositor.locked() => gone.push(*node),
                Err(_) => {}
            }
        }
        for node in gone {
            tracing::debug!(node, "a shared window is gone: its stream ends");
            self.stop(node);
        }
    }

    /// Tell the desktop services what the desktop's selection is now, so
    /// the apps holding a portal clipboard hear of it.
    fn watch_selection(&mut self, compositor: &Compositor) {
        let changes = compositor.selection_changes(Selection::Clipboard);
        let mimes = compositor.selection_formats(Selection::Clipboard);
        {
            let mut selection = self.shared.selection.lock_recover();
            if selection.0 == changes {
                return;
            }
            *selection = (changes, mimes);
        }
        // The portal's own offer is not news to the portal.
        if self.own_change == Some(changes) {
            self.own_change = None;
            return;
        }
        self.shared.selection_changed.notify_one();
    }

    /// What the seat does for `input`.
    fn event(&self, compositor: &mut Compositor, input: RemoteInput) -> Option<InputEvent> {
        Some(match input {
            RemoteInput::PointerMotion { dx, dy } => InputEvent::PointerMotionRelative { dx, dy },
            RemoteInput::PointerMotionAbsolute { stream, x, y } => {
                let (x, y) = self.absolute(compositor, stream, x, y);
                InputEvent::PointerMotion { x, y }
            }
            RemoteInput::PointerButton { button, pressed } => InputEvent::PointerButton {
                button: u32::try_from(button).unwrap_or(0),
                pressed,
            },
            RemoteInput::PointerAxis { dx, dy } => InputEvent::PointerAxis {
                horizontal: dx,
                vertical: dy,
            },
            RemoteInput::PointerAxisDiscrete { axis, steps } => {
                let distance = f64::from(steps) * NOTCH;
                InputEvent::PointerAxis {
                    horizontal: if axis == 1 { distance } else { 0.0 },
                    vertical: if axis == 1 { 0.0 } else { distance },
                }
            }
            RemoteInput::KeyboardKeycode { keycode, pressed } => InputEvent::Key {
                keycode: u32::try_from(keycode).unwrap_or(0),
                pressed,
            },
            RemoteInput::KeyboardKeysym { keysym, pressed } => {
                let keycode = compositor.keycode_for_keysym(u32::try_from(keysym).ok()?)?;
                InputEvent::Key { keycode, pressed }
            }
        })
    }

    /// Where a point of stream `stream` is in the layout: the stream shows
    /// an output at its place, or a window where it is.
    fn absolute(&self, compositor: &Compositor, stream: u32, x: f64, y: f64) -> (f64, f64) {
        let Some(live) = self.live.get(&stream) else {
            return (x, y);
        };
        let (width, height) = live.size;
        let (width, height) = (f64::from(width.max(1)), f64::from(height.max(1)));
        let place = match &live.target {
            Target::Output { name, .. } => compositor
                .outputs()
                .into_iter()
                .find(|output| &output.name == name)
                .and_then(|output| output.rect),
            Target::Window(identifier) => compositor.shareable_window_rect(identifier),
        };
        let Some((left, top, w, h)) = place else {
            return (x, y);
        };
        // The stream's pixels are the output's, which the layout scales.
        let scale_x = f64::from(w.max(1)) / width;
        let scale_y = f64::from(h.max(1)) / height;
        (f64::from(left) + x * scale_x, f64::from(top) + y * scale_y)
    }
}

/// What the portal's side of a stream asks about the session.
struct Streams {
    shared: Arc<Shared>,
}

impl CaptureStreams for Streams {
    fn sources(&self) -> Vec<CaptureSource> {
        self.shared.sources.lock_recover().clone()
    }

    fn cursor_modes(&self) -> u32 {
        cursor_mode::ALL
    }

    fn start(&self, request: StreamRequest) -> tilo_portal::Pending<Result<Stream, String>> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.shared.ask(Ask::Start(request, reply));
        Box::pin(async move {
            answer
                .await
                .unwrap_or_else(|_| Err("the session ended".to_owned()))
        })
    }

    fn stop(&self, node_id: u32) {
        self.shared.ask(Ask::Stop(node_id));
    }
}

/// What the portal's side of remote input asks of the session.
struct Injection {
    shared: Arc<Shared>,
}

impl InputInjector for Injection {
    fn available(&self) -> bool {
        self.shared.serving.load(Ordering::Acquire)
    }

    fn inject(&self, input: RemoteInput) -> bool {
        if !self.available() {
            return false;
        }
        self.shared.ask(Ask::Inject(input));
        true
    }
}

/// What the portal's side of the clipboard asks of the session.
struct ClipboardLink {
    shared: Arc<Shared>,
}

impl Clipboard for ClipboardLink {
    fn available(&self) -> bool {
        self.shared.serving.load(Ordering::Acquire)
    }

    fn offer(&self, mimes: &[String]) -> Result<(), String> {
        if !self.available() {
            return Err("the session is not running".to_owned());
        }
        self.shared.ask(Ask::Offer(mimes.to_vec()));
        Ok(())
    }

    fn write(&self, mime: &str, bytes: Vec<u8>) -> Result<(), String> {
        if bytes.len() > MAX_CLIPBOARD_BYTES {
            return Err("clipboard payload too large".to_owned());
        }
        if !self.available() {
            return Err("the session is not running".to_owned());
        }
        self.shared.ask(Ask::Write(mime.to_owned(), bytes));
        Ok(())
    }

    fn read(&self, mime: &str) -> tilo_portal::Pending<Result<Vec<u8>, String>> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.shared.ask(Ask::Read(mime.to_owned(), reply));
        Box::pin(async move {
            answer
                .await
                .unwrap_or_else(|_| Err("the session ended".to_owned()))
        })
    }

    fn clear(&self) {
        self.shared.ask(Ask::Clear);
    }
}

/// The desk's name for what a portal asks of an app: reading the screen
/// and driving the seat are one grant each, however they are asked.
fn desk_scope(capability: &str) -> Option<&'static str> {
    match capability {
        "camera" => Some("camera"),
        "microphone" => Some("microphone"),
        "screenshot" | "screencast" => Some("screencast"),
        "input-capture" | "remote-desktop" => Some("remote-desktop"),
        _ => None,
    }
}

/// The size of a scroll as input captures see it.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a scroll of a pixel grid is far from f32's limits"
)]
fn magnitude(value: f64) -> f32 {
    value as f32
}
