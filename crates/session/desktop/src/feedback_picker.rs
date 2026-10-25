//! Pointing at what is wrong: the screen dims a little, the part of a
//! window under the pointer lights up with its bounds, a click picks it,
//! Shift-click or a dragged rectangle picks several, the wheel widens or
//! narrows to the part around or inside, and Escape leaves.
//!
//! What can be picked comes from the host's retained widget tree through a
//! [`PickSource`], never from pixels; the picker only knows each part's
//! bounds, a key the host resolves it by, and a caption. Native and browser
//! desktops draw the same picker.

use ui::TextRole;
use ui::i18n::ArgValue;
use ui::motion::{Appear, Direction, ListChange, MotionValue, Presence, input_source};
use ui::num::f32_of_f64;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::Instant;
use ui::tokens::{EffectiveMotion, MotionSource};
use ui::widget::{BadgeFace, Button, ButtonFace, ButtonRole, ChipStyle, ControlSize, SurfaceLevel};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Shape, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::i18n::{t, t_args};

/// The most parts one report is about.
pub const MAX_PICKS: usize = tilo_app::feedback::report::MAX_PICKS;

/// How far the pointer travels with the button down before it drags a
/// rectangle rather than clicks.
const DRAG_SLOP: f64 = 4.0;
/// How far a highlight stands off the part it outlines.
const OUTSET: f64 = 3.0;
const RADIUS: f64 = 6.0;
/// How much of the theme's modal scrim the picker dims with: enough that
/// the lit part stands out even when it is most of the screen.
const DIM_SHARE: f32 = 0.75;
/// The hint's distance from the screen's top: over the desktop's top bar,
/// where the least of what people report sits.
const HINT_TOP: f64 = 6.0;
/// The hint's distance from the screen's bottom, when it moves out of the
/// way of what is lit up top.
const HINT_BOTTOM: f64 = 28.0;

/// One part of the screen the picker can light up and pick.
#[derive(Clone, Debug, PartialEq)]
pub struct PickTarget {
    /// How the host finds the part again; unique among what it offers.
    pub key: String,
    /// Where the part is, in logical pixels.
    pub rect: Rect,
    /// What the highlight's caption calls it ("Chat · Button").
    pub caption: String,
}

impl PickTarget {
    /// What a part of `app` is called while picking it and in the review:
    /// what kind of thing the app says it is ("Message in Chat"), else the
    /// app's own name for it by its role ("The “Send message” button in
    /// Chat"), else its role (`ListItem`, as assistive technology names it:
    /// "A list item in Chat"). Content never names a part here. An `app`
    /// with no display name known leaves the part alone ("A list item"):
    /// an app's id only routes and is never shown.
    #[must_use]
    pub fn caption(app: &str, role: &str, kind: Option<&str>, copy: Option<&str>) -> String {
        let what = match (kind, copy) {
            (Some(kind), _) => kind.to_owned(),
            (None, Some(name)) => t_args(
                "feedback-part-named",
                &[("name", name.into()), ("role", role.into())],
            ),
            (None, None) => t_args("feedback-part-role", &[("role", role.into())]),
        };
        if app.trim().is_empty() {
            return what;
        }
        t_args(
            "feedback-part-in",
            &[("what", what.into()), ("app", app.into())],
        )
    }
}

/// The host's view of what is on screen, from its retained widget tree.
pub trait PickSource {
    /// The nameable parts under `point`, the outermost first and the
    /// deepest last.
    fn at(&self, point: Point) -> Vec<PickTarget>;
    /// The outermost nameable parts wholly inside `area`, at most `limit`.
    fn within(&self, area: Rect, limit: usize) -> Vec<PickTarget>;
}

/// How picking ended.
#[derive(Clone, Debug, PartialEq)]
pub enum PickerOutcome {
    Picked(Vec<PickTarget>),
    Cancelled,
}

