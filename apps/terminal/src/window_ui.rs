use std::sync::atomic::AtomicU64;

use ui::hosted_window::HostedWindow;
use ui::window_toolbar::{LocationChoice, ToolbarEvent, ToolbarLocation, WindowToolbar};

#[derive(Clone, Debug, Default)]
pub(crate) enum TerminalWindow {
    #[default]
    None,
    Own,
    Hosted(HostedWindow),
}

#[derive(Default)]
pub(crate) struct WindowLife {
    window: TerminalWindow,
    declared_bar: Option<BarInputs>,
    /// Bumped whenever a session is replaced or dropped, so its threads
    /// never close the window for a shell the tab no longer shows.
    session_generation: Arc<std::sync::atomic::AtomicU64>,
    session_end: Arc<Mutex<Option<SessionEnd>>>,
    reconnect: Option<Reconnect>,
    banner_action: std::cell::Cell<Rect>,
    pub(crate) held_action: std::cell::Cell<Rect>,
    pub(crate) held_secondary: std::cell::Cell<Rect>,
    close_asked: bool,
    retry_rect: Rect,
    restart_action: std::cell::Cell<Rect>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SessionEnd {
    Exited,
    Lost(String),
    Superseded,
    Failed(String),
}

impl SessionEnd {
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
            Failed {
                #[serde(default)]
                reason: String,
            },
        }
        match serde_json::from_slice::<Report>(line) {
            Ok(Report::Lost { reason }) => Self::Lost(reason),
            Ok(Report::Superseded) => Self::Superseded,
            Ok(Report::Failed { reason }) => Self::Failed(reason),
            Ok(Report::Exited) | Err(_) => Self::Exited,
        }
    }
}

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

#[derive(Clone)]
pub(crate) struct SessionExit {
    end: Arc<Mutex<Option<SessionEnd>>>,
    generation: Arc<AtomicU64>,
    mine: u64,
    window: Option<HostedWindow>,
}

impl SessionExit {
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

