//! [`ConversationList`]: the message list, with the mechanics underneath it.
//!
//! This is the list a widget-tree app draws. It is the same conversation the
//! painting apps compose themselves — the day breaks, the clock each line
//! carries, the delivery vocabulary, the way consecutive lines from one person
//! run together — so a message reads the same whether it arrived in a channel or
//! a private thread.
//!
//! What the list owns is the *mechanics*, not the app's identity: an app names
//! its lines ([`ConversationLine`]) and which side they sit on
//! ([`ConversationRole`]), and the list decides where the day changes, how wide
//! a bubble may grow, when a line is a continuation of the one above it, and
//! where the time and the delivery state go. A private thread puts your own
//! words on the right and the other person's on the left; a channel would put
//! everything on one side with its author's name over each run
//! (`ConversationRole::Incoming` for every line).
//!
//! The list is static: it paints and it measures. It takes no pointer, holds no
//! selection and keeps no state of its own, so a scroll view can measure it
//! exactly and a screen reader can walk it — every line is a list item with its
//! author, its time and its delivery state in its spoken name.

use std::collections::HashMap;

use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::tokens::colors::mix8;
use crate::tokens::contrast::BODY_TEXT_MIN_CONTRAST;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

use super::day::{ConversationClock, DayBreaks, DayDividerFace};
use super::delivery::{DeliveryFace, DeliveryState};

/// How long after the line before it a message from the same person still
/// continues that person's run (and loses its repeated name).
pub const RUN_WINDOW_SECS: i64 = 5 * 60;

/// The most of the column one bubble may take. A bubble that fills the column
/// is no longer a bubble: you lose the side that says who is talking.
const BUBBLE_COLUMN_SHARE: f64 = 0.78;

/// Which side of the column a line sits on, and what its bubble says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversationRole {
    /// The other person: left, with their name over the first line of a run.
    Incoming,
    /// You: right, with the delivery state under the last thing you see.
    Outgoing,
}

/// One line of a conversation, as the list draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct ConversationLine {
    /// Stable identity: a semantic key, and what a caller anchors a scroll to.
    pub id: String,
    /// When it was said, in milliseconds since the epoch.
    pub at_ms: i64,
    /// Who said it, as the person reads names. Empty for a line that
    /// knowingly continues the run above (a bridged message with no author,
    /// an import that lost one).
    pub author: String,
    pub body: String,
    pub role: ConversationRole,
    /// What the transport said about it, when it said anything.
    pub delivery: Option<DeliveryState>,
}

impl ConversationLine {
    /// A line from the other person.
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

    /// One of your own lines.
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

/// Which lines start a new run: the first always, and then any line whose
/// author or side differs from the one before, or that follows it by more than
/// [`RUN_WINDOW_SECS`].
#[must_use]
pub fn run_breaks(lines: &[ConversationLine]) -> Vec<bool> {
    let mut starts: Vec<bool> = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        let starts_run = match index.checked_sub(1).and_then(|before| lines.get(before)) {
            None => true,
            Some(previous) => {
                previous.author != line.author
                    || previous.role != line.role
                    || line.at_ms.saturating_sub(previous.at_ms).abs()
                        > RUN_WINDOW_SECS.saturating_mul(1_000)
                    || line.author.is_empty()
            }
        };
        starts.push(starts_run);
    }
    starts
}

/// One thing the list paints, in order.
#[derive(Clone, Debug, PartialEq)]
enum Row {
    /// A day divider: a band with its label, across the conversation column.
    Divider {
        y0: f64,
        y1: f64,
        x0: f64,
        x1: f64,
        label: String,
    },
    /// A message: its bubble, and the people and time above and below it.
    Line {
        index: usize,
        y0: f64,
        y1: f64,
        starts_run: bool,
        bubble: Rect,
        /// Where the body's first line starts.
        text: Point,
        text_width: f32,
        /// The band the author's name occupies, on a run's first line.
        author: Option<Rect>,
        /// The band under the bubble with the time and the delivery state.
        caption: Rect,
    },
}

impl Row {
    /// A message row's bubble and, on a run's first line, its author band.
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
}

/// The list's geometry, measured once and painted from the same measurements.
struct Layout {
    rows: Vec<Row>,
    height: f64,
}

/// The bubble and ink for a line on `theme`.
///
/// The other person's words sit on the raised tier — an object inside the
/// content ground. Yours sit on a wash of your own colour, strengthened until
/// body ink reads on it, so "mine" is the same mark here as your caret and your
/// selection elsewhere; the ink is measured, never assumed.
#[must_use]
pub fn bubble_colors(theme: &Theme, role: ConversationRole) -> (Color, Color) {
    let colors = &theme.tokens.colors;
    let ground = colors.surfaces.content;
    match role {
        ConversationRole::Incoming => {
            let fill = colors.surfaces.raised;
            (
                fill,
                colors.ink_on(fill, colors.text_primary, BODY_TEXT_MIN_CONTRAST),
            )
        }
        ConversationRole::Outgoing => {
            let own = colors.intent.you;
            let mut fill = mix8(own, ground, 0.16);
            for step in 1..=12_u8 {
                if crate::a11y::contrast_ratio(colors.text_primary, fill) >= BODY_TEXT_MIN_CONTRAST
                {
                    break;
                }
                fill = mix8(own, ground, 0.16 + f32::from(step) / 20.0);
            }
            (
                fill,
                colors.ink_on(fill, colors.text_primary, BODY_TEXT_MIN_CONTRAST),
            )
        }
    }
}

