//! Painting a list row: its face (ground, lift, leading slot, text, trailing columns and
//! the strike-through of a removed item) and the short classifying code a leading pill
//! shows, which a navigation row shares.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use super::{
    ListRowFace, RowGeometry, RowLeading, RowMark, RowMetrics, RowValue, SwipeAction, found_spans,
};
use crate::{widget::BadgeFace, widget::MediaArt};
use tilo_ui_base::icons::Icon;
use tilo_ui_base::paint::PaintCtx;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::Tone;
use tilo_ui_base::{chrome::RowJoin, chrome::SelectionState, chrome::paint_row_selection_joined};
use tilo_ui_base::{
    tokens::colors::NON_TEXT_MIN_CONTRAST, tokens::colors::SMALL_TEXT_MIN_CONTRAST,
};

/// Where a row's words stand ([`ListRowFace::text_frame`]).
pub(super) struct TextFrame {
    pub(super) geo: RowGeometry,
    /// It shows lines under its title.
    pub(super) extra: bool,
    pub(super) title_style: tilo_ui_base::text_render::TextStyle,
    pub(super) title_line: f64,
    /// Where the title's words start, past the quiet word before them.
    pub(super) title_x: f64,
    /// The title as the lines it takes, the last cut short when it does not fit.
    pub(super) lines: Vec<String>,
    pub(super) text_top: f64,
    pub(super) title_cy: f64,
    /// The line the value, the meta and the marks are centred on.
    pub(super) facts_cy: f64,
}

pub(super) fn paint_badge(ctx: &mut PaintCtx<'_>, _m: &RowMetrics, column: Rect, badge: &str) {
    paint_code_badge(ctx.scene, ctx.transform, ctx.text, ctx.theme, column, badge);
}

/// A short classifying code (`GET`, `.rs`) on its quiet pill at the leading edge of
/// `column`, centred on the column's line: the one badge a list row and a navigation row
/// share.
pub(crate) fn paint_code_badge(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    column: Rect,
    badge: &str,
) {
    let style = RowMetrics::new(theme).badge;
    let (width, height) = text.measure_style(badge, None, &style);
    let pad = f64::from(theme.tokens.spacing.s1);
    let pill = Rect::from_origin_size(
        (
            column.x0,
            column.center().y - f64::from(height) / 2.0 - pad / 2.0,
        ),
        (f64::from(width) + 2.0 * pad, f64::from(height) + pad),
    );
    let raised = theme.tokens.colors.surfaces.raised;
    tilo_ui_base::surface::card(
        scene,
        transform,
        pill,
        f64::from(theme.tokens.radii.detail),
        raised,
    );
    text.draw_style(
        scene,
        badge,
        theme.text_dim,
        transform.pre_translate(vello::kurbo::Vec2::new(pill.x0 + pad, pill.y0 + pad / 2.0)),
        None,
        &style,
    );
}

