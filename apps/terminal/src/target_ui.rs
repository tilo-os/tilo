// Where a tab's session runs, on screen: the target chip in the tab's header,
// the target picker it opens, and the environments panel. Included into the
// widget's module next to `main_body.rs`.

use crate::i18n::t;
use crate::sessions::OpenState;
use crate::target::{EnvironmentSummary, Location, RelaySummary, SessionTarget};
use ui::widget::{Button, ButtonFace, ButtonRole, ControlSize, TextInput};

/// One row of the target picker.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TargetMenuRow {
    Open(SessionTarget),
    /// Run the current environment here or on a relay.
    RunOn(Location),
    Manage,
}

/// The environments panel: list, create and rename.
#[derive(Default)]
struct EnvironmentsPanel {
    open: bool,
    input: Option<TextInput>,
    /// The record being renamed; `None` while the input names a new one.
    renaming: Option<String>,
    error: Option<String>,
    rect: Rect,
    close_rect: Rect,
    submit_rect: Rect,
    /// Per environment: its buttons.
    rows: Vec<EnvironmentRow>,
    /// A folder picker the person opened for this environment record.
    picker: Option<(String, ui::portal::PortalChannel)>,
}

/// One environment's buttons in the panel, as last drawn.
struct EnvironmentRow {
    key: String,
    grant: Rect,
    rename: Rect,
    open: Rect,
}

/// What a click on an environment's row asks for.
enum RowAction {
    Grant,
    Rename,
    Open,
}

/// The tools and granted folders of an environment, as the panel shows them.
fn environment_details(env: &EnvironmentSummary) -> (String, String) {
    let tools = if env.tools.is_empty() {
        t("terminal-env-no-tools")
    } else {
        format!("{}: {}", t("terminal-env-tools"), env.tools.join(", "))
    };
    let grants = if env.grants.is_empty() {
        t("terminal-env-no-grants")
    } else {
        let read_only = t("terminal-env-read-only");
        let dirs: Vec<String> = env
            .grants
            .iter()
            .map(|(path, write)| {
                if *write {
                    path.clone()
                } else {
                    format!("{path} ({read_only})")
                }
            })
            .collect();
        format!("{}: {}", t("terminal-env-grants"), dirs.join(", "))
    };
    (tools, grants)
}

/// What the picker calls a place an environment runs.
fn location_label(location: &Location, relays: &[RelaySummary]) -> String {
    match location {
        Location::ThisDevice => t("terminal-run-on-this-device"),
        Location::Relay { relay } => relays
            .iter()
            .find(|r| r.relay == *relay)
            .map_or_else(|| relay.chars().take(8).collect(), |r| r.label.clone()),
    }
}

/// What the chip and the picker call a target.
fn target_label(
    target: &SessionTarget,
    environments: &[EnvironmentSummary],
    relays: &[RelaySummary],
) -> String {
    match target {
        SessionTarget::Host => t("terminal-target-host"),
        SessionTarget::Environment { record, location } => {
            let name = record
                .as_deref()
                .and_then(|key| environments.iter().find(|env| env.key == key))
                .or_else(|| {
                    record.is_none().then(|| {
                        environments
                            .iter()
                            .find(|env| env.key == tilo_environment::DEFAULT_RECORD)
                    })?
                })
                .map_or_else(
                    || match record {
                        None => t("terminal-target-default"),
                        Some(key) => key.clone(),
                    },
                    |env| env.name.clone(),
                );
            match location {
                Location::ThisDevice => name,
                Location::Relay { .. } => format!("{name} on {}", location_label(location, relays)),
            }
        }
        SessionTarget::Legacy { vm } => format!("{vm} ({})", t("terminal-target-legacy")),
    }
}

/// Parse an agent's or a person's spelled-out target: `host`,
/// `environment`, `environment:<record>` or `legacy:<vm>`.
pub fn parse_target_argument(raw: &str) -> Option<SessionTarget> {
    let raw = raw.trim();
    // `…@<relay endpoint id>` runs an environment on that relay.
    let (raw, location) = match raw.split_once('@') {
        Some((base, relay)) if base.starts_with("environment") => (
            base,
            Location::Relay {
                relay: relay.to_ascii_lowercase(),
            },
        ),
        Some(_) => return None,
        None => (raw, Location::ThisDevice),
    };
    let target = match raw.split_once(':') {
        None if raw == "host" => SessionTarget::Host,
        None if raw == "environment" => SessionTarget::default_environment(),
        Some(("environment", record)) => SessionTarget::environment(record),
        Some(("legacy", vm)) => SessionTarget::Legacy { vm: vm.to_owned() },
        _ => return None,
    }
    .at(location);
    target.is_valid().then_some(target)
}

