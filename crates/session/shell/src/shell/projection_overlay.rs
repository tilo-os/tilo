//! The Super+P display sheet: where the desktop appears.
//!
//! The upper section switches the whole desktop between the four familiar
//! layouts — extend, mirror, only this screen, only external — and stages
//! the choice as a display transaction the compositor applies at once and
//! restores unless the user keeps it. Wireless receivers are the lower
//! section of the same sheet, so "put my desktop on that screen" has one
//! home whether the screen is on a cable or on the network.
//!
//! The sheet owns its state, animation, rendering, credential editing and
//! hit-testing. The overlay host only coordinates visibility and invalidation.

use std::time::{Duration, Instant};

use settings_store::display_mode::{self, DisplayLayoutError, DisplayLayoutMode};
use settings_store::hardware::{
    DisplayConfig, DisplayHardwareSnapshot, DisplayOutputSnapshot, DisplayTransaction,
    DisplayTransactionState,
};
use taffy::{Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::i18n::ArgValue;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{EventCtx, TextInput, Widget};

use crate::shell::i18n::{t, t_args};
use crate::shell::projection::{
    DisplayResolutionLimit, ProjectionBackend, ProjectionController, ProjectionMode,
    ProjectionProtocol, ProjectionStatus,
};
use ui::TextRole;
use ui::widget::ButtonRole;

const OPEN_ANIM_MS: u64 = 220;
const SCRIM_ALPHA: f32 = 0.45;
/// How often an open sheet re-reads the compositor's display snapshot and
/// the transaction it staged. Bounded so an open sheet never turns into
/// per-frame settings I/O.
const DISPLAY_POLL: Duration = Duration::from_millis(250);

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

/// Title key of a layout.
pub(crate) fn layout_title_key(mode: DisplayLayoutMode) -> &'static str {
    match mode {
        DisplayLayoutMode::Extend => "display-mode-extend",
        DisplayLayoutMode::Mirror => "display-mode-mirror",
        DisplayLayoutMode::BuiltinOnly => "display-mode-builtin-only",
        DisplayLayoutMode::ExternalOnly => "display-mode-external-only",
    }
}

fn layout_detail_key(mode: DisplayLayoutMode) -> &'static str {
    match mode {
        DisplayLayoutMode::Extend => "display-mode-extend-detail",
        DisplayLayoutMode::Mirror => "display-mode-mirror-detail",
        DisplayLayoutMode::BuiltinOnly => "display-mode-builtin-only-detail",
        DisplayLayoutMode::ExternalOnly => "display-mode-external-only-detail",
    }
}

fn unavailable_key(error: &DisplayLayoutError) -> &'static str {
    match error {
        DisplayLayoutError::NoBuiltin => "display-mode-unavailable-builtin",
        DisplayLayoutError::NoCommonMirrorMode => "display-mode-unavailable-mirror",
        DisplayLayoutError::SingleDisplay
        | DisplayLayoutError::NoHardwareSnapshot
        | DisplayLayoutError::Invalid(_) => "display-mode-unavailable-single",
    }
}

/// The wireless stream layout that corresponds to a desktop layout. A
/// receiver is an external display, so "only this screen" has no stream.
fn wireless_mode_for(mode: DisplayLayoutMode) -> Option<ProjectionMode> {
    match mode {
        DisplayLayoutMode::Extend => Some(ProjectionMode::Extend),
        DisplayLayoutMode::Mirror => Some(ProjectionMode::Mirror),
        DisplayLayoutMode::ExternalOnly => Some(ProjectionMode::ExternalOnly),
        DisplayLayoutMode::BuiltinOnly => None,
    }
}

fn layout_for_wireless(mode: ProjectionMode) -> DisplayLayoutMode {
    match mode {
        ProjectionMode::Mirror => DisplayLayoutMode::Mirror,
        ProjectionMode::Extend => DisplayLayoutMode::Extend,
        ProjectionMode::ExternalOnly => DisplayLayoutMode::ExternalOnly,
    }
}

/// Attached displays, the committed layout and the transaction this
/// session staged, as the compositor published them.
#[derive(Default)]
pub(crate) struct DisplayLayouts {
    outputs: Vec<DisplayOutputSnapshot>,
    committed: DisplayConfig,
    transaction: Option<DisplayTransaction>,
    polled_at: Option<Instant>,
    /// Id of the transaction this sheet staged, so an old rejection left in
    /// settings is never reported as the answer to a new choice.
    staged: Option<u64>,
    notice: Option<String>,
    answer: Option<crate::shell::display_persistence::PendingAnswer>,
    staging: Option<crate::shell::display_persistence::PendingLayout>,
}

impl DisplayLayouts {
    fn refresh(&mut self, now: Instant) {
        self.outputs = DisplayHardwareSnapshot::load()
            .map(|snapshot| snapshot.outputs)
            .unwrap_or_default();
        self.committed =
            DisplayConfig::load_for(&settings_store::display_mode::attached_set(&self.outputs));
        self.transaction = DisplayTransaction::load();
        self.polled_at = Some(now);
    }

    fn poll_completions(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if let Some(staging) = self.staging.as_ref() {
            match staging.poll() {
                Ok(None) => {}
                Ok(Some(transaction)) => {
                    changed = true;
                    self.staging = None;
                    self.staged = Some(transaction.id);
                    self.transaction = Some(transaction);
                    self.notice = None;
                }
                Err(error) => {
                    changed = true;
                    self.staging = None;
                    self.notice = Some(error.to_string());
                }
            }
        }
        if let Some(answer) = self.answer.as_ref() {
            match answer.poll() {
                Ok(None) => {}
                Ok(Some(())) => {
                    changed = true;
                    self.answer = None;
                    self.notice = None;
                    self.refresh(now);
                }
                Err(error) => {
                    changed = true;
                    self.answer = None;
                    self.notice = Some(error.to_string());
                    self.refresh(now);
                }
            }
        }
        changed
    }

    fn poll(&mut self, now: Instant) {
        self.poll_completions(now);
        if self
            .polled_at
            .is_none_or(|at| now.duration_since(at) >= DISPLAY_POLL)
        {
            self.refresh(now);
        }
    }

    /// The staged transaction while it still waits for Keep or Revert.
    fn pending(&self) -> Option<&DisplayTransaction> {
        self.transaction.as_ref().filter(|transaction| {
            matches!(
                transaction.state,
                DisplayTransactionState::Requested
                    | DisplayTransactionState::AwaitingConfirmation
                    | DisplayTransactionState::ConfirmRequested
            )
        })
    }

