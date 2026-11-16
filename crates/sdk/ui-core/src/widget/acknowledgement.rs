//! A passing acknowledgement: "Link copied", said once in a small pill near the bottom of the surface,
//! then gone. One that can be taken back ("Notes closed") carries the one button that does it, and
//! stays long enough to reach it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::a11y::{AccessNode, Politeness, Role};
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::time::{Duration, Instant};
use crate::tokens::TextRole;
use crate::widget::{Button, ButtonFace, ButtonRole, ControlSize, TaffyTreeExt as _, Widget};
use taffy::{Dimension, LengthPercentageAuto, NodeId, Position, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};

pub const ACKNOWLEDGEMENT_HOLD: Duration = Duration::from_millis(1_500);

/// How long a pill with a button stays: long enough to read it and reach the button.
pub const ACKNOWLEDGEMENT_ACTION_HOLD: Duration = Duration::from_millis(6_000);

fn motion_for(reduce_motion: bool) -> crate::tokens::EffectiveMotion {
    crate::tokens::EffectiveMotion::new(crate::tokens::Motion::default(), reduce_motion)
}

/// How a pill is drawn: its opacity, and whether the pointer is over its button or pressing it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Look {
    pub alpha: f32,
    pub hovered: bool,
    pub pressed: bool,
}

