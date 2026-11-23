//! [`ConversationList`] — the message list a widget-tree app draws, the same conversation the painting
//! apps compose themselves — the day breaks, the clock each line carries, the delivery vocabulary, the
//! way consecutive lines from one person run together — so a message reads the same whether it arrived
//! in a channel or a private thread.

use std::collections::HashMap;

use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use crate::paint::PaintCtx;

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::tokens::colors::mix8;
use crate::tokens::contrast::BODY_TEXT_MIN_CONTRAST;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

use super::band::SectionDividerFace;
use super::day::{ConversationClock, ConversationDay, DayBreaks};
use super::delivery::{DeliveryFace, DeliveryState};

/// How close two messages by one author are to show as one group: the avatar, the name and the
/// time stand over the first of them.
pub const GROUPING_WINDOW_SECS: i64 = 5 * 60;

/// The most of the column one bubble may take. A bubble that fills the column
/// is no longer a bubble: you lose the side that says who is talking.
const BUBBLE_COLUMN_SHARE: f64 = 0.78;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversationRole {
    Incoming,
    Outgoing,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConversationLine {
    /// Stable identity: a semantic key, and what a caller anchors a scroll to.
    pub id: String,
    pub at_ms: i64,
    pub author: String,
    pub body: String,
    pub role: ConversationRole,
    pub delivery: Option<DeliveryState>,
}

impl ConversationLine {
    #[must_use]
    pub fn incoming(
        id: impl Into<String>,
        at_ms: i64,
        author: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            at_ms,
            author: author.into(),
            body: body.into(),
            role: ConversationRole::Incoming,
            delivery: None,
        }
    }

    #[must_use]
    pub fn outgoing(id: impl Into<String>, at_ms: i64, body: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            at_ms,
            author: String::new(),
            body: body.into(),
            role: ConversationRole::Outgoing,
            delivery: None,
        }
    }

    #[must_use]
    pub fn with_delivery(mut self, delivery: DeliveryState) -> Self {
        self.delivery = Some(delivery);
        self
    }

    #[must_use]
    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.author = author.into();
        self
    }
}

/// Whether `line` starts a run of its own, after `previous` (the line above it,
/// if there is one).
///
/// The rule is the one [`run_breaks`] documents, kept in one place so the list's
/// layout and its public answer cannot drift apart.
fn begins_group(previous: Option<&ConversationLine>, line: &ConversationLine) -> bool {
    match previous {
        None => true,
        Some(previous) => {
            previous.author != line.author
                || previous.role != line.role
                || line.at_ms.saturating_sub(previous.at_ms).abs()
                    > GROUPING_WINDOW_SECS.saturating_mul(1_000)
                || (line.author.is_empty() && line.role == ConversationRole::Incoming)
        }
    }
}

/// What the caption under a line shows: its time, its delivery state, both or neither.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptionShows {
    pub time: bool,
    pub delivery: bool,
}

impl CaptionShows {
    #[must_use]
    pub const fn any(self) -> bool {
        self.time || self.delivery
    }
}

/// What `line`'s caption shows, given the line under it.
///
/// A group of one person's lines says a thing once, under its last line that says it: a time the
/// next line of the group repeats to the minute is left to that line, and so is a delivery state
/// the next line shares (three messages read together read as one "Read" under the last of them).
fn caption_shows(
    clock: ConversationClock,
    line: &ConversationLine,
    next: Option<&ConversationLine>,
) -> CaptionShows {
    let continued = next.filter(|next| {
        !begins_group(Some(line), next)
            && clock.day(seconds(line.at_ms)) == clock.day(seconds(next.at_ms))
    });
    let minute = |line: &ConversationLine| seconds(line.at_ms).div_euclid(60);
    CaptionShows {
        time: continued.is_none_or(|next| minute(next) != minute(line)),
        delivery: line.delivery.is_some()
            && continued.is_none_or(|next| next.delivery != line.delivery),
    }
}

/// What each line's caption shows, in order.
#[must_use]
pub fn captions(lines: &[ConversationLine], clock: ConversationClock) -> Vec<CaptionShows> {
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| caption_shows(clock, line, lines.get(index.saturating_add(1))))
        .collect()
}

