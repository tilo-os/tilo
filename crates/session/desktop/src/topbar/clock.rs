//! The bar's centre lane and the clock: the time it reads at the grain the visible activity needs,
//! the pill an activity shows there, the width the band takes and when its frame is stale.

use super::{
    Affine, BAR_CONTROL_PX, CENTER_INSET_PX, ClockSnapshot, Duration, Instant, NARROW_MENU_BAR_PX,
    Rect, Scene, TextRenderer, TextRole, TextStyle, TextWeight, Theme, TopBarChrome,
    TopBarPresence,
};

/// How far apart the dots after an activity's reading stand: one dot per activity waiting behind
/// the pill's own, measured into the pill's width.
const DOT_PITCH: f64 = 5.0;
/// The thumbnail a song's pill heads its title with, square.
const PILL_ART: f64 = 14.0;
/// Between the thumbnail and the song's title.
const PILL_ART_GAP: f64 = 6.0;

impl TopBarChrome {
    /// The centre selector is visually anchored at the viewport centre, but its wide-state width
    /// must not consume the entire narrow bar. Keep the established 360 px treatment on
    /// laptop-sized and larger viewports and release the reclaimed lane to the workspace identity
    /// below that point. The lower bound still leaves enough room for the clock and its three
    /// position dots at the large-text setting.
    pub(super) fn center_band_width(&self, theme: &Theme, text: &mut TextRenderer) -> f64 {
        // While this person follows someone the lane is the banner's, at the width the banner asks
        // for it: the clock yields the seat rather than standing under a card about someone else.
        if self.following {
            return crate::follow_banner::lane_width(self.rect.width());
        }
        if self.clock_gives_way() {
            return 0.0;
        }
        let text_headroom = (f64::from(theme.text_scale) - 1.0).max(0.0) * 16.0;
        if self.rect.width() < NARROW_MENU_BAR_PX {
            // A phone-wide bar keeps the time and gives the rest of the lane to the workspace name
            // and the people beside it. A live activity's pill stands on its own reading, so the
            // lane grows around it and the name beside it yields: the selector's lift stays behind
            // the text it belongs to.
            let clock = 64.0 + text_headroom;
            if self.center_pill().is_empty() {
                return clock;
            }
            let reading = self.pill_reading_width(theme, text);
            return reading.max(clock).min(self.rect.width() / 2.0);
        }
        let min_width = (220.0 + text_headroom).min(260.0);
        (self.rect.width() - 580.0).clamp(min_width, 360.0)
    }

    /// Whether the bar's middle lane stands empty: a phone-wide bar on a device that shows the
    /// time itself, with no live activity to show there. The time would be said twice, and the
    /// workspace name beside it cut to a few letters.
    pub(super) fn clock_gives_way(&self) -> bool {
        self.device_clock && !self.following && self.phone_wide() && self.center_pill().is_empty()
    }

    pub(super) fn live_clock(&self) -> ClockSnapshot {
        self.chrome
            .as_ref()
            .map_or_else(|| self.clock.clone(), |c| c.snapshot().clock)
    }

    /// The host's clock reading, already formatted — and pinned by the host when a screenshot
    /// harness needs every frame to hold still.
    pub(super) fn current_clock_string(&self) -> String {
        self.live_clock().clock
    }

    /// Bold, so it reads as the chrome's focal glance value.
    pub fn clock_text_weight(&self) -> TextWeight {
        TextWeight::Bold
    }

