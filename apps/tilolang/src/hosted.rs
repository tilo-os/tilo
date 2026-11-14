//! The window of a tilolang app whose instance a tilolang host runs: presentation alone.
//!
//! The session started this process as the app's window (`TILO_TILOLANG_HOSTED`): the launcher
//! gave it no collaboration connection, so it holds and commits none of the app's data. The
//! app's instance runs in the tilolang host of its workspace and author, on the app's own
//! connection, and sends this window component patches over a patch connection the session
//! hands it on its window door (`ui::window_door::take_patch`). The window shows them through
//! the UI host's half (`tilo_ui_host::AppWindow`) and sends back what the person did. Its own
//! launch identity stays for its surface and accessibility.
//!
//! Without a session (`tilolang dev`, a capture), there is no host to run the instance: the
//! window runs one in its own process ([`start`]), over the app's record on this device alone,
//! and turns it on its own frames. The instance runs the same either way.

use std::cell::RefCell;
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use tilo_render_protocol::channel::Channel;
use tilo_render_protocol::patch::unix::ChannelTransport;
use tilo_ui_host::AppWindow;
use tilolang_host::TilolangHost;
use tilolang_host::hosted::{Hosted, Kept, Spec};
use tilolang_host::phrases::Phrases;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::instance::record::{InstanceRecord, RecordError};
use ui::widget::DrawChild;
use ui::widget::{AppModel, ModelPath, Widget};

/// The variable the launcher sets for a window whose instance a tilolang host runs.
pub const HOSTED_ENV: &str = "TILO_TILOLANG_HOSTED";

/// Whether this process is the window of an instance a tilolang host runs.
#[must_use]
pub fn is_hosted() -> bool {
    static HOSTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HOSTED.get_or_init(|| std::env::var(HOSTED_ENV).is_ok_and(|value| value == "1"))
}

/// The instance of the window being built: the one the shell asked the running process for, or
/// the process's first window's.
#[must_use]
pub fn window_instance() -> Option<u64> {
    ui::window_door::requested_instance().or_else(|| {
        // Unset: the process was not launched for a window of its own.
        let value = std::env::var("TILO_CONTINUITY_INSTANCE").ok()?;
        match value.parse() {
            Ok(instance) => Some(instance),
            Err(error) => {
                tracing::error!(
                    value,
                    %error,
                    "the session named the window's instance as something that is not one"
                );
                None
            }
        }
    })
}

/// Wakes the window whenever its patch connection has something, and only then: a thread waits
/// on the connection, wakes the window once, and waits again only after the window took what
/// arrived. Idle, nothing runs.
///
/// It wakes the component that runs the window now. The window is made by the component shown
/// before the record ([`ui::framework::Opened`]) and taken over by the one the record opens,
/// which closes the first: a wake captured once would go to a closed component, and the window
/// would show what its instance sent only when something else woke it. Every pass of the
/// window points the watch at the component running it ([`Watch::follow`]).
struct Watch {
    /// Set by the thread when it woke the window; the window resumes it after reading.
    woke: Arc<AtomicBool>,
    resume: std::sync::mpsc::Sender<()>,
    /// What the thread wakes: the component running the window.
    wake: Arc<Mutex<ui::wake::WakeHandle>>,
    /// Closed when the window goes: the thread stops waiting on the connection, and lets go of
    /// the descriptor it holds, so the instance sees its window close and ends.
    _stop: std::io::PipeWriter,
}

