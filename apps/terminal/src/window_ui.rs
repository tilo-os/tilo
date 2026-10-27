// The terminal as the app of its window: where its shell runs is the
// window's title in the window bar, with the system picker to switch it; the
// window (or its tab) closes when the shell exits; and a shell that could not
// start says so in the window, with a retry. Included into the widget's
// module next to `main_body.rs` and `target_ui.rs`.

use std::sync::atomic::AtomicU64;

use ui::hosted_window::HostedWindow;
use ui::window_toolbar::{LocationChoice, ToolbarEvent, ToolbarLocation, WindowToolbar};

/// The window this terminal is the app of.
#[derive(Clone, Debug, Default)]
pub(crate) enum TerminalWindow {
    /// None: a terminal inside another app (the code editor's panel). It
    /// keeps its target chip and its ended session in its own content.
    #[default]
    None,
    /// The app of a window in a process of its own: its bar through
    /// [`ui::window_toolbar`], its tab through [`ui::shell_tabs`].
    Own,
    /// The app of a tab the shell shows in its own process, through the
    /// window the shell lent it.
    Hosted(HostedWindow),
}

/// The window a terminal is the app of, and how the end of its session
/// reaches it: closed for an exit, reconnected for a lost connection.
#[derive(Default)]
pub(crate) struct WindowLife {
    /// The window this terminal is the app of: its bar shows where the
    /// session runs, and it closes when the shell exits.
    window: TerminalWindow,
    /// What the slots last declared for the window's bar were made of.
    declared_bar: Option<BarInputs>,
    /// Bumped whenever a session is replaced or dropped, so its threads
    /// never close the window for a shell the tab no longer shows.
    session_generation: Arc<std::sync::atomic::AtomicU64>,
    /// How the current session ended, once it did.
    session_end: Arc<Mutex<Option<SessionEnd>>>,
    /// The connection to the remote shell was lost: the tab reconnects.
    reconnect: Option<Reconnect>,
    /// The reconnecting banner's Try again, as last drawn.
    banner_action: std::cell::Cell<Rect>,
    /// The window was asked to close for the exited shell.
    close_asked: bool,
    /// The retry while the shell could not start, as last drawn.
    retry_rect: Rect,
}

/// How a session ended, as far as the tab can tell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SessionEnd {
    /// Its shell exited: the window closes.
    Exited,
    /// The connection to a remote shell was lost. It may run on there: the
    /// tab says it is reconnecting and re-attaches to it.
    Lost(String),
    /// Another terminal took the remote session over; it runs on there.
    Superseded,
}

impl SessionEnd {
    /// The end a relay session's supervisor reported on its lease
    /// (`{"ended":"exited"}`, `{"ended":"lost","reason":…}`,
    /// `{"ended":"superseded"}`). A report the tab does not understand is
    /// the lease closing, which is the shell exiting.
    pub(crate) fn from_report(line: &[u8]) -> Self {
        #[derive(serde::Deserialize)]
        #[serde(tag = "ended", rename_all = "kebab-case")]
        enum Report {
            Exited,
            Lost {
                #[serde(default)]
                reason: String,
            },
            Superseded,
        }
        match serde_json::from_slice::<Report>(line) {
            Ok(Report::Lost { reason }) => Self::Lost(reason),
            Ok(Report::Superseded) => Self::Superseded,
            Ok(Report::Exited) | Err(_) => Self::Exited,
        }
    }
}

/// The reader thread's view of its session's lease: whether the supervisor
/// still holds the session open, and how it said the session ended.
pub(crate) struct ReaderLease {
    lease: Option<runtime_portal::pending_connection::ReaderLifetime>,
    report: std::cell::RefCell<Option<Vec<u8>>>,
}

impl ReaderLease {
    pub(crate) fn new(lease: Option<runtime_portal::pending_connection::ReaderLifetime>) -> Self {
        Self {
            lease,
            report: std::cell::RefCell::new(None),
        }
    }

    /// Whether the supervisor still holds the session open; a report of
    /// how it ended is kept, never taken for an open session.
    pub(crate) fn open(&self) -> bool {
        if let Some(lease) = &self.lease {
            use runtime_portal::pending_connection::LeaseState;
            return match lease.state() {
                LeaseState::Open => true,
                LeaseState::Closed => false,
                LeaseState::Reported(line) => {
                    self.report.replace(Some(line));
                    false
                }
            };
        }
        false
    }

