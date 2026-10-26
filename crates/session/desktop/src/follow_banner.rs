//! The "Following Ada" banner: what says, while this person's view tracks
//! someone else's, whose view it is and how to stop.
//!
//! A 2 px frame in their colour runs round the work area, and a small bar
//! hangs in the transients' zone under the top bar (never on a window's
//! bar) with their face, "Following Ada" and Stop. When the person's own
//! input ends it, the bar says "Stopped following" for a moment. The
//! native shell and the browser paint the same banner; each host keeps its
//! own follow state and tells the banner when following starts and ends.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::TextRole;
use ui::motion::{Appear, Presence, Transient};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::tokens::{EffectiveMotion, MotionSource};
use ui::widget::{Button, ButtonFace, ButtonRole};

use crate::collaborators::RosterRow;
use crate::i18n::{t, t_args};

/// How long "Stopped following" stays after the person's own input ended
/// following.
pub const STOPPED_FOR: Duration = Duration::from_secs(1);

/// Where the banner may go this frame, and how it moves.
#[derive(Clone, Copy, Debug)]
pub struct BannerFrame<'a> {
    /// The work area the frame runs round: the screen under the top bar.
    pub work_area: Rect,
    /// The transients' zone the bar hangs in.
    pub viewport: Rect,
    /// Window bars the bar keeps off.
    pub window_bars: &'a [Rect],
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
}

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

    /// Following ended; `by_own_input` when the person took their view
    /// back by doing something of their own (the bar says so a moment).
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

    #[must_use]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.transition.is_moving(now)
    }

    /// When "Stopped following" goes.
    #[must_use]
    pub fn next_wake_deadline(&self) -> Option<Instant> {
        self.stopped.and_then(|at| at.checked_add(STOPPED_FOR))
    }

    /// Paint the banner. `following` is the followed person's row while
    /// following, as the host knows it now. Returns `false` when following
    /// lost its person (they are gone from the roster): the host stops.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        place: BannerFrame<'_>,
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
        // The bar comes down from the top bar it hangs under.
        let pose = Appear::pose(
            f64::from(openness),
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

/// Where the bar and its Stop go: the transients' slot in the viewport,
/// below any window bar it would cover. Their face, `label` and, while
/// following, Stop.
fn rects(
    text: &mut TextRenderer,
    theme: &Theme,
    (label, stop_label): (&str, &str),
    place: BannerFrame<'_>,
    following: bool,
) -> (Rect, Rect) {
    let spacing = &theme.tokens.spacing;
    let face = f64::from(theme.tokens.metrics.avatar_inline);
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
    let chip = crate::notifications::transient_slot(
        place.viewport,
        place.viewport.y0,
        (width, 40.0),
        place.window_bars,
    );
    let stop = Rect::new(
        chip.x1 - pad - stop_width,
        chip.center().y - 14.0,
        chip.x1 - pad,
        chip.center().y + 14.0,
    );
    (chip, stop)
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
    let face = f64::from(theme.tokens.metrics.avatar_inline);
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
