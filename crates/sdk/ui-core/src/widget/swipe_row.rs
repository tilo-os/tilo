//! [`SwipeRow`]: a row that follows the finger sideways and uncovers what swiping it does.
//!
//! A horizontal drag on a touch screen moves the row with the finger (a vertical one is the
//! list's scroll and leaves it alone). Dragging towards the trailing edge uncovers the leading
//! action, towards the leading edge the trailing one; a side with no action does not move.
//! Let go past [`COMMIT_SHARE`] of the row's width, or flung so it would come to rest past it, and the action runs;
//! either way the row settles back on the platform's spring, carrying the finger's velocity
//! ([`crate::motion::Snap::SPEC`]). A touch during the settle catches the row where it is
//! drawn: the motion is interruptible. Crossing the threshold is fed back on the uncovered
//! action itself: its glyph grows and its label shows, so the person knows letting go now
//! runs it. Reduced motion lands the settle at once.
//!
//! The row is moved by transform, never laid out again; while it moves only its own box is
//! damaged.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::Fill;
use winit::event::{TouchPhase, WindowEvent};

use crate::icons::Icon;
use crate::motion::{Direction, MotionValue, Snap};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::Instant;
use crate::tokens::{MotionSource, TextRole};
use crate::widget::{Decorator, Element, EventCtx, SwipeActionProps, Widget};

/// How far a released row must have moved, as a share of its width, for its action to run.
pub const COMMIT_SHARE: f64 = 0.35;
/// How far a finger moves before the row decides whether the drag is sideways: the platform's
/// tap slop, past which a touch is no tap.
const SLOP: f64 = crate::interaction::TouchPointerEmulator::TAP_SLOP;

/// One side's action.
struct Side {
    icon: Icon,
    label: String,
    destructive: bool,
    run: Option<Box<dyn FnMut()>>,
}

impl Side {
    fn of(props: SwipeActionProps) -> Self {
        let SwipeActionProps {
            label,
            icon,
            destructive,
            swipe,
        } = props;
        Self {
            icon: icon.unwrap_or(if destructive {
                Icon::Trash
            } else {
                Icon::Check
            }),
            label,
            destructive,
            run: swipe.handler(),
        }
    }
}

/// The contact id of a drag that reaches the row as the pointer.
const POINTER: u64 = u64::MAX;

/// Whether the contact `id` moves `drag`: its own finger, or the pointer the platform makes of
/// a finger (a touchscreen's drag reaches widgets as the pointer once it heads across).
fn follows(drag: &Drag, id: u64) -> bool {
    drag.id == id || id == POINTER
}

/// A finger on the row.
struct Drag {
    id: u64,
    start: Point,
    /// Where the row was when the finger came down: a settle caught mid-way.
    from: f64,
    /// Whether the drag is sideways: unknown until it moved past [`SLOP`].
    sideways: Option<bool>,
    /// The last two samples of the finger, for its velocity at release.
    samples: [(f64, Instant); 2],
}

/// A row that swipes sideways to run an action. See the [module docs](self).
#[crate::component::not_a_component(part, "a row's swipe, from `ui.Swipe` and its `swipe` slot")]
pub struct SwipeRow {
    content: Element,
    /// Uncovered by dragging towards the trailing edge.
    leading: Option<Side>,
    /// Uncovered by dragging towards the leading edge.
    trailing: Option<Side>,
    /// How far the row is moved: positive towards the trailing edge.
    offset: MotionValue<f64>,
    drag: Option<Drag>,
    /// The offset last drawn, to damage while it changes.
    drawn: f64,
    /// How many actions it ran, for tests.
    ran: usize,
    /// Whether the person uses a touchscreen now: a finger's sideways drag then reaches the row
    /// as a left-button drag ([`crate::interaction::TouchClaim::DragAcross`]); a mouse's drag
    /// selects text instead.
    touch: bool,
    /// Where the finger was last, for a drag that reaches the row as the pointer.
    pointer: Point,
}