#[must_use]
pub fn run_breaks(lines: &[ConversationLine]) -> Vec<bool> {
    let mut previous: Option<&ConversationLine> = None;
    let mut starts: Vec<bool> = Vec::with_capacity(lines.len());
    for line in lines {
        starts.push(begins_group(previous, line));
        previous = Some(line);
    }
    starts
}

/// One thing the list paints, in order.
#[derive(Clone, Debug, PartialEq)]
enum Row {
    Divider {
        y0: f64,
        y1: f64,
        x0: f64,
        x1: f64,
        label: String,
    },
    Line {
        index: usize,
        y0: f64,
        y1: f64,
        begins_group: bool,
        bubble: Rect,
        text: Point,
        text_width: f32,
        author: Option<Rect>,
        /// The time and delivery band under the bubble, when the line says either.
        caption: Option<(Rect, CaptionShows)>,
    },
}

impl Row {
    #[cfg(test)]
    fn line_geometry(&self) -> Option<(Rect, Option<Rect>)> {
        match self {
            Self::Line { bubble, author, .. } => Some((*bubble, *author)),
            Self::Divider { .. } => None,
        }
    }

    fn y1(&self) -> f64 {
        match self {
            Self::Divider { y1, .. } | Self::Line { y1, .. } => *y1,
        }
    }

    fn band(&self) -> (f64, f64) {
        match self {
            Self::Divider { y0, y1, .. } | Self::Line { y0, y1, .. } => (*y0, *y1),
        }
    }

    fn meets(&self, viewport: Rect) -> bool {
        let (y0, y1) = self.band();
        y1 > viewport.y0 && y0 < viewport.y1
    }
}

#[derive(Clone, Default)]
struct Layout {
    rows: Vec<Row>,
    height: f64,
}

/// The conversation column at one width: where a bubble may sit and how wide
/// its text may grow before it wraps.
///
/// It is a function of the width and the theme alone, so a layout that keeps its
/// measurements keeps this too.
struct Column {
    left: f64,
    right: f64,
    bubble_max: f64,
    inner_max: f64,
    wrap: f32,
}

impl Column {
    fn of(theme: &Theme, x0: f64, width: f64) -> Self {
        let gutter = f64::from(theme.tokens.spacing.s2);
        let readable = f64::from(theme.tokens.metrics.readable_width);
        let floor = f64::from(theme.tokens.spacing.s16);
        let pad_x = f64::from(theme.tokens.spacing.s3);
        // The conversation column: the width the list leaves, but never wider
        // than the readable measure, and centred in what is left over. Two
        // bubbles that hug the edges of a wide window are two bubbles nobody
        // reads as a conversation.
        let column_left = x0 + gutter;
        let column_right = x0 + width - gutter;
        let column = (column_right - column_left).min(readable).max(floor);
        let left = column_left + ((column_right - column_left) - column).max(0.0) / 2.0;
        let right = left + column;
        let bubble_max = (column * BUBBLE_COLUMN_SHARE).min(readable).max(floor);
        let inner_max = (bubble_max - 2.0 * pad_x).max(floor);
        Self {
            left,
            right,
            bubble_max,
            inner_max,
            wrap: crate::num::f32_of_f64(inner_max),
        }
    }
}

#[derive(Clone, Debug)]
struct Measured {
    /// The body the measurement was taken from. Compared, never painted: a line
    /// whose text is the text it was measured with needs no second measurement.
    body: String,
    /// The wrap width it was measured at, so a narrower or wider column does not
    /// borrow this box.
    wrap: f32,
    text_w: f32,
    text_h: f32,
}

#[derive(Clone, PartialEq)]
struct Typeface {
    theme: Theme,
    /// The renderer's shaping epoch: a font or locale change reshapes every
    /// line, so no measurement survives it.
    epoch: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ClockKey {
    offset: i64,
    today: ConversationDay,
}

impl ClockKey {
    fn of(clock: ConversationClock) -> Self {
        Self {
            offset: clock.offset(),
            today: clock.today(),
        }
    }
}

struct Placed {
    /// The address and length of the lines they were placed from, when the face
    /// that placed them owns them and never changes them.
    at: usize,
    len: usize,
    /// The lines as they were placed, for a face whose lines are a fresh slice
    /// each frame. Empty when the slice is the identity.
    lines: Vec<ConversationLine>,
    x0: f64,
    width: f64,
    y0: f64,
    /// The clock decides the dividers' days and labels, so a clock that names a
    /// different day re-places them.
    clock: ClockKey,
}

/// What a list's layout did, counted.
///
/// A frame of a long conversation measures nothing it measured before and paints
/// only what is in view; these are the numbers that say so, for a test or a
/// frame profiler.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListWork {
    /// Lines the last layout measured, because no kept measurement applied.
    pub measured: usize,
    pub reused: usize,
    pub painted: usize,
    pub culled: usize,
}

