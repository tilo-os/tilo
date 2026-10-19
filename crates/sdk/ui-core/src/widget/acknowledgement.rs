//! A passing acknowledgement: "Link copied", said once in a small pill
//! near the bottom of the surface, then gone.
//!
//! Some actions leave nothing to see. A copy changes the clipboard, which
//! is out of sight, so the person cannot tell whether anything happened.
//! That needs an answer, but not a notification: a notification waits to be
//! read, stacks with its peers and is kept in the centre. This answer is
//! only about the moment it confirms, the way a phone shows "Copied" for a
//! moment.
//!
//! - It never takes focus and has no controls.
//! - A new one replaces the one showing, so a burst of copies shows one
//!   pill.
//! - It is a polite live region, so a screen reader says it once, after
//!   what it is already saying.
//! - It holds for [`ACKNOWLEDGEMENT_HOLD`], the last [`ACKNOWLEDGEMENT_FADE`]
//!   of that fading out. With reduced motion it disappears without the
//!   fade.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::a11y::{AccessNode, Politeness, Role};
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::time::{Duration, Instant};
use crate::tokens::TextRole;
use crate::widget::{TaffyTreeExt as _, Widget};
use taffy::{Dimension, LengthPercentageAuto, NodeId, Position, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};

/// How long an acknowledgement stays: enough to read two or three words
/// after the action, short enough to be gone before the next one.
pub const ACKNOWLEDGEMENT_HOLD: Duration = Duration::from_millis(1_500);

/// The fade at the end of [`ACKNOWLEDGEMENT_HOLD`]: the standard
/// confirmation's exit (`motion::Transient::CONFIRMATION`), the token
/// `durations.base`.
pub const ACKNOWLEDGEMENT_FADE: Duration = Duration::from_millis(210);

/// The motion a host allows, from its reduced-motion setting.
fn motion_for(reduce_motion: bool) -> crate::tokens::EffectiveMotion {
    crate::tokens::EffectiveMotion::new(crate::tokens::Motion::default(), reduce_motion)
}

/// One acknowledgement and when it was raised.
#[derive(Clone, Debug, PartialEq)]
pub struct Acknowledgement {
    text: String,
    raised_at: Instant,
}

impl Acknowledgement {
    pub fn new(text: impl Into<String>, raised_at: Instant) -> Self {
        Self {
            text: text.into(),
            raised_at,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether it still shows at `now`.
    pub fn is_live(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.raised_at) < ACKNOWLEDGEMENT_HOLD
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

    /// When it shows and when it is gone.
    fn span(&self) -> (Instant, Instant) {
        (self.raised_at, self.raised_at + ACKNOWLEDGEMENT_HOLD)
    }

    /// When the host must next draw without input: the fade's start, or
    /// the moment it goes.
    pub fn next_wake_at(&self, now: Instant, reduce_motion: bool) -> Option<Instant> {
        let fade_from = crate::motion::Transient::exit_starts(
            crate::motion::Transient::CONFIRMATION,
            self.raised_at + ACKNOWLEDGEMENT_HOLD,
            motion_for(reduce_motion),
        );
        let gone = self.raised_at + ACKNOWLEDGEMENT_HOLD;
        if !reduce_motion && now < fade_from {
            Some(fade_from)
        } else if now < gone {
            Some(gone)
        } else {
            None
        }
    }

    /// The pill for a `text_width` by `text_height` row, centred near the bottom of
    /// `viewport`.
    fn rect_in(viewport: Rect, text_width: f64, text_height: f64, theme: &Theme) -> Rect {
        let pad_x = f64::from(theme.tokens.spacing.s4);
        let pad_y = f64::from(theme.tokens.spacing.s2);
        let inset = f64::from(theme.tokens.spacing.s8);
        let width = (text_width + 2.0 * pad_x).min((viewport.width() - 2.0 * inset).max(0.0));
        let height = text_height + 2.0 * pad_y;
        let center_x = viewport.center().x;
        let y1 = viewport.y1 - inset;
        Rect::new(
            center_x - width / 2.0,
            y1 - height,
            center_x + width / 2.0,
            y1,
        )
    }

    /// Paint it at `alpha` near the bottom of `viewport`.
    pub fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
        alpha: f32,
    ) {
        if alpha <= 0.0 || self.text.is_empty() {
            return;
        }
        let style = theme.text(TextRole::Label).with_weight(TextWeight::Medium);
        let inset = f64::from(theme.tokens.spacing.s8);
        let pad_x = f64::from(theme.tokens.spacing.s4);
        let icon = f64::from(theme.tokens.metrics.icon_sm);
        let gap = f64::from(theme.tokens.spacing.s2);
        let max_text = (viewport.width() - 2.0 * (inset + pad_x) - icon - gap).max(0.0);
        let (text_width, text_height) = text_renderer.measure_style(&self.text, None, &style);
        let text_width = f64::from(text_width).min(max_text);
        let text_height = f64::from(text_height);
        let content_width = icon + gap + text_width;
        let rect = Self::rect_in(viewport, content_width, text_height.max(icon), theme);
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
        // A floating surface: the floating level with its inner hairline,
        // over the shadow above; the check is the success status.
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
        let x0 = rect.center().x - content_width / 2.0;
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
        if faded {
            scene.pop_layer();
        }
    }

    /// A polite status: said once, without moving focus.
    pub fn accessibility(&self) -> AccessNode {
        AccessNode::new(Role::Label)
            .with_live(Politeness::Polite)
            .with_value(self.text.clone())
    }
}

/// Where acknowledgements are raised from: cheap to clone, handed to
/// whatever confirms an action (a host's page, a search action).
#[derive(Clone, Debug, Default)]
pub struct Acknowledger {
    current: Rc<RefCell<Option<Acknowledgement>>>,
    raised: Rc<Cell<u64>>,
}

impl Acknowledger {
    /// Say `text` now, in place of whatever was showing.
    pub fn say(&self, text: impl Into<String>) {
        *self.current.borrow_mut() = Some(Acknowledgement::new(text, crate::time::now()));
        self.raised.set(self.raised.get().wrapping_add(1));
    }

