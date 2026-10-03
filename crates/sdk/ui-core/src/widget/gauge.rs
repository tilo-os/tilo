//! Gauges: [`ProgressRing`] (a countdown or progress ring with a centred
//! readout) and [`BarGauge`] (a labelled horizontal bar).
//!
//! The ring sweeps clockwise from 12 o'clock (`-π/2`) with a round-capped
//! arc over a track that registers against the ground and leaves the fill
//! at least 3:1 against it. Its fill is progress (the inverted fill), a
//! measured level that turns warning and danger past [`GaugeThresholds`],
//! or a [`Tone`]. The bar fills a rounded track left to right. Every colour
//! and size comes from the [`Theme`].

use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, TAU};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Arc, Cap, Circle, Join, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{Brush, Color, Fill};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::tokens::colors::Colors;
use crate::widget::TaffyTreeExt;
use crate::widget::{Tone, Widget};

/// Alpha of a gauge's unfilled track, derived against the raised surface
/// a gauge sits on (HIG §9.5): a declared border at any alpha is three
/// shades off that surface on the dark palette and the track vanishes.
const TRACK_ALPHA: u8 = 40;
const BAR_TRACK_H: f64 = 8.0;

/// The unfilled track colour on the theme's raised surface.
fn track_color(theme: &Theme, alpha: u8) -> Color {
    crate::widget::data_table::tier_on(theme.tokens.colors.surfaces.raised, alpha)
}

/// Paint options of a [`BarGauge`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GaugeStyle {
    /// Alpha of the unfilled track against the raised surface. Denser
    /// dashboards raise it so a thin track still reads next to its fill.
    pub track_alpha: u8,
    /// Set a bar gauge's label and value at the `xs` step instead of `sm`,
    /// for a dense grid of small gauges.
    pub compact_labels: bool,
}

impl Default for GaugeStyle {
    fn default() -> Self {
        Self {
            track_alpha: TRACK_ALPHA,
            compact_labels: false,
        }
    }
}

impl GaugeStyle {
    #[must_use]
    pub fn with_track_alpha(mut self, alpha: u8) -> Self {
        self.track_alpha = alpha;
        self
    }

    #[must_use]
    pub fn compact(mut self) -> Self {
        self.compact_labels = true;
        self
    }
}

/// What an immediate-mode gauge paints: how full it is, in which colour,
/// and the text around it. Either string may be empty.
#[derive(Clone, Copy, Debug)]
pub struct GaugeReading<'a> {
    /// Fill fraction, clamped to `0..1` when painted.
    pub frac: f32,
    pub color: Color,
    /// What is measured — a ring's dim caption, a bar's top-left label.
    pub label: &'a str,
    /// The readout — a ring's prominent centre text, a bar's right-aligned value.
    pub value: &'a str,
}

/// Rounds a `0..=1` fraction to a whole percentage for the accessibility
/// value string. Always within `0..=100`, far inside `i64` range.
#[allow(
    clippy::cast_possible_truncation,
    reason = "frac is clamped to 0..1 immediately above, so the percentage is always within 0..=100"
)]
fn pct(frac: f32) -> i64 {
    (frac.clamp(0.0, 1.0) * 100.0).round() as i64
}

/// Fraction thresholds (inclusive lower bounds) at which a gauge switches
/// from success → warning → danger. The default mirrors the dashboard
/// convention: `>= 0.85` danger, `>= 0.6` warning, else success.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GaugeThresholds {
    pub warning: f32,
    pub danger: f32,
}

impl Default for GaugeThresholds {
    fn default() -> Self {
        Self {
            warning: 0.6,
            danger: 0.85,
        }
    }
}

impl GaugeThresholds {
    pub fn color(&self, theme: &Theme, frac: f32) -> Color {
        let s = &theme.tokens.colors.semantic;
        if frac >= self.danger {
            s.danger
        } else if frac >= self.warning {
            s.warning
        } else {
            s.success
        }
    }
}