    fn effective(&self) -> &DisplayConfig {
        self.pending()
            .map_or(&self.committed, |transaction| &transaction.candidate)
    }

    fn multiple_displays(&self) -> bool {
        self.outputs.len() >= 2
    }

    fn active(&self) -> Option<DisplayLayoutMode> {
        display_mode::active_layout(&self.outputs, self.effective())
    }

    fn plan_error(&self, mode: DisplayLayoutMode) -> Option<DisplayLayoutError> {
        display_mode::plan_layout(mode, &self.outputs, &self.committed).err()
    }

    fn apply(&mut self, mode: DisplayLayoutMode, now: Instant) {
        if self.staging.is_some() {
            return;
        }
        match crate::shell::display_persistence::stage_layout(mode) {
            Ok(staging) => {
                self.staging = Some(staging);
                self.notice = None;
            }
            Err(error) => {
                tracing::warn!(%error, mode = mode.id(), "display layout was not staged");
                self.notice = Some(error.to_string());
            }
        }
        self.polled_at = Some(now);
    }

    fn keep(&mut self) {
        self.queue_answer(crate::shell::display_persistence::Answer::Keep);
    }

    fn revert(&mut self) {
        self.queue_answer(crate::shell::display_persistence::Answer::Revert);
    }

    fn queue_answer(&mut self, answer: crate::shell::display_persistence::Answer) {
        if self.answer.is_some() {
            return;
        }
        let Some(transaction) = self.transaction.as_ref() else {
            return;
        };
        match crate::shell::display_persistence::answer_for(answer, Some(transaction.id)) {
            Ok(answer) => {
                self.answer = Some(answer);
                self.notice = None;
            }
            Err(error) => {
                self.notice = Some(error.to_string());
            }
        }
    }

    /// Seconds left before an unconfirmed layout restores itself.
    fn seconds_left(&self, now_unix_ms: u64) -> Option<u64> {
        let transaction = self.pending()?;
        let deadline = transaction.deadline_unix_ms?;
        Some(deadline.saturating_sub(now_unix_ms).div_ceil(1000))
    }

    /// Why the layout this sheet staged was refused, if it was.
    fn rejection(&self) -> Option<String> {
        if let Some(notice) = &self.notice {
            return Some(notice.clone());
        }
        let transaction = self.transaction.as_ref()?;
        (transaction.state == DisplayTransactionState::Rejected
            && self.staged == Some(transaction.id))
        .then(|| transaction.message.clone().unwrap_or_default())
    }
}

/// Keyboard focus inside the sheet, in visual order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SheetFocus {
    Layout(DisplayLayoutMode),
    Keep,
    Revert,
    Receivers,
}