struct Session {
    /// How far the screen is dimmed: it comes as a modal's scrim does, on
    /// [`Appear`], so a shortcut dims it at once and a click fades it in.
    dim: Presence,
    /// What is under the pointer, outermost first; `level` indexes the
    /// one lit.
    chain: Vec<PickTarget>,
    level: usize,
    /// Where the highlight is drawn: it lands on the first part lit and on
    /// a part a key lights, and glides on [`ListChange::MOVE`], as a
    /// selection does, to a part the pointer lights, so the eye follows it.
    glide: Option<MotionValue<Rect>>,
    selected: Vec<PickTarget>,
    press: Option<Point>,
    drag: Option<Rect>,
    /// The hint's Continue and Cancel buttons, as last drawn.
    buttons: Option<(Rect, Rect)>,
}

impl Session {
    fn lit(&self) -> Option<&PickTarget> {
        self.chain.get(self.level)
    }

    fn toggle(&mut self, target: PickTarget) {
        if let Some(at) = self.selected.iter().position(|s| s.key == target.key) {
            self.selected.remove(at);
        } else if self.selected.len() < MAX_PICKS {
            self.selected.push(target);
        }
    }

    fn add(&mut self, target: PickTarget) {
        if self.selected.len() < MAX_PICKS && !self.selected.iter().any(|s| s.key == target.key) {
            self.selected.push(target);
        }
    }
}

/// The picker over a desktop. Closed until [`Self::open`].
#[derive(Default)]
pub struct FeedbackPicker {
    session: Option<Session>,
    outcome: Option<PickerOutcome>,
    /// The motion the person allows, as of the last draw: what the next
    /// open dims with.
    motion: EffectiveMotion,
}

impl FeedbackPicker {
    /// Open as the input the toolkit handled last asked for it
    /// ([`input_source`]): a key or an action dims the screen at once, a
    /// click fades it in.
    pub fn open(&mut self, now: Instant) {
        self.outcome = None;
        let mut dim = Presence::closed();
        dim.show(true, Appear::SPEC, self.motion, input_source(), now);
        self.session = Some(Session {
            dim,
            chain: Vec::new(),
            level: 0,
            glide: None,
            selected: Vec::new(),
            press: None,
            drag: None,
            buttons: None,
        });
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.session.is_some()
    }

    /// How picking ended, once.
    pub fn take_outcome(&mut self) -> Option<PickerOutcome> {
        self.outcome.take()
    }

    /// What is picked so far.
    #[must_use]
    pub fn selected(&self) -> &[PickTarget] {
        self.session.as_ref().map_or(&[], |s| s.selected.as_slice())
    }

    /// The rectangle being dragged, while the button is down past a click:
    /// releasing it picks what [`PickSource::within`] finds there. A host
    /// whose parts are drawn elsewhere asks for them before it lets the
    /// release through.
    #[must_use]
    pub fn dragging(&self) -> Option<Rect> {
        self.session.as_ref().and_then(|s| s.drag)
    }

    fn finish(&mut self, outcome: PickerOutcome) {
        self.session = None;
        self.outcome = Some(outcome);
    }

    fn done(&mut self) {
        let picked = self
            .session
            .as_ref()
            .map(|s| s.selected.clone())
            .unwrap_or_default();
        self.finish(if picked.is_empty() {
            PickerOutcome::Cancelled
        } else {
            PickerOutcome::Picked(picked)
        });
    }

