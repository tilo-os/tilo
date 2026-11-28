//! Gauges: [`ProgressRing`] (a countdown or progress ring with a centred readout) and
//! [`BarGauge`] (a labelled horizontal bar).

use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, TAU};

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Arc, Cap, Circle, Join, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{Brush, Color, Fill};

use tilo_ui_base::paint::PaintCtx;

use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::widget::TaffyTreeExt;
use tilo_ui_base::{widget::Tone, widget::Widget};

const BAR_TRACK_H: f64 = 8.0;

/// Paint options of a [`BarGauge`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GaugeStyle {
    pub compact_labels: bool,
}

impl GaugeStyle {
    #[must_use]
    pub fn compact(mut self) -> Self {
        self.compact_labels = true;
        self
    }
}

/// What an immediate-mode gauge paints: how full it is, in which colour, and the text
/// around it.
#[derive(Clone, Copy, Debug)]
pub struct GaugeReading<'a> {
    /// Fill fraction, clamped to `0..1` when painted.
    pub frac: f32,
    pub color: Color,
    pub label: &'a str,
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

#[path = "gauge_component.rs"]
mod component;
pub use component::{BarGaugeProps, ProgressRingProps};

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

#[derive(Clone, Copy, Debug)]
enum Fillcolor {
    Threshold(GaugeThresholds),
    Fixed(Color),
    Neutral,
}

/// How large a [`ProgressRing`] is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProgressRingSize {
    Small,
    #[default]
    Regular,
    Large,
}

impl ProgressRingSize {
    /// The ring's diameter under `theme`. A ring with a readout grows with
    /// the text scale so its value never elides.
    #[must_use]
    pub fn diameter(self, theme: &Theme) -> f64 {
        self.diameter_with_readout(theme, false)
    }

    fn diameter_with_readout(self, theme: &Theme, readout: bool) -> f64 {
        if readout && self == Self::Small {
            return f64::from(theme.tokens.metrics.min_pointer_target)
                * f64::from(theme.text_growth());
        }
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
            Self::Small => Some((TextRole::Caption, TextRole::Caption)),
            Self::Regular => Some((TextRole::Title, TextRole::Caption)),
            Self::Large => Some((TextRole::Display, TextRole::Label)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum RingFill {
    Progress,
    Level(GaugeThresholds),
    Tone(Tone),
}

/// Immediate-mode progress ring: a countdown or progress with an optional centred readout.
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

    fn fill_colour(&self, theme: &Theme) -> Color {
        match self.fill {
            RingFill::Progress => {
                let colors = &theme.tokens.colors;
                tilo_ui_base::tokens::colors::value_fill_on(
                    colors.intent.accent_ink,
                    colors.surfaces.content,
                    colors.intent.value_track,
                )
            }
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a 0..=1 fraction narrows to the thresholds' f32 exactly enough"
            )]
            RingFill::Level(thresholds) => thresholds.color(theme, self.frac as f32),
            RingFill::Tone(tone) => tone.colour(theme),
        }
    }

    /// The ring's diameter: a small ring with a value grows to hold it.
    pub fn diameter(&self, theme: &Theme) -> f64 {
        self.size.diameter_with_readout(theme, self.value.is_some())
    }

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
        let track = theme.tokens.colors.intent.value_track;
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
        let small = self.size == ProgressRingSize::Small;
        // A small ring holds its numeric value only; its caption is the
        // accessible name.
        let caption_text = if small { None } else { self.caption };
        let caption_style = theme.text(caption_role);
        let air = if small {
            theme.tokens.spacing.s1 / 2.0
        } else {
            theme.tokens.spacing.s2
        };
        let clear = (radius - thickness / 2.0 - f64::from(air)).max(1.0);
        let caption_h = caption_text.map_or(0.0, |c| {
            f64::from(ctx.text.measure_style(c, None, &caption_style).1)
        });
        let chord = |block_h: f64| {
            let half = (block_h / 2.0).min(clear);
            2.0 * (clear * clear - half * half).max(0.0).sqrt()
        };
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
        let inner = tilo_ui_base::num::f32_of_f64(chord(value_h + caption_h).max(1.0));
        let value = self
            .value
            .map(|v| ctx.text.elide_to_width_style(v, inner, &value_style));
        let caption = caption_text.map(|c| ctx.text.elide_to_width_style(c, inner, &caption_style));
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

/// What a state-bound ring shows ([`ProgressRing::from_state`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RingReading {
    pub frac: f64,
    pub value: String,
    pub caption: Option<String>,
}

