//! Session power modal: state, layout, paint, animation, and click policy.

use std::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};

use ui::text_render::TextRenderer;
use ui::theme::Theme;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};

use crate::shell::i18n::t;
use crate::shell::overlay_host::layers::{
    LayerCx, LayerGeometry, LayerId, LayerIntent, LayerPaint, Routed, ShellLayer,
};
use ui::TextRole;
use ui::paint::PaintCtx;
use ui::widget::Interaction;
use ui::widget::tooltip::{
    TooltipHost, TooltipSide, chip_rect_on, chip_size_in_viewport, draw_chip,
};
use ui::widget::{ListRowFace, RowLeading};

/// A Panel opens and closes over the motion token's `base` (HIG §9.7). The
/// default token: `open`/`close` have no theme in hand, and the menu is
/// compositor chrome that does not follow per-app motion overrides.
fn panel_open_duration() -> Duration {
    Duration::from_millis(u64::from(ui::tokens::Durations::default().base_ms))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PowerMenuClick {
    Ignored,
    Consumed,
    /// Locking is handled by the native compositor's `SessionManager`, not
    /// by the external session-action helper.  That keeps the lock scene and
    /// client-input cutoff in one authoritative lifecycle.
    Lock,
    Execute(os_actions::SessionAction),
}

#[derive(Clone, Copy)]
enum DetailInput {
    None,
    Keyboard,
    Pointer(Point),
}

pub(crate) struct PowerMenu {
    open: bool,
    transition: ui::Transition,
    selected: usize,
    detail_tip: TooltipHost,
    detail_rect: Option<Rect>,
    detail_input: DetailInput,
    /// Theme-resolved row geometry from the last frame, shared by paint,
    /// pointer targets and accessibility.
    resolved_metrics: PowerMenuMetrics,
    /// Whether the display manager can open a greeter beside this session.
    /// Without it, Switch user is not offered at all rather than failing.
    user_switching: bool,
    /// Whether this session owns the machine's power. A headless or nested
    /// session shares the host with another desktop, so it offers no
    /// suspend, restart or shut down.
    owns_host_power: bool,
}

impl Default for PowerMenu {
    /// Every action. The live session builds its menu with
    /// [`PowerMenu::for_session`] from what the display manager supports.
    fn default() -> Self {
        Self::for_session(true)
    }
}

/// The menu's rhythm. Before the first frame, base tokens provide semantic
/// targets; drawing resolves shared face metrics from the actual theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PowerMenuMetrics {
    /// Panel padding and the gap between the title band and the rows.
    pub pad: f64,
    /// One action row: the Control row height, or taller when the label
    /// plus detail lines at the user's scale need more.
    pub row_h: f64,
    /// Gap between rows.
    pub row_gap: f64,
    /// Top of the first row from the panel's top edge.
    pub rows_top: f64,
    /// Single-line height of the Control label (`scale.sm`).
    label_h: f64,
    /// Single-line height of the Control sub-line (`scale.xs`).
    detail_h: f64,
    /// Width needed for the longest localized action name and shared leading slot.
    min_row_width: f64,
}

impl PowerMenuMetrics {
    pub(crate) fn for_text_scale(text_scale: f32) -> Self {
        let spacing = ui::tokens::Spacing::BASE;
        let metrics = ui::tokens::Metrics::BASE;
        let typography = ui::tokens::Typography::default();
        let line = |role: ui::TextRole| {
            f64::from(typography.style(role).line_height_px()) * f64::from(text_scale)
        };
        let pad = f64::from(spacing.s4);
        let title_h = line(ui::TextRole::Heading);
        let label_h = line(ui::TextRole::Label);
        let detail_h = line(ui::TextRole::Caption);
        // A row's two lines keep at least `s1` above and below them.
        let inner = f64::from(spacing.s1);
        Self {
            pad,
            row_h: f64::from(metrics.panel_row_height).max(inner + label_h + detail_h + inner),
            row_gap: f64::from(spacing.s2),
            rows_top: pad + title_h + pad,
            label_h,
            detail_h,
            min_row_width: 180.0,
        }
    }
    fn for_theme(theme: &Theme, text: &mut TextRenderer) -> Self {
        let pad = f64::from(theme.tokens.spacing.s4);
        let label_h = f64::from(
            text.measure_style("Mg", None, &theme.text(TextRole::Body))
                .1,
        );
        let detail_h = f64::from(
            text.measure_style("Mg", None, &theme.text(TextRole::Caption))
                .1,
        );
        let title_h = f64::from(
            text.measure_style("Mg", None, &theme.text(TextRole::Heading))
                .1,
        );
        let row_h = ListRowFace::new("Mg")
            .secondary("Mg")
            .leading(RowLeading::Icon(ui::icons::Icon::Shield))
            .height(text, theme);
        let title_width = ACTIONS
            .iter()
            .map(|(action, _)| {
                f64::from(
                    text.measure_style(&action.label(), None, &theme.text(TextRole::Body))
                        .0,
                )
            })
            .fold(0.0_f64, f64::max);
        let min_row_width = (title_width
            + 2.0 * ui::chrome::row_bleed(theme)
            + f64::from(theme.tokens.metrics.icon_md + theme.tokens.spacing.s3)
            + 2.0)
            .ceil();
        Self {
            pad,
            row_h,
            row_gap: f64::from(theme.tokens.spacing.s2),
            rows_top: pad + title_h + pad,
            label_h,
            detail_h,
            min_row_width: min_row_width.max(180.0),
        }
    }
}

impl PowerMenu {
    pub(crate) fn for_session(user_switching: bool) -> Self {
        Self {
            open: false,
            transition: ui::Transition::closed(),
            selected: 0,
            detail_tip: TooltipHost::new(),
            detail_rect: None,
            detail_input: DetailInput::None,
            resolved_metrics: PowerMenuMetrics::for_text_scale(1.0),
            user_switching,
            owns_host_power: true,
        }
    }

    /// Hide the rows that act on the host: switching user, suspend, restart
    /// and shut down.
    pub(crate) fn detach_from_host(&mut self) {
        self.user_switching = false;
        self.owns_host_power = false;
        self.selected = 0;
        self.clear_detail_tip();
    }

    /// The rows this session offers, in paint order. Paint, clicks, keys,
    /// semantic targets and the accessibility tree all index this list.
    fn rows(&self) -> impl Iterator<Item = &'static PowerMenuRow> + '_ {
        ACTIONS.iter().filter(|(action, ..)| match action {
            PowerMenuAction::External(os_actions::SessionAction::SwitchUser) => self.user_switching,
            PowerMenuAction::External(
                os_actions::SessionAction::Suspend
                | os_actions::SessionAction::Restart
                | os_actions::SessionAction::ShutDown,
            ) => self.owns_host_power,
            _ => true,
        })
    }