impl TerminalWidget {
    /// Where this tab's session runs, once it has a target.
    pub fn session_target(&self) -> Option<&SessionTarget> {
        self.sessions.target.as_ref()
    }

    /// The pending request for the shell (the device's own shell needs the
    /// person's allow).
    pub fn take_session_intent(&mut self) -> Option<ui::widget::AppHostIntent> {
        self.sessions.take_intent()
    }

    /// Where this tab's session stands.
    pub fn session_state(&self) -> &OpenState {
        &self.sessions.state
    }

    /// Show `target` and `environments` without opening anything — for
    /// screenshot scenes and hosts that render a recorded session.
    pub fn show_target(&mut self, target: SessionTarget, environments: Vec<EnvironmentSummary>) {
        self.sessions.target = Some(target);
        self.sessions.environments = environments;
    }

    /// Report legacy VMs from the legacy VM service; the picker lists them
    /// only while there are any.
    pub fn set_legacy_vms(&mut self, vms: Vec<String>) {
        self.sessions.legacy_vms = vms;
    }

    /// Report the person's relays, for "Run on" — for hosts and screenshot
    /// scenes; a live tab lists them from the supervisor.
    pub fn set_relays(&mut self, relays: Vec<RelaySummary>) {
        self.sessions.relays = relays;
    }

    /// Open sessions through `sessions` — for tests.
    #[cfg(test)]
    pub(crate) fn set_sessions(&mut self, sessions: crate::sessions::Sessions) {
        self.sessions = sessions;
    }

    fn header_h(&self) -> f64 {
        if self.sessions.target.is_some() {
            self.chrome_header_h
        } else {
            0.0
        }
    }

    fn record_target(&mut self, target: &SessionTarget) {
        self.recorded_target = Some(target.clone());
        self.submit(crate::resource::Terminal::RECORD_TARGET.with(target.encode()));
    }

    /// Keep (or forget) the relay session this session re-attaches to.
    fn record_relay_session(&mut self, session: Option<String>) {
        if session.is_none() && self.recorded_relay_session.is_none() {
            return;
        }
        self.recorded_relay_session.clone_from(&session);
        self.submit(crate::resource::Terminal::RECORD_RELAY_SESSION.with(session));
    }

    /// Open the tab's session if it has none yet: on the record's target or,
    /// for a new tab, the default — recorded, so reopening returns to it.
    fn ensure_session(&mut self) {
        if matches!(self.sessions.state, OpenState::NoRuntime(_))
            && std::mem::take(&mut self.sessions.host_requested)
        {
            // Keep the explanation until the person's allowed session replaces it.
            let _ = self.switch_target(&SessionTarget::Host, Requester::Person);
            return;
        }

        if self.sessions.state != OpenState::Unopened {
            self.poll_allow();
            return;
        }
        let target = if let Some(target) = self.recorded_target.clone() {
            target
        } else {
            let target = self.sessions.default_target();
            self.record_target(&target);
            target
        };
        // A session on a relay outlives the tab; reopening re-attaches to it.
        let attach = target
            .location()
            .is_some_and(|location| !location.is_this_device())
            .then(|| self.recorded_relay_session.clone())
            .flatten();
        self.open_session_attaching(&target, Requester::Person, attach.as_deref());
    }

    /// Put a session on `target` behind this tab, replacing any it has.
    fn open_session(&mut self, target: &SessionTarget, requested_by: Requester) {
        self.open_session_attaching(target, requested_by, None);
    }

    /// [`Self::open_session`], re-attaching to the relay session `attach`
    /// when one is named; a session that is gone is replaced by a new one.
    fn open_session_attaching(
        &mut self,
        target: &SessionTarget,
        requested_by: Requester,
        attach: Option<&str>,
    ) {
        self.sessions.target = Some(target.clone());
        // Dropping the platform lifetime also shuts down reader-held clones.
        self.service_lifetime = None;
        if let Some(result) =
            self.sessions
                .begin_open(target, requested_by, attach, Arc::clone(&self.waker))
        {
            self.finish_session_open(target, requested_by, attach, result);
        }
    }