impl Look {
    /// A pill at `alpha` , its button at rest.
    #[must_use]
    pub fn at(alpha: f32) -> Self {
        Self {
            alpha,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Acknowledgement {
    text: String,
    raised_at: Instant,
    /// The label of the button that takes the action back, when it can be.
    action: Option<String>,
}

impl Acknowledgement {
    pub fn new(text: impl Into<String>, raised_at: Instant) -> Self {
        Self {
            text: text.into(),
            raised_at,
            action: None,
        }
    }

    /// The pill offers a button labelled `label` .
    #[must_use]
    pub fn with_action(mut self, label: impl Into<String>) -> Self {
        self.action = Some(label.into());
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The label of its button, when it has one.
    pub fn action(&self) -> Option<&str> {
        self.action.as_deref()
    }

    fn hold(&self) -> Duration {
        if self.action.is_some() {
            ACKNOWLEDGEMENT_ACTION_HOLD
        } else {
            ACKNOWLEDGEMENT_HOLD
        }
    }

    pub fn is_live(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.raised_at) < self.hold()
    }

    /// Its opacity at `now`: whole until the fade, then down to nothing.
    /// With `reduce_motion` it stays whole until it goes.
    pub fn alpha_at(&self, now: Instant, reduce_motion: bool) -> f32 {
        crate::motion::Transient::opacity(
            crate::motion::Transient::CONFIRMATION,
            self.span(),
            now,
            motion_for(reduce_motion),
        )
    }

    /// Whether it is fading at `now`, so every frame changes.
    pub fn fading_at(&self, now: Instant, reduce_motion: bool) -> bool {
        crate::motion::Transient::is_moving(
            crate::motion::Transient::CONFIRMATION,
            self.span(),
            now,
            motion_for(reduce_motion),
        )
    }

    fn span(&self) -> (Instant, Instant) {
        (self.raised_at, self.raised_at + self.hold())
    }

    /// When the host must next draw without input: the fade's start, or
    /// the moment it goes.
    pub fn next_wake_at(&self, now: Instant, reduce_motion: bool) -> Option<Instant> {
        let gone = self.raised_at + self.hold();
        let fade_from = crate::motion::Transient::exit_starts(
            crate::motion::Transient::CONFIRMATION,
            gone,
            motion_for(reduce_motion),
        );
        if !reduce_motion && now < fade_from {
            Some(fade_from)
        } else if now < gone {
            Some(gone)
        } else {
            None
        }
    }

    fn rect_in(
        viewport: Rect,
        content_width: f64,
        height: f64,
        pad: (f64, f64),
        theme: &Theme,
    ) -> Rect {
        let inset = f64::from(theme.tokens.spacing.s8);
        let width = (content_width + pad.0 + pad.1).min((viewport.width() - 2.0 * inset).max(0.0));
        let center_x = viewport.center().x;
        let y1 = viewport.y1 - inset;
        Rect::new(
            center_x - width / 2.0,
            y1 - height,
            center_x + width / 2.0,
            y1,
        )
    }

    /// Draw the pill in `viewport` as `look` says; where its button stands, when it has one.
    pub fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
        look: Look,
    ) -> Option<Rect> {
        let Look {
            alpha,
            hovered,
            pressed,
        } = look;
        if alpha <= 0.0 || self.text.is_empty() {
            return None;
        }
        let style = theme.text(TextRole::Label).with_weight(TextWeight::Medium);
        let inset = f64::from(theme.tokens.spacing.s8);
        let pad_x = f64::from(theme.tokens.spacing.s4);
        let pad_y = f64::from(theme.tokens.spacing.s2);
        let icon = f64::from(theme.tokens.metrics.icon_sm);
        let gap = f64::from(theme.tokens.spacing.s2);
        let button = self.action.as_deref().map(|label| {
            let face = ButtonFace::new(Rect::ZERO, label)
                .role(ButtonRole::Primary)
                .control_size(ControlSize::Small);
            (
                label,
                face.intrinsic_width(text_renderer, theme),
                ButtonFace::height(ControlSize::Small, theme),
            )
        });
        // A button stands in the pill's end, as far from its edges as from its top and bottom.
        let button_inset = f64::from(theme.tokens.spacing.s1);
        let (button_width, button_height) = button.map_or((0.0, 0.0), |(_, w, h)| (w, h));
        let trailing = if button.is_some() {
            pad_x + button_width + button_inset
        } else {
            pad_x
        };
        let max_text = (viewport.width() - 2.0 * inset - pad_x - trailing - icon - gap).max(0.0);
        let (text_width, text_height) = text_renderer.measure_style(&self.text, None, &style);
        let text_width = f64::from(text_width).min(max_text);
        let text_height = f64::from(text_height);
        let content_width = icon + gap + text_width;
        let height = (text_height.max(icon) + 2.0 * pad_y).max(button_height + 2.0 * button_inset);
        let rect = Self::rect_in(viewport, content_width, height, (pad_x, trailing), theme);
        let radius = rect.height() / 2.0;
        let faded = alpha < 0.999;
        if faded {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                alpha,
                transform,
                &crate::widget::floating_surface::painted_bounds(rect, theme),
            );
        }
        crate::surface::shadow(
            scene,
            transform,
            rect,
            radius,
            &theme.tokens.elevation.floating,
        );
        let colors = &theme.tokens.colors;
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.overlay,
            None,
            &RoundedRect::from_rect(rect, radius),
        );
        crate::surface::floating_edge(scene, transform, rect, radius, theme);
        let ink = colors.on_surface(colors.surfaces.overlay).ink;
        let x0 = rect.x0 + pad_x;
        let center_y = rect.center().y;
        crate::icons::draw_icon(
            scene,
            transform,
            Rect::from_center_size(Point::new(x0 + icon / 2.0, center_y), (icon, icon)),
            crate::icons::Icon::Check,
            colors.semantic.success,
            crate::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        let origin = Point::new(x0 + icon + gap, center_y - text_height / 2.0);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a pill's text width is a few hundred logical pixels"
        )]
        let max_width = Some(max_text as f32);
        text_renderer.draw_style(
            scene,
            &self.text,
            ink,
            transform.pre_translate(origin.to_vec2()),
            max_width,
            &style,
        );
        let button = button.map(|(label, width, height)| {
            let x1 = rect.x1 - button_inset - (radius - height / 2.0 - button_inset).max(0.0);
            let at = Rect::new(
                x1 - width,
                center_y - height / 2.0,
                x1,
                center_y + height / 2.0,
            );
            let face = ButtonFace::new(at, label)
                .role(ButtonRole::Primary)
                .control_size(ControlSize::Small)
                .hovered(hovered)
                .pressed(pressed);
            Button::paint(scene, transform, text_renderer, theme, &face);
            at
        });
        if faded {
            scene.pop_layer();
        }
        button
    }

    pub fn accessibility(&self) -> AccessNode {
        AccessNode::new(Role::Label)
            .with_live(Politeness::Polite)
            .with_value(self.text.clone())
    }
}

/// What a pill's button does.
type Run = Rc<dyn Fn()>;

#[derive(Clone, Default)]
pub struct Acknowledger {
    current: Rc<RefCell<Option<Acknowledgement>>>,
    /// What the current pill's button does, when it has one.
    run: Rc<RefCell<Option<Run>>>,
    raised: Rc<Cell<u64>>,
}