/// A list's layout, kept from one frame to the next.
///
/// Measuring a line's bubble — shaping its text to find where it wraps — is a
/// function of (its text, the column's width, the theme's type). None of those
/// change between the frames of a caret blink, a presence change or an arriving
/// message, so neither does the measurement: it is kept here and reused, and the
/// rows are kept whole until something they were placed from changes.
///
/// What invalidates what:
///
/// - a line whose text changed is measured again, alone;
/// - a width or text-scale change measures every line again, because every
///   bubble is a different shape;
/// - a font or locale change (the renderer's shaping epoch) drops every
///   measurement too;
/// - a moved list, or a line whose instant, author or side changed, re-places
///   the rows from the kept measurements without measuring anything.
///
/// Nothing here estimates. The height of the last message is the height it is
/// painted at, so a scrollbar cannot disagree with where the conversation ends.
#[derive(Default)]
pub struct ListCache {
    /// Each line's measured bubble text, by line id.
    measures: HashMap<String, Measured>,
    typeface: Option<Typeface>,
    layout: Layout,
    placed: Option<Placed>,
    work: ListWork,
}

impl ListCache {
    #[must_use]
    pub fn work(&self) -> ListWork {
        self.work
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct MeasuredCounts {
    measured: usize,
    reused: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Painted {
    painted: usize,
    culled: usize,
}

/// The bubble and ink for a line on `theme`.
///
/// The other person's words sit on the raised tier — an object inside the
/// content ground. Yours sit on your own colour itself, solid, so "mine" is the
/// same mark here as your caret and your selection elsewhere and reads at a
/// glance from across the column. It carries the accent's ink, as a primary
/// button does, deepened until that ink reads on it; the ink is measured, never
/// assumed.
#[must_use]
pub fn bubble_colors(theme: &Theme, role: ConversationRole) -> (Color, Color) {
    use crate::a11y::contrast_ratio;
    let colors = &theme.tokens.colors;
    match role {
        ConversationRole::Incoming => {
            let fill = colors.surfaces.raised;
            (
                fill,
                colors.ink_on(fill, colors.text_primary, BODY_TEXT_MIN_CONTRAST),
            )
        }
        ConversationRole::Outgoing => {
            let ink = colors.intent.on_accent;
            let ground = colors.surfaces.content;
            let own = colors.intent.you;
            let mut fill = own;
            for step in 1..=12_u8 {
                if contrast_ratio(ink, fill) >= BODY_TEXT_MIN_CONTRAST {
                    break;
                }
                fill = mix8(own, ground, f32::from(step) / 20.0);
            }
            (fill, colors.ink_on(fill, ink, BODY_TEXT_MIN_CONTRAST))
        }
    }
}

pub struct ListFace<'a> {
    lines: &'a [ConversationLine],
    clock: ConversationClock,
    /// Whether `lines` are an owner's own, which it never changes: then the
    /// slice is the conversation's identity and a kept layout stands while it
    /// does, without comparing a thread to itself every frame.
    owned: bool,
}

impl<'a> ListFace<'a> {
    #[must_use]
    pub fn new(lines: &'a [ConversationLine], clock: ConversationClock) -> Self {
        Self {
            lines,
            clock,
            owned: false,
        }
    }

    /// The same face for a list whose lines are its own and never change
    /// ([`ConversationList`] holds them and offers no way to change them).
    ///
    /// A fresh slice with the same contents is *not* the same conversation — a
    /// caller may edit a line in place — so only an owner that keeps its lines
    /// may say this, and it is not part of the public face.
    pub(crate) fn owned(lines: &'a [ConversationLine], clock: ConversationClock) -> Self {
        Self {
            lines,
            clock,
            owned: true,
        }
    }

    /// Whether `placed` holds the conversation this face is of, at `width`.
    ///
    /// The width is compared as the bits it was measured at: a row placed at a
    /// width one representable step away was placed from a different column, and
    /// where a height is asked for, where the rows start does not matter, so the
    /// origin and the column's left edge are not part of it.
    fn holds(&self, placed: &Placed, width: f64) -> bool {
        placed.width.to_bits() == width.to_bits()
            && placed.clock == ClockKey::of(self.clock)
            && self.is_the_same_lines(placed)
    }

    fn is_the_same_lines(&self, placed: &Placed) -> bool {
        if self.owned {
            return placed.at == self.lines.as_ptr().addr() && placed.len == self.lines.len();
        }
        placed.lines == self.lines
    }

    /// Say what this frame's measurements would be taken against, dropping the
    /// ones that no longer apply.
    ///
    /// A measurement is a function of the type and of the renderer that shaped
    /// it: a theme, text-scale or font change makes every kept box wrong.
    fn declare(&self, cache: &mut ListCache, text: &TextRenderer, theme: &Theme) {
        let typeface = Typeface {
            theme: theme.clone(),
            epoch: std::sync::Arc::as_ptr(text.layout_epoch()).addr(),
        };
        if cache.typeface.as_ref() != Some(&typeface) {
            cache.typeface = Some(typeface);
            cache.measures.clear();
            cache.placed = None;
        }
    }

    fn placed(&self, x0: f64, width: f64, y0: f64) -> Placed {
        let (at, len) = (self.lines.as_ptr().addr(), self.lines.len());
        Placed {
            at,
            len,
            lines: if self.owned {
                Vec::new()
            } else {
                self.lines.to_vec()
            },
            x0,
            width,
            y0,
            clock: ClockKey::of(self.clock),
        }
    }

    fn layout<'c>(
        &self,
        cache: &'c mut ListCache,
        text: &mut TextRenderer,
        theme: &Theme,
        x0: f64,
        width: f64,
        y0: f64,
    ) -> &'c Layout {
        self.declare(cache, text, theme);
        let stands = cache.placed.as_ref().is_some_and(|placed| {
            placed.x0.to_bits() == x0.to_bits()
                && placed.y0.to_bits() == y0.to_bits()
                && self.holds(placed, width)
        });
        if stands {
            cache.work = ListWork {
                measured: 0,
                reused: self.lines.len(),
                painted: 0,
                culled: 0,
            };
            return &cache.layout;
        }
        let column = Column::of(theme, x0, width);
        let counts = self.place(cache, text, theme, &column, y0);
        cache.placed = Some(self.placed(x0, width, y0));
        cache.work = ListWork {
            measured: counts.measured,
            reused: counts.reused,
            painted: 0,
            culled: 0,
        };
        &cache.layout
    }