    #[cfg(test)]
    pub(crate) fn exited(&self) {
        self.ended(SessionEnd::Exited);
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Reconnect {
    pub(crate) reason: String,
    attempt: u32,
    /// When the next automatic attempt is due; `None` while one is under
    /// way, and for a session another terminal took over, which only the
    /// person's Try again takes back.
    at: Option<ui::time::Instant>,
    session: Option<String>,
    pub(crate) superseded: bool,
}

fn reconnect_delay(attempt: u32) -> std::time::Duration {
    let seconds = 2u64.saturating_mul(1u64 << attempt.min(4));
    std::time::Duration::from_secs(seconds.min(30))
}

#[derive(Debug)]
pub(crate) struct BarInputs {
    target: Option<crate::target::SessionTarget>,
    environments: Vec<crate::target::EnvironmentSummary>,
    legacy_vms: Vec<String>,
}

impl BarInputs {
    fn of(sessions: &crate::sessions::Sessions) -> Self {
        Self {
            target: sessions.target.clone(),
            environments: sessions.environments.clone(),
            legacy_vms: sessions.legacy_vms.clone(),
        }
    }

    fn matches(&self, sessions: &crate::sessions::Sessions) -> bool {
        self.target == sessions.target
            && self.environments == sessions.environments
            && self.legacy_vms == sessions.legacy_vms
    }
}

impl TerminalWidget {
    pub fn show_in_window(&mut self, hosted: Option<HostedWindow>) {
        self.sessions.in_window(hosted.clone());
        self.life.window = match hosted {
            Some(window) => TerminalWindow::Hosted(window),
            None => TerminalWindow::Own,
        };
    }

    pub(crate) fn tell_window_session(&self, session: Option<String>) {
        if let TerminalWindow::Hosted(window) = &self.life.window {
            window.set_session(session);
        }
    }

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

    fn end_session_exit(&self) {
        self.life.session_generation.fetch_add(1, Ordering::AcqRel);
    }

    fn session_end(&self) -> Option<SessionEnd> {
        self.life
            .session_end
            .lock()
            .ok()
            .and_then(|end| end.clone())
    }

    pub(crate) fn shell_exited(&self) -> bool {
        self.session_end() == Some(SessionEnd::Exited)
    }

    #[cfg(test)]
    pub(crate) fn reconnecting(&self) -> Option<&Reconnect> {
        self.life.reconnect.as_ref()
    }

    fn begin_reconnect_if_lost(&mut self) {
        let (reason, superseded) = match self.session_end() {
            Some(SessionEnd::Lost(reason)) => (reason, false),
            Some(SessionEnd::Superseded) => (t("terminal-reconnect-superseded"), true),
            Some(SessionEnd::Exited | SessionEnd::Failed(_)) | None => return,
        };
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

    /// The environment could not be set up: the tab says so, with Try
    /// again, in place of the shell; the supervisor's reason goes to the
    /// log, never into the terminal.
    pub(crate) fn show_setup_failure(&mut self) {
        let Some(SessionEnd::Failed(reason)) = self.session_end() else {
            return;
        };
        if let Ok(mut end) = self.life.session_end.lock() {
            *end = None;
        }
        tracing::warn!(%reason, "terminal: the environment could not be set up");
        let message = t("terminal-setup-failed");
        self.show_status(&message);
        self.sessions.state = crate::sessions::OpenState::Failed(message);
        request_terminal_wake(&self.waker);
    }

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
            crate::sessions::OpenState::AwaitingAllow
            | crate::sessions::OpenState::Fixed
            | crate::sessions::OpenState::Joined => {
                self.life.reconnect = None;
            }
        }
    }

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

    pub(crate) fn can_restart(&self) -> bool {
        self.is_disconnected()
            && !self.closing_for_exit()
            && self.life.reconnect.is_none()
            && !matches!(
                self.session_end(),
                Some(SessionEnd::Lost(_) | SessionEnd::Superseded)
            )
            && match self.sessions.state {
                crate::sessions::OpenState::Open | crate::sessions::OpenState::Joined => {
                    self.sessions.target.is_some()
                }
                crate::sessions::OpenState::Fixed => self.respawn.is_some(),
                _ => false,
            }
    }

    pub(crate) fn restart_session(&mut self) -> Result<(), String> {
        if !self.can_restart() {
            return Err(if self.is_disconnected() {
                "this terminal cannot start its shell again".into()
            } else {
                "the shell is still running".into()
            });
        }
        if let Some(command) = self.respawn.clone() {
            let backend = TerminalBackend::with_command(command).map_err(|e| e.to_string())?;
            self.attach_backend(backend);
            self.resize_pty_to_grid();
        } else if let Some(target) = self.sessions.target.clone() {
            self.open_session(&target, Requester::current());
        }
        request_terminal_wake(&self.waker);
        Ok(())
    }

    fn restart_target(&self) -> String {
        format!("terminal.restart:{}", self.pane_id)
    }

    fn route_restart(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) -> bool {
        let target = self.life.restart_action.get();
        if target.is_zero_area() || !self.can_restart() {
            return false;
        }
        let pressed = match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => target.contains(self.last_cursor_pos),
            WindowEvent::Touch(touch) if touch.phase == winit::event::TouchPhase::Started => {
                target.contains(ctx.logical(touch.location))
            }
            _ => false,
        };
        if pressed {
            self.submit_effect(crate::resource::TerminalEffect::Restart);
            *ctx.handled = true;
        }
        pressed
    }

    fn shows_empty_state(&self) -> bool {
        !self.alt_active
            && !self.ever_nonblank
            && !self.closing_for_exit()
            && (self.headless || self.is_disconnected())
    }

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

    fn route_start_failure(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let reconnecting = self.life.reconnect.is_some();
        if self.start_failure().is_none() && !reconnecting {
            return false;
        }
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

    #[cfg(test)]
    pub(crate) fn lifecycle_banner(&self) -> Option<String> {
        self.status_banner_message()
    }

    #[cfg(test)]
    pub(crate) fn content_top_inset(&self) -> f64 {
        self.header_h()
    }

    pub(crate) fn reconnect_banner(&self) -> Option<String> {
        let reconnect = self.life.reconnect.as_ref()?;
        Some(if reconnect.superseded {
            reconnect.reason.clone()
        } else {
            t("terminal-reconnecting")
        })
    }

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