    /// How the session ended, once its terminal closed: what the supervisor
    /// reported, else (a local shell, or a supervisor that only closes the
    /// lease) the shell exited.
    pub(crate) fn end(&self) -> SessionEnd {
        if self.report.borrow().is_none() {
            let _ = self.open();
        }
        self.report
            .borrow()
            .as_deref()
            .map_or(SessionEnd::Exited, SessionEnd::from_report)
    }
}

/// How a session's own threads say that it ended: the widget sees it on its
/// next sync, and a window the shell lent closes at once when the shell
/// exited, even while the tab is not the one shown. A session the widget
/// replaced or dropped says nothing.
#[derive(Clone)]
pub(crate) struct SessionExit {
    end: Arc<Mutex<Option<SessionEnd>>>,
    generation: Arc<AtomicU64>,
    mine: u64,
    window: Option<HostedWindow>,
}

impl SessionExit {
    /// The session ended so. Only a shell that exited closes its window; a
    /// lost connection reconnects.
    pub(crate) fn ended(&self, end: SessionEnd) {
        let exited = end == SessionEnd::Exited;
        if let Ok(mut slot) = self.end.lock() {
            *slot = Some(end);
        }
        if exited
            && self.generation.load(Ordering::Acquire) == self.mine
            && let Some(window) = &self.window
        {
            window.close();
        }
    }

    /// The shell this session ran exited.
    #[cfg(test)]
    pub(crate) fn exited(&self) {
        self.ended(SessionEnd::Exited);
    }
}

/// The tab lost its connection to a remote shell and reconnects to it.
#[derive(Clone, Debug)]
pub(crate) struct Reconnect {
    /// Why the connection ended.
    pub(crate) reason: String,
    /// Automatic attempts made so far.
    attempt: u32,
    /// When the next automatic attempt is due; `None` while one is under
    /// way, and for a session another terminal took over, which only the
    /// person's Try again takes back.
    at: Option<ui::time::Instant>,
    /// The relay session to re-attach to.
    session: Option<String>,
    /// Another terminal took the session over.
    pub(crate) superseded: bool,
}

/// How long the tab waits before automatic reconnect `attempt` (counting
/// from 0): 2 s, doubling, at most 30 s.
fn reconnect_delay(attempt: u32) -> std::time::Duration {
    let seconds = 2u64.saturating_mul(1u64 << attempt.min(4));
    std::time::Duration::from_secs(seconds.min(30))
}

/// What the window bar's slots were last made of.
#[derive(Debug)]
pub(crate) struct BarInputs {
    target: Option<crate::target::SessionTarget>,
    environments: Vec<crate::target::EnvironmentSummary>,
    relays: Vec<crate::target::RelaySummary>,
    legacy_vms: Vec<String>,
}

impl BarInputs {
    fn of(sessions: &crate::sessions::Sessions) -> Self {
        Self {
            target: sessions.target.clone(),
            environments: sessions.environments.clone(),
            relays: sessions.relays.clone(),
            legacy_vms: sessions.legacy_vms.clone(),
        }
    }

    /// Whether `sessions` still makes the same slots.
    fn matches(&self, sessions: &crate::sessions::Sessions) -> bool {
        self.target == sessions.target
            && self.environments == sessions.environments
            && self.relays == sessions.relays
            && self.legacy_vms == sessions.legacy_vms
    }
}

impl TerminalWidget {
    /// Show this terminal as the app of a window: `hosted` is the window
    /// the shell lent a tab it shows in its own process; without one the
    /// terminal is the app of its own process's window.
    pub fn show_in_window(&mut self, hosted: Option<HostedWindow>) {
        self.life.window = match hosted {
            Some(window) => TerminalWindow::Hosted(window),
            None => TerminalWindow::Own,
        };
    }

    /// Tell the window a desktop shows this terminal in which supervisor
    /// session its tab now runs.
    pub(crate) fn tell_window_session(&self, session: Option<String>) {
        if let TerminalWindow::Hosted(window) = &self.life.window {
            window.set_session(session);
        }
    }