impl std::fmt::Debug for Acknowledger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Acknowledger")
            .field("current", &self.current)
            .field("raised", &self.raised)
            .finish_non_exhaustive()
    }
}

impl Acknowledger {
    pub fn say(&self, text: impl Into<String>) {
        self.raise(Acknowledgement::new(text, crate::time::now()), None);
    }

    /// Say `text` with a button labelled `label` that runs `run` : what the person just did, and
    /// the one press that takes it back.
    pub fn say_with_action(
        &self,
        text: impl Into<String>,
        label: impl Into<String>,
        run: impl Fn() + 'static,
    ) {
        self.raise(
            Acknowledgement::new(text, crate::time::now()).with_action(label),
            Some(Rc::new(run)),
        );
    }

    fn raise(&self, said: Acknowledgement, run: Option<Run>) {
        *self.current.borrow_mut() = Some(said);
        *self.run.borrow_mut() = run;
        self.raised.set(self.raised.get().wrapping_add(1));
    }

    /// Press the current pill's button: the pill goes and what it offered runs. Whether there was
    /// one to press.
    pub fn press_action(&self) -> bool {
        let live = self.current(crate::time::now()).is_some();
        let run = self.run.borrow_mut().take();
        let Some(run) = run.filter(|_| live) else {
            return false;
        };
        *self.current.borrow_mut() = None;
        self.raised.set(self.raised.get().wrapping_add(1));
        run();
        true
    }

    pub fn current(&self, now: Instant) -> Option<Acknowledgement> {
        self.current
            .borrow()
            .as_ref()
            .filter(|ack| ack.is_live(now))
            .cloned()
    }
}

#[crate::component::not_a_component(framework, "the framework's acknowledgement of an input")]
pub struct AcknowledgementLayer {
    acknowledger: Acknowledger,
    rect: Rect,
    drawn: Cell<Option<(u64, bool, bool, bool)>>,
    reduce_motion: bool,
    /// Where the pill's button was last drawn.
    button: Cell<Option<Rect>>,
    /// The pointer is over the button, and a press began on it.
    hovered: bool,
    pressed: bool,
}

impl AcknowledgementLayer {
    pub fn new(acknowledger: Acknowledger) -> Self {
        Self {
            acknowledger,
            rect: Rect::ZERO,
            drawn: Cell::new(None),
            reduce_motion: false,
            button: Cell::new(None),
            hovered: false,
            pressed: false,
        }
    }

    pub fn acknowledger(&self) -> &Acknowledger {
        &self.acknowledger
    }

    /// Say the pill within `area` : a host that draws the layer itself, not through layout, says
    /// where it stands, such as the display the person works on.
    pub fn place(&mut self, area: Rect) {
        self.rect = area;
    }

    fn showing(&self, now: Instant) -> bool {
        self.acknowledger.current(now).is_some()
    }

    /// The pointer moved to `at` (logical, in the surface the layer draws on), or left it. Whether
    /// it is over the pill's button.
    pub fn pointer_at(&mut self, at: Option<Point>) -> bool {
        self.hovered = at.is_some_and(|at| self.button_at(at));
        if !self.hovered {
            self.pressed = false;
        }
        self.hovered
    }

    /// The primary button went down (`down`) or up at `at` . Whether the pill's button takes it: a
    /// press that begins on the button is the button's, and its release there presses it.
    pub fn press(&mut self, at: Point, down: bool) -> bool {
        let on = self.button_at(at);
        if down {
            self.pressed = on;
            return on;
        }
        let pressed = std::mem::take(&mut self.pressed);
        if pressed && on {
            self.acknowledger.press_action();
        }
        pressed
    }

    fn button_at(&self, at: Point) -> bool {
        self.acknowledger.current(crate::time::now()).is_some()
            && self.button.get().is_some_and(|button| button.contains(at))
    }
}