/// How a gauge sources its fill colour.
#[derive(Clone, Copy, Debug)]
enum Fillcolor {
    /// Resolve via [`GaugeThresholds`] against the value (default).
    Threshold(GaugeThresholds),
    /// A fixed caller-supplied colour, ignoring thresholds.
    Fixed(Color),
}

/// How large a [`ProgressRing`] is. Each size reads from the theme's
/// metrics, so it follows the UI scale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProgressRingSize {
    /// Inline beside a line of text, without a readout (a one-time code's
    /// countdown, a row's upload).
    Small,
    /// A readout inside the ring: the value and its caption.
    #[default]
    Regular,
    /// The one hero readout of a screen (a speed test, a flash in progress).
    Large,
}

impl ProgressRingSize {
    /// The ring's diameter under `theme`. A ring with a readout grows with
    /// the text scale so its value never elides.
    #[must_use]
    pub fn diameter(self, theme: &Theme) -> f64 {
        let metrics = &theme.tokens.metrics;
        let growth = f64::from(theme.text_growth());
        match self {
            Self::Small => f64::from(metrics.icon_lg),
            Self::Regular => f64::from(metrics.min_pointer_target) * 3.0 * growth,
            Self::Large => f64::from(metrics.min_pointer_target) * 5.0 * growth,
        }
    }

    fn thickness(self, theme: &Theme) -> f64 {
        let rule = f64::from(theme.tokens.metrics.chrome_rule_px);
        match self {
            Self::Small => rule * 3.0,
            Self::Regular => rule * 6.0,
            Self::Large => rule * 10.0,
        }
    }

    fn roles(self) -> Option<(TextRole, TextRole)> {
        match self {
            Self::Small => None,
            Self::Regular => Some((TextRole::Title, TextRole::Caption)),
            Self::Large => Some((TextRole::Display, TextRole::Label)),
        }
    }
}

/// What a ring's fill means.
#[derive(Clone, Copy, Debug, PartialEq)]
enum RingFill {
    /// Progress toward done: the inverted fill (the same as a progress bar).
    Progress,
    /// A measured level that turns warning and danger past `thresholds`.
    Level(GaugeThresholds),
    /// A status or a person.
    Tone(Tone),
}

/// The ring's unfilled track on `ground`: a derived step that registers
/// against the ground and leaves the fill at least 3:1 against it.
fn ring_track(ground: Color, fill: Color) -> Color {
    use crate::color::contrast_ratio;
    let mut track =
        crate::tokens::colors::composite(Colors::hairline_on(ground, RING_TRACK_ALPHA), ground);
    // Back the track off toward the ground while the fill would not clear
    // it, so a light fill on a light track still reads as progress.
    let mut alpha = RING_TRACK_ALPHA;
    while contrast_ratio(fill, track) < crate::tokens::colors::NON_TEXT_MIN_CONTRAST
        && alpha > RING_TRACK_MIN_ALPHA
    {
        alpha = alpha.saturating_sub(RING_TRACK_STEP);
        track = crate::tokens::colors::composite(Colors::hairline_on(ground, alpha), ground);
    }
    track
}

/// The track's strength on its ground.
const RING_TRACK_ALPHA: u8 = 56;
const RING_TRACK_MIN_ALPHA: u8 = 24;
const RING_TRACK_STEP: u8 = 8;

/// Immediate-mode progress ring: a countdown or progress with an optional
/// centred readout.
#[derive(Clone, Copy, Debug)]
pub struct ProgressRingFace<'a> {
    frac: f64,
    value: Option<&'a str>,
    caption: Option<&'a str>,
    size: ProgressRingSize,
    fill: RingFill,
}

impl<'a> ProgressRingFace<'a> {
    /// `frac` is how full the ring is, clamped to `0..=1`. A countdown
    /// passes the share of time left.
    #[must_use]
    pub fn new(frac: f64) -> Self {
        Self {
            frac: if frac.is_finite() {
                frac.clamp(0.0, 1.0)
            } else {
                0.0
            },
            value: None,
            caption: None,
            size: ProgressRingSize::Regular,
            fill: RingFill::Progress,
        }
    }