/// Result of routing an input event through the open sheet.
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
    displays: DisplayLayouts,
    focus: SheetFocus,
    transition: ui::Transition,
    was_open: bool,
    /// Whether the open-time state (display snapshot, initial focus) was
    /// taken for the current opening. Events can arrive before the first
    /// frame, so both paths take it.
    session_open: bool,
    host_rect: Rect,
    layout_rects: [Rect; 4],
    keep_rect: Rect,
    revert_rect: Rect,
    device_rects: Vec<Rect>,
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
            displays: DisplayLayouts::default(),
            focus: SheetFocus::Layout(DisplayLayoutMode::Extend),
            transition: ui::Transition::closed(),
            was_open: false,
            session_open: false,
            host_rect: Rect::ZERO,
            layout_rects: [Rect::ZERO; 4],
            keep_rect: Rect::ZERO,
            revert_rect: Rect::ZERO,
            device_rects: Vec::new(),
            refresh_rect: Rect::ZERO,
            close_rect: Rect::ZERO,
            audio_rect: Rect::ZERO,
            adaptive_rect: Rect::ZERO,
            resolution_rect: Rect::ZERO,
            fps_rect: Rect::ZERO,
            credential_input: TextInput::local()
                .with_id("projection-airplay-credential")
                .with_placeholder(t("wireless-credential-placeholder"))
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

    /// Where the sheet shows the `AirPlay` PIN or password field, while it
    /// does, covering every pose of the sheet's slide-in. A capture draws it
    /// as the placeholder — see [`crate::shell::capture_protection`].
    pub fn secret_prompt_rect(&self, now: Instant) -> Option<Rect> {
        if !self.transition.is_visible(now)
            || !self.airplay_selected()
            || self.credential_rect.area() <= 0.0
        {
            return None;
        }
        // `draw_at_theme_scale` slides the sheet in from the right by up to
        // this much.
        let slide = self.sheet_rect().width() * 0.22;
        let field = self.credential_rect.inflate(2.0, 2.0);
        Some(Rect::new(field.x0, field.y0, field.x1 + slide, field.y1))
    }

    /// Drain storage receipts on the ordinary host update, even at rest.
    /// Waiting itself is not animation; the existing native wake schedules
    /// this update and only a completed receipt invalidates the sheet.
    pub(crate) fn poll_storage(&mut self) -> bool {
        self.displays.poll_completions(Instant::now())
    }

    pub fn needs_animation_frame(&self, now: Instant) -> bool {
        self.transition.is_animating(now)
            || matches!(
                self.controller.status,
                ProjectionStatus::Scanning | ProjectionStatus::Connecting { .. }
            )
            // The Keep/Revert countdown ticks while the sheet is open.
            || (self.was_open && self.displays.pending().is_some())
    }

    /// The layout the sheet highlights as current: the physical layout when
    /// two or more displays are attached, else the wireless stream layout.
    fn active_layout(&self) -> Option<DisplayLayoutMode> {
        if self.displays.multiple_displays() {
            self.displays.active()
        } else if self.controller.devices.is_empty() {
            None
        } else {
            Some(layout_for_wireless(self.controller.options.mode))
        }
    }

    /// Whether choosing `mode` does anything: it can be planned for the
    /// attached displays, or the selected receiver can stream it.
    fn layout_enabled(&self, mode: DisplayLayoutMode) -> bool {
        if self.displays.staging.is_some() {
            return false;
        }
        self.displays.plan_error(mode).is_none()
            || (!self.controller.devices.is_empty()
                && wireless_mode_for(mode)
                    .is_some_and(|wireless| self.controller.selected_device_supports(wireless)))
    }

    fn enabled_layouts(&self) -> Vec<DisplayLayoutMode> {
        DisplayLayoutMode::ALL
            .into_iter()
            .filter(|mode| self.layout_enabled(*mode))
            .collect()
    }

    /// Take the display snapshot and put focus on the current layout the
    /// first time an opening is seen; forget it once closed.
    fn sync_open_state(&mut self, open: bool) {
        if open && !self.session_open {
            self.session_open = true;
            self.displays.notice = None;
            self.displays.staged = None;
            self.displays.refresh(Instant::now());
            let initial = self
                .active_layout()
                .or_else(|| self.enabled_layouts().first().copied())
                .unwrap_or(DisplayLayoutMode::Extend);
            self.focus = SheetFocus::Layout(initial);
        } else if !open {
            self.session_open = false;
        }
    }

    /// Super+P while open: move the highlight to the next layout that can
    /// be chosen, wrapping around, starting from the highlighted one.
    fn cycle_layout(&mut self) {
        let enabled = self.enabled_layouts();
        let from = match self.focus {
            SheetFocus::Layout(mode) => Some(mode),
            _ => self.active_layout(),
        };
        let next = from
            .and_then(|from| {
                DisplayLayoutMode::ALL
                    .iter()
                    .cycle()
                    .skip(from.index().saturating_add(1))
                    .take(DisplayLayoutMode::ALL.len())
                    .find(|mode| enabled.contains(mode))
                    .copied()
            })
            .or_else(|| enabled.first().copied());
        if let Some(next) = next {
            self.focus = SheetFocus::Layout(next);
        }
    }

    /// Choose `mode`: stage it on the attached displays when it applies to
    /// them, and make it the stream layout for wireless receivers.
    fn choose_layout(&mut self, mode: DisplayLayoutMode) {
        if !self.layout_enabled(mode) {
            return;
        }
        if let Some(wireless) = wireless_mode_for(mode)
            && (self.controller.devices.is_empty()
                || self.controller.selected_device_supports(wireless))
        {
            self.controller.options.mode = wireless;
        }
        if self.displays.plan_error(mode).is_none() {
            self.displays.apply(mode, Instant::now());
            self.focus = SheetFocus::Keep;
        } else {
            self.focus = SheetFocus::Layout(mode);
        }
    }

    fn confirming(&self) -> bool {
        self.displays.pending().is_some()
    }

    /// Route `event` to the sheet shown over `host_rect`. The outcome says
    /// whether the sheet used it; `ctx.handled` is left to the caller.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut EventCtx<'_>,
        cursor: Option<Point>,
        host_rect: Rect,
        open: bool,
    ) -> EventOutcome {
        self.sync_open_state(open);
        if !open {
            return EventOutcome::default();
        }
        self.host_rect = host_rect;
        if self.airplay_selected()
            && let Some(outcome) = self.handle_credential_event(event, ctx, cursor)
        {
            return outcome;
        }
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        repeat,
                        ..
                    },
                ..
            } => {
                if *repeat
                    && matches!(logical_key, Key::Character(s) if s.eq_ignore_ascii_case("p"))
                {
                    return EventOutcome::consumed(false);
                }
                self.handle_key(logical_key, *ctx.modifiers)
            }
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

    fn handle_credential_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut EventCtx<'_>,
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
            &mut EventCtx {
                scale_factor: ctx.scale_factor,
                modifiers: ctx.modifiers,
                state: ctx.state.reborrow(),
                path: ctx.path,
                handled: &mut input_handled,
                focus_target: &mut *ctx.focus_target,
                node_id: input_node,
                taffy: &input_taffy,
            },
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

    /// Up/Down walk the sheet in visual order: layouts, Keep/Revert while a
    /// change waits for confirmation, then the receivers.
    fn move_focus(&mut self, down: bool) {
        let confirming = self.confirming();
        let last_layout = DisplayLayoutMode::ExternalOnly;
        self.focus = match (self.focus, down) {
            (SheetFocus::Layout(mode), true) if mode == last_layout => {
                if confirming {
                    SheetFocus::Keep
                } else if self.controller.devices.is_empty() {
                    self.focus
                } else {
                    self.controller.selected = 0;
                    SheetFocus::Receivers
                }
            }
            (SheetFocus::Layout(mode), true) => DisplayLayoutMode::ALL
                .get(mode.index().saturating_add(1))
                .map_or(self.focus, |next| SheetFocus::Layout(*next)),
            (SheetFocus::Layout(mode), false) => mode
                .index()
                .checked_sub(1)
                .and_then(|index| DisplayLayoutMode::ALL.get(index))
                .map_or(self.focus, |previous| SheetFocus::Layout(*previous)),
            (SheetFocus::Keep | SheetFocus::Revert, true) => {
                if self.controller.devices.is_empty() {
                    self.focus
                } else {
                    self.controller.selected = 0;
                    SheetFocus::Receivers
                }
            }
            (SheetFocus::Keep | SheetFocus::Revert, false) => SheetFocus::Layout(last_layout),
            (SheetFocus::Receivers, true) => {
                self.controller.move_selection(1);
                self.focus
            }
            (SheetFocus::Receivers, false) => {
                if self.controller.selected == 0 {
                    if confirming {
                        SheetFocus::Keep
                    } else {
                        SheetFocus::Layout(last_layout)
                    }
                } else {
                    self.controller.move_selection(-1);
                    self.focus
                }
            }
        };
    }

    fn activate_focus(&mut self) {
        match self.focus {
            SheetFocus::Layout(mode) => self.choose_layout(mode),
            SheetFocus::Keep => {
                self.displays.keep();
                self.focus = self.active_layout().map_or(self.focus, SheetFocus::Layout);
            }
            SheetFocus::Revert => {
                self.displays.revert();
                self.focus = self.active_layout().map_or(self.focus, SheetFocus::Layout);
            }
            SheetFocus::Receivers => {
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
        }
    }

    fn handle_key(&mut self, key: &Key, modifiers: Modifiers) -> EventOutcome {
        let super_down = modifiers.state().super_key();
        let shift_down = modifiers.state().shift_key();
        match key {
            Key::Named(NamedKey::Escape) => return EventOutcome::close(),
            Key::Character(s) if super_down && s.eq_ignore_ascii_case("p") => {
                if shift_down {
                    // Super+Shift+P belongs to picture-in-picture capture.
                    return EventOutcome::close();
                }
                self.cycle_layout();
            }
            Key::Named(NamedKey::ArrowUp) => self.move_focus(false),
            Key::Named(NamedKey::ArrowDown) => self.move_focus(true),
            Key::Named(NamedKey::ArrowLeft) if self.focus == SheetFocus::Revert => {
                self.focus = SheetFocus::Keep;
            }
            Key::Named(NamedKey::ArrowRight) if self.focus == SheetFocus::Keep => {
                self.focus = SheetFocus::Revert;
            }
            Key::Named(NamedKey::Enter) => self.activate_focus(),
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
            Key::Character(s) if matches!(s.as_str(), "1" | "2" | "3" | "4") => {
                let index = s.parse::<usize>().unwrap_or(1).saturating_sub(1);
                if let Some(&mode) = DisplayLayoutMode::ALL.get(index) {
                    self.choose_layout(mode);
                }
            }
            _ => {}
        }
        EventOutcome::consumed(true)
    }

    fn handle_click(&mut self, point: Point) -> EventOutcome {
        if self.close_rect.contains(point) || !self.sheet_rect().contains(point) {
            return EventOutcome::close();
        } else if self.keep_rect.contains(point) {
            self.focus = SheetFocus::Keep;
            self.activate_focus();
        } else if self.revert_rect.contains(point) {
            self.focus = SheetFocus::Revert;
            self.activate_focus();
        } else if let Some(index) = self
            .layout_rects
            .iter()
            .position(|rect| rect.area() > 0.0 && rect.contains(point))
        {
            if let Some(&mode) = DisplayLayoutMode::ALL.get(index) {
                self.focus = SheetFocus::Layout(mode);
                self.choose_layout(mode);
            }
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
            .device_rects
            .iter()
            .position(|rect| rect.contains(point))
        {
            self.click_device(index);
        }
        EventOutcome::consumed(true)
    }

    fn click_device(&mut self, index: usize) {
        self.focus = SheetFocus::Receivers;
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

    /// Semantic tree of the open sheet: one button per layout (selected is
    /// the highlighted one, checked is the current one), Keep and Revert
    /// while a change waits, and one button per receiver.
    pub fn accessibility(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};

        let mut sheet = AccessNode::new(Role::Dialog)
            .with_name(t("display-sheet-title"))
            .with_description(if self.displays.staging.is_some() {
                format!(
                    "{}. {}",
                    t("display-sheet-saving-layout"),
                    t("display-sheet-a11y-description")
                )
            } else {
                t("display-sheet-a11y-description")
            })
            .with_bounds(self.sheet_rect())
            .with_state(AccessState::new().modal(true));
        let active = self.active_layout();
        for (index, mode) in DisplayLayoutMode::ALL.into_iter().enumerate() {
            let description = self.displays.plan_error(mode).map_or_else(
                || t(layout_detail_key(mode)),
                |error| {
                    if self.layout_enabled(mode) {
                        t(layout_detail_key(mode))
                    } else {
                        t(unavailable_key(&error))
                    }
                },
            );
            sheet.children.push(
                AccessNode::new(Role::Button)
                    .with_name(t(layout_title_key(mode)))
                    .with_description(description)
                    .with_bounds(self.layout_rects.get(index).copied().unwrap_or_default())
                    .with_state(
                        AccessState::new()
                            .focused(self.focus == SheetFocus::Layout(mode))
                            .selected(self.focus == SheetFocus::Layout(mode))
                            .checked(active == Some(mode))
                            .disabled(!self.layout_enabled(mode)),
                    ),
            );
        }
        if self.confirming() {
            for (focus, key, rect) in [
                (SheetFocus::Keep, "display-confirm-keep", self.keep_rect),
                (
                    SheetFocus::Revert,
                    "display-confirm-revert",
                    self.revert_rect,
                ),
            ] {
                sheet.children.push(
                    AccessNode::new(Role::Button)
                        .with_name(t(key))
                        .with_description(t("display-confirm-title"))
                        .with_bounds(rect)
                        .with_state(
                            AccessState::new()
                                .focused(self.focus == focus)
                                .disabled(self.displays.answer.is_some()),
                        ),
                );
            }
        }
        for (index, device) in self.controller.devices.iter().enumerate() {
            sheet.children.push(
                AccessNode::new(Role::Button)
                    .with_name(device.name.clone())
                    .with_description(device.protocol.label())
                    .with_bounds(self.device_rects.get(index).copied().unwrap_or_default())
                    .with_state(AccessState::new().focused(
                        self.focus == SheetFocus::Receivers && self.controller.selected == index,
                    )),
            );
        }
        sheet
    }

    /// Paint the complete sheet and return whether worker polling changed state.
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
        self.sync_open_state(open);
        let changed = self.controller.poll();
        if open && !self.was_open {
            self.controller.refresh();
        }
        self.was_open = open;
        let now = Instant::now();
        if open {
            self.displays.poll(now);
        }
        self.transition.set_open(
            open,
            if theme.reduce_motion {
                Duration::ZERO
            } else {
                Duration::from_millis(OPEN_ANIM_MS)
            },
            ui::Curve::EaseOut,
        );
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
        self.layout_rects = [Rect::ZERO; 4];
        self.keep_rect = Rect::ZERO;
        self.revert_rect = Rect::ZERO;
        self.device_rects.clear();
        self.refresh_rect = Rect::ZERO;
        self.close_rect = Rect::ZERO;
        self.audio_rect = Rect::ZERO;
        self.adaptive_rect = Rect::ZERO;
        self.resolution_rect = Rect::ZERO;
        self.fps_rect = Rect::ZERO;
        self.credential_rect = Rect::ZERO;
    }

    /// Height of the Keep/Revert band: a title line and a status line with
    /// the two buttons beside them.
    fn confirm_band_height() -> f64 {
        crate::shell::display_confirm::band_height()
    }

    /// Receiver rows are 56 px with an 8 px gap between them.
    fn device_row_pitch() -> f64 {
        64.0
    }

    fn rect_for_host(host_rect: Rect) -> Rect {
        let margin = 20.0;
        let width = 432.0_f64.min((host_rect.width() - margin * 2.0).max(280.0));
        let top = host_rect.y0 + 56.0;
        let bottom = (top + 780.0).min(host_rect.y1 - 24.0);
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

    fn layout_row_rect(&self, index: usize) -> Rect {
        let r = self.sheet_rect();
        // Rows of 52 px on a 58 px pitch, below the 80 px header.
        let y0 = r.y0 + 80.0 + usize_to_f64_lossy(index) * 58.0;
        Rect::new(r.x0 + 16.0, y0, r.x1 - 16.0, y0 + 52.0)
    }

    /// Top of the confirmation band, directly under the layout rows.
    fn confirm_top(&self) -> f64 {
        self.layout_row_rect(DisplayLayoutMode::ALL.len().saturating_sub(1))
            .y1
            + 10.0
    }

    /// Top of the wireless section; the confirmation band or a rejection
    /// notice pushes it down while visible.
    fn wireless_top(&self) -> f64 {
        let band = if self.displays.staging.is_some()
            || self.confirming()
            || self.displays.rejection().is_some()
        {
            Self::confirm_band_height() + 10.0
        } else {
            0.0
        };
        self.confirm_top() + band + 8.0
    }

    fn row_rect(&self, index: usize) -> Rect {
        let r = self.sheet_rect();
        let y0 = self.device_rows_top() + usize_to_f64_lossy(index) * Self::device_row_pitch();
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
        self.wireless_top()
            + if self.airplay_selected() {
                214.0
            } else {
                118.0
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
        let type_scale = &theme.tokens.typography;

        self.draw_header(scene, transform, text_renderer, theme, r);
        self.draw_layout_rows(scene, transform, text_renderer, theme);
        self.draw_confirm_band(scene, transform, text_renderer, theme, r);
        self.draw_devices_heading(scene, transform, text_renderer, theme, r);
        self.draw_audio_toggle(scene, transform, text_renderer, theme, r);
        self.draw_stream_chips(scene, transform, text_renderer, theme, r);
        self.draw_credential_field(scene, transform, text_renderer, theme, presence, r);

        self.device_rects.clear();
        let rows_top = self.device_rows_top();
        let max_rows =
            f64_floor_to_usize_saturating((r.y1 - rows_top - 40.0) / Self::device_row_pitch())
                .max(1);
        if self.controller.devices.is_empty() {
            self.draw_devices_empty(scene, transform, text_renderer, theme, r);
        } else {
            self.draw_devices_list(scene, transform, text_renderer, theme, max_rows);
        }

        let footer = t("wireless-protocols");
        let (fw, _) =
            text_renderer.measure_style(&footer, None, &type_scale.style(TextRole::Caption));
        text_renderer.draw_style(
            scene,
            &footer,
            colors.text_dim,
            transform.pre_translate((r.center().x - f64::from(fw) / 2.0, r.y1 - 28.0).into()),
            None,
            &type_scale.style(TextRole::Caption),
        );
    }

    /// The sheet's card, title, subtitle and close button.
    fn draw_header(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography;
        let radius = ui::chrome::ChromeRole::Panel.radius(theme, r.height());
        let card = RoundedRect::from_rect(r, radius);
        ui::chrome::shell::panel_shadow(scene, transform, r, radius, theme, 1.0);
        ui::floating_surface::paint_material(
            scene,
            transform,
            &card,
            ui::floating_surface::Material::Floating,
            theme,
        );
        // Panel edge derived against the ground (HIG §9.5), not declared.
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            ui::chrome::shell::panel_border(theme),
            None,
            &card,
        );

        text_renderer.draw_style(
            scene,
            &t("display-sheet-title"),
            colors.text_primary,
            transform.pre_translate((r.x0 + 20.0, r.y0 + 18.0).into()),
            None,
            &type_scale.style(TextRole::Title),
        );
        text_renderer.draw_style(
            scene,
            &t("display-sheet-subtitle"),
            colors.text_dim,
            transform.pre_translate((r.x0 + 20.0, r.y0 + 49.0).into()),
            Some(f64_to_f32_lossy(r.width() - 88.0)),
            &type_scale.style(TextRole::Caption),
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

    /// The four desktop layouts as a vertical list, each with a two-screen
    /// pictogram, its name and what it does (or why it cannot be chosen).
    fn draw_layout_rows(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography;
        // Control role (HIG §9.1/§9.3): the highlighted layout is a lift with
        // the tier's hairline, never an accent wash (the accent means live
        // state, §9.4); idle rows rest on the lift floor with an edge
        // derived from that floor (§9.5).
        let floor = colors.lift(0.35);
        let idle_edge = ui::surface::edge_on(floor);
        let active = self.active_layout();
        for (index, mode) in DisplayLayoutMode::ALL.into_iter().enumerate() {
            let row = self.layout_row_rect(index);
            if let Some(slot) = self.layout_rects.get_mut(index) {
                *slot = row;
            }
            let enabled = self.layout_enabled(mode);
            let highlighted = self.focus == SheetFocus::Layout(mode);
            let radius = ui::chrome::ChromeRole::Control.radius(theme, row.height());
            let shape = RoundedRect::from_rect(row, radius);
            let tier =
                ui::chrome::selection_tier(theme, ui::chrome::SelectionState::active(highlighted));
            scene.fill(
                Fill::NonZero,
                transform,
                if highlighted { tier.fill } else { floor },
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
            if highlighted {
                ui::widget::paint_focus_ring(scene, transform, row, radius, theme);
            }
            let pictogram = Rect::new(row.x0 + 12.0, row.y0 + 14.0, row.x0 + 56.0, row.y1 - 14.0);
            draw_layout_pictogram(scene, transform, pictogram, mode, theme, enabled);
            let text_x = pictogram.x1 + 14.0;
            let text_w = f64_to_f32_lossy(row.x1 - text_x - 44.0);
            text_renderer.draw_style(
                scene,
                &t(layout_title_key(mode)),
                if enabled {
                    colors.text_primary
                } else {
                    colors.text_dim.with_alpha_factor(0.62)
                },
                transform.pre_translate((text_x, row.y0 + 8.0).into()),
                Some(text_w),
                &type_scale
                    .style(TextRole::Caption)
                    .with_weight(TextWeight::Medium),
            );
            let detail = if enabled {
                t(layout_detail_key(mode))
            } else {
                self.displays
                    .plan_error(mode)
                    .map_or_else(|| t(layout_detail_key(mode)), |e| t(unavailable_key(&e)))
            };
            let detail = text_renderer.elide_to_width_style(
                &detail,
                text_w,
                &type_scale.style(TextRole::Caption),
            );
            text_renderer.draw_style(
                scene,
                &detail,
                colors
                    .text_dim
                    .with_alpha_factor(if enabled { 1.0 } else { 0.62 }),
                transform.pre_translate((text_x, row.y0 + 29.0).into()),
                None,
                &type_scale.style(TextRole::Caption),
            );
            if active == Some(mode) {
                ui::icons::draw_icon(
                    scene,
                    transform,
                    Rect::new(
                        row.x1 - 34.0,
                        row.center().y - 9.0,
                        row.x1 - 16.0,
                        row.center().y + 9.0,
                    ),
                    ui::icons::Icon::Check,
                    colors.semantic.success,
                    ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
        }
    }

    /// Keep/Revert with the restore countdown while a staged layout waits
    /// for confirmation, or why the last choice was refused.
    fn draw_confirm_band(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        self.keep_rect = Rect::ZERO;
        self.revert_rect = Rect::ZERO;
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography;
        let top = self.confirm_top();
        let band = Rect::new(
            r.x0 + 16.0,
            top,
            r.x1 - 16.0,
            top + Self::confirm_band_height(),
        );
        if self.displays.staging.is_some() {
            text_renderer.draw_style(
                scene,
                &t("display-sheet-saving-layout"),
                colors.text_dim,
                transform.pre_translate((band.x0 + 4.0, band.y0 + 8.0).into()),
                Some(f64_to_f32_lossy(band.width() - 8.0)),
                &type_scale.style(TextRole::Caption),
            );
        } else if self.confirming() {
            let status = if self.displays.answer.is_some() {
                t("display-confirm-answer-pending")
            } else if let Some(notice) = self.displays.notice.as_ref() {
                t_args(
                    "display-confirm-answer-failed",
                    &[("reason", ArgValue::Text(notice.clone()))],
                )
            } else {
                crate::shell::display_confirm::countdown_status(
                    self.displays.seconds_left(tilo_clock::unix_now_ms()),
                )
            };
            let focus = match self.focus {
                SheetFocus::Keep => Some(crate::shell::display_confirm::BandFocus::Keep),
                SheetFocus::Revert => Some(crate::shell::display_confirm::BandFocus::Revert),
                SheetFocus::Layout(_) | SheetFocus::Receivers => None,
            };
            let buttons = crate::shell::display_confirm::paint_keep_revert_band(
                scene,
                transform,
                text_renderer,
                theme,
                band,
                &t("display-confirm-title"),
                &status,
                crate::shell::display_confirm::BandState {
                    focus,
                    disabled: self.displays.answer.is_some(),
                },
            );
            self.keep_rect = buttons.keep;
            self.revert_rect = buttons.revert;
        } else if let Some(reason) = self.displays.rejection() {
            let message = t_args("display-rejected", &[("reason", ArgValue::Text(reason))]);
            text_renderer.draw_style(
                scene,
                &message,
                colors.semantic.danger,
                transform.pre_translate((band.x0 + 4.0, band.y0 + 8.0).into()),
                Some(f64_to_f32_lossy(band.width() - 8.0)),
                &type_scale.style(TextRole::Caption),
            );
        }
    }

    /// "Wireless displays" heading plus the refresh button beside it.
    fn draw_devices_heading(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography;
        let heading_y = self.wireless_top();
        text_renderer.draw_style(
            scene,
            &t("wireless-heading"),
            colors.text_primary,
            transform.pre_translate((r.x0 + 20.0, heading_y).into()),
            None,
            &type_scale
                .style(TextRole::Caption)
                .with_weight(TextWeight::Semibold),
        );
        self.refresh_rect = Rect::new(r.x1 - 48.0, heading_y - 8.0, r.x1 - 12.0, heading_y + 28.0);
        ui::icons::draw_icon(
            scene,
            transform,
            self.refresh_rect.inset(-9.0),
            ui::icons::Icon::Refresh,
            colors.text_dim,
            1.5,
        );
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
        let type_scale = &theme.tokens.typography;
        let top = self.wireless_top() + 32.0;
        self.audio_rect = Rect::new(r.x0 + 20.0, top, r.x1 - 62.0, top + 38.0);
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::new(r.x0 + 22.0, top + 9.0, r.x0 + 42.0, top + 29.0),
            if self.controller.options.audio {
                ui::icons::Icon::Volume2
            } else {
                ui::icons::Icon::VolumeX
            },
            colors.text_dim,
            1.5,
        );
        text_renderer.draw_style(
            scene,
            &t("wireless-share-audio"),
            colors.text_primary,
            transform.pre_translate((r.x0 + 52.0, top + 10.0).into()),
            None,
            &type_scale.style(TextRole::Caption),
        );
        let toggle = Rect::new(r.x1 - 58.0, top + 6.0, r.x1 - 20.0, top + 28.0);
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
        let type_scale = &theme.tokens.typography;
        let top = self.wireless_top() + 76.0;
        text_renderer.draw_style(
            scene,
            &t("wireless-stream"),
            colors.text_dim,
            transform.pre_translate((r.x0 + 20.0, top + 10.0).into()),
            None,
            &type_scale
                .style(TextRole::Caption)
                .with_weight(TextWeight::Medium),
        );
        let x0 = r.x0 + 76.0;
        let fps = f64::from(self.controller.options.stream_policy.max_fps);
        let labels = [
            if self.controller.options.stream_policy.adaptive {
                t("wireless-stream-auto")
            } else {
                t("wireless-stream-fixed")
            },
            self.controller
                .options
                .stream_policy
                .resolution_limit
                .label()
                .to_owned(),
            t_args("wireless-stream-fps", &[("fps", ArgValue::Number(fps))]),
        ];
        let widths = [76.0, 88.0, 76.0];
        let mut chip_x = x0;
        // Control chips at the Control radius: the engaged "Auto" choice is
        // a lift with the tier's hairline, idle chips rest on the lift floor
        // with a derived edge.
        let floor = colors.lift(0.35);
        let idle_edge = ui::surface::edge_on(floor);
        for (i, (label, width)) in labels.iter().zip(widths).enumerate() {
            let chip = Rect::new(chip_x, top, chip_x + width, top + 30.0);
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
            let (label_w, label_h) =
                text_renderer.measure_style(label, None, &type_scale.style(TextRole::Caption));
            text_renderer.draw_style(
                scene,
                label,
                colors.text_primary,
                transform.pre_translate(
                    (
                        chip.center().x - f64::from(label_w) / 2.0,
                        chip.center().y - f64::from(label_h) / 2.0,
                    )
                        .into(),
                ),
                None,
                &type_scale
                    .style(TextRole::Caption)
                    .with_weight(TextWeight::Medium),
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
        let type_scale = &theme.tokens.typography;
        if self.airplay_selected() {
            let top = self.wireless_top() + 120.0;
            text_renderer.draw_style(
                scene,
                &t("wireless-credential-label"),
                colors.text_primary,
                transform.pre_translate((r.x0 + 20.0, top).into()),
                None,
                &type_scale
                    .style(TextRole::Caption)
                    .with_weight(TextWeight::Semibold),
            );
            self.credential_rect = Rect::new(r.x0 + 20.0, top + 21.0, r.x1 - 20.0, top + 63.0);
            self.credential_input
                .apply_layout_external(self.credential_rect);
            self.credential_input
                .draw(scene, transform, text_renderer, theme, presence);
            text_renderer.draw_style(
                scene,
                &t("wireless-credential-hint"),
                colors.text_dim,
                transform.pre_translate((r.x0 + 20.0, top + 71.0).into()),
                Some(f64_to_f32_lossy(r.width() - 40.0)),
                &type_scale.style(TextRole::Caption),
            );
        } else {
            self.credential_rect = Rect::ZERO;
            self.credential_input.set_focus(false);
        }
    }

    /// An honest zero-state for the receiver list: what's happening
    /// (scanning / errored / nothing found) and what to do about it.
    fn draw_devices_empty(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography;
        let (message, detail) = match &self.controller.status {
            ProjectionStatus::Scanning => (t("wireless-scanning"), t("wireless-scanning-detail")),
            ProjectionStatus::Error(error) => (t("wireless-scan-failed"), error.clone()),
            _ => (t("wireless-none"), t("wireless-none-detail")),
        };
        let top = self.device_rows_top();
        let icon = Rect::new(r.center().x - 16.0, top, r.center().x + 16.0, top + 32.0);
        ui::icons::draw_icon(
            scene,
            transform,
            icon,
            ui::icons::Icon::Monitor,
            colors.text_dim,
            1.5,
        );
        let (mw, _) =
            text_renderer.measure_style(&message, None, &type_scale.style(TextRole::Caption));
        text_renderer.draw_style(
            scene,
            &message,
            colors.text_primary,
            transform.pre_translate((r.center().x - f64::from(mw) / 2.0, top + 40.0).into()),
            None,
            &type_scale
                .style(TextRole::Caption)
                .with_weight(TextWeight::Medium),
        );
        let detail = text_renderer.elide_to_width_style(
            &detail,
            f64_to_f32_lossy(r.width() - 56.0),
            &type_scale.style(TextRole::Caption),
        );
        let (dw, _) =
            text_renderer.measure_style(&detail, None, &type_scale.style(TextRole::Caption));
        text_renderer.draw_style(
            scene,
            &detail,
            colors.text_dim,
            transform.pre_translate((r.center().x - f64::from(dw) / 2.0, top + 64.0).into()),
            None,
            &type_scale.style(TextRole::Caption),
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
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography;
        // Control role: the selected device is a lift (HIG §9.3); every row
        // carries an edge derived from its fill (§9.5).
        let floor = colors.lift(0.35);
        let idle_edge = ui::surface::edge_on(floor);
        let receivers_focused = self.focus == SheetFocus::Receivers;
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
            if selected && receivers_focused {
                ui::widget::paint_focus_ring(scene, transform, row, radius, theme);
            }
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
            let name = text_renderer.elide_to_width_style(
                &device.name,
                f64_to_f32_lossy(row.width() - 142.0),
                &type_scale.style(TextRole::Caption),
            );
            text_renderer.draw_style(
                scene,
                &name,
                colors.text_primary,
                transform.pre_translate((row.x0 + 46.0, row.y0 + 8.0).into()),
                None,
                &type_scale
                    .style(TextRole::Caption)
                    .with_weight(TextWeight::Medium),
            );
            let sub = if connected {
                t("wireless-connected")
            } else if connecting {
                t("wireless-connecting")
            } else if device.detail.is_empty() {
                device.protocol.label().to_string()
            } else {
                format!("{} · {}", device.protocol.label(), device.detail)
            };
            text_renderer.draw_style(
                scene,
                &sub,
                if connected {
                    colors.semantic.success
                } else {
                    colors.text_dim
                },
                transform.pre_translate((row.x0 + 46.0, row.y0 + 31.0).into()),
                Some(f64_to_f32_lossy(row.width() - 142.0)),
                &type_scale.style(TextRole::Caption),
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

    /// The Connect/Stop/Wait action on a device row, painted by the shared
    /// button face.
    fn draw_device_action(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        connected: bool,
        connecting: bool,
    ) {
        let action = Rect::new(row.x1 - 86.0, row.y0 + 11.0, row.x1 - 10.0, row.y1 - 11.0);
        let label = if connected {
            t("wireless-stop")
        } else if connecting {
            t("wireless-wait")
        } else {
            t("wireless-connect")
        };
        ui::widget::Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ui::widget::ButtonFace::new(action, &label)
                .role(if connected {
                    ButtonRole::Destructive
                } else {
                    ButtonRole::Secondary
                })
                .disabled(connecting),
        );
    }
}

/// Two small screens — this one on the left, the external one on the right
/// — showing what each layout does: one desktop spanning both, the same
/// picture on both, or one of them dark.
fn draw_layout_pictogram(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    mode: DisplayLayoutMode,
    theme: &Theme,
    enabled: bool,
) {
    let colors = &theme.tokens.colors;
    let alpha = if enabled { 1.0 } else { 0.5 };
    let lit = colors.text_primary.with_alpha_factor(alpha);
    let dark = colors.text_dim.with_alpha_factor(0.45 * alpha);
    let content = theme
        .tokens
        .colors
        .text_primary
        .with_alpha_factor(0.85 * alpha);
    let gap = 4.0;
    let width = (rect.width() - gap) / 2.0;
    let left = Rect::new(rect.x0, rect.y0, rect.x0 + width, rect.y1);
    let right = Rect::new(left.x1 + gap, rect.y0, rect.x1, rect.y1);
    let (left_on, right_on) = match mode {
        DisplayLayoutMode::Extend | DisplayLayoutMode::Mirror => (true, true),
        DisplayLayoutMode::BuiltinOnly => (true, false),
        DisplayLayoutMode::ExternalOnly => (false, true),
    };
    let stroke = vello::kurbo::Stroke::new(1.25);
    for (screen, on) in [(left, left_on), (right, right_on)] {
        let shape = RoundedRect::from_rect(screen, 2.5);
        scene.stroke(
            &stroke,
            transform,
            if on { lit } else { dark },
            None,
            &shape,
        );
    }
    match mode {
        DisplayLayoutMode::Extend => {
            // One band continuing from the left screen onto the right.
            let band = Rect::new(
                left.x0 + 4.0,
                rect.center().y - 2.5,
                right.x1 - 4.0,
                rect.center().y + 2.5,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                content,
                None,
                &RoundedRect::from_rect(band, 1.5),
            );
        }
        DisplayLayoutMode::Mirror => {
            for screen in [left, right] {
                let dot = Rect::from_center_size(screen.center(), (6.0, 6.0));
                scene.fill(
                    Fill::NonZero,
                    transform,
                    content,
                    None,
                    &RoundedRect::from_rect(dot, 1.5),
                );
            }
        }
        DisplayLayoutMode::BuiltinOnly | DisplayLayoutMode::ExternalOnly => {
            let screen = if left_on { left } else { right };
            let dot = Rect::from_center_size(screen.center(), (6.0, 6.0));
            scene.fill(
                Fill::NonZero,
                transform,
                content,
                None,
                &RoundedRect::from_rect(dot, 1.5),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use settings_store::hardware::{OutputDescriptor, OutputMode};

    use super::*;

    #[test]
    fn projection_sheet_owns_its_geometry_options_and_close_intent() {
        /// HIG §9.5: the sheet's Panel border on its ground, and the derived edge
        /// of its lift-floor rows, must register on every palette.
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
                    ("layout / device row", ui::surface::edge_on(floor), floor),
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

        fn airplay_selection_owns_masked_pairing_geometry() {
            let mut overlay = overlay_with(Vec::new());
            overlay.controller.devices = vec![airplay_device()];
            overlay.controller.selected = 0;
            assert!(overlay.airplay_selected());
            assert!(overlay.credential_input.is_secret());
            let expected = overlay.wireless_top() + 214.0;
            assert!((overlay.device_rows_top() - expected).abs() < f64::EPSILON);
        }

        fn the_airplay_credential_is_a_secret_prompt_only_while_it_is_shown() {
            let mut overlay = overlay_with(Vec::new());
            overlay.controller.devices = vec![airplay_device()];
            overlay.controller.selected = 0;
            overlay.credential_rect = Rect::new(900.0, 300.0, 1200.0, 342.0);
            let now = Instant::now();
            assert_eq!(overlay.secret_prompt_rect(now), None, "the sheet is closed");
            overlay.transition = ui::Transition::open();
            let rect = overlay
                .secret_prompt_rect(now)
                .expect("an open sheet asking for the AirPlay credential");
            let slide = overlay.sheet_rect().width() * 0.22;
            assert!(rect.contains(Point::new(901.0, 301.0)));
            assert!(rect.contains(Point::new(1199.0 + slide, 341.0)));
            overlay.controller.devices.clear();
            assert_eq!(
                overlay.secret_prompt_rect(now),
                None,
                "no AirPlay receiver selected, no credential field"
            );
        }

        fn keyboard_controls_update_owned_options_and_close_intent() {
            let mut overlay = overlay_with(Vec::new());
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

        fn outside_click_requests_host_owned_visibility_close() {
            let mut overlay = overlay_with(Vec::new());
            assert_eq!(
                overlay.handle_click(Point::new(10.0, 10.0)),
                EventOutcome::close()
            );
        }

        sheet_cells_keep_an_edge_on_every_palette();
        airplay_selection_owns_masked_pairing_geometry();
        the_airplay_credential_is_a_secret_prompt_only_while_it_is_shown();
        keyboard_controls_update_owned_options_and_close_intent();
        outside_click_requests_host_owned_visibility_close();
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

    fn two_displays() -> Vec<DisplayOutputSnapshot> {
        let mode = OutputMode {
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
        };
        ["eDP-1", "HDMI-A-1"]
            .into_iter()
            .map(|connector| DisplayOutputSnapshot {
                descriptor: OutputDescriptor {
                    connector: connector.into(),
                    connector_label: connector.into(),
                    modes: vec![mode],
                    hdr: false,
                    vrr: false,
                    color_profiles: Vec::new(),
                    monitor: None,
                },
                current_mode: mode,
                scale: 1.0,
                builtin: connector == "eDP-1",
                sink_hdr: false,
                hdr_unavailable_reason: None,
                sink_vrr: false,
                vrr_unavailable_reason: None,
                color_profile_labels: BTreeMap::new(),
            })
            .collect()
    }

    /// An overlay whose display state is already taken, so key handling does
    /// not read the developer machine's settings.
    fn overlay_with(outputs: Vec<DisplayOutputSnapshot>) -> ProjectionOverlay {
        let mut overlay = ProjectionOverlay {
            host_rect: Rect::new(0.0, 0.0, 1280.0, 900.0),
            session_open: true,
            ..ProjectionOverlay::default()
        };
        overlay.displays.outputs = outputs;
        overlay.displays.polled_at = Some(Instant::now());
        overlay
    }

    #[test]
    fn super_p_cycles_through_every_layout_the_displays_allow() {
        let mut overlay = overlay_with(two_displays());
        overlay.focus = SheetFocus::Layout(DisplayLayoutMode::Extend);
        let sup = Modifiers::from(winit::keyboard::ModifiersState::SUPER);
        let mut seen = Vec::new();
        for _ in 0..4 {
            let outcome = overlay.handle_key(&Key::Character("p".into()), sup);
            assert!(outcome.consumed && !outcome.close_requested);
            let SheetFocus::Layout(mode) = overlay.focus else {
                panic!("Super+P keeps the highlight on a layout");
            };
            seen.push(mode);
        }
        assert_eq!(
            seen,
            [
                DisplayLayoutMode::Mirror,
                DisplayLayoutMode::BuiltinOnly,
                DisplayLayoutMode::ExternalOnly,
                DisplayLayoutMode::Extend,
            ]
        );
    }

    #[test]
    fn a_single_display_offers_no_physical_layout() {
        let overlay = overlay_with(two_displays().into_iter().take(1).collect());
        assert!(overlay.enabled_layouts().is_empty());
    }

    #[test]
    fn arrows_walk_layouts_then_receivers() {
        let mut overlay = overlay_with(two_displays());
        overlay.controller.devices = vec![airplay_device()];
        overlay.focus = SheetFocus::Layout(DisplayLayoutMode::BuiltinOnly);
        let none = Modifiers::default();
        overlay.handle_key(&Key::Named(NamedKey::ArrowDown), none);
        assert_eq!(
            overlay.focus,
            SheetFocus::Layout(DisplayLayoutMode::ExternalOnly)
        );
        overlay.handle_key(&Key::Named(NamedKey::ArrowDown), none);
        assert_eq!(overlay.focus, SheetFocus::Receivers);
        overlay.handle_key(&Key::Named(NamedKey::ArrowUp), none);
        assert_eq!(
            overlay.focus,
            SheetFocus::Layout(DisplayLayoutMode::ExternalOnly)
        );
    }
}