impl ListRowFace<'_> {
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let disabled = self.interaction.disabled;
        let frame = self.frame.unwrap_or(rect);
        if disabled {
            ctx.scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                tilo_ui_base::widget::DISABLED_ALPHA,
                ctx.transform,
                &frame,
            );
        }
        self.paint_enabled(ctx, rect, frame);
        if disabled {
            ctx.scene.pop_layer();
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one pass over the row's columns in paint order keeps their geometry in one place"
    )]
    fn paint_enabled(&self, ctx: &mut PaintCtx<'_>, rect: Rect, frame: Rect) {
        let theme = ctx.theme;
        let m = self.metrics(theme);
        let colors = &theme.tokens.colors;
        let ground = colors.surfaces.content;

        let offset = self
            .swipe
            .map_or(0.0, |(_, offset)| offset.min(rect.width()));
        if let Some((action, _)) = self.swipe
            && offset > 0.0
        {
            self.paint_swipe(ctx, frame, &m, action, offset);
        }
        let shift = vello::kurbo::Vec2::new(offset, 0.0);
        let row = rect - shift;
        let frame_row = frame - shift;
        if offset > 0.0 {
            ctx.scene
                .fill(Fill::NonZero, ctx.transform, ground, None, &frame_row);
        }

        let (ground, under) = if self.card {
            crate::widget::card::paint_surface(
                ctx,
                frame_row,
                None,
                self.interaction,
                true,
                self.selected,
            );
            let raised = colors.surfaces.raised;
            let under = if self.interaction.disabled {
                raised
            } else if self.interaction.pressed {
                colors.lift_on(raised, 1.0)
            } else if self.interaction.hovered {
                colors.lift_on(raised, 0.5)
            } else {
                raised
            };
            (raised, under)
        } else {
            let lifted = self.selected || self.interaction.pressed;
            let hovered = self.interaction.hovered && !self.interaction.disabled;
            let pressed = self.interaction.pressed && !self.selected;
            paint_row_selection_joined(
                ctx.scene,
                ctx.transform,
                frame_row,
                SelectionState::new(self.selected, hovered, pressed),
                ground,
                theme,
                if self.selected {
                    self.join
                } else {
                    RowJoin::NONE
                },
            );
            let lift = tilo_ui_base::chrome::selection_tier_on(
                theme,
                SelectionState::new(lifted, hovered, false),
                ground,
            )
            .fill;
            let under = if lifted || hovered {
                tilo_ui_base::tokens::colors::composite(lift, ground)
            } else {
                ground
            };
            (ground, under)
        };

        let TextFrame {
            geo,
            extra,
            title_style,
            title_line,
            title_x,
            lines,
            text_top,
            title_cy,
            facts_cy,
        } = self.text_frame(row, &m, ctx.text, theme);

        self.paint_leading(ctx, &m, geo.leading, under);

        if let Some(r) = geo.disclosure {
            let centre = r.center();
            let turned = ctx.transform
                * Affine::translate(centre.to_vec2())
                * Affine::rotate(self.disclosure_turn * std::f64::consts::FRAC_PI_2)
                * Affine::translate(-centre.to_vec2());
            Icon::ChevronRight.draw(ctx.scene, turned, r, theme.text_tertiary);
        }
        if let (Some(r), Some(true)) = (geo.check, self.checked) {
            Icon::Check.draw(ctx.scene, ctx.transform, r, theme.text_primary);
        }
        if let (Some(status), Some(x)) = (self.status, geo.dot_x) {
            let cy = Self::dot_cy(&geo, row, extra, title_cy);
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                status.colour(theme),
                None,
                &Circle::new(Point::new(x, cy), m.dot / 2.0),
            );
        }
        if self.shows_actions() {
            let rects = self.action_rects_with(row, &m, ctx.text, theme);
            for (index, (action, r)) in self.actions.iter().zip(&rects).enumerate() {
                if self.hovered_action == Some(index) {
                    ctx.scene.fill(
                        Fill::NonZero,
                        ctx.transform,
                        colors.lift_on(under, 1.0),
                        None,
                        &RoundedRect::from_rect(*r, m.radius),
                    );
                    if self.interaction.focused {
                        crate::widget::activation::paint_focus_ring(
                            ctx.scene,
                            ctx.transform,
                            r.inset(-2.0),
                            (m.radius - 2.0).max(0.0),
                            theme,
                        );
                    }
                }
                let glyph = Rect::from_center_size(r.center(), (m.icon, m.icon));
                action
                    .icon
                    .draw(ctx.scene, ctx.transform, glyph, theme.text_primary);
            }
        } else {
            if let Some(slot) = geo.author {
                self.paint_author(ctx, &m, slot, facts_cy);
            }
            if let (Some(slot), Some(avatar)) = (geo.avatar, &self.avatar) {
                avatar.paint(ctx, slot);
            }
            if let (Some(right), Some(label)) = (geo.count_right, self.count_label()) {
                let w = BadgeFace::intrinsic_width(&label, ctx.text, theme);
                let h = BadgeFace::height(theme);
                BadgeFace::new(
                    Rect::new(right - w, facts_cy - h / 2.0, right, facts_cy + h / 2.0),
                    &label,
                )
                .paint(ctx.scene, ctx.transform, ctx.text, theme);
            }
            if let (Some((value, size)), right) = (self.value, geo.value_right) {
                // A body value says something of the row (a size, a count, a time), so it reads
                // as the row's other facts do, a caption in dim ink under the name it belongs to;
                // a title value is the row's main figure.
                let (style, ink) = match size {
                    RowValue::Body => (&m.value_body, theme.text_dim),
                    RowValue::Title => (&m.value_title, theme.text_primary),
                };
                // The value's room was reserved where the row laid out its
                // columns: one that does not fit elides there, never widening
                // past the row's trailing edge or under the name.
                let cap = (right - geo.value_left).max(0.0);
                if cap > 0.5 {
                    let shown = ctx.text.elide_to_width_style(
                        value,
                        tilo_ui_base::num::f32_of_f64(cap),
                        style,
                    );
                    let (w, h) = ctx.text.measure_style(&shown, None, style);
                    ctx.text(
                        &shown,
                        style,
                        ink,
                        Point::new(right - f64::from(w), facts_cy - f64::from(h) / 2.0),
                        None,
                    );
                }
            }
            self.paint_meta(ctx, &m, &geo, facts_cy, &[ground, under]);
            self.paint_marks(ctx, &m, &geo, facts_cy);
        }

        let text_x = geo.text_x;
        if let Some(prefix) = &self.prefix {
            ctx.text(
                prefix,
                &title_style,
                theme.text_dim,
                Point::new(text_x, text_top),
                None,
            );
        }
        let mut y = text_top;
        let mut last_end = title_x;
        for line in &lines {
            for span in found_spans(line, self.found) {
                let (Some(before), Some(word)) = (line.get(..span.start), line.get(span.clone()))
                else {
                    continue;
                };
                let start =
                    title_x + f64::from(ctx.text.measure_style(before, None, &title_style).0);
                let (width, height) = ctx.text.measure_style(word, None, &title_style);
                let mark = Rect::new(start, y, start + f64::from(width), y + f64::from(height));
                crate::widget::paint_find_match(ctx.scene, ctx.transform, mark, false, theme);
            }
            let ink = if self.struck || self.untitled() {
                theme.text_dim
            } else if self.faint {
                theme.text_tertiary
            } else if self.quiet {
                theme.text_dim
            } else {
                theme.text_primary
            };
            ctx.text(line, &title_style, ink, Point::new(title_x, y), None);
            last_end = title_x + f64::from(ctx.text.measure_style(line, None, &title_style).0);
            if self.struck {
                let through = y + title_line / 2.0;
                ctx.scene.stroke(
                    &Stroke::new(1.0),
                    ctx.transform,
                    ink,
                    None,
                    &vello::kurbo::Line::new((title_x, through), (last_end, through)),
                );
            }
            y += title_line;
        }
        if lines.is_empty() {
            y += title_line;
        }
        let line_cy = y - title_line / 2.0;
        let mut x = last_end + m.tag_gap;
        let tag_h = BadgeFace::height(theme);
        for (label, style) in self.tags {
            let w = BadgeFace::intrinsic_width(label, ctx.text, theme);
            if x + w > geo.title_right {
                break;
            }
            BadgeFace::new(
                Rect::new(x, line_cy - tag_h / 2.0, x + w, line_cy + tag_h / 2.0),
                label,
            )
            .style(*style)
            .paint(ctx.scene, ctx.transform, ctx.text, theme);
            x += w + m.tag_gap;
        }
        if let Some(subtitle) = self.subtitle {
            let width = tilo_ui_base::num::f32_of_f64((geo.second_right - text_x).max(0.0));
            let line = ctx.text.elide_to_width_style(subtitle, width, &m.subtitle);
            ctx.text(
                &line,
                &m.subtitle,
                theme.text_primary,
                Point::new(text_x, y),
                None,
            );
            y += RowMetrics::line(ctx.text, &m.subtitle);
        }
        if let Some(secondary) = self.secondary {
            let secondary_line = RowMetrics::line(ctx.text, &m.secondary);
            let right = if self.subtitle.is_some() {
                geo.text_right
            } else {
                geo.second_right
            };
            let width = tilo_ui_base::num::f32_of_f64((right - text_x).max(0.0));
            let kept = ctx.text.clamp_to_lines(
                secondary,
                &m.secondary,
                width,
                usize::from(self.secondary_lines),
            );
            for (index, line) in kept.iter().enumerate() {
                ctx.text(
                    line,
                    &m.secondary,
                    theme.text_dim,
                    Point::new(
                        text_x,
                        y + tilo_ui_base::num::f64_of_usize(index) * secondary_line,
                    ),
                    None,
                );
            }
        } else if !self.facts.is_empty() {
            let secondary_line = RowMetrics::line(ctx.text, &m.secondary);
            let right = if self.subtitle.is_some() {
                geo.text_right
            } else {
                geo.second_right
            };
            crate::widget::FactsFace::new(self.facts).paint(
                ctx,
                Rect::new(text_x, y, right.max(text_x), y + secondary_line),
            );
        }
        if let (Some(change), Some(face)) = (self.change, geo.change)
            && !change.said.is_empty()
        {
            let secondary_line = RowMetrics::line(ctx.text, &m.secondary);
            let y = if self.has_secondary_line() {
                y + f64::from(self.secondary_lines) * secondary_line
            } else {
                y
            };
            let words = face.x1 + m.column_gap;
            let width = tilo_ui_base::num::f32_of_f64((geo.second_right - words).max(0.0));
            let line = ctx
                .text
                .elide_to_width_style(change.said, width, &m.secondary);
            ctx.text(
                &line,
                &m.secondary,
                theme.text_tertiary,
                Point::new(words, y),
                None,
            );
            tilo_ui_base::widget::avatar_face::AvatarFace::of(change.by).paint(ctx, face);
        }
        if let Some(note) = self.note.filter(|_| !self.note_under) {
            let secondary_line = RowMetrics::line(ctx.text, &m.secondary);
            let y = if self.has_secondary_line() {
                y + f64::from(self.secondary_lines) * secondary_line
            } else {
                y
            };
            let y = if self.change.is_some_and(|change| !change.said.is_empty()) {
                y + secondary_line
            } else {
                y
            };
            let width = tilo_ui_base::num::f32_of_f64((geo.second_right - text_x).max(0.0));
            let line = ctx.text.elide_to_width_style(note, width, &m.secondary);
            ctx.text(
                &line,
                &m.secondary,
                theme.text_tertiary,
                Point::new(text_x, y),
                None,
            );
        }

        if self.separator && !self.selected && !self.next_selected {
            tilo_ui_base::surface::separator(
                ctx.scene,
                ctx.transform,
                text_x,
                frame_row.x1 - m.bleed,
                frame.y1 - 0.5,
                0.0,
                ground,
            );
        }

        if self.interaction.focused
            && !self.interaction.disabled
            && self.hovered_action.is_none()
            && !self.card
        {
            crate::widget::activation::paint_focus_ring(
                ctx.scene,
                ctx.transform,
                frame_row.inset(-2.0),
                (m.radius - 2.0).max(0.0),
                theme,
            );
        }
    }

    /// Where the title's lines stand in `row`: what [`Self::paint`] draws them by, and what a
    /// write in the title is marked on.
    fn title_layout(
        &self,
        row: Rect,
        m: &RowMetrics,
        geo: &RowGeometry,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> TitleLayout {
        let style = self.title_style(m);
        let line = RowMetrics::line(text, &style);
        let spare = self.title_width(geo, text, theme, m);
        let lines = text.clamp_to_lines(
            &self.shown_title(),
            &style,
            spare,
            usize::from(self.title_lines),
        );
        let drawn = lines.len().clamp(1, usize::from(self.title_lines));
        let unused = usize::from(self.title_lines).saturating_sub(drawn);
        let block = self.text_block(text, m) - line * tilo_ui_base::num::f64_of_usize(unused);
        let top = if self.card {
            self.card_content_top(row, m, text, theme)
        } else {
            row.center().y - block / 2.0
        };
        let x = geo.text_x + self.prefix_width(text, m);
        TitleLayout {
            style,
            line,
            lines,
            top,
            x,
        }
    }

    /// Where the row's words stand in `row`: its columns, its title's lines and the line its
    /// facts are centred on, as the canvas paints them and a page composes them.
    pub(super) fn text_frame(
        &self,
        row: Rect,
        m: &RowMetrics,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> TextFrame {
        let geo = self.geometry(row, m, text, theme);
        let extra = self.extra_lines() > 0;
        let title = self.title_layout(row, m, &geo, text, theme);
        let title_cy = title.top + title.line / 2.0;
        let facts_cy = if self.card {
            geo.facts_cy
        } else if extra {
            title_cy
        } else {
            row.center().y
        };
        TextFrame {
            geo,
            extra,
            title_style: title.style,
            title_line: title.line,
            title_x: title.x,
            lines: title.lines,
            text_top: title.top,
            title_cy,
            facts_cy,
        }
    }

    /// Where the bytes `start..end` of the title stand in `rect` ([`LabeledCaret::after_span`]).
    pub(super) fn title_span(
        &self,
        rect: Rect,
        span: (usize, usize),
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> (Vec<Rect>, tilo_ui_base::presence::LabeledCaret) {
        let m = self.metrics(theme);
        let geo = self.geometry(rect, &m, text, theme);
        let layout = self.title_layout(rect, &m, &geo, text, theme);
        tilo_ui_base::presence::LabeledCaret::after_span(
            text,
            (self.title, &layout.lines, &layout.style),
            (layout.x, layout.top, layout.line),
            (rect.y0, rect.x1),
            span,
        )
    }

    /// The width the title is set in: its column, less the quiet word before it and the tags
    /// after it. The title keeps at least half its line; the tags take the rest and drop off
    /// before the title elides further.
    pub(super) fn title_width(
        &self,
        geo: &RowGeometry,
        text: &mut TextRenderer,
        theme: &Theme,
        m: &RowMetrics,
    ) -> f32 {
        let title_width = (geo.title_right - geo.text_x - self.prefix_width(text, m)).max(0.0);
        let tags_w = self.tags_width(text, theme, m);
        let accessories = tags_w.min(title_width / 2.0);
        tilo_ui_base::num::f32_of_f64((title_width - accessories).max(0.0))
    }

    /// The width the row's quiet word before the title takes, with the gap after it: none for
    /// a row that names none.
    pub(super) fn prefix_width(&self, text: &mut TextRenderer, m: &RowMetrics) -> f64 {
        match &self.prefix {
            Some(prefix) => {
                f64::from(text.measure_style(prefix, None, &self.title_style(m)).0) + m.tag_gap
            }
            None => 0.0,
        }
    }

    fn paint_marks(&self, ctx: &mut PaintCtx<'_>, m: &RowMetrics, geo: &RowGeometry, cy: f64) {
        let Some(right) = geo.marks_right else {
            return;
        };
        let theme = ctx.theme;
        let ink = if self.mark_tone == Tone::Neutral {
            theme.text_tertiary
        } else {
            self.mark_tone.colour(theme)
        };
        let mut x = right - self.marks_width(m);
        for mark in self.marks.iter().flatten().copied() {
            let r = Rect::new(x, cy - m.mark / 2.0, x + m.mark, cy + m.mark / 2.0);
            match mark {
                RowMark::Icon(icon @ (Icon::Star | Icon::Pin)) => {
                    icon.draw_filled(ctx.scene, ctx.transform, r, ink);
                }
                RowMark::Icon(icon) => icon.draw(ctx.scene, ctx.transform, r, ink),
                RowMark::Swatch(color) => {
                    let swatch = r.inset(-(m.mark * 0.1));
                    let radius = swatch.width() * 0.25;
                    let shape = RoundedRect::from_rect(swatch, radius);
                    ctx.scene
                        .fill(Fill::NonZero, ctx.transform, color, None, &shape);
                    ctx.scene.stroke(
                        &vello::kurbo::Stroke::new(1.0),
                        ctx.transform,
                        tilo_ui_base::surface::edge_on(color),
                        None,
                        &tilo_ui_base::surface::edge_outline(swatch, radius),
                    );
                }
            }
            x += m.mark + m.tag_gap;
        }
    }

    /// Whether the title, painted in `rect`, is cut short with an ellipsis.
    #[must_use]
    pub fn title_elided(&self, rect: Rect, text: &mut TextRenderer, theme: &Theme) -> bool {
        if self.title.is_empty() {
            return false;
        }
        let m = self.metrics(theme);
        let geo = self.geometry(rect, &m, text, theme);
        let spare = self.title_width(&geo, text, theme, &m);
        let lines = text.clamp_to_lines(
            self.title,
            &self.title_style(&m),
            spare,
            usize::from(self.title_lines),
        );
        lines
            .last()
            .is_none_or(|line| line.ends_with('\u{2026}') && !self.title.ends_with('\u{2026}'))
    }

    pub(super) fn tags_width(&self, text: &mut TextRenderer, theme: &Theme, m: &RowMetrics) -> f64 {
        self.tags
            .iter()
            .map(|(label, _)| BadgeFace::intrinsic_width(label, text, theme) + m.tag_gap)
            .sum()
    }

    /// Who made what the row shows, as one cell: the person's face, their name in the
    /// person's own ink, and what wrote for them as a chip wearing its mark and hue. The
    /// name is cut short before the chip, never the chip: what wrote is part of who, and a
    /// reader who cannot see the mark hears it in the row's description.
    fn paint_author(&self, ctx: &mut PaintCtx<'_>, m: &RowMetrics, slot: Rect, line_cy: f64) {
        let theme = ctx.theme;
        let Some(author) = self.author else {
            return;
        };
        let side = slot.height().min(RowMetrics::avatar(theme, false));
        let face = Rect::from_center_size(Point::new(slot.x0 + side / 2.0, line_cy), (side, side));
        tilo_ui_base::widget::AvatarFace::new(author.person, author.identity)
            .image(author.image)
            .paint(ctx, face);
        let x = face.x1 + m.column_gap;
        // The cell keeps to its slot: the name gives way first, then the writer's chip is its
        // mark alone, then the chip goes, rather than drawing over the face or the title.
        let room = (slot.x1 - x).max(0.0);
        let chip = author.writer.and_then(|writer| {
            let lead = crate::widget::ChipLead::Agent(theme.tokens.colors.presence.hue(writer.hue));
            let named = crate::widget::ChipFace::intrinsic_width(
                writer.label,
                lead,
                false,
                ctx.text,
                theme,
            );
            if named <= room {
                return Some((writer.label, lead, named));
            }
            let mark = crate::widget::ChipFace::intrinsic_width("", lead, false, ctx.text, theme);
            (mark <= room).then_some(("", lead, mark))
        });
        let chip_w = chip.as_ref().map_or(0.0, |(.., width)| *width);
        let name_right = slot.x1 - chip_w - if chip.is_some() { m.tag_gap } else { 0.0 };
        let cap = (name_right - x).max(0.0);
        let name = ctx.text.elide_to_width_style(
            author.person,
            tilo_ui_base::num::f32_of_f64(cap),
            &m.author,
        );
        let (_, height) = ctx.text.measure_style(&name, None, &m.author);
        ctx.text(
            &name,
            &m.author,
            theme.text_dim,
            Point::new(x, line_cy - f64::from(height) / 2.0),
            None,
        );
        if let (Some((label, lead, width)), Some(writer)) = (chip, author.writer) {
            let height = crate::widget::ChipFace::height(theme);
            let pill = Rect::new(
                slot.x1 - width,
                line_cy - height / 2.0,
                slot.x1,
                line_cy + height / 2.0,
            );
            crate::widget::ChipFace::new(pill, label)
                .lead(lead)
                .style(crate::widget::ChipStyle::Identity(
                    theme.tokens.colors.presence.hue(writer.hue),
                ))
                .bare()
                .paint(ctx.scene, ctx.transform, ctx.text, theme);
        }
    }

    fn paint_meta(
        &self,
        ctx: &mut PaintCtx<'_>,
        m: &RowMetrics,
        geo: &RowGeometry,
        line_cy: f64,
        grounds: &[Color],
    ) {
        let theme = ctx.theme;
        let cap = (geo.meta_right - geo.meta_left).max(0.0);
        if !self.meta_spans.is_empty() {
            let mut x = geo.meta_left;
            for (span, tone) in self.meta_spans {
                let ink = if *tone == Tone::Neutral {
                    theme.text_dim
                } else {
                    theme.tokens.colors.ink_on_any(
                        grounds,
                        tone.colour(theme),
                        SMALL_TEXT_MIN_CONTRAST,
                    )
                };
                let (w, h) = ctx.text.measure_style(span, None, &m.meta);
                if x + f64::from(w) > geo.meta_right + 0.5 {
                    break;
                }
                ctx.text(
                    span,
                    &m.meta,
                    ink,
                    Point::new(x, line_cy - f64::from(h) / 2.0),
                    None,
                );
                x += f64::from(w) + m.tag_gap;
            }
            return;
        }
        let Some(meta) = self.meta else {
            return;
        };
        let meta = ctx
            .text
            .elide_to_width_style(meta, tilo_ui_base::num::f32_of_f64(cap), &m.meta);
        let (width, height) = ctx.text.measure_style(&meta, None, &m.meta);
        ctx.text(
            &meta,
            &m.meta,
            theme.text_dim,
            Point::new(
                geo.meta_right - f64::from(width),
                line_cy - f64::from(height) / 2.0,
            ),
            None,
        );
    }

    fn paint_leading(&self, ctx: &mut PaintCtx<'_>, m: &RowMetrics, slot: Rect, under: Color) {
        let theme = ctx.theme;
        let Some(leading) = self.leading else {
            return;
        };
        match leading {
            RowLeading::Icon(icon) => {
                let r = Rect::from_center_size(slot.center(), (m.icon, m.icon));
                icon.draw(ctx.scene, ctx.transform, r, theme.text_dim);
            }
            RowLeading::Avatar(face) => {
                let side = slot.height();
                face.paint(ctx, Rect::from_center_size(slot.center(), (side, side)));
            }
            RowLeading::Face(face) => {
                let side = slot.height();
                face.paint(
                    ctx,
                    Rect::from_center_size(slot.center(), (side, side)),
                    under,
                );
            }
            RowLeading::Badge(badge) => paint_badge(ctx, m, slot, badge),
            RowLeading::Thumbnail(image) => {
                let side = slot.height();
                let r = Rect::from_center_size(slot.center(), (side, side));
                crate::widget::media_card::paint_art_tile(
                    ctx,
                    r,
                    MediaArt::Image(image),
                    self.title,
                    f64::from(theme.tokens.radii.detail),
                );
            }
            RowLeading::Media(art, aspect) => {
                let h = slot.height();
                let w = (h * aspect.ratio()).min(slot.width());
                let r = Rect::from_center_size(slot.center(), (w, h));
                crate::widget::media_card::paint_art_tile(
                    ctx,
                    r,
                    art,
                    self.title,
                    f64::from(theme.tokens.radii.detail),
                );
            }
            RowLeading::Checkbox { checked, icon } => {
                let boxed = super::geometry::checkbox_box(slot, icon, m);
                if let Some(icon) = icon {
                    let at = slot.x0 + m.icon + m.column_gap + m.icon / 2.0;
                    icon.draw(
                        ctx.scene,
                        ctx.transform,
                        Rect::new(
                            at - m.icon / 2.0,
                            slot.center().y - m.icon / 2.0,
                            at + m.icon / 2.0,
                            slot.center().y + m.icon / 2.0,
                        ),
                        theme.text_primary,
                    );
                }
                let mut face = crate::widget::checkbox::CheckboxFace::new(boxed, checked);
                if self.mark_tone != Tone::Neutral {
                    face = face.ring(theme.tokens.colors.ink_on_any(
                        &[under],
                        self.mark_tone.colour(theme),
                        NON_TEXT_MIN_CONTRAST,
                    ));
                }
                if let Some(pose) = self.check_pose {
                    face = face.pose(pose);
                }
                crate::widget::Checkbox::paint(ctx.scene, ctx.transform, theme, &face);
            }
            RowLeading::Swatch(colour) => {
                let r = Rect::from_center_size(slot.center(), (m.icon, m.icon));
                let circle = Circle::new(r.center(), r.width() / 2.0);
                ctx.scene
                    .fill(Fill::NonZero, ctx.transform, colour, None, &circle);
                ctx.scene.stroke(
                    &Stroke::new(1.0),
                    ctx.transform,
                    tilo_ui_base::surface::edge_on(under),
                    None,
                    &Circle::new(r.center(), r.width() / 2.0 - 0.5),
                );
            }
            RowLeading::Signal(level) => {
                let r = Rect::from_center_size(slot.center(), (m.icon, m.icon));
                crate::widget::SignalMeterFace::new(level).paint(
                    ctx.scene,
                    ctx.transform,
                    theme,
                    r,
                );
            }
            RowLeading::Paint(painter) => painter(ctx, slot),
        }
    }

    fn paint_swipe(
        &self,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        m: &RowMetrics,
        action: SwipeAction<'_>,
        offset: f64,
    ) {
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        let revealed = Rect::new(rect.x1 - offset, rect.y0, rect.x1, rect.y1);
        let fill = if action.destructive {
            colors.semantic.danger
        } else {
            colors.lift_on(colors.surfaces.content, 1.0)
        };
        ctx.scene
            .fill(Fill::NonZero, ctx.transform, fill, None, &revealed);
        let ink = if action.destructive {
            tilo_ui_base::widget::avatar_face::initial_ink(fill, theme)
        } else {
            theme.text_primary
        };
        if offset >= m.icon + 2.0 * m.bleed {
            let glyph = Rect::new(
                revealed.x1 - m.bleed - m.icon,
                revealed.center().y - m.icon / 2.0,
                revealed.x1 - m.bleed,
                revealed.center().y + m.icon / 2.0,
            );
            action.icon.draw(ctx.scene, ctx.transform, glyph, ink);
        }
    }
}

/// Where a row's title stands ([`ListRowFace::title_layout`]).
struct TitleLayout {
    style: tilo_ui_base::text_render::TextStyle,
    /// One line's height.
    line: f64,
    lines: Vec<String>,
    /// The first line's top.
    top: f64,
    /// Where the title's words start, past the quiet word before them.
    x: f64,
}