    fn poll_session_open(&mut self) -> bool {
        let Some((requester, attach, result)) = self.sessions.take_open_answer() else {
            return false;
        };
        if let Some(target) = self.sessions.target.clone() {
            self.finish_session_open(&target, requester, attach.as_deref(), result);
        }
        true
    }

    fn finish_session_open(
        &mut self,
        target: &SessionTarget,
        requested_by: Requester,
        attach: Option<&str>,
        result: Result<TerminalBackend, OpenError>,
    ) {
        match result {
            Ok(backend) => {
                self.record_relay_session(backend.relay_session.clone());
                self.attach_backend(backend);
                // A process stands behind the grid from now on.
                self.headless = false;
                self.resize_pty_to_grid();
                self.sessions.state = OpenState::Open;
                self.sessions.remember(target);
            }
            Err(_) if attach.is_some() => {
                self.record_relay_session(None);
                self.open_session_attaching(target, requested_by, None);
                return;
            }
            Err(OpenError::NeedsAllow(_))
                if requested_by == Requester::Person && self.sessions.request_allow() =>
            {
                self.sessions.state = OpenState::AwaitingAllow;
                self.show_status(&t("terminal-host-awaiting-allow"));
            }
            Err(OpenError::NoRuntime(reason)) => {
                tracing::warn!(
                    target = %target.encode(),
                    %reason,
                    "terminal: the environment cannot be built on this device"
                );
                self.explain_no_runtime(&reason, requested_by);
                self.sessions.state = if requested_by == Requester::Person {
                    OpenState::NoRuntime(reason)
                } else {
                    OpenState::Failed(reason)
                };
            }
            Err(error) => {
                let reason = error.to_string();
                self.show_status(&reason);
                self.sessions.state = OpenState::Failed(reason);
            }
        }
        request_terminal_wake(&self.waker);
    }

    /// Say on the tab why its environment cannot start and what to do: the
    /// device's own shell for the person (behind its allow), another target
    /// from the picker, or a runtime to install. Never a blank pane.
    fn explain_no_runtime(&mut self, reason: &str, requested_by: Requester) {
        let mut lines = vec![
            t("terminal-no-runtime-title"),
            reason.to_owned(),
            String::new(),
        ];
        if requested_by == Requester::Person {
            lines.push(t("terminal-no-runtime-host"));
            lines.push(t("terminal-no-runtime-picker"));
        }
        lines.push(t("terminal-no-runtime-install"));
        let mut text = String::from("\x1b[2J\x1b[H");
        for line in lines {
            text.push_str(&line.replace(['\r', '\n'], " "));
            text.push_str("\r\n");
        }
        self.feed(text.as_bytes());
        self.show_status(&if requested_by == Requester::Person {
            t("terminal-no-runtime-status")
        } else {
            reason.to_owned()
        });
    }

    /// Enter on a tab whose environment cannot start takes its offer of the
    /// device's own shell. Returns whether the key was taken.
    fn take_no_runtime_offer(
        &mut self,
        key: &Key,
        modifiers: winit::keyboard::ModifiersState,
    ) -> bool {
        if !matches!(self.sessions.state, OpenState::NoRuntime(_))
            || !matches!(key, Key::Named(NamedKey::Enter))
            || modifiers.control_key()
            || modifiers.alt_key()
            || modifiers.super_key()
        {
            return false;
        }
        self.sessions.host_requested = true;
        request_terminal_wake(&self.waker);
        true
    }

    /// The person answered the shell's prompt for the device's own shell.
    fn poll_allow(&mut self) {
        if self.sessions.state != OpenState::AwaitingAllow {
            return;
        }
        match self.sessions.take_allow_answer() {
            Some(true) => {
                if let Some(target) = self.sessions.target.clone() {
                    self.open_session(&target, Requester::Person);
                }
            }
            Some(false) => {
                let reason = t("terminal-host-denied");
                self.show_status(&reason);
                self.sessions.state = OpenState::Failed(reason);
            }
            None => {}
        }
    }

    /// The authenticated owner selected Host through the trusted shell lifecycle channel.
    /// This follows the ordinary picker path and still waits for supervisor permission.
    pub(crate) fn request_device_shell(&mut self) -> bool {
        self.switch_target(&SessionTarget::Host, Requester::Person)
            .is_ok()
    }