    #[must_use]
    pub fn needs_animation_frame(&self, now: Instant) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| s.dim.is_moving(now) || s.glide.is_some_and(|g| g.is_moving(now)))
    }

    /// Light up what is under `point`.
    pub fn hover(&mut self, point: Point, source: &dyn PickSource, now: Instant) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if let Some(press) = session.press {
            if session.drag.is_some() || press.distance(point) > DRAG_SLOP {
                session.drag = Some(Rect::from_points(press, point));
            }
            return;
        }
        let chain = source.at(point);
        // Widened or narrowed along the same chain, the same level stays.
        let same = chain.len() == session.chain.len()
            && chain
                .iter()
                .zip(&session.chain)
                .all(|(a, b)| a.key == b.key);
        if !same {
            session.level = chain.len().saturating_sub(1);
            session.chain = chain;
        }
        relight(session, self.motion, MotionSource::Pointer, now);
    }

    /// Widen to the part around what is lit (`out`) or narrow back in, as
    /// `source` asked: a key lands the highlight, the wheel glides it.
    pub fn widen(&mut self, out: bool, source: MotionSource, now: Instant) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        session.level = if out {
            session.level.saturating_sub(1)
        } else {
            session
                .level
                .saturating_add(1)
                .min(session.chain.len().saturating_sub(1))
        };
        relight(session, self.motion, source, now);
    }

    /// The person pressed or released the primary button at `point`.
    pub fn button(&mut self, point: Point, pressed: bool, shift: bool, source: &dyn PickSource) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if pressed {
            if let Some((go, cancel)) = session.buttons {
                if go.contains(point) {
                    return self.done();
                }
                if cancel.contains(point) {
                    return self.finish(PickerOutcome::Cancelled);
                }
            }
            session.press = Some(point);
            return;
        }
        session.press = None;
        if let Some(area) = session.drag.take() {
            let room = MAX_PICKS.saturating_sub(session.selected.len());
            for target in source.within(area, room) {
                session.add(target);
            }
            return;
        }
        let Some(target) = session.lit().cloned() else {
            return;
        };
        if shift {
            session.toggle(target);
        } else {
            session.add(target);
            self.done();
        }
    }

    /// A key while picking; whether the picker took it (it takes all).
    pub fn key(&mut self, key: &Key, now: Instant) -> bool {
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        match key {
            Key::Named(NamedKey::Escape) => self.finish(PickerOutcome::Cancelled),
            Key::Named(NamedKey::Enter) => {
                if session.selected.is_empty()
                    && let Some(target) = session.lit().cloned()
                {
                    session.add(target);
                }
                self.done();
            }
            Key::Named(NamedKey::ArrowUp) => self.widen(true, MotionSource::Keyboard, now),
            Key::Named(NamedKey::ArrowDown) => {
                self.widen(false, MotionSource::Keyboard, now);
            }
            _ => {}
        }
        true
    }

    /// Route a window event; while open the picker takes every pointer and
    /// key event, so nothing underneath reacts to a pick. The pointer is
    /// `cursor`, in logical pixels.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        cursor: Option<Point>,
        modifiers: ModifiersState,
        source: &dyn PickSource,
        now: Instant,
    ) -> bool {
        if self.session.is_none() {
            return false;
        }
        match event {
            WindowEvent::CursorMoved { .. } => {
                if let Some(point) = cursor {
                    self.hover(point, source, now);
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(point) = cursor {
                    self.button(
                        point,
                        *state == ElementState::Pressed,
                        modifiers.shift_key(),
                        source,
                    );
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    MouseScrollDelta::PixelDelta(p) => p.y,
                };
                if dy.abs() > f64::EPSILON {
                    self.widen(dy > 0.0, MotionSource::Pointer, now);
                }
            }
            WindowEvent::KeyboardInput { event: key, .. } if key.state == ElementState::Pressed => {
                self.key(&key.logical_key, now);
            }
            _ => {}
        }
        true
    }

    /// Draw over everything else within `bounds`.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        bounds: Rect,
        now: Instant,
    ) {
        self.motion = theme.motion();
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let colors = &theme.tokens.colors;
        let fade = session.dim.progress(now);
        let lit = session.glide.map(|g| g.get(now));
        // The screen dims, except over what is picked and what is lit.
        let mut holes: Vec<Rect> = session.selected.iter().map(|s| s.rect).collect();
        holes.extend(lit);
        holes.extend(session.drag);
        let dim = colors
            .surfaces
            .scrim
            .multiply_alpha(colors.surfaces.scrim_strength * DIM_SHARE * f32_of_f64(fade));
        scene.fill(Fill::EvenOdd, transform, dim, None, &dimmed(bounds, &holes));
        let ink = colors.intent.accent_ink;
        for (index, picked) in session.selected.iter().enumerate() {
            let outline = round(picked.rect);
            scene.stroke(&Stroke::new(2.0), transform, ink, None, &outline);
            badge(
                scene,
                transform,
                text,
                theme,
                picked.rect,
                index.saturating_add(1),
            );
        }
        if let (Some(rect), None) = (lit, session.drag) {
            let outline = round(rect);
            scene.fill(
                Fill::NonZero,
                transform,
                ink.multiply_alpha(0.08),
                None,
                &outline,
            );
            scene.stroke(&Stroke::new(1.5), transform, ink, None, &outline);
            if let Some(target) = session.lit() {
                caption(scene, transform, text, theme, rect, bounds, &target.caption);
            }
        }
        if let Some(area) = session.drag {
            scene.fill(
                Fill::NonZero,
                transform,
                ink.multiply_alpha(0.10),
                None,
                &area,
            );
            scene.stroke(&Stroke::new(1.0), transform, ink, None, &area);
        }
        session.buttons = hint(
            scene,
            transform,
            text,
            theme,
            bounds,
            session.selected.len(),
            &holes,
        );
    }
}