    /// Place the rows in `column` from the origin `y0`, measuring only the lines
    /// no kept measurement applies to.
    fn place(
        &self,
        cache: &mut ListCache,
        text: &mut TextRenderer,
        theme: &Theme,
        column: &Column,
        y0: f64,
    ) -> MeasuredCounts {
        let spacing = &theme.tokens.spacing;
        let body_style = theme.text(TextRole::Body);
        let author_style = theme.text(TextRole::Caption);
        let caption_style = theme.text(TextRole::Caption).numeric();
        let gap = f64::from(spacing.s1);
        let run_gap = f64::from(spacing.s4);
        let pad_x = f64::from(spacing.s3);
        let pad_y = f64::from(spacing.s2);
        // The kept rows are placed again in the same rect: a scrolling list
        // re-places on every frame it moves, and that must not allocate.
        let mut rows: Vec<Row> = std::mem::take(&mut cache.layout.rows);
        rows.clear();
        let mut counts = MeasuredCounts::default();
        let mut days = DayBreaks::new();
        let mut previous: Option<&ConversationLine> = None;
        let mut y = y0;
        let author_line = f64::from(text.measure_style("Ag", None, &author_style).1);
        let caption_line = f64::from(text.measure_style("Ag", None, &caption_style).1);
        for (index, line) in self.lines.iter().enumerate() {
            let day = self.clock.day(seconds(line.at_ms));
            if days.begins(day) {
                let height = SectionDividerFace::height(theme);
                rows.push(Row::Divider {
                    y0: y,
                    y1: y + height,
                    x0: column.left,
                    x1: column.right,
                    label: self.clock.day_label(day),
                });
                y += height;
            }
            let begins_group = begins_group(previous, line);
            previous = Some(line);
            y += if begins_group { run_gap } else { gap };
            let author = (begins_group
                && line.role == ConversationRole::Incoming
                && !line.author.is_empty())
            .then(|| {
                let band = Rect::new(column.left, y, column.right, y + author_line);
                y += author_line + f64::from(spacing.s1);
                band
            });
            let (text_w, text_h) =
                self.measure(cache, text, line, column.wrap, &body_style, &mut counts);
            let used_w = f64::from(text_w).min(column.inner_max);
            let bubble_w = (used_w + 2.0 * pad_x).min(column.bubble_max);
            let bubble_h = f64::from(text_h) + 2.0 * pad_y;
            let bubble_x = match line.role {
                ConversationRole::Incoming => column.left,
                ConversationRole::Outgoing => (column.right - bubble_w).max(column.left),
            };
            let bubble = Rect::new(bubble_x, y, bubble_x + bubble_w, y + bubble_h);
            y = bubble.y1;
            let shows = caption_shows(self.clock, line, self.lines.get(index.saturating_add(1)));
            let caption = shows.any().then(|| {
                let band = Rect::new(bubble.x0, y + gap, bubble.x1, y + gap + caption_line);
                y = band.y1;
                (band, shows)
            });
            rows.push(Row::Line {
                index,
                y0: rows.last().map_or(y0, Row::y1),
                y1: y,
                begins_group,
                bubble,
                text: Point::new(bubble.x0 + pad_x, bubble.y0 + pad_y),
                text_width: crate::num::f32_of_f64((bubble_w - 2.0 * pad_x).max(1.0)),
                author,
                caption,
            });
        }
        if cache.measures.len() > self.lines.len() {
            let live: std::collections::HashSet<&str> =
                self.lines.iter().map(|line| line.id.as_str()).collect();
            cache.measures.retain(|id, _| live.contains(id.as_str()));
        }
        cache.layout = Layout {
            rows,
            height: y - y0,
        };
        counts
    }