    /// Move this tab to `target`: record it, end the current session, and
    /// open one there.
    ///
    /// # Errors
    /// Refuses the device's own shell for anyone but the person.
    pub(crate) fn switch_target(
        &mut self,
        target: &SessionTarget,
        requested_by: Requester,
    ) -> Result<(), String> {
        if target.is_host() && requested_by == Requester::Agent {
            return Err("agents may not open the device's own shell".into());
        }
        if !self.sessions.is_live() {
            return Err("this terminal does not open sessions".into());
        }
        self.record_target(target);
        self.open_session(target, requested_by);
        match &self.sessions.state {
            OpenState::Failed(reason) | OpenState::NoRuntime(reason) => Err(reason.clone()),
            _ => Ok(()),
        }
    }

    /// Show a lifecycle line on the tab, replacing any earlier one.
    fn show_status(&self, message: &str) {
        self.session_disconnected.store(true, Ordering::Release);
        if let Ok(mut status) = self.session_status_message.lock() {
            message.clone_into(&mut status);
        }
    }

    fn resize_pty_to_grid(&self) {
        if let Ok(master) = self.pty_master.lock() {
            let _ = master.resize(PtySize {
                rows: self.height as u16,
                cols: self.width as u16,
                pixel_width: self.grid_rect.width() as u16,
                pixel_height: self.grid_rect.height() as u16,
            });
        }
    }

    /// Route an event to the target chrome: the environments panel while it
    /// is open, and a click on the chip. Returns whether it was taken.
    fn route_target_chrome(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if self.environments_panel.open {
            self.handle_environments_panel_event(event, ctx);
            return true;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
            && self.chip_rect.contains(self.last_cursor_pos)
        {
            self.open_target_menu();
            *ctx.handled = true;
            return true;
        }
        false
    }

    // --- the chip and the picker

    fn chip_label(&self) -> Option<String> {
        self.sessions
            .target
            .as_ref()
            .map(|target| target_label(target, &self.sessions.environments, &self.sessions.relays))
    }

    fn draw_target_header(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(label) = self.chip_label() else {
            self.chip_rect = Rect::ZERO;
            return;
        };
        let header = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + self.header_h(),
        );
        // The target is a dropdown-like trigger: the shared button face with
        // a chevron, as wide as its label and never wider than the header.
        let size = ui::widget::ControlSize::Small;
        let inset = self.chrome_body_pad;
        let width = ui::widget::ButtonFace::new(Rect::ZERO, &label)
            .icon(Icon::ChevronDown)
            .control_size(size)
            .intrinsic_width(tr, theme)
            .min((header.width() - inset * 2.0).max(0.0));
        let height = ui::widget::ButtonFace::height(size, theme).min(header.height());
        let chip = Rect::new(
            header.x0 + inset,
            header.center().y - height / 2.0,
            header.x0 + inset + width,
            header.center().y + height / 2.0,
        );
        self.chip_rect = chip;
        let face = ui::widget::ButtonFace::new(chip, &label)
            .icon(Icon::ChevronDown)
            .control_size(size)
            .hovered(chip.contains(self.last_cursor_pos))
            .pressed(self.context_menu.is_open() && self.target_menu_rows.is_some());
        ui::widget::Button::paint(scene, transform, tr, theme, &face);
    }

    /// Drop the picker's rows once its menu has closed.
    fn forget_closed_target_menu(&mut self) {
        if !self.context_menu.is_open() {
            self.target_menu_rows = None;
        }
    }