impl SwipeRow {
    /// `content`, swiping to `leading` and `trailing` (either may be absent).
    #[must_use]
    pub fn new(
        content: Element,
        leading: Option<SwipeActionProps>,
        trailing: Option<SwipeActionProps>,
    ) -> Self {
        Self {
            content,
            leading: leading.map(Side::of),
            trailing: trailing.map(Side::of),
            offset: MotionValue::new(0.0),
            drag: None,
            drawn: 0.0,
            ran: 0,
            touch: false,
            pointer: Point::ZERO,
        }
    }

    /// How far the row is moved now: positive towards the trailing edge.
    #[must_use]
    pub fn offset(&self) -> f64 {
        self.offset.get(crate::motion::frame_now())
    }

    /// How many times a swipe ran its action.
    #[must_use]
    pub fn ran(&self) -> usize {
        self.ran
    }

    /// The row under the swipe.
    #[must_use]
    pub fn content(&self) -> &dyn Widget {
        self.content.as_ref()
    }

    fn width(&self) -> f64 {
        self.content.get_rect().width().max(1.0)
    }

    /// `offset` kept to the sides that have an action, and to the row's width.
    fn bounded(&self, offset: f64) -> f64 {
        let width = self.width();
        let most = if self.leading.is_some() { width } else { 0.0 };
        let least = if self.trailing.is_some() { -width } else { 0.0 };
        offset.clamp(least, most)
    }

    /// The side `offset` uncovers.
    fn side(&self, offset: f64) -> Option<&Side> {
        if offset > 0.0 {
            self.leading.as_ref()
        } else if offset < 0.0 {
            self.trailing.as_ref()
        } else {
            None
        }
    }

    /// Whether letting go at `offset` runs the action.
    fn past_threshold(&self, offset: f64) -> bool {
        offset.abs() >= self.width() * COMMIT_SHARE
    }

    fn settle(&mut self, velocity: f64, theme_motion: crate::tokens::EffectiveMotion) {
        let now = crate::motion::frame_now();
        let timing = Snap::SPEC.timing(Direction::Exit, theme_motion, MotionSource::Gesture);
        self.offset.release(0.0, velocity, timing, now);
    }

    fn release(&mut self, drag: &Drag, cancelled: bool) {
        let now = crate::motion::frame_now();
        let offset = self.offset.get(now);
        let [(x0, t0), (x1, t1)] = drag.samples;
        let dt = t1.saturating_duration_since(t0).as_secs_f64();
        let velocity = if dt > 0.0 { (x1 - x0) / dt } else { 0.0 };
        // A flick is judged where the release would come to rest, as every thrown surface is.
        let motion = crate::tokens::EffectiveMotion::default();
        let projected = Snap::rest_for(offset, velocity, motion);
        let flung = projected.signum() == offset.signum() && self.past_threshold(projected);
        if !cancelled && offset != 0.0 && (self.past_threshold(offset) || flung) {
            let side = if offset > 0.0 {
                self.leading.as_mut()
            } else {
                self.trailing.as_mut()
            };
            if let Some(run) = side.and_then(|side| side.run.as_mut()) {
                run();
                self.ran = self.ran.saturating_add(1);
            }
        }
        // The motion is read with the theme's at the next draw; until then, the default.
        self.settle(velocity, crate::tokens::EffectiveMotion::default());
    }