    /// Whether this terminal is the app of a window, which closes when its
    /// shell exits.
    fn owns_window(&self) -> bool {
        !matches!(self.life.window, TerminalWindow::None)
    }

    /// Whether the window bar shows where the session runs, so the content
    /// holds only the terminal.
    pub(crate) fn bar_shows_target(&self) -> bool {
        match &self.life.window {
            TerminalWindow::None => false,
            TerminalWindow::Own => ui::window_toolbar::is_hosted(),
            TerminalWindow::Hosted(_) => true,
        }
    }

    /// The window bar's slots: where the session runs, as the window's
    /// title, with the target picker's rows as its choices.
    pub(crate) fn window_toolbar(&self) -> Option<WindowToolbar> {
        let label = self.chip_label()?;
        let choices = self
            .target_picker_rows()
            .into_iter()
            .map(|row| LocationChoice {
                label: row.label,
                value: row.row.encode(),
                current: row.current,
                separated: row.separated,
            })
            .collect();
        Some(WindowToolbar {
            location: Some(ToolbarLocation {
                label,
                name: t("terminal-target-choose"),
                choices,
                ..ToolbarLocation::default()
            }),
            ..WindowToolbar::default()
        })
    }

    /// Run what the person chose in the window bar's picker, and declare
    /// the bar's slots as they are now.
    fn sync_window_bar(&mut self) {
        let events = match &self.life.window {
            TerminalWindow::None => return,
            TerminalWindow::Own => ui::window_toolbar::take_events(),
            TerminalWindow::Hosted(window) => window.take_events(),
        };
        for event in events {
            if let ToolbarEvent::Commit { text } = event
                && let Some(row) = TargetMenuRow::decode(&text)
            {
                self.choose_picker_row(row);
            }
        }
        // The environments the picker names arrive off the UI thread.
        self.sessions.poll_environment_refresh();
        // The slots are made again only when what they are made of changed.
        if self
            .life
            .declared_bar
            .as_ref()
            .is_some_and(|inputs| inputs.matches(&self.sessions))
        {
            return;
        }
        let toolbar = self.window_toolbar();
        match &self.life.window {
            TerminalWindow::None => {}
            TerminalWindow::Own => ui::window_toolbar::declare(toolbar),
            TerminalWindow::Hosted(window) => window.declare(toolbar),
        }
        self.life.declared_bar = Some(BarInputs::of(&self.sessions));
    }

    /// What a new session's threads say when it ends; earlier sessions say
    /// nothing from now on.
    pub(crate) fn next_session_exit(&mut self) -> SessionExit {
        self.end_session_exit();
        self.life.session_end = Arc::new(Mutex::new(None));
        SessionExit {
            end: Arc::clone(&self.life.session_end),
            generation: Arc::clone(&self.life.session_generation),
            mine: self.life.session_generation.load(Ordering::Acquire),
            window: match &self.life.window {
                TerminalWindow::Hosted(window) => Some(window.clone()),
                TerminalWindow::None | TerminalWindow::Own => None,
            },
        }
    }

    /// The current session is being replaced or dropped: when its shell
    /// ends, that closes nothing.
    fn end_session_exit(&self) {
        self.life.session_generation.fetch_add(1, Ordering::AcqRel);
    }

    /// How the current session ended, if it did.
    fn session_end(&self) -> Option<SessionEnd> {
        self.life
            .session_end
            .lock()
            .ok()
            .and_then(|end| end.clone())
    }

    /// Whether the shell of the current session exited.
    pub(crate) fn shell_exited(&self) -> bool {
        self.session_end() == Some(SessionEnd::Exited)
    }

    /// Whether the tab is reconnecting to its remote shell — for tests.
    #[cfg(test)]
    pub(crate) fn reconnecting(&self) -> Option<&Reconnect> {
        self.life.reconnect.as_ref()
    }

