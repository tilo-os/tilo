//! Wireless projection picker UI.
//!
//! Owns projection state, animation, rendering, credential editing, and hit-testing.
//! The overlay host only coordinates global visibility and invalidation.

use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{TextInput, Widget};

use crate::shell::projection::{
    DisplayResolutionLimit, ProjectionBackend, ProjectionController, ProjectionMode,
    ProjectionProtocol, ProjectionStatus,
};

const OPEN_ANIM_MS: u64 = 220;
const SCRIM_ALPHA: f32 = 0.45;
const SURFACE_ALPHA: f32 = ui::chrome::shell::CHROME_SURFACE_ALPHA;

#[allow(clippy::cast_precision_loss, reason = "device counts are small")]
fn usize_to_f64_lossy(n: usize) -> f64 {
    n as f64
}

#[allow(clippy::cast_possible_truncation, reason = "screen pixels fit f32")]
fn f64_to_f32_lossy(v: f64) -> f32 {
    v as f32
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "float-to-usize casts saturate and caller clamps the result"
)]
fn f64_floor_to_usize_saturating(v: f64) -> usize {
    v.floor() as usize
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "alpha is clamped to the u8 range"
)]
fn f32_to_u8_saturating(v: f32) -> u8 {
    v.clamp(0.0, 255.0) as u8
}

fn panel_surface(theme: &Theme) -> Color {
    ui::chrome::shell::panel_ground(theme).with_alpha_factor(SURFACE_ALPHA)
}

fn draw_scrim(scene: &mut Scene, transform: Affine, rect: Rect, openness: f32, theme: &Theme) {
    let alpha = f32_to_u8_saturating((SCRIM_ALPHA * openness.clamp(0.0, 1.0) * 255.0).round());
    if alpha == 0 {
        return;
    }
    let c = theme.tokens.colors.surfaces.scrim.to_rgba8();
    scene.fill(
        Fill::NonZero,
        transform,
        Color::from_rgba8(c.r, c.g, c.b, alpha),
        None,
        &rect,
    );
}

/// Result of routing an input event through the open projection sheet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EventOutcome {
    pub consumed: bool,
    pub close_requested: bool,
    pub invalidate: bool,
}

impl EventOutcome {
    fn consumed(invalidate: bool) -> Self {
        Self {
            consumed: true,
            close_requested: false,
            invalidate,
        }
    }

    fn close() -> Self {
        Self {
            consumed: true,
            close_requested: true,
            invalidate: true,
        }
    }
}

pub struct ProjectionOverlay {
    controller: ProjectionController,
    transition: ui::Transition,
    was_open: bool,
    host_rect: Rect,
    device_rects: Vec<Rect>,
    mode_rects: [Rect; 3],
    refresh_rect: Rect,
    close_rect: Rect,
    audio_rect: Rect,
    adaptive_rect: Rect,
    resolution_rect: Rect,
    fps_rect: Rect,
    credential_input: TextInput,
    credential_rect: Rect,
}

impl Default for ProjectionOverlay {
    fn default() -> Self {
        Self::from_controller(ProjectionController::default())
    }
}

impl ProjectionOverlay {
    pub(crate) fn from_controller(controller: ProjectionController) -> Self {
        Self {
            controller,
            transition: ui::Transition::closed(),
            was_open: false,
            host_rect: Rect::ZERO,
            device_rects: Vec::new(),
            mode_rects: [Rect::ZERO; 3],
            refresh_rect: Rect::ZERO,
            close_rect: Rect::ZERO,
            audio_rect: Rect::ZERO,
            adaptive_rect: Rect::ZERO,
            resolution_rect: Rect::ZERO,
            fps_rect: Rect::ZERO,
            credential_input: TextInput::local()
                .with_id("projection-airplay-credential")
                .with_placeholder("PIN or receiver password")
                .with_secret(true)
                .with_enter_submits(true),
            credential_rect: Rect::ZERO,
        }
    }

    pub fn with_backend(backend: Box<dyn ProjectionBackend>) -> Self {
        Self::from_controller(ProjectionController::new(backend))
    }

    pub fn controller(&self) -> &ProjectionController {
        &self.controller
    }

    pub fn capture_requested(&self) -> bool {
        let (ProjectionStatus::Connecting { device_id }
        | ProjectionStatus::Connected { device_id }) = &self.controller.status
        else {
            return false;
        };
        self.controller
            .devices
            .iter()
            .find(|device| &device.id == device_id)
            .is_some_and(|device| {
                matches!(
                    device.protocol,
                    ProjectionProtocol::Chromecast | ProjectionProtocol::FCast
                )
            })
    }

