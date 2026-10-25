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
use ui::motion::SpringSpec;
use ui::num::f32_of_f64;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::Instant;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Shape, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::i18n::{t, t_args};

/// The most parts one report is about.
pub const MAX_PICKS: usize = tilo_app::feedback::report::MAX_PICKS;

/// How far the pointer travels with the button down before it drags a
/// rectangle rather than clicks.
const DRAG_SLOP: f64 = 4.0;
/// How long the screen takes to dim.
const DIM_SECS: f64 = 0.18;
/// How far a highlight stands off the part it outlines.
const OUTSET: f64 = 3.0;
const RADIUS: f64 = 6.0;

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

/// A rectangle gliding to where it is asked to be on a critically damped
/// spring, so the highlight slides between parts rather than jumping.
#[derive(Clone, Copy, Debug)]
struct Glide {
    from: Rect,
    to: Rect,
    since: Instant,
}

impl Glide {
    const SPRING: f64 = 30.0;

    fn at(&self, now: Instant) -> Rect {
        let t = now.saturating_duration_since(self.since).as_secs_f64();
        let (left, _) = SpringSpec::critical(Self::SPRING).state(1.0, 0.0, t);
        let mix = |a: f64, b: f64| b + (a - b) * left;
        Rect::new(
            mix(self.from.x0, self.to.x0),
            mix(self.from.y0, self.to.y0),
            mix(self.from.x1, self.to.x1),
            mix(self.from.y1, self.to.y1),
        )
    }

    fn settled(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.since).as_secs_f64() > 0.4
    }

    fn toward(&mut self, to: Rect, now: Instant) {
        if self.to != to {
            *self = Self {
                from: self.at(now),
                to,
                since: now,
            };
        }
    }
}

struct Session {
    opened: Instant,
    /// What is under the pointer, outermost first; `level` indexes the
    /// one lit.
    chain: Vec<PickTarget>,
    level: usize,
    glide: Option<Glide>,
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
}

impl FeedbackPicker {
    pub fn open(&mut self, now: Instant) {
        self.outcome = None;
        self.session = Some(Session {
            opened: now,
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
        self.session.as_ref().is_some_and(|s| {
            now.saturating_duration_since(s.opened).as_secs_f64() < DIM_SECS
                || s.glide.is_some_and(|g| !g.settled(now))
        })
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
        relight(session, now);
    }

    /// Widen to the part around what is lit (`out`) or narrow back in.
    pub fn widen(&mut self, out: bool, now: Instant) {
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
        relight(session, now);
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
            Key::Named(NamedKey::ArrowUp) => self.widen(true, now),
            Key::Named(NamedKey::ArrowDown) => self.widen(false, now),
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
                    self.widen(dy > 0.0, now);
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
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let colors = &theme.tokens.colors;
        let opened = now.saturating_duration_since(session.opened).as_secs_f64();
        let fade = ease_out((opened / DIM_SECS).clamp(0.0, 1.0));
        let lit = session.glide.map(|g| g.at(now));
        // The screen dims, except over what is picked and what is lit.
        let mut holes: Vec<Rect> = session.selected.iter().map(|s| s.rect).collect();
        holes.extend(lit);
        holes.extend(session.drag);
        let dim = colors
            .surfaces
            .scrim
            .multiply_alpha(colors.surfaces.scrim_strength * 0.45 * f32_of_f64(fade));
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
        );
    }
}

fn relight(session: &mut Session, now: Instant) {
    let Some(rect) = session.lit().map(|t| t.rect) else {
        return;
    };
    match session.glide.as_mut() {
        Some(glide) => glide.toward(rect, now),
        None => {
            session.glide = Some(Glide {
                from: rect,
                to: rect,
                since: now,
            });
        }
    }
}

fn ease_out(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
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

/// A picked part's number, on its top-left corner.
fn badge(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    number: usize,
) {
    let colors = &theme.tokens.colors;
    let style = theme.text(TextRole::Label);
    let centre = Point::new(rect.x0 - OUTSET + 2.0, rect.y0 - OUTSET + 2.0);
    let disc = Circle::new(centre, 10.0);
    scene.fill(Fill::NonZero, transform, colors.intent.accent, None, &disc);
    let label = number.to_string();
    let (w, h) = text.measure_style(&label, None, &style);
    text.draw_style(
        scene,
        &label,
        colors.intent.on_accent,
        transform
            .pre_translate((centre.x - f64::from(w) / 2.0, centre.y - f64::from(h) / 2.0).into()),
        None,
        &style,
    );
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
    let colors = &theme.tokens.colors;
    scene.fill(
        Fill::NonZero,
        transform,
        colors.intent.accent,
        None,
        &RoundedRect::from_rect(chip, chip_h / 2.0),
    );
    text.draw_style(
        scene,
        &shown,
        colors.intent.on_accent,
        transform.pre_translate((x + pad_x, y + pad_y).into()),
        None,
        &style,
    );
}

/// The calm line at the bottom saying what to do, with Continue and
/// Cancel once something is picked. Where its buttons are.
fn hint(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    bounds: Rect,
    picked: usize,
) -> Option<(Rect, Rect)> {
    let colors = &theme.tokens.colors;
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
    let y0 = bounds.y1 - pill_h - 28.0;
    let pill = Rect::new(x0, y0, x0 + pill_w, y0 + pill_h);
    ui::surface::shadow(
        scene,
        transform,
        pill,
        pill_h / 2.0,
        &theme.tokens.elevation.floating,
    );
    scene.fill(
        Fill::NonZero,
        transform,
        ui::chrome::shell::panel_ground(theme),
        None,
        &RoundedRect::from_rect(pill, pill_h / 2.0),
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        ui::chrome::shell::panel_border(theme),
        None,
        &RoundedRect::from_rect(pill, pill_h / 2.0),
    );
    text.draw_style(
        scene,
        &shown,
        colors.text_primary,
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
    let mut paint = PaintCtx::new(scene, transform, text, theme);
    pill_button(
        &mut paint,
        cancel,
        &cancel_label,
        (colors.overlay_medium, colors.text_primary),
    );
    pill_button(
        &mut paint,
        go,
        &go_label,
        (colors.intent.accent, colors.intent.on_accent),
    );
    Some((go, cancel))
}

/// One of the hint's buttons: `label` on a pill of `fill`, in `ink`.
fn pill_button(paint: &mut PaintCtx<'_>, rect: Rect, label: &str, (fill, ink): (Color, Color)) {
    let style = paint.theme.text(TextRole::Label);
    paint.scene.fill(
        Fill::NonZero,
        paint.transform,
        fill,
        None,
        &RoundedRect::from_rect(rect, rect.height() / 2.0),
    );
    let (lw, lh) = paint.text.measure_style(label, None, &style);
    paint.text.draw_style(
        paint.scene,
        label,
        ink,
        paint.transform.pre_translate(
            (
                rect.center().x - f64::from(lw) / 2.0,
                rect.center().y - f64::from(lh) / 2.0,
            )
                .into(),
        ),
        None,
        &style,
    );
}

#[cfg(test)]
#[path = "feedback_picker_tests.rs"]
mod tests;