impl RingReading {
    pub fn new(frac: f64, value: impl Into<String>) -> Self {
        Self {
            frac,
            value: value.into(),
            caption: None,
        }
    }

    /// See [`ProgressRingFace::caption`].
    #[must_use]
    pub fn caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self
    }
}

/// A retained progress ring.
pub struct ProgressRing {
    /// Where the ring is going: what the reading says now.
    frac: f64,
    /// What the ring is drawn at: it follows `frac` on the motion a reading settles on, so a value
    /// that steps (a speed test's dial, a scan's progress) eases there instead of jumping.
    drawn: tilo_ui_base::motion::MotionValue<f64>,
    value: String,
    caption: String,
    size: ProgressRingSize,
    fill: RingFill,
    rect: Rect,
    dirty: bool,
    layout_dirty: bool,
    source: Option<Box<dyn FnMut() -> Option<RingReading>>>,
}

impl ProgressRing {
    /// `frac` is how full the ring is, clamped to `0..=1`.
    pub fn new(frac: f64) -> Self {
        Self {
            frac: if frac.is_finite() {
                frac.clamp(0.0, 1.0)
            } else {
                0.0
            },
            drawn: {
                let at = if frac.is_finite() {
                    frac.clamp(0.0, 1.0)
                } else {
                    0.0
                };
                tilo_ui_base::motion::MotionValue::new(at)
            },
            value: String::new(),
            caption: String::new(),
            size: ProgressRingSize::Regular,
            fill: RingFill::Progress,
            rect: Rect::ZERO,
            dirty: true,
            layout_dirty: true,
            source: None,
        }
    }

    /// A ring bound to `state`: `read` turns the state into what the ring shows, and the
    /// ring follows every change on its own.
    pub fn from_state<T: 'static>(
        state: &tilo_ui_base::LocalState<T>,
        mut read: impl FnMut(&T) -> RingReading + 'static,
    ) -> Self {
        let first = state.with(&mut read);
        let mut ring = Self::new(first.frac).value(first.value);
        if let Some(caption) = first.caption {
            ring.caption = caption;
        }
        let state = state.clone();
        let mut observed = state.revision();
        ring.source = Some(Box::new(move || {
            let revision = state.revision();
            if revision == observed {
                return None;
            }
            observed = revision;
            Some(state.with(&mut read))
        }));
        ring
    }

    /// Change what the value is ("seconds" to "minutes").
    pub fn set_caption(&mut self, caption: impl Into<String>) {
        let caption = caption.into();
        if caption != self.caption {
            self.caption = caption;
            self.dirty = true;
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
        let at = if frac.is_finite() {
            frac.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.frac = at;
        let value = value.into();
        if value.is_empty() != self.value.is_empty() {
            self.layout_dirty = true;
        }
        self.value = value;
        self.dirty = true;
    }

    fn apply_reading(&mut self, reading: RingReading) {
        self.set(reading.frac, reading.value);
        if let Some(caption) = reading.caption {
            self.set_caption(caption);
        }
    }

    fn face(&self) -> ProgressRingFace<'_> {
        let mut face = ProgressRingFace::new(self.drawn.get(tilo_ui_base::motion::frame_now()))
            .size(self.size);
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
    tilo_ui_base::described!();

    fn sync(
        &mut self,
        _state: &tilo_ui_base::widget::AppModel,
        _path: &tilo_ui_base::widget::ModelPath,
    ) {
        if let Some(reading) = self.source.as_mut().and_then(|read| read()) {
            self.apply_reading(reading);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let side = tilo_ui_base::num::f32_of_f64(self.face().diameter(theme));
        let style = Style {
            size: Size::from_lengths(side, side),
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.dirty = true;
        }
        self.rect = rect;
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        if env.should_cull(self.rect) {
            return;
        }
        // A value that stepped eases to where it is going, so a dial that lands a sample a second
        // moves between them as a reading does rather than jumping.
        if !self.drawn.target().total_cmp(&self.frac).is_eq() {
            self.drawn.set(
                self.frac,
                tilo_ui_base::motion::Follow::READING.timing(
                    tilo_ui_base::motion::Direction::of(self.frac >= self.drawn.target()),
                    theme.motion(),
                    tilo_ui_base::tokens::MotionSource::Pointer,
                ),
                tilo_ui_base::motion::frame_now(),
            );
        }
        self.face().paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.rect,
        );
    }

    tilo_ui_base::redraw_flag!();

    fn needs_animation_frame(&self) -> bool {
        // The ring keeps asking for frames while its fill is on its way to the value it shows.
        self.drawn.is_moving(tilo_ui_base::motion::frame_now())
            || tilo_ui_base::widget::defaults::needs_animation_frame(&|visit| {
                self.for_each_child(visit)
            })
    }

    fn name(&self) -> &'static str {
        "ProgressRing"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::Role};
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
        self.layout_dirty || self.rect == Rect::ZERO
    }

    fn layout_measured(&mut self) {
        self.layout_dirty = false;
    }
}