/// The list's painting and measuring, without the widget around it.
///
/// A painting app can use this directly when it wants the list's geometry and
/// mechanics but its own chrome around them.
pub struct ListFace<'a> {
    lines: &'a [ConversationLine],
    clock: ConversationClock,
}

impl<'a> ListFace<'a> {
    #[must_use]
    pub fn new(lines: &'a [ConversationLine], clock: ConversationClock) -> Self {
        Self { lines, clock }
    }

    /// Measure every row at `width`, from the origin `(x0, y0)`.
    fn layout(
        &self,
        text: &mut TextRenderer,
        theme: &Theme,
        x0: f64,
        width: f64,
        y0: f64,
    ) -> Layout {
        let spacing = &theme.tokens.spacing;
        let author_style = theme.text(TextRole::Caption);
        let caption_style = theme.text(TextRole::Caption).numeric();
        let gap = f64::from(spacing.s1);
        let run_gap = f64::from(spacing.s4);
        let pad_x = f64::from(spacing.s3);
        let pad_y = f64::from(spacing.s2);
        let gutter = f64::from(spacing.s2);
        let readable = f64::from(theme.tokens.metrics.readable_width);
        let floor = f64::from(theme.tokens.spacing.s16);
        // The conversation column: the room the list leaves, but never wider
        // than the readable measure, and centred in what is left over. Two
        // bubbles that hug the edges of a wide window are two bubbles nobody
        // reads as a conversation.
        let room_left = x0 + gutter;
        let room_right = x0 + width - gutter;
        let column = (room_right - room_left).min(readable).max(floor);
        let left = room_left + ((room_right - room_left) - column).max(0.0) / 2.0;
        let right = left + column;
        let bubble_max = (column * BUBBLE_COLUMN_SHARE).min(readable).max(floor);
        let inner_max = (bubble_max - 2.0 * pad_x).max(floor);
        let author_line = f64::from(text.measure_style("Ag", None, &author_style).1);
        let caption_line = f64::from(text.measure_style("Ag", None, &caption_style).1);

        let mut rows: Vec<Row> = Vec::new();
        let mut days = DayBreaks::new();
        let mut y = y0;
        let starts = run_breaks(self.lines);
        for (index, line) in self.lines.iter().enumerate() {
            let day = self.clock.day(seconds(line.at_ms));
            if days.begins(day) {
                let height = DayDividerFace::height(theme);
                rows.push(Row::Divider {
                    y0: y,
                    y1: y + height,
                    x0: left,
                    x1: right,
                    label: self.clock.day_label(day),
                });
                y += height;
            }
            let starts_run = starts.get(index).copied().unwrap_or(true);
            y += if starts_run { run_gap } else { gap };
            let author =
                (starts_run && line.role == ConversationRole::Incoming && !line.author.is_empty())
                    .then(|| {
                        let band = Rect::new(left, y, right, y + author_line);
                        y += author_line + f64::from(spacing.s1);
                        band
                    });
            let wrap = crate::num::f32_of_f64(inner_max);
            let (text_w, text_h) =
                text.measure_style(&line.body, Some(wrap), &theme.text(TextRole::Body));
            let used_w = f64::from(text_w).min(inner_max);
            let bubble_w = (used_w + 2.0 * pad_x).min(bubble_max);
            let bubble_h = f64::from(text_h) + 2.0 * pad_y;
            let bubble_x = match line.role {
                ConversationRole::Incoming => left,
                ConversationRole::Outgoing => (right - bubble_w).max(left),
            };
            let bubble = Rect::new(bubble_x, y, bubble_x + bubble_w, y + bubble_h);
            y = bubble.y1 + gap;
            let caption = Rect::new(bubble.x0, y, bubble.x1, y + caption_line);
            y = caption.y1;
            rows.push(Row::Line {
                index,
                y0: rows.last().map_or(y0, Row::y1),
                y1: y,
                starts_run,
                bubble,
                text: Point::new(bubble.x0 + pad_x, bubble.y0 + pad_y),
                text_width: crate::num::f32_of_f64((bubble_w - 2.0 * pad_x).max(1.0)),
                author,
                caption,
            });
        }
        Layout {
            rows,
            height: y - y0,
        }
    }