    pub(crate) fn row_count(&self) -> usize {
        self.rows().count()
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// Semantic hit targets for the visible session actions.  The menu is
    /// painted directly by the overlay host rather than as child widgets, so
    /// external interaction/accessibility tests need the same geometry the
    /// click handler uses instead of guessing screen coordinates.
    pub(crate) fn action_rects(&self, viewport: Rect) -> Vec<(&'static str, Rect)> {
        if !self.open {
            return Vec::new();
        }
        self.rows()
            .enumerate()
            .map(|(index, (action, _))| (action.semantic_name(), self.action_rect(viewport, index)))
            .collect()
    }

    pub(crate) fn accessibility_snapshot(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        self.open.then(|| {
            AccessNode::new(Role::Dialog)
                .with_name(t("power-menu-title"))
                .with_description(if self.user_switching {
                    t("power-menu-description-switch-user")
                } else {
                    t("power-menu-description")
                })
                .with_state(AccessState::new().modal(true))
                .with_children(
                    self.rows()
                        .enumerate()
                        .map(|(index, (action, _))| {
                            AccessNode::new(Role::Button)
                                .with_name(action.label())
                                .with_action_target(action.semantic_name())
                                .with_description(action.detail())
                                .with_state(
                                    AccessState::new()
                                        .focusable(true)
                                        .focused(index == self.selected),
                                )
                        })
                        .collect(),
                )
        })
    }

    pub(crate) fn accessibility_rect_for_path(
        &self,
        viewport: Rect,
        path: &[usize],
    ) -> Option<Rect> {
        if !self.open {
            return None;
        }
        match path {
            [] => Some(self.panel_rect(viewport)),
            [index] if *index < self.row_count() => Some(self.action_rect(viewport, *index)),
            _ => None,
        }
    }

    pub(crate) fn focus_accessibility_path(&mut self, path: &[usize]) -> bool {
        let [index] = path else {
            return false;
        };
        if !self.open || *index >= self.row_count() {
            return false;
        }
        self.selected = *index;
        self.detail_input = DetailInput::Keyboard;
        true
    }

    pub(crate) fn open(&mut self) {
        self.open = true;
        self.selected = 0;
        self.clear_detail_tip();
        self.transition
            .set_open(true, panel_open_duration(), ui::Curve::EaseOut);
    }

    pub(crate) fn close(&mut self) -> bool {
        let was_open = self.open;
        self.open = false;
        self.clear_detail_tip();
        if was_open {
            self.transition
                .set_open(false, panel_open_duration(), ui::Curve::EaseOut);
        }
        was_open
    }

    pub(crate) fn is_animating(&self, now: Instant) -> bool {
        self.transition.is_animating(now) || (self.open && self.detail_tip.needs_frame())
    }

    /// Pin the production modal at its resting state for deterministic visual
    /// review. This is deliberately test-only state control: opening and
    /// arming still go through [`Self::open`] and [`Self::key`].
    pub(crate) fn force_settled_for_visual_test(&mut self) {
        self.transition
            .set_open(self.open, Duration::ZERO, ui::Curve::Linear);
    }

    pub(crate) fn click(&mut self, viewport: Rect, point: Point) -> PowerMenuClick {
        if !self.open {
            return PowerMenuClick::Ignored;
        }
        if self.detail_rect.is_some_and(|rect| rect.contains(point)) {
            return PowerMenuClick::Consumed;
        }
        let stable_point = point;
        let painted_point = self.untransform_pointer(viewport, point, Instant::now());
        if !self.panel_rect(viewport).contains(painted_point)
            && !self.panel_rect(viewport).contains(stable_point)
        {
            self.close();
            return PowerMenuClick::Consumed;
        }
        // Prefer the inverse-painted coordinate while the flyout animates so
        // adjacent actions cannot be confused. The stable rectangles remain
        // valid semantic hit targets too: `action_rects` publishes them to
        // accessibility and automation before the transition settles.
        let count = self.row_count();
        let Some(index) = (0..count)
            .find(|&index| self.action_rect(viewport, index).contains(painted_point))
            .or_else(|| {
                (0..count).find(|&index| self.action_rect(viewport, index).contains(stable_point))
            })
        else {
            return PowerMenuClick::Consumed;
        };
        self.selected = index;
        self.activate(index)
    }

    pub(crate) fn metrics(&self) -> PowerMenuMetrics {
        self.resolved_metrics
    }

    /// The flyout, anchored under the top-right status area it was opened
    /// from, at the list-Panel width (HIG §9.6) and as tall as its rows need
    /// at the current text scale — capped to the viewport's bottom inset.
    pub(crate) fn panel_rect(&self, viewport: Rect) -> Rect {
        panel_rect_with(viewport, self.metrics(), self.row_count())
    }

    /// Row `index`, inset by the panel padding, on the metrics' pitch.
    pub(crate) fn action_rect(&self, viewport: Rect, index: usize) -> Rect {
        action_rect_with(viewport, index, self.metrics(), self.row_count())
    }

    /// Convert a pointer in painted viewport coordinates back into the
    /// flyout's stable layout coordinates. During the 220ms open/close scale
    /// animation the rows are painted through a centre transform; hit-testing
    /// the untransformed rectangles made a visibly correct click activate an
    /// adjacent action (most dangerously Lock instead of Log out).
    fn untransform_pointer(&self, viewport: Rect, point: Point, now: Instant) -> Point {
        let (_, dy, scale) = self.transition.modal_curve(now, 0.94);
        if !scale.is_finite() || scale <= f64::EPSILON {
            return point;
        }
        let centre = self.panel_rect(viewport).center();
        Point::new(
            centre.x + (point.x - centre.x) / scale,
            centre.y + (point.y - centre.y - dy) / scale,
        )
    }

    pub(crate) fn key(&mut self, key: &winit::keyboard::Key, shift: bool) -> PowerMenuClick {
        use winit::keyboard::{Key, NamedKey};
        if !self.open {
            return PowerMenuClick::Ignored;
        }
        let count = self.row_count();
        if matches!(
            key,
            Key::Named(
                NamedKey::ArrowDown
                    | NamedKey::ArrowUp
                    | NamedKey::Tab
                    | NamedKey::Home
                    | NamedKey::End
            )
        ) {
            self.detail_input = DetailInput::Keyboard;
        }
        match key {
            Key::Named(NamedKey::Escape) => {
                self.close();
                PowerMenuClick::Consumed
            }
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowLeft) => {
                self.selected = self
                    .selected
                    .checked_sub(1)
                    .unwrap_or_else(|| count.saturating_sub(1));
                PowerMenuClick::Consumed
            }
            Key::Named(NamedKey::ArrowDown | NamedKey::ArrowRight) => {
                self.selected = if self.selected.saturating_add(1) >= count {
                    0
                } else {
                    self.selected.saturating_add(1)
                };
                PowerMenuClick::Consumed
            }
            Key::Named(NamedKey::Tab) => {
                if shift {
                    self.selected = self
                        .selected
                        .checked_sub(1)
                        .unwrap_or_else(|| count.saturating_sub(1));
                } else {
                    self.selected = if self.selected.saturating_add(1) >= count {
                        0
                    } else {
                        self.selected.saturating_add(1)
                    };
                }
                PowerMenuClick::Consumed
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => self.activate(self.selected),
            _ => PowerMenuClick::Consumed,
        }
    }

    fn activate(&mut self, index: usize) -> PowerMenuClick {
        let Some(action) = self.rows().nth(index) else {
            return PowerMenuClick::Consumed;
        };
        self.close();
        match action.0 {
            PowerMenuAction::Lock => PowerMenuClick::Lock,
            PowerMenuAction::External(action) => PowerMenuClick::Execute(action),
        }
    }

    pub(crate) fn draw(
        &mut self,
        viewport: Rect,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if theme.reduce_motion {
            self.transition
                .set_open(self.open, Duration::ZERO, ui::Curve::Linear);
        }
        let now = Instant::now();
        if !self.transition.is_visible(now) {
            return;
        }
        // Shared faces consume resolved theme type. Measure with the same
        // renderer scale as paint, then retain that geometry for input.
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        self.resolved_metrics = PowerMenuMetrics::for_theme(theme, text_renderer);
        let (alpha, dy, scale) = self.transition.modal_curve(now, 0.94);
        let panel = self.panel_rect(viewport);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
            transform,
            &viewport,
        );
        let centre = panel.center();
        // `Affine`'s `Mul` composes transform matrices with plain float
        // multiply-adds — it cannot overflow or panic — but kurbo has no
        // non-operator way to chain a translate/scale/translate around an
        // arbitrary center point.
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine * Affine is a panic-free float matrix multiply; no combinator \
                      exists to compose this translate/scale/translate chain"
        )]
        let modal_transform = transform
            * Affine::translate((centre.x, centre.y + dy))
            * Affine::scale(scale)
            * Affine::translate((-centre.x, -centre.y));
        // Theme type tokens already carry the user's text scale, and the
        // panel paints with those resolved sizes; the renderer must not
        // scale them again (HIG §7a) — reset for this pass, as the shell's
        // immediate-mode chrome passes do, and restored afterwards.
        self.draw_panel(scene, modal_transform, text_renderer, theme, viewport);
        self.draw_detail_tip(
            viewport,
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
        );
        text_renderer.set_text_scale(renderer_scale);
        scene.pop_layer();
    }

    fn clear_detail_tip(&mut self) {
        self.detail_tip.dismiss();
        self.detail_rect = None;
        self.detail_input = DetailInput::None;
    }

    fn draw_detail_tip(&mut self, viewport: Rect, ctx: &mut PaintCtx<'_>) {
        let now = ui::time::now();
        let target = if !self.open {
            None
        } else if matches!(self.detail_input, DetailInput::Pointer(point)
            if self.detail_rect.is_some_and(|rect| rect.contains(point)))
        {
            self.detail_tip
                .visible()
                .map(|(label, anchor)| (anchor, label.to_owned()))
        } else {
            let index = match self.detail_input {
                DetailInput::Keyboard => Some(self.selected),
                DetailInput::Pointer(point) => (0..self.row_count())
                    .find(|index| self.action_rect(viewport, *index).contains(point)),
                DetailInput::None => None,
            };
            index.and_then(|index| {
                self.rows()
                    .nth(index)
                    .map(|(action, _)| (self.action_rect(viewport, index), action.detail()))
            })
        };
        self.detail_tip.on_pointer(target, now);
        self.detail_tip.tick(now);
        self.detail_rect = None;
        if let Some((label, anchor)) = self.detail_tip.visible() {
            let size = chip_size_in_viewport(label, ctx.text, ctx.theme, viewport);
            // Keep the hint outside the action panel whenever the output has room.
            // Compact outputs still use the shared clamped placement and inert hit area.
            let panel = self.panel_rect(viewport);
            let gap = f64::from(ctx.theme.tokens.spacing.s4);
            let placement_anchor = if panel.x0 - viewport.x0 >= size.0 + gap {
                Rect::new(panel.x0, anchor.y0, panel.x1, anchor.y1)
            } else {
                anchor
            };
            let rect = chip_rect_on(TooltipSide::Beside, placement_anchor, size, viewport);
            self.detail_rect = Some(rect);
            draw_chip(ctx.scene, ctx.transform, ctx.text, ctx.theme, rect, label);
        }
    }

    fn draw_panel(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
    ) {
        let panel = self.panel_rect(viewport);
        let m = self.metrics();
        let colors = &theme.tokens.colors;
        // This is a compact flyout rooted in the status area, not a modal
        // task. Its bounds are the click-dismiss surface; the rest of the
        // desktop stays visually present so the power button and result read
        // as one continuous interaction.
        // Panel role (HIG §9.1/§9.2): the flyout is the same material as
        // every other Panel — `panel_ground` at the Panel radius, with the
        // derived Panel border — rather than a ramp slot and a literal 24.
        ui::chrome::shell::paint_panel(scene, transform, panel, theme, 1.0);
        // Theme-resolved sizes, painted once: `draw` has reset the renderer
        // for this pass.
        let title_style = theme.text(TextRole::Heading);
        // Panel title on the padding line (HIG §9.6), centred on the band
        // the metrics reserved for it.
        let title = t("power-menu-title");
        let (_, title_h) = text_renderer.measure_style(&title, None, &title_style);
        let title_band_h = m.rows_top - 2.0 * m.pad;
        text_renderer.draw_style(
            scene,
            &title,
            colors.text_primary,
            at(
                transform,
                (
                    panel.x0 + m.pad,
                    panel.y0 + m.pad + (title_band_h - f64::from(title_h)) / 2.0,
                ),
            ),
            None,
            &title_style,
        );
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        for (index, (action, icon)) in self.rows().enumerate() {
            let (label, detail) = (action.label(), action.detail());
            let row = self.action_rect(viewport, index);
            let selected = self.selected == index;
            // Keep the flyout's quiet row floor. The shared face owns the
            // selected lift, type, glyph, elision and keyboard focus ring.
            let shape = RoundedRect::from_rect(
                row,
                ui::chrome::ChromeRole::Control.radius(theme, row.height()),
            );
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                colors.lift(0.35),
                None,
                &shape,
            );
            let face = ListRowFace::new(&label)
                .leading(RowLeading::Icon(*icon))
                .selected(selected)
                .interaction(Interaction {
                    focused: selected,
                    ..Interaction::default()
                });
            // A short viewport can squeeze the row: retain its action label
            // and accessible detail without painting across its neighbour.
            let full = face.clone().secondary(&detail);
            if full.height(ctx.text, theme) <= row.height() {
                full.paint(&mut ctx, row);
            } else {
                face.paint(&mut ctx, row);
            }
        }
    }
}