    /// Open the target picker under the chip.
    pub fn open_target_menu(&mut self) {
        self.sessions.refresh_environments();
        let targets = crate::target::picker_targets(
            &self.sessions.environments,
            true,
            &self.sessions.legacy_vms,
        );
        let mut menu = ContextMenu::new();
        menu.set_platform_conventions(self.conventions);
        let mut rows = Vec::with_capacity(targets.len() + 1);
        // Picking another environment keeps it where the current one runs.
        let here = self
            .sessions
            .target
            .as_ref()
            .and_then(SessionTarget::location)
            .cloned();
        for target in targets {
            let target = match here.clone() {
                Some(location) => target.at(location),
                None => target,
            };
            let current = self.sessions.target.as_ref() == Some(&target);
            let label = target_label(&target, &self.sessions.environments, &self.sessions.relays);
            menu.push(MenuItem::action(label, || {}).with_checked(current));
            rows.push(Some(TargetMenuRow::Open(target)));
        }
        // "Run on": this device or one of the person's relays, for the
        // environment the tab runs.
        if let Some(here) = here.filter(|_| !self.sessions.relays.is_empty()) {
            menu.push(MenuItem::separator());
            rows.push(None);
            let places =
                std::iter::once(Location::ThisDevice).chain(self.sessions.relays.iter().map(|r| {
                    Location::Relay {
                        relay: r.relay.clone(),
                    }
                }));
            for place in places {
                let label = format!(
                    "{}{} {}",
                    if place == here { "✓ " } else { "" },
                    t("terminal-run-on"),
                    location_label(&place, &self.sessions.relays)
                );
                menu.push(MenuItem::action(label, || {}));
                rows.push(Some(TargetMenuRow::RunOn(place)));
            }
        }
        menu.push(MenuItem::separator());
        rows.push(None);
        menu.push(MenuItem::action(t("terminal-target-manage"), || {}));
        rows.push(Some(TargetMenuRow::Manage));
        menu.set_viewport(Some(self.rect));
        let anchor = if self.chip_rect.width() > 0.0 {
            self.chip_rect
        } else {
            let at = Point::new(
                self.rect.x0 + self.chrome_body_pad,
                self.rect.y0 + self.header_h(),
            );
            Rect::from_origin_size(at, (0.0, 0.0))
        };
        menu.open_anchored(anchor, self.rect);
        self.context_menu = menu;
        self.target_menu_rows = Some(rows);
    }

    /// The picker's rows, in order: `None` for a separator — for tests and
    /// accessibility.
    pub fn target_menu_labels(&self) -> Option<Vec<String>> {
        self.target_menu_rows.as_ref()?;
        Some(
            self.context_menu
                .items()
                .iter()
                .filter_map(MenuItem::label)
                .map(str::to_owned)
                .collect(),
        )
    }

    /// Which of the picker's rows (as [`Self::target_menu_labels`] lists
    /// them) carry the check of the current target — for tests.
    #[cfg(test)]
    pub(crate) fn target_menu_checked(&self) -> Option<Vec<bool>> {
        self.target_menu_rows.as_ref()?;
        Some(
            self.context_menu
                .items()
                .iter()
                .filter(|item| item.label().is_some())
                .map(|item| item.checked().unwrap_or(false))
                .collect(),
        )
    }

    /// Act on picker row `index`; `false` when the open menu is not the
    /// picker.
    fn activate_target_menu_item(&mut self, index: usize) -> bool {
        let Some(rows) = self.target_menu_rows.take() else {
            return false;
        };
        match rows.get(index).cloned().flatten() {
            Some(TargetMenuRow::Open(target)) => {
                let _ = self.switch_target(&target, Requester::Person);
            }
            Some(TargetMenuRow::RunOn(location)) => {
                if let Some(target) = self.sessions.target.clone() {
                    let _ = self.switch_target(&target.at(location), Requester::Person);
                }
            }
            Some(TargetMenuRow::Manage) => self.open_environments_panel(),
            None => {}
        }
        true
    }

    // --- the environments panel

    /// Open the environments panel.
    pub fn open_environments_panel(&mut self) {
        self.sessions.refresh_environments();
        let placeholder = t("terminal-env-name-placeholder");
        let mut input = TextInput::local()
            .with_id("terminal_environment_name")
            .with_placeholder(placeholder.clone())
            .with_accessible_name(placeholder);
        input.set_focus(true);
        self.environments_panel = EnvironmentsPanel {
            open: true,
            input: Some(input),
            ..EnvironmentsPanel::default()
        };
    }

    fn start_rename(&mut self, key: String) {
        let name = self
            .sessions
            .environments
            .iter()
            .find(|env| env.key == key)
            .map(|env| env.name.clone())
            .unwrap_or_default();
        self.environments_panel.renaming = Some(key);
        if let Some(input) = self.environments_panel.input.as_mut() {
            input.set_text(&name);
            input.set_focus(true);
        }
    }