impl Watch {
    fn start(fd: OwnedFd, wake: Arc<Mutex<ui::wake::WakeHandle>>) -> Option<Self> {
        let signalled = Arc::new(AtomicBool::new(false));
        let (resume, resumed) = std::sync::mpsc::channel::<()>();
        let flag = Arc::clone(&signalled);
        let waking = Arc::clone(&wake);
        let (stopped, stop) = std::io::pipe().ok()?;
        std::thread::Builder::new()
            .name("tilolang-patches".into())
            .spawn(move || {
                loop {
                    let mut poll = [
                        libc::pollfd {
                            fd: stopped.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        },
                        libc::pollfd {
                            fd: fd.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        },
                    ];
                    // SAFETY: two initialized pollfds naming descriptors this thread owns.
                    let ready = unsafe { libc::poll(poll.as_mut_ptr(), 2, -1) };
                    if ready < 0 {
                        if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                        {
                            continue;
                        }
                        return;
                    }
                    // The window went: what the thread holds of the connection goes with it.
                    if poll[0].revents != 0 {
                        return;
                    }
                    flag.store(true, Ordering::Release);
                    waking
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .wake();
                    // The host hung up: the window keeps its last frame.
                    if poll[1].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                        return;
                    }
                    if resumed.recv().is_err() {
                        return;
                    }
                }
            })
            .inspect_err(|error| {
                tracing::error!(
                    %error,
                    "no thread watches the window's patch connection: it shows what arrives \
                     only when something else wakes it"
                );
            })
            .ok()?;
        Some(Self {
            woke: signalled,
            resume,
            wake,
            _stop: stop,
        })
    }

    /// Wake the component running the window from now on: the one whose pass this is, when it
    /// is another than the one the watch wakes.
    fn follow(&self) {
        let running = ui::wake::WakeHandle::new();
        // A pass outside any component's scope names none to wake.
        if !running.same_open_scope(&running) {
            return;
        }
        let mut wake = self
            .wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !wake.same_open_scope(&running) {
            *wake = running;
        }
    }

    /// The window took what arrived: wait for more.
    fn took(&self) {
        if self.woke.swap(false, Ordering::AcqRel) {
            let _ = self.resume.send(());
        }
    }
}

/// What says a host has work for its next turn: its instances' connections and the work they
/// do off the frame wake it through [`Self::waker`], and its runner waits on it.
#[derive(Default)]
pub struct Woken {
    woken: Mutex<bool>,
    signal: Condvar,
}

impl Woken {
    /// What wakes the host: marks it woken.
    pub fn waker(self: &Arc<Self>) -> tilolang_tilo::background::Waker {
        self.waker_and(|| {})
    }

    /// [`Self::waker`], and `then` after each wake: the window the host runs in, woken too.
    pub fn waker_and(
        self: &Arc<Self>,
        then: impl Fn() + Send + Sync + 'static,
    ) -> tilolang_tilo::background::Waker {
        let woken = Arc::clone(self);
        Arc::new(move || {
            woken.mark();
            then();
        })
    }

    fn mark(&self) {
        *self
            .woken
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        self.signal.notify_all();
    }

    /// Whether the host was woken since this was last asked.
    pub fn take(&self) -> bool {
        std::mem::take(
            &mut *self
                .woken
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Wait until the host is woken, `timeout` at most.
    pub fn wait(&self, timeout: std::time::Duration) {
        let _ = self.signal.wait_timeout_while(
            self.woken
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            timeout,
            |woken| !*woken,
        );
    }
}

/// A tilolang host in this process running the one instance `spec` of `bundle`, over the record
/// `record` opens where the host keeps the app's data, its words read by `phrases`, woken by
/// `wake`; the host keeps what it keeps under `storage` (in memory without). Answers the host
/// and the window's end of the instance's patch connection: what a window shows it through
/// ([`HostedWindow::over`]). The host is the one a session runs, and turns as it does.
///
/// # Errors
/// The app's data cannot be kept, its record does not open, or the patch connection cannot be
/// made.
pub fn start(
    spec: Spec,
    record: impl FnOnce(&Kept) -> Result<InstanceRecord, RecordError>,
    bundle: Arc<Bundle>,
    phrases: &Rc<Phrases>,
    storage: Option<std::path::PathBuf>,
    wake: tilolang_tilo::background::Waker,
) -> Result<(TilolangHost, Channel), StartError> {
    let mut host = TilolangHost::new(storage, Arc::clone(&wake));
    let kept = host.kept(&spec.app)?;
    let record = record(&kept)?;
    let (ours, theirs) = Channel::pair()?;
    let watched = ours.as_fd().try_clone_to_owned()?;
    let hosted = Hosted::with_record(
        spec,
        record,
        bundle,
        &kept,
        Box::new(ChannelTransport::new(ours)?),
        phrases,
        host.formats(),
        wake,
    );
    host.add(hosted, Some(watched));
    Ok((host, theirs))
}

/// Why [`start`] ran no instance.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Record(#[from] RecordError),
}

/// The longest a capture waits for the instance it runs to show what it stages.
const STAGE_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// The host a window runs in its own process ([`start`]), and what wakes it.
struct InProcess {
    host: TilolangHost,
    /// The app the instance runs.
    app: String,
    woken: Arc<Woken>,
    /// What a search hit or an agent's `open` asked the window to show, for its instance.
    reveals: Rc<RefCell<Vec<String>>>,
    /// The person's standing in the workspace as the window's host keeps it, read each turn,
    /// and what the instance was last told of it: it offers and invokes by that.
    standing: Option<StandingSource>,
    told_standing: Option<ui::permissions::Standing>,
}

/// Where a window running its instance in this process reads the person's standing from.
type StandingSource = Box<dyn Fn() -> Option<ui::permissions::Standing>>;

impl InProcess {
    /// The host's turns until it has nothing more to take in now. Whether anything changed.
    fn turn(&mut self) -> bool {
        if let Some(source) = &self.standing {
            let standing = source();
            if standing != self.told_standing {
                self.host.set_standing(&self.app, standing.as_ref());
                self.told_standing = standing;
            }
        }
        for anchor in self.reveals.borrow_mut().drain(..) {
            if let Some(hosted) = self.host.instances_mut().next() {
                hosted.reveal(anchor);
            }
        }
        let mut changed = false;
        // A turn's work can be what the next one takes in (a change committed, then answered).
        for _ in 0..16 {
            self.woken.take();
            if !self.host.turn() {
                break;
            }
            changed = true;
        }
        changed
    }

