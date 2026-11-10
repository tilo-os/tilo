//! The "Following Ada" banner: what says, while this person's view tracks someone else's, whose
//! view it is and how to stop.
//! A 2 px frame in their colour runs round the work area, and a small bar docks in the top bar
//! with their face, "Following Ada" and Stop. It stands in the bar's own strip, above every
//! window's content at every width, so it never covers what the person is reading; its height and
//! length follow the bar's, so it stays inside it however narrow the display is. When the person's
//! own input ends it, the bar says "Stopped following" for a moment. The native shell and the
//! browser paint the same banner; each host keeps its own follow state and tells the banner when
//! following starts and ends.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::TextRole;
use ui::motion::{Appear, Direction, Presence, Transient};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::tokens::{EffectiveMotion, MotionSource};
use ui::widget::{Button, ButtonFace, ButtonRole};

use crate::collaborators::RosterRow;
use crate::i18n::{t, t_args};

/// How long "Stopped following" stays after the person's own input ended following.
pub const STOPPED_FOR: Duration = Duration::from_secs(1);

/// The height the banner's bar asks for: what a floating card would take, cut down to the top
/// bar's strip where that is shorter, so the banner stands in the bar and on nothing else.
const CHIP_HEIGHT: f64 = 40.0;

/// Where the banner goes this frame, and how it moves.
#[derive(Clone, Copy, Debug)]
pub struct BannerFrame {
    /// The work area the frame runs round: the screen under the top bar.
    pub work_area: Rect,
    /// The top bar, where the banner docks: the strip above every window's content.
    pub bar: Rect,
    pub cursor: Option<Point>,
    pub motion: EffectiveMotion,
}

/// The banner across frames.
#[derive(Debug, Default)]
pub struct FollowBanner {
    transition: Presence,
    /// When the person's own input ended following.
    stopped: Option<Instant>,
    /// Whom the banner shows, kept while it leaves after they left.
    shown: Option<RosterRow>,
    chip: Rect,
    stop: Rect,
    /// What the banner says as painted ("Following Ada", "Stopped following"), and its Stop's words
    /// while following.
    said: (String, String),
}

/// The semantic action target of the banner's Stop.
pub const STOP_TARGET: &str = "follow:stop";

impl FollowBanner {
    /// Following `row` began.
    pub fn started(
        &mut self,
        row: Option<RosterRow>,
        motion: EffectiveMotion,
        source: MotionSource,
        now: Instant,
    ) {
        self.stopped = None;
        self.shown = row;
        self.transition
            .show(true, Appear::SPEC, motion, source, now);
    }

    /// Following ended; `by_own_input` when the person took their view back by doing something of
    /// their own (the bar says so a moment).
    pub fn ended(
        &mut self,
        by_own_input: bool,
        motion: EffectiveMotion,
        source: MotionSource,
        now: Instant,
    ) {
        if by_own_input && self.transition.is_open() {
            self.stopped = Some(now);
        }
        self.transition
            .show(false, Appear::SPEC, motion, source, now);
        self.stop = Rect::ZERO;
    }

    /// The bar as painted, `Rect::ZERO` while there is none.
    #[must_use]
    pub fn chip_rect(&self) -> Rect {
        self.chip
    }

    /// Its Stop, while following.
    #[must_use]
    pub fn stop_rect(&self) -> Rect {
        self.stop
    }

