// Where a tab's session runs, on screen: the target chip in the tab's header,
// the target picker it opens, and the environments panel. Included into the
// widget's module next to `main_body.rs`.

use crate::sessions::OpenState;
use crate::target::{EnvironmentSummary, Location, SessionTarget};

/// One row of the target picker.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TargetMenuRow {
    Open(SessionTarget),
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
    /// Per environment: its rename and open buttons.
    rows: Vec<(String, Rect, Rect)>,
}

fn t(key: &str) -> String {
    ui::i18n::I18nManager::lookup(key, None)
}

/// What the chip and the picker call a target.
fn target_label(target: &SessionTarget, environments: &[EnvironmentSummary]) -> String {
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
                Location::Relay { id } => format!("{name} · {id}"),
            }
        }
        SessionTarget::Legacy { vm } => format!("{vm} · {}", t("terminal-target-legacy")),
    }
}

/// Parse an agent's or a person's spelled-out target: `host`,
/// `environment`, `environment:<record>` or `legacy:<vm>`.
pub fn parse_target_argument(raw: &str) -> Option<SessionTarget> {
    let raw = raw.trim();
    let target = match raw.split_once(':') {
        None if raw == "host" => SessionTarget::Host,
        None if raw == "environment" => SessionTarget::default_environment(),
        Some(("environment", record)) => SessionTarget::environment(record),
        Some(("legacy", vm)) => SessionTarget::Legacy { vm: vm.to_owned() },
        _ => return None,
    };
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

    fn read_recorded_target(state: &AutoCommit) -> Option<SessionTarget> {
        let parent = match state.get(&automerge::ROOT, "terminal_panel") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => automerge::ROOT,
        };
        match state.get(&parent, crate::target::TARGET_KEY) {
            Ok(Some((Value::Scalar(value), _))) => value.as_str().and_then(SessionTarget::decode),
            _ => None,
        }
    }

    fn record_target(&mut self, state: &mut AutoCommit, target: &SessionTarget) {
        self.apply_session_action(
            state,
            &crate::resource::TerminalAction::SetTarget {
                target: target.encode(),
            },
        );
    }

    /// Open the tab's session if it has none yet: on the record's target or,
    /// for a new tab, the default — recorded, so reopening returns to it.
    fn ensure_session(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.sessions.state != OpenState::Unopened {
            self.poll_allow(state, root);
            return;
        }
        let target = if let Some(target) = Self::read_recorded_target(state) {
            target
        } else {
            let target = self.sessions.default_target();
            self.record_target(state, &target);
            target
        };
        self.open_session(state, root, &target, Requester::Person);
    }

    /// Put a session on `target` behind this tab, replacing any it has.
    fn open_session(
        &mut self,
        state: &mut AutoCommit,
        root: &ObjId,
        target: &SessionTarget,
        requested_by: Requester,
    ) {
        self.sessions.target = Some(target.clone());
        match self.sessions.open(target, requested_by) {
            Ok(backend) => {
                self.attach_backend(backend);
                self.resize_pty_to_grid();
                self.sessions.state = OpenState::Open;
                self.sessions.remember(target);
                self.reset_stale_local_session(state, root);
            }
            Err(OpenError::NeedsAllow(_))
                if requested_by == Requester::Person && self.sessions.request_allow() =>
            {
                self.sessions.state = OpenState::AwaitingAllow;
                self.show_status(&t("terminal-host-awaiting-allow"));
            }
            Err(error) => {
                let reason = error.to_string();
                self.show_status(&reason);
                self.sessions.state = OpenState::Failed(reason);
            }
        }
        request_terminal_wake(&self.waker, &self.wake_pending);
    }

    /// The person answered the shell's prompt for the device's own shell.
    fn poll_allow(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.sessions.state != OpenState::AwaitingAllow {
            return;
        }
        match self.sessions.take_allow_answer() {
            Some(true) => {
                if let Some(target) = self.sessions.target.clone() {
                    self.open_session(state, root, &target, Requester::Person);
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

    /// Move this tab to `target`: record it, end the current session, and
    /// open one there.
    ///
    /// # Errors
    /// Refuses the device's own shell for anyone but the person.
    pub(crate) fn switch_target(
        &mut self,
        state: &mut AutoCommit,
        root: &ObjId,
        target: &SessionTarget,
        requested_by: Requester,
    ) -> Result<(), String> {
        if target.is_host() && requested_by == Requester::Agent {
            return Err("agents may not open the device's own shell".into());
        }
        if !self.sessions.is_live() {
            return Err("this terminal does not open sessions".into());
        }
        self.record_target(state, target);
        self.open_session(state, root, target, requested_by);
        match &self.sessions.state {
            OpenState::Failed(reason) => Err(reason.clone()),
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
            .map(|target| target_label(target, &self.sessions.environments))
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
        let pad = f64::from(theme.tokens.spacing.s2);
        let inner = f64::from(theme.tokens.spacing.s3);
        let size = tr
            .renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.xs))
            .size;
        let label = format!("{label}  ▾");
        let max_w = (header.width() - pad * 2.0 - inner * 2.0).max(0.0) as f32;
        let label = tr.elide_to_width(&label, size, max_w);
        let (w, h) = tr.measure(&label, size, None);
        let chip_h = (f64::from(h) + pad).min(header.height() - 4.0).max(0.0);
        let chip = Rect::new(
            header.x0 + pad,
            header.center().y - chip_h / 2.0,
            header.x0 + pad + f64::from(w) + inner * 2.0,
            header.center().y + chip_h / 2.0,
        );
        self.chip_rect = chip;
        let host = self
            .sessions
            .target
            .as_ref()
            .is_some_and(SessionTarget::is_host);
        let (fill, border) = ui::surface::chip_colors(theme, host);
        let fill = if chip.contains(self.last_cursor_pos) && !host {
            theme.tokens.colors.overlay_medium
        } else {
            fill
        };
        ui::surface::pill(scene, transform, chip, fill, border, None);
        tr.draw_text_weighted(
            scene,
            &label,
            size,
            theme.text_primary,
            transform
                * Affine::translate((
                    chip.x0 + inner,
                    chip.y0 + (chip.height() - f64::from(h)) / 2.0,
                )),
            None,
            TextWeight::Medium,
        );
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
        for target in targets {
            let current = self.sessions.target.as_ref() == Some(&target);
            let label = target_label(&target, &self.sessions.environments);
            let label = if current {
                format!("✓ {label}")
            } else {
                label
            };
            menu.push(MenuItem::action(label, || {}));
            rows.push(Some(TargetMenuRow::Open(target)));
        }
        menu.push(MenuItem::separator());
        rows.push(None);
        menu.push(MenuItem::action(t("terminal-target-manage"), || {}));
        rows.push(Some(TargetMenuRow::Manage));
        menu.set_viewport(Some(self.rect));
        let anchor = if self.chip_rect.width() > 0.0 {
            Rect::new(
                self.chip_rect.x0,
                self.chip_rect.y1,
                self.chip_rect.x0,
                self.chip_rect.y1,
            )
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

    /// Act on picker row `index`; `false` when the open menu is not the
    /// picker.
    fn activate_target_menu_item(
        &mut self,
        index: usize,
        state: &mut AutoCommit,
        root: &ObjId,
    ) -> bool {
        let Some(rows) = self.target_menu_rows.take() else {
            return false;
        };
        match rows.get(index).cloned().flatten() {
            Some(TargetMenuRow::Open(target)) => {
                let _ = self.switch_target(state, root, &target, Requester::Person);
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
        let mut input = TextInput::local().with_id("terminal_environment_name");
        input.set_focus(true);
        self.environments_panel = EnvironmentsPanel {
            open: true,
            input: Some(input),
            ..EnvironmentsPanel::default()
        };
    }

    pub fn environments_panel_is_open(&self) -> bool {
        self.environments_panel.open
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
            self.last_cursor_pos =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
            let hit = self
                .environments_panel
                .rows
                .iter()
                .find_map(|(key, rename, open)| {
                    if rename.contains(p) {
                        Some((key.clone(), false))
                    } else if open.contains(p) {
                        Some((key.clone(), true))
                    } else {
                        None
                    }
                });
            if let Some((key, open)) = hit {
                if open {
                    self.close_environments_panel();
                    let target = SessionTarget::environment(key);
                    let _ = self.switch_target(ctx.state, ctx.path, &target, Requester::Person);
                } else {
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
        let scale = f64::from(theme.text_scale).clamp(1.0, 2.0);
        let margin = 12.0 * scale.min(1.25);
        let gap = 8.0 * scale.min(1.25);
        let line = 18.0 * scale;
        let button_h = 28.0 * scale.min(1.25);
        let title_size = 15.0 * scale as f32;
        let body_size = 13.0 * scale as f32;
        let dim_size = 12.0 * scale as f32;
        let row_h = line * 3.0 + gap;
        let envs = self.sessions.environments.clone();
        let rows = envs.len().max(1) as f64;
        let panel_w = (560.0 * scale)
            .min(self.rect.width() - margin * 2.0)
            .max(0.0);
        let panel_h = (button_h + gap * 3.0 + rows * row_h + button_h + gap * 2.0 + line)
            .min(self.rect.height() - margin * 2.0)
            .max(0.0);
        let x0 = self.rect.center().x - panel_w / 2.0;
        let y0 = self.rect.y0 + margin + self.header_h();
        let panel_rect = Rect::new(x0, y0, x0 + panel_w, y0 + panel_h);
        self.environments_panel.rect = panel_rect;
        let shape = RoundedRect::from_rect(panel_rect, f64::from(theme.tokens.radii.md));
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &shape,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_focus,
            None,
            &shape,
        );

        // Title and close.
        tr.draw_text_weighted(
            scene,
            &t("terminal-env-title"),
            title_size,
            theme.text_primary,
            transform * Affine::translate((panel_rect.x0 + margin, panel_rect.y0 + gap)),
            None,
            TextWeight::Semibold,
        );
        let close = Rect::new(
            panel_rect.x1 - gap - button_h,
            panel_rect.y0 + gap,
            panel_rect.x1 - gap,
            panel_rect.y0 + gap + button_h,
        );
        self.environments_panel.close_rect = close;
        draw_icon(
            scene,
            transform,
            close.inset(6.0),
            Icon::X,
            theme.tokens.colors.text_primary,
            1.5,
        );

        // One row per environment.
        let mut y = panel_rect.y0 + gap * 2.0 + button_h;
        let text_w =
            (panel_w - margin * 2.0 - 2.0 * (96.0 * scale.min(1.25) + gap)).max(40.0) as f32;
        self.environments_panel.rows.clear();
        if envs.is_empty() {
            tr.draw_text(
                scene,
                &t("terminal-env-unavailable"),
                body_size,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((panel_rect.x0 + margin, y)),
                Some((panel_w - margin * 2.0) as f32),
            );
            y += row_h;
        }
        for env in &envs {
            if y + row_h > panel_rect.y1 - button_h - gap * 2.0 {
                break;
            }
            let current = self
                .sessions
                .target
                .as_ref()
                .and_then(SessionTarget::environment_record)
                == Some(env.key.as_str());
            let name = if current {
                format!("✓ {}", env.name)
            } else {
                env.name.clone()
            };
            let name = tr.elide_to_width(&name, body_size, text_w);
            tr.draw_text_weighted(
                scene,
                &name,
                body_size,
                theme.text_primary,
                transform * Affine::translate((panel_rect.x0 + margin, y)),
                None,
                TextWeight::Medium,
            );
            let tools = if env.tools.is_empty() {
                t("terminal-env-no-tools")
            } else {
                format!("{}: {}", t("terminal-env-tools"), env.tools.join(", "))
            };
            let grants = if env.grants.is_empty() {
                t("terminal-env-no-grants")
            } else {
                let dirs: Vec<String> = env
                    .grants
                    .iter()
                    .map(|(path, write)| {
                        if *write {
                            path.clone()
                        } else {
                            format!("{path} (ro)")
                        }
                    })
                    .collect();
                format!("{}: {}", t("terminal-env-grants"), dirs.join(", "))
            };
            for (i, text) in [tools, grants].iter().enumerate() {
                let text = tr.elide_to_width(text, dim_size, text_w);
                tr.draw_text(
                    scene,
                    &text,
                    dim_size,
                    theme.tokens.colors.text_dim,
                    transform
                        * Affine::translate((panel_rect.x0 + margin, y + line * (i as f64 + 1.0))),
                    None,
                );
            }
            let bw = 96.0 * scale.min(1.25);
            let open = Rect::new(
                panel_rect.x1 - margin - bw,
                y,
                panel_rect.x1 - margin,
                y + button_h,
            );
            let rename = Rect::new(open.x0 - gap - bw, y, open.x0 - gap, y + button_h);
            for (rect, label) in [
                (rename, t("terminal-env-rename")),
                (open, t("terminal-env-open")),
            ] {
                self.draw_panel_button(scene, transform, tr, theme, rect, &label);
            }
            self.environments_panel
                .rows
                .push((env.key.clone(), rename, open));
            y += row_h;
        }

        // The name field: a new environment, or the one being renamed.
        let field_y = panel_rect.y1 - gap - button_h;
        let submit_w = 96.0 * scale.min(1.25);
        let submit = Rect::new(
            panel_rect.x1 - margin - submit_w,
            field_y,
            panel_rect.x1 - margin,
            field_y + button_h,
        );
        self.environments_panel.submit_rect = submit;
        let submit_label = if self.environments_panel.renaming.is_some() {
            t("terminal-env-rename")
        } else {
            t("terminal-env-create")
        };
        self.draw_panel_button(scene, transform, tr, theme, submit, &submit_label);
        if let Some(error) = self.environments_panel.error.clone() {
            let error = tr.elide_to_width(&error, dim_size, (panel_w - margin * 2.0) as f32);
            tr.draw_text(
                scene,
                &error,
                dim_size,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((panel_rect.x0 + margin, field_y - line)),
                None,
            );
        }
        if let Some(input) = self.environments_panel.input.as_mut() {
            input.apply_layout_external(Rect::new(
                panel_rect.x0 + margin,
                field_y,
                submit.x0 - gap,
                field_y + button_h,
            ));
            input.draw(scene, transform, tr, theme, presence);
            if input.value().is_empty() {
                let r = input.get_rect();
                tr.draw_text(
                    scene,
                    &t("terminal-env-name-placeholder"),
                    dim_size,
                    theme.tokens.colors.text_dim,
                    transform
                        * Affine::translate((
                            r.x0 + 10.0,
                            r.center().y - f64::from(dim_size) / 2.0,
                        )),
                    None,
                );
            }
        }
    }

    fn draw_panel_button(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        label: &str,
    ) {
        let active = rect.contains(self.last_cursor_pos);
        let shape = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.sm));
        scene.fill(
            Fill::NonZero,
            transform,
            if active {
                theme.tokens.colors.overlay_medium
            } else {
                theme.tokens.colors.overlay_subtle
            },
            None,
            &shape,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &shape,
        );
        let size = 12.0_f32;
        let label = tr.elide_to_width(label, size, (rect.width() - 8.0).max(0.0) as f32);
        let (w, h) = tr.measure(&label, size, None);
        tr.draw_text(
            scene,
            &label,
            size,
            theme.text_primary,
            transform
                * Affine::translate((
                    rect.center().x - f64::from(w) / 2.0,
                    rect.center().y - f64::from(h) / 2.0,
                )),
            None,
        );
    }

    // --- agent and palette operations

    /// `terminal.target`: move this session to another target.
    fn answer_target_operation(
        &mut self,
        payload: &[u8],
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<(), String> {
        let raw: String = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
        let target = parse_target_argument(&raw)
            .ok_or_else(|| format!("`{raw}` is not a target: use environment, environment:<record>, legacy:<vm> or host"))?;
        self.switch_target(doc, path, &target, Requester::current())
    }

    /// `terminal.environment`: create or rename an environment.
    fn answer_environment_operation(&mut self, payload: &[u8]) -> Result<(), String> {
        let raw: String = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
        let request: serde_json::Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        let text = |key: &str| {
            request
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        match text("op").as_str() {
            "create" => self.sessions.create_environment(&text("name")).map(|_| ()),
            "rename" => self
                .sessions
                .rename_environment(&text("record"), &text("name")),
            other => Err(format!("unknown environment operation `{other}`")),
        }
    }
}