    /// Ask the person for a folder to grant the environment `record`,
    /// through the same folder picker every app uses.
    fn pick_folder_to_grant(&mut self, record: String) {
        if self.environments_panel.picker.is_some() {
            return;
        }
        let channel = ui::portal::PickRequest::directory(
            &t("terminal-env-grant-title"),
            &t("terminal-env-grant-accept"),
        )
        .thread_name("terminal-grant-folder-picker")
        .send();
        self.environments_panel.picker = Some((record, channel));
    }

    /// Grant the folder the person picked, once they have.
    fn poll_folder_picker(&mut self) {
        let Some((record, channel)) = self.environments_panel.picker.as_ref() else {
            return;
        };
        let Ok(outcome) = channel.try_recv() else {
            return;
        };
        let record = record.clone();
        self.environments_panel.picker = None;
        match outcome {
            ui::portal::PortalOutcome::Opened(path) => {
                let path = path.display().to_string();
                self.environments_panel.error =
                    self.sessions.grant_folder(&record, &path, true).err();
            }
            ui::portal::PortalOutcome::Cancelled => {}
            ui::portal::PortalOutcome::Failed(reason) => {
                self.environments_panel.error = Some(reason);
            }
        }
    }

    /// Whether a folder picker is waiting for the person.
    fn folder_picker_pending(&self) -> bool {
        self.environments_panel.picker.is_some()
    }

    fn close_environments_panel(&mut self) {
        self.environments_panel = EnvironmentsPanel::default();
    }

    /// Type into the panel's name field — for hosts and screenshot scenes.
    pub fn set_environment_name_input(&mut self, text: &str) {
        if let Some(input) = self.environments_panel.input.as_mut() {
            input.set_text(text);
        }
    }

    /// Create a new environment, or finish renaming one, from the name field.
    fn submit_environment_name(&mut self) {
        let name = self
            .environments_panel
            .input
            .as_ref()
            .map(|input| input.value().trim().to_owned())
            .unwrap_or_default();
        let result = match self.environments_panel.renaming.clone() {
            Some(record) => self.sessions.rename_environment(&record, &name),
            None => self.sessions.create_environment(&name).map(|_| ()),
        };
        match result {
            Ok(()) => {
                self.environments_panel.renaming = None;
                self.environments_panel.error = None;
                if let Some(input) = self.environments_panel.input.as_mut() {
                    input.set_text("");
                }
            }
            Err(reason) => self.environments_panel.error = Some(reason),
        }
    }