    /// The banner for assistive technology, while it is up: a polite status saying whose view this
    /// follows (or that it stopped), with its Stop ([`STOP_TARGET`]) while following.
    #[must_use]
    pub fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};
        if self.chip.is_zero_area() || self.said.0.is_empty() {
            return None;
        }
        let mut node = AccessNode::new(Role::Group)
            .with_hashed_key("follow-banner")
            .with_name(self.said.0.clone())
            .with_live(Politeness::Polite)
            .with_bounds(self.chip);
        if !self.stop.is_zero_area() {
            node = node.with_children(vec![
                AccessNode::new(Role::Button)
                    .with_name(self.said.1.clone())
                    .with_action_target(STOP_TARGET)
                    .with_bounds(self.stop)
                    .with_state(AccessState::new().focusable(true)),
            ]);
        }
        Some(node)
    }

    #[must_use]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.transition.is_moving(now)
    }

    /// When "Stopped following" goes.
    #[must_use]
    pub fn next_wake_deadline(&self) -> Option<Instant> {
        self.stopped.and_then(|at| at.checked_add(STOPPED_FOR))
    }

    /// Paint the banner. `following` is the followed person's row while following, as the host
    /// knows it now. Returns `false` when following lost its person (they are gone from the
    /// roster): the host stops.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        place: BannerFrame,
        following: Option<Option<RosterRow>>,
    ) -> bool {
        let BannerFrame { cursor, motion, .. } = place;
        let now = ui::time::now();
        let stopped = self
            .stopped
            .filter(|at| now.saturating_duration_since(*at) < STOPPED_FOR);
        if stopped.is_none() {
            self.stopped = None;
        }
        if !self.transition.is_visible(now) && stopped.is_none() {
            self.chip = Rect::ZERO;
            self.stop = Rect::ZERO;
            self.shown = None;
            return true;
        }
        let is_following = following.is_some();
        if let Some(Some(row)) = following {
            self.shown = Some(row);
        }
        let Some(row) = self.shown.clone() else {
            self.chip = Rect::ZERO;
            self.stop = Rect::ZERO;
            return !is_following;
        };
        let label = if is_following {
            t_args("follow-label", &[("person", row.label.as_str().into())])
        } else {
            t("follow-stopped")
        };
        let openness = if is_following {
            ui::motion::unit_opacity(self.transition.progress(now))
        } else {
            stopped.map_or(0.0, |at| {
                Transient::opacity(
                    Transient::CONFIRMATION,
                    (at, at.checked_add(STOPPED_FOR).unwrap_or(at)),
                    now,
                    motion,
                )
            })
        };
        // The frame says whose view this is, all the way round.
        if is_following {
            ui::presence::paint_follow_frame(
                scene,
                transform,
                place.work_area,
                row.color,
                openness,
                theme,
            );
        }
        let stop_label = t("follow-stop");
        let (chip, stop) = rects(text, theme, (&label, &stop_label), place, is_following);
        self.chip = chip;
        self.stop = if is_following { stop } else { Rect::ZERO };
        self.said = (label.clone(), stop_label.clone());
        // The bar comes down into the top bar it stands in.
        let pose = Appear::pose(
            f64::from(openness),
            Direction::of(is_following),
            chip,
            Point::new(chip.center().x, chip.y0),
            motion,
        );
        let placed = pose.applied_to(transform);
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            pose.opacity.min(openness),
            placed,
            &ui::widget::floating_surface::painted_bounds(chip, theme),
        );
        ui::widget::floating_surface::paint_card(scene, placed, chip, theme);
        paint_contents(
            &mut ui::paint::PaintCtx::new(scene, placed, text, theme),
            &row,
            (&label, &stop_label),
            (chip, stop),
            is_following.then_some(cursor).flatten(),
            is_following,
        );
        scene.pop_layer();
        true
    }
}

/// Where the bar and its Stop go: in the top bar's strip, centred, with their face, `label` and,
/// while following, Stop. It asks for a card's height and gives way to the bar's where the bar is
/// shorter, so it never reaches past the bar onto a window.
fn rects(
    text: &mut TextRenderer,
    theme: &Theme,
    (label, stop_label): (&str, &str),
    place: BannerFrame,
    following: bool,
) -> (Rect, Rect) {
    let spacing = &theme.tokens.spacing;
    let inset = f64::from(spacing.s1);
    // The height the bar has room for decides their face too: the bar's strip is shorter than a
    // card on every display.
    let height = CHIP_HEIGHT.min((place.bar.height() - 2.0 * inset).max(0.0));
    let face = face_side(theme, height);
    let label_style = theme.text(TextRole::Label).with_weight(TextWeight::Medium);
    let (label_w, _) = text.measure_style(label, None, &label_style);
    let (stop_w, _) = text.measure_style(stop_label, None, &theme.text(TextRole::Body));
    let pad = f64::from(spacing.s3);
    let stop_width = if following {
        f64::from(stop_w) + 2.0 * f64::from(spacing.s3)
    } else {
        0.0
    };
    let width = pad
        + face
        + f64::from(spacing.s2)
        + f64::from(label_w)
        + if following {
            f64::from(spacing.s3) + stop_width
        } else {
            0.0
        }
        + pad;
    let chip = docked_in(place.bar, (width, height), inset);
    // Stop keeps the bar's control height where the bar has it, and the banner's own where the bar
    // is shorter.
    let stop_h = (chip.height() - 6.0).clamp(16.0, 28.0);
    let stop = Rect::new(
        chip.x1 - pad - stop_width,
        chip.center().y - stop_h / 2.0,
        chip.x1 - pad,
        chip.center().y + stop_h / 2.0,
    );
    (chip, stop)
}

/// Where a bar of `size` docks: inside `bar`, centred, as long and as tall as the bar has room for.
/// The top bar stands above every window's content, so a bar inside it stands on nothing the person
/// reads, however narrow the display is.
fn docked_in(bar: Rect, size: (f64, f64), inset: f64) -> Rect {
    let (width, height) = size;
    let room_w = (bar.width() - 2.0 * inset).max(0.0);
    let room_h = (bar.height() - 2.0 * inset).max(0.0);
    Rect::from_center_size(bar.center(), (width.min(room_w), height.min(room_h)))
}

