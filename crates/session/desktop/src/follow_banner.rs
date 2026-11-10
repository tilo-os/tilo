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
use ui::icons::Icon;
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

/// The bar's own middle lane: the width the clock takes at rest, and the banner's while following.
/// A view that is not the person's own is said where the bar's glance value stands, so the clock
/// keeps out of its way while it is said.
#[must_use]
pub fn lane_width(bar_width: f64) -> f64 {
    if bar_width < NARROW_BAR_PX {
        FOLDED_LANE_PX
    } else {
        (bar_width - WIDE_ASIDE_PX).clamp(WIDE_LANE_MIN_PX, WIDE_LANE_MAX_PX)
    }
}

/// Whether the bar across `bar_width` has no room for the banner's card, which names the person
/// and offers Stop in words: the banner then folds into their face, with Stop beside it.
#[must_use]
pub fn folds(bar_width: f64) -> bool {
    lane_width(bar_width) < CARD_MIN_PX
}

/// Below this the bar is a phone's: its middle lane is the time alone, and the banner's with it.
const NARROW_BAR_PX: f64 = 720.0;
/// The middle lane of a phone-wide bar: their face and Stop, side by side.
const FOLDED_LANE_PX: f64 = 64.0;
/// What a wide bar keeps for its own ends, either side of the middle lane.
const WIDE_ASIDE_PX: f64 = 580.0;
/// A wide bar's middle lane, at least the time and its dots read at a large text size and never so
/// long that the workspace's name starves.
const WIDE_LANE_MIN_PX: f64 = 220.0;
/// See [`WIDE_LANE_MIN_PX`].
const WIDE_LANE_MAX_PX: f64 = 360.0;
/// The narrowest lane the card's words and Stop stand in: below it the banner folds, since a card
/// elided to less says less than their face and one word.
const CARD_MIN_PX: f64 = 180.0;

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
        let card = rects(text, theme, (&label, &stop_label), place, is_following);
        let chip = card.chip;
        self.chip = chip;
        self.stop = if is_following { card.stop } else { Rect::ZERO };
        self.said = (label, stop_label);
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
            &card,
            is_following.then_some(cursor).flatten(),
        );
        scene.pop_layer();
        true
    }
}

/// Where the bar and its Stop go: in the top bar's own middle lane, with their face, `label` and,
/// while following, Stop. The lane is the clock's seat, so the card never reaches a control of the
/// bar's: where the lane is too narrow for the words, the banner folds into their face with Stop
/// beside it, and the words it cannot show are the ones a reader is told instead.
///
/// Answers the card as it stands, its label elided to the room.
pub(crate) fn rects(
    text: &mut TextRenderer,
    theme: &Theme,
    (label, stop_label): (&str, &str),
    place: BannerFrame,
    following: bool,
) -> Card {
    let folded = folds(place.bar.width());
    let spacing = &theme.tokens.spacing;
    let inset = f64::from(spacing.s1);
    // The height the bar has room for decides their face too: the bar's strip is shorter than a
    // card on every display.
    let height = CHIP_HEIGHT.min((place.bar.height() - 2.0 * inset).max(0.0));
    let face = face_side(theme, height);
    let label_style = theme.text(TextRole::Label).with_weight(TextWeight::Medium);
    let (stop_w, _) = text.measure_style(stop_label, None, &theme.text(TextRole::Body));
    let stop_width = if following {
        f64::from(stop_w) + 2.0 * f64::from(spacing.s3)
    } else {
        0.0
    };
    // The lane the bar keeps for this banner: the card stands in it and no wider, so it never
    // reaches a control either side.
    let lane = lane_width(place.bar.width());
    if folded {
        let pad = f64::from(spacing.s1);
        // Stop is a square as tall as their face: the one word the bar can hold, as the mark that
        // ends following, beside the face whose view this is.
        let stop_side = face;
        let width = if following {
            2.0 * pad + face + f64::from(spacing.s2) + stop_side
        } else {
            2.0 * pad + face
        };
        let chip = docked_in(place.bar, (width.min(lane), height), inset);
        let stop = if following {
            Rect::from_center_size(
                Point::new(chip.x1 - pad - stop_side / 2.0, chip.center().y),
                (stop_side, stop_side),
            )
        } else {
            Rect::ZERO
        };
        return Card {
            label: String::new(),
            stop_label: stop_label.to_owned(),
            chip,
            stop,
            folded,
            following,
        };
    }
    let pad = f64::from(spacing.s3);
    let room_for_label = (lane
        - 2.0 * pad
        - face
        - f64::from(spacing.s2)
        - if following {
            f64::from(spacing.s3) + stop_width
        } else {
            0.0
        })
    .max(0.0);
    let shown = text.elide_to_width_style(label, ui::num::f32_of_f64(room_for_label), &label_style);
    let (label_w, _) = text.measure_style(&shown, None, &label_style);
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
    let chip = docked_in(place.bar, (width.min(lane), height), inset);
    // Stop keeps the bar's control height where the bar has it, and the banner's own where the bar
    // is shorter.
    let stop_h = (chip.height() - 6.0).clamp(16.0, 28.0);
    let stop = Rect::new(
        chip.x1 - pad - stop_width,
        chip.center().y - stop_h / 2.0,
        chip.x1 - pad,
        chip.center().y + stop_h / 2.0,
    );
    Card {
        label: shown,
        stop_label: stop_label.to_owned(),
        chip,
        stop,
        folded,
        following,
    }
}

