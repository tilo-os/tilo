//! The window of a tilolang app whose instance a tilolang host runs: presentation alone.
//!
//! The session started this process as the app's window (`TILO_TILOLANG_HOSTED`): the launcher
//! gave it no collaboration connection, so it holds and commits none of the app's data. The
//! app's instance runs in the tilolang host of its workspace and author, on the app's own
//! connection, and sends this window component patches over a patch connection the session
//! hands it on its window door (`ui::window_door::take_patch`). The window shows them through
//! the UI host's half (`tilo_ui_host::AppWindow`) and sends back what the person did. Its own
//! launch identity stays for its surface and accessibility.

use std::collections::HashMap;
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
        std::env::var("TILO_CONTINUITY_INSTANCE")
            .ok()
            .and_then(|value| value.parse().ok())
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
        let woke = Arc::new(AtomicBool::new(false));
        let (resume, resumed) = std::sync::mpsc::channel::<()>();
        let flag = Arc::clone(&woke);
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
            .ok()?;
        Some(Self { woke, resume })
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
    /// Shown until the patch connection arrives: the platform's loading state.
    waiting: ui::widget::Element,
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
            waiting: Box::new(ui::widget::Spinner::new()),
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
        let connected = Channel::from_socket(patch).and_then(|channel| {
            let watched = channel.as_fd().try_clone_to_owned()?;
            let transport = ChannelTransport::new(channel)?;
            Ok((transport, watched))
        });
        match connected {
            Ok((transport, watched)) => {
                tilo_startup_metrics::mark("tilolang-patch-connected");
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

    /// What arrived, shown; the watch resumed.
    fn receive(connected: &mut Connected) -> bool {
        let received = connected.view.receive();
        if let Some(watch) = &connected.watch {
            watch.took();
        }
        received
    }

    fn content(&self) -> &dyn Widget {
        match &self.connected {
            Some(connected) => &connected.view,
            None => self.waiting.as_ref(),
        }
    }

    fn content_mut(&mut self) -> &mut dyn Widget {
        match &mut self.connected {
            Some(connected) => &mut connected.view,
            None => self.waiting.as_mut(),
        }
    }
}

impl Widget for HostedWindow {
    fn name(&self) -> &'static str {
        "TilolangHostedWindow"
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.content()]
    }

    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        vec![self.content_mut()]
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if let Some(connected) = &mut self.connected {
            Self::receive(connected);
        }
        self.content_mut().sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let connected_now = self.connect();
        let received = match &mut self.connected {
            Some(connected) => {
                connected.view.flush();
                Self::receive(connected)
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

    fn build_layout(
        &mut self,
        tree: &mut ui::taffy::TaffyTree<()>,
        theme: &ui::Theme,
        nodes: &mut HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        self.content_mut().build_layout(tree, theme, nodes)
    }

    fn apply_layout(
        &mut self,
        tree: &ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        origin: ui::kurbo::Point,
    ) {
        self.content_mut().apply_layout(tree, node, origin);
    }

    fn get_rect(&self) -> ui::kurbo::Rect {
        self.content().get_rect()
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
        self.content_mut()
            .draw(scene, transform, text, theme, presence);
    }

    fn collect_damage(&self, regions: &mut Vec<ui::kurbo::Rect>) {
        self.content().collect_damage(regions);
    }

    fn take_structure_dirty(&mut self) -> bool {
        match &mut self.connected {
            Some(connected) => connected.view.take_structure_dirty(),
            None => self.waiting.take_structure_dirty(),
        }
    }

    fn take_structure_change(&mut self) -> ui::widget::StructureChange {
        match &mut self.connected {
            Some(connected) => connected.view.take_structure_change(),
            None => self.waiting.take_structure_change(),
        }
    }
}
