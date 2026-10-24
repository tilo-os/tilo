//! A sheet up from the foot of a narrow window: where an inspector stands
//! when the window has no room for it beside the content.
//!
//! The sheet rests at one of three detents: peeking (its grip and the top
//! of what it holds), half the window, or the whole window under its header
//! row. The content above it stays live: it is not modal, a press above it
//! goes to the content, and the content keeps the part above the sheet's
//! detent clear to scroll into. The grip moves the sheet: a drag (pointer
//! or finger) follows the hand and the release settles on the detent the
//! fling points to ([`motion::Snap`]); a press on the grip without a drag
//! raises it a detent; the arrow keys on the focused grip step it. To
//! assistive technology the sheet is a named region and its grip a slider
//! whose value is the detent.
//!
//! The sheet is laid out at its detent while it rests (peeking, at half
//! height, the rest of it below the window's foot). While it moves it is
//! laid out at the whole window and clipped at the window's foot, so what
//! it holds reflows once when it settles, never on every frame.

use crate::time::Instant;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, KeyEvent, MouseButton, TouchPhase, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::a11y::{AccessNode, AccessState, Action, Role};
use crate::motion::{self, Direction, MotionValue, Snap};
use crate::theme::Theme;
use crate::tokens::{EffectiveMotion, MotionSource};

/// Where a sheet rests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SheetDetent {
    /// Its grip and the top of what it holds.
    Peek,
    /// Half the window.
    #[default]
    Half,
    /// The whole window under its header row.
    Full,
}

impl SheetDetent {
    /// Lowest first.
    pub const ALL: [Self; 3] = [Self::Peek, Self::Half, Self::Full];

    fn index(self) -> f64 {
        match self {
            Self::Peek => 0.0,
            Self::Half => 1.0,
            Self::Full => 2.0,
        }
    }

    /// The detent a step up (`up`) or down from this one lands on.
    #[must_use]
    pub fn step(self, up: bool) -> Option<Self> {
        match (self, up) {
            (Self::Peek, true) | (Self::Full, false) => Some(Self::Half),
            (Self::Half, true) => Some(Self::Full),
            (Self::Half, false) => Some(Self::Peek),
            (Self::Peek, false) | (Self::Full, true) => None,
        }
    }

    /// The detent as assistive technology reads it.
    #[must_use]
    pub fn label(self) -> String {
        crate::i18n::widget_format(
            match self {
                Self::Peek => "widget-sheet-peek",
                Self::Half => "widget-sheet-half",
                Self::Full => "widget-sheet-full",
            },
            &[],
        )
    }
}

/// Where a sheet stands this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SheetGeometry {
    /// The sheet as drawn, its grip included, down to the window's foot.
    pub panel: Rect,
    /// The band along its top edge a drag takes it by.
    pub grip: Rect,
    /// What the sheet holds, under the grip: as tall as it is laid out, so
    /// it may run past the window's foot while the sheet moves.
    pub content: Rect,
    /// How much of the window's foot the sheet covers where it rests: the
    /// content above keeps that part clear. The whole window at full
    /// height hides the content anyway, so it never covers more than half.
    pub covered: f64,
}

/// What the sheet stands in: `area` reaches from the highest the sheet may
/// rise to the window's foot; `peek` is how much of what it holds shows
/// while it peeks; `grip` is the grip's height; `least_half` is the least
/// a half-height sheet must hold (the panel's width where it docks): in a
/// window too short for that, half height is the whole height, so what it
/// holds keeps the room it needs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SheetFrame {
    pub area: Rect,
    pub peek: f64,
    pub grip: f64,
    pub least_half: f64,
}

impl SheetFrame {
    /// The sheet's height at each detent, lowest first.
    fn heights(self) -> [f64; 3] {
        let full = self.area.height().max(0.0);
        let peek = (self.grip + self.peek).min(full);
        let half = if full / 2.0 >= self.least_half {
            (full / 2.0).clamp(peek, full)
        } else {
            full
        };
        [peek, half, full]
    }