impl ShellLayer for PowerMenu {
    fn id(&self) -> LayerId {
        LayerId::PowerMenu
    }

    fn is_open(&self) -> bool {
        PowerMenu::is_open(self)
    }

    /// Modal for the keyboard: it owns every pressed key so typing cannot
    /// leak into a hosted or Wayland app behind it, and the conventional
    /// navigation keys make every action reachable. For the pointer it is a
    /// flyout: a press outside dismisses it.
    fn handle_event(&mut self, event: &WindowEvent, cx: &mut LayerCx<'_, '_>) -> Routed {
        let outcome = match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => self.key(logical_key, cx.event.modifiers.state().shift_key()),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => match cx.cursor {
                Some(point) => self.click(cx.geometry.viewport, point),
                None => return Routed::Pass,
            },
            WindowEvent::CursorMoved { .. } => {
                self.detail_input = cx.cursor.map_or(DetailInput::None, DetailInput::Pointer);
                cx.request(LayerIntent::Repaint);
                return Routed::Consumed;
            }
            WindowEvent::CursorLeft { .. } => {
                self.clear_detail_tip();
                cx.request(LayerIntent::Repaint);
                return Routed::Consumed;
            }
            _ => return Routed::Pass,
        };
        match outcome {
            PowerMenuClick::Ignored => return Routed::Pass,
            PowerMenuClick::Consumed => {}
            PowerMenuClick::Lock => {
                cx.request(LayerIntent::Session(os_actions::SessionAction::Lock));
            }
            PowerMenuClick::Execute(action) => cx.request(LayerIntent::Session(action)),
        }
        Routed::Consumed
    }

    fn draw(&mut self, cx: &mut LayerPaint<'_>) {
        self.draw(
            cx.geometry.viewport,
            cx.scene,
            cx.transform,
            cx.text_renderer,
            cx.theme,
        );
    }

    fn accessibility(&self, _geometry: LayerGeometry<'_>) -> Option<ui::a11y::AccessNode> {
        self.accessibility_snapshot()
    }

    fn accessibility_rect(&self, geometry: LayerGeometry<'_>, path: &[usize]) -> Option<Rect> {
        self.accessibility_rect_for_path(geometry.viewport, path)
    }

    fn focus_accessibility(&mut self, path: &[usize]) -> bool {
        self.focus_accessibility_path(path)
    }

    fn semantic_intent(&self, target: &str) -> Option<LayerIntent> {
        if !self.open {
            return None;
        }
        self.rows()
            .find(|(action, _)| action.semantic_name() == target)
            .map(|(action, _)| {
                LayerIntent::Session(match action {
                    PowerMenuAction::Lock => os_actions::SessionAction::Lock,
                    PowerMenuAction::External(action) => *action,
                })
            })
    }

    fn dismiss(&mut self) -> bool {
        self.close()
    }

    fn is_animating(&self, now: Instant) -> bool {
        PowerMenu::is_animating(self, now)
    }
}

#[derive(Clone, Copy)]
enum PowerMenuAction {
    Lock,
    External(os_actions::SessionAction),
}