/// Labelled horizontal bar gauge — a rounded track with a left-anchored fill, a top-left
/// label and a right-aligned value readout.
pub struct BarGauge {
    frac: f32,
    fill: Fillcolor,
    label: String,
    value: String,
    height: f32,
    style: GaugeStyle,
    rect: Rect,

    dirty: bool,
}

impl BarGauge {
    /// `frac` is clamped to `0..1`.
    pub fn new(frac: f32) -> Self {
        Self {
            dirty: true,
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

    /// A share with no status (storage by kind, a quota not yet full): the
    /// inverted fill, never a status colour.
    #[must_use]
    pub fn neutral(mut self) -> Self {
        self.fill = Fillcolor::Neutral;
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

    #[must_use]
    pub fn with_height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }

    fn resolve_color(&self, theme: &Theme) -> Color {
        match self.fill {
            Fillcolor::Threshold(t) => t.color(theme, self.frac),
            Fillcolor::Fixed(c) => c,
            Fillcolor::Neutral => theme.tokens.colors.intent.value_fill,
        }
    }

    /// Paint `reading.label` top-left and `reading.value` top-right of `rect`, over a track
    /// along its bottom filled to `reading.frac`.
    pub fn draw_at(ctx: &mut PaintCtx<'_>, rect: Rect, reading: &GaugeReading<'_>) {
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
            theme.tokens.colors.intent.value_track,
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
    ) {
        if track.width() <= 0.0 || track.height() <= 0.0 {
            return;
        }
        let radius = track.height() / 2.0;
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.intent.value_track,
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
    tilo_ui_base::described!();

    /// A leaf as wide as its parent and `self.height` tall, dirty until first placed.
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &tilo_ui_base::theme::Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::length(self.height),
            },
            ..Default::default()
        };
        tilo_ui_base::widget::TaffyTreeExt::widget_leaf(taffy, style, node_map, self)
    }