    /// The centred readout, in tabular figures ("23", "42%", "128 Mb/s").
    #[must_use]
    pub fn value(mut self, value: &'a str) -> Self {
        self.value = Some(value);
        self
    }

    /// What the value is, under it ("seconds", "Download").
    #[must_use]
    pub fn caption(mut self, caption: &'a str) -> Self {
        self.caption = Some(caption);
        self
    }

    #[must_use]
    pub fn size(mut self, size: ProgressRingSize) -> Self {
        self.size = size;
        self
    }

    /// A measured level (CPU, disk) that turns warning and danger.
    #[must_use]
    pub fn level(mut self, thresholds: GaugeThresholds) -> Self {
        self.fill = RingFill::Level(thresholds);
        self
    }

    /// A fill that means a status or a person.
    #[must_use]
    pub fn tone(mut self, tone: Tone) -> Self {
        self.fill = RingFill::Tone(tone);
        self
    }

    fn fill_colour(&self, theme: &Theme) -> Color {
        match self.fill {
            RingFill::Progress => theme.tokens.colors.intent.inverted,
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a 0..=1 fraction narrows to the thresholds' f32 exactly enough"
            )]
            RingFill::Level(thresholds) => thresholds.color(theme, self.frac as f32),
            RingFill::Tone(tone) => tone.colour(theme),
        }
    }

    /// The ring's diameter.
    pub fn diameter(&self, theme: &Theme) -> f64 {
        self.size.diameter(theme)
    }

    /// Paint the ring centred in `rect`.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        let theme = ctx.theme;
        let side = self.diameter(theme).min(rect.width()).min(rect.height());
        if side <= 0.0 {
            return;
        }
        let thickness = self.size.thickness(theme).min(side / 4.0);
        let radius = (side - thickness) / 2.0;
        let center = rect.center();
        let fill = self.fill_colour(theme);
        let track = ring_track(theme.tokens.colors.surfaces.content, fill);
        ctx.scene.stroke(
            &Stroke::new(thickness),
            ctx.transform,
            track,
            None,
            &Circle::new(center, radius),
        );
        if self.frac > 0.0 {
            let arc = Arc::new(center, (radius, radius), -FRAC_PI_2, self.frac * TAU, 0.0);
            let stroke = Stroke::new(thickness)
                .with_caps(Cap::Round)
                .with_join(Join::Round);
            ctx.scene.stroke(&stroke, ctx.transform, fill, None, &arc);
        }
        let Some((value_role, caption_role)) = self.size.roles() else {
            return;
        };
        let caption_style = theme.text(caption_role);
        // The text stays inside the ring with `s2` of air: the width it may
        // take is the chord of the ring's inner edge at the top and bottom
        // of the text block, not the diameter.
        let clear = (radius - thickness / 2.0 - f64::from(theme.tokens.spacing.s2)).max(1.0);
        let caption_h = self.caption.map_or(0.0, |c| {
            f64::from(ctx.text.measure_style(c, None, &caption_style).1)
        });
        let chord = |block_h: f64| {
            let half = (block_h / 2.0).min(clear);
            2.0 * (clear * clear - half * half).max(0.0).sqrt()
        };
        // A value too wide for its role at this size steps down to `Label`
        // before it elides: "3 of 10" reads whole in a regular ring.
        let value_style = self.value.map_or_else(
            || theme.text(value_role).numeric(),
            |v| {
                [value_role, TextRole::Heading, TextRole::Label]
                    .into_iter()
                    .map(|role| theme.text(role).numeric())
                    .find(|style| {
                        let (w, h) = ctx.text.measure_style(v, None, style);
                        f64::from(w) <= chord(f64::from(h) + caption_h)
                    })
                    .unwrap_or_else(|| theme.text(TextRole::Label).numeric())
            },
        );
        let value_h = self.value.map_or(0.0, |v| {
            f64::from(ctx.text.measure_style(v, None, &value_style).1)
        });
        let inner = crate::num::f32_of_f64(chord(value_h + caption_h).max(1.0));
        let value = self
            .value
            .map(|v| ctx.text.elide_to_width_style(v, inner, &value_style));
        let caption = self
            .caption
            .map(|c| ctx.text.elide_to_width_style(c, inner, &caption_style));
        let mut y = center.y - (value_h + caption_h) / 2.0;
        if let Some(value) = value {
            let w = f64::from(ctx.text.measure_style(&value, None, &value_style).0);
            let ink = theme.text_primary;
            ctx.text(
                &value,
                &value_style,
                ink,
                Point::new(center.x - w / 2.0, y),
                None,
            );
            y += value_h;
        }
        if let Some(caption) = caption {
            let w = f64::from(ctx.text.measure_style(&caption, None, &caption_style).0);
            let ink = theme.text_dim;
            ctx.text(
                &caption,
                &caption_style,
                ink,
                Point::new(center.x - w / 2.0, y),
                None,
            );
        }
    }
}