    /// `line`'s bubble text box at `wrap`: the kept one when it still applies,
    /// else a measurement — which only this line's own text, a new width or a new
    /// type can ask for.
    fn measure(
        &self,
        cache: &mut ListCache,
        text: &mut TextRenderer,
        line: &ConversationLine,
        wrap: f32,
        style: &crate::text_render::TextStyle,
        counts: &mut MeasuredCounts,
    ) -> (f32, f32) {
        if let Some(kept) = cache.measures.get_mut(&line.id) {
            if kept.body == line.body && kept.wrap.to_bits() == wrap.to_bits() {
                counts.reused += 1;
                return (kept.text_w, kept.text_h);
            }
            let (text_w, text_h) = text.measure_style(&line.body, Some(wrap), style);
            counts.measured += 1;
            kept.body.clone_from(&line.body);
            kept.wrap = wrap;
            kept.text_w = text_w;
            kept.text_h = text_h;
            return (text_w, text_h);
        }
        let (text_w, text_h) = text.measure_style(&line.body, Some(wrap), style);
        counts.measured += 1;
        cache.measures.insert(
            line.id.clone(),
            Measured {
                body: line.body.clone(),
                wrap,
                text_w,
                text_h,
            },
        );
        (text_w, text_h)
    }

    #[cfg(test)]
    fn layout_for_test(&self, text: &mut TextRenderer, theme: &Theme, rect: Rect) -> Layout {
        let mut cache = ListCache::default();
        self.layout(&mut cache, text, theme, rect.x0, rect.width(), rect.y0)
            .clone()
    }

    pub fn height(&self, text: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        self.height_in(&mut ListCache::default(), text, theme, width)
    }

    /// The list's height at `width`, reusing what `cache` measured last time.
    ///
    /// A height does not depend on where the rows start, so this answers from
    /// the kept rows wherever a last frame placed them, and places them only when
    /// the conversation or the column changed.
    pub fn height_in(
        &self,
        cache: &mut ListCache,
        text: &mut TextRenderer,
        theme: &Theme,
        width: f64,
    ) -> f64 {
        self.declare(cache, text, theme);
        if let Some(height) = cache
            .placed
            .as_ref()
            .filter(|placed| self.holds(placed, width))
            .map(|_| cache.layout.height)
        {
            return height;
        }
        self.layout(cache, text, theme, 0.0, width, 0.0).height
    }

