//! The window of a tilolang app whose instance a tilolang host runs: presentation alone.
//!
//! The session started this process as the app's window (`TILO_TILOLANG_HOSTED`): the launcher
//! gave it no collaboration connection, so it holds and commits none of the app's data. The
//! app's instance runs in the tilolang host of its workspace and author, on the app's own
//! connection, and sends this window component patches over a patch connection the session
//! hands it on its window door (`ui::window_door::take_patch`). The window shows them through
//! the UI host's half (`tilo_ui_host::AppWindow`) and sends back what the person did. Its own
//! launch identity stays for its surface and accessibility.

use std::os::fd::{AsRawFd as _, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_render_protocol::channel::Channel;
use tilo_render_protocol::patch::unix::ChannelTransport;
use tilo_ui_host::AppWindow;
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
struct Watch {
    /// Set by the thread when it woke the window; the window resumes it after reading.
    woke: Arc<AtomicBool>,
    resume: std::sync::mpsc::Sender<()>,
}

impl Watch {
    fn start(fd: OwnedFd, wake: ui::wake::WakeHandle) -> Option<Self> {
        let signalled = Arc::new(AtomicBool::new(false));
        let (resume, resumed) = std::sync::mpsc::channel::<()>();
        let flag = Arc::clone(&signalled);
        std::thread::Builder::new()
            .name("tilolang-patches".into())
            .spawn(move || {
                loop {
                    let mut poll = libc::pollfd {
                        fd: fd.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY: one initialized pollfd naming a descriptor this thread owns.
                    let ready = unsafe { libc::poll(&raw mut poll, 1, -1) };
                    if ready < 0 {
                        if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                        {
                            continue;
                        }
                        return;
                    }
                    flag.store(true, Ordering::Release);
                    wake.wake();
                    // The host hung up: the window keeps its last frame.
                    if poll.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
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
        })
    }

    /// The window took what arrived: wait for more.
    fn took(&self) {
        if self.woke.swap(false, Ordering::AcqRel) {
            let _ = self.resume.send(());
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
    told: Option<(String, Vec<(String, String)>)>,
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
                self.take_channel(channel)
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
        let mut window = Self {
            instance: None,
            connected: None,
            tree_drawn: false,
            drawn: false,
            activation: std::cell::Cell::new(false),
            waiting: Box::new(ui::widget::Spinner::new()),
            told: None,
        };
        window.take_channel(channel);
        window
    }

    /// The window's half of the UI host, once its patch connection arrived.
    #[must_use]
    pub fn view(&self) -> Option<&AppWindow> {
        self.connected.as_ref().map(|connected| &connected.view)
    }

    fn take_channel(&mut self, channel: Channel) -> bool {
        let connected = channel
            .as_fd()
            .try_clone_to_owned()
            .and_then(|watched| Ok((ChannelTransport::new(channel)?, watched)));
        match connected {
            Ok((transport, watched)) => {
                let watch = Watch::start(watched, ui::wake::WakeHandle::new());
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

impl ui::component_lifecycle::WindowActivation for HostedWindow {
    fn take_window_activation_intent(&self) -> bool {
        self.activation.replace(false)
    }
}

/// The window stands for what it shows (the UI host's window, or the loading state before it)
/// in the layout and in every walk of the widgets: it adds no node of its own.
impl ui::widget::Decorator for HostedWindow {
    type Inner = dyn Widget;

    ui::provides!(dyn ui::component_lifecycle::WindowActivation);

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
                let (me, others) = said.clone();
                connected
                    .view
                    .tell(tilo_render_protocol::patch::ToApp::Names { me, others });
                connected.view.flush();
                self.told = Some(said);
            }
        }
        self.content_mut()
            .draw(scene, transform, text, theme, presence);
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