/// A retained progress ring. See [`ProgressRingFace`].
pub struct ProgressRing {
    frac: f64,
    value: String,
    caption: String,
    size: ProgressRingSize,
    fill: RingFill,
    rect: Rect,
    dirty: bool,
}

impl ProgressRing {
    pub fn new(frac: f64) -> Self {
        Self {
            frac: if frac.is_finite() {
                frac.clamp(0.0, 1.0)
            } else {
                0.0
            },
            value: String::new(),
            caption: String::new(),
            size: ProgressRingSize::Regular,
            fill: RingFill::Progress,
            rect: Rect::ZERO,
            dirty: true,
        }
    }

    /// See [`ProgressRingFace::value`].
    #[must_use]
    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self
    }

    /// See [`ProgressRingFace::caption`].
    #[must_use]
    pub fn caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = caption.into();
        self
    }

    #[must_use]
    pub fn size(mut self, size: ProgressRingSize) -> Self {
        self.size = size;
        self
    }

    /// See [`ProgressRingFace::level`].
    #[must_use]
    pub fn level(mut self, thresholds: GaugeThresholds) -> Self {
        self.fill = RingFill::Level(thresholds);
        self
    }

    /// See [`ProgressRingFace::tone`].
    #[must_use]
    pub fn tone(mut self, tone: Tone) -> Self {
        self.fill = RingFill::Tone(tone);
        self
    }

    /// Update the fill and readout (a ticking countdown).
    pub fn set(&mut self, frac: f64, value: impl Into<String>) {
        self.frac = if frac.is_finite() {
            frac.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.value = value.into();
        self.dirty = true;
    }

    fn face(&self) -> ProgressRingFace<'_> {
        let mut face = ProgressRingFace::new(self.frac).size(self.size);
        face.fill = self.fill;
        if !self.value.is_empty() {
            face = face.value(&self.value);
        }
        if !self.caption.is_empty() {
            face = face.caption(&self.caption);
        }
        face
    }
}

impl Widget for ProgressRing {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let side = crate::num::f32_of_f64(self.size.diameter(theme));
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(side),
                height: Dimension::length(side),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        let rect = Rect::from_origin_size(
            (
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
            ),
            (f64::from(l.size.width), f64::from(l.size.height)),
        );
        if rect != self.rect {
            self.dirty = true;
        }
        self.rect = rect;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if presence.should_cull(self.rect) {
            return;
        }
        self.face().paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.rect,
        );
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn name(&self) -> &'static str {
        "ProgressRing"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a 0..=1 fraction narrows to f32 for the percentage"
        )]
        let pct = pct(self.frac as f32);
        let name = [&self.caption, &self.value]
            .into_iter()
            .find(|s| !s.is_empty())
            .cloned();
        let value = if self.value.is_empty() {
            format!("{pct}%")
        } else {
            self.value.clone()
        };
        Some(
            AccessNode::new(Role::ProgressBar)
                .with_optional_name(name)
                .with_value(value),
        )
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

/// Labelled horizontal bar gauge — a rounded track with a left-anchored
/// fill, a top-left label and a right-aligned value readout.
pub struct BarGauge {
    frac: f32,
    fill: Fillcolor,
    label: String,
    value: String,
    height: f32,
    style: GaugeStyle,
    rect: Rect,
}