/// Send the highlight to what is lit, as `source` asked.
fn relight(session: &mut Session, motion: EffectiveMotion, source: MotionSource, now: Instant) {
    let Some(rect) = session.lit().map(|t| t.rect) else {
        return;
    };
    match session.glide.as_mut() {
        Some(glide) => {
            glide.set(
                rect,
                ListChange::MOVE.timing(Direction::Enter, motion, source),
                now,
            );
        }
        None => session.glide = Some(MotionValue::new(rect)),
    }
}

fn round(rect: Rect) -> RoundedRect {
    RoundedRect::from_rect(rect.inflate(OUTSET, OUTSET), RADIUS)
}

/// The screen with a hole for each of `holes`; one inside another is left
/// out, since an even-odd fill would dim it again.
fn dimmed(bounds: Rect, holes: &[Rect]) -> BezPath {
    let mut path = bounds.to_path(0.1);
    for (index, hole) in holes.iter().enumerate() {
        let inside = holes.iter().enumerate().any(|(other, around)| {
            other != index && around.contains_rect(*hole) && (*around != *hole || other < index)
        });
        if !inside {
            path.extend(round(*hole).path_elements(0.1));
        }
    }
    path
}

/// A picked part's number, on its top-left corner: the shared badge in
/// the accent, the selection's colour.
fn badge(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    number: usize,
) {
    let label = number.to_string();
    let height = BadgeFace::height(theme);
    let width = BadgeFace::intrinsic_width(&label, text, theme).max(height);
    let centre = Point::new(rect.x0 - OUTSET + 2.0, rect.y0 - OUTSET + 2.0);
    BadgeFace::new(Rect::from_center_size(centre, (width, height)), &label)
        .style(ChipStyle::Accent)
        .paint(scene, transform, text, theme);
}

/// What the lit part is, just above it (inside its top when it touches the
/// screen's top).
fn caption(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    bounds: Rect,
    caption: &str,
) {
    if caption.is_empty() {
        return;
    }
    let style = theme.text(TextRole::Label);
    let shown = text.elide_to_width_style(caption, 320.0, &style);
    let (w, h) = text.measure_style(&shown, None, &style);
    let (pad_x, pad_y) = (8.0, 4.0);
    let chip_w = f64::from(w) + 2.0 * pad_x;
    let chip_h = f64::from(h) + 2.0 * pad_y;
    let x = (rect.x0 - OUTSET).clamp(bounds.x0 + 4.0, (bounds.x1 - chip_w - 4.0).max(bounds.x0));
    let above = rect.y0 - OUTSET - chip_h - 4.0;
    let y = if above >= bounds.y0 + 4.0 {
        above
    } else {
        rect.y0 + OUTSET + 4.0
    };
    let chip = Rect::new(x, y, x + chip_w, y + chip_h);
    let on = ui::widget::paint_surface(scene, transform, chip, SurfaceLevel::Floating, theme);
    text.draw_style(
        scene,
        &shown,
        on.tokens.colors.text_primary,
        transform.pre_translate((x + pad_x, y + pad_y).into()),
        None,
        &style,
    );
}