/// Their face in a banner `height` tall: what the strip leaves, never more than the inline avatar.
fn face_side(theme: &Theme, height: f64) -> f64 {
    (height - 8.0)
        .max(12.0)
        .min(f64::from(theme.tokens.metrics.avatar_inline))
}

/// The bar's contents: their face, `label` and, while following, Stop.
fn paint_contents(
    ctx: &mut ui::paint::PaintCtx<'_>,
    row: &RosterRow,
    (label, stop_label): (&str, &str),
    (chip, stop): (Rect, Rect),
    cursor: Option<Point>,
    following: bool,
) {
    let theme = ctx.theme;
    let spacing = &theme.tokens.spacing;
    let pad = f64::from(spacing.s3);
    // Their face takes what the bar leaves: the bar's own strip is shorter than a card.
    let face = face_side(theme, chip.height());
    let label_style = theme.text(TextRole::Label).with_weight(TextWeight::Medium);
    let (_, label_h) = ctx.text.measure_style(label, None, &label_style);
    let mut their_face = ui::presence::Face::new(&row.label, &row.id, row.color);
    their_face.is_agent = row.is_agent;
    ui::presence::FaceStack {
        side: face - 6.0,
        ..ui::presence::FaceStack::BAR
    }
    .paint(
        ctx,
        (chip.x0 + pad + face, chip.center().y),
        (&[their_face], 0),
        ui::widget::floating_surface::card_ground(theme),
    );
    ctx.text(
        label,
        &label_style,
        theme.tokens.colors.text_primary,
        Point::new(
            chip.x0 + pad + face + f64::from(spacing.s2),
            chip.center().y - f64::from(label_h) / 2.0,
        ),
        None,
    );
    if following {
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::new(stop, stop_label)
                .role(ButtonRole::Plain)
                .hovered(cursor.is_some_and(|p| stop.contains(p))),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topbar::topbar_height;
    use ui::kurbo::Rect;

    /// Where the banner's bar and Stop stand on a display `width` wide, its top bar the height the
    /// desktop gives it, the desk under it filled from edge to edge by a window's content.
    fn placed(width: f64, following: bool) -> (Rect, Rect, Rect, Rect, Vec<Rect>) {
        let theme = Theme::default();
        let mut text = TextRenderer::new();
        let bar = Rect::new(0.0, 0.0, width, f64::from(topbar_height(&theme)));
        let work_area = Rect::new(0.0, bar.y1, width, bar.y1 + 760.0);
        let label = t_args("follow-label", &[("person", "Claude Code".into())]);
        let stop_label = t("follow-stop");
        let (chip, stop) = rects(
            &mut text,
            &theme,
            (&label, &stop_label),
            BannerFrame {
                work_area,
                bar,
                cursor: None,
                motion: theme.motion(),
            },
            following,
        );
        // A desk with a window filling it: its content starts where the bar ends.
        let windows = vec![Rect::new(0.0, work_area.y0, width, work_area.y1)];
        (bar, work_area, chip, stop, windows)
    }

    /// The banner stands in the top bar and on nothing else: no window's content lies under it, at
    /// a wide display or a phone's, and nothing of it reaches past the bar's ends either.
    #[test]
    fn the_pill_stands_on_no_window_content_at_any_width() {
        for width in [1280.0, 390.0, 360.0] {
            for following in [true, false] {
                let (bar, _work_area, chip, stop, windows) = placed(width, following);
                assert!(
                    chip.width() > 0.0 && chip.height() > 0.0,
                    "the banner stands at {width}: {chip:?}"
                );
                assert!(
                    chip.x0 >= bar.x0
                        && chip.x1 <= bar.x1
                        && chip.y0 >= bar.y0
                        && chip.y1 <= bar.y1,
                    "inside the bar at {width} ({following}): {chip:?} in {bar:?}"
                );
                for window in &windows {
                    assert_eq!(
                        window.intersect(chip).area(),
                        0.0,
                        "covering no content at {width} ({following}): {chip:?} over {window:?}"
                    );
                    assert_eq!(
                        window.intersect(stop).area(),
                        0.0,
                        "Stop covering no content at {width} ({following})"
                    );
                }
                if following {
                    assert!(
                        stop.x1 <= chip.x1 && stop.x0 >= chip.x0,
                        "Stop inside the banner at {width}"
                    );
                } else {
                    assert!(stop.is_zero_area(), "no Stop while stopped at {width}");
                }
            }
        }
    }

    /// A display narrower than the words still keeps the banner inside its bar: it gives way to the
    /// bar's width rather than reaching past it onto what the person reads.
    #[test]
    fn a_bar_narrower_than_its_words_keeps_them_inside_it() {
        let (bar, _work_area, chip, _stop, windows) = placed(160.0, true);
        assert!(
            chip.x0 >= bar.x0 && chip.x1 <= bar.x1,
            "{chip:?} in {bar:?}"
        );
        for window in &windows {
            assert_eq!(window.intersect(chip).area(), 0.0);
        }
    }
}