impl Widget for AcknowledgementLayer {
    fn name(&self) -> &'static str {
        "Acknowledgement"
    }

    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            position: Position::Absolute,
            inset: taffy::Rect {
                left: LengthPercentageAuto::length(0.0),
                top: LengthPercentageAuto::length(0.0),
                right: LengthPercentageAuto::auto(),
                bottom: LengthPercentageAuto::auto(),
            },
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        tree.widget_leaf(style, map, self)
    }

    crate::rect_geometry!();

    /// The layer covers the surface whatever it says, so nothing in it ever
    /// changes its size: once laid out it has nothing to measure.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.reduce_motion = theme.reduce_motion;
        let now = crate::time::now();
        let current = self.acknowledger.current(now);
        let button = current.as_ref().and_then(|ack| {
            let alpha = ack.alpha_at(now, theme.reduce_motion);
            ack.draw(
                scene,
                transform,
                text,
                theme,
                self.rect,
                Look {
                    alpha,
                    hovered: self.hovered,
                    pressed: self.pressed,
                },
            )
        });
        self.button.set(button);
        self.drawn.set(Some((
            self.acknowledger.raised.get(),
            current.is_some(),
            self.hovered,
            self.pressed,
        )));
    }

    fn needs_redraw(&self) -> bool {
        let now = crate::time::now();
        self.drawn.get()
            != Some((
                self.acknowledger.raised.get(),
                self.showing(now),
                self.hovered,
                self.pressed,
            ))
    }

    fn needs_animation_frame(&self) -> bool {
        let now = crate::time::now();
        self.acknowledger
            .current(now)
            .is_some_and(|ack| ack.fading_at(now, self.reduce_motion))
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let now = crate::time::now();
        self.acknowledger
            .current(now)
            .and_then(|ack| ack.next_wake_at(now, self.reduce_motion))
    }

    /// The words, and the button that takes the action back where it is drawn: a reader presses it
    /// as the pointer does.
    fn accessibility(&self) -> Option<AccessNode> {
        let now = crate::time::now();
        let Some(ack) = self.acknowledger.current(now) else {
            return Some(Acknowledgement::new("", now).accessibility());
        };
        let button = ack.action().zip(self.button.get()).map(|(label, rect)| {
            AccessNode::new(Role::Button)
                .with_name(label)
                .with_bounds(rect)
        });
        Some(
            ack.accessibility()
                .with_children(button.into_iter().collect()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    tilo_test_env::scenario! {
        /// The acknowledgement as a value: its hold, its fade, the wakes it asks for and its announcement.
        /// Each phase was a test of its own and builds its own fixture.
        fn an_acknowledgement_holds_fades_wakes_and_is_announced;

        phase an_acknowledgement_holds_then_fades_then_goes;
        {
            let start = crate::time::now();
            let ack = Acknowledgement::new("Link copied", start);
            assert!(ack.is_live(start));
            assert!((ack.alpha_at(at(start, 100), false) - 1.0).abs() < f32::EPSILON);
            assert!(!ack.fading_at(at(start, 100), false));
            let fading = at(start, 1_400);
            assert!(ack.fading_at(fading, false));
            let alpha = ack.alpha_at(fading, false);
            assert!(alpha > 0.0 && alpha < 1.0, "half way out: {alpha}");
            assert!(!ack.is_live(at(start, 1_500)));
            assert!(ack.alpha_at(at(start, 1_600), false) <= 0.0);
        }

        phase reduced_motion_shows_it_whole_until_it_goes;
        {
            let start = crate::time::now();
            let ack = Acknowledgement::new("Link copied", start);
            assert!((ack.alpha_at(at(start, 1_400), true) - 1.0).abs() < f32::EPSILON);
            assert!(!ack.fading_at(at(start, 1_400), true));
            assert_eq!(
                ack.next_wake_at(at(start, 100), true),
                Some(at(start, 1_500))
            );
        }

        phase the_host_wakes_for_the_fade_and_for_the_end;
        {
            let start = crate::time::now();
            let ack = Acknowledgement::new("Link copied", start);
            assert_eq!(
                ack.next_wake_at(start, false),
                Some(
                    start + ACKNOWLEDGEMENT_HOLD
                        - crate::motion::Transient::exit_time(
                            crate::motion::Transient::CONFIRMATION,
                            motion_for(false),
                        )
                )
            );
            assert_eq!(
                ack.next_wake_at(at(start, 1_300), false),
                Some(start + ACKNOWLEDGEMENT_HOLD)
            );
            assert_eq!(ack.next_wake_at(at(start, 1_500), false), None);
        }

        phase it_is_a_polite_status_with_its_words;
        {
            let ack = Acknowledgement::new("Link copied", crate::time::now());
            let node = ack.accessibility();
            assert_eq!(node.live, Some(Politeness::Polite));
            assert_eq!(node.value.as_deref(), Some("Link copied"));
        }
    }
}