impl BarGauge {
    /// `frac` is clamped to `0..1`.
    pub fn new(frac: f32) -> Self {
        Self {
            frac: frac.clamp(0.0, 1.0),
            fill: Fillcolor::Threshold(GaugeThresholds::default()),
            label: String::new(),
            value: String::new(),
            height: 32.0,
            style: GaugeStyle::default(),
            rect: Rect::ZERO,
        }
    }

    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>, value: impl Into<String>) -> Self {
        self.label = label.into();
        self.value = value.into();
        self
    }

    #[must_use]
    pub fn with_thresholds(mut self, thresholds: GaugeThresholds) -> Self {
        self.fill = Fillcolor::Threshold(thresholds);
        self
    }

    /// Bypasses the threshold bands.
    #[must_use]
    pub fn with_color(mut self, color: Color) -> Self {
        self.fill = Fillcolor::Fixed(color);
        self
    }

    #[must_use]
    pub fn with_style(mut self, style: GaugeStyle) -> Self {
        self.style = style;
        self
    }

    pub fn style(&self) -> &GaugeStyle {
        &self.style
    }

    pub fn style_mut(&mut self) -> &mut GaugeStyle {
        &mut self.style
    }

    #[must_use]
    pub fn with_height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }

    /// Update the fraction (clamped to `0..1`).
    pub fn set_fraction(&mut self, frac: f32) {
        self.frac = frac.clamp(0.0, 1.0);
    }

    fn resolve_color(&self, theme: &Theme) -> Color {
        match self.fill {
            Fillcolor::Threshold(t) => t.color(theme, self.frac),
            Fillcolor::Fixed(c) => c,
        }
    }

    /// Paint `reading.label` top-left and `reading.value` top-right of
    /// `rect`, over a track along its bottom filled to `reading.frac`.
    pub fn draw_at(ctx: &mut PaintCtx<'_>, rect: Rect, reading: &GaugeReading<'_>) {
        Self::draw_styled(ctx, rect, reading, &GaugeStyle::default());
    }

    /// [`Self::draw_at`] with an explicit [`GaugeStyle`].
    pub fn draw_styled(
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        reading: &GaugeReading<'_>,
        style: &GaugeStyle,
    ) {
        if rect.width() <= 0.0 {
            return;
        }
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let GaugeReading {
            frac,
            color,
            label,
            value,
        } = *reading;
        let tokens = &theme.tokens;
        let text_style = theme.text(TextRole::Caption).numeric();
        if !label.is_empty() {
            text_renderer.draw_style(
                scene,
                label,
                tokens.colors.text_primary,
                transform.pre_translate(Vec2::new(rect.x0, rect.y0)),
                None,
                &theme.text(TextRole::Label),
            );
        }
        if !value.is_empty() {
            let (vw, _) = text_renderer.measure_style(value, None, &text_style);
            text_renderer.draw_style(
                scene,
                value,
                tokens.colors.text_dim,
                transform.pre_translate(Vec2::new(rect.x1 - f64::from(vw), rect.y0)),
                None,
                &text_style,
            );
        }

        let track = Rect::new(rect.x0, rect.y1 - BAR_TRACK_H, rect.x1, rect.y1);
        let radius = BAR_TRACK_H / 2.0;
        scene.fill(
            Fill::NonZero,
            transform,
            track_color(theme, style.track_alpha),
            None,
            &RoundedRect::from_rect(track, radius),
        );

        let frac = f64::from(frac.clamp(0.0, 1.0));
        if frac > 0.0 {
            let fill = Rect::new(
                track.x0,
                track.y0,
                track.x0 + track.width() * frac,
                track.y1,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &RoundedRect::from_rect(fill, radius),
            );
        }
    }

    /// A label-less span gauge: `track` is the full rounded track and the
    /// fill covers `from..=to` of it (fractions, clamped and ordered) — a
    /// day's low–high on the week's range, an interval on a scale. `fill` is
    /// any brush, so a caller can lay one gradient across the whole track and
    /// let each span show its slice of it. An empty span still paints as a
    /// round dot the track's height wide, so a single value stays visible.
    pub fn draw_span(
        scene: &mut Scene,
        transform: Affine,
        track: Rect,
        theme: &Theme,
        (from, to): (f32, f32),
        fill: &Brush,
        style: &GaugeStyle,
    ) {
        if track.width() <= 0.0 || track.height() <= 0.0 {
            return;
        }
        let radius = track.height() / 2.0;
        scene.fill(
            Fill::NonZero,
            transform,
            track_color(theme, style.track_alpha),
            None,
            &RoundedRect::from_rect(track, radius),
        );
        let (lo, hi) = {
            let (a, b) = (
                f64::from(from.clamp(0.0, 1.0)),
                f64::from(to.clamp(0.0, 1.0)),
            );
            (a.min(b), a.max(b))
        };
        let x0 = track.x0 + track.width() * lo;
        let x1 = (track.x0 + track.width() * hi).max(x0 + track.height());
        scene.fill(
            Fill::NonZero,
            transform,
            fill,
            None,
            &RoundedRect::new(x0, track.y0, x1, track.y1, radius),
        );
    }
}

