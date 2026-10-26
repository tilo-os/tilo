//! The live banner stack: title + body cards pinned top-right, sliding in,
//! fading out and auto-dismissing, each on the shared `ui::widget::toast::ToastCard`:
//! dual-line layout, a severity tint, a close affordance, a card for the
//! banners that do not fit, keyboard focus on request and the
//! locked-session redaction.
//!
//! A banner's time runs only while it is on screen and nobody holds it: the
//! stack tells its host which banners it laid out and whether the pointer
//! rests on it or keyboard focus is in it ([`NotificationHost::present`]).
//!
//! The host owns the data; the stack only paints, routes presses and keys,
//! and says when it next needs a frame.

use ui::a11y::AccessNode;
use ui::num::f32_of_f64;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::Instant;
use ui::widget::PaintedTargets;
use ui::widget::toast::ToastCard;
use ui::widget::top_edge_reveal::AIM_HOLD;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;
use winit::keyboard::{Key, NamedKey};

use super::NotificationHost;
use super::model::{Addressed, Notification, NotificationSeverity, lock_suppressed_label};
use crate::i18n::{t, t_args};
use ui::TextRole;

/// How a banner arrives and leaves: from its edge, on the panel slide.
const BANNER: ui::motion::MotionSpec = ui::motion::PanelSlide::SPEC;

/// A banner's arrival and exit for the motion the person allows: the panel
/// slide, or under reduced motion a fade in place at `fast` (a change of
/// what is shown still reads; nothing travels).
fn banner_motion(reduce_motion: bool) -> (ui::motion::MotionSpec, ui::tokens::EffectiveMotion) {
    if reduce_motion {
        (
            ui::motion::CROSS_FADE,
            ui::tokens::EffectiveMotion::new(ui::tokens::Motion::default(), true),
        )
    } else {
        (BANNER, ui::tokens::EffectiveMotion::default())
    }
}

pub(super) const TOAST_WIDTH: f64 = 360.0;
pub(super) const TOAST_HEIGHT: f64 = 64.0;
const TOAST_GAP: f64 = 8.0;
/// A banner's slot is in place within a quarter of a pixel.
const SLOT_TOLERANCE: f64 = 0.25;
pub(super) const EDGE_INSET: f64 = 24.0;
/// Horizontal text inset of the locked card and the "more" card.
/// Deliberately the shared toast card's own `CARD_PAD_X`
/// (`ui::widget::toast`, 14) so their text starts on the cards' line; it is
/// off the 4 px grid for the same reason the card is.
const CARD_PADDING_X: f64 = 14.0;
/// Least height of the "more" card: a Regular control's target.
const MORE_HEIGHT: f64 = 40.0;

/// Bundles the render handles the per-section painters need.
struct PaintCtx<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    text_renderer: &'a mut TextRenderer,
    theme: &'a Theme,
    host: &'a dyn NotificationHost,
}

/// The y-extent of everything painted, so the stack's rect is exactly what
/// was drawn.
struct StackBounds {
    min_y: f64,
    max_y: f64,
}