    /// A finger's drag that reaches the row as the pointer: the left button goes down once the
    /// finger heads across, so the drag is sideways from the start.
    fn pointer(&mut self, event: &WindowEvent, ctx: &EventCtx<'_>) -> bool {
        use winit::event::{ElementState, MouseButton};
        let phase = match event {
            // The platform parks the pointer off the window once a finger's contact ends: that
            // is no move of the finger.
            WindowEvent::CursorMoved { position, .. } if position.x < 0.0 || position.y < 0.0 => {
                return false;
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = ctx.logical(position);
                TouchPhase::Moved
            }
            // The finger already on the row headed across: the platform presses where it
            // landed once it is sure.
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.drag.is_some() => {
                if let Some(drag) = self.drag.as_mut() {
                    drag.sideways = Some(true);
                }
                return true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.touch => TouchPhase::Started,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => TouchPhase::Ended,
            _ => return false,
        };
        let started = phase == TouchPhase::Started;
        let handled = self.contact(POINTER, phase, self.pointer);
        if started && let Some(drag) = self.drag.as_mut() {
            drag.sideways = Some(true);
        }
        handled
    }

    fn touch(&mut self, event: &winit::event::Touch, ctx: &EventCtx<'_>) -> bool {
        let at = ctx.logical(event.location);
        self.contact(event.id, event.phase, at)
    }

    /// One contact's update: the finger `id` at `at`.
    fn contact(&mut self, id: u64, phase: TouchPhase, at: Point) -> bool {
        let now = crate::motion::frame_now();
        match phase {
            TouchPhase::Started if self.drag.is_none() && self.content.get_rect().contains(at) => {
                let from = self.offset.get(now);
                // A settle caught mid-way stays where it is drawn.
                self.offset.snap(from);
                self.drag = Some(Drag {
                    id,
                    start: at,
                    from,
                    sideways: None,
                    samples: [(at.x, now), (at.x, now)],
                });
                false
            }
            TouchPhase::Moved => {
                let Some(drag) = self.drag.as_mut().filter(|drag| follows(drag, id)) else {
                    return false;
                };
                let moved = at - drag.start;
                if drag.sideways.is_none() && moved.hypot() >= SLOP {
                    drag.sideways = Some(moved.x.abs() > moved.y.abs());
                }
                drag.samples = [drag.samples[1], (at.x, now)];
                match drag.sideways {
                    Some(true) => {
                        let want = drag.from + moved.x;
                        let offset = self.bounded(want);
                        self.offset.snap(offset);
                        true
                    }
                    Some(false) => {
                        // A scroll: the list's, not the row's.
                        let drag = self.drag.take();
                        if let Some(drag) = drag {
                            self.release(&drag, true);
                        }
                        false
                    }
                    None => false,
                }
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                let Some(mut drag) = self.drag.take_if(|drag| follows(drag, id)) else {
                    return false;
                };
                // Where the finger lifted, and when: a finger held still before it lifts
                // throws nothing.
                drag.samples = [drag.samples[1], (at.x, now)];
                let sideways = drag.sideways == Some(true);
                self.release(&drag, phase == TouchPhase::Cancelled);
                sideways
            }
            TouchPhase::Started => false,
        }
    }

    /// Draw what the row uncovers at `offset`: the action's ground, its glyph, and its label
    /// once letting go would run it.
    fn paint_uncovered(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        offset: f64,
    ) {
        let Some(side) = self.side(offset) else {
            return;
        };
        let row = self.content.get_rect();
        let uncovered = if offset > 0.0 {
            Rect::new(row.x0, row.y0, row.x0 + offset, row.y1)
        } else {
            Rect::new(row.x1 + offset, row.y0, row.x1, row.y1)
        };
        let armed = self.past_threshold(offset);
        let (fill, ink) = uncovered_colors(theme, side.destructive, armed);
        scene.fill(Fill::NonZero, transform, fill, None, &uncovered);
        let inset = f64::from(theme.tokens.spacing.s4);
        // Armed, the glyph grows: what letting go now does is told at a glance.
        let icon = if armed {
            f64::from(theme.tokens.metrics.icon_md)
        } else {
            f64::from(theme.tokens.metrics.icon_sm)
        };
        if uncovered.width() < icon + 2.0 * inset {
            return;
        }
        let x = if offset > 0.0 {
            uncovered.x0 + inset
        } else {
            uncovered.x1 - inset - icon
        };
        let glyph = Rect::new(
            x,
            uncovered.center().y - icon / 2.0,
            x + icon,
            uncovered.center().y + icon / 2.0,
        );
        side.icon.draw(scene, transform, glyph, ink);
        if !armed {
            return;
        }
        let style = theme.text(TextRole::Label);
        let (w, h) = text.measure_style(&side.label, None, &style);
        let gap = f64::from(theme.tokens.spacing.s2);
        let room = uncovered.width() - icon - 2.0 * inset - gap;
        if room < f64::from(w) {
            return;
        }
        let label_x = if offset > 0.0 {
            glyph.x1 + gap
        } else {
            glyph.x0 - gap - f64::from(w)
        };
        text.draw_style(
            scene,
            &side.label,
            ink,
            transform.pre_translate(Vec2::new(
                label_x,
                uncovered.center().y - f64::from(h) / 2.0,
            )),
            None,
            &style,
        );
    }
}

impl Decorator for SwipeRow {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn name(&self) -> &'static str {
        "SwipeRow"
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let WindowEvent::Touch(touch) = event
            && self.touch(touch, ctx)
        {
            *ctx.handled = true;
            return;
        }
        if self.pointer(event, ctx) {
            *ctx.handled = true;
            return;
        }
        // While it is dragged sideways the row's content sees none of the finger.
        if self
            .drag
            .as_ref()
            .is_some_and(|drag| drag.sideways == Some(true))
        {
            return;
        }
        self.content.handle_event(event, ctx);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let now = crate::motion::frame_now();
        // Reduced motion lands a settle at once.
        if theme.motion().reduced() && self.drag.is_none() && self.offset.is_moving(now) {
            self.offset.snap(0.0);
        }
        let offset = self.offset.get(now);
        self.drawn = offset;
        if offset == 0.0 {
            self.content.draw(scene, transform, text, theme, presence);
            return;
        }
        let row = self.content.get_rect();
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &row,
        );
        self.paint_uncovered(scene, transform, text, theme, offset);
        let moved = transform.pre_translate(Vec2::new(offset, 0.0));
        // The moving row stays opaque over what it uncovers.
        scene.fill(
            Fill::NonZero,
            moved,
            theme.tokens.colors.surfaces.content,
            None,
            &row,
        );
        self.content.draw(scene, moved, text, theme, presence);
        scene.pop_layer();
    }

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        self.touch = environment.modality == crate::interaction::InputModality::Touch;
        self.content.set_interaction_environment(environment);
    }

    /// A finger heading across swipes the row; heading up or down it scrolls the list.
    fn touch_claim(&self, point: Point) -> crate::interaction::TouchClaim {
        let has_action = self.leading.is_some() || self.trailing.is_some();
        if has_action && self.content.get_rect().contains(point) {
            crate::interaction::TouchClaim::DragAcross
        } else {
            self.content.touch_claim(point)
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.drag.is_some() || self.content.wants_pointer_capture()
    }

    fn needs_animation_frame(&self) -> bool {
        self.offset.is_moving(crate::motion::frame_now()) || self.content.needs_animation_frame()
    }

    fn needs_redraw(&self) -> bool {
        let offset = self.offset.get(crate::motion::frame_now());
        (offset - self.drawn).abs() > f64::EPSILON || self.content.needs_redraw()
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        let offset = self.offset.get(crate::motion::frame_now());
        if (offset - self.drawn).abs() > f64::EPSILON {
            regions.push(self.content.get_rect());
        } else {
            self.content.collect_damage(regions);
        }
    }
}

/// The fill a swipe uncovers and the ink of its glyph and label: quiet until armed, then the
/// accent (the danger colour for a destructive command), with the ink made for a filled control
/// stepped out until the label reads at body-text contrast.
pub(crate) fn uncovered_colors(
    theme: &Theme,
    destructive: bool,
    armed: bool,
) -> (vello::peniko::Color, vello::peniko::Color) {
    let colors = &theme.tokens.colors;
    if !destructive && !armed {
        return (
            colors.lift_on(colors.surfaces.content, 1.0),
            theme.text_primary,
        );
    }
    let fill = if destructive {
        colors.semantic.danger
    } else {
        colors.intent.accent
    };
    let ink = colors.ink_on(
        fill,
        colors.intent.on_accent,
        crate::tokens::contrast::BODY_TEXT_MIN_CONTRAST,
    );
    (fill, ink)
}

#[cfg(test)]
#[path = "swipe_row_tests.rs"]
mod tests;