    /// The host's turns until it owes nothing: what a capture stages is all shown.
    fn settle(&mut self) {
        let began = std::time::Instant::now();
        while began.elapsed() < STAGE_WAIT {
            if self.turn() {
                continue;
            }
            if !self.host.owes_work() {
                return;
            }
            self.woken.wait(STAGE_WAIT.saturating_sub(began.elapsed()));
        }
        tracing::error!(owed = ?self.host.owed(), "the instance did not settle for the capture");
    }

    /// Close the instance as a session's host does when its window goes: its record is written
    /// to disk.
    fn close(mut self) {
        let began = std::time::Instant::now();
        while !self.host.is_empty() && began.elapsed() < STAGE_WAIT {
            if !self.host.turn() {
                self.woken.wait(std::time::Duration::from_millis(50));
            }
        }
    }
}

/// The window, once its patch connection arrived, and what wakes it.
struct Connected {
    view: AppWindow,
    watch: Option<Watch>,
}

/// The window. See the module docs.
pub struct HostedWindow {
    instance: Option<u64>,
    connected: Option<Connected>,
    /// Whether a frame showing the instance's tree was drawn yet.
    tree_drawn: bool,
    /// Whether any frame was drawn yet.
    drawn: bool,
    /// The instance asked for the window to come forward ("Show this window").
    activation: std::cell::Cell<bool>,
    /// Shown until the patch connection arrives: the platform's loading state.
    waiting: ui::widget::Element,
    /// What the window last told the instance it calls people (`ToApp::Names`).
    told: Option<crate::names::Said>,
    /// The host running the window's instance in this process, without a session.
    in_process: Option<InProcess>,
}

impl HostedWindow {
    /// The window of the instance `instance`.
    #[must_use]
    pub fn new(instance: Option<u64>) -> Self {
        if instance.is_none() {
            tracing::error!("a hosted tilolang window has no instance");
        }
        let mut window = Self {
            instance,
            connected: None,
            tree_drawn: false,
            drawn: false,
            activation: std::cell::Cell::new(false),
            waiting: Box::new(ui::widget::Spinner::new()),
            told: None,
            in_process: None,
        };
        window.connect();
        window
    }

    /// Take the patch connection, once the shell sent it.
    fn connect(&mut self) -> bool {
        if self.connected.is_some() {
            return false;
        }
        let Some(instance) = self.instance else {
            return false;
        };
        let Some(patch) = ui::window_door::take_patch(instance, ui::wake::WakeHandle::new()) else {
            return false;
        };
        match Channel::from_socket(patch) {
            Ok(channel) => {
                tilo_startup_metrics::mark("tilolang-patch-connected");
                self.take_channel(channel, Arc::new(Mutex::new(ui::wake::WakeHandle::new())))
            }
            Err(error) => {
                tracing::error!(%error, "a hosted tilolang window's patch connection is unusable");
                false
            }
        }
    }