    /// The height at `pos` in detent units (0 peek, 1 half, 2 full).
    fn height_at(self, pos: f64) -> f64 {
        let [peek, half, full] = self.heights();
        if pos <= 1.0 {
            peek + (half - peek) * pos.max(0.0)
        } else {
            half + (full - half) * (pos - 1.0).min(1.0)
        }
    }

    /// The position in detent units of a sheet `height` tall.
    fn pos_of(self, height: f64) -> f64 {
        let [peek, half, full] = self.heights();
        if height <= half {
            if half - peek > f64::EPSILON {
                ((height - peek) / (half - peek)).max(0.0)
            } else {
                1.0
            }
        } else if full - half > f64::EPSILON {
            1.0 + ((height - half) / (full - half)).min(1.0)
        } else {
            2.0
        }
    }

    /// How many pixels one detent unit is around `pos`.
    fn unit_at(self, pos: f64) -> f64 {
        let [peek, half, full] = self.heights();
        if pos < 1.0 { half - peek } else { full - half }
    }
}

/// A grip held by the pointer or a finger.
#[derive(Clone, Debug)]
struct Drag {
    from_y: f64,
    from_height: f64,
    /// How far up the hand has moved, report by report: the release's
    /// speed is read from it.
    path: super::finger_path::FingerPath,
    touch: Option<u64>,
    moved: bool,
}

/// What [`BottomSheet::handle_event`] did with an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetInput {
    /// Not the sheet's: route it on.
    Ignored,
    /// The sheet took it; it moved when `moved` (lay out again).
    Taken { moved: bool },
}

impl SheetInput {
    /// Whether the sheet took the event.
    #[must_use]
    pub fn taken(self) -> bool {
        matches!(self, Self::Taken { .. })
    }
}

/// A sheet up from a narrow window's foot. See the module docs.
#[derive(Clone, Debug)]
pub struct BottomSheet {
    detent: SheetDetent,
    /// Where it is going and where it is drawn, in detent units.
    pos: MotionValue<f64>,
    drawn: f64,
    drag: Option<Drag>,
    focused: bool,
    pointer: Point,
    frame: SheetFrame,
    geometry: SheetGeometry,
}

impl Default for BottomSheet {
    fn default() -> Self {
        Self::new()
    }
}

impl BottomSheet {
    /// A sheet that opens at half the window.
    #[must_use]
    pub fn new() -> Self {
        let detent = SheetDetent::default();
        Self {
            detent,
            pos: MotionValue::new(detent.index()),
            drawn: detent.index(),
            drag: None,
            focused: false,
            pointer: Point::ORIGIN,
            frame: SheetFrame::default(),
            geometry: SheetGeometry::default(),
        }
    }

    /// The detent it rests at, or is going to.
    #[must_use]
    pub fn detent(&self) -> SheetDetent {
        self.detent
    }

    /// Send the sheet to `detent` on the snap motion, as `source` asked.
    pub fn set_detent(
        &mut self,
        detent: SheetDetent,
        source: MotionSource,
        motion: EffectiveMotion,
    ) {
        self.detent = detent;
        let now = motion::frame_now();
        let to = detent.index();
        let timing = Snap::SPEC.timing(Direction::of(to >= self.drawn), motion, source);
        self.pos.set(to, timing, now);
        self.drawn = self.pos.get(now);
    }

    /// Rest at `detent` at once (a capture, a test).
    pub fn snap_to(&mut self, detent: SheetDetent) {
        self.detent = detent;
        self.pos.snap(detent.index());
        self.drawn = detent.index();
    }

    /// Step a detent up or down; whether there was one to step to.
    pub fn step(&mut self, up: bool, source: MotionSource, motion: EffectiveMotion) -> bool {
        let Some(next) = self.detent.step(up) else {
            return false;
        };
        self.set_detent(next, source, motion);
        true
    }

    /// Back's step on the sheet: from the whole window it comes down to
    /// half, the content above it in view again; from half or peeking there
    /// is nothing lower to rest at, and the holder puts it away. Returns
    /// whether it came down.
    pub fn lower(&mut self, motion: EffectiveMotion) -> bool {
        if self.detent != SheetDetent::Full {
            return false;
        }
        self.set_detent(SheetDetent::Half, MotionSource::Keyboard, motion);
        true
    }