/// The banner's card as [`rects`] placed it: the words it shows, their face's seat and Stop's.
#[derive(Clone, Debug)]
pub(crate) struct Card {
    /// The label as painted, elided to the room; empty where the banner folds.
    pub(crate) label: String,
    /// Stop's own words, which a reader is told even where the card has no room for them.
    pub(crate) stop_label: String,
    pub(crate) chip: Rect,
    pub(crate) stop: Rect,
    /// Their face and Stop alone: the lane is too narrow for the words.
    pub(crate) folded: bool,
    /// Whether the card names someone this person follows, so Stop stands on it.
    pub(crate) following: bool,
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

/// The bar's contents: their face, the card's label and, while following, Stop.
fn paint_contents(
    ctx: &mut ui::paint::PaintCtx<'_>,
    row: &RosterRow,
    card: &Card,
    cursor: Option<Point>,
) {
    let Card {
        label,
        stop_label,
        chip,
        stop,
        folded,
        following,
    } = card;
    let theme = ctx.theme;
    let spacing = &theme.tokens.spacing;
    let pad = if *folded {
        f64::from(spacing.s1)
    } else {
        f64::from(spacing.s3)
    };
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
    if !folded {
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
    }
    if !following {
        return;
    }
    let hovered = cursor.is_some_and(|p| stop.contains(p));
    if *folded {
        // Folded, Stop has only its mark to say it: a square as tall as their face, named to a
        // reader who is told whose view this is with the words the card could not show.
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::icon_only(*stop, Icon::Square, stop_label).hovered(hovered),
        );
    } else {
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::new(*stop, stop_label)
                .role(ButtonRole::Plain)
                .hovered(hovered),
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
        let card = rects(
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
        let (chip, stop) = (card.chip, card.stop);
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

    /// Where the bar is a phone's, the banner folds into the middle lane's own width: their face
    /// and Stop stand there, and the bar's people keep the room either side.
    #[test]
    fn a_phone_wide_bar_folds_the_banner_into_its_lane() {
        for width in [390.0, 360.0, 320.0] {
            let (bar, _work_area, chip, stop, _windows) = placed(width, true);
            let theme = Theme::default();
            let lane = Rect::from_center_size(bar.center(), (lane_width(width), chip.height()));
            assert!(folds(width), "a phone-wide bar folds at {width}");
            assert!(
                chip.width() <= lane.width() + 0.5,
                "the folded banner fits the lane at {width}: {chip:?} in {lane:?}"
            );
            assert!(
                chip.x0 >= lane.x0 - 0.5 && chip.x1 <= lane.x1 + 0.5,
                "and stands in it at {width}: {chip:?} in {lane:?}"
            );
            assert!(
                stop.x0 >= chip.x0 && stop.x1 <= chip.x1 && !stop.is_zero_area(),
                "Stop stands in the folded banner at {width}"
            );
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
