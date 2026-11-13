//! Where a row puts what it holds: the height it asks for and the rect of every column — leading,
//! text, meta and trailing — so a retained row and the immediate [`ListRowFace`] agree.

use vello::kurbo::{Point, Rect};

use super::{ListRowFace, RowGeometry, RowLeading, RowMetrics, RowValue};
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::widget::BadgeFace;

/// Where a row led by a mark — a glyph or a face — starts its text, past the left edge of its
/// rect: the content edge, the mark's slot and the gap to the text. Both lead in one slot, so a
/// list's titles keep one column; a row's own line under its title starts here too.
#[must_use]
pub fn mark_text_inset(theme: &Theme) -> f64 {
    let m = RowMetrics::new(theme);
    m.bleed + m.icon + m.column_gap
}

impl ListRowFace<'_> {
    /// Whether the row says anything under its title: a secondary line, or facts standing where
    /// one would.
    pub(super) fn has_secondary_line(&self) -> bool {
        self.secondary.is_some() || !self.facts.is_empty()
    }

    pub(super) fn extra_lines(&self) -> u8 {
        let secondary = if self.has_secondary_line() {
            self.secondary_lines
        } else {
            0
        };
        secondary
            + u8::from(self.note.is_some())
            + u8::from(self.subtitle.is_some())
            + u8::from(self.change.is_some_and(|change| !change.said.is_empty()))
            + (self.title_lines - 1)
    }

    pub(super) fn title_style(&self, m: &RowMetrics) -> TextStyle {
        let style = if self.emphasized {
            m.title_strong.clone()
        } else {
            m.title.clone()
        };
        match self.title_family {
            Some(family) => style.with_family(family.clone()),
            None => style,
        }
    }

    /// The text block's height: the title lines, the subtitle and the
    /// secondary lines.
    /// Where the status dot's centre stands: on the title's line, as a
    /// presence dot beside a name; beside a check or chevron it centres with
    /// them, so the trailing column stands on one line.
    pub(super) fn dot_cy(geo: &RowGeometry, row: Rect, extra: bool, title_cy: f64) -> f64 {
        let accessory = geo.disclosure.is_some() || geo.check.is_some();
        if extra && !accessory {
            title_cy
        } else {
            row.center().y
        }
    }

    /// The status dot's centre in `row`, as painted.
    #[cfg(test)]
    pub(crate) fn dot_centre(
        &self,
        row: Rect,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> Option<vello::kurbo::Point> {
        let m = self.metrics(theme);
        let geo = self.geometry(row, &m, text, theme);
        let x = geo.dot_x?;
        let block = self.text_block(text, &m);
        let title_line = RowMetrics::line(text, &self.title_style(&m));
        let title_cy = row.center().y - block / 2.0 + title_line / 2.0;
        Some(vello::kurbo::Point::new(
            x,
            Self::dot_cy(&geo, row, self.extra_lines() > 0, title_cy),
        ))
    }

    pub(super) fn text_block(&self, text: &mut TextRenderer, m: &RowMetrics) -> f64 {
        let title = RowMetrics::line(text, &self.title_style(m)) * f64::from(self.title_lines);
        let subtitle = if self.subtitle.is_some() {
            RowMetrics::line(text, &m.subtitle)
        } else {
            0.0
        };
        let secondary = if self.has_secondary_line() {
            f64::from(self.secondary_lines) * RowMetrics::line(text, &m.secondary)
        } else {
            0.0
        };
        let note = if self.note.is_some() {
            RowMetrics::line(text, &m.secondary)
        } else {
            0.0
        };
        let said = if self.change.is_some_and(|change| !change.said.is_empty()) {
            RowMetrics::line(text, &m.secondary)
        } else {
            0.0
        };
        title + subtitle + secondary + note + said
    }

    pub(super) fn leading_extent(
        &self,
        m: &RowMetrics,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> (f64, f64) {
        let extra = self.extra_lines() > 0;
        let avatar = RowMetrics::avatar(theme, extra);
        let own = match self.leading {
            None if self.nested => (m.icon, m.icon),
            None => (0.0, 0.0),
            Some(RowLeading::Icon(_) | RowLeading::Swatch(_) | RowLeading::Signal(_)) => {
                (m.icon, m.icon)
            }
            // A face takes the glyph's slot, so a list's titles line up whichever a row leads
            // with; its ring and ground stand in the gap to the text.
            Some(RowLeading::Face(_)) => (m.icon, m.icon),
            // A box says whether it is done, and a glyph after it what kind of thing it is.
            Some(RowLeading::Checkbox { icon, .. }) => match icon {
                Some(_) => (2.0 * m.icon + m.column_gap, m.icon),
                None => (m.icon, m.icon),
            },
            Some(
                RowLeading::Avatar(_)
                | RowLeading::Thumbnail(_)
                | RowLeading::Paint(_)
                | RowLeading::AvatarSpace,
            ) => (avatar, avatar),
            Some(RowLeading::Media(_, aspect)) => ((avatar * aspect.ratio()).ceil(), avatar),
            Some(RowLeading::Badge(badge)) => {
                let width = super::badge_column_width(text, theme, &[badge]);
                (width, m.icon)
            }
        };
        if self.leading.is_none() {
            return own;
        }
        (own.0.max(self.leading_column.unwrap_or(0.0)), own.1)
    }

    /// The row's height at this text scale.
    pub fn height(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let m = self.metrics(theme);
        if self.card {
            return self.card_height(text, &m, theme);
        }
        let (_, leading_h) = self.leading_extent(&m, text, theme);
        let fact_avatar = self.fact_avatar(theme).unwrap_or(0.0);
        let value = match self.value {
            Some((value, RowValue::Title)) => {
                f64::from(text.measure_style(value, None, &m.value_title).1)
            }
            _ => 0.0,
        };
        let big_leading = fact_avatar > 0.0
            || matches!(
                self.leading,
                Some(
                    RowLeading::Avatar(_)
                        | RowLeading::Thumbnail(_)
                        | RowLeading::Media(..)
                        | RowLeading::AvatarSpace
                )
            );
        let held = self
            .text_block(text, &m)
            .max(leading_h)
            .max(value)
            .max(fact_avatar);
        // A row given its own height stands at that token and insets what it holds by whatever
        // the token leaves; a tree's rows are shorter than a list's rows of the same theme.
        if let Some(height) = self.row_height {
            return held.max(height).max(self.min_target).ceil();
        }
        // A dense row is as tall as its own line and inset, never the list's row height.
        let min = if self.dense {
            self.min_target
        } else {
            RowMetrics::min_height_for(theme, self.extra_lines() > 0, big_leading)
                .max(m.min_height)
                .max(self.min_target)
        };
        (held + 2.0 * m.pad_y).max(min).ceil()
    }

    /// A card's height: its title's lines and what follows them, the facts on their own line
    /// under that, and the card's own padding around both.
    fn card_height(&self, text: &mut TextRenderer, m: &RowMetrics, theme: &Theme) -> f64 {
        let (_, leading_h) = self.leading_extent(m, text, theme);
        let facts = self.facts_height(text, m, theme);
        let gap = if facts > 0.0 {
            f64::from(theme.tokens.spacing.rhythm.tight)
        } else {
            0.0
        };
        (self.text_block(text, m) + gap + facts + 2.0 * m.pad_y)
            .max(leading_h + 2.0 * m.pad_y)
            .max(self.min_target)
            .ceil()
    }

    /// Whether the card has facts to stand under its title: a day, who, how far, how important.
    fn has_facts(&self) -> bool {
        self.meta.is_some()
            || !self.meta_spans.is_empty()
            || self.avatar.is_some()
            || self.count.is_some()
            || self.marks.iter().flatten().next().is_some()
            || self.value.is_some()
    }

    /// The height of the line the facts stand on: the tallest of them, nothing when the card
    /// holds none.
    fn facts_height(&self, text: &mut TextRenderer, m: &RowMetrics, theme: &Theme) -> f64 {
        if !self.has_facts() {
            return 0.0;
        }
        let value = self.value.map_or(0.0, |(value, size)| {
            let style = match size {
                RowValue::Body => &m.value_body,
                RowValue::Title => &m.value_title,
            };
            f64::from(text.measure_style(value, None, style).1)
        });
        let count = self
            .count_label()
            .map_or(0.0, |_| f64::from(BadgeFace::height(theme)));
        RowMetrics::line(text, &m.meta)
            .max(self.fact_avatar(theme).unwrap_or(0.0))
            .max(count)
            .max(m.mark)
            .max(value)
    }

    /// How many lines the title wraps to in a card `width` wide, up to [`CARD_TITLE_LINES`]:
    /// a card is as tall as its title needs, and never cuts a title that fits.
    pub(super) fn card_title_lines(
        &self,
        width: f64,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> u8 {
        let m = RowMetrics::card(theme);
        let left = m.bleed;
        let (lead_w, _) = self.leading_extent(&m, text, theme);
        let text_x = if lead_w > 0.0 {
            left + lead_w + m.column_gap
        } else {
            left
        };
        let room = (width - m.bleed - text_x).max(0.0);
        let lines = text.clamp_to_lines(
            self.title,
            &self.title_style(&m),
            crate::num::f32_of_f64(room),
            usize::from(super::CARD_TITLE_LINES),
        );
        let lines = u8::try_from(lines.len()).unwrap_or(1);
        lines.clamp(1, super::CARD_TITLE_LINES)
    }

    pub(super) fn shows_actions(&self) -> bool {
        !self.actions.is_empty()
            && !self.interaction.disabled
            && (self.interaction.hovered || self.interaction.focused || self.reveal_actions)
    }

    pub(super) fn value_width(&self, m: &RowMetrics, text: &mut TextRenderer) -> f64 {
        self.value.map_or(0.0, |(value, size)| {
            let style = match size {
                RowValue::Body => &m.value_body,
                RowValue::Title => &m.value_title,
            };
            f64::from(text.measure_style(value, None, style).0)
        })
    }

    pub(super) fn count_label(&self) -> Option<String> {
        self.count.map(|count| count.to_string())
    }

    /// The side of the avatar in the facts column, where the row shows one: the inline size,
    /// as a fact's line is. A row without one takes no room there: its other facts run on
    /// together.
    pub(super) fn fact_avatar(&self, theme: &Theme) -> Option<f64> {
        // A fact stands on a line of its own height: the inline size, however many lines the
        // row's text takes, so a person's face reads beside the other facts, not as a leading
        // avatar over them.
        self.avatar
            .is_some()
            .then(|| RowMetrics::avatar(theme, false))
    }

    /// The width of the marks' run, the gaps between them included.
    pub(super) fn marks_width(&self, m: &RowMetrics) -> f64 {
        #[allow(
            clippy::cast_precision_loss,
            reason = "the marks of a row are at most MAX_MARKS"
        )]
        let n = self.marks.iter().flatten().count() as f64;
        n * m.mark + (n - 1.0).max(0.0) * m.tag_gap
    }

    /// How far inside the row's edge its trailing slot ends: its own marks
    /// after it, or the room its list keeps there if that is wider.
    pub(super) fn after_trailing(&self, m: &RowMetrics) -> f64 {
        self.marks_after_trailing(m).max(self.trailing_after)
    }

    /// The width of the columns [`Self::geometry`] lays out after the
    /// trailing slot (a disclosure chevron, a check, a status dot), each
    /// with its gap.
    pub(super) fn marks_after_trailing(&self, m: &RowMetrics) -> f64 {
        let mut width = 0.0;
        if self.disclosure {
            width += m.icon + m.column_gap;
        }
        if self.checked.is_some() {
            width += m.icon + m.column_gap;
        }
        if self.status.is_some() {
            width += m.dot + m.column_gap;
        }
        width
    }

    /// Lay the columns out in `rect`, the row's outer rect.
    pub(super) fn geometry(
        &self,
        rect: Rect,
        m: &RowMetrics,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> RowGeometry {
        if self.card {
            return self.card_geometry(rect, m, text, theme);
        }
        // A nested row indents its content one step per depth, its lift still filling the row.
        // A tree's chevron hangs in the step in front of its row's content: the list leads every
        // row's depth's column by one step, so the top level stands where the list's own column
        // starts and nothing indents it twice. A row that keeps a chevron column inside its own
        // box holds that column first, and its content starts past it.
        let left = rect.x0 + m.bleed + self.chevron_column + f64::from(self.depth) * m.nest;
        let mut right = rect.x1 - m.bleed;
        let cy = rect.center().y;
        let mut geo = RowGeometry::default();
        let (lead_w, lead_h) = self.leading_extent(m, text, theme);
        geo.leading = Rect::new(left, cy - lead_h / 2.0, left + lead_w, cy + lead_h / 2.0);
        geo.text_x = if lead_w > 0.0 {
            left + lead_w + m.column_gap
        } else {
            left
        };
        if self.disclosure {
            geo.disclosure = Some(Rect::new(
                right - m.icon,
                cy - m.icon / 2.0,
                right,
                cy + m.icon / 2.0,
            ));
            right -= m.icon + m.column_gap;
        }
        if self.checked.is_some() {
            geo.check = Some(Rect::new(
                right - m.icon,
                cy - m.icon / 2.0,
                right,
                cy + m.icon / 2.0,
            ));
            right -= m.icon + m.column_gap;
        }
        if self.status.is_some() {
            geo.dot_x = Some(right - m.dot / 2.0);
            right -= m.dot + m.column_gap;
        }
        if let Some(width) = self.trailing_width
            && width > 0.0
        {
            right = right.min(rect.x1 - m.bleed - self.after_trailing(m));
            geo.trailing = Some(Rect::new(right - width, rect.y0, right, rect.y1));
            right -= width + m.column_gap;
        }
        // The facts stand in one column, right-aligned on the edge the trailing slot leaves:
        // when, who, how far, how important, in that order from the text to the row's end. A
        // value of the row's own is no fact: it keeps the edge, the facts stand inside it.
        if self.value.is_some() {
            geo.value_right = right;
            right -= self.value_width(m, text) + m.column_gap;
        }
        let facts_right = right;
        let actions = self.shows_actions().then(|| {
            let n = self.actions.len() as f64;
            n * m.action + (n - 1.0).max(0.0) * m.action_gap
        });
        if let Some(width) = actions {
            // The hover actions take the facts' place: nothing else stands on that edge while
            // they show.
            geo.meta_right = facts_right;
            geo.meta_left = facts_right;
            geo.text_right = facts_right - width - m.column_gap;
            geo.title_right = geo.text_right;
            geo.second_right = geo.text_right;
            return geo;
        }
        let marks = self.marks_width(m);
        if marks > 0.0 {
            geo.marks_right = Some(right);
            right -= marks + m.column_gap;
        }
        if let Some(label) = self.count_label() {
            let pill = BadgeFace::intrinsic_width(&label, text, theme);
            geo.count_right = Some(right);
            right -= pill + m.column_gap;
        }
        if let Some(side) = self.fact_avatar(theme) {
            geo.avatar = Some(Rect::new(
                right - side,
                cy - side / 2.0,
                right,
                cy + side / 2.0,
            ));
            right -= side + m.column_gap;
        }
        geo.meta_right = right;
        let meta_w = self.meta_width(m, text, ((right - left).max(0.0)) / 2.0);
        geo.meta_left = right - meta_w;
        if meta_w > 0.0 {
            right -= meta_w + m.column_gap;
        }
        geo.text_right = right;
        geo.title_right = right;
        geo.second_right = right;
        // The changer's face leads the line under the title, before the words it says.
        geo.change = self.said_face(rect, &geo, m, text, theme);
        geo
    }

    /// Where the face of whoever changed the row sits: the start of the line under the title,
    /// as tall as an icon, the words it leads after it. `None` while the row says nothing of a
    /// change.
    fn said_face(
        &self,
        rect: Rect,
        geo: &RowGeometry,
        m: &RowMetrics,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> Option<Rect> {
        self.change.filter(|change| !change.said.is_empty())?;
        let title_style = self.title_style(m);
        let title_line = RowMetrics::line(text, &title_style);
        let secondary_line = RowMetrics::line(text, &m.secondary);
        let reserved = usize::from(self.title_lines);
        let room = self.title_room(geo, text, theme, m);
        let shown = self.shown_title();
        let drawn = text
            .clamp_to_lines(&shown, &title_style, room, reserved)
            .len()
            .clamp(1, reserved);
        // The row keeps the height of every title line it allows; the lines it draws centre
        // the block, as the row paints them.
        let unused = reserved - drawn;
        let block = self.text_block(text, m) - title_line * crate::num::f64_of_usize(unused);
        let text_top = if self.card {
            self.card_content_top(rect, m, text, theme)
        } else {
            rect.center().y - block / 2.0
        };
        let mut top = text_top + title_line * crate::num::f64_of_usize(drawn);
        if self.subtitle.is_some() {
            top += RowMetrics::line(text, &m.subtitle);
        }
        if self.secondary.is_some() {
            top += secondary_line * f64::from(self.secondary_lines);
        }
        let cy = top + secondary_line / 2.0;
        Some(Rect::new(
            geo.text_x,
            cy - m.icon / 2.0,
            geo.text_x + m.icon,
            cy + m.icon / 2.0,
        ))
    }

    /// Where a card's content starts: under its own padding, and centred in the card where
    /// the row's minimum height (the target a finger needs) leaves it taller than the
    /// content it holds, so a grown card carries no dead space under its text.
    pub(super) fn card_content_top(
        &self,
        rect: Rect,
        m: &RowMetrics,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        let held = self.card_content_height(m, text, theme);
        rect.y0 + m.pad_y + ((rect.height() - 2.0 * m.pad_y - held) / 2.0).max(0.0)
    }

    /// The height of what a card holds: its text, the gap and the facts line under it, or
    /// its leading column where that is taller.
    fn card_content_height(&self, m: &RowMetrics, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let facts = self.facts_height(text, m, theme);
        let gap = if facts > 0.0 {
            f64::from(theme.tokens.spacing.rhythm.tight)
        } else {
            0.0
        };
        let (_, lead_h) = self.leading_extent(m, text, theme);
        (self.text_block(text, m) + gap + facts).max(lead_h)
    }

    /// Lay a card out in `rect`, its own surface: the checkbox hangs on the title's first line,
    /// the text takes the rest of the card's width, and the facts stand on one line under the
    /// text, read left to right in the order a row keeps them — the day, who, how far, how
    /// important — so they sit together under the title rather than spread across it.
    fn card_geometry(
        &self,
        rect: Rect,
        m: &RowMetrics,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> RowGeometry {
        let left = rect.x0 + m.bleed;
        let right = (rect.x1 - m.bleed).max(left);
        let (lead_w, lead_h) = self.leading_extent(m, text, theme);
        let title_line = RowMetrics::line(text, &self.title_style(m));
        let top = self.card_content_top(rect, m, text, theme);
        let first_cy = top + title_line / 2.0;
        let mut geo = RowGeometry {
            leading: Rect::new(
                left,
                first_cy - lead_h / 2.0,
                left + lead_w,
                first_cy + lead_h / 2.0,
            ),
            text_x: if lead_w > 0.0 {
                left + lead_w + m.column_gap
            } else {
                left
            },
            ..RowGeometry::default()
        };
        geo.text_right = right;
        geo.title_right = right;
        geo.second_right = right;
        let block = self.text_block(text, m);
        let facts = self.facts_height(text, m, theme);
        if facts <= 0.0 {
            return geo;
        }
        // The hover actions take the facts' place, on the facts line's trailing edge.
        if self.shows_actions() {
            geo.meta_right = right;
            geo.meta_left = right;
            geo.facts_cy = top + block + f64::from(theme.tokens.spacing.rhythm.tight) + facts / 2.0;
            return geo;
        }
        let gap = f64::from(theme.tokens.spacing.rhythm.tight);
        let mut cy = top + block + gap + facts / 2.0;
        // A card is as tall as its facts line; the facts centre on it.
        cy = cy
            .min(rect.y1 - m.pad_y - facts / 2.0)
            .max(rect.y0 + m.pad_y + facts / 2.0);
        geo.facts_cy = cy;
        // The facts that stand: only those the card has, in the row's order, a single spacing
        // token apart. An absent fact takes no room, so the run never leaves a hole.
        let avatar = self.fact_avatar(theme);
        let count = self
            .count_label()
            .map(|label| BadgeFace::intrinsic_width(&label, text, theme));
        let marks = self.marks_width(m);
        let tail = avatar.unwrap_or(0.0) + count.unwrap_or(0.0) + marks;
        let tail_items =
            usize::from(avatar.is_some()) + usize::from(count.is_some()) + usize::from(marks > 0.0);
        let has_meta = self.meta.is_some() || !self.meta_spans.is_empty();
        let items = tail_items + usize::from(has_meta);
        #[allow(clippy::cast_precision_loss, reason = "a card's facts are a handful")]
        let gaps = items.saturating_sub(1) as f64 * m.tag_gap;
        let meta = self.meta_width(m, text, (right - geo.text_x - tail - gaps).max(0.0));
        let mut x = geo.text_x;
        let mut gap = 0.0;
        if meta > 0.0 {
            geo.meta_left = x;
            geo.meta_right = x + meta;
            x += meta;
            gap = m.tag_gap;
        }
        if let Some(side) = avatar {
            let at = x + gap;
            geo.avatar = Some(Rect::new(at, cy - side / 2.0, at + side, cy + side / 2.0));
            x = at + side;
            gap = m.tag_gap;
        }
        if let Some(width) = count {
            let at = x + gap;
            geo.count_right = Some(at + width);
            x = at + width;
            gap = m.tag_gap;
        }
        if marks > 0.0 {
            geo.marks_right = Some(x + gap + marks);
        }
        // A value of the row's own keeps the card's trailing edge.
        if self.value.is_some() {
            geo.value_right = right;
        }
        geo
    }

    /// The meta's painted width, capped at `cap`.
    pub(super) fn meta_width(&self, m: &RowMetrics, text: &mut TextRenderer, cap: f64) -> f64 {
        if !self.meta_spans.is_empty() {
            let gap = m.tag_gap;
            let width: f64 = self
                .meta_spans
                .iter()
                .map(|(span, _)| f64::from(text.measure_style(span, None, &m.meta).0))
                .sum::<f64>()
                + gap * (self.meta_spans.len() as f64 - 1.0).max(0.0);
            return width.min(cap);
        }
        self.meta.map_or(0.0, |meta| {
            let shown = text.elide_to_width_style(meta, crate::num::f32_of_f64(cap), &m.meta);
            f64::from(text.measure_style(&shown, None, &m.meta).0)
        })
    }

    /// Where each hover action sits in `rect`, in order. Empty unless the
    /// row is hovered, focused or revealing its actions.
    pub fn action_rects(&self, rect: Rect, theme: &Theme) -> Vec<Rect> {
        crate::text_render::with_layout_renderer(theme, |text| {
            self.action_rects_with(rect, &self.metrics(theme), text, theme)
        })
    }

    pub(super) fn action_rects_with(
        &self,
        rect: Rect,
        m: &RowMetrics,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> Vec<Rect> {
        if !self.shows_actions() {
            return Vec::new();
        }
        self.action_slots_with(rect, m, text, theme)
    }

    /// Where each of the row's actions stands in `rect`, in order, whether
    /// or not the row shows them: assistive technology reaches an action
    /// without hovering the row, so its node says where it is.
    pub(super) fn action_slots_with(
        &self,
        rect: Rect,
        m: &RowMetrics,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> Vec<Rect> {
        if self.actions.is_empty() || self.interaction.disabled {
            return Vec::new();
        }
        let geo = self.geometry(rect, m, text, theme);
        let mut right = geo.meta_right;
        let cy = if self.card {
            geo.facts_cy
        } else {
            rect.center().y
        };
        let mut rects: Vec<Rect> = self
            .actions
            .iter()
            .rev()
            .map(|_| {
                let r = Rect::new(
                    right - m.action,
                    cy - m.action / 2.0,
                    right,
                    cy + m.action / 2.0,
                );
                right -= m.action + m.action_gap;
                r
            })
            .collect();
        rects.reverse();
        rects
    }

    /// The slot [`Self::trailing_width`] reserved in `rect`, as tall as the
    /// row; centre the control in it.
    pub fn trailing_rect(
        &self,
        rect: Rect,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> Option<Rect> {
        self.geometry(rect, &self.metrics(theme), text, theme)
            .trailing
    }

    /// Where the row's text starts in `rect`: the column its title, its lines and its facts keep,
    /// whatever it leads with. A block that belongs under the row's title — a question its own
    /// control asks — starts here too.
    pub fn text_column(&self, rect: Rect, text: &mut TextRenderer, theme: &Theme) -> f64 {
        self.text_x(rect, &self.metrics(theme), text, theme)
    }

    /// The leading column's rect in `rect`: the checkbox's hit target, or
    /// the badge a tooltip explains.
    pub fn leading_rect(&self, rect: Rect, text: &mut TextRenderer, theme: &Theme) -> Rect {
        let m = self.metrics(theme);
        let geo = self.geometry(rect, &m, text, theme);
        let Some(RowLeading::Checkbox { icon, .. }) = self.leading else {
            return geo.leading;
        };
        let hit = match icon {
            // The whole leading column, row-tall: a box is a small target.
            None => Rect::new(geo.leading.x0 - m.bleed, rect.y0, geo.text_x, rect.y1),
            // Only the box takes the press; the glyph after it is what the row is, unless the
            // target grown below reaches it.
            Some(_) => Rect::new(
                geo.leading.x0 - m.bleed,
                rect.y0,
                geo.leading.x0 + m.icon,
                rect.y1,
            ),
        };
        // `rect` is the row's band, which bleeds into the list's gutter as the rest of the row
        // does: a press on the box stays in it.
        self.reach_target(hit, checkbox_box(geo.leading, icon, &m), rect)
    }

    /// `hit` grown about `boxed` to the smallest target a pointer needs, kept inside `row` by
    /// moving it, never by shrinking it: a press at the row's edge still reaches all of it. A row
    /// smaller than the target (a band measured for another pointer, a dense row) gives the
    /// target all the same, centred on the row: a press is not painted, so it reaches past the
    /// row's band rather than under the least the person's pointer presses.
    fn reach_target(&self, hit: Rect, boxed: Rect, row: Rect) -> Rect {
        let side = self.check_target;
        let grown = hit.union(Rect::from_center_size(boxed.center(), (side, side)));
        let along = |centre: f64, size: f64, from: f64, to: f64| {
            if to - from >= size {
                (from + size / 2.0).max((to - size / 2.0).min(centre))
            } else {
                (from + to) / 2.0
            }
        };
        let centre = grown.center();
        Rect::from_center_size(
            Point::new(
                along(centre.x, grown.width(), row.x0, row.x1),
                along(centre.y, grown.height(), row.y0, row.y1),
            ),
            (grown.width(), grown.height()),
        )
    }

    /// Where the title starts: the inset separator starts here too.
    pub(super) fn text_x(
        &self,
        rect: Rect,
        m: &RowMetrics,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        self.geometry(rect, m, text, theme).text_x
    }
}

/// Where a row draws the box it leads with, in `slot` (its leading column): first in the column,
/// with the glyph after it where the row says what kind of thing it is.
pub(super) fn checkbox_box(slot: Rect, icon: Option<crate::icons::Icon>, m: &RowMetrics) -> Rect {
    match icon {
        Some(_) => Rect::new(
            slot.x0,
            slot.center().y - m.icon / 2.0,
            slot.x0 + m.icon,
            slot.center().y + m.icon / 2.0,
        ),
        None => Rect::from_center_size(slot.center(), (m.icon, m.icon)),
    }
}