    /// The connection to the remote shell was lost (or another terminal
    /// took it over): the tab says it is reconnecting, keeps what it showed,
    /// and re-attaches to the session — automatically, unless another
    /// terminal has it.
    fn begin_reconnect_if_lost(&mut self) {
        let (reason, superseded) = match self.session_end() {
            Some(SessionEnd::Lost(reason)) => (reason, false),
            Some(SessionEnd::Superseded) => (t("terminal-reconnect-superseded"), true),
            Some(SessionEnd::Exited) | None => return,
        };
        // Seen once: the end belongs to the session the tab now replaces.
        if let Ok(mut end) = self.life.session_end.lock() {
            *end = None;
        }
        self.life.reconnect = Some(Reconnect {
            reason,
            attempt: 0,
            at: (!superseded).then(|| ui::time::now() + reconnect_delay(0)),
            session: self.recorded_relay_session.clone(),
            superseded,
        });
        request_terminal_wake(&self.waker);
    }

    /// Reconnect now when the next automatic attempt is due.
    fn reconnect_when_due(&mut self) {
        if self
            .life
            .reconnect
            .as_ref()
            .and_then(|reconnect| reconnect.at)
            .is_some_and(|at| at <= ui::time::now())
        {
            self.reconnect_now();
        }
    }

    /// Re-attach to the remote session now.
    fn reconnect_now(&mut self) -> bool {
        let Some(target) = self.sessions.target.clone() else {
            return false;
        };
        let Some(reconnect) = self.life.reconnect.as_mut() else {
            return false;
        };
        reconnect.at = None;
        let attach = reconnect.session.clone();
        self.open_session_attaching(&target, Requester::Person, attach.as_deref());
        request_terminal_wake(&self.waker);
        true
    }

    /// A reconnect attempt settled: reconnected, or still under way (a new
    /// session opening in place of one that is gone), or failed — then the
    /// session to re-attach to is kept and the next attempt scheduled.
    fn settle_reconnect(&mut self) {
        let Some(reconnect) = self.life.reconnect.as_mut() else {
            return;
        };
        match &self.sessions.state {
            crate::sessions::OpenState::Open => self.life.reconnect = None,
            crate::sessions::OpenState::Opening | crate::sessions::OpenState::Unopened => {}
            crate::sessions::OpenState::Failed(reason)
            | crate::sessions::OpenState::NoRuntime(reason) => {
                reconnect.reason.clone_from(reason);
                if !reconnect.superseded {
                    reconnect.attempt = reconnect.attempt.saturating_add(1);
                    reconnect.at = Some(ui::time::now() + reconnect_delay(reconnect.attempt));
                }
                let session = reconnect.session.clone();
                self.record_relay_session(session);
            }
            crate::sessions::OpenState::AwaitingAllow | crate::sessions::OpenState::Fixed => {
                self.life.reconnect = None;
            }
        }
    }

    /// When the next automatic reconnect is due.
    pub(crate) fn reconnect_deadline(&self) -> Option<ui::time::Instant> {
        self.life
            .reconnect
            .as_ref()
            .and_then(|reconnect| reconnect.at)
    }

    /// Whether this window closes because its shell exited: it keeps
    /// nothing behind, no ended-session banner and no "no output" panel.
    fn closing_for_exit(&self) -> bool {
        self.owns_window() && self.shell_exited()
    }

    /// The shell exited: close the window's tab (the window with it when it
    /// is the only tab), as every terminal emulator does. Once.
    fn close_if_exited(&mut self) {
        if !self.closing_for_exit() || self.life.close_asked {
            return;
        }
        self.life.close_asked = true;
        match &self.life.window {
            TerminalWindow::None => {}
            TerminalWindow::Own => ui::shell_tabs::close_this(),
            TerminalWindow::Hosted(window) => window.close(),
        }
    }

    /// Why the shell could not start, while the tab shows that: a target
    /// that could not be reached, a supervisor that refused. A target with
    /// no runtime on this device explains itself in the grid instead, with
    /// its own offer of the device's shell. A tab reconnecting to its remote
    /// shell says that instead.
    pub(crate) fn start_failure(&self) -> Option<&str> {
        if self.life.reconnect.is_some() {
            return None;
        }
        match &self.sessions.state {
            crate::sessions::OpenState::Failed(reason) if self.sessions.is_live() => {
                Some(reason.as_str())
            }
            _ => None,
        }
    }