    /// Advance the settle to `now`. Returns whether it is still moving.
    pub fn advance(&mut self, now: Instant) -> bool {
        if self.drag.is_some() {
            return false;
        }
        let moving = self.pos.is_moving(now);
        self.drawn = if moving {
            self.pos.get(now)
        } else {
            self.pos.target()
        };
        moving
    }

    /// Whether the sheet is on its way to a detent or held by a drag.
    #[must_use]
    pub fn is_moving(&self) -> bool {
        self.drag.is_some() || self.pos.is_moving(motion::frame_now())
    }

    /// Whether a drag holds the grip.
    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The pose drawn this frame, to tell whether a tick changed it.
    #[must_use]
    pub fn drawn(&self) -> f64 {
        self.drawn
    }

    /// Where the sheet stands in `frame` with its open/close slide at
    /// `reveal` (`0..=1`); `open` is whether it is open or opening.
    #[must_use]
    pub fn geometry(&self, frame: SheetFrame, reveal: f64, open: bool) -> SheetGeometry {
        let area = frame.area;
        if area.width() <= 0.0 || area.height() <= 0.0 {
            return SheetGeometry::default();
        }
        // The content keeps the foot clear from the moment the sheet opens,
        // not as it rises: it reflows once, not every frame of the slide.
        let covered = if open {
            frame
                .height_at(self.detent.index().min(SheetDetent::Half.index()))
                .min(area.height() / 2.0)
        } else {
            0.0
        };
        let height = frame.height_at(self.drawn) * reveal.clamp(0.0, 1.0);
        if height <= 0.0 {
            return SheetGeometry {
                covered,
                ..SheetGeometry::default()
            };
        }
        let top = area.y1 - height;
        let panel = Rect::new(area.x0, top, area.x1, area.y1);
        let grip_h = frame.grip.min(height);
        let grip = Rect::new(area.x0, top, area.x1, top + grip_h);
        // Peeking, it shows the top of what it holds laid out as at half
        // height; the rest stands below the window's foot until it rises.
        let laid_out = if self.is_moving() {
            frame.height_at(SheetDetent::Full.index())
        } else {
            frame.height_at(self.detent.index().max(SheetDetent::Half.index()))
        };
        let content = Rect::new(area.x0, top + grip_h, area.x1, top + laid_out.max(grip_h));
        SheetGeometry {
            panel,
            grip,
            content,
            covered,
        }
    }

    /// Stand in `frame` this frame (see [`Self::geometry`]); events and
    /// painting use where it was placed last.
    pub fn place(&mut self, frame: SheetFrame, reveal: f64, open: bool) -> SheetGeometry {
        self.frame = frame;
        self.geometry = self.geometry(frame, reveal, open);
        self.geometry
    }

    /// Where it was placed last.
    #[must_use]
    pub fn placed(&self) -> SheetGeometry {
        self.geometry
    }

    /// Whether the grip holds the keyboard.
    #[must_use]
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Whether `point` is on the sheet: what lands there is the sheet's,
    /// never the content's under it.
    #[must_use]
    pub fn contains(&self, point: Point) -> bool {
        self.geometry.panel.height() > 0.0 && self.geometry.panel.contains(point)
    }

    /// Whether a finger landing at `point` takes the grip (it drags the
    /// sheet; it never scrolls).
    #[must_use]
    pub fn claims_touch(&self, point: Point) -> bool {
        self.geometry.grip.height() > 0.0 && self.geometry.grip.contains(point)
    }