/// The calm line at the top saying what to do, with Continue and
/// Cancel once something is picked. Where its buttons are.
fn hint(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    bounds: Rect,
    picked: usize,
    avoid: &[Rect],
) -> Option<(Rect, Rect)> {
    let style = theme.text(TextRole::Label);
    let line = if picked == 0 {
        t("feedback-picker-hint")
    } else {
        t_args(
            "feedback-picker-picked",
            &[(
                "count",
                ArgValue::Number(f64::from(u32::try_from(picked).unwrap_or(u32::MAX))),
            )],
        )
    };
    let shown =
        text.elide_to_width_style(&line, f32_of_f64((bounds.width() - 64.0).max(0.0)), &style);
    let (w, h) = text.measure_style(&shown, None, &style);
    let (w, h) = (f64::from(w), f64::from(h));
    let pad = 14.0;
    let gap = 8.0;
    let go_label = t("feedback-picker-continue");
    let cancel_label = t("feedback-picker-cancel");
    let (go_w, _) = text.measure_style(&go_label, None, &style);
    let (cancel_w, _) = text.measure_style(&cancel_label, None, &style);
    let button_h = h + 12.0;
    let go_w = f64::from(go_w) + 24.0;
    let cancel_w = f64::from(cancel_w) + 24.0;
    let buttons_w = if picked > 0 {
        gap + cancel_w + gap + go_w
    } else {
        0.0
    };
    let pill_h = button_h + 12.0;
    let pill_w = (w + 2.0 * pad + buttons_w).min(bounds.width() - 16.0);
    let x0 = bounds.center().x - pill_w / 2.0;
    // Over the top bar, clear of the windows and their composers, unless
    // that covers what is lit or picked: then low, so the hint never hides
    // the part it talks about.
    let high = Rect::new(
        x0,
        bounds.y0 + HINT_TOP,
        x0 + pill_w,
        bounds.y0 + HINT_TOP + pill_h,
    );
    let pill = if avoid
        .iter()
        .any(|part| part.intersect(high.inflate(0.0, 12.0)).area() > 0.0)
    {
        let y1 = bounds.y1 - HINT_BOTTOM;
        Rect::new(x0, y1 - pill_h, x0 + pill_w, y1)
    } else {
        high
    };
    let x0 = pill.x0;
    let on = ui::widget::paint_surface(scene, transform, pill, SurfaceLevel::Floating, theme);
    text.draw_style(
        scene,
        &shown,
        on.tokens.colors.text_primary,
        transform.pre_translate((x0 + pad, pill.center().y - h / 2.0).into()),
        None,
        &style,
    );
    if picked == 0 {
        return None;
    }
    let by = pill.center().y - button_h / 2.0;
    let go = Rect::new(pill.x1 - 6.0 - go_w, by, pill.x1 - 6.0, by + button_h);
    let cancel = Rect::new(go.x0 - gap - cancel_w, by, go.x0 - gap, by + button_h);
    for (rect, label, role) in [
        (cancel, &cancel_label, ButtonRole::Secondary),
        (go, &go_label, ButtonRole::Primary),
    ] {
        Button::paint(
            scene,
            transform,
            text,
            &on,
            &ButtonFace::new(rect, label)
                .role(role)
                .control_size(ControlSize::Small),
        );
    }
    Some((go, cancel))
}

#[cfg(test)]
#[path = "feedback_picker_tests.rs"]
mod tests;