    /// The window over the patch connection `channel`, whose other end its instance's host
    /// holds: what the session hands it on the window door.
    #[must_use]
    pub fn over(channel: Channel) -> Self {
        let mut window = Self::unconnected();
        window.take_channel(channel, Arc::new(Mutex::new(ui::wake::WakeHandle::new())));
        window
    }

    fn unconnected() -> Self {
        Self {
            instance: None,
            connected: None,
            tree_drawn: false,
            drawn: false,
            activation: std::cell::Cell::new(false),
            waiting: Box::new(ui::widget::Spinner::new()),
            told: None,
            in_process: None,
        }
    }

    /// The window running its instance in this process ([`start`]): `spec` of `bundle`, over the
    /// record `record` opens, the host keeping the app's data under `storage`. The window turns
    /// the host on its frames, and is woken when the host's work off the frame is ready.
    ///
    /// # Errors
    /// As [`start`].
    pub fn in_process(
        spec: Spec,
        record: impl FnOnce(&Kept) -> Result<InstanceRecord, RecordError>,
        bundle: Arc<Bundle>,
        storage: Option<std::path::PathBuf>,
    ) -> Result<Self, StartError> {
        let wake = Arc::new(Mutex::new(ui::wake::WakeHandle::new()));
        let woken = Arc::new(Woken::default());
        let waking = Arc::clone(&wake);
        let waker = woken.waker_and(move || {
            waking
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .wake();
        });
        let phrases = Rc::new(Phrases::from_env());
        let app = spec.app.clone();
        let (host, channel) = start(spec, record, bundle, &phrases, storage, waker)?;
        let mut window = Self::unconnected();
        window.take_channel(channel, wake);
        window.in_process = Some(InProcess {
            host,
            app,
            woken,
            reveals: Rc::default(),
            standing: None,
            told_standing: None,
        });
        Ok(window)
    }

    /// Have the instance running in this process offer and invoke by the person's standing in
    /// the workspace, as `source` reads it when the window turns: what a session tells a hosted
    /// window on its launch and whenever the workspace's policy changes.
    pub fn follow_standing(
        &mut self,
        source: impl Fn() -> Option<ui::permissions::Standing> + 'static,
    ) {
        if let Some(running) = &mut self.in_process {
            running.standing = Some(Box::new(source));
        }
    }

    /// Where a search hit or an agent's `open` asks the window to show what it names: its
    /// instance shows it on the next frame. `None` for a window whose instance runs elsewhere.
    #[must_use]
    pub fn reveals(&self) -> Option<Rc<RefCell<Vec<String>>>> {
        self.in_process
            .as_ref()
            .map(|running| Rc::clone(&running.reveals))
    }

    /// Keep the types of the package with an origin in step with `origin`'s service, in a window
    /// whose instance runs in this process.
    pub fn add_origin(&mut self, origin: tilolang_tilo::origin::SharedOrigin) {
        if let Some(hosted) = self
            .in_process
            .as_mut()
            .and_then(|running| running.host.instances_mut().next())
        {
            hosted.add_origin(origin);
        }
    }

    /// The window's half of the UI host, once its patch connection arrived.
    #[must_use]
    pub fn view(&self) -> Option<&AppWindow> {
        self.connected.as_ref().map(|connected| &connected.view)
    }

    fn take_channel(&mut self, channel: Channel, wake: Arc<Mutex<ui::wake::WakeHandle>>) -> bool {
        let connected = channel
            .as_fd()
            .try_clone_to_owned()
            .and_then(|watched| Ok((ChannelTransport::new(channel)?, watched)));
        match connected {
            Ok((transport, watched)) => {
                let watch = Watch::start(watched, wake);
                self.connected = Some(Connected {
                    view: AppWindow::new(Box::new(transport), Vec::new()),
                    watch,
                });
                true
            }
            Err(error) => {
                tracing::error!(%error, "a hosted tilolang window's patch connection is unusable");
                false
            }
        }
    }

    /// What arrived, shown; the watch resumed. A request to come forward is kept for the
    /// runner, which brings the window forward.
    fn receive(connected: &mut Connected, activation: &std::cell::Cell<bool>) -> bool {
        if let Some(watch) = &connected.watch {
            watch.follow();
        }
        let received = connected.view.receive();
        if connected.view.take_activation() {
            activation.set(true);
        }
        if let Some(watch) = &connected.watch {
            watch.took();
        }
        received
    }