    tilo_ui_base::rect_geometry!();

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        let reading = GaugeReading {
            frac: self.frac,
            color: self.resolve_color(theme),
            label: &self.label,
            value: &self.value,
        };
        BarGauge::draw_at(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.rect,
            &reading,
        );
    }

    fn needs_redraw(&self) -> bool {
        tilo_ui_base::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        tilo_ui_base::widget::Widget::mark_children_drawn(self);
    }

    fn name(&self) -> &'static str {
        "BarGauge"
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::Role};
        // `label` names it; `value` is the caller's readout when present
        // (e.g. "3.2 GB"), else the fraction as a percentage. Both are
        // display strings, not FTL keys, so no lookup.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_ui_base::text_render::TextRenderer;
    use vello_encoding::DrawTag;

    fn count_color_tags(scene: &Scene) -> usize {
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count()
    }

    tilo_test_env::scenario! {
        fn gauges_and_progress_rings_paint_their_tracks_and_readouts;

        phase gauge_ring_and_bar_paint_tracks_labels_and_threshold_bands;
        {
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

            thresholds_pick_semantic_bands();
        }

        phase a_progress_fill_reads_against_its_track_and_the_page_on_every_preset;
        {
            // What a person reads is the indicator: the fill. The track is one light neutral
            // under every bar, ring and gauge; the fill stands out from it and from the page.
            let min = tilo_ui_base::tokens::colors::NON_TEXT_MIN_CONTRAST;
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let ground = theme.tokens.colors.surfaces.content;
                let track = theme.tokens.colors.intent.value_track;
                assert_ne!(track, ground, "{}: the track shows the extent", theme.name);
                for (what, fill) in [
                    ("ring", ProgressRingFace::new(0.5).fill_colour(&theme)),
                    ("bar", theme.tokens.colors.intent.value_fill),
                    (
                        "success ring",
                        ProgressRingFace::new(0.5)
                            .tone(Tone::Success)
                            .fill_colour(&theme),
                    ),
                    (
                        "level ring",
                        ProgressRingFace::new(0.9)
                            .level(GaugeThresholds::default())
                            .fill_colour(&theme),
                    ),
                ] {
                    for (on, against) in [("track", track), ("page", ground)] {
                        let ratio = crate::color::contrast_ratio(fill, against);
                        assert!(
                            ratio >= min,
                            "{}: the {what}'s fill reads on the {on} at only {ratio:.2}:1",
                            theme.name
                        );
                    }
                }
            }
        }

        phase a_small_ring_holds_a_numeric_readout_and_follows_its_state;
        {
            let theme = Theme::dark();
            let plain = ProgressRingFace::new(0.5).size(ProgressRingSize::Small);
            let counting = plain.value("23");
            assert!(counting.diameter(&theme) > plain.diameter(&theme));
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            let side = counting.diameter(&theme);
            counting.paint(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
                Rect::new(0.0, 0.0, side, side),
            );
            assert!(
                !scene.encoding().resources.glyph_runs.is_empty(),
                "the seconds paint inside a small ring"
            );

            let state = tilo_ui_base::LocalState::new(30_u32);
            let mut ring = ProgressRing::from_state(&state, |left| {
                RingReading::new(f64::from(*left) / 30.0, left.to_string()).caption("seconds")
            })
            .size(ProgressRingSize::Small);
            assert_eq!(ring.value, "30");
            state.set(12);
            let doc = automerge::AutoCommit::new();
            ring.sync(&doc, &automerge::ROOT);
            assert_eq!(ring.value, "12");
            assert_eq!(ring.caption, "seconds");
            ring.set_caption("minutes");
            assert_eq!(ring.caption, "minutes");
        }

        phase progress_ring_sizes_readouts_and_semantics;
        {
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
            assert_eq!(node.role, tilo_ui_base::a11y::Role::ProgressBar);
            assert_eq!(node.name.as_deref(), Some("CPU"));
            assert_eq!(node.value.as_deref(), Some("42%"));
        }

        phase a_stepped_value_eases_to_the_next;
        {
            let mut ring = ProgressRing::new(0.0);
            assert_eq!(ring.face().frac, 0.0, "it starts where it is set");
            ring.set(0.8, "8.0");
            assert_eq!(ring.frac, 0.8, "the reading is what it is going to");
            // Drawing starts the motion, and the frames it asks for carry it there.
            let frame = tilo_ui_base::motion::pin_frame();
            let start = tilo_ui_base::motion::frame_now();
            let drawn = {
                ring.drawn.set(
                    ring.frac,
                    tilo_ui_base::motion::Follow::READING.timing(
                        tilo_ui_base::motion::Direction::Enter,
                        Theme::dark().motion(),
                        tilo_ui_base::tokens::MotionSource::Pointer,
                    ),
                    start,
                );
                ring.face().frac
            };
            assert!(drawn < 0.8, "it has not jumped to the value: {drawn}");
            drop(frame);
            let _later = tilo_ui_base::motion::pin_frame_at(start + crate::time::Duration::from_millis(40));
            assert!(ring.face().frac > drawn, "it eases there");
        }

    }
}