impl StackBounds {
    fn expand(&mut self, y0: f64, y1: f64) {
        self.min_y = self.min_y.min(y0);
        self.max_y = self.max_y.max(y1);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum BannerTarget {
    Open(u64),
    Dismiss(u64),
    Action(u64, String),
    /// The card standing in for the banners that did not fit: opens the
    /// notification centre.
    More,
}

impl BannerTarget {
    fn key(&self) -> String {
        match self {
            Self::Open(id) => format!("notification:{id}:open"),
            Self::Dismiss(id) => format!("notification:{id}:dismiss"),
            Self::Action(id, key) => format!("notification:{id}:action:{key}"),
            Self::More => "notification:more".to_owned(),
        }
    }

    fn banner(&self) -> Option<u64> {
        match self {
            Self::Open(id) | Self::Dismiss(id) | Self::Action(id, _) => Some(*id),
            Self::More => None,
        }
    }
}

/// One row of keyboard focus: a banner's controls left to right (the card
/// itself, its actions, its close button), or the "more" card alone.
#[derive(Clone, Debug)]
struct FocusRow {
    controls: Vec<(BannerTarget, Rect, f64)>,
}

/// Top-right banner stack over a host's [`NotificationHost`].
#[derive(Debug, Default)]
pub struct NotificationStack {
    /// The area the stack pins itself into, set by the host each frame.
    viewport: Rect,
    /// Per-banner `(id, card, close)` hit rects; cleared every paint so a
    /// stale hit cannot fire after expiry.
    hit_rects: Vec<(u64, Rect, Rect)>,
    action_rects: Vec<(u64, String, Rect)>,
    /// What a press on a painted banner does, as buttons for assistive
    /// technology (and anything else that reads the semantics): each card
    /// (opening it, labelled by its title) and its action buttons.
    /// Each painted control as assistive technology reads it: its rect,
    /// its label and the target activating it (a [`BannerTarget::key`]).
    action_buttons: Vec<(Rect, String, String)>,
    rect: Rect,
    /// Mirrored from the last paint so the frame and deadline hooks obey
    /// reduced motion without a theme argument.
    reduce_motion: bool,
    targets: PaintedTargets,
    target_actions: Vec<(String, BannerTarget)>,
    /// Live banners the last paint laid out on screen, newest first. Only
    /// these age.
    on_screen: Vec<u64>,
    /// The "more" card's rect; zero when every banner fit.
    more_rect: Rect,
    /// The keyboard order of the last paint, top to bottom.
    focus_rows: Vec<FocusRow>,
    /// The control keyboard focus is on, while the person moved focus into
    /// the stack ([`Self::focus_newest`]).
    focus: Option<BannerTarget>,
    /// Where the pointer is (logical), followed also while a window under
    /// it takes the motion.
    pointer: Option<Point>,
    /// The pointer's trajectory, read for aim: heading for the stack and
    /// coming to rest on it, or passing over it on the way elsewhere.
    aim: ui::widget::EdgeAim,
    /// The last sample that confirmed the pointer was aiming at the stack.
    aimed_at: Option<Instant>,
    /// Where each banner on screen is drawn, by id: when the banners above
    /// it change it springs to its new slot (`motion::ListChange`) instead
    /// of jumping, and while the person holds the stack a slot that would
    /// close upward waits, so the banner under the pointer stays put.
    slots: Vec<(u64, ui::motion::MotionValue<f64>)>,
    /// Where the windows' content starts: banners begin below it, so the
    /// window bars' controls (maximise, close) under the corner stay
    /// reachable. Zero when the host has no windows to keep clear.
    clear_top: f64,
    /// The windows' bars on screen: a banner never covers one, the stack
    /// shifts below it instead ([`transient_slot`]).
    window_bars: Vec<Rect>,
}

/// The least a banner is tall, for where the first one may start.
const MIN_BANNER_H: f64 = 56.0;

/// Where a transient goes (a banner, the follow bar): the reserved zone on
/// the trailing side of `viewport`, the area under the top bar, inset from
/// its corner, starting no higher than `top`, `size` wide and tall. A
/// window's bar always wins: where the slot would cover one of
/// `window_bars`, it shifts down below that bar, until it covers none.
#[must_use]
pub fn transient_slot(viewport: Rect, top: f64, size: (f64, f64), window_bars: &[Rect]) -> Rect {
    let (width, height) = size;
    let x1 = viewport.x1 - EDGE_INSET;
    let mut y0 = top.max(viewport.y0 + EDGE_INSET);
    // Each shift clears at least one bar, so this ends.
    for _ in 0..=window_bars.len() {
        let slot = Rect::new(x1 - width, y0, x1, y0 + height);
        let below = window_bars
            .iter()
            .filter(|bar| bar.intersect(slot).area() > 0.0)
            .map(|bar| bar.y1 + TOAST_GAP)
            .fold(None, |most: Option<f64>, y| {
                Some(most.map_or(y, |m| m.max(y)))
            });
        match below {
            Some(y) if y > y0 => y0 = y,
            _ => break,
        }
    }
    Rect::new(x1 - width, y0, x1, y0 + height)
}

impl NotificationStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pin the stack into `viewport` (top-right corner, inset).
    pub fn set_viewport(&mut self, viewport: Rect) {
        if self.viewport != viewport {
            self.targets.cancel_pointer_press();
        }
        self.viewport = viewport;
    }

    /// Keep everything above `y` clear: the window bars, whose controls a
    /// banner pinned under the top bar would cover and take clicks from.
    pub fn set_clear_top(&mut self, y: f64) {
        if (self.clear_top - y).abs() > f64::EPSILON {
            self.targets.cancel_pointer_press();
        }
        self.clear_top = y;
    }

    /// Keep the windows' bars on screen clear: where the first banner
    /// would cover one, the stack starts below it.
    pub fn set_window_bars(&mut self, bars: Vec<Rect>) {
        if self.window_bars != bars {
            self.targets.cancel_pointer_press();
        }
        self.window_bars = bars;
    }

    /// Where the first banner's top sits: inset from the viewport's corner,
    /// and below the window bars the host keeps clear.
    fn first_top(&self) -> f64 {
        let top = (self.viewport.y0 + EDGE_INSET).max(self.clear_top + TOAST_GAP);
        transient_slot(
            self.viewport,
            top,
            (self.toast_width(), MIN_BANNER_H),
            &self.window_bars,
        )
        .y0
    }

    /// Exactly what the last paint covered; zero when nothing was painted.
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Whether the person holds the banners: keyboard focus is in the
    /// stack, or the pointer reaches for it (`Self::pointer_holds`).
    /// Held banners do not age.
    pub fn held(&self) -> bool {
        self.held_at(ui::time::now())
    }

    fn held_at(&self, now: Instant) -> bool {
        self.focus.is_some() || self.pointer_holds(now)
    }

    /// Whether the pointer holds the banners at `now`, by the toolkit's
    /// hover-intent rules ([`ui::widget::EdgeAim`]): it rests on the stack,
    /// or it is heading for it and coming to rest there (held for
    /// [`AIM_HOLD`] past the last sample that said so, so a hand that
    /// stops short lets go). A pointer passing over the stack on its way
    /// elsewhere is in transit and holds nothing.
    fn pointer_holds(&self, now: Instant) -> bool {
        let resting = self
            .pointer
            .is_some_and(|p| self.rect.contains(p) && self.aim.settled(now));
        resting
            || self
                .aimed_at
                .is_some_and(|at| now.saturating_duration_since(at) < AIM_HOLD)
    }

    fn register_target(&mut self, target: BannerTarget, rect: Rect, label: &str) {
        let key = target.key();
        self.targets.target(&key, rect, label);
        self.target_actions.push((key, target));
    }

    /// Press the control assistive technology names by `target` (one of
    /// the action targets [`Self::accessibility`] publishes), as a click on
    /// it does. Whether it was one of the stack's controls.
    pub fn perform_semantic(
        &mut self,
        host: &dyn NotificationHost,
        target: &str,
        action: &ui::a11y::Action,
    ) -> bool {
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        let Some((_, banner)) = self.target_actions.iter().find(|(id, _)| id == target) else {
            return false;
        };
        Self::dispatch_target(host, &banner.clone());
        true
    }

    /// Retain the matching release when foreground content interrupts a click.
    pub fn cancel_pointer_press(&mut self) {
        self.targets.cancel_pointer_press();
    }

    /// Whether an armed or cancelled banner gesture owns the next release.
    pub fn wants_pointer_capture(&self) -> bool {
        self.targets.wants_pointer_capture()
    }

    fn dispatch_target(host: &dyn NotificationHost, target: &BannerTarget) {
        if host.session_locked() {
            return;
        }
        let Some(id) = target.banner() else {
            host.open_center();
            return;
        };
        let available = {
            let log = host.log();
            log.active()
                .iter()
                .find(|notice| notice.id == id)
                .is_some_and(|notice| match target {
                    BannerTarget::Action(_, key) => host
                        .actions_for(notice)
                        .iter()
                        .any(|(current, _)| current == key),
                    _ => true,
                })
        };
        if !available {
            return;
        }
        match target {
            BannerTarget::Open(id) => host.activate(*id),
            BannerTarget::Dismiss(id) => host.dismiss(*id),
            BannerTarget::Action(id, key) => host.invoke_action(*id, key),
            BannerTarget::More => {}
        }
    }

    /// Tell the host what is on screen and whether it is held, now.
    fn present(&self, host: &dyn NotificationHost) {
        host.present(ui::time::now(), &self.on_screen, self.held());
    }

    /// Route the real pointer stream through the toolkit's completed-click
    /// controller. Banners do not take keyboard focus from the application
    /// on a click. The pointer resting on the stack holds the banners.
    /// Returns whether hover/press feedback changed.
    pub fn handle_pointer_event(
        &mut self,
        host: &dyn NotificationHost,
        event: &WindowEvent,
        scale_factor: f64,
        handled: &mut bool,
    ) -> bool {
        if host.session_locked() {
            self.targets.cancel_pointer_press();
        }
        let mut changed = match event {
            WindowEvent::CursorMoved { position, .. } => self.note_pointer(
                host,
                Some(Point::new(
                    position.x / scale_factor,
                    position.y / scale_factor,
                )),
            ),
            WindowEvent::CursorLeft { .. } => self.note_pointer(host, None),
            _ => false,
        };
        changed |= self
            .targets
            .handle_pointer_event(event, scale_factor, handled);
        let activated = self.targets.take_activated();
        for key in activated {
            if let Some((_, target)) = self.target_actions.iter().find(|(id, _)| *id == key) {
                Self::dispatch_target(host, target);
            }
        }
        changed
    }

    /// Follow the pointer at `point` (logical; `None` once it left the
    /// display), also while a window under it takes the motion: reaching
    /// for the stack holds the banners. Returns whether that changed.
    pub fn note_pointer(&mut self, host: &dyn NotificationHost, point: Option<Point>) -> bool {
        self.note_pointer_at(host, point, ui::time::now())
    }

    fn note_pointer_at(
        &mut self,
        host: &dyn NotificationHost,
        point: Option<Point>,
        now: Instant,
    ) -> bool {
        let before = self.pointer_holds(now);
        self.pointer = point;
        if let Some(p) = point {
            if !self.rect.is_zero_area() && self.aim.on_motion(p, now, &[self.rect]).is_some() {
                self.aimed_at = Some(now);
            }
        } else {
            self.aim.clear();
            self.aimed_at = None;
        }
        if self.pointer_holds(now) == before {
            return false;
        }
        host.present(now, &self.on_screen, self.held_at(now));
        true
    }

    /// Move keyboard focus into the newest banner on screen, onto its first
    /// action (or the card itself when it has none). Returns whether there
    /// was one to focus.
    pub fn focus_newest(&mut self, host: &dyn NotificationHost) -> bool {
        if host.session_locked() {
            return false;
        }
        let Some(row) = self.focus_rows.first() else {
            return false;
        };
        let first = row
            .controls
            .iter()
            .find(|(target, _, _)| matches!(target, BannerTarget::Action(..)))
            .or_else(|| row.controls.first())
            .map(|(target, _, _)| target.clone());
        self.focus = first;
        self.present(host);
        self.focus.is_some()
    }

    pub fn has_keyboard_focus(&self) -> bool {
        self.focus.is_some()
    }

    /// Whether `key` is one the focused stack acts on: the arrows, Tab,
    /// Return, Space and Escape. Any other key goes back to the person's
    /// window, and takes focus with it ([`Self::release_keyboard`]).
    pub fn owns_key(&self, key: &Key) -> bool {
        self.focus.is_some()
            && matches!(
                key,
                Key::Named(
                    NamedKey::ArrowUp
                        | NamedKey::ArrowDown
                        | NamedKey::ArrowLeft
                        | NamedKey::ArrowRight
                        | NamedKey::Tab
                        | NamedKey::Enter
                        | NamedKey::Space
                        | NamedKey::Escape
                )
            )
    }

    /// Give keyboard focus back to the person's window. Returns whether the
    /// stack had it.
    pub fn release_keyboard(&mut self, host: &dyn NotificationHost) -> bool {
        if self.focus.take().is_none() {
            return false;
        }
        self.present(host);
        true
    }

    /// Act on a key while keyboard focus is in the stack: Up and Down move
    /// between banners, Left, Right and Tab between a banner's controls,
    /// Return or Space runs the focused one, Escape dismisses the focused
    /// banner. Running or dismissing gives focus back to the window.
    /// Returns whether the key was taken.
    pub fn handle_key(&mut self, host: &dyn NotificationHost, key: &Key, shift: bool) -> bool {
        if !self.owns_key(key) {
            return false;
        }
        let Some((row, column)) = self.focus_position() else {
            // The focused banner left (it was answered elsewhere): focus
            // goes back to the window.
            self.release_keyboard(host);
            return true;
        };
        match key {
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown) => {
                let up = matches!(key, Key::Named(NamedKey::ArrowUp));
                let next = if up {
                    row.checked_sub(1)
                } else {
                    Some(row.saturating_add(1))
                };
                if let Some(target) = next
                    .and_then(|index| self.focus_rows.get(index))
                    .and_then(|row| row.controls.first())
                {
                    self.focus = Some(target.0.clone());
                }
            }
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight | NamedKey::Tab) => {
                let back = matches!(key, Key::Named(NamedKey::ArrowLeft))
                    || (shift && matches!(key, Key::Named(NamedKey::Tab)));
                let next = if back {
                    column.checked_sub(1)
                } else {
                    Some(column.saturating_add(1))
                };
                if let Some(target) = next.and_then(|index| {
                    self.focus_rows
                        .get(row)
                        .and_then(|row| row.controls.get(index))
                }) {
                    self.focus = Some(target.0.clone());
                }
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                if let Some(target) = self.focus.take() {
                    Self::dispatch_target(host, &target);
                }
                self.present(host);
            }
            Key::Named(NamedKey::Escape) => {
                if let Some(id) = self.focus.take().and_then(|target| target.banner())
                    && !host.session_locked()
                {
                    host.dismiss(id);
                }
                self.present(host);
            }
            _ => {}
        }
        true
    }

    /// Where the focused control sits in the last paint's keyboard order.
    fn focus_position(&self) -> Option<(usize, usize)> {
        let focus = self.focus.as_ref()?;
        self.focus_rows.iter().enumerate().find_map(|(row, line)| {
            line.controls
                .iter()
                .position(|(target, _, _)| target == focus)
                .map(|column| (row, column))
        })
    }

    /// The moment the banner first came on screen, for its entry.
    fn shown_at(n: &Notification, now: Instant) -> Instant {
        n.dwell.first_shown().unwrap_or(now)
    }

    /// Enter/exit alpha + horizontal slide: a banner comes in from its
    /// edge (`ui::motion::PanelSlide::sheet_pose`) when it first comes on
    /// screen, and fades out in place so that it is gone at expiry. A held
    /// banner does not fade; under reduced motion a banner simply stands.
    fn animation_state(
        n: &Notification,
        now: Instant,
        (reduce_motion, held): (bool, bool),
        travel_width: f64,
    ) -> (f32, f64) {
        let (spec, motion) = banner_motion(reduce_motion);
        let source = ui::tokens::MotionSource::Pointer;
        let age = now.saturating_duration_since(Self::shown_at(n, now));
        let enter = spec.progress_after(ui::motion::Direction::Enter, motion, source, age);
        // Under reduced motion it fades in where it rests; otherwise it
        // slides in from its edge.
        let (arrive, dx) = if reduce_motion {
            (ui::motion::unit_opacity(enter), 0.0)
        } else {
            let pose = ui::motion::PanelSlide::sheet_pose(enter, travel_width, true, motion);
            (pose.opacity, pose.to_drawn(Point::ZERO).x)
        };
        // Exit fades only: the card drops in place rather than retracting.
        let remaining = n.time_left(now);
        let leaving = spec.settle_time(ui::motion::Direction::Exit, motion, source);
        let exit = if !held && remaining < leaving {
            1.0 - spec.progress_after(
                ui::motion::Direction::Exit,
                motion,
                source,
                leaving.saturating_sub(remaining),
            )
        } else {
            1.0
        };
        let alpha = arrive * ui::motion::unit_opacity(exit);
        (alpha.clamp(0.0, 1.0), dx)
    }

    /// Where banner `id`, laid out at `top`, is drawn at `now`: it springs
    /// to a new slot (`motion::ListChange`); a new banner starts in its
    /// slot (its arrival is the slide). While the stack is `held`, a slot
    /// closing upward waits, so nothing moves under the pointer.
    fn slot_top(&mut self, id: u64, top: f64, now: Instant, held: bool) -> f64 {
        let motion = banner_motion(self.reduce_motion).1;
        let timing = ui::motion::ListChange::MOVE.timing(
            ui::motion::Direction::Enter,
            motion,
            ui::tokens::MotionSource::Pointer,
        );
        if let Some((_, slot)) = self.slots.iter_mut().find(|(slot_id, _)| *slot_id == id) {
            let target = slot.target();
            let closes_up = top < target;
            if (top - target).abs() > SLOT_TOLERANCE && !(held && closes_up) {
                slot.set(top, timing, now);
            }
            return slot.get(now);
        }
        self.slots.push((
            id,
            ui::motion::MotionValue::new(top).with_tolerance(SLOT_TOLERANCE),
        ));
        top
    }

    /// Cards keep their designed width on ordinary displays and shrink on
    /// compact viewports instead of painting beyond the left edge.
    fn toast_width(&self) -> f64 {
        (self.viewport.width() - EDGE_INSET * 2.0).clamp(0.0, TOAST_WIDTH)
    }

    /// Locked-session collapse: a single redacted card showing only `count`,
    /// no hit rects.
    fn draw_locked_card(ctx: &mut PaintCtx, x0: f64, y: f64, width: f64, count: usize) -> Rect {
        let title_style = ctx.theme.text(TextRole::Caption);
        let card_rect = Rect::new(x0, y, x0 + width, y + TOAST_HEIGHT);
        // The floating card the banners it stands in for are made of.
        ui::widget::toast::paint_card_surface(ctx.scene, ctx.transform, card_rect, 1.0, ctx.theme);
        let label = lock_suppressed_label(count);
        // Centre on the measured line, not the font size (HIG §7a).
        let max_width = f32_of_f64(width - CARD_PADDING_X * 2.0);
        let (_, label_h) = ctx
            .text_renderer
            .measure_style(&label, Some(max_width), &title_style);
        ctx.text_renderer.draw_style(
            ctx.scene,
            &label,
            ctx.theme.tokens.colors.text_dim,
            ctx.transform.pre_translate(
                (
                    card_rect.x0 + CARD_PADDING_X,
                    card_rect.y0 + (TOAST_HEIGHT - f64::from(label_h)) / 2.0,
                )
                    .into(),
            ),
            Some(max_width),
            &title_style,
        );
        card_rect
    }

    /// The "more" card's label and height for `count` banners.
    fn more_card(ctx: &mut PaintCtx, width: f64, count: usize) -> (String, f64, f64) {
        let style = ctx.theme.text(TextRole::Label);
        let label = t_args("notification-more", &[("count", count.into())]);
        let max_width = f32_of_f64((width - CARD_PADDING_X * 2.0).max(1.0));
        let (_, label_h) = ctx
            .text_renderer
            .measure_style(&label, Some(max_width), &style);
        let height =
            MORE_HEIGHT.max(f64::from(label_h) + 2.0 * f64::from(ctx.theme.tokens.spacing.s2));
        (label, height, f64::from(label_h))
    }

    /// The card standing in for `count` banners that did not fit: one line,
    /// the same floating card, opening the notification centre.
    fn draw_more_card(&mut self, ctx: &mut PaintCtx, rect: Rect, label: &str, label_h: f64) {
        let style = ctx.theme.text(TextRole::Label);
        // Like a banner's own body, the card is pressable as a whole and
        // shows no hover film: passing over it is not aiming at it.
        ui::widget::toast::paint_card_surface(ctx.scene, ctx.transform, rect, 1.0, ctx.theme);
        let max_width = f32_of_f64((rect.width() - CARD_PADDING_X * 2.0).max(1.0));
        ctx.text_renderer.draw_style(
            ctx.scene,
            label,
            ctx.theme.tokens.colors.text_primary,
            ctx.transform.pre_translate(
                (
                    rect.x0 + CARD_PADDING_X,
                    rect.y0 + (rect.height() - label_h) / 2.0,
                )
                    .into(),
            ),
            Some(max_width),
            &style,
        );
        self.register_target(BannerTarget::More, rect, label);
        self.action_buttons
            .push((rect, label.to_owned(), BannerTarget::More.key()));
        self.focus_rows.push(FocusRow {
            controls: vec![(
                BannerTarget::More,
                rect,
                ui::widget::toast::card_radius(ctx.theme),
            )],
        });
        self.more_rect = rect;
    }

    /// The shared card for banner `n`, with its interaction state.
    fn banner_card<'n>(
        &self,
        n: &'n Notification,
        actions: &'n [(String, String)],
    ) -> ToastCard<'n> {
        // The stack owns what to show and click routing; the toast
        // widget owns the card's look and hands back the close rect.
        let mut card = ToastCard::new(&n.title, n.severity.toast_kind())
            .icon(n.icon_kind.icon())
            .body(if n.show_preview { &n.body } else { "" })
            .caption(n.app_name.as_deref().unwrap_or(""))
            // A card that offers its own way out ("Not now", "Dismiss")
            // needs no second one in its corner.
            .closable(
                !actions
                    .iter()
                    .any(|(key, _)| key == super::model::NOT_NOW_ACTION || key == "dismiss"),
            )
            .actionable(!n.actions.is_empty());
        // A person's notification leads with them; the app stays the
        // small source line above the title.
        if let Some(person) = &n.from {
            card = card.person(&person.name, &person.identity);
        }
        let close = self.targets.interaction(&BannerTarget::Dismiss(n.id).key());
        card.close_hovered = close.hovered;
        card.close_pressed = close.pressed;
        if let Some((key, label)) = actions.first() {
            card.primary_action = label;
            let state = self
                .targets
                .interaction(&BannerTarget::Action(n.id, key.clone()).key());
            card.primary_action_hovered = state.hovered;
            card.primary_action_pressed = state.pressed;
        }
        if let Some((key, label)) = actions.get(1) {
            card.secondary_action = card.primary_action;
            card.secondary_action_hovered = card.primary_action_hovered;
            card.secondary_action_pressed = card.primary_action_pressed;
            card.primary_action = label;
            let state = self
                .targets
                .interaction(&BannerTarget::Action(n.id, key.clone()).key());
            card.primary_action_hovered = state.hovered;
            card.primary_action_pressed = state.pressed;
        }
        card
    }

    /// Paint each live card top to bottom from `y`, newest first, recording
    /// its hit rects and folding its bounds into `bounds`. Banners that do
    /// not fit fold into one "more" card under the last that does.
    fn draw_toast_cards(
        &mut self,
        ctx: &mut PaintCtx,
        banners: &[&Notification],
        now: Instant,
        (x0_rest, width): (f64, f64),
        mut y: f64,
        bounds: &mut StackBounds,
    ) {
        let bottom = self.viewport.y1 - EDGE_INSET;
        let card_radius = ui::widget::toast::card_radius(ctx.theme);
        let control_radius = f64::from(ctx.theme.tokens.radii.control);
        let held = self.held();
        for (index, n) in banners.iter().enumerate() {
            let actions = ctx.host.actions_for(n);
            let card = self.banner_card(n, &actions);
            let height = ui::widget::toast::card_height(ctx.text_renderer, ctx.theme, &card, width)
                .max(TOAST_HEIGHT);
            // The rest fold into the "more" card; a banner that is not the
            // last must leave room for it.
            let below = banners.len().saturating_sub(index.saturating_add(1));
            let reserve = if below > 0 {
                Self::more_card(ctx, width, below).1 + TOAST_GAP
            } else {
                0.0
            };
            if y + height + reserve > bottom {
                let hidden = banners.len().saturating_sub(index);
                let (label, more_h, label_h) = Self::more_card(ctx, width, hidden);
                if y + more_h <= bottom {
                    let rect = Rect::new(x0_rest, y, x0_rest + width, y + more_h);
                    bounds.expand(rect.y0, rect.y1);
                    self.draw_more_card(ctx, rect, &label, label_h);
                }
                return;
            }
            self.on_screen.push(n.id);
            let (alpha, slide_dx) =
                Self::animation_state(n, now, (self.reduce_motion, held), width);
            let card_x0 = x0_rest + slide_dx;
            let top = self.slot_top(n.id, y, now, held);
            let card_rect = Rect::new(card_x0, top, card_x0 + width, top + height);
            bounds.expand(card_rect.y0, card_rect.y1);
            if alpha <= 0.0 {
                // A banner's fade starts the frame it first comes on screen,
                // at no opacity. That frame still records its card, unseen,
                // so it differs from the last one where the banner arrives:
                // drawn as nothing, the frame matched the last and repainted
                // nothing until a later frame happened to come.
                ui::widget::toast::paint_card_surface(
                    ctx.scene,
                    ctx.transform,
                    card_rect,
                    0.0,
                    ctx.theme,
                );
                y += height + TOAST_GAP;
                continue;
            }
            let mut card = card;
            card.alpha = alpha;
            let hit = ui::widget::toast::paint_card(
                ctx.scene,
                ctx.transform,
                ctx.text_renderer,
                ctx.theme,
                card_rect,
                &card,
            );
            let mut row = FocusRow {
                controls: vec![(BannerTarget::Open(n.id), card_rect, card_radius)],
            };
            self.register_target(BannerTarget::Open(n.id), card_rect, &n.title);
            self.action_buttons
                .push((card_rect, n.title.clone(), BannerTarget::Open(n.id).key()));
            // Painted left to right: the secondary choice, then the primary.
            let mut choices = Vec::new();
            if let Some((key, label)) = actions.first() {
                let rect = if actions.len() >= 2 {
                    hit.secondary_action_rect
                } else {
                    hit.primary_action_rect
                };
                choices.push((key, label, rect));
            }
            if let Some((key, label)) = actions.get(1) {
                choices.push((key, label, hit.primary_action_rect));
            }
            for (key, label, rect) in choices {
                let target = BannerTarget::Action(n.id, key.clone());
                self.register_target(target.clone(), rect, label);
                self.action_rects.push((n.id, key.clone(), rect));
                self.action_buttons
                    .push((rect, label.clone(), target.key()));
                row.controls.push((target, rect, control_radius));
            }
            if card.closable {
                let target = BannerTarget::Dismiss(n.id);
                self.register_target(
                    target.clone(),
                    hit.close_rect,
                    &t("notification-center-dismiss"),
                );
                row.controls.push((target, hit.close_rect, control_radius));
            }
            self.focus_rows.push(row);
            self.hit_rects.push((n.id, card_rect, hit.close_rect));
            y += height + TOAST_GAP;
        }
    }

    /// The keyboard focus ring, on the control focus is on.
    fn paint_focus(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let Some(focus) = self.focus.as_ref() else {
            return;
        };
        if let Some((_, rect, radius)) = self
            .focus_rows
            .iter()
            .flat_map(|row| row.controls.iter())
            .find(|(target, _, _)| target == focus)
        {
            ui::widget::paint_focus_ring(scene, transform, *rect, *radius, theme);
        }
    }

    /// Paint the stack: the locked card, or the live cards. Then tell the
    /// host which banners are on screen, so only those age.
    pub fn draw(
        &mut self,
        host: &dyn NotificationHost,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.hit_rects.clear();
        self.action_rects.clear();
        self.action_buttons.clear();
        self.targets.begin_frame();
        self.target_actions.clear();
        self.on_screen.clear();
        self.focus_rows.clear();
        self.more_rect = Rect::ZERO;
        self.reduce_motion = theme.reduce_motion;
        self.rect = Rect::ZERO;
        self.paint(host, scene, transform, text_renderer, theme);
        if self.focus_position().is_none() {
            // The focused banner left: focus goes back to the window.
            self.focus = None;
        }
        if self.rect.is_zero_area() {
            self.aimed_at = None;
        }
        self.paint_focus(scene, transform, theme);
        self.present(host);
    }

    fn paint(
        &mut self,
        host: &dyn NotificationHost,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.viewport.area() == 0.0 {
            return;
        }
        let locked = host.session_locked();
        let log = host.log();
        let shown = |n: &&Notification| !locked || n.show_on_lock_screen;
        let banners: Vec<&Notification> = log.active().iter().rev().filter(shown).collect();
        if banners.is_empty() {
            return;
        }
        let width = self.toast_width();
        if width <= 0.0 {
            return;
        }
        // Theme type tokens already carry the user's text scale, and the
        // stack (and the toast card it paints) lays out with those resolved
        // sizes; the renderer must not scale them again (HIG §7a). Reset for
        // this pass and restore it below.
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        let x0_rest = self.viewport.x1 - EDGE_INSET - width;
        let y = self.first_top();
        let mut ctx = PaintCtx {
            scene,
            transform,
            text_renderer,
            theme,
            host,
        };
        if locked {
            self.rect = Self::draw_locked_card(&mut ctx, x0_rest, y, width, banners.len());
            ctx.text_renderer.set_text_scale(renderer_scale);
            return;
        }
        let mut bounds = StackBounds {
            min_y: f64::INFINITY,
            max_y: f64::NEG_INFINITY,
        };
        self.draw_toast_cards(
            &mut ctx,
            &banners,
            ui::time::now(),
            (x0_rest, width),
            y,
            &mut bounds,
        );
        // Slots of banners no longer on screen are forgotten: one that comes
        // back starts in its slot.
        let on_screen = &self.on_screen;
        self.slots.retain(|(id, _)| on_screen.contains(id));
        ctx.text_renderer.set_text_scale(renderer_scale);
        if bounds.min_y.is_finite() {
            self.rect = Rect::new(x0_rest, bounds.min_y, x0_rest + width, bounds.max_y);
        }
    }

    /// The banners about the whole machine (critical battery) on a display
    /// other than the one the stack lives on, pinned top-right into
    /// `viewport` as the stack pins its own: the same cards at rest, newest
    /// first, without controls, hover or focus, which stay with the stack on
    /// the display in use. Nothing while the session is locked. Like every
    /// transient they never cover one of `window_bars`. Returns what the
    /// cards cover, zero when nothing was painted.
    pub fn draw_machine_wide(
        host: &dyn NotificationHost,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        (viewport, window_bars): (Rect, &[Rect]),
    ) -> Rect {
        if viewport.area() == 0.0 || host.session_locked() {
            return Rect::ZERO;
        }
        let log = host.log();
        let banners: Vec<&Notification> = log
            .active()
            .iter()
            .rev()
            .filter(|n| n.machine_wide)
            .collect();
        let width = (viewport.width() - EDGE_INSET * 2.0).clamp(0.0, TOAST_WIDTH);
        if banners.is_empty() || width <= 0.0 {
            return Rect::ZERO;
        }
        // Resolved type sizes, as the stack paints them (HIG §7a).
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        let x0 = viewport.x1 - EDGE_INSET - width;
        let bottom = viewport.y1 - EDGE_INSET;
        let mut y = transient_slot(viewport, viewport.y0, (width, MIN_BANNER_H), window_bars).y0;
        let mut covered = Rect::ZERO;
        for n in banners {
            let card = ToastCard::new(&n.title, n.severity.toast_kind())
                .icon(n.icon_kind.icon())
                .body(if n.show_preview { &n.body } else { "" })
                .caption(n.app_name.as_deref().unwrap_or(""))
                .closable(false);
            let height = ui::widget::toast::card_height(text_renderer, theme, &card, width)
                .max(TOAST_HEIGHT);
            if y + height > bottom {
                break;
            }
            let rect = Rect::new(x0, y, x0 + width, y + height);
            let _ =
                ui::widget::toast::paint_card(scene, transform, text_renderer, theme, rect, &card);
            covered = if covered.is_zero_area() {
                rect
            } else {
                covered.union(rect)
            };
            y += height + TOAST_GAP;
        }
        text_renderer.set_text_scale(renderer_scale);
        covered
    }

    /// A call rings now: an incoming-call banner is up and the session is
    /// unlocked. Its card is on the display in use; the other displays show
    /// a quiet edge.
    pub fn call_ringing(host: &dyn NotificationHost) -> bool {
        !host.session_locked()
            && host
                .log()
                .active()
                .iter()
                .any(|n| n.addressed() == Some(Addressed::IncomingCall))
    }

    /// The notification whose banner is drawn at `p`.
    pub fn banner_at(&self, p: Point) -> Option<u64> {
        self.hit_rects
            .iter()
            .find(|(_, full, _)| full.contains(p))
            .map(|(id, _, _)| *id)
    }

    /// Activate a completed click at p. Interactive hosts forward the full
    /// pointer stream through `handle_pointer_event` so gestures remain cancellable.
    /// Returns whether the completed click landed on a banner.
    pub fn click_at(&mut self, host: &dyn NotificationHost, p: Point) -> bool {
        for (id, key, rect) in &self.action_rects {
            if rect.contains(p) {
                host.invoke_action(*id, key);
                return true;
            }
        }
        for &(id, full, close) in &self.hit_rects {
            if close.contains(p) {
                host.dismiss(id);
                return true;
            }
            if full.contains(p) {
                host.activate(id);
                return true;
            }
        }
        if self.more_rect.contains(p) {
            host.open_center();
            return true;
        }
        false
    }

    /// Whether a banner on screen is mid-entry or mid-exit.
    pub fn needs_animation_frame(&self, host: &dyn NotificationHost) -> bool {
        let now = ui::time::now();
        if self.slots.iter().any(|(_, slot)| slot.is_moving(now)) {
            return true;
        }
        let (spec, motion) = banner_motion(self.reduce_motion);
        let source = ui::tokens::MotionSource::Pointer;
        let entry = spec.settle_time(ui::motion::Direction::Enter, motion, source);
        let window = spec.settle_time(ui::motion::Direction::Exit, motion, source);
        let held = self.held();
        host.log()
            .active()
            .iter()
            .filter(|n| self.on_screen.contains(&n.id))
            .any(|n| {
                let entering = n
                    .dwell
                    .first_shown()
                    .is_none_or(|shown| now.saturating_duration_since(shown) < entry);
                let remaining = n.time_left(now);
                let leaving =
                    !held && n.dwell.is_running() && !remaining.is_zero() && remaining <= window;
                entering || leaving
            })
    }

    /// The next instant a banner on screen starts leaving or expires.
    /// Nothing wakes for a held banner or one off screen: their time stands
    /// still.
    pub fn next_wake_deadline(&self, host: &dyn NotificationHost) -> Option<Instant> {
        let (spec, motion) = banner_motion(self.reduce_motion);
        let window = spec.settle_time(
            ui::motion::Direction::Exit,
            motion,
            ui::tokens::MotionSource::Pointer,
        );
        let now = ui::time::now();
        // The pointer's hold changes with no further input: a pointer that
        // stops on the stack comes to rest, an aim lapses.
        let resting = self
            .pointer
            .filter(|p| self.rect.contains(*p))
            .and_then(|_| self.aim.deadline(now));
        let aim_lapses = self
            .aimed_at
            .and_then(|at| at.checked_add(AIM_HOLD))
            .filter(|at| *at > now);
        let banners = host
            .log()
            .active()
            .iter()
            .filter_map(|notification| {
                let expiry = notification.dwell.ends_at(notification.dismiss_after)?;
                let exit_start = expiry.checked_sub(window).unwrap_or(expiry);
                // While an animation runs the frame hook drives it; the next
                // useful sleeping deadline is the next phase edge.
                Some(if exit_start > now { exit_start } else { expiry })
            })
            .min();
        [banners, resting, aim_lapses].into_iter().flatten().min()
    }

    /// A live region, so a screen reader announces a freshly-posted banner.
    /// While locked only the count is exposed, and nothing when there is
    /// nothing to count.
    pub fn accessibility(&self, host: &dyn NotificationHost) -> Option<AccessNode> {
        use ui::a11y::{AccessState, Politeness, Role};
        let log = host.log();
        if host.session_locked() {
            let n = log.active().len();
            if n == 0 {
                return None;
            }
            return Some(
                AccessNode::new(Role::Group)
                    .with_name(t("notification-stack-a11y"))
                    .with_live(Politeness::Polite)
                    .with_value(lock_suppressed_label(n)),
            );
        }
        let latest = log.active().last();
        let text = latest.map_or_else(String::new, |t| {
            let body = if t.show_preview { t.body.as_str() } else { "" };
            if body.is_empty() {
                t.title.clone()
            } else {
                format!("{}. {body}", t.title)
            }
        });
        // Ordinary arrivals wait their turn; only an error cuts in. An empty
        // stack publishes an empty value so its clearing is silent.
        let politeness = if latest.is_some_and(|t| t.severity == NotificationSeverity::Error) {
            Politeness::Assertive
        } else {
            Politeness::Polite
        };
        let focused = self.focus.as_ref().and_then(|focus| {
            self.focus_rows
                .iter()
                .flat_map(|row| row.controls.iter())
                .find(|(target, _, _)| target == focus)
                .map(|(_, rect, _)| *rect)
        });
        Some(
            AccessNode::new(Role::Group)
                .with_name(t("notification-stack-a11y"))
                .with_live(politeness)
                .with_value(text)
                .with_children(
                    self.action_buttons
                        .iter()
                        .map(|(rect, label, target)| {
                            AccessNode::new(Role::Button)
                                .with_name(label.clone())
                                .with_bounds(*rect)
                                // Pressed by assistive technology as by a
                                // click: [`Self::perform_semantic`].
                                .with_action_target(target.clone())
                                .with_state(
                                    AccessState::new()
                                        .focusable(true)
                                        .focused(focused == Some(*rect)),
                                )
                        })
                        .collect(),
                ),
        )
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::{HistoryActivation, NotificationLog};
    use super::*;
    use std::cell::{Cell, RefCell};
    use ui::time::Duration;

    /// A host with no senders: dismissing retains, activating retains.
    #[derive(Default)]
    pub(crate) struct TestHost {
        pub log: RefCell<NotificationLog>,
        pub locked: Cell<bool>,
        pub quiet: RefCell<Option<String>>,
        pub actions: RefCell<Vec<(String, String)>>,
        pub invoked: RefCell<Vec<(u64, String)>>,
        pub centre_opened: Cell<usize>,
    }

    impl TestHost {
        pub fn push(&self, notification: Notification) -> u64 {
            let id = notification.id;
            let evicted = self.log.borrow_mut().show(notification);
            for n in evicted {
                self.log.borrow_mut().retain(n);
            }
            id
        }
    }

    impl NotificationHost for TestHost {
        fn actions_for(&self, _notice: &Notification) -> Vec<(String, String)> {
            self.actions.borrow().clone()
        }
        fn invoke_action(&self, id: u64, key: &str) {
            self.invoked.borrow_mut().push((id, key.to_owned()));
        }
        fn log(&self) -> std::cell::Ref<'_, NotificationLog> {
            self.log.borrow()
        }
        fn session_locked(&self) -> bool {
            self.locked.get()
        }
        fn quiet_status(&self) -> Option<String> {
            self.quiet.borrow().clone()
        }
        fn present(&self, now: Instant, on_screen: &[u64], held: bool) {
            self.log.borrow_mut().present(now, on_screen, held);
        }
        fn open_center(&self) {
            self.centre_opened
                .set(self.centre_opened.get().saturating_add(1));
        }
        fn dismiss(&self, id: u64) {
            let taken = self.log.borrow_mut().take_active(id);
            if let Some(n) = taken {
                self.log.borrow_mut().retain(n);
            }
        }
        fn activate(&self, id: u64) {
            self.dismiss(id);
        }
        fn activate_history(&self, id: u64) -> HistoryActivation {
            let Some(n) = self.log.borrow().history_entry(id).cloned() else {
                return HistoryActivation::Missing;
            };
            let Some(app_id) = n.app_id.clone() else {
                return HistoryActivation::Unavailable;
            };
            self.log.borrow_mut().take_history(id);
            HistoryActivation::OpenApp {
                app_id,
                label: super::super::group_label(&n),
            }
        }
        fn dismiss_history(&self, id: u64) {
            self.log.borrow_mut().take_history(id);
        }
        fn clear_history(&self) {
            self.log.borrow_mut().clear_history(ui::time::now());
        }
        fn undo_clear(&self) {
            self.log.borrow_mut().undo_clear();
        }
    }

    #[test]
    fn clicking_a_painted_close_rect_dismisses_only_that_notification() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = ui::motion::testing::settled();
        let host = TestHost::default();
        let first = host.push(Notification::new("First", ""));
        let second = host.push(Notification::new("Second", ""));
        let mut stack = NotificationStack::new();
        // The paint records this exact geometry; seeding it keeps the routing
        // test independent of a GPU while exercising the production hit test.
        stack.hit_rects.push((
            first,
            Rect::new(100.0, 100.0, 460.0, 164.0),
            Rect::new(430.0, 106.0, 452.0, 128.0),
        ));
        stack.hit_rects.push((
            second,
            Rect::new(100.0, 172.0, 460.0, 236.0),
            Rect::new(430.0, 178.0, 452.0, 200.0),
        ));
        assert!(stack.click_at(&host, Point::new(441.0, 189.0)));
        let remaining: Vec<_> = host
            .log()
            .active()
            .iter()
            .map(|n| n.title.clone())
            .collect();
        assert_eq!(remaining, vec!["First".to_string()]);
        assert_eq!(
            host.log().history().len(),
            1,
            "a dismissed banner is retained"
        );
        stack.hit_rects.clear();
        assert!(!stack.click_at(&host, Point::new(441.0, 189.0)));
    }

    #[test]
    fn shared_choice_buttons_route_their_keys_and_retire_stale_geometry() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = ui::motion::testing::settled();
        for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
            let theme = theme.with_reduce_motion(true).with_text_scale(2.0);
            let host = TestHost::default();
            host.actions.replace(vec![
                ("dismiss".into(), "Dismiss".into()),
                ("snooze".into(), "Snooze".into()),
            ]);
            let id = host.push(Notification::new("Alarm", "Synthetic alarm"));
            let mut stack = NotificationStack::new();
            stack.set_viewport(Rect::new(0.0, 0.0, 320.0, 600.0));
            let mut text = TextRenderer::new();
            stack.draw(
                &host,
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut text,
                &theme,
            );
            assert_eq!(
                stack.action_rects.len(),
                2,
                "both real painted controls must exist"
            );
            let actions = stack.action_rects.clone();
            let buttons = stack.accessibility(&host).expect("a live region").children;
            let mut named: Vec<_> = buttons
                .iter()
                .map(|button| (button.role, button.name.clone().unwrap_or_default()))
                .collect();
            named.sort_by(|a, b| a.1.cmp(&b.1));
            assert_eq!(
                named,
                [
                    (ui::a11y::Role::Button, "Alarm".to_owned()),
                    (ui::a11y::Role::Button, "Dismiss".to_owned()),
                    (ui::a11y::Role::Button, "Snooze".to_owned())
                ],
                "the painted choices are buttons a reader can press"
            );
            // Assistive technology presses them by their targets, as a
            // click does: each choice reaches the host with its key.
            for (_, key, _) in &actions {
                let label = if key == "dismiss" {
                    "Dismiss"
                } else {
                    "Snooze"
                };
                let target = buttons
                    .iter()
                    .find(|button| button.name.as_deref() == Some(label))
                    .and_then(|button| button.action_target.clone())
                    .expect("a choice a reader can press carries its target");
                assert!(stack.perform_semantic(&host, &target, &ui::a11y::Action::Activate));
                assert_eq!(host.invoked.borrow().last(), Some(&(id, key.clone())));
            }
            assert!(!stack.perform_semantic(
                &host,
                "notification:none",
                &ui::a11y::Action::Activate
            ));
            let card = stack.hit_rects[0].1;
            for (actual, _, rect) in &actions {
                assert_eq!(*actual, id);
                assert!(rect.width() > 0.0 && rect.height() >= 28.0);
                assert!(card.contains(rect.origin()) && card.x1 >= rect.x1 && card.y1 >= rect.y1);
            }
            assert!(actions[0].2.intersect(actions[1].2).area() <= 0.0);
            for (_, key, rect) in &actions {
                assert!(stack.click_at(&host, rect.center()));
                assert_eq!(host.invoked.borrow().last(), Some(&(id, key.clone())));
            }
            assert_eq!(
                host.log().active().len(),
                1,
                "host owns durable completion, not the painter"
            );
            host.log.borrow_mut().take_active(id);
            stack.draw(
                &host,
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut text,
                &theme,
            );
            assert!(stack.action_rects.is_empty());
            assert!(!stack.click_at(&host, actions[1].2.center()));
        }
    }

    #[test]
    fn the_frame_a_banner_arrives_differs_from_the_one_before() {
        // The running clock: an arriving banner's fade is at its start.
        for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
            for theme in [theme.clone().with_reduce_motion(true), theme] {
                let host = TestHost::default();
                let mut stack = NotificationStack::new();
                stack.set_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
                let mut text = TextRenderer::new();
                let mut before = Scene::new();
                stack.draw(&host, &mut before, Affine::IDENTITY, &mut text, &theme);
                host.push(Notification::new("Build finished", "it passed its tests"));
                let mut arriving = Scene::new();
                stack.draw(&host, &mut arriving, Affine::IDENTITY, &mut text, &theme);
                assert_ne!(
                    arriving.encoding().n_paths,
                    before.encoding().n_paths,
                    "the frame a banner arrives records it, though unseen"
                );
                assert!(!stack.rect().is_zero_area(), "it covers where it arrives");
            }
        }
    }

    #[test]
    fn scaled_attributed_banner_reserves_text_above_real_action_controls() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = ui::motion::testing::settled();
        for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
            let theme = theme.with_reduce_motion(true).with_text_scale(2.0);
            let host = TestHost::default();
            host.actions.replace(vec![
                ("dismiss".into(), "Dismiss".into()),
                ("snooze".into(), "Snooze".into()),
            ]);
            host.push(
                Notification::new("Meeting begins", "Join your workspace")
                    .with_app_name("Calendar"),
            );
            let mut stack = NotificationStack::new();
            stack.set_viewport(Rect::new(0.0, 0.0, 320.0, 600.0));
            let mut text = TextRenderer::new();
            stack.draw(
                &host,
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut text,
                &theme,
            );
            let card = stack.hit_rects[0].1;
            let actions_top = stack
                .action_rects
                .iter()
                .map(|(_, _, rect)| rect.y0)
                .reduce(f64::min)
                .expect("real shared actions");
            let content_height: f64 = [
                ("Calendar", theme.text(TextRole::Caption)),
                ("Meeting begins", theme.text(TextRole::Label).strong()),
                ("Join your workspace", theme.text(TextRole::Caption)),
            ]
            .into_iter()
            .map(|(label, style)| f64::from(text.measure_style(label, None, &style).1))
            .sum();
            assert!(
                actions_top - card.y0 >= content_height + 2.0 * f64::from(theme.tokens.spacing.s2),
                "painted text requires {content_height}px plus padding before actions at {}px",
                actions_top - card.y0
            );
        }
    }

    #[test]
    fn persistent_alarm_choices_fit_the_viewport_and_keep_the_newest_alarm_visible() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = ui::motion::testing::settled();
        for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
            let theme = theme.with_reduce_motion(true).with_text_scale(2.0);
            let host = TestHost::default();
            host.actions.replace(vec![
                ("dismiss".into(), "Dismiss".into()),
                ("snooze".into(), "Snooze".into()),
            ]);
            let mut newest = 0;
            for index in 0..super::super::MAX_NOTIFICATIONS {
                newest = host.push(
                    Notification::new(format!("Alarm {index}"), "Fixture")
                        .with_dismiss_after(super::super::PERSISTENT_DISMISS),
                );
            }
            let viewport = Rect::new(0.0, 0.0, 320.0, 600.0);
            let mut stack = NotificationStack::new();
            stack.set_viewport(viewport);
            stack.draw(
                &host,
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &theme,
            );
            assert!(
                !stack.hit_rects.is_empty(),
                "the newest alarm must actually paint"
            );
            assert!(
                stack.hit_rects.len() < host.log().active().len(),
                "fixture must exercise capacity"
            );
            assert_eq!(stack.hit_rects[0].0, newest);
            for (_, card, _) in &stack.hit_rects {
                assert!(
                    card.y0 >= viewport.y0 && card.y1 <= viewport.y1,
                    "a painted card escaped the viewport"
                );
            }
            for (_, _, control) in &stack.action_rects {
                assert!(
                    control.y0 >= viewport.y0 && control.y1 <= viewport.y1,
                    "a real alarm choice is unreachable below the viewport"
                );
            }
            assert_eq!(
                host.log().active().len(),
                super::super::MAX_NOTIFICATIONS,
                "fitting banners must not discard alarm data"
            );
        }
    }

    #[test]
    fn compact_viewport_keeps_notification_inside_both_edges() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = ui::motion::testing::settled();
        let mut stack = NotificationStack::new();
        stack.set_viewport(Rect::new(0.0, 0.0, 320.0, 480.0));
        let width = stack.toast_width();
        let x0 = stack.viewport.x1 - EDGE_INSET - width;
        assert_eq!(width, 272.0);
        assert_eq!(x0, EDGE_INSET);
    }

    fn draw(stack: &mut NotificationStack, host: &TestHost, theme: &Theme) {
        stack.draw(
            host,
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            theme,
        );
    }

    /// A transient's slot shifts below any window bar it would cover, and
    /// stays where nothing is under it.
    #[test]
    fn a_window_bar_wins_and_the_transient_shifts() {
        let viewport = Rect::new(0.0, 40.0, 1600.0, 900.0);
        let free = transient_slot(viewport, 0.0, (360.0, 60.0), &[]);
        assert_eq!(free.y0, 40.0 + EDGE_INSET);
        assert_eq!(free.x1, 1600.0 - EDGE_INSET);
        let bars = [
            Rect::new(900.0, 48.0, 1590.0, 92.0),
            Rect::new(0.0, 48.0, 880.0, 400.0),
        ];
        let slot = transient_slot(viewport, 0.0, (360.0, 60.0), &bars);
        assert!(bars.iter().all(|bar| bar.intersect(slot).area() <= 0.0));
        assert_eq!(slot.y0, 92.0 + TOAST_GAP, "just below the bar it met");
    }

    /// Banners start below the window bars the host keeps clear, so a
    /// window's maximise and close under the corner keep their clicks.
    #[test]
    fn banners_start_below_the_window_bars() {
        // Motion reads at its end: this checks where the banner rests.
        let _settled = ui::motion::testing::settled();
        let host = TestHost::default();
        host.push(Notification::new("Keep your name and work?", "A passkey"));
        let mut stack = NotificationStack::new();
        stack.set_viewport(Rect::new(0.0, 32.0, 1440.0, 900.0));
        assert!((stack.first_top() - (32.0 + EDGE_INSET)).abs() < f64::EPSILON);
        // The windows' content starts at 76: their bars run from 32 to 76.
        stack.set_clear_top(76.0);
        draw(&mut stack, &host, &Theme::dark());
        let (_, card, _) = stack
            .hit_rects
            .first()
            .copied()
            .expect("the banner is painted");
        assert!(
            (card.y0 - (76.0 + TOAST_GAP)).abs() < 0.5,
            "{card:?} rests just below the window bars"
        );
        assert!(
            stack.rect().y0 >= 76.0,
            "{:?} covers a window bar",
            stack.rect()
        );
    }

    /// The stack keeps clear of the window bars under its column at the
    /// top, and only of those: a bar beside the column or a window further
    /// down leaves the banners at the corner.
    #[test]
    fn banners_keep_clear_of_the_window_bars_under_the_corner() {
        let _settled = ui::motion::testing::settled();
        let host = TestHost::default();
        host.push(Notification::new("Jonas is calling", "Video call"));
        let viewport = Rect::new(0.0, 32.0, 1440.0, 900.0);
        let corner = 32.0 + EDGE_INSET;
        let mut stack = NotificationStack::new();
        stack.set_viewport(viewport);

        // A window on the left half: its bar is not under the banners.
        stack.set_window_bars(vec![Rect::new(0.0, 36.0, 700.0, 80.0)]);
        assert!((stack.first_top() - corner).abs() < f64::EPSILON);
        // A window lower down, under the first slot: no bar to clear.
        stack.set_window_bars(vec![Rect::new(0.0, 400.0, 1440.0, 444.0)]);
        assert!((stack.first_top() - corner).abs() < f64::EPSILON);

        // A window reaching the right edge, its bar with a toolbar row: the
        // banner starts below it.
        let bar = Rect::new(720.0, 36.0, 1428.0, 120.0);
        stack.set_window_bars(vec![Rect::new(0.0, 36.0, 700.0, 80.0), bar]);
        draw(&mut stack, &host, &Theme::dark());
        let (_, card, _) = stack
            .hit_rects
            .first()
            .copied()
            .expect("the banner is painted");
        assert!(
            (card.y0 - (bar.y1 + TOAST_GAP)).abs() < 0.5,
            "{card:?} rests just below the window bar {bar:?}"
        );
        assert!(
            stack.rect().intersect(bar).area() <= 0.0,
            "{:?} covers the window bar {bar:?}",
            stack.rect()
        );

        // The window goes: the banners go back to the corner.
        stack.set_window_bars(Vec::new());
        assert!((stack.first_top() - corner).abs() < f64::EPSILON);
    }

    #[test]
    fn live_notification_requests_entry_frames_and_a_future_exit_wake() {
        let host = TestHost::default();
        host.push(Notification::new("Saved", "Ready"));
        let mut stack = NotificationStack::new();
        assert_eq!(
            stack.next_wake_deadline(&host),
            None,
            "a banner not yet on screen has no time running"
        );
        stack.set_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
        draw(&mut stack, &host, &Theme::dark());
        assert!(stack.needs_animation_frame(&host));
        assert!(stack.next_wake_deadline(&host).is_some());
    }

    /// Reduced motion is a designed experience, not "off": a banner fades
    /// in where it rests at `fast` (a change of what is shown still reads),
    /// nothing slides, and it fades out so it is gone at expiry.
    #[test]
    fn under_reduced_motion_a_banner_fades_in_place() {
        let host = TestHost::default();
        host.push(Notification::new("Saved", "Ready"));
        let mut stack = NotificationStack::new();
        stack.set_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
        draw(&mut stack, &host, &Theme::dark().with_reduce_motion(true));
        assert!(stack.needs_animation_frame(&host), "it fades in");
        assert!(stack.next_wake_deadline(&host).is_some());
        let n = host.log().active()[0].clone();
        let shown = n.dwell.first_shown().unwrap();
        let (alpha, dx) = NotificationStack::animation_state(
            &n,
            shown + Duration::from_millis(40),
            (true, false),
            272.0,
        );
        assert!(alpha > 0.0 && alpha < 1.0, "mid-fade: {alpha}");
        assert!(dx.abs() < f64::EPSILON, "nothing slides");
        assert_eq!(
            NotificationStack::animation_state(
                &n,
                shown + Duration::from_secs(1),
                (true, false),
                272.0
            ),
            (1.0, 0.0),
            "then it stands"
        );
    }

    /// A banner below one that leaves springs up to its new slot instead of
    /// jumping (`motion::ListChange`), and lands there; while the person
    /// holds the stack, a slot closing upward waits, so nothing moves under
    /// the pointer.
    #[test]
    fn a_banner_springs_to_its_slot_and_waits_while_held() {
        let mut stack = NotificationStack::new();
        let t0 = ui::time::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        assert!(
            (stack.slot_top(7, 100.0, t0, false) - 100.0).abs() < 1e-9,
            "a new banner starts in its slot"
        );
        // The banner above it left: its slot is higher now.
        assert!(
            (stack.slot_top(7, 20.0, t0, false) - 100.0).abs() < 1e-9,
            "no jump"
        );
        let mid = stack.slot_top(7, 20.0, at(60), false);
        assert!(mid < 100.0 && mid > 20.0, "on its way up: {mid}");
        assert!(stack.slots.iter().any(|(_, slot)| slot.is_moving(at(60))));
        let landed = stack.slot_top(7, 20.0, at(600), false);
        assert!((landed - 20.0).abs() < 0.5, "it lands in the freed slot");

        stack.slot_top(8, 200.0, t0, false);
        let held = stack.slot_top(8, 120.0, at(10), true);
        assert!((held - 200.0).abs() < 1e-9, "held: it waits");
        stack.slot_top(8, 120.0, at(20), false);
        let released = stack.slot_top(8, 120.0, at(80), false);
        assert!(released < 200.0, "let go: it closes up");
    }

    /// Banners that do not fit fold into one "N more" card that opens the
    /// centre; they stand still until they come on screen, and only the
    /// banners on screen age.
    #[test]
    fn banners_that_do_not_fit_fold_into_a_more_card_and_do_not_age() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = ui::motion::testing::settled();
        for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
            let theme = theme.with_reduce_motion(true);
            let host = TestHost::default();
            let ids: Vec<u64> = (0..super::super::MAX_NOTIFICATIONS)
                .map(|i| host.push(Notification::new(format!("Banner {i}"), "Body")))
                .collect();
            let viewport = Rect::new(0.0, 0.0, 800.0, 300.0);
            let mut stack = NotificationStack::new();
            stack.set_viewport(viewport);
            draw(&mut stack, &host, &theme);
            let shown = stack.on_screen.clone();
            assert!(!shown.is_empty() && shown.len() < ids.len());
            assert_eq!(shown[0], *ids.last().unwrap(), "the newest leads");
            let more = stack.more_rect;
            assert!(more.area() > 0.0, "the rest fold into one card");
            assert!(more.y1 <= viewport.y1 - EDGE_INSET);
            let hidden = ids.len() - shown.len();
            let label = t_args("notification-more", &[("count", hidden.into())]);
            assert!(
                stack
                    .action_buttons
                    .iter()
                    .any(|(rect, name, _)| *rect == more && *name == label),
                "the card names how many more: {label}"
            );
            let log = host.log();
            for n in log.active() {
                assert_eq!(
                    n.dwell.is_running(),
                    shown.contains(&n.id),
                    "only a banner on screen ages"
                );
            }
            drop(log);
            assert!(stack.click_at(&host, more.center()));
            assert_eq!(host.centre_opened.get(), 1, "the card opens the centre");
        }
    }

    /// Resting the pointer on the stack holds every banner on it: none
    /// fades or expires under the pointer, and letting go starts them
    /// again.
    #[test]
    fn the_pointer_resting_on_the_stack_holds_the_banners() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = ui::motion::testing::settled();
        let host = TestHost::default();
        host.push(Notification::new("Ada", "Can you look?"));
        let mut stack = NotificationStack::new();
        stack.set_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
        draw(&mut stack, &host, &Theme::dark().with_reduce_motion(true));
        let device_id = winit::event::DeviceId::dummy();
        let at = |point: Point| WindowEvent::CursorMoved {
            device_id,
            position: winit::dpi::PhysicalPosition::new(point.x, point.y),
        };
        let mut handled = false;
        assert!(stack.handle_pointer_event(&host, &at(stack.rect().center()), 1.0, &mut handled));
        assert!(stack.held());
        assert!(host.log().held());
        assert!(!host.log().active()[0].dwell.is_running());
        assert_eq!(stack.next_wake_deadline(&host), None);
        let n = host.log().active()[0].clone();
        assert_eq!(
            NotificationStack::animation_state(
                &n,
                ui::time::now() + Duration::from_secs(60),
                (false, true),
                272.0
            )
            .0,
            1.0,
            "a held banner does not fade"
        );
        assert!(stack.handle_pointer_event(&host, &at(Point::new(5.0, 590.0)), 1.0, &mut handled));
        assert!(!stack.held());
        assert!(host.log().active()[0].dwell.is_running());
    }

    /// Passing over the stack on the way elsewhere is transit and holds
    /// nothing; heading for it and slowing to a stop there holds it, and a
    /// hand that stops short lets go after the aim lapses.
    #[test]
    fn only_a_pointer_reaching_for_the_stack_holds_it() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = ui::motion::testing::settled();
        let host = TestHost::default();
        host.push(Notification::new("Ada", "Can you look?"));
        let mut stack = NotificationStack::new();
        stack.set_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
        draw(&mut stack, &host, &Theme::dark().with_reduce_motion(true));
        let card = stack.rect();
        let y = card.center().y;
        let t0 = ui::time::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);

        // A fast sweep down the screen across the stack, 40 px every 8 ms.
        let x = card.center().x;
        let mut ms = 0;
        for step in 0..16_u32 {
            let py = card.y0 - 120.0 + 40.0 * f64::from(step);
            stack.note_pointer_at(&host, Some(Point::new(x, py)), at(ms));
            if card.contains(Point::new(x, py)) {
                assert!(!stack.pointer_holds(at(ms)), "in transit at {py}");
            }
            ms += 8;
        }
        assert!(!host.log().held());
        stack.note_pointer_at(&host, None, at(ms));

        // Reaching for the stack from the left: slowing to a stop on it.
        let t1 = ms + 500;
        let path = [
            (card.x0 - 200.0, 0),
            (card.x0 - 100.0, 16),
            (card.x0 - 40.0, 32),
            (card.x0 - 10.0, 48),
            (card.x0 + 2.0, 64),
        ];
        for (px, dt) in path {
            stack.note_pointer_at(&host, Some(Point::new(px, y)), at(t1 + dt));
        }
        assert!(
            stack.pointer_holds(at(t1 + 64)),
            "heading there and coming to rest"
        );
        assert!(host.log().held());
        assert!(
            stack.pointer_holds(at(t1 + 64 + 200)),
            "resting on the stack keeps holding it"
        );

        // Heading for it but stopping short: the aim lapses.
        stack.note_pointer_at(&host, None, at(t1 + 1000));
        let t2 = t1 + 2000;
        for (px, dt) in [
            (card.x0 - 300.0, 0),
            (card.x0 - 150.0, 16),
            (card.x0 - 60.0, 32),
        ] {
            stack.note_pointer_at(&host, Some(Point::new(px, y)), at(t2 + dt));
        }
        let lapse = at(t2 + 32) + AIM_HOLD;
        assert!(
            !stack.pointer_holds(lapse),
            "a hand that stops short lets go"
        );
    }

    /// The Notifications chord moves focus into the newest banner: arrows
    /// walk its controls and the banners, Return runs the focused control,
    /// Escape dismisses the banner; either gives focus back.
    #[test]
    fn keyboard_focus_reaches_banner_actions_and_escape_dismisses() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = ui::motion::testing::settled();
        let host = TestHost::default();
        host.actions.replace(vec![
            ("dismiss".into(), "Dismiss".into()),
            ("snooze".into(), "Snooze".into()),
        ]);
        let older = host.push(Notification::new("Alarm", "Tea"));
        let newest = host.push(Notification::new("Alarm", "Meeting"));
        let mut stack = NotificationStack::new();
        stack.set_viewport(Rect::new(0.0, 0.0, 800.0, 900.0));
        let theme = Theme::dark().with_reduce_motion(true);
        draw(&mut stack, &host, &theme);
        assert!(!stack.owns_key(&Key::Named(NamedKey::Enter)));
        assert!(stack.focus_newest(&host));
        assert!(host.log().held(), "a focused banner does not age");
        assert_eq!(
            stack.focus,
            Some(BannerTarget::Action(newest, "dismiss".into())),
            "focus lands on the first choice, as painted"
        );
        assert!(!stack.owns_key(&Key::Character("a".into())));
        assert!(stack.handle_key(&host, &Key::Named(NamedKey::ArrowRight), false));
        assert_eq!(
            stack.focus,
            Some(BannerTarget::Action(newest, "snooze".into()))
        );
        assert!(stack.handle_key(&host, &Key::Named(NamedKey::ArrowDown), false));
        assert_eq!(stack.focus, Some(BannerTarget::Open(older)));
        assert!(stack.handle_key(&host, &Key::Named(NamedKey::ArrowUp), false));
        assert_eq!(stack.focus, Some(BannerTarget::Open(newest)));
        assert!(stack.handle_key(&host, &Key::Named(NamedKey::Tab), false));
        assert!(stack.handle_key(&host, &Key::Named(NamedKey::Tab), false));
        assert!(stack.handle_key(&host, &Key::Named(NamedKey::Enter), false));
        assert_eq!(
            host.invoked.borrow().last(),
            Some(&(newest, "snooze".to_owned()))
        );
        assert!(!stack.has_keyboard_focus(), "acting gives focus back");
        assert!(!host.log().held());

        draw(&mut stack, &host, &theme);
        assert!(stack.focus_newest(&host));
        assert!(stack.handle_key(&host, &Key::Named(NamedKey::Escape), false));
        assert!(!stack.has_keyboard_focus());
        assert!(
            host.log().active().iter().all(|n| n.id != newest),
            "Escape dismisses the focused banner"
        );
    }

    /// Do Not Disturb puts no card on screen: with no banner, the stack
    /// paints nothing.
    #[test]
    fn quiet_mode_paints_no_card_of_its_own() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = ui::motion::testing::settled();
        let host = TestHost::default();
        host.quiet.replace(Some("Do Not Disturb".into()));
        host.log.borrow_mut().retain(Notification::new("Held", ""));
        let mut stack = NotificationStack::new();
        stack.set_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
        draw(&mut stack, &host, &Theme::dark());
        assert!(stack.rect().is_zero_area());
        assert!(stack.accessibility(&host).unwrap().children.is_empty());
    }

    #[test]
    fn accessibility_exposes_a_named_live_region_with_latest_notification() {
        use ui::a11y::{Politeness, Role};
        // Motion reads at its end: this test checks the resting frame.
        let _settled = ui::motion::testing::settled();
        let host = TestHost::default();
        let stack = NotificationStack::new();
        let node = stack
            .accessibility(&host)
            .expect("the stack is a live region");
        assert_eq!(node.role, Role::Group);
        assert_eq!(node.name.as_deref(), Some("Notifications"));
        assert_eq!(node.live, Some(Politeness::Polite));
        assert_eq!(
            node.value.as_deref(),
            Some(""),
            "an empty stack announces nothing"
        );
        host.push(Notification::new("New mail", "secret subject"));
        let node = stack.accessibility(&host).unwrap();
        assert_eq!(node.value.as_deref(), Some("New mail. secret subject"));
        assert_eq!(
            node.live,
            Some(Politeness::Polite),
            "an ordinary arrival waits its turn"
        );
        host.push(
            Notification::new("Backup failed", "").with_severity(NotificationSeverity::Error),
        );
        let node = stack.accessibility(&host).unwrap();
        assert_eq!(node.value.as_deref(), Some("Backup failed"));
        assert_eq!(node.live, Some(Politeness::Assertive), "an error cuts in");
        // Locked: no title/body/app reaches the a11y tree.
        host.locked.set(true);
        let node = stack.accessibility(&host).unwrap();
        assert_eq!(node.value.as_deref(), Some("2 notifications"));
        assert_eq!(node.live, Some(Politeness::Polite));
    }
}