    fn content(&self) -> &(dyn Widget + 'static) {
        match &self.connected {
            Some(connected) => &connected.view,
            None => self.waiting.as_ref(),
        }
    }

    fn content_mut(&mut self) -> &mut (dyn Widget + 'static) {
        match &mut self.connected {
            Some(connected) => &mut connected.view,
            None => self.waiting.as_mut(),
        }
    }
}

impl Drop for HostedWindow {
    /// A window running its instance in this process closes it as a session's host does: the
    /// patch connection goes first, and the instance ends, its record written to disk.
    fn drop(&mut self) {
        if let Some(running) = self.in_process.take() {
            self.connected = None;
            running.close();
        }
    }
}

/// A window running its instance in this process is the framework's surface of the app: what a
/// capture stages runs in the instance, and is shown once the instance settled.
impl ui::framework::ResourceSurface<crate::resource::Workspace> for HostedWindow {
    fn present(&mut self, _state: &Rc<crate::resource::WorkspaceState>) {}

    fn stage(&mut self, scenario: Option<&str>) {
        let Some(running) = &mut self.in_process else {
            return;
        };
        if let Some(scenario) = scenario
            && let Some(hosted) = running.host.instances_mut().next()
        {
            hosted.stage(scenario);
        }
        running.settle();
    }
}

impl ui::component_lifecycle::WindowActivation for HostedWindow {
    fn take_window_activation_intent(&self) -> bool {
        self.activation.replace(false)
    }
}

/// The window's undo and redo (Ctrl+Z, Ctrl+Shift+Z, Edit › Undo) are the record's, and the
/// record is the instance's: they go to the instance, which takes back the person's last step
/// there, typing into a field included, and sends the window what it shows then. The window
/// holds no record, so no undo of its own could take anything back; and a field keeps no
/// history beside the record's.
impl ui::undo::LocalUndo for HostedWindow {
    fn handle_local_undo_request(&mut self, request: ui::undo::UndoRequest) -> bool {
        let Some(connected) = &mut self.connected else {
            // Nothing shown yet: nothing of the person's to take back.
            return false;
        };
        // What the person typed before asking goes first: the undo takes it back too.
        connected.view.flush();
        connected.view.tell(match request {
            ui::undo::UndoRequest::Undo => tilo_render_protocol::patch::ToApp::Undo,
            ui::undo::UndoRequest::Redo => tilo_render_protocol::patch::ToApp::Redo,
        });
        connected.view.flush();
        true
    }
}

/// A change mark in the window asks the shell to show the task run that made the change.
impl ui::component_lifecycle::HostIntents for HostedWindow {
    fn take_host_intent(&mut self) -> Option<ui::widget::AppHostIntent> {
        let connected = self.connected.as_mut()?;
        let run = connected.view.take_shown_run()?;
        Some(ui::widget::AppHostIntent::ShowTaskRun { group: run })
    }
}

/// The window is named by what it shows: the item it shows as its page, else the page, the record,
/// the place its instance's composed shell names in its content header. Two windows of the app
/// read apart in the desktop's tabs, the switcher and assistive technology without either being
/// opened.
impl ui::component_lifecycle::WindowTitle for HostedWindow {
    fn window_title(&self) -> Option<&str> {
        self.view().and_then(tilo_ui_host::AppWindow::title)
    }
}

/// The window stands for what it shows (the UI host's window, or the loading state before it)
/// in the layout and in every walk of the widgets: it adds no node of its own.
impl ui::widget::Decorator for HostedWindow {
    type Inner = dyn Widget;