    pub fn paint(&self, paint: &mut PaintCtx<'_>, rect: Rect) {
        self.paint_in(paint, rect, None, &mut ListCache::default());
    }

    pub fn paint_in(
        &self,
        paint: &mut PaintCtx<'_>,
        rect: Rect,
        viewport: Option<Rect>,
        cache: &mut ListCache,
    ) {
        let layout = self.layout(
            cache,
            paint.text,
            paint.theme,
            rect.x0,
            rect.width(),
            rect.y0,
        );
        let painted = self.paint_layout(paint, layout, viewport);
        cache.work.painted = painted.painted;
        cache.work.culled = painted.culled;
    }

    /// Paint rows already measured by [`Self::layout`], so a caller that needs
    /// the geometry (a list storing its rows for its accessible nodes) lays
    /// out once.
    fn paint_layout(
        &self,
        paint: &mut PaintCtx<'_>,
        layout: &Layout,
        viewport: Option<Rect>,
    ) -> Painted {
        let theme = paint.theme;
        let radius = f64::from(theme.tokens.radii.control);
        let mut painted = Painted::default();
        for row in &layout.rows {
            if viewport.is_some_and(|viewport| !row.meets(viewport)) {
                painted.culled += 1;
                continue;
            }
            painted.painted += 1;
            match row {
                Row::Divider {
                    y0,
                    y1,
                    x0,
                    x1,
                    label,
                } => {
                    let band = Rect::new(*x0, *y0, *x1, *y1);
                    SectionDividerFace::new(label).paint(paint, band, band.x0, band.x1);
                }
                Row::Line {
                    index,
                    bubble,
                    text,
                    text_width,
                    author,
                    caption,
                    ..
                } => {
                    let Some(line) = self.lines.get(*index) else {
                        continue;
                    };
                    if let Some(band) = author {
                        let style = theme.text(TextRole::Caption);
                        paint.text(
                            &line.author,
                            &style,
                            theme.text_dim,
                            Point::new(band.x0, band.y0),
                            None,
                        );
                    }
                    let (fill, ink) = bubble_colors(theme, line.role);
                    paint.fill_round(*bubble, radius, fill);
                    let style = theme.text(TextRole::Body);
                    paint.text(&line.body, &style, ink, *text, Some(*text_width));
                    if let Some((band, shows)) = caption {
                        self.paint_caption(paint, line, *band, *shows);
                    }
                }
            }
        }
        painted
    }

    /// The caption under a line: its delivery state, then its time, the pair against the bubble's
    /// own edge (the right one under yours), either one alone where the group says the other later.
    fn paint_caption(
        &self,
        paint: &mut PaintCtx<'_>,
        line: &ConversationLine,
        band: Rect,
        shows: CaptionShows,
    ) {
        let theme = paint.theme;
        let style = theme.text(TextRole::Caption).numeric();
        let time = shows.time.then(|| self.clock.clock(seconds(line.at_ms)));
        let time_w = time.as_ref().map_or(0.0, |time| {
            f64::from(paint.text.measure_style(time, None, &style).0)
        });
        let face = line
            .delivery
            .filter(|_| shows.delivery)
            .map(DeliveryFace::new);
        let face_w = face
            .as_ref()
            .map_or(0.0, |face| face.width(paint.text, theme));
        let gap = if time.is_some() && face.is_some() {
            f64::from(theme.tokens.spacing.s2)
        } else {
            0.0
        };
        let start = match line.role {
            ConversationRole::Incoming => band.x0,
            ConversationRole::Outgoing => (band.x1 - face_w - gap - time_w).max(band.x0),
        };
        if let Some(face) = &face {
            face.paint(paint, start, band);
        }
        if let Some(time) = &time {
            let line_h = f64::from(paint.text.measure_style("Ag", None, &style).1);
            let top = band.center().y - line_h / 2.0;
            let x = start + face_w + gap;
            paint.text(time, &style, theme.text_tertiary, Point::new(x, top), None);
        }
    }