impl PowerMenuAction {
    const fn semantic_name(self) -> &'static str {
        match self {
            Self::Lock | Self::External(os_actions::SessionAction::Lock) => "power_lock",
            Self::External(os_actions::SessionAction::Logout) => "power_logout",
            Self::External(os_actions::SessionAction::SwitchUser) => "power_switch_user",
            Self::External(os_actions::SessionAction::Suspend) => "power_suspend",
            Self::External(os_actions::SessionAction::Restart) => "power_restart",
            Self::External(os_actions::SessionAction::ShutDown) => "power_shut_down",
        }
    }

    /// The row's name.
    fn label(self) -> String {
        match self {
            Self::Lock | Self::External(os_actions::SessionAction::Lock) => t("power-lock"),
            Self::External(os_actions::SessionAction::Logout) => t("power-logout"),
            Self::External(os_actions::SessionAction::SwitchUser) => t("power-switch-user"),
            Self::External(os_actions::SessionAction::Suspend) => t("power-suspend"),
            Self::External(os_actions::SessionAction::Restart) => t("power-restart"),
            Self::External(os_actions::SessionAction::ShutDown) => t("power-shut-down"),
        }
    }

    /// The line under the row's name.
    fn detail(self) -> String {
        match self {
            Self::Lock | Self::External(os_actions::SessionAction::Lock) => t("power-lock-detail"),
            Self::External(os_actions::SessionAction::Logout) => t("power-logout-detail"),
            Self::External(os_actions::SessionAction::SwitchUser) => t("power-switch-user-detail"),
            Self::External(os_actions::SessionAction::Suspend) => t("power-suspend-detail"),
            Self::External(os_actions::SessionAction::Restart) => t("power-restart-detail"),
            Self::External(os_actions::SessionAction::ShutDown) => t("power-shut-down-detail"),
        }
    }
}

type PowerMenuRow = (PowerMenuAction, ui::icons::Icon);

const ACTIONS: [PowerMenuRow; 6] = [
    (PowerMenuAction::Lock, ui::icons::Icon::Shield),
    (
        PowerMenuAction::External(os_actions::SessionAction::Logout),
        ui::icons::Icon::ArrowRight,
    ),
    (
        PowerMenuAction::External(os_actions::SessionAction::SwitchUser),
        ui::icons::Icon::Users,
    ),
    (
        PowerMenuAction::External(os_actions::SessionAction::Suspend),
        ui::icons::Icon::Sun,
    ),
    (
        PowerMenuAction::External(os_actions::SessionAction::Restart),
        ui::icons::Icon::RotateCw,
    ),
    (
        PowerMenuAction::External(os_actions::SessionAction::ShutDown),
        ui::icons::Icon::Power,
    ),
];

#[cfg(test)]
fn panel_rect(viewport: Rect) -> Rect {
    panel_rect_with(
        viewport,
        PowerMenuMetrics::for_text_scale(1.0),
        ACTIONS.len(),
    )
}

#[cfg(test)]
fn action_rect(viewport: Rect, index: usize) -> Rect {
    action_rect_with(
        viewport,
        index,
        PowerMenuMetrics::for_text_scale(1.0),
        ACTIONS.len(),
    )
}

fn layout_metrics(viewport: Rect, m: PowerMenuMetrics) -> (usize, f64, f64, f64) {
    let columns = columns_for_metrics(viewport, m);
    let base = PowerMenuMetrics::for_text_scale(1.0);
    let top = 52.0 + (m.rows_top - base.rows_top).max(0.0);
    let row_h = action_height(viewport, columns) + (m.row_h - base.row_h).max(0.0);
    (columns, top, row_h, action_gap(columns))
}

fn panel_rect_with(viewport: Rect, m: PowerMenuMetrics, count: usize) -> Rect {
    let (columns, top, row_h, gap) = layout_metrics(viewport, m);
    let rows = count.div_ceil(columns);
    let preferred_width = if columns == 2 {
        424.0_f64.max(2.0 * m.min_row_width + 32.0)
    } else {
        336.0_f64.max(m.min_row_width + 24.0)
    };
    let width = preferred_width.min((viewport.width() - 16.0).max(1.0));
    let height = top
        + small_count_to_f64(rows) * row_h
        + small_count_to_f64(rows.saturating_sub(1)) * gap
        + 12.0;
    let right = viewport.x1 - 4.0;
    let y0 = viewport.y0 + 48.0;
    Rect::new(
        right - width,
        y0,
        right,
        (y0 + height).min(viewport.y1 - 8.0),
    )
}

#[expect(
    clippy::arithmetic_side_effects,
    reason = "`columns` comes from action_columns, which only returns 1 or 2, so the remainder and quotient cannot divide by zero"
)]
fn action_rect_with(viewport: Rect, index: usize, m: PowerMenuMetrics, count: usize) -> Rect {
    let panel = panel_rect_with(viewport, m, count);
    let (columns, top, wanted_h, gap) = layout_metrics(viewport, m);
    let rows = count.div_ceil(columns);
    let width = (panel.width() - 24.0 - small_count_to_f64(columns.saturating_sub(1)) * gap)
        / small_count_to_f64(columns);
    let row_h = wanted_h.min(
        ((panel.height() - top - 12.0 - small_count_to_f64(rows.saturating_sub(1)) * gap)
            / small_count_to_f64(rows))
        .max(0.0),
    );
    let x0 = panel.x0 + 12.0 + small_count_to_f64(index % columns) * (width + gap);
    let y0 = panel.y0 + top + small_count_to_f64(index / columns) * (row_h + gap);
    Rect::new(x0, y0, x0 + width, y0 + row_h)
}

fn columns_for_metrics(viewport: Rect, metrics: PowerMenuMetrics) -> usize {
    let columns = action_columns(viewport);
    // Keep every action visible on short outputs. When there is vertical
    // room, prefer one column over truncating the action names at large type.
    if columns == 2
        && viewport.height() >= 400.0
        && 2.0 * metrics.min_row_width + 32.0 > viewport.width() - 16.0
    {
        1
    } else {
        columns
    }
}

fn action_columns(viewport: Rect) -> usize {
    // At normal heights narrow windows retain the roomy single-column list.
    // A short viewport needs the compact two-column arrangement so all six
    // actions remain visible and clickable instead of being clipped below
    // the panel's clamped bottom edge.
    if viewport.width() >= 640.0 || (viewport.width() >= 420.0 && viewport.height() < 400.0) {
        2
    } else {
        1
    }
}

fn action_height(viewport: Rect, columns: usize) -> f64 {
    if columns == 2 && viewport.height() < 400.0 {
        44.0
    } else if columns == 2 {
        56.0
    } else {
        44.0
    }
}

fn action_gap(columns: usize) -> f64 {
    if columns == 2 { 8.0 } else { 4.0 }
}

/// Widens a row index to `f64` for draw-time row-position math. Every
/// call site is a row index into the fixed [`ACTIONS`] table, so
/// the integer is exactly representable.
fn small_count_to_f64(n: usize) -> f64 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "bounded well under 2^53 — a row index into a 5-action menu"
    )]
    {
        n as f64
    }
}