    /// What the window itself answers, and everything else the widget it shows: the UI host's
    /// window publishes the app's actions for the command palette and runs the one the person
    /// chooses ([`tilo_ui_host::catalogue`]), so a request for either reaches it through here.
    fn provide<'a>(&'a self, request: &mut ui::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn ui::component_lifecycle::WindowActivation>() {
            request.provide::<dyn ui::component_lifecycle::WindowActivation>(self);
            return;
        }
        if request.wants::<dyn ui::component_lifecycle::WindowTitle>() {
            request.provide::<dyn ui::component_lifecycle::WindowTitle>(self);
            return;
        }
        if request.wants::<dyn ui::component_lifecycle::HostIntents>() {
            request.provide::<dyn ui::component_lifecycle::HostIntents>(self);
            return;
        }
        if request.wants::<dyn ui::undo::LocalUndo>() {
            request.provide::<dyn ui::undo::LocalUndo>(self);
            return;
        }
        self.content().provide(request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut ui::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn ui::component_lifecycle::WindowActivation>() {
            request.provide::<dyn ui::component_lifecycle::WindowActivation>(self);
            return;
        }
        if request.wants::<dyn ui::component_lifecycle::WindowTitle>() {
            request.provide::<dyn ui::component_lifecycle::WindowTitle>(self);
            return;
        }
        if request.wants::<dyn ui::component_lifecycle::HostIntents>() {
            request.provide::<dyn ui::component_lifecycle::HostIntents>(self);
            return;
        }
        if request.wants::<dyn ui::undo::LocalUndo>() {
            request.provide::<dyn ui::undo::LocalUndo>(self);
            return;
        }
        self.content_mut().provide_mut(request);
    }

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content_mut()
    }

    fn name(&self) -> &'static str {
        "TilolangHostedWindow"
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if let Some(connected) = &mut self.connected {
            Self::receive(connected, &self.activation);
        }
        self.content_mut().sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let connected_now = self.connect();
        let received = match &mut self.connected {
            Some(connected) => {
                connected.view.flush();
                // What the person did runs before what it shows is read.
                if let Some(running) = &mut self.in_process {
                    running.turn();
                }
                Self::receive(connected, &self.activation)
            }
            None => false,
        };
        let child = self.content_mut().update(state, path);
        connected_now || received || child
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.content_mut().handle_event(event, ctx);
        // What the person did goes to the instance at once, not on the next frame.
        if let Some(connected) = &mut self.connected {
            connected.view.flush();
        }
    }

    fn draw(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: ui::kurbo::Affine,
        text: &mut ui::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        // What the window's first frames show, for its launch timing: the instance's own
        // tree, or the loading state before it arrived.
        let showing_tree = self
            .connected
            .as_ref()
            .is_some_and(|connected| connected.view.composed().is_some());
        if showing_tree && !self.tree_drawn {
            self.tree_drawn = true;
            tilo_startup_metrics::mark("tilolang-first-tree-drawn");
        } else if !showing_tree && !self.drawn {
            tilo_startup_metrics::mark("tilolang-first-draw-before-tree");
        }
        self.drawn = true;
        // The instance runs in the host, which knows people only by id: it names them as the
        // window does, told whenever that changes.
        if let Some(connected) = &mut self.connected {
            let said = crate::names::said(presence);
            if self.told.as_ref() != Some(&said) {
                let crate::names::Said {
                    me,
                    others,
                    persons,
                } = said.clone();
                connected
                    .view
                    .tell(tilo_render_protocol::patch::ToApp::Names {
                        me,
                        others,
                        persons,
                    });
                connected.view.flush();
                self.told = Some(said);
            }
        }
        self.content_mut()
            .draw_child(scene, transform, text, theme, presence);
    }

    fn needs_idle_sync(&self) -> bool {
        self.in_process
            .as_ref()
            .is_some_and(|running| running.host.owes_work())
            || self.content().needs_idle_sync()
    }

    fn has_pending_visual_work(&self) -> bool {
        self.in_process
            .as_ref()
            .is_some_and(|running| running.host.owes_work())
            || self.content().has_pending_visual_work()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let host = self
            .in_process
            .as_ref()
            .and_then(|running| running.host.wakes_in())
            .and_then(|wait| ui::time::now().checked_add(wait));
        match (host, self.content().next_wake_deadline()) {
            (Some(host), Some(content)) => Some(host.min(content)),
            (host, content) => host.or(content),
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        match &mut self.connected {
            Some(connected) => Widget::take_structure_dirty(&mut connected.view),
            None => self.waiting.take_structure_dirty(),
        }
    }

    fn take_structure_change(&mut self) -> ui::widget::StructureChange {
        match &mut self.connected {
            Some(connected) => Widget::take_structure_change(&mut connected.view),
            None => self.waiting.take_structure_change(),
        }
    }
}