    /// Open the tab's session again, on the target it names; a tab that
    /// lost its remote shell re-attaches to it now.
    pub(crate) fn retry_session(&mut self) -> bool {
        if self.life.reconnect.is_some() {
            return self.reconnect_now();
        }
        if self.start_failure().is_none() {
            return false;
        }
        let Some(target) = self.sessions.target.clone() else {
            return false;
        };
        self.life.retry_rect = Rect::ZERO;
        self.open_session(&target, Requester::Person);
        request_terminal_wake(&self.waker);
        true
    }

    /// A press on the retry, or Enter while the shell could not start,
    /// opens the session again. Returns whether the event was taken.
    fn route_start_failure(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let reconnecting = self.life.reconnect.is_some();
        if self.start_failure().is_none() && !reconnecting {
            return false;
        }
        // The failure's retry, or the reconnecting banner's Try again. While
        // reconnecting the output stays, and keys are the terminal's.
        let target = if reconnecting {
            self.life.banner_action.get()
        } else {
            self.life.retry_rect
        };
        let pressed_retry = match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => target.contains(self.last_cursor_pos),
            WindowEvent::Touch(touch) if touch.phase == winit::event::TouchPhase::Started => {
                target.contains(ctx.logical(touch.location))
            }
            WindowEvent::KeyboardInput { event, .. } if !reconnecting => {
                event.state == ElementState::Pressed
                    && matches!(event.logical_key, Key::Named(NamedKey::Enter))
            }
            _ => false,
        };
        let retried = pressed_retry && self.retry_session();
        if retried {
            *ctx.handled = true;
        }
        retried
    }

    /// What the window shows while the shell could not start: the reason,
    /// plainly, and a retry, in the toolkit's empty state over the grid.
    fn draw_start_failure(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        reason: &str,
    ) {
        let area = self.grid_rect;
        if area.width() <= 0.0 || area.height() <= 0.0 {
            self.life.retry_rect = Rect::ZERO;
            return;
        }
        let title = t("terminal-start-failed-title");
        let retry = t("terminal-start-retry");
        let content = ui::widget::EmptyStateContent {
            icon: Some(Icon::Terminal),
            message: Some(reason),
            action_label: Some(&retry),
            error: true,
            ..ui::widget::EmptyStateContent::new(&title)
        };
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        self.life.retry_rect =
            ui::widget::EmptyState::paint(&mut ctx, area, &content, self.last_cursor_pos)
                .unwrap_or(Rect::ZERO);
    }

    /// The lifecycle line over the grid, if any — for tests.
    #[cfg(test)]
    pub(crate) fn lifecycle_banner(&self) -> Option<String> {
        self.status_banner_message()
    }

    /// How far the terminal's own chrome pushes the grid down — for tests.
    #[cfg(test)]
    pub(crate) fn content_top_inset(&self) -> f64 {
        self.header_h()
    }

    /// The banner over the grid while the tab reconnects to its remote
    /// shell: quiet, with Try again.
    pub(crate) fn reconnect_banner(&self) -> Option<String> {
        let reconnect = self.life.reconnect.as_ref()?;
        Some(if reconnect.superseded {
            reconnect.reason.clone()
        } else {
            t("terminal-reconnecting")
        })
    }

    /// The retry (or, reconnecting, Try again) as assistive technology
    /// reaches it.
    fn start_failure_accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};
        let (key, name, reason, bounds, politeness) = if let Some(banner) = self.reconnect_banner()
        {
            let reason = self
                .life
                .reconnect
                .as_ref()
                .map(|reconnect| reconnect.reason.clone())
                .unwrap_or_default();
            (
                "terminal-reconnecting",
                banner,
                reason,
                self.life.banner_action.get(),
                Politeness::Polite,
            )
        } else {
            (
                "terminal-start-failure",
                t("terminal-start-failed-title"),
                self.start_failure()?.to_owned(),
                self.life.retry_rect,
                Politeness::Assertive,
            )
        };
        let retry = AccessNode::new(Role::Button)
            .with_name(t("terminal-start-retry"))
            .with_action_target(format!("terminal.retry:{}", self.pane_id))
            .with_bounds(bounds)
            .with_state(AccessState::new().focusable(true));
        Some(
            AccessNode::new(Role::Group)
                .with_hashed_key(key)
                .with_name(name)
                .with_description(reason)
                .with_live(politeness)
                .with_children(vec![retry]),
        )
    }
}