    fn spoken(&self, line: &ConversationLine) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !line.author.is_empty() {
            parts.push(line.author.clone());
        } else if line.role == ConversationRole::Outgoing {
            parts.push(super::t("conversation-draft"));
        }
        parts.push(self.clock.stamp(seconds(line.at_ms)));
        if let Some(state) = line.delivery {
            parts.push(state.label());
        }
        parts.push(line.body.clone());
        parts.join(", ")
    }
}

/// Milliseconds to seconds, the unit every clock in the toolkit reads.
fn seconds(at_ms: i64) -> i64 {
    at_ms.div_euclid(1_000)
}

#[crate::component::not_a_component(
    part,
    "`ui.ConversationList` (`DescribedConversationList`) is its described form"
)]
pub struct ConversationList {
    lines: Vec<ConversationLine>,
    rect: Rect,
    name: String,
    dirty: bool,
    theme: Option<Theme>,
    /// What the list measured and placed at each width a layout asked it about, the latest
    /// first, so the next frame — a caret blink, a presence change, one arriving message —
    /// measures and places only what changed. A layout asks at widths the list will not stand
    /// at (its content's, what a container offers before it settles), and the rows it composes
    /// and paints are the ones at the width it stands at.
    caches: Vec<(f64, ListCache)>,
}

/// How many widths a list keeps what it placed at: its own, and the probes one layout makes.
const KEPT_WIDTHS: usize = 4;

/// Whether two widths are one column: the width a layout measured at, and the box it then gave,
/// which can differ in their last bits.
fn same_width(left: f64, right: f64) -> bool {
    (left - right).abs() < 0.01
}

impl ConversationList {
    #[must_use]
    pub fn new(lines: Vec<ConversationLine>) -> Self {
        Self {
            lines,
            rect: Rect::ZERO,
            name: String::new(),
            dirty: true,
            theme: None,
            caches: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Show `lines` instead, oldest first: the list measures only what changed, since a line's
    /// bubble is a function of its text, the column's width and the theme's type.
    pub fn set_lines(&mut self, lines: Vec<ConversationLine>) {
        if self.lines == lines {
            return;
        }
        self.lines = lines;
        self.dirty = true;
    }

    /// The lines, in order.
    #[must_use]
    pub fn lines(&self) -> &[ConversationLine] {
        &self.lines
    }

    /// The row rects the last measure produced, for a test or a caller that
    /// anchors to a line.
    #[must_use]
    pub fn line_rect(&self, text: &mut TextRenderer, theme: &Theme, index: usize) -> Option<Rect> {
        let face = ListFace::new(&self.lines, ConversationClock::local());
        let mut cache = ListCache::default();
        let layout = face.layout(
            &mut cache,
            text,
            theme,
            self.rect.x0,
            self.rect.width(),
            self.rect.y0,
        );
        layout.rows.iter().find_map(|row| match row {
            Row::Line {
                index: at, bubble, ..
            } if *at == index => Some(*bubble),
            _ => None,
        })
    }

    #[must_use]
    pub fn work(&self) -> ListWork {
        self.kept().map(ListCache::work).unwrap_or_default()
    }

    /// What the list placed at the width it stands at, when a layout asked it about that width.
    fn kept(&self) -> Option<&ListCache> {
        let width = self.rect.width();
        self.caches
            .iter()
            .find(|(at, _)| same_width(*at, width))
            .map(|(_, cache)| cache)
    }

    /// What the list placed at `width`, made the latest kept: a new one, the oldest let go,
    /// when no layout asked about `width` lately.
    fn kept_at(caches: &mut Vec<(f64, ListCache)>, width: f64) -> &mut ListCache {
        let at = caches
            .iter()
            .position(|(at, _)| same_width(*at, width))
            .unwrap_or_else(|| {
                caches.truncate(KEPT_WIDTHS - 1);
                caches.push((width, ListCache::default()));
                caches.len() - 1
            });
        let kept = caches.remove(at);
        caches.insert(0, kept);
        &mut caches[0].1
    }

    /// How far the list stands from where its kept rows were placed: a measure places them at
    /// the origin, a paint where it paints, and the list may have moved since, as a scrolled
    /// page moves it without painting it.
    fn moved(&self) -> [f64; 2] {
        self.kept()
            .and_then(|cache| cache.placed.as_ref())
            .map_or([0.0, 0.0], |placed| {
                [self.rect.x0 - placed.x0, self.rect.y0 - placed.y0]
            })
    }
}

impl Widget for ConversationList {
    fn needs_redraw(&self) -> bool {
        crate::widget::redraw_or_children(self.dirty, self)
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        crate::widget::Widget::mark_children_drawn(self);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = Some(theme.clone());
        let style = Style {
            size: Size {
                width: percent(1.0_f32),
                height: auto(),
            },
            min_size: Size {
                width: length(0.0_f32),
                height: auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let Some(theme) = &self.theme else {
            return taffy::Size::ZERO;
        };
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w,
            taffy::AvailableSpace::MinContent | taffy::AvailableSpace::MaxContent => 0.0,
        });
        let face = ListFace::owned(&self.lines, ConversationClock::local());
        let height = face.height_in(
            Self::kept_at(&mut self.caches, f64::from(width)),
            text_renderer,
            theme,
            f64::from(width),
        );
        taffy::Size {
            width,
            height: crate::num::f32_of_f64(height),
        }
    }

    crate::rect_geometry!();

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(unused_variables, unused_mut, reason = "a draw binds only the parts it uses")]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        if env.should_cull(self.rect) {
            return;
        }
        let face = ListFace::owned(&self.lines, ConversationClock::local());
        let mut paint = PaintCtx::new(scene, transform, text_renderer, theme);
        let rect = self.rect;
        face.paint_in(
            &mut paint,
            rect,
            env.viewport(),
            Self::kept_at(&mut self.caches, rect.width()),
        );
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn name(&self) -> &'static str {
        "ConversationList"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let face = ListFace::new(&self.lines, ConversationClock::local());
        let total = self.lines.len();
        let [_, moved] = self.moved();
        let children: Vec<AccessNode> = self
            .lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                // The row's own band, from the last paint: a list item that
                // names a line must point at where that line is.
                let row = self
                    .kept()
                    .map_or(&[][..], |cache| &cache.layout.rows[..])
                    .iter()
                    .find_map(|row| match row {
                        Row::Line {
                            index: at, y0, y1, ..
                        } if *at == index => Some(Rect::new(
                            self.rect.x0,
                            *y0 + moved,
                            self.rect.x1,
                            *y1 + moved,
                        )),
                        _ => None,
                    })
                    .unwrap_or(self.rect);
                AccessNode::new(Role::ListItem)
                    .with_action_target(format!("conversation-line:{}", line.id))
                    .with_name(face.spoken(line))
                    .with_position(index.saturating_add(1), total)
                    .with_bounds(row)
                    .with_state(AccessState::new().focusable(false))
            })
            .collect();
        let mut list = AccessNode::new(Role::List);
        if !self.name.is_empty() {
            list = list.with_name(self.name.clone());
        }
        Some(list.with_children(children))
    }
}