    /// What shows at `now`, if anything.
    pub fn current(&self, now: Instant) -> Option<Acknowledgement> {
        self.current
            .borrow()
            .as_ref()
            .filter(|ack| ack.is_live(now))
            .cloned()
    }
}

/// The layer that shows an [`Acknowledger`]'s words over a surface: lay it
/// out over the whole surface, draw it above everything else.
pub struct AcknowledgementLayer {
    acknowledger: Acknowledger,
    rect: Rect,
    drawn: Cell<Option<(u64, bool)>>,
    reduce_motion: bool,
}

impl AcknowledgementLayer {
    pub fn new(acknowledger: Acknowledger) -> Self {
        Self {
            acknowledger,
            rect: Rect::ZERO,
            drawn: Cell::new(None),
            reduce_motion: false,
        }
    }

    pub fn acknowledger(&self) -> &Acknowledger {
        &self.acknowledger
    }

    fn showing(&self, now: Instant) -> bool {
        self.acknowledger.current(now).is_some()
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
        if let Some(ack) = &current {
            let alpha = ack.alpha_at(now, theme.reduce_motion);
            ack.draw(scene, transform, text, theme, self.rect, alpha);
        }
        self.drawn
            .set(Some((self.acknowledger.raised.get(), current.is_some())));
    }

    fn needs_redraw(&self) -> bool {
        let now = crate::time::now();
        self.drawn.get() != Some((self.acknowledger.raised.get(), self.showing(now)))
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

    /// Always present, empty while nothing shows: screen readers announce
    /// a live region whose words change, not one that appears with them.
    fn accessibility(&self) -> Option<AccessNode> {
        Some(self.acknowledger.current(crate::time::now()).map_or_else(
            || Acknowledgement::new("", crate::time::now()).accessibility(),
            |ack| ack.accessibility(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn an_acknowledgement_holds_then_fades_then_goes() {
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

    #[test]
    fn reduced_motion_shows_it_whole_until_it_goes() {
        let start = crate::time::now();
        let ack = Acknowledgement::new("Link copied", start);
        assert!((ack.alpha_at(at(start, 1_400), true) - 1.0).abs() < f32::EPSILON);
        assert!(!ack.fading_at(at(start, 1_400), true));
        assert_eq!(
            ack.next_wake_at(at(start, 100), true),
            Some(at(start, 1_500))
        );
    }

    #[test]
    fn the_host_wakes_for_the_fade_and_for_the_end() {
        let start = crate::time::now();
        let ack = Acknowledgement::new("Link copied", start);
        assert_eq!(
            ack.next_wake_at(start, false),
            Some(start + ACKNOWLEDGEMENT_HOLD - ACKNOWLEDGEMENT_FADE)
        );
        assert_eq!(
            ack.next_wake_at(at(start, 1_300), false),
            Some(start + ACKNOWLEDGEMENT_HOLD)
        );
        assert_eq!(ack.next_wake_at(at(start, 1_500), false), None);
    }

    #[test]
    fn it_is_a_polite_status_with_its_words() {
        let ack = Acknowledgement::new("Link copied", crate::time::now());
        let node = ack.accessibility();
        assert_eq!(node.live, Some(Politeness::Polite));
        assert_eq!(node.value.as_deref(), Some("Link copied"));
    }
}