    /// The centre band. The clock is plain text on the bar, with no pill
    /// (a pill around five characters read as a second bar inside the bar);
    /// a live activity over it keeps a quiet selector pill. Returns the
    /// centre line the time or the activity draws on.
    pub(super) fn draw_center_band(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
    ) -> f64 {
        let lane_w = self.center_band_width(theme, text_renderer);
        let target_w = self.center_pill_width(theme, text_renderer, lane_w);
        let now = ui::time::now();
        if (self.center_lane.width() - lane_w).abs() > 0.5 {
            // A new lane (the bar's first paint, a resized display): the selector takes its width
            // as it is.
            self.center_width.snap(target_w);
        } else if (self.center_width.target() - target_w).abs() > 0.25 {
            self.center_width.follow(
                target_w,
                theme.motion(),
                ui::tokens::MotionSource::Pointer,
                now,
            );
        }
        let band_w = self.center_width.get(now).clamp(0.0, lane_w);
        // The same height as every other bar control, so the band's top and bottom edges line up
        // with the pills either side of it.
        let band_h = BAR_CONTROL_PX;
        let band_cx = f64::midpoint(self.rect.x0, self.rect.x1);
        let band = Rect::new(
            band_cx - band_w / 2.0,
            center_y - band_h / 2.0,
            band_cx + band_w / 2.0,
            center_y + band_h / 2.0,
        );
        self.center_rect = band;
        self.center_lane = Rect::new(
            band_cx - lane_w / 2.0,
            band.y0,
            band_cx + lane_w / 2.0,
            band.y1,
        );
        // The quiet selector pill comes and goes with the activity: an activity's pill, or a
        // glance's sentence, always stands on one.
        let fade = self.center_activity_opacity(now);
        let pill = if self.center_pill().is_empty() {
            fade
        } else {
            1.0
        };
        if pill > 0.0 {
            if pill < 1.0 {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    pill,
                    transform,
                    &band.inflate(2.0, 2.0),
                );
            }
            // The live activity keeps a quiet selector pill: the bar control's own whole lift, from
            // its face.
            ui::widget::BarControlFace::new("").open(true).paint_lift(
                scene,
                transform,
                band,
                theme,
                theme.tokens.colors.surfaces.base,
            );
            if pill < 1.0 {
                scene.pop_layer();
            }
        }
        let (content_x0, content_x1) = Self::center_content_lane(band);
        f64::midpoint(content_x0, content_x1)
    }

    /// The clock with nothing live over it: drawn as bare text.
    #[cfg(test)]
    pub(super) fn center_is_plain_text(&self) -> bool {
        !self.activity_is_visible() && self.center_pill().is_empty()
    }

    /// How strongly the live activity (and its pill) shows at `now` , as it cross-fades with the
    /// time.
    pub(super) fn center_activity_opacity(&self, now: Instant) -> f32 {
        if *self.center_fade.shown() {
            return self.center_fade.incoming(now);
        }
        self.center_fade
            .outgoing(now)
            .filter(|(activity, _)| **activity)
            .map_or(0.0, |(_, opacity)| opacity)
    }

    /// The lane the active centre widget draws into: the same inset in from either end of the band.
    /// One function so [`Self::draw_center_band`] and [`Self::activity_content_width`] cannot
    /// disagree about how much workspace the activity headline has.
    pub(super) fn center_content_lane(band: Rect) -> (f64, f64) {
        // Symmetric, so the content's centre is the band's centre.
        (band.x0 + CENTER_INSET_PX, band.x1 - CENTER_INSET_PX)
    }

    /// The date, said beside the time to assistive technology.
    pub(super) fn date_description(&self) -> String {
        let clock = self.live_clock();
        format!("{} {}", clock.weekday, clock.date)
            .trim()
            .to_owned()
    }

    /// The pill the centre lane shows: the most urgent activity's reading,
    /// and how many more wait behind it, as the session ordered them.
    pub(super) fn center_pill(&self) -> ui::floating_layer::Pill {
        self.chrome
            .as_ref()
            .map(|chrome| chrome.snapshot().activities)
            .unwrap_or_default()
    }

    /// What the clock widget reads: the time, or the pill of the activity running over it.
    pub(super) fn clock_widget_text(&self) -> String {
        let pill = self.center_pill();
        if pill.is_empty() {
            self.current_clock_string()
        } else {
            pill.reading()
        }
    }

    /// The width of the centre widget inside its `lane_w` lane. A glance's sentence fills the lane;
    /// an activity's pill and the clock hug their reading, so the band is a pill and not a second
    /// bar inside the bar. The reading is measured with every digit set as the widest one, so the
    /// target does not change width second to second.
    pub(super) fn center_pill_width(
        &self,
        theme: &Theme,
        text: &mut TextRenderer,
        lane_w: f64,
    ) -> f64 {
        if self.activity_is_visible() {
            return lane_w;
        }
        self.pill_reading_width(theme, text).min(lane_w)
    }

    /// The width a pill needs around its reading: the reading at the weight it draws at, the dots
    /// of the activities waiting behind it, and the selector's inset on either side. The reading is
    /// measured with every digit set as the widest one, so the target does not change width second
    /// to second.
    pub(super) fn pill_reading_width(&self, theme: &Theme, text: &mut TextRenderer) -> f64 {
        let style = theme
            .tokens
            .typography
            .style(TextRole::Body)
            .with_weight(self.clock_text_weight());
        let widest_digit = ('0'..='9')
            .map(|digit| {
                (
                    text.measure_style(&digit.to_string(), None, &style).0,
                    digit,
                )
            })
            .fold((0.0_f32, '0'), |widest, next| {
                if next.0 > widest.0 { next } else { widest }
            })
            .1;
        let reading: String = self
            .clock_widget_text()
            .chars()
            .map(|c| if c.is_ascii_digit() { widest_digit } else { c })
            .collect();
        let (width, _) = text.measure_style(&reading, None, &style);
        // The dots behind the reading and a song's thumbnail in front of it are part of the pill
        // too.
        let pill = self.center_pill();
        let dots = ui::num::f64_of_usize(pill.more) * DOT_PITCH;
        let art = pill.artwork().map_or(0.0, |_| PILL_ART + PILL_ART_GAP);
        f64::from(width) + art + dots + 2.0 * CENTER_INSET_PX
    }

    pub(super) fn draw_clock_widget(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        content_cx: f64,
    ) {
        use ui::widget::TextLineFace;
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let pill = self.center_pill();
        let s = self.clock_widget_text();
        let style = tokens.typography.style(TextRole::Body);
        // Measured at the draw weight — Bold advances wider than Regular, so centring drifts
        // otherwise. One centred line, through the shared text-line painter.
        let line =
            TextLineFace::styled(&s, &style, TextRole::Body).weight(self.clock_text_weight());
        let (tw, th) = line.measure(text_renderer);
        // An activity's own ink marks the pill: its tone, or the reading's own colour.
        let ink = if pill.is_empty() {
            colors.text_primary
        } else {
            pill.ink(theme)
        };
        // Extra activities stand behind the reading as dots, so a person reads at a glance that
        // more than one is running, and a song shows its cover in front of its title.
        let dots = ui::num::f64_of_usize(pill.more) * DOT_PITCH;
        let art = pill.artwork().map_or(0.0, |_| PILL_ART + PILL_ART_GAP);
        let tx = content_cx - f64::midpoint(tw + art, dots);
        if let Some(picture) = pill.artwork() {
            let rect =
                Rect::from_center_size((tx + PILL_ART / 2.0, center_y), (PILL_ART, PILL_ART));
            paint_pill_art(scene, transform, theme, rect, &picture);
        }
        let tx = tx + art;
        let ty = center_y - th / 2.0;
        let rect = Rect::new(tx, ty, tx + tw, ty + th);
        line.paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            rect,
            ink,
        );
        for dot in 0..pill.more {
            let x = tx + tw + 2.0 + f64::from(u32::try_from(dot).unwrap_or(u32::MAX)) * DOT_PITCH;
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                colors.text_dim,
                None,
                &Rect::new(x, center_y - 1.5, x + 3.0, center_y + 1.5),
            );
        }
    }

    /// Sizes the center band, then draws the time, or a live activity over it. Split out of
    /// [`Widget::draw`] to stay under clippy's line budget.
    pub(super) fn draw_center_widget(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
    ) {
        if self.following {
            // The banner that says whose view this is stands in this lane, in the frame drawn over
            // the bar: the lane is kept, and nothing of the bar's own is drawn in it.
            let lane = crate::follow_banner::lane_width(self.rect.width());
            let center = f64::midpoint(self.rect.x0, self.rect.x1);
            let half = BAR_CONTROL_PX / 2.0;
            self.center_rect = Rect::ZERO;
            self.center_lane = Rect::new(
                center - lane / 2.0,
                center_y - half,
                center + lane / 2.0,
                center_y + half,
            );
            return;
        }
        if self.clock_gives_way() {
            // Nothing stands in the lane, and nothing points at it.
            let center = f64::midpoint(self.rect.x0, self.rect.x1);
            self.center_rect = Rect::ZERO;
            self.center_lane = Rect::new(center, center_y, center, center_y);
            return;
        }
        let content_cx = self.draw_center_band(scene, transform, theme, text_renderer, center_y);
        // The time and a live activity cross-fade in place.
        let now = ui::time::now();
        let mut layers = Vec::with_capacity(2);
        if let Some((activity, opacity)) = self.center_fade.outgoing(now) {
            layers.push((*activity, opacity));
        }
        layers.push((*self.center_fade.shown(), self.center_fade.incoming(now)));
        for (activity, opacity) in layers {
            let layered = opacity < 1.0;
            if layered {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    opacity,
                    transform,
                    &self.center_lane,
                );
            }
            if activity {
                self.draw_activity_widget(
                    scene,
                    transform,
                    theme,
                    text_renderer,
                    center_y,
                    content_cx,
                );
            } else {
                self.draw_clock_widget(
                    scene,
                    transform,
                    theme,
                    text_renderer,
                    center_y,
                    content_cx,
                );
            }
            if layered {
                scene.pop_layer();
            }
        }
    }

    /// The centre's cross-fade as the cache sees it: its progress in thousandths while it runs, so
    /// each of its frames draws anew.
    pub(super) fn center_fade_key(&self) -> Option<u16> {
        let now = ui::time::now();
        self.center_fade.is_moving(now).then(|| {
            let permille = (self.center_fade.incoming(now) * 1000.0)
                .round()
                .clamp(0.0, 1000.0);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to 0..=1000 just above"
            )]
            let permille = permille as u16;
            permille
        })
    }

    /// The centre selector's width as the cache sees it while it morphs: in quarter pixels, so each
    /// of its frames draws anew.
    pub(super) fn center_width_key(&self) -> Option<u32> {
        let now = ui::time::now();
        self.center_width.is_moving(now).then(|| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a bar's width in quarter pixels, clamped non-negative"
            )]
            let quarters = (self.center_width.get(now) * 4.0).round().max(0.0) as u32;
            quarters
        })
    }

    /// Whether the centre is moving: the time and an activity cross-fading,
    /// or the selector morphing between their widths.
    pub(super) fn center_is_moving(&self) -> bool {
        let now = ui::motion::frame_now();
        self.center_fade.is_moving(now) || self.center_width.is_moving(now)
    }

    /// When the centre's motion settles, if it is moving.
    #[cfg(test)]
    pub(super) fn center_settles_at(&self) -> Option<Instant> {
        self.center_fade
            .settles_at()
            .into_iter()
            .chain(self.center_width.settles_at())
            .max()
    }

    /// Whether the centre on screen is not the centre as it is now: the cached frame was painted
    /// part-way through a cross-fade that has since settled, or the activity came or went since.
    /// Without this the last frame painted while the fade moved stays up, the time and the activity
    /// both at part opacity, because nothing else asks to paint once the motion has settled.
    pub(super) fn center_frame_stale(&self) -> bool {
        // A bar that paints nothing has no centre on screen to go stale.
        if self.presence() == TopBarPresence::Hidden
            || self.rect.width() <= 0.0
            || self.rect.height() <= 0.0
        {
            return false;
        }
        self.center_fade_key() != self.cache_fp.center_fade
            || self.center_width_key() != self.cache_fp.center_width
            || self.activity_is_visible() != self.cache_fp.activity_visible
    }

    pub(super) fn activity_is_visible(&self) -> bool {
        self.activity_headline.is_some()
            && self.activity_started.is_some_and(|started| {
                ui::time::now().saturating_duration_since(started) < Duration::from_secs(6)
            })
    }

    pub(super) fn draw_activity_widget(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        center_x: f64,
    ) {
        use ui::widget::TextLineFace;
        let Some(_headline) = self.activity_headline.as_deref() else {
            return;
        };
        let style = theme.text(TextRole::Caption);
        // Keep activity in one line. `measure(..., Some(width))` wraps at word boundaries, which
        // made a large-text notification grow to two lines inside the fixed-height selector and
        // clip into the bar edge. The dots reserve the first three pitches of the band; the
        // trailing inset reserves the right breathing room. Elision preserves that geometry at
        // every text scale and viewport width.
        let max_width = self.activity_content_width(theme);
        let label = self.activity_display_label(text_renderer, &style, max_width);
        let line =
            TextLineFace::styled(&label, &style, TextRole::Caption).weight(TextWeight::Medium);
        let (w, h) = line.measure(text_renderer);
        let rect = Rect::new(
            center_x - w / 2.0,
            center_y - h / 2.0,
            center_x + w / 2.0,
            center_y + h / 2.0,
        );
        line.paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            rect,
            theme.tokens.colors.text_primary,
        );
    }

    /// Width of the centre lane the activity headline may occupy — the band minus its insets,
    /// exactly as [`Self::draw_center_band`] lays it out.
    pub(super) fn activity_content_width(&self, theme: &Theme) -> f32 {
        let (x0, x1) = Self::center_content_lane(self.center_rect);
        ui::num::f32_of_f64((x1 - x0).max(f64::from(theme.tokens.spacing.s12)))
    }

    pub(super) fn activity_display_label(
        &self,
        text_renderer: &mut TextRenderer,
        style: &TextStyle,
        max_width: f32,
    ) -> String {
        let Some(headline) = self.activity_headline.as_deref() else {
            return String::new();
        };
        // The headline alone: the unread count is the badge's, never joined into the sentence, and
        // clicking the pill opens the activity.
        let medium = style.clone().with_weight(TextWeight::Medium);
        let (full_width, _) = text_renderer.measure_style(headline, None, &medium);
        if full_width <= max_width {
            return headline.to_string();
        }
        // Aggregate activity sometimes lists its examples after a colon (the workspaces just
        // recorded). Keep the whole event clause rather than cutting part-way through the examples.
        if let Some((summary, _examples)) = headline.split_once(':') {
            let (compact_width, _) = text_renderer.measure_style(summary, None, &medium);
            if compact_width <= max_width {
                return summary.to_string();
            }
        }
        text_renderer.elide_to_width_style(headline, max_width, &medium)
    }
}