    /// The cursor over the grip.
    #[must_use]
    pub fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        (self.drag.is_some() || self.claims_touch(point)).then_some(if self.drag.is_some() {
            winit::window::CursorIcon::Grabbing
        } else {
            winit::window::CursorIcon::Grab
        })
    }

    /// The sheet's own events: a drag on its grip, a press on it, the
    /// arrow keys while it holds the keyboard. Place it first.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        ctx: &crate::widget::EventCtx<'_>,
        motion: EffectiveMotion,
    ) -> SheetInput {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = ctx.logical(position);
                if self.held_by_pointer() {
                    self.drag_to(self.pointer.y)
                } else {
                    SheetInput::Ignored
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.drag.is_none() && self.claims_touch(self.pointer) {
                    self.take(self.pointer.y, None);
                    return SheetInput::Taken { moved: false };
                }
                if self.focused {
                    // A press anywhere else hands the keyboard on.
                    self.focused = false;
                }
                SheetInput::Ignored
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.held_by_pointer() => self.let_go(motion),
            WindowEvent::Touch(touch) => {
                let p = ctx.logical(touch.location);
                match touch.phase {
                    TouchPhase::Started if self.drag.is_none() && self.claims_touch(p) => {
                        self.take(p.y, Some(touch.id));
                        SheetInput::Taken { moved: false }
                    }
                    TouchPhase::Moved if self.held_by(touch.id) => self.drag_to(p.y),
                    TouchPhase::Ended | TouchPhase::Cancelled if self.held_by(touch.id) => {
                        self.let_go(motion)
                    }
                    _ => SheetInput::Ignored,
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.focused => {
                let up = match logical_key {
                    Key::Named(NamedKey::ArrowUp) => true,
                    Key::Named(NamedKey::ArrowDown) => false,
                    _ => return SheetInput::Ignored,
                };
                let moved = self.step(up, MotionSource::Keyboard, motion);
                SheetInput::Taken { moved }
            }
            _ => SheetInput::Ignored,
        }
    }

    fn held_by_pointer(&self) -> bool {
        self.drag.as_ref().is_some_and(|drag| drag.touch.is_none())
    }

    fn held_by(&self, touch: u64) -> bool {
        self.drag
            .as_ref()
            .is_some_and(|drag| drag.touch == Some(touch))
    }

    fn take(&mut self, y: f64, touch: Option<u64>) {
        let from_height = self.frame.height_at(self.drawn);
        self.pos.snap(self.drawn);
        self.focused = touch.is_none();
        let mut path = super::finger_path::FingerPath::default();
        path.push(crate::time::now(), 0.0);
        self.drag = Some(Drag {
            from_y: y,
            from_height,
            path,
            touch,
            moved: false,
        });
    }

    /// The grip follows the hand one to one, from peeking to full.
    fn drag_to(&mut self, y: f64) -> SheetInput {
        let Some(drag) = self.drag.as_mut() else {
            return SheetInput::Ignored;
        };
        drag.path.push(crate::time::now(), drag.from_y - y);
        let slop = f64::from(crate::tokens::Metrics::BASE.click_slop);
        drag.moved |= (y - drag.from_y).abs() > slop;
        if !drag.moved {
            return SheetInput::Taken { moved: false };
        }
        let [_, _, full] = self.frame.heights();
        let height = (drag.from_height + (drag.from_y - y)).clamp(0.0, full);
        self.drawn = self.frame.pos_of(height);
        self.pos.snap(self.drawn);
        SheetInput::Taken { moved: true }
    }

    /// The hand let go: a press without a drag raises the sheet a detent
    /// (from full, back to half); a drag settles on the detent its fling
    /// points to, carrying the release's speed.
    fn let_go(&mut self, motion: EffectiveMotion) -> SheetInput {
        let Some(drag) = self.drag.take() else {
            return SheetInput::Ignored;
        };
        if !drag.moved {
            let up = self.detent != SheetDetent::Full;
            let moved = self.step(up, MotionSource::Pointer, motion);
            return SheetInput::Taken { moved };
        }
        let height = self.frame.height_at(self.drawn);
        let speed = drag.path.release_velocity(crate::time::now());
        let rest = Snap::rest_for(height, speed, motion);
        let heights = self.frame.heights();
        let landed = Snap::nearest(heights, rest).unwrap_or(height);
        let detent = SheetDetent::ALL
            .into_iter()
            .zip(heights)
            .find(|(_, h)| (*h - landed).abs() < f64::EPSILON)
            .map_or(self.detent, |(detent, _)| detent);
        self.detent = detent;
        let unit = self.frame.unit_at(self.drawn);
        let velocity = if unit > f64::EPSILON {
            speed / unit
        } else {
            0.0
        };
        let to = detent.index();
        let now = motion::frame_now();
        let timing = Snap::SPEC.timing(
            Direction::of(to >= self.drawn),
            motion,
            MotionSource::Gesture,
        );
        self.pos.release(to, velocity, timing, now);
        self.drawn = self.pos.get(now);
        SheetInput::Taken { moved: true }
    }

    /// The sheet as a region named `name`, its grip a slider whose value is
    /// the detent, routed back by `target`
    /// ([`Self::perform_action`]).
    pub fn accessibility(&self, name: &str, target: &str) -> AccessNode {
        let grip = AccessNode::new(Role::Slider)
            .with_name(crate::i18n::widget_format("widget-sheet-height", &[]))
            .with_value(self.detent.label())
            .with_action_target(target)
            .with_hashed_key(target)
            .with_bounds(self.geometry.grip)
            .with_state(AccessState::new().focusable(true).focused(self.focused));
        AccessNode::new(Role::Group)
            .with_name(name)
            .with_hashed_key(format!("{target}.region"))
            .with_bounds(self.geometry.panel)
            .with_children(vec![grip])
    }

    /// Carry out `action` on the grip: focus it, step it, or set it to a
    /// detent by its label. Returns whether it did.
    pub fn perform_action(&mut self, action: &Action, motion: EffectiveMotion) -> bool {
        match action {
            Action::Focus => {
                self.focused = true;
                true
            }
            Action::Increment => self.step(true, MotionSource::Action, motion),
            Action::Decrement => self.step(false, MotionSource::Action, motion),
            Action::SetValue(value) => {
                let Some(detent) = SheetDetent::ALL
                    .into_iter()
                    .find(|detent| detent.label().eq_ignore_ascii_case(value.trim()))
                else {
                    return false;
                };
                self.set_detent(detent, MotionSource::Action, motion);
                true
            }
            _ => false,
        }
    }

    /// Paint the sheet where it was placed: the floating level with its
    /// two-layer shadow and inner hairline, as every floating surface
    /// stands off what is under it ([`crate::surface::floating_shadow`],
    /// [`crate::surface::panel`]), with the panel radius along its top edge,
    /// standing on the window's foot, and its grip. Returns the theme to
    /// draw what it holds with.
    pub fn paint(&self, scene: &mut Scene, transform: Affine, theme: &Theme) -> Theme {
        let local = theme.on_ground(crate::chrome::panel_ground(theme));
        let panel = self.geometry.panel;
        if panel.height() <= 0.0 {
            return local;
        }
        let radius = f64::from(theme.tokens.radii.panel);
        // Its lower corners run past the window's foot, out of sight: the
        // sheet stands on the edge, it does not float above it. What it
        // casts above its top edge stays in the area it rises in.
        let shape = Rect::new(panel.x0, panel.y0, panel.x1, panel.y1 + radius);
        let area = self.frame.area;
        let clip = Rect::new(area.x0, area.y0, area.x1, panel.y1);
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &clip,
        );
        crate::surface::floating_shadow(scene, transform, shape, radius, 1.0, theme);
        crate::surface::panel(scene, transform, shape, radius, theme);
        scene.pop_layer();
        let grip = self.geometry.grip;
        let s = theme.tokens.spacing;
        let w = f64::from(theme.tokens.metrics.icon_lg) * 1.5;
        let h = f64::from(s.s1);
        let c = grip.center();
        let pill = RoundedRect::new(
            c.x - w / 2.0,
            c.y - h / 2.0,
            c.x + w / 2.0,
            c.y + h / 2.0,
            h / 2.0,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            local.tokens.colors.text_tertiary,
            None,
            &pill,
        );
        if self.focused {
            let ring = Rect::new(c.x - w, grip.y0 + h, c.x + w, grip.y1 - h);
            crate::widget::paint_focus_ring(scene, transform, ring, 2.0, &local);
        }
        local
    }
}