    /// The rows this list lays out in `rect`, for a test of the geometry.
    #[cfg(test)]
    fn layout_for_test(&self, text: &mut TextRenderer, theme: &Theme, rect: Rect) -> Layout {
        self.layout(text, theme, rect.x0, rect.width(), rect.y0)
    }

    /// The list's height at `width`.
    pub fn height(&self, text: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        self.layout(text, theme, 0.0, width, 0.0).height
    }

    /// Paint the list from `rect`'s top left, `rect`'s width wide.
    pub fn paint(&self, paint: &mut PaintCtx<'_>, rect: Rect) {
        let layout = self.layout(paint.text, paint.theme, rect.x0, rect.width(), rect.y0);
        self.paint_layout(paint, &layout);
    }

    /// Paint rows already measured by [`Self::layout`], so a caller that needs
    /// the geometry (a list storing its rows for its accessible nodes) lays
    /// out once.
    fn paint_layout(&self, paint: &mut PaintCtx<'_>, layout: &Layout) {
        let theme = paint.theme;
        let radius = f64::from(theme.tokens.radii.control);
        for row in &layout.rows {
            match row {
                Row::Divider {
                    y0,
                    y1,
                    x0,
                    x1,
                    label,
                } => {
                    let band = Rect::new(*x0, *y0, *x1, *y1);
                    DayDividerFace::new(label).paint(paint, band, band.x0, band.x1);
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
                    self.paint_caption(paint, line, *caption);
                }
            }
        }
    }

    /// The time, and the delivery state of one of your own lines, under its
    /// bubble.
    fn paint_caption(&self, paint: &mut PaintCtx<'_>, line: &ConversationLine, band: Rect) {
        let theme = paint.theme;
        let style = theme.text(TextRole::Caption).numeric();
        let time = self.clock.clock(seconds(line.at_ms));
        let time_w = f64::from(paint.text.measure_style(&time, None, &style).0);
        let mut x = match line.role {
            ConversationRole::Incoming => band.x0,
            ConversationRole::Outgoing => (band.x1 - time_w).max(band.x0),
        };
        let line_h = f64::from(paint.text.measure_style("Ag", None, &style).1);
        let top = band.center().y - line_h / 2.0;
        paint.text(&time, &style, theme.text_tertiary, Point::new(x, top), None);
        x += time_w;
        if let Some(state) = line.delivery {
            let face = DeliveryFace::new(state);
            let gap = f64::from(theme.tokens.spacing.s2);
            let width = face.width(paint.text, theme);
            let start = match line.role {
                ConversationRole::Outgoing => band.x1 - time_w - gap - width,
                ConversationRole::Incoming => x + gap,
            };
            face.paint(paint, start, band);
        }
    }

    /// What a line says to a screen reader: who, when, whether it arrived, and
    /// what it says.
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

/// The message list: a conversation, drawn. See the module docs.
pub struct ConversationList {
    lines: Vec<ConversationLine>,
    rect: Rect,
    name: String,
    theme: Option<Theme>,
    /// The rows the last paint laid out, so an assistive-technology node
    /// carries the bounds of the row it names rather than the list's.
    rows: Vec<Row>,
}

impl ConversationList {
    /// A list of `lines`, oldest first.
    #[must_use]
    pub fn new(lines: Vec<ConversationLine>) -> Self {
        Self {
            lines,
            rect: Rect::ZERO,
            name: String::new(),
            theme: None,
            rows: Vec::new(),
        }
    }

    /// The list's accessible name ("Conversation with Ada").
    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
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
        let layout = face.layout(text, theme, self.rect.x0, self.rect.width(), self.rect.y0);
        layout.rows.iter().find_map(|row| match row {
            Row::Line {
                index: at, bubble, ..
            } if *at == index => Some(*bubble),
            _ => None,
        })
    }
}

impl Widget for ConversationList {
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
        let face = ListFace::new(&self.lines, ConversationClock::local());
        let height = face.height(text_renderer, theme, f64::from(width));
        taffy::Size {
            width,
            height: crate::num::f32_of_f64(height),
        }
    }

    crate::rect_geometry!();

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
        let face = ListFace::new(&self.lines, ConversationClock::local());
        let mut paint = PaintCtx::new(scene, transform, text_renderer, theme);
        let layout = face.layout(
            paint.text,
            theme,
            self.rect.x0,
            self.rect.width(),
            self.rect.y0,
        );
        face.paint_layout(&mut paint, &layout);
        self.rows = layout.rows;
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
        let children: Vec<AccessNode> = self
            .lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                // The row's own band, from the last paint: a list item that
                // names a line must point at where that line is.
                let row = self
                    .rows
                    .iter()
                    .find_map(|row| match row {
                        Row::Line {
                            index: at, y0, y1, ..
                        } if *at == index => Some(Rect::new(self.rect.x0, *y0, self.rect.x1, *y1)),
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
mod geometry_tests {
    use super::*;

    /// Every row a list lays out sits inside the list it was given: a bubble
    /// that escapes its column is a bubble drawn over its neighbours.
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
