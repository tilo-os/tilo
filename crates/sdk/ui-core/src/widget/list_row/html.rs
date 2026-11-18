//! A list row on a page: its words as text where the canvas sets them, its leading icon as an
//! icon, its box as a checkbox, and what else it draws (a face, a dot, a chevron, its marks) as
//! pictures of those parts, so a page shows everything the canvas does.

use vello::kurbo::{Point, Rect};
use vello::peniko::Color;

use super::paint::TextFrame;
use super::{ListRowFace, RowLeading, RowMetrics, RowValue};
use crate::a11y::AccessNode;
use crate::composition::{CheckStyle, Control, HtmlCx, TextLook};
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::widget::BadgeFace;

impl ListRowFace<'_> {
    /// The row's parts in `cx`, laid out in `row` as the canvas paints them; `check` is the
    /// leading box's own node, which a page's checkbox toggles.
    pub(super) fn compose(&self, cx: &mut HtmlCx<'_>, row: Rect, check: Option<AccessNode>) {
        if row.width() <= 0.0 || row.height() <= 0.0 {
            return;
        }
        let theme = cx.theme();
        let m = self.metrics(theme);
        crate::text_render::with_layout_renderer(theme, |text| {
            let frame = self.text_frame(row, &m, text, theme);
            self.compose_leading(cx, &m, &frame, check);
            self.compose_title(cx, &m, &frame, text, theme);
            self.compose_lines(cx, &m, &frame, text, theme);
            self.compose_trailing(cx, &m, &frame, row, text, theme);
        });
    }

    fn compose_leading(
        &self,
        cx: &mut HtmlCx<'_>,
        m: &RowMetrics,
        frame: &TextFrame,
        check: Option<AccessNode>,
    ) {
        let slot = frame.geo.leading;
        if slot.is_zero_area() {
            return;
        }
        let theme = cx.theme();
        match self.leading {
            None | Some(RowLeading::AvatarSpace) => {}
            Some(RowLeading::Icon(icon)) => {
                let r = Rect::from_center_size(slot.center(), (m.icon, m.icon));
                cx.icon("leading", r, icon, theme.text_dim);
            }
            Some(RowLeading::Checkbox { checked, icon }) => {
                let boxed = super::geometry::checkbox_box(slot, icon, m);
                let part = cx.part("leading", "ListRowCheck", boxed);
                part.tag = Some("label");
                part.focusable = true;
                part.semantics = check;
                part.own = Some(crate::composition::Own::Control {
                    controls: vec![Control::Check {
                        style: CheckStyle::Checkbox,
                        checked,
                        mixed: false,
                        label: None,
                    }],
                });
                if let Some(icon) = icon {
                    let at = slot.x0 + m.icon + m.column_gap;
                    let r = Rect::new(
                        at,
                        slot.center().y - m.icon / 2.0,
                        at + m.icon,
                        slot.center().y + m.icon / 2.0,
                    );
                    cx.icon("leading-icon", r, icon, theme.text_primary);
                }
            }
            Some(_) => painted(cx, "leading", slot),
        }
    }

    fn compose_title(
        &self,
        cx: &mut HtmlCx<'_>,
        m: &RowMetrics,
        frame: &TextFrame,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let geo = &frame.geo;
        let top = frame.text_top;
        if let Some(prefix) = self.prefix {
            let (w, _) = text.measure_style(prefix, None, &frame.title_style);
            let r = Rect::new(
                geo.text_x,
                top,
                geo.text_x + f64::from(w),
                top + frame.title_line,
            );
            words(
                cx,
                "prefix",
                r,
                prefix.to_owned(),
                &frame.title_style,
                theme.text_dim,
            );
        }
        let title_x = frame.title_x;
        let room = f64::from(self.title_width(geo, text, theme, m));
        let ink = if self.struck || self.untitled() {
            theme.text_dim
        } else if self.faint {
            theme.text_tertiary
        } else if self.quiet {
            theme.text_dim
        } else {
            theme.text_primary
        };
        let lines = frame.lines.len().max(1);
        let r = Rect::new(
            title_x,
            top,
            title_x + room,
            top + frame.title_line * crate::num::f64_of_usize(lines),
        );
        words(
            cx,
            "title",
            r,
            frame.lines.join("\n"),
            &frame.title_style,
            ink,
        );
        // The tags after the title's last line, as far as they fit.
        let last = frame.lines.last().map_or(0.0, |line| {
            f64::from(text.measure_style(line, None, &frame.title_style).0)
        });
        let line_cy = r.y1 - frame.title_line / 2.0;
        let tag_h = BadgeFace::height(theme);
        let mut x = title_x + last + m.tag_gap;
        for (index, (label, _)) in self.tags.iter().enumerate() {
            let w = BadgeFace::intrinsic_width(label, text, theme);
            if x + w > geo.title_right {
                break;
            }
            let tag = Rect::new(x, line_cy - tag_h / 2.0, x + w, line_cy + tag_h / 2.0);
            painted(cx, &format!("tag:{index}"), tag);
            x += w + m.tag_gap;
        }
    }

    /// The lines under the title: the subtitle, the secondary line or its facts, and the note.
    fn compose_lines(
        &self,
        cx: &mut HtmlCx<'_>,
        m: &RowMetrics,
        frame: &TextFrame,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let geo = &frame.geo;
        let x = geo.text_x;
        let mut y =
            frame.text_top + frame.title_line * crate::num::f64_of_usize(frame.lines.len().max(1));
        if let Some(subtitle) = self.subtitle {
            let width = (geo.second_right - x).max(0.0);
            let line =
                text.elide_to_width_style(subtitle, crate::num::f32_of_f64(width), &m.subtitle);
            let h = RowMetrics::line(text, &m.subtitle);
            words(
                cx,
                "subtitle",
                Rect::new(x, y, x + width, y + h),
                line,
                &m.subtitle,
                theme.text_primary,
            );
            y += h;
        }
        let secondary_line = RowMetrics::line(text, &m.secondary);
        let right = if self.subtitle.is_some() {
            geo.text_right
        } else {
            geo.second_right
        };
        if let Some(secondary) = self.secondary {
            let width = (right - x).max(0.0);
            let kept = text.clamp_to_lines(
                secondary,
                &m.secondary,
                crate::num::f32_of_f64(width),
                usize::from(self.secondary_lines),
            );
            let h = secondary_line * crate::num::f64_of_usize(kept.len().max(1));
            words(
                cx,
                "secondary",
                Rect::new(x, y, x + width, y + h),
                kept.join("\n"),
                &m.secondary,
                theme.text_dim,
            );
        } else if !self.facts.is_empty() {
            painted(
                cx,
                "facts",
                Rect::new(x, y, right.max(x), y + secondary_line),
            );
        }
        let mut below = if self.has_secondary_line() {
            y + f64::from(self.secondary_lines) * secondary_line
        } else {
            y
        };
        if let (Some(change), Some(face)) = (self.change, geo.change)
            && !change.said.is_empty()
        {
            painted(
                cx,
                "change",
                Rect::new(
                    face.x0,
                    face.y0,
                    geo.second_right,
                    face.y1.max(below + secondary_line),
                ),
            );
            below += secondary_line;
        }
        if let Some(note) = self.note {
            let width = (geo.second_right - x).max(0.0);
            let line = text.elide_to_width_style(note, crate::num::f32_of_f64(width), &m.secondary);
            words(
                cx,
                "note",
                Rect::new(x, below, x + width, below + secondary_line),
                line,
                &m.secondary,
                theme.text_tertiary,
            );
        }
    }

    /// What stands after the words: the value, the meta, the marks, the count, the faces, the
    /// status dot, the check and the chevron.
    fn compose_trailing(
        &self,
        cx: &mut HtmlCx<'_>,
        m: &RowMetrics,
        frame: &TextFrame,
        row: Rect,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let geo = &frame.geo;
        let cy = frame.facts_cy;
        if let Some(r) = geo.disclosure {
            painted(cx, "disclosure", r);
        }
        if let (Some(r), Some(true)) = (geo.check, self.checked) {
            painted(cx, "check", r);
        }
        if let (Some(_), Some(x)) = (self.status, geo.dot_x) {
            let dot_cy = Self::dot_cy(geo, row, frame.extra, frame.title_cy);
            painted(
                cx,
                "status",
                Rect::from_center_size(Point::new(x, dot_cy), (m.dot, m.dot)),
            );
        }
        if self.shows_actions() {
            return;
        }
        if let Some(slot) = geo.author {
            painted(cx, "author", slot);
        }
        if let (Some(slot), Some(_)) = (geo.avatar, &self.avatar) {
            painted(cx, "avatar", slot);
        }
        if let (Some(right), Some(label)) = (geo.count_right, self.count_label()) {
            let w = BadgeFace::intrinsic_width(&label, text, theme);
            let h = BadgeFace::height(theme);
            painted(
                cx,
                "count",
                Rect::new(right - w, cy - h / 2.0, right, cy + h / 2.0),
            );
        }
        if let Some((value, size)) = self.value {
            let (style, ink) = match size {
                RowValue::Body => (&m.value_body, theme.text_dim),
                RowValue::Title => (&m.value_title, theme.text_primary),
            };
            let right = geo.value_right;
            let cap = (right - geo.value_left).max(0.0);
            if cap > 0.5 {
                let shown = text.elide_to_width_style(value, crate::num::f32_of_f64(cap), style);
                let (w, h) = text.measure_style(&shown, None, style);
                let r = Rect::new(
                    right - f64::from(w),
                    cy - f64::from(h) / 2.0,
                    right,
                    cy + f64::from(h) / 2.0,
                );
                words(cx, "value", r, shown, style, ink);
            }
        }
        if !self.meta_spans.is_empty() {
            let h = RowMetrics::line(text, &m.meta);
            painted(
                cx,
                "meta",
                Rect::new(geo.meta_left, cy - h / 2.0, geo.meta_right, cy + h / 2.0),
            );
        } else if let Some(meta) = self.meta {
            let cap = (geo.meta_right - geo.meta_left).max(0.0);
            let shown = text.elide_to_width_style(meta, crate::num::f32_of_f64(cap), &m.meta);
            let (w, h) = text.measure_style(&shown, None, &m.meta);
            let r = Rect::new(
                geo.meta_right - f64::from(w),
                cy - f64::from(h) / 2.0,
                geo.meta_right,
                cy + f64::from(h) / 2.0,
            );
            words(cx, "meta", r, shown, &m.meta, theme.text_dim);
        }
        if let Some(right) = geo.marks_right {
            let left = right - self.marks_width(m);
            let reach = m.mark * 0.1 + 1.0;
            painted(
                cx,
                "marks",
                Rect::new(
                    left - reach,
                    cy - m.mark / 2.0 - reach,
                    right + reach,
                    cy + m.mark / 2.0 + reach,
                ),
            );
        }
    }
}

/// A part showing `text` set in `style` and `ink`, at `rect`.
fn words(cx: &mut HtmlCx<'_>, key: &str, rect: Rect, text: String, style: &TextStyle, ink: Color) {
    if text.is_empty() || rect.width() <= 0.0 {
        return;
    }
    cx.words(key, "span", rect, TextLook::new(text, style, Some(ink)));
}

/// A part showing the row's paint within `rect`.
fn painted(cx: &mut HtmlCx<'_>, key: &str, rect: Rect) {
    if rect.width() > 0.0 && rect.height() > 0.0 {
        cx.part(key, "ListRowPart", rect).painted(0);
    }
}