#[cfg(test)]
#[path = "list_cache_tests.rs"]
mod cache_tests;

#[cfg(test)]
#[path = "list_cull_tests.rs"]
mod cull_tests;

#[cfg(test)]
#[path = "list_frame_tests.rs"]
mod frame_tests;

#[cfg(test)]
mod geometry_tests {
    use super::*;

    #[test]
    fn every_row_is_laid_out_inside_the_list_it_is_given() {
        let mut text = crate::font_catalog::fixture_renderer();
        let theme = Theme::dark();
        let clock = ConversationClock::at(0, 1_782_828_300);
        let lines = vec![
            ConversationLine::incoming("a", 1_782_828_300_000, "Ada Rivera", "are you nearby?"),
            ConversationLine::outgoing("b", 1_782_828_360_000, "on my way"),
        ];
        let face = ListFace::new(&lines, clock);
        let rect = Rect::new(360.0, 100.0, 780.0, 500.0);
        let layout = face.layout_for_test(&mut text, &theme, rect);
        let mut checked = 0;
        for row in &layout.rows {
            if let Some((bubble, author)) = row.line_geometry() {
                assert!(
                    bubble.x0 >= rect.x0 && bubble.x1 <= rect.x1,
                    "a bubble stays inside its list: {bubble:?} in {rect:?}"
                );
                if let Some(author) = author {
                    assert!(
                        author.x0 >= rect.x0 && author.x1 <= rect.x1,
                        "an author line stays inside its list: {author:?} in {rect:?}"
                    );
                }
                checked += 1;
            }
        }
        assert_eq!(checked, 2);
    }
}