impl Widget for BarGauge {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(self.height),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let reading = GaugeReading {
            frac: self.frac,
            color: self.resolve_color(theme),
            label: &self.label,
            value: &self.value,
        };
        BarGauge::draw_styled(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.rect,
            &reading,
            &self.style,
        );
    }

    fn name(&self) -> &'static str {
        "BarGauge"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        // `label` names it; `value` is the caller's readout when present
        // (e.g. "3.2 GB"), else the fraction as a percentage. Both are
        // display strings, not FTL keys → no `i18n!`.
        let pct = pct(self.frac);
        let value = if self.value.is_empty() {
            format!("{pct}%")
        } else {
            self.value.clone()
        };
        let name = (!self.label.is_empty()).then(|| self.label.clone());
        Some(
            AccessNode::new(Role::ProgressBar)
                .with_optional_name(name)
                .with_value(value),
        )
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello_encoding::DrawTag;

    fn count_color_tags(scene: &Scene) -> usize {
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count()
    }

    #[test]
    fn gauge_ring_and_bar_paint_tracks_labels_and_threshold_bands() {
        /// Threshold colour resolution: below `warning` → success, in the
        /// `[warning, danger)` band → warning, at/above `danger` → danger.
        fn thresholds_pick_semantic_bands() {
            let theme = Theme::dark();
            let t = GaugeThresholds::default(); // warning 0.6, danger 0.85
            let s = &theme.tokens.colors.semantic;
            assert_eq!(t.color(&theme, 0.10), s.success);
            assert_eq!(t.color(&theme, 0.59), s.success);
            assert_eq!(t.color(&theme, 0.60), s.warning, "boundary is inclusive");
            assert_eq!(t.color(&theme, 0.84), s.warning);
            assert_eq!(t.color(&theme, 0.85), s.danger, "boundary is inclusive");
            assert_eq!(t.color(&theme, 1.0), s.danger);
        }

        /// The bar gauge fills proportionally: a 0-fraction bar paints just the
        /// track, a non-zero fraction adds the fill rect.
        fn bar_zero_paints_track_only() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let rect = Rect::new(0.0, 0.0, 200.0, 32.0);

            let mut zero = Scene::new();
            BarGauge::draw_at(
                &mut PaintCtx::new(&mut zero, Affine::IDENTITY, &mut tr, &theme),
                rect,
                &GaugeReading {
                    frac: 0.0,
                    color: theme.tokens.colors.intent.inverted,
                    label: "",
                    value: "",
                },
            );
            let mut filled = Scene::new();
            BarGauge::draw_at(
                &mut PaintCtx::new(&mut filled, Affine::IDENTITY, &mut tr, &theme),
                rect,
                &GaugeReading {
                    frac: 0.5,
                    color: theme.tokens.colors.intent.inverted,
                    label: "",
                    value: "",
                },
            );
            assert_eq!(count_color_tags(&zero), 1, "track only at frac 0");
            assert!(
                count_color_tags(&filled) > count_color_tags(&zero),
                "fill adds a rect"
            );
        }

        /// A bar gauge with a label + value paints two glyph runs.
        fn bar_label_and_value_paint() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            BarGauge::draw_at(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
                Rect::new(0.0, 0.0, 200.0, 32.0),
                &GaugeReading {
                    frac: 0.4,
                    color: theme.tokens.colors.intent.inverted,
                    label: "Memory",
                    value: "4.2 GB",
                },
            );
            assert!(
                !scene.encoding().resources.glyph_runs.is_empty(),
                "label/value must paint glyph runs",
            );
        }

        thresholds_pick_semantic_bands();
        bar_zero_paints_track_only();
        bar_label_and_value_paint();
    }

    #[test]
    fn progress_ring_fill_reads_against_its_track_on_every_preset() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let ground = theme.tokens.colors.surfaces.content;
            for face in [
                ProgressRingFace::new(0.5),
                ProgressRingFace::new(0.5).tone(Tone::Success),
                ProgressRingFace::new(0.9).level(GaugeThresholds::default()),
            ] {
                let fill = face.fill_colour(&theme);
                let track = ring_track(ground, fill);
                let ratio = crate::color::contrast_ratio(fill, track);
                assert!(
                    ratio >= crate::tokens::colors::NON_TEXT_MIN_CONTRAST,
                    "{}: fill on track only {ratio:.2}:1",
                    theme.name
                );
                assert!(
                    crate::surface::edge_delta(track, ground) > 0.0,
                    "{}: the track registers on the ground",
                    theme.name
                );
            }
        }
    }

    #[test]
    fn progress_ring_sizes_readouts_and_semantics() {
        let theme = Theme::dark();
        assert!(
            ProgressRingSize::Small.diameter(&theme) < ProgressRingSize::Regular.diameter(&theme)
        );
        assert!(
            ProgressRingSize::Regular.diameter(&theme) < ProgressRingSize::Large.diameter(&theme)
        );
        let mut tr = TextRenderer::new();
        let mut empty = Scene::new();
        ProgressRingFace::new(0.0).paint(
            &mut PaintCtx::new(&mut empty, Affine::IDENTITY, &mut tr, &theme),
            Rect::new(0.0, 0.0, 120.0, 120.0),
        );
        let mut half = Scene::new();
        ProgressRingFace::new(0.5)
            .value("23")
            .caption("seconds")
            .paint(
                &mut PaintCtx::new(&mut half, Affine::IDENTITY, &mut tr, &theme),
                Rect::new(0.0, 0.0, 120.0, 120.0),
            );
        assert!(count_color_tags(&half) > count_color_tags(&empty));
        assert!(!half.encoding().resources.glyph_runs.is_empty());
        let hot = ProgressRingFace::new(0.9).level(GaugeThresholds::default());
        assert_eq!(hot.fill_colour(&theme), theme.tokens.colors.semantic.danger);
        let ring = ProgressRing::new(0.42).value("42%").caption("CPU");
        let node = Widget::accessibility(&ring).unwrap();
        assert_eq!(node.role, crate::a11y::Role::ProgressBar);
        assert_eq!(node.name.as_deref(), Some("CPU"));
        assert_eq!(node.value.as_deref(), Some("42%"));
    }

    /// A span paints its track and its fill even when empty (a dot), and
    /// the style's track alpha reaches the track.
    #[test]
    fn span_paints_track_and_fill() {
        let theme = Theme::dark();
        let mut scene = Scene::new();
        BarGauge::draw_span(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 120.0, 6.0),
            &theme,
            (0.7, 0.7),
            &Brush::Solid(theme.tokens.colors.intent.inverted),
            &GaugeStyle::default(),
        );
        assert_eq!(count_color_tags(&scene), 2);
        let (faint, firm) = (
            track_color(&theme, TRACK_ALPHA),
            track_color(
                &theme,
                GaugeStyle::default().with_track_alpha(56).track_alpha,
            ),
        );
        assert!(firm.components[3] > faint.components[3]);
    }
}