/// `transform * Affine::translate(xy)`, used at every glyph draw site
/// below. `Affine::mul` is plain matrix composition with no side effects,
/// but it is a custom `Mul` impl rather than primitive integer arithmetic,
/// which is what trips the lint at each call site.
fn at(transform: Affine, xy: (f64, f64)) -> Affine {
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "Affine::mul is pure matrix composition (translate here), not primitive integer arithmetic that can silently wrap"
    )]
    {
        transform * Affine::translate(xy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::ColorCompat;
    use winit::keyboard::{Key, NamedKey};

    const VIEWPORT: Rect = Rect::new(0.0, 0.0, 1280.0, 800.0);

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn power_menu_rows_export_and_execute_on_first_click_or_key() {
        fn open_menu_exports_the_same_six_semantic_rows_it_paints() {
            let mut menu = PowerMenu::default();
            assert!(menu.action_rects(VIEWPORT).is_empty());
            assert!(menu.accessibility_snapshot().is_none());
            menu.open();
            let targets = menu.action_rects(VIEWPORT);
            assert_eq!(
                targets.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
                [
                    "power_lock",
                    "power_logout",
                    "power_switch_user",
                    "power_suspend",
                    "power_restart",
                    "power_shut_down",
                ]
            );
            assert!(targets.iter().all(|(_, rect)| rect.area() > 0.0));
            assert_eq!(
                menu.accessibility_snapshot()
                    .expect("open session menu")
                    .children
                    .len(),
                ACTIONS.len()
            );
        }

        fn lock_switch_user_and_suspend_execute_on_first_click() {
            let mut menu = PowerMenu::default();
            for (index, expected) in [
                (0, PowerMenuClick::Lock),
                (
                    2,
                    PowerMenuClick::Execute(os_actions::SessionAction::SwitchUser),
                ),
                (
                    3,
                    PowerMenuClick::Execute(os_actions::SessionAction::Suspend),
                ),
            ] {
                menu.open();
                menu.force_settled_for_visual_test();
                let point = menu.action_rect(VIEWPORT, index).center();
                assert_eq!(menu.click(VIEWPORT, point), expected);
                assert!(!menu.open);
            }
        }

        fn every_visible_action_executes_on_first_click() {
            for (index, expected) in [
                (1, os_actions::SessionAction::Logout),
                (4, os_actions::SessionAction::Restart),
                (5, os_actions::SessionAction::ShutDown),
            ] {
                let mut menu = PowerMenu::default();
                menu.open();
                menu.force_settled_for_visual_test();
                let point = menu.action_rect(VIEWPORT, index).center();
                assert_eq!(
                    menu.click(VIEWPORT, point),
                    PowerMenuClick::Execute(expected)
                );
                assert!(!menu.open);
            }
        }

        fn opening_animation_hit_test_tracks_the_painted_logout_row() {
            let mut menu = PowerMenu::default();
            menu.open();
            let now = Instant::now();
            let panel = panel_rect(VIEWPORT);
            let stable = action_rect(VIEWPORT, 1).center();
            let (_, dy, scale) = menu.transition.modal_curve(now, 0.94);
            let centre = panel.center();
            let painted = Point::new(
                centre.x + (stable.x - centre.x) * scale,
                centre.y + dy + (stable.y - centre.y) * scale,
            );
            let logical = menu.untransform_pointer(VIEWPORT, painted, now);
            assert!((logical.x - stable.x).abs() < 0.001);
            assert!((logical.y - stable.y).abs() < 0.001);
            // The production click samples a fresh instant. Freeze the transition
            // at this same intermediate value so the regression remains exact.
            assert_eq!(
                (0..ACTIONS.len()).find(|&index| action_rect(VIEWPORT, index).contains(logical)),
                Some(1)
            );
        }

        fn keyboard_navigation_reaches_actions_and_activates_once() {
            use winit::keyboard::{Key, NamedKey};

            let mut menu = PowerMenu::default();
            menu.open();
            assert_eq!(
                menu.key(&Key::Named(NamedKey::ArrowDown), false),
                PowerMenuClick::Consumed
            );
            assert_eq!(
                menu.key(&Key::Named(NamedKey::Enter), false),
                PowerMenuClick::Execute(os_actions::SessionAction::Logout)
            );

            menu.open();
            menu.selected = 5;
            assert_eq!(
                menu.key(&Key::Named(NamedKey::Space), false),
                PowerMenuClick::Execute(os_actions::SessionAction::ShutDown)
            );
        }

        /// A display manager that cannot open a second greeter (greetd) gets no
        /// Switch user row at all, and every remaining row keeps paint, click,
        /// keyboard and accessibility on the same index.
        fn switch_user_is_offered_only_when_the_display_manager_supports_it() {
            use winit::keyboard::{Key, NamedKey};

            let mut menu = PowerMenu::for_session(false);
            menu.open();
            menu.force_settled_for_visual_test();
            let targets = menu.action_rects(VIEWPORT);
            assert_eq!(
                targets.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
                [
                    "power_lock",
                    "power_logout",
                    "power_suspend",
                    "power_restart",
                    "power_shut_down",
                ]
            );
            let tree = menu.accessibility_snapshot().expect("open session menu");
            assert_eq!(tree.children.len(), targets.len());
            assert!(
                tree.children
                    .iter()
                    .all(|node| node.name.as_deref() != Some("Switch user"))
            );
            for (index, expected) in [
                PowerMenuClick::Lock,
                PowerMenuClick::Execute(os_actions::SessionAction::Logout),
                PowerMenuClick::Execute(os_actions::SessionAction::Suspend),
                PowerMenuClick::Execute(os_actions::SessionAction::Restart),
                PowerMenuClick::Execute(os_actions::SessionAction::ShutDown),
            ]
            .into_iter()
            .enumerate()
            {
                menu.open();
                menu.force_settled_for_visual_test();
                assert_eq!(menu.click(VIEWPORT, targets[index].1.center()), expected);
            }

            menu.open();
            assert_eq!(
                menu.key(&Key::Named(NamedKey::ArrowUp), false),
                PowerMenuClick::Consumed
            );
            assert_eq!(
                menu.key(&Key::Named(NamedKey::Enter), false),
                PowerMenuClick::Execute(os_actions::SessionAction::ShutDown),
                "wrapping up from the first row lands on the last offered row"
            );
            menu.open();
            menu.selected = 2;
            assert_eq!(
                menu.key(&Key::Named(NamedKey::Space), false),
                PowerMenuClick::Execute(os_actions::SessionAction::Suspend)
            );
        }

        open_menu_exports_the_same_six_semantic_rows_it_paints();
        lock_switch_user_and_suspend_execute_on_first_click();
        every_visible_action_executes_on_first_click();
        opening_animation_hit_test_tracks_the_painted_logout_row();
        keyboard_navigation_reaches_actions_and_activates_once();
        switch_user_is_offered_only_when_the_display_manager_supports_it();
    }
    #[test]
    fn power_menu_dismisses_without_acting_and_animates_immediately() {
        fn outside_click_closes_without_an_action() {
            let mut menu = PowerMenu::default();
            menu.open();
            assert_eq!(menu.click(VIEWPORT, Point::ZERO), PowerMenuClick::Consumed);
            assert!(!menu.open);
        }

        fn escape_closes_the_keyboard_modal_without_an_action() {
            use winit::keyboard::{Key, NamedKey};

            let mut menu = PowerMenu::default();
            menu.open();
            assert_eq!(
                menu.key(&Key::Named(NamedKey::Escape), false),
                PowerMenuClick::Consumed
            );
            assert!(!menu.open);
        }

        fn opening_requests_animation_frames_without_waiting_for_draw() {
            let mut menu = PowerMenu::default();
            menu.open();
            assert!(menu.is_animating(Instant::now()));
        }

        outside_click_closes_without_an_action();
        escape_closes_the_keyboard_modal_without_an_action();
        opening_requests_animation_frames_without_waiting_for_draw();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn power_menu_layout_anchors_and_fits_every_viewport() {
        fn menu_is_anchored_below_the_top_right_status_area() {
            let menu = PowerMenu::default();
            for viewport in [VIEWPORT, Rect::new(0.0, 0.0, 960.0, 540.0)] {
                let panel = menu.panel_rect(viewport);
                assert_eq!(panel.x1, viewport.x1 - 4.0);
                assert_eq!(panel.y0, viewport.y0 + 48.0);
                assert!(panel.y1 <= viewport.y1 - 8.0);
                for index in 0..ACTIONS.len() {
                    assert!(
                        panel.contains(menu.action_rect(viewport, index).center()),
                        "action {index} must remain inside the anchored menu at {viewport:?}"
                    );
                }
            }
        }

        fn compact_flyout_is_materially_smaller_than_the_old_modal_sheet() {
            let panel = panel_rect(Rect::new(0.0, 0.0, 960.0, 540.0));
            assert_eq!(panel.width(), 424.0);
            assert_eq!(panel.height(), 248.0);
            assert!(panel.area() < 420.0 * 446.0 * 0.65);
        }

        fn two_column_layout_fits_both_visual_audit_viewports() {
            for viewport in [
                Rect::new(0.0, 0.0, 800.0, 600.0),
                Rect::new(0.0, 0.0, 1280.0, 800.0),
            ] {
                let panel = panel_rect(viewport);
                assert_eq!(action_columns(viewport), 2);
                assert_eq!(panel.height(), 248.0);
                assert!(panel.x1 >= viewport.x1 - 4.0);
                let rows = (0..ACTIONS.len())
                    .map(|index| action_rect(viewport, index))
                    .collect::<Vec<_>>();
                assert!(rows.iter().all(|row| panel.contains(row.center())));
                assert!(rows.iter().all(|row| row.width() >= 180.0));
                for (index, row) in rows.iter().enumerate() {
                    for earlier in &rows[..index] {
                        let overlap = row.intersect(*earlier);
                        assert!(
                            overlap.width() <= 0.0 || overlap.height() <= 0.0,
                            "action {index} overlaps an earlier action at {viewport:?}"
                        );
                    }
                }
            }
        }

        fn narrow_layout_falls_back_to_one_column_without_clipping_actions() {
            let viewport = Rect::new(0.0, 0.0, 480.0, 600.0);
            let panel = panel_rect(viewport);
            assert_eq!(action_columns(viewport), 1);
            assert_eq!(panel.height(), 348.0);
            for index in 0..ACTIONS.len() {
                let row = action_rect(viewport, index);
                assert!(panel.contains(row.center()));
                assert!(row.width() >= 44.0 && row.height() >= 44.0);
            }
        }

        fn tiny_viewport_uses_compact_two_columns_for_all_actions() {
            let viewport = Rect::new(0.0, 0.0, 480.0, 300.0);
            let panel = panel_rect(viewport);
            assert_eq!(action_columns(viewport), 2);
            assert!(panel.y1 <= viewport.y1 - 8.0);
            for index in 0..ACTIONS.len() {
                let row = action_rect(viewport, index);
                assert!(
                    row.x0 >= panel.x0
                        && row.y0 >= panel.y0
                        && row.x1 <= panel.x1
                        && row.y1 <= panel.y1,
                    "action {index} must be fully inside the compact panel: {row:?} / {panel:?}"
                );
                assert!(
                    row.x0 >= viewport.x0
                        && row.y0 >= viewport.y0
                        && row.x1 <= viewport.x1
                        && row.y1 <= viewport.y1,
                    "action {index} must be fully inside the viewport: {row:?}"
                );
            }
        }

        fn tiny_viewport_all_six_actions_are_reachable_by_click() {
            let viewport = Rect::new(0.0, 0.0, 480.0, 300.0);
            let expected = [
                PowerMenuClick::Lock,
                PowerMenuClick::Execute(os_actions::SessionAction::Logout),
                PowerMenuClick::Execute(os_actions::SessionAction::SwitchUser),
                PowerMenuClick::Execute(os_actions::SessionAction::Suspend),
                PowerMenuClick::Execute(os_actions::SessionAction::Restart),
                PowerMenuClick::Execute(os_actions::SessionAction::ShutDown),
            ];
            for (index, expected) in expected.into_iter().enumerate() {
                let mut menu = PowerMenu::default();
                menu.open();
                menu.force_settled_for_visual_test();
                assert_eq!(
                    menu.click(viewport, action_rect(viewport, index).center()),
                    expected
                );
                assert!(!menu.open);
            }
        }

        menu_is_anchored_below_the_top_right_status_area();
        compact_flyout_is_materially_smaller_than_the_old_modal_sheet();
        two_column_layout_fits_both_visual_audit_viewports();
        narrow_layout_falls_back_to_one_column_without_clipping_actions();
        tiny_viewport_uses_compact_two_columns_for_all_actions();
        tiny_viewport_all_six_actions_are_reachable_by_click();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn power_menu_rows_hold_their_text_at_every_scale_and_contrast() {
        /// HIG §9.6: a list Panel at the roster width, Control rows on the
        /// `s2` pitch inside `s4` padding — read from the tokens, not restated.
        fn rows_are_control_rows_on_the_token_pitch_at_default_text() {
            let m = PowerMenuMetrics::for_text_scale(1.0);
            assert_eq!(
                m.row_h,
                f64::from(ui::tokens::Metrics::BASE.panel_row_height)
            );
            assert_eq!(m.row_gap, f64::from(ui::tokens::Spacing::BASE.s2));
            assert_eq!(m.pad, f64::from(ui::tokens::Spacing::BASE.s4));
            let menu = PowerMenu::default();
            let panel = menu.panel_rect(VIEWPORT);
            assert_eq!(panel.width(), 424.0);
            let first = menu.action_rect(VIEWPORT, 0);
            let last = menu.action_rect(VIEWPORT, ACTIONS.len() - 1);
            assert!(
                first.height() >= m.row_h,
                "an uncapped panel keeps the Control row"
            );
            assert_eq!(first.x0 - panel.x0, 12.0);
            assert_eq!(panel.x1 - menu.action_rect(VIEWPORT, 1).x1, 12.0);
            assert!(
                (panel.y1 - last.y1 - 12.0).abs() < 1e-9,
                "the last row sits on the panel padding: {panel:?} / {last:?}"
            );
            assert_eq!(menu.action_rect(VIEWPORT, 2).y0 - first.y1, m.row_gap);
        }

        /// When the viewport caps the panel (3–4× text on a short output) the
        /// rows shrink to tile the capped panel rather than being laid on the
        /// uncapped pitch past its bottom edge, where they could be painted but
        /// never pressed — and none drops under the minimum pointer target.
        fn a_capped_panel_still_tiles_every_row_inside_it() {
            for (viewport, scale) in [
                (Rect::new(0.0, 0.0, 1280.0, 350.0), 3.0_f32),
                (Rect::new(0.0, 0.0, 1280.0, 400.0), 4.0),
            ] {
                let m = PowerMenuMetrics::for_text_scale(scale);
                let panel = panel_rect_with(viewport, m, ACTIONS.len());
                assert!(
                    (panel.y1 - (viewport.y1 - 8.0)).abs() < 1e-9,
                    "the test needs a capped panel at {scale}× on {viewport:?}"
                );
                let target = f64::from(ui::tokens::Metrics::BASE.min_pointer_target);
                for index in 0..ACTIONS.len() {
                    let row = action_rect_with(viewport, index, m, ACTIONS.len());
                    assert!(
                        row.y1 <= panel.y1 - 12.0 + 1e-9,
                        "row {index} stays inside the capped panel: {row:?} in {panel:?}"
                    );
                    assert!(row.height() >= target, "row {index} keeps a pointer target");
                    if let Some(next) = index
                        .checked_add(action_columns(viewport))
                        .filter(|n| *n < ACTIONS.len())
                    {
                        assert!(action_rect_with(viewport, next, m, ACTIONS.len()).y0 >= row.y1);
                    }
                }
            }
        }

        /// At 200 % a label and a detail line no longer fit a 44 px row; the
        /// row grows to hold both and the panel grows to hold every row, so
        /// nothing paints through its neighbour.
        fn large_text_grows_the_rows_and_the_panel_holds_them() {
            let one = PowerMenuMetrics::for_text_scale(1.0);
            let two = PowerMenuMetrics::for_text_scale(2.0);
            let inner = f64::from(ui::tokens::Spacing::BASE.s1);
            assert!(two.row_h >= two.label_h + two.detail_h + 2.0 * inner);
            assert!(two.row_h > one.row_h);
            for viewport in [VIEWPORT, Rect::new(0.0, 0.0, 800.0, 700.0)] {
                let panel = panel_rect_with(viewport, two, ACTIONS.len());
                for index in 0..ACTIONS.len() {
                    let row = action_rect_with(viewport, index, two, ACTIONS.len());
                    assert!(
                        row.y1 <= panel.y1 - 12.0 + 1e-9,
                        "row {index} must stay inside the panel at {viewport:?}"
                    );
                    if let Some(next) = index
                        .checked_add(action_columns(viewport))
                        .filter(|n| *n < ACTIONS.len())
                    {
                        assert!(action_rect_with(viewport, next, two, ACTIONS.len()).y0 >= row.y1);
                    }
                }
            }
        }

        /// The click path has no theme: it must hit-test the rows where the
        /// last frame painted them, so a 200 % session activates the row under
        /// the pointer rather than the one the 1× geometry would put there.
        fn clicks_hit_the_rows_where_a_large_text_frame_painted_them() {
            let mut menu = PowerMenu::default();
            menu.open();
            menu.force_settled_for_visual_test();
            let theme = Theme::dark().with_text_scale(2.0);
            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);
            let mut scene = Scene::new();
            menu.draw(VIEWPORT, &mut scene, Affine::IDENTITY, &mut tr, &theme);
            let painted = menu.action_rect(VIEWPORT, 3);
            assert!(
                painted.height()
                    >= ListRowFace::new("Mg")
                        .secondary("Mg")
                        .height(&mut TextRenderer::new(), &theme),
                "paint and input reserve the shared face's measured height"
            );
            assert!(
                !PowerMenu::default()
                    .action_rect(VIEWPORT, 3)
                    .contains(painted.center()),
                "the test is only meaningful if 1× and 2× geometry disagree"
            );
            assert_eq!(
                menu.click(VIEWPORT, painted.center()),
                PowerMenuClick::Execute(os_actions::SessionAction::Suspend)
            );
        }

        fn power_flyout_surface_is_opaque_on_every_shipped_contrast_mode() {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                assert_eq!(
                    ui::chrome::shell::panel_ground(&theme).a(),
                    255,
                    "power flyout must fully cover bright app content"
                );
            }
        }

        rows_are_control_rows_on_the_token_pitch_at_default_text();
        a_capped_panel_still_tiles_every_row_inside_it();
        large_text_grows_the_rows_and_the_panel_holds_them();
        clicks_hit_the_rows_where_a_large_text_frame_painted_them();
        power_flyout_surface_is_opaque_on_every_shipped_contrast_mode();
    }

    #[test]
    fn power_menu_scaled_theme_resize_keeps_focus_and_semantic_hit_targets_aligned() {
        use winit::keyboard::{Key, NamedKey};
        let mut menu = PowerMenu::default();
        menu.open();
        menu.force_settled_for_visual_test();
        assert!(menu.focus_accessibility_path(&[3]));
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        let theme = Theme::dark().with_ui_scale(1.5).with_text_scale(1.3);
        for viewport in [VIEWPORT, Rect::new(0.0, 0.0, 480.0, 600.0)] {
            menu.draw(viewport, &mut scene, Affine::IDENTITY, &mut text, &theme);
            let full_height = ListRowFace::new("Mg")
                .secondary("Mg")
                .height(&mut text, &theme);
            assert!(menu.metrics().row_h >= full_height);
            assert!(
                menu.metrics().row_h > PowerMenuMetrics::for_text_scale(theme.text_scale).row_h
            );
            let tree = menu.accessibility_snapshot().expect("open power menu");
            assert!(tree.children[3].state.focused);
            let targets = menu.action_rects(viewport);
            for (index, (_, rect)) in targets.iter().enumerate() {
                assert_eq!(
                    menu.accessibility_rect_for_path(viewport, &[index]),
                    Some(*rect)
                );
                assert!(menu.panel_rect(viewport).contains(rect.center()));
            }
        }
        assert_eq!(
            menu.key(&Key::Named(NamedKey::Escape), false),
            PowerMenuClick::Consumed
        );
        assert_eq!(
            menu.key(&Key::Named(NamedKey::Enter), false),
            PowerMenuClick::Ignored
        );
        menu.open();
        menu.force_settled_for_visual_test();
        menu.draw(VIEWPORT, &mut scene, Affine::IDENTITY, &mut text, &theme);
        assert_eq!(
            menu.click(VIEWPORT, menu.action_rect(VIEWPORT, 0).center()),
            PowerMenuClick::Lock
        );
        assert_eq!(menu.click(VIEWPORT, Point::ZERO), PowerMenuClick::Ignored);
    }

    /// Review fixtures: the ux-bench cannot open the power menu on macOS,
    /// so it is photographed from its paint path at 1× and 200 % text, on a
    /// laptop viewport, and on the light and high-contrast palettes. The
    /// rendered pixels are asserted, not just the file: the title band is
    /// the Panel ground and an idle row's interior is its lift floor.
    #[test]
    fn power_menu_review_fixtures_are_written() {
        use crate::shell::osd::review_offscreen::OffscreenRenderer;
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/ui-review/s2-r2/power-menu");
        // One wgpu device per viewport size, shared by every variant of it.
        let mut desktop = OffscreenRenderer::new(1280, 800);
        let mut laptop = OffscreenRenderer::new(800, 700);
        let mut narrow = OffscreenRenderer::new(480, 600);
        let variants = vec![
            (
                "dark-1x-1280x800",
                Theme::dark(),
                1.0_f32,
                (1280_u32, 800_u32),
            ),
            (
                "dark-2x-1280x800",
                Theme::dark().with_text_scale(2.0),
                2.0,
                (1280, 800),
            ),
            (
                "dark-2x-800x700",
                Theme::dark().with_text_scale(2.0),
                2.0,
                (800, 700),
            ),
            ("light-1x-1280x800", Theme::light(), 1.0, (1280, 800)),
            (
                "dark-ui-1.5-text-1.3-1280x800",
                Theme::dark().with_ui_scale(1.5).with_text_scale(1.3),
                1.3,
                (1280, 800),
            ),
            (
                "dark-2x-480x600",
                Theme::dark().with_text_scale(2.0),
                2.0,
                (480, 600),
            ),
            (
                "high-contrast-1x-1280x800",
                Theme::high_contrast(),
                1.0,
                (1280, 800),
            ),
        ];
        for (name, theme, scale, (w, h)) in variants {
            let viewport = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
            let mut menu = PowerMenu::default();
            menu.open();
            menu.selected = 2;
            menu.force_settled_for_visual_test();
            let mut tr = TextRenderer::new();
            tr.set_text_scale(scale);
            let mut scene = Scene::new();
            menu.draw(viewport, &mut scene, Affine::IDENTITY, &mut tr, &theme);
            assert_eq!(
                tr.text_scale(),
                scale,
                "the pass restores the renderer's scale"
            );
            let renderer = match w {
                1280 => &mut desktop,
                800 => &mut laptop,
                _ => &mut narrow,
            };
            let ground = theme.tokens.colors.surfaces.base;
            let path = out.join(format!("{name}.png"));
            renderer.render_png(&scene, ground, &path);
            assert!(
                path.is_file(),
                "power menu fixture missing: {}",
                path.display()
            );
            assert_power_menu_review_pixels(
                renderer,
                (&scene, ground, w),
                &menu,
                viewport,
                &theme,
                name,
            );
        }
    }

    #[expect(
        clippy::many_single_char_names,
        reason = "menu metrics and sampled colour channels read best as m and r/g/b/e"
    )]
    fn assert_power_menu_review_pixels(
        renderer: &mut crate::shell::osd::review_offscreen::OffscreenRenderer,
        paint: (&Scene, vello::peniko::Color, u32),
        menu: &PowerMenu,
        viewport: Rect,
        theme: &Theme,
        name: &str,
    ) {
        use crate::shell::osd::review_offscreen::OffscreenRenderer;
        let (scene, ground, width) = paint;
        let rgba = renderer.render_resilient(scene, ground);
        let panel = menu.panel_rect(viewport);
        let m = menu.metrics();
        let probes = [
            (
                Point::new(panel.center().x, panel.y0 + m.pad / 2.0),
                ui::chrome::shell::panel_ground(theme),
                "title band is the Panel ground",
            ),
            (
                {
                    let row = menu.action_rect(viewport, 0);
                    Point::new(row.x1 - m.pad, row.center().y)
                },
                theme.tokens.colors.lift(0.35),
                "an idle row's interior is its lift floor",
            ),
        ];
        for (at, expected, what) in probes {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "probe points are inside a positive viewport"
            )]
            let (r, g, b, _) = OffscreenRenderer::sample(&rgba, width, at.x as u32, at.y as u32);
            let e = expected.to_rgba8();
            assert!(
                r.abs_diff(e.r) <= 2 && g.abs_diff(e.g) <= 2 && b.abs_diff(e.b) <= 2,
                "{name}: {what}: got ({r},{g},{b}) expected ({},{},{})",
                e.r,
                e.g,
                e.b
            );
        }
    }
    #[test]
    fn power_menu_detail_capture_matrix_is_written() {
        use crate::shell::osd::review_offscreen::OffscreenRenderer;
        use ui::widget::{Tooltip, Widget as _};
        let out = std::env::var_os("TILO_POWER_DETAIL_SHOTS").map_or_else(
            || std::path::PathBuf::from("target/power-menu-details"),
            std::path::PathBuf::from,
        );
        std::fs::create_dir_all(&out).expect("capture output");
        for (name, theme, width, height) in [
            ("dark", Theme::dark(), 1280_u32, 800_u32),
            ("light", Theme::light(), 1280, 800),
            ("high-contrast", Theme::high_contrast(), 1280, 800),
            ("large-text", Theme::dark().with_text_scale(2.0), 800, 700),
            (
                "ui-text-scaled",
                Theme::dark().with_ui_scale(1.5).with_text_scale(1.3),
                1280,
                800,
            ),
            ("narrow", Theme::dark().with_text_scale(2.0), 320, 480),
        ] {
            let viewport = Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
            let mut text = TextRenderer::new();
            let mut menu = PowerMenu::default();
            menu.open();
            menu.force_settled_for_visual_test();
            menu.key(
                &winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowDown),
                false,
            );
            menu.draw(
                viewport,
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut text,
                &theme,
            );
            std::thread::sleep(ui::widget::tooltip::HOVER_DWELL + Duration::from_millis(20));
            let mut scene = Scene::new();
            menu.draw(viewport, &mut scene, Affine::IDENTITY, &mut text, &theme);
            let mut renderer = OffscreenRenderer::new(width, height);
            renderer.render_png(
                &scene,
                theme.tokens.colors.surfaces.base,
                out.join(format!("{name}-keyboard-description.png")),
            );
            let label = "Close applications and end this session, then return to the login screen.";
            let mut tooltip =
                Tooltip::new(label)
                    .with_delay(Duration::ZERO)
                    .with_anchor(Rect::new(
                        viewport.x1 - 60.0,
                        100.0,
                        viewport.x1 - 20.0,
                        132.0,
                    ));
            tooltip.on_hover_enter(Duration::ZERO);
            let presence = ui::presence::PresenceCtx::default();
            presence.viewport.set(Some(viewport));
            scene.reset();
            tooltip.draw(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
            renderer.render_png(
                &scene,
                theme.tokens.colors.surfaces.base,
                out.join(format!("{name}-tooltip-wrap.png")),
            );
        }
    }
    fn paint_detail(menu: &mut PowerMenu, viewport: Rect, theme: &Theme) {
        menu.draw(
            viewport,
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            theme,
        );
    }

    #[test]
    fn power_menu_complete_details_follow_focus_cancel_and_resize() {
        let mut menu = PowerMenu {
            detail_tip: TooltipHost::new().with_dwell(ui::time::Duration::ZERO),
            ..PowerMenu::default()
        };
        menu.open();
        menu.force_settled_for_visual_test();
        assert_eq!(
            menu.key(&Key::Named(NamedKey::ArrowDown), false),
            PowerMenuClick::Consumed
        );
        let theme = Theme::dark().with_text_scale(2.0);
        paint_detail(&mut menu, VIEWPORT, &theme);
        let expected = PowerMenuAction::External(os_actions::SessionAction::Logout).detail();
        assert_eq!(
            menu.detail_tip.visible().map(|(label, _)| label),
            Some(expected.as_str())
        );
        let wide_hint = menu.detail_rect.expect("wide-output description");
        assert!(
            wide_hint.x1 <= menu.panel_rect(VIEWPORT).x0,
            "a wide-output hint must not cover another action"
        );
        let small = Rect::new(0.0, 0.0, 320.0, 480.0);
        paint_detail(&mut menu, small, &theme);
        let rect = menu.detail_rect.expect("complete focused description");
        assert!(small.contains(rect.origin()) && rect.x1 <= small.x1 && rect.y1 <= small.y1);
        assert_eq!(
            menu.key(&Key::Named(NamedKey::Escape), false),
            PowerMenuClick::Consumed
        );
        assert!(menu.detail_tip.visible().is_none());
        assert_eq!(
            menu.key(&Key::Named(NamedKey::Enter), false),
            PowerMenuClick::Ignored
        );
        menu.open();
        menu.force_settled_for_visual_test();
        paint_detail(&mut menu, small, &theme);
        assert!(
            menu.detail_tip.visible().is_none(),
            "reopening must clear the previous hint"
        );
    }

    fn route_detail_pointer(
        menu: &mut PowerMenu,
        event: &WindowEvent,
        cursor: Point,
        viewport: Rect,
    ) -> Vec<LayerIntent> {
        let mut doc = automerge::AutoCommit::new();
        let mut taffy = taffy::TaffyTree::<()>::new();
        let node = taffy.new_leaf(taffy::Style::default()).expect("leaf");
        let modifiers = winit::event::Modifiers::default();
        let mut handled = false;
        let mut focus = None;
        let mut event_cx = ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &modifiers,
            state: ui::widget::Edits::lend(&mut doc),
            path: &automerge::ROOT,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: node,
            taffy: &taffy,
        };
        let mut cx = LayerCx::new(
            LayerGeometry {
                viewport,
                outputs: &[],
                focused_output: None,
            },
            Some(cursor),
            &mut event_cx,
        );
        assert_eq!(menu.handle_event(event, &mut cx), Routed::Consumed);
        cx.into_intents()
    }
    #[test]
    fn power_menu_hover_description_is_inert_and_preserves_semantic_actions() {
        let mut menu = PowerMenu {
            detail_tip: TooltipHost::new().with_dwell(ui::time::Duration::ZERO),
            ..PowerMenu::default()
        };
        menu.open();
        menu.force_settled_for_visual_test();
        let viewport = Rect::new(0.0, 0.0, 320.0, 480.0);
        let theme = Theme::dark().with_text_scale(2.0);
        paint_detail(&mut menu, viewport, &theme);
        let cursor = menu.action_rect(viewport, 1).center();
        let hover = ui::synthetic_input::cursor_moved(cursor.x, cursor.y);
        assert_eq!(
            route_detail_pointer(&mut menu, &hover, cursor, viewport),
            [LayerIntent::Repaint]
        );
        paint_detail(&mut menu, viewport, &theme);
        let rect = menu.detail_rect.expect("hover hint");
        assert_eq!(
            route_detail_pointer(&mut menu, &hover, rect.center(), viewport),
            [LayerIntent::Repaint]
        );
        paint_detail(&mut menu, viewport, &theme);
        assert!(
            menu.detail_tip.visible().is_some(),
            "reading the hint keeps it visible"
        );
        let press = ui::synthetic_input::mouse_press();
        for _ in 0..2 {
            assert!(route_detail_pointer(&mut menu, &press, rect.center(), viewport).is_empty());
        }
        assert!(
            menu.open,
            "a hint click must not execute the row underneath"
        );
        assert!(matches!(
            menu.semantic_intent("power_logout"),
            Some(LayerIntent::Session(os_actions::SessionAction::Logout))
        ));
        let node = menu.accessibility_snapshot().expect("visible menu");
        assert!(
            node.children
                .iter()
                .any(|child| child.action_target.as_deref() == Some("power_logout"))
        );
        let leave = WindowEvent::CursorLeft {
            device_id: winit::event::DeviceId::dummy(),
        };
        assert_eq!(
            route_detail_pointer(&mut menu, &leave, rect.center(), viewport),
            [LayerIntent::Repaint]
        );
        paint_detail(&mut menu, viewport, &theme);
        assert!(menu.detail_tip.visible().is_none());
        menu.detach_from_host();
        assert!(menu.semantic_intent("power_restart").is_none());
        assert!(menu.semantic_intent("power_switch_user").is_none());
        menu.close();
        assert!(menu.semantic_intent("power_logout").is_none());
    }
}