    pub fn contains(host_rect: Rect, point: Point) -> bool {
        Self::rect_for_host(host_rect).contains(point)
    }

    pub fn needs_animation_frame(&self, now: Instant) -> bool {
        self.transition.is_animating(now)
            || matches!(
                self.controller.status,
                ProjectionStatus::Scanning | ProjectionStatus::Connecting { .. }
            )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "widget event context passes through unchanged"
    )]
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
        cursor: Option<Point>,
        host_rect: Rect,
        open: bool,
    ) -> EventOutcome {
        if !open {
            return EventOutcome::default();
        }
        self.host_rect = host_rect;
        if self.airplay_selected()
            && let Some(outcome) = self.handle_credential_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                focus_target,
                cursor,
            )
        {
            return outcome;
        }
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => self.handle_key(logical_key, modifiers),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => cursor.map_or_else(
                || EventOutcome::consumed(false),
                |point| self.handle_click(point),
            ),
            WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. } => EventOutcome::consumed(false),
            _ => EventOutcome::default(),
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "TextInput event context passes through unchanged"
    )]
    fn handle_credential_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
        cursor: Option<Point>,
    ) -> Option<EventOutcome> {
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
        {
            self.credential_input
                .set_focus(cursor.is_some_and(|p| self.credential_rect.contains(p)));
        }
        let mut input_handled = false;
        let mut input_taffy = TaffyTree::<()>::new();
        let Ok(input_node) = input_taffy.new_leaf(Style::default()) else {
            return Some(EventOutcome::default());
        };
        self.credential_input.handle_event(
            event,
            scale_factor,
            &modifiers,
            state,
            path,
            &mut input_handled,
            focus_target,
            input_node,
            &input_taffy,
        );
        if let Some(value) = self.credential_input.take_dirty() {
            self.controller.options.credential = (!value.is_empty()).then_some(value);
        }
        if self.credential_input.take_submit() {
            self.controller.connect_selected();
            self.credential_input.set_text("");
            return Some(EventOutcome::consumed(true));
        }
        input_handled.then(|| EventOutcome::consumed(false))
    }

    fn handle_key(&mut self, key: &Key, modifiers: Modifiers) -> EventOutcome {
        let super_down = modifiers.state().super_key();
        match key {
            Key::Named(NamedKey::Escape) => return EventOutcome::close(),
            Key::Character(s) if super_down && s.eq_ignore_ascii_case("p") => {
                return EventOutcome::close();
            }
            Key::Named(NamedKey::ArrowUp) => self.controller.move_selection(-1),
            Key::Named(NamedKey::ArrowDown) => self.controller.move_selection(1),
            Key::Named(NamedKey::Enter) => {
                let selected_id = self
                    .controller
                    .devices
                    .get(self.controller.selected)
                    .map(|device| device.id.as_str());
                if matches!(
                    &self.controller.status,
                    ProjectionStatus::Connected { device_id }
                        if Some(device_id.as_str()) == selected_id
                ) {
                    self.controller.disconnect();
                } else {
                    self.controller.connect_selected();
                }
            }
            Key::Named(NamedKey::Space) => {
                self.controller.options.audio = !self.controller.options.audio;
            }
            Key::Character(s) if s.eq_ignore_ascii_case("r") => self.controller.refresh(),
            Key::Character(s) if s.eq_ignore_ascii_case("a") => {
                self.controller.options.stream_policy.adaptive =
                    !self.controller.options.stream_policy.adaptive;
            }
            Key::Character(s) if s.eq_ignore_ascii_case("q") => self.cycle_resolution(),
            Key::Character(s) if s.eq_ignore_ascii_case("f") => self.toggle_fps(),
            Key::Character(s) if matches!(s.as_str(), "1" | "2" | "3") => {
                let index = s.parse::<usize>().unwrap_or(1).saturating_sub(1);
                if let Some(mode) = ProjectionMode::ALL.get(index)
                    && self.controller.selected_device_supports(*mode)
                {
                    self.controller.options.mode = *mode;
                }
            }
            _ => {}
        }
        EventOutcome::consumed(true)
    }

    fn handle_click(&mut self, point: Point) -> EventOutcome {
        if self.close_rect.contains(point) || !self.sheet_rect().contains(point) {
            return EventOutcome::close();
        } else if self.refresh_rect.contains(point) {
            self.controller.refresh();
        } else if self.audio_rect.contains(point) {
            self.controller.options.audio = !self.controller.options.audio;
        } else if self.adaptive_rect.contains(point) {
            self.controller.options.stream_policy.adaptive =
                !self.controller.options.stream_policy.adaptive;
        } else if self.resolution_rect.contains(point) {
            self.cycle_resolution();
        } else if self.fps_rect.contains(point) {
            self.toggle_fps();
        } else if let Some(index) = self
            .mode_rects
            .iter()
            .position(|rect| rect.area() > 0.0 && rect.contains(point))
        {
            if let Some(&mode) = ProjectionMode::ALL.get(index)
                && self.controller.selected_device_supports(mode)
            {
                self.controller.options.mode = mode;
            }
        } else if let Some(index) = self
            .device_rects
            .iter()
            .position(|rect| rect.contains(point))
        {
            let selection_changed = self.controller.selected != index;
            self.controller.selected = index;
            if selection_changed {
                self.controller.options.credential = None;
                self.credential_input.set_text("");
            }
            let id = self
                .controller
                .devices
                .get(index)
                .map(|device| device.id.as_str());
            if matches!(
                &self.controller.status,
                ProjectionStatus::Connected { device_id } if Some(device_id.as_str()) == id
            ) {
                self.controller.disconnect();
            } else if !selection_changed
                || self
                    .controller
                    .devices
                    .get(index)
                    .is_none_or(|device| device.protocol != ProjectionProtocol::AirPlay)
            {
                self.controller.connect_selected();
                self.credential_input.set_text("");
            } else {
                self.credential_input.set_focus(true);
            }
        }
        EventOutcome::consumed(true)
    }

    fn cycle_resolution(&mut self) {
        let limits = DisplayResolutionLimit::ALL;
        let current = limits
            .iter()
            .position(|limit| *limit == self.controller.options.stream_policy.resolution_limit)
            .unwrap_or(0);
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "max(1) keeps the divisor nonzero"
        )]
        let next = current.wrapping_add(1) % limits.len().max(1);
        if let Some(&limit) = limits.get(next) {
            self.controller.options.stream_policy.resolution_limit = limit;
        }
    }

    fn toggle_fps(&mut self) {
        self.controller.options.stream_policy.max_fps =
            if self.controller.options.stream_policy.max_fps == 60 {
                30
            } else {
                60
            };
    }

    /// Paint the complete sheet and return whether worker polling changed state.
    #[allow(
        clippy::too_many_arguments,
        reason = "draw context passes through unchanged"
    )]
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        host_rect: Rect,
        open: bool,
    ) -> bool {
        // Theme type tokens already carry the user's text scale, and this
        // sheet lays out and paints with those resolved sizes; the renderer
        // must not scale them a second time (HIG §7a), so it is reset for
        // the pass exactly as the lens pass resets it, and restored after.
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        let changed = self.draw_at_theme_scale(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            host_rect,
            open,
        );
        text_renderer.set_text_scale(renderer_scale);
        changed
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "draw context passes through unchanged"
    )]
    fn draw_at_theme_scale(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        host_rect: Rect,
        open: bool,
    ) -> bool {
        self.host_rect = host_rect;
        let changed = self.controller.poll();
        if open && !self.was_open {
            self.controller.refresh();
        }
        self.was_open = open;
        self.transition.set_open(
            open,
            if theme.reduce_motion {
                Duration::ZERO
            } else {
                Duration::from_millis(OPEN_ANIM_MS)
            },
            ui::Curve::EaseOut,
        );
        let now = Instant::now();
        if self.transition.is_visible(now) {
            let openness = self.transition.value(now);
            draw_scrim(scene, transform, host_rect, openness, theme);
            let sheet = self.sheet_rect();
            let slide = (1.0 - f64::from(openness)) * (sheet.width() * 0.22);
            let sheet_transform = transform.pre_translate((slide, 0.0).into());
            if openness < 1.0 {
                scene.push_layer(
                    Fill::NonZero,
                    BlendMode::default(),
                    openness,
                    transform,
                    &host_rect,
                );
            }
            self.draw_content(scene, sheet_transform, text_renderer, theme, presence);
            if openness < 1.0 {
                scene.pop_layer();
            }
        } else {
            self.clear_geometry();
        }
        changed
    }

    fn clear_geometry(&mut self) {
        self.device_rects.clear();
        self.mode_rects = [Rect::ZERO; 3];
        self.refresh_rect = Rect::ZERO;
        self.close_rect = Rect::ZERO;
        self.audio_rect = Rect::ZERO;
        self.adaptive_rect = Rect::ZERO;
        self.resolution_rect = Rect::ZERO;
        self.fps_rect = Rect::ZERO;
        self.credential_rect = Rect::ZERO;
    }

    fn rect_for_host(host_rect: Rect) -> Rect {
        let margin = 20.0;
        let width = 432.0_f64.min((host_rect.width() - margin * 2.0).max(280.0));
        let top = host_rect.y0 + 56.0;
        let bottom = (top + 620.0).min(host_rect.y1 - 24.0);
        Rect::new(
            host_rect.x1 - margin - width,
            top,
            host_rect.x1 - margin,
            bottom,
        )
    }

    fn sheet_rect(&self) -> Rect {
        Self::rect_for_host(self.host_rect)
    }

    fn row_rect(&self, index: usize) -> Rect {
        let r = self.sheet_rect();
        let y0 = self.device_rows_top() + usize_to_f64_lossy(index) * 64.0;
        Rect::new(r.x0 + 16.0, y0, r.x1 - 16.0, y0 + 56.0)
    }

    fn airplay_selected(&self) -> bool {
        self.controller
            .devices
            .get(self.controller.selected)
            .is_some_and(|device| {
                device.protocol == crate::shell::projection::ProjectionProtocol::AirPlay
            })
    }

    fn device_rows_top(&self) -> f64 {
        self.sheet_rect().y0
            + if self.airplay_selected() {
                366.0
            } else {
                270.0
            }
    }

    fn draw_content(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let r = self.sheet_rect();
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography.scale;

        self.draw_header(scene, transform, text_renderer, theme, r);
        self.draw_mode_picker(scene, transform, text_renderer, theme, r);
        self.draw_audio_toggle(scene, transform, text_renderer, theme, r);
        self.draw_stream_chips(scene, transform, text_renderer, theme, r);
        self.draw_credential_field(scene, transform, text_renderer, theme, presence, r);
        self.draw_devices_heading(scene, transform, text_renderer, theme, r);

        self.device_rects.clear();
        let rows_top = self.device_rows_top();
        let max_rows = f64_floor_to_usize_saturating((r.y1 - rows_top - 58.0) / 64.0).max(1);
        if self.controller.devices.is_empty() {
            self.draw_devices_empty(scene, transform, text_renderer, theme, r);
        } else {
            self.draw_devices_list(scene, transform, text_renderer, theme, max_rows);
        }

        let footer = "Miracast · Google Cast · AirPlay · FCast";
        let (fw, _) = text_renderer.measure(footer, type_scale.xs, None);
        text_renderer.draw_text(
            scene,
            footer,
            type_scale.xs,
            colors.text_dim,
            transform.pre_translate((r.center().x - f64::from(fw) / 2.0, r.y1 - 28.0).into()),
            None,
        );
    }

    /// The projection sheet's card, title, subtitle and close button.
    fn draw_header(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography.scale;
        let radius = ui::chrome::ChromeRole::Panel.radius(theme, r.height());
        let card = RoundedRect::from_rect(r, radius);
        ui::chrome::shell::panel_shadow(scene, transform, r, radius, theme, 1.0);
        scene.fill(Fill::NonZero, transform, panel_surface(theme), None, &card);
        // Panel edge derived against the ground (HIG §9.5), not declared.
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            ui::chrome::shell::panel_border(theme),
            None,
            &card,
        );

        text_renderer.draw_text_weighted(
            scene,
            "Project",
            type_scale.xl,
            colors.text_primary,
            transform.pre_translate((r.x0 + 20.0, r.y0 + 18.0).into()),
            None,
            TextWeight::Semibold,
        );
        text_renderer.draw_text(
            scene,
            "Choose how this desktop appears, then pick a display.",
            type_scale.xs,
            colors.text_dim,
            transform.pre_translate((r.x0 + 20.0, r.y0 + 49.0).into()),
            Some(f64_to_f32_lossy(r.width() - 88.0)),
        );

        self.close_rect = Rect::new(r.x1 - 48.0, r.y0 + 12.0, r.x1 - 12.0, r.y0 + 48.0);
        ui::icons::draw_icon(
            scene,
            transform,
            self.close_rect.inset(-9.0),
            ui::icons::Icon::X,
            colors.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
    }

    /// Three familiar Win+P layout choices. A network stream is not
    /// itself a compositor output, so unsupported choices are visibly
    /// disabled.
    fn draw_mode_picker(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        use crate::shell::projection::ProjectionMode;

        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography.scale;
        let modes_top = r.y0 + 82.0;
        let gap = 8.0;
        let cell_w = (r.width() - 40.0 - gap * 2.0) / 3.0;
        // Control role (HIG §9.1/§9.3): the chosen layout is a lift with the
        // tier's hairline, never an accent wash (the accent means live
        // state, §9.4); idle cells rest on the lift floor with an edge
        // derived from that floor (§9.5).
        let floor = colors.lift(0.35);
        let idle_edge = ui::surface::edge_on(floor);
        for (i, mode) in ProjectionMode::ALL.iter().enumerate() {
            let cell = Rect::new(
                r.x0 + 20.0 + usize_to_f64_lossy(i) * (cell_w + gap),
                modes_top,
                r.x0 + 20.0 + usize_to_f64_lossy(i) * (cell_w + gap) + cell_w,
                modes_top + 66.0,
            );
            if let Some(slot) = self.mode_rects.get_mut(i) {
                *slot = cell;
            }
            let selected = self.controller.options.mode == *mode;
            let supported = self.controller.selected_device_supports(*mode);
            let radius = ui::chrome::ChromeRole::Control.radius(theme, cell.height());
            let shape = RoundedRect::from_rect(cell, radius);
            let tier =
                ui::chrome::selection_tier(theme, ui::chrome::SelectionState::active(selected));
            scene.fill(
                Fill::NonZero,
                transform,
                if selected { tier.fill } else { floor },
                None,
                &shape,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                tier.border.unwrap_or(idle_edge),
                None,
                &ui::surface::edge_outline(cell, radius),
            );
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::new(
                    cell.x0 + 12.0,
                    cell.y0 + 10.0,
                    cell.x0 + 32.0,
                    cell.y0 + 30.0,
                ),
                ui::icons::Icon::Monitor,
                if selected {
                    colors.text_primary
                } else if !supported {
                    colors.text_dim.with_alpha_factor(0.42)
                } else {
                    colors.text_dim
                },
                1.5,
            );
            text_renderer.draw_text_weighted(
                scene,
                mode.label(),
                type_scale.xs,
                if supported {
                    colors.text_primary
                } else {
                    colors.text_dim.with_alpha_factor(0.52)
                },
                transform.pre_translate((cell.x0 + 12.0, cell.y0 + 39.0).into()),
                Some(f64_to_f32_lossy(cell.width() - 18.0)),
                TextWeight::Medium,
            );
        }
    }

    /// The "Share audio" row: glyph, label and an on/off toggle knob.
    fn draw_audio_toggle(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography.scale;
        self.audio_rect = Rect::new(r.x0 + 20.0, r.y0 + 164.0, r.x1 - 62.0, r.y0 + 202.0);
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::new(r.x0 + 22.0, r.y0 + 173.0, r.x0 + 42.0, r.y0 + 193.0),
            if self.controller.options.audio {
                ui::icons::Icon::Volume2
            } else {
                ui::icons::Icon::VolumeX
            },
            colors.text_dim,
            1.5,
        );
        text_renderer.draw_text(
            scene,
            "Share audio",
            type_scale.sm,
            colors.text_primary,
            transform.pre_translate((r.x0 + 52.0, r.y0 + 174.0).into()),
            None,
        );
        let toggle = Rect::new(r.x1 - 58.0, r.y0 + 170.0, r.x1 - 20.0, r.y0 + 192.0);
        // The shared switch (HIG §9.9): track, knob and their palette come
        // from `Toggle`, not a hand-rolled accent fill over a ramp slot.
        ui::widget::Toggle::paint(
            scene,
            transform,
            theme,
            &ui::widget::ToggleFace::new(toggle, self.controller.options.audio),
        );
    }

    /// The Stream row's three chips: Auto/Fixed, resolution limit, fps.
    fn draw_stream_chips(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography.scale;
        text_renderer.draw_text_weighted(
            scene,
            "Stream",
            type_scale.xs,
            colors.text_dim,
            transform.pre_translate((r.x0 + 20.0, r.y0 + 218.0).into()),
            None,
            TextWeight::Medium,
        );
        let x0 = r.x0 + 76.0;
        let labels = [
            if self.controller.options.stream_policy.adaptive {
                "Auto".to_owned()
            } else {
                "Fixed".to_owned()
            },
            self.controller
                .options
                .stream_policy
                .resolution_limit
                .label()
                .to_owned(),
            format!("{} fps", self.controller.options.stream_policy.max_fps),
        ];
        let widths = [76.0, 88.0, 76.0];
        let mut chip_x = x0;
        // Control chips at the Control radius (9 was off the radius scale):
        // the engaged "Auto" choice is a lift with the tier's hairline, idle
        // chips rest on the lift floor with a derived edge.
        let floor = colors.lift(0.35);
        let idle_edge = ui::surface::edge_on(floor);
        for (i, (label, width)) in labels.iter().zip(widths).enumerate() {
            let chip = Rect::new(chip_x, r.y0 + 208.0, chip_x + width, r.y0 + 238.0);
            match i {
                0 => self.adaptive_rect = chip,
                1 => self.resolution_rect = chip,
                _ => self.fps_rect = chip,
            }
            let on = i == 0 && self.controller.options.stream_policy.adaptive;
            let radius = ui::chrome::ChromeRole::Control.radius(theme, chip.height());
            let shape = RoundedRect::from_rect(chip, radius);
            let tier = ui::chrome::selection_tier(theme, ui::chrome::SelectionState::active(on));
            scene.fill(
                Fill::NonZero,
                transform,
                if on { tier.fill } else { floor },
                None,
                &shape,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                tier.border.unwrap_or(idle_edge),
                None,
                &ui::surface::edge_outline(chip, radius),
            );
            let (label_w, label_h) = text_renderer.measure(label, type_scale.xs, None);
            text_renderer.draw_text_weighted(
                scene,
                label,
                type_scale.xs,
                colors.text_primary,
                transform.pre_translate(
                    (
                        chip.center().x - f64::from(label_w) / 2.0,
                        chip.center().y - f64::from(label_h) / 2.0,
                    )
                        .into(),
                ),
                None,
                TextWeight::Medium,
            );
            chip_x += width + 6.0;
        }
    }

    /// The AirPlay-only credential field, shown when the selected device
    /// needs a PIN or password; cleared and unfocused otherwise.
    fn draw_credential_field(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        r: Rect,
    ) {
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography.scale;
        let airplay_selected = self.airplay_selected();
        if airplay_selected {
            text_renderer.draw_text_weighted(
                scene,
                "Receiver PIN or password",
                type_scale.xs,
                colors.text_primary,
                transform.pre_translate((r.x0 + 20.0, r.y0 + 253.0).into()),
                None,
                TextWeight::Semibold,
            );
            self.credential_rect = Rect::new(r.x0 + 20.0, r.y0 + 274.0, r.x1 - 20.0, r.y0 + 316.0);
            self.credential_input
                .apply_layout_external(self.credential_rect);
            self.credential_input
                .draw(scene, transform, text_renderer, theme, presence);
            text_renderer.draw_text(
                scene,
                "Leave blank unless the receiver asks. Enter connects.",
                type_scale.xs,
                colors.text_dim,
                transform.pre_translate((r.x0 + 20.0, r.y0 + 324.0).into()),
                Some(f64_to_f32_lossy(r.width() - 40.0)),
            );
        } else {
            self.credential_rect = Rect::ZERO;
            self.credential_input.set_focus(false);
        }
    }

    /// "Available displays" heading plus the refresh button beside it.
    fn draw_devices_heading(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography.scale;
        let devices_heading_y = self.device_rows_top() - 16.0;
        text_renderer.draw_text_weighted(
            scene,
            "Available displays",
            type_scale.sm,
            colors.text_primary,
            transform.pre_translate((r.x0 + 20.0, devices_heading_y).into()),
            None,
            TextWeight::Semibold,
        );
        self.refresh_rect = Rect::new(
            r.x1 - 48.0,
            devices_heading_y - 8.0,
            r.x1 - 12.0,
            devices_heading_y + 28.0,
        );
        ui::icons::draw_icon(
            scene,
            transform,
            self.refresh_rect.inset(-9.0),
            ui::icons::Icon::Refresh,
            colors.text_dim,
            1.5,
        );
    }

    /// An honest zero-state for the device list: what's happening
    /// (scanning / errored / nothing found) and what to do about it.
    fn draw_devices_empty(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        use crate::shell::projection::ProjectionStatus;

        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography.scale;
        let (message, detail) = match &self.controller.status {
            ProjectionStatus::Scanning => (
                "Looking for displays…",
                "Keep the receiver awake and on the same network.",
            ),
            ProjectionStatus::Error(error) => ("Couldn't scan", error.as_str()),
            _ => (
                "No displays found",
                "Try Refresh, or check Wi-Fi and the receiver's casting mode.",
            ),
        };
        let icon = Rect::new(
            r.center().x - 22.0,
            r.y0 + 274.0,
            r.center().x + 22.0,
            r.y0 + 318.0,
        );
        ui::icons::draw_icon(
            scene,
            transform,
            icon,
            ui::icons::Icon::Monitor,
            colors.text_dim,
            1.5,
        );
        let (mw, _) = text_renderer.measure(message, type_scale.sm, None);
        text_renderer.draw_text_weighted(
            scene,
            message,
            type_scale.sm,
            colors.text_primary,
            transform.pre_translate((r.center().x - f64::from(mw) / 2.0, r.y0 + 330.0).into()),
            None,
            TextWeight::Medium,
        );
        let detail =
            text_renderer.elide_to_width(detail, type_scale.xs, f64_to_f32_lossy(r.width() - 56.0));
        let (dw, _) = text_renderer.measure(&detail, type_scale.xs, None);
        text_renderer.draw_text(
            scene,
            &detail,
            type_scale.xs,
            colors.text_dim,
            transform.pre_translate((r.center().x - f64::from(dw) / 2.0, r.y0 + 354.0).into()),
            None,
        );
    }

    /// One row per discovered device, up to `max_rows`: icon, name,
    /// status sub-line and a Connect/Stop/Wait action pill.
    fn draw_devices_list(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        max_rows: usize,
    ) {
        use crate::shell::projection::ProjectionStatus;

        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography.scale;
        // Control role: the selected device is a lift (HIG §9.3); every row
        // carries an edge derived from its fill (§9.5).
        let floor = colors.lift(0.35);
        let idle_edge = ui::surface::edge_on(floor);
        for (i, device) in self.controller.devices.iter().take(max_rows).enumerate() {
            let row = self.row_rect(i);
            self.device_rects.push(row);
            let selected = self.controller.selected == i;
            let connected = matches!(&self.controller.status, ProjectionStatus::Connected { device_id } if device_id == &device.id);
            let connecting = matches!(&self.controller.status, ProjectionStatus::Connecting { device_id } if device_id == &device.id);
            let radius = ui::chrome::ChromeRole::Control.radius(theme, row.height());
            let shape = RoundedRect::from_rect(row, radius);
            let tier =
                ui::chrome::selection_tier(theme, ui::chrome::SelectionState::active(selected));
            scene.fill(
                Fill::NonZero,
                transform,
                if selected { tier.fill } else { floor },
                None,
                &shape,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                tier.border.unwrap_or(idle_edge),
                None,
                &ui::surface::edge_outline(row, radius),
            );
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::new(row.x0 + 12.0, row.y0 + 17.0, row.x0 + 34.0, row.y0 + 39.0),
                ui::icons::Icon::Monitor,
                if connected {
                    colors.semantic.success
                } else {
                    colors.text_dim
                },
                1.5,
            );
            let name = text_renderer.elide_to_width(
                &device.name,
                type_scale.sm,
                f64_to_f32_lossy(row.width() - 142.0),
            );
            text_renderer.draw_text_weighted(
                scene,
                &name,
                type_scale.sm,
                colors.text_primary,
                transform.pre_translate((row.x0 + 46.0, row.y0 + 8.0).into()),
                None,
                TextWeight::Medium,
            );
            let sub = if connected {
                "Connected".to_string()
            } else if connecting {
                "Connecting…".to_string()
            } else if device.detail.is_empty() {
                device.protocol.label().to_string()
            } else {
                format!("{} · {}", device.protocol.label(), device.detail)
            };
            text_renderer.draw_text(
                scene,
                &sub,
                type_scale.xs,
                if connected {
                    colors.semantic.success
                } else {
                    colors.text_dim
                },
                transform.pre_translate((row.x0 + 46.0, row.y0 + 31.0).into()),
                Some(f64_to_f32_lossy(row.width() - 142.0)),
            );
            Self::draw_device_action(
                scene,
                transform,
                text_renderer,
                theme,
                row,
                connected,
                connecting,
            );
        }
    }

    /// The Connect/Stop/Wait action pill on a device row.
    fn draw_device_action(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        connected: bool,
        connecting: bool,
    ) {
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography.scale;
        let action = Rect::new(row.x1 - 86.0, row.y0 + 11.0, row.x1 - 10.0, row.y1 - 11.0);
        scene.fill(
            Fill::NonZero,
            transform,
            if connected {
                colors.semantic.danger.with_alpha_factor(0.16)
            } else {
                colors.accent[5].with_alpha_factor(0.24)
            },
            None,
            &RoundedRect::from_rect(action, 10.0),
        );
        let label = if connected {
            "Stop"
        } else if connecting {
            "Wait"
        } else {
            "Connect"
        };
        let (lw, lh) = text_renderer.measure(label, type_scale.xs, None);
        text_renderer.draw_text_weighted(
            scene,
            label,
            type_scale.xs,
            if connected {
                colors.semantic.danger
            } else {
                colors.text_primary
            },
            transform.pre_translate(
                (
                    action.center().x - f64::from(lw) / 2.0,
                    action.center().y - f64::from(lh) / 2.0,
                )
                    .into(),
            ),
            None,
            TextWeight::Medium,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// HIG §9.5: the sheet's Panel border on its ground, and the derived edge
    /// of its lift-floor cells and rows, must register on every palette.
    #[test]
    fn sheet_cells_keep_an_edge_on_every_palette() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let ground = ui::chrome::shell::panel_ground(&theme);
            let floor = theme.tokens.colors.lift(0.35);
            for (what, edge, fill) in [
                (
                    "panel border",
                    ui::chrome::shell::panel_border(&theme),
                    ground,
                ),
                ("mode cell / device row", ui::surface::edge_on(floor), floor),
            ] {
                let delta = ui::surface::edge_delta(edge, fill);
                assert!(
                    delta >= ui::tokens::colors::HAIRLINE_MIN_DELTA,
                    "{}: the {what} must separate from its fill (Δ luminance {delta:.4})",
                    theme.name
                );
            }
        }
    }

    fn airplay_device() -> crate::shell::projection::ProjectionDevice {
        crate::shell::projection::ProjectionDevice {
            id: "airplay:test".to_owned(),
            name: "Living Room".to_owned(),
            protocol: ProjectionProtocol::AirPlay,
            detail: "Apple TV".to_owned(),
            modes: vec![ProjectionMode::Mirror],
            addresses: vec!["192.0.2.8".to_owned()],
            port: 7000,
        }
    }

    #[test]
    fn airplay_selection_owns_masked_pairing_geometry() {
        let mut overlay = ProjectionOverlay {
            host_rect: Rect::new(0.0, 0.0, 1280.0, 800.0),
            ..ProjectionOverlay::default()
        };
        overlay.controller.devices = vec![airplay_device()];
        overlay.controller.selected = 0;
        assert!(overlay.airplay_selected());
        assert!(overlay.credential_input.is_secret());
        let expected = overlay.sheet_rect().y0 + 366.0;
        assert!((overlay.device_rows_top() - expected).abs() < f64::EPSILON);
    }

    #[test]
    fn keyboard_controls_update_owned_options_and_close_intent() {
        let mut overlay = ProjectionOverlay::default();
        let none = Modifiers::default();
        let old_audio = overlay.controller.options.audio;
        assert!(
            overlay
                .handle_key(&Key::Named(NamedKey::Space), none)
                .consumed
        );
        assert_ne!(overlay.controller.options.audio, old_audio);
        let old_fps = overlay.controller.options.stream_policy.max_fps;
        overlay.handle_key(&Key::Character("f".into()), none);
        assert_ne!(overlay.controller.options.stream_policy.max_fps, old_fps);
        assert!(
            overlay
                .handle_key(&Key::Named(NamedKey::Escape), none)
                .close_requested
        );
    }

    #[test]
    fn outside_click_requests_host_owned_visibility_close() {
        let mut overlay = ProjectionOverlay {
            host_rect: Rect::new(0.0, 0.0, 1280.0, 800.0),
            ..ProjectionOverlay::default()
        };
        assert_eq!(
            overlay.handle_click(Point::new(10.0, 10.0)),
            EventOutcome::close()
        );
    }
}