    fn handle_environments_panel_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor_pos = ctx.logical(position);
        }
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            match logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.close_environments_panel();
                    *ctx.handled = true;
                    return;
                }
                Key::Named(NamedKey::Enter) => {
                    self.submit_environment_name();
                    *ctx.handled = true;
                    return;
                }
                _ => {}
            }
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            let p = self.last_cursor_pos;
            if self.environments_panel.close_rect.contains(p) {
                self.close_environments_panel();
                *ctx.handled = true;
                return;
            }
            if self.environments_panel.submit_rect.contains(p) {
                self.submit_environment_name();
                *ctx.handled = true;
                return;
            }
            let hit = self.environments_panel.rows.iter().find_map(|row| {
                if row.grant.contains(p) {
                    Some((row.key.clone(), RowAction::Grant))
                } else if row.rename.contains(p) {
                    Some((row.key.clone(), RowAction::Rename))
                } else if row.open.contains(p) {
                    Some((row.key.clone(), RowAction::Open))
                } else {
                    None
                }
            });
            if let Some((key, action)) = hit {
                match action {
                    RowAction::Open => {
                        self.close_environments_panel();
                        let target = SessionTarget::environment(key);
                        let _ = self.switch_target(&target, Requester::Person);
                    }
                    RowAction::Rename => self.start_rename(key),
                    RowAction::Grant => self.pick_folder_to_grant(key),
                }
                *ctx.handled = true;
                return;
            }
        }
        if let Some(input) = self.environments_panel.input.as_mut() {
            let mut input_handled = false;
            Widget::handle_event(
                input,
                event,
                &mut ui::widget::EventCtx {
                    handled: &mut input_handled,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
        }
        // The panel is a focus boundary, like find: nothing reaches the shell.
        *ctx.handled = true;
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one panel: header, a row per environment, and the name field"
    )]
    fn draw_environments_panel(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if !self.environments_panel.open {
            return;
        }
        let spacing = &theme.tokens.spacing;
        let metrics = &theme.tokens.metrics;
        let margin = f64::from(spacing.s4);
        let gap = f64::from(spacing.s2);
        let size = ControlSize::Small;
        let button_h = ButtonFace::height(size, theme);
        let title_style = theme.text(TextRole::Subheading);
        let name_style = theme.text(TextRole::Label).strong();
        let caption = theme.text(TextRole::Caption);
        // Text on the panel is inked for the panel's own ground.
        let ink = theme.on_ground(ui::chrome::panel_ground(theme));
        let envs = self.sessions.environments.clone();
        let top = self.rect.y0 + self.header_h();
        let panel_w = (f64::from(metrics.panel_width_sheet) * theme.responsive_scale())
            .min(self.rect.width() - margin * 2.0)
            .max(0.0);
        // Everything an environment has is shown in full: the lists wrap to
        // the panel's width rather than being cut off.
        let text_w = (panel_w - margin * 2.0).max(1.0) as f32;
        let (_, caption_line) = tr.measure_style("Ag", None, &caption);
        let caption_line = f64::from(caption_line);
        let details: Vec<(String, String)> = envs.iter().map(environment_details).collect();
        let row_heights: Vec<f64> = details
            .iter()
            .map(|(tools, grants)| {
                let (_, tools_h) = tr.measure_style(tools, Some(text_w), &caption);
                let (_, grants_h) = tr.measure_style(grants, Some(text_w), &caption);
                button_h + gap / 2.0 + f64::from(tools_h) + f64::from(grants_h) + gap * 2.0
            })
            .collect();
        let unavailable = t("terminal-env-unavailable");
        let rows_h = if envs.is_empty() {
            let (_, h) = tr.measure_style(&unavailable, Some(text_w), &caption);
            f64::from(h) + gap * 2.0
        } else {
            row_heights.iter().sum()
        };
        let panel_h = (button_h + gap * 3.0 + rows_h + button_h + gap * 2.0 + caption_line)
            .min(self.rect.height() - margin * 2.0 - self.header_h())
            .max(0.0);
        let x0 = self.rect.center().x - panel_w / 2.0;
        let y0 = top + margin;
        let panel_rect = Rect::new(x0, y0, x0 + panel_w, y0 + panel_h);
        self.environments_panel.rect = panel_rect;
        ui::widget::floating_surface::paint(scene, transform, panel_rect, theme);
        let p = self.last_cursor_pos;

        // Title and close.
        let close = Rect::new(
            panel_rect.x1 - gap - button_h,
            panel_rect.y0 + gap,
            panel_rect.x1 - gap,
            panel_rect.y0 + gap + button_h,
        );
        self.environments_panel.close_rect = close;
        let title = t("terminal-env-title");
        let (_, title_h) = tr.measure_style(&title, None, &title_style);
        tr.draw_style(
            scene,
            &title,
            ink.text_primary,
            transform
                * Affine::translate((
                    panel_rect.x0 + margin,
                    close.center().y - f64::from(title_h) / 2.0,
                )),
            None,
            &title_style,
        );
        Button::paint(
            scene,
            transform,
            tr,
            theme,
            &ButtonFace::icon_only(close, Icon::X, "Close")
                .control_size(size)
                .hovered(close.contains(p)),
        );

        // One row per environment; its buttons are as wide as their labels.
        let row_labels = [
            t("terminal-env-grant"),
            t("terminal-env-rename"),
            t("terminal-env-open"),
        ];
        let [grant_w, rename_w, open_w] = row_labels.each_ref().map(|label| {
            ButtonFace::new(Rect::ZERO, label)
                .control_size(size)
                .intrinsic_width(tr, theme)
        });
        let mut y = panel_rect.y0 + gap * 2.0 + button_h;
        self.environments_panel.rows.clear();
        if envs.is_empty() {
            tr.draw_style(
                scene,
                &unavailable,
                ink.text_dim,
                transform * Affine::translate((panel_rect.x0 + margin, y)),
                Some(text_w),
                &caption,
            );
        }
        for ((env, (tools, grants)), row_h) in envs.iter().zip(&details).zip(&row_heights) {
            if y + row_h > panel_rect.y1 - button_h - gap * 2.0 {
                break;
            }
            let open = Rect::new(
                panel_rect.x1 - margin - open_w,
                y,
                panel_rect.x1 - margin,
                y + button_h,
            );
            let rename = Rect::new(open.x0 - gap - rename_w, y, open.x0 - gap, y + button_h);
            let grant = Rect::new(rename.x0 - gap - grant_w, y, rename.x0 - gap, y + button_h);
            let current = self
                .sessions
                .target
                .as_ref()
                .and_then(SessionTarget::environment_record)
                == Some(env.key.as_str());
            let band = Rect::new(panel_rect.x0 + margin, y, grant.x0 - gap, y + button_h);
            let mut name_x = band.x0;
            if current {
                // The environment this tab runs in carries the check a menu
                // gives its applied row.
                let mark_side = f64::from(metrics.icon_sm);
                let mark = Rect::from_center_size(
                    Point::new(name_x + mark_side / 2.0, band.center().y),
                    (mark_side, mark_side),
                );
                Icon::Check.draw(scene, transform, mark, ink.text_primary);
                name_x += mark_side + f64::from(metrics.icon_label_gap);
            }
            let name_w = (band.x1 - name_x).max(0.0) as f32;
            let name = tr.elide_to_width_style(&env.name, name_w, &name_style);
            let (_, name_h) = tr.measure_style(&name, None, &name_style);
            tr.draw_style(
                scene,
                &name,
                ink.text_primary,
                transform * Affine::translate((name_x, band.center().y - f64::from(name_h) / 2.0)),
                None,
                &name_style,
            );
            let mut text_y = y + button_h + gap / 2.0;
            for text in [tools, grants] {
                tr.draw_style(
                    scene,
                    text,
                    ink.text_dim,
                    transform * Affine::translate((panel_rect.x0 + margin, text_y)),
                    Some(text_w),
                    &caption,
                );
                let (_, h) = tr.measure_style(text, Some(text_w), &caption);
                text_y += f64::from(h);
            }
            for (rect, label) in [grant, rename, open].into_iter().zip(&row_labels) {
                Button::paint(
                    scene,
                    transform,
                    tr,
                    theme,
                    &ButtonFace::new(rect, label)
                        .control_size(size)
                        .hovered(rect.contains(p)),
                );
            }
            self.environments_panel.rows.push(EnvironmentRow {
                key: env.key.clone(),
                grant,
                rename,
                open,
            });
            y += row_h;
        }

        // The name field: a new environment, or the one being renamed. Making
        // or renaming one is the panel's main action.
        let field_y = panel_rect.y1 - gap - button_h;
        let submit_label = if self.environments_panel.renaming.is_some() {
            t("terminal-env-rename")
        } else {
            t("terminal-env-create")
        };
        let submit_face = ButtonFace::new(Rect::ZERO, &submit_label)
            .role(ButtonRole::Primary)
            .control_size(size);
        let submit_w = submit_face.intrinsic_width(tr, theme);
        let submit = Rect::new(
            panel_rect.x1 - margin - submit_w,
            field_y,
            panel_rect.x1 - margin,
            field_y + button_h,
        );
        self.environments_panel.submit_rect = submit;
        Button::paint(
            scene,
            transform,
            tr,
            theme,
            &ButtonFace::new(submit, &submit_label)
                .role(ButtonRole::Primary)
                .control_size(size)
                .hovered(submit.contains(p)),
        );
        let error = self.environments_panel.error.clone();
        if let Some(error) = &error {
            let error = tr.elide_to_width_style(error, text_w, &caption);
            tr.draw_style(
                scene,
                &error,
                theme.tokens.colors.semantic.danger,
                transform
                    * Affine::translate((
                        panel_rect.x0 + margin,
                        field_y - gap / 2.0 - caption_line,
                    )),
                None,
                &caption,
            );
        }
        if let Some(input) = self.environments_panel.input.as_mut() {
            input.set_invalid(error.is_some());
            input.apply_layout_external(Rect::new(
                panel_rect.x0 + margin,
                field_y,
                submit.x0 - gap,
                field_y + button_h,
            ));
            input.draw(scene, transform, tr, theme, presence);
        }
    }

    // --- agent and palette operations

    /// `open-in`: move this session to another target.
    fn open_in(&mut self, raw: &str) -> Result<(), String> {
        let target = parse_target_argument(raw).ok_or_else(|| {
            format!("`{raw}` is not a target: use environment, environment:<record>, legacy:<vm> or host")
        })?;
        self.switch_target(&target, Requester::current())
    }
}