/// The thumbnail a song's pill heads its title with: its cover, drawn by the image store's own tile
/// so the bar's picture is cut and rounded as every other cover is. A cover the store has not read
/// yet leaves the slot it keeps, so the title never jumps once it lands.
fn paint_pill_art(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    rect: Rect,
    picture: &ui::widget::PictureSource,
) {
    let Some(image) = ui::pictures::Pictures::window().get(picture) else {
        return;
    };
    ui::image_store::paint_image_cover(
        scene,
        transform,
        rect,
        &image,
        f64::from(theme.tokens.radii.control),
    );
}

/// Seconds the centre band's time reading stays the same for: the clock paints `HH:MM` , a live
/// activity counts seconds.
pub(super) fn time_bucket_secs(activity_visible: bool) -> u64 {
    if activity_visible { 1 } else { 60 }
}

/// How long from `since_epoch` (wall time) until the next whole multiple of `bucket_secs` seconds,
/// when a reading of that grain changes.
pub(super) fn until_next_time_bucket(since_epoch: Duration, bucket_secs: u64) -> Duration {
    let bucket = Duration::from_secs(bucket_secs.max(1));
    let into = Duration::from_nanos(
        since_epoch
            .as_nanos()
            .checked_rem(bucket.as_nanos())
            .and_then(|into| u64::try_from(into).ok())
            .unwrap_or_default(),
    );
    bucket.saturating_sub(into)
}
