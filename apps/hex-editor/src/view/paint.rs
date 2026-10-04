//! Painting the hex view.
//!
//! The byte grid is genuine content drawing — a dense, fixed-pitch canvas
//! with a cursor synced across two parallel columns — so it paints itself,
//! reading every colour, size and gap from the theme. Everything around it is
//! the framework's: the header's edit-mode strip, the inspector drawer and
//! its toggle, the find band, the status bar, the property list, the empty
//! state and the toast.

use std::fmt::Write as _;

use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{TextKind, TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind};
use ui::widget::{PropertyListFace, PropertyRow, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{BlendMode, Color, Fill};

use super::HexView;
use super::layout::Geo;
use crate::inspector::{self, Interpretation};
use crate::model::Column;

/// Hex digits, so a cell's two characters need no allocation.
const DIGITS: &[u8; 16] = b"0123456789ABCDEF";

/// The hex digit of a nibble.
fn digit(nibble: u8) -> u8 {
    DIGITS
        .get(usize::from(nibble & 0x0F))
        .copied()
        .unwrap_or(b'0')
}

/// The two hex digits of `byte`.
fn hex_pair(byte: u8) -> [u8; 2] {
    [digit(byte >> 4), digit(byte)]
}

/// The glyph of the ASCII column: the printable character, or a centred dot
/// for a control or high byte (the hex-editor convention).
fn ascii_glyph(byte: u8, buf: &mut [u8; 4]) -> &str {
    let ch = if (0x20..=0x7E).contains(&byte) {
        char::from(byte)
    } else {
        '·'
    };
    ch.encode_utf8(buf)
}

/// `n` bytes, singular or plural.
fn byte_count(n: usize) -> String {
    if n == 1 {
        "1 byte".to_string()
    } else {
        format!("{n} bytes")
    }
}

/// The part of `lo..=hi` inside the row starting at `row_off`, as columns.
fn span_in_row(
    lo: usize,
    hi: usize,
    row_off: usize,
    bytes_per_row: usize,
) -> Option<(usize, usize)> {
    let first = lo.max(row_off);
    let last = hi.min(row_off.saturating_add(bytes_per_row).saturating_sub(1));
    (first <= last).then(|| (first - row_off, last - row_off))
}

/// What one painted row needs beyond the geometry.
struct RowCtx<'a> {
    theme: &'a Theme,
    g: &'a Geo,
    code: &'a TextStyle,
    code_strong: &'a TextStyle,
    selection: Option<(usize, usize)>,
    inspected: Option<(usize, usize)>,
    cursor: usize,
    bytes_per_row: usize,
}

impl HexView {
    /// The whole frame.
    pub(super) fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.has_file {
            self.paint_empty_state(scene, transform, tr, theme, presence);
            self.paint_notices(scene, transform, tr, theme);
            return;
        }
        self.paint_header(scene, transform, tr, theme, presence);
        if self.find_open {
            self.paint_find(scene, transform, tr, theme, presence);
        }
        self.paint_grid(scene, transform, tr, theme);
        let metrics = self.scroll_metrics();
        if metrics.overflows() {
            self.scrollbar.draw(
                scene,
                transform,
                self.geo.scroll_track,
                metrics,
                theme,
                false,
            );
        }
        self.paint_status(scene, transform, tr, theme);
        self.paint_inspector_region(scene, transform, tr, theme, presence);
        self.paint_notices(scene, transform, tr, theme);
    }

    /// The file name and the edit-mode strip, standing on the content with
    /// one hairline under them.
    fn paint_header(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let g = self.geo.clone();
        let colors = &theme.tokens.colors;
        let inset = f64::from(theme.tokens.spacing.s4);
        let style = theme.text(TextRole::Title).with_kind(TextKind::Identifier);
        let room = (g.mode.x0 - g.header.x0 - inset - f64::from(theme.tokens.spacing.s3)).max(0.0);
        let name = tr.elide_to_width_style(&self.file_name, narrow(room), &style);
        let (_, line) = tr.measure_style(&name, None, &style);
        tr.draw_style(
            scene,
            &name,
            colors.text_primary,
            transform
                * Affine::translate(Vec2::new(
                    g.header.x0 + inset,
                    g.header.y0 + (g.header.height() - f64::from(line)) / 2.0,
                )),
            None,
            &style,
        );
        ui::surface::separator(
            scene,
            transform,
            g.header.x0,
            g.header.x1,
            g.header.y1,
            0.0,
            colors.surfaces.content,
        );
        self.mode_group
            .set_selected(super::layout::mode_index(self.model.mode()));
        Widget::draw(&mut self.mode_group, scene, transform, tr, theme, presence);
    }

    /// The find band under the header. The bar insets its field by the
    /// shell's `s3` gutter; the band starts `s4` in, so the field's edge is the
    /// file name's and the offset column's, and the hairline under it still
    /// runs the full width like the header's.
    fn paint_find(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let band = self.geo.find;
        let sp = &theme.tokens.spacing;
        let lead = f64::from(sp.s4 - sp.s3);
        self.find_bar.layout(
            Rect::new(band.x0 + lead, band.y0, band.x1, band.y1),
            theme,
            tr,
        );
        self.find_bar.paint(scene, transform, tr, theme, presence);
        ui::surface::separator(
            scene,
            transform,
            band.x0,
            band.x1,
            band.y1,
            0.0,
            theme.tokens.colors.surfaces.content,
        );
    }

    /// The column labels, the rules between the columns and the rows of
    /// bytes.
    fn paint_grid(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let g = self.geo.clone();
        let colors = &theme.tokens.colors;
        let bpr = self.model.bytes_per_row();
        let code = theme.text(TextRole::Code);
        let code_strong = code.clone().strong();
        let head_y = g.column_head.y0 + (g.row_h - g.line_h) / 2.0;

        // Column labels share the cells' style and pitch, so each label's
        // centre is its column's centre.
        let label = |scene: &mut Scene, tr: &mut TextRenderer, text: &str, x: f64| {
            tr.draw_style(
                scene,
                text,
                colors.text_dim,
                transform * Affine::translate(Vec2::new(x, head_y)),
                None,
                &code,
            );
        };
        label(scene, tr, "Offset", g.gutter_x0);
        for c in 0..bpr {
            let digits = hex_pair(u8::try_from(c).unwrap_or(u8::MAX));
            label(
                scene,
                tr,
                std::str::from_utf8(&digits).unwrap_or(""),
                g.hex_x(c),
            );
        }
        label(scene, tr, "ASCII", g.ascii_x0);

        // The rules between the offset, hex and ASCII columns run from the
        // labels to the status bar.
        let rule = f64::from(theme.tokens.metrics.chrome_rule_px);
        let edge = ui::surface::edge_on(colors.surfaces.content);
        for x in [g.rule_hex, g.rule_ascii] {
            scene.fill(
                Fill::NonZero,
                transform,
                edge,
                None,
                &Rect::new(x, g.column_head.y0, x + rule, g.grid.y1),
            );
        }

        let ctx = RowCtx {
            theme,
            g: &g,
            code: &code,
            code_strong: &code_strong,
            selection: self.model.selection(),
            inspected: self.inspector_span(),
            cursor: self.model.cursor(),
            bytes_per_row: bpr,
        };
        for vr in 0..g.visible_rows {
            let row = self.model.top_row() + vr;
            let row_off = row * bpr;
            if row_off >= self.model.len() && row_off > 0 {
                break;
            }
            let y = g.grid.y0 + vr as f64 * g.row_h;
            self.paint_row(scene, transform, tr, &ctx, (row_off, y));
        }
    }

    /// One row: its offset, the tint layers, the cursor, then the glyphs.
    /// `at` is the row's first byte offset and its top edge.
    fn paint_row(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        ctx: &RowCtx<'_>,
        at: (usize, f64),
    ) {
        let (row_off, y) = at;
        let g = ctx.g;
        let colors = &ctx.theme.tokens.colors;
        let text_y = y + (g.row_h - g.line_h) / 2.0;
        let offset = format!("{row_off:08X}");
        tr.draw_style(
            scene,
            &offset,
            colors.text_dim,
            transform * Affine::translate(Vec2::new(g.gutter_x0, text_y)),
            None,
            ctx.code,
        );
        self.paint_row_tints(scene, transform, ctx, at);
        for c in 0..ctx.bytes_per_row {
            let off = row_off + c;
            let Some(&byte) = self.model.bytes().get(off) else {
                break;
            };
            let dirty = self.model.is_dirty(off);
            let style = if dirty { ctx.code_strong } else { ctx.code };
            let hex_ink = if byte == 0 && !dirty {
                colors.text_tertiary
            } else {
                colors.text_primary
            };
            let digits = hex_pair(byte);
            tr.draw_style(
                scene,
                std::str::from_utf8(&digits).unwrap_or(""),
                hex_ink,
                transform * Affine::translate(Vec2::new(g.hex_x(c), text_y)),
                None,
                style,
            );
            let ascii_ink = if dirty || (0x20..=0x7E).contains(&byte) {
                colors.text_primary
            } else {
                colors.text_tertiary
            };
            let mut buf = [0_u8; 4];
            tr.draw_style(
                scene,
                ascii_glyph(byte, &mut buf),
                ascii_ink,
                transform * Affine::translate(Vec2::new(g.ascii_x(c), text_y)),
                None,
                style,
            );
        }
    }

    /// The bands under one row's glyphs, lowest to highest: the bytes the
    /// inspector reads, find matches, the selection, then the cursor.
    fn paint_row_tints(
        &self,
        scene: &mut Scene,
        transform: Affine,
        ctx: &RowCtx<'_>,
        at: (usize, f64),
    ) {
        let (row_off, y) = at;
        let g = ctx.g;
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        let bpr = ctx.bytes_per_row;
        let radius = f64::from(theme.tokens.radii.detail);
        let half = g.byte_gap / 2.0;
        let hex_band = |first: usize, last: usize| {
            Rect::new(
                g.hex_x(first) - half,
                y,
                g.hex_x(last) + g.byte_w + half,
                y + g.row_h,
            )
        };
        let ascii_band = |first: usize, last: usize| {
            Rect::new(
                g.ascii_cell_x(first),
                y,
                g.ascii_cell_x(last) + g.ascii_pitch,
                y + g.row_h,
            )
        };
        let fill = |scene: &mut Scene, rect: Rect, color: Color| {
            scene.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &RoundedRect::from_rect(rect, radius),
            );
        };

        // An edited byte stands on a neutral lift in both columns, in heavier
        // ink: it is changed, not a warning and nobody's presence.
        let edited = colors.lift_on(colors.surfaces.content, 1.0);
        for c in 0..bpr {
            if self.model.is_dirty(row_off + c) {
                fill(scene, hex_band(c, c), edited);
                fill(scene, ascii_band(c, c), edited);
            }
        }

        if let Some((lo, hi)) = ctx.inspected
            && let Some((a, b)) = span_in_row(lo, hi, row_off, bpr)
        {
            let wash = colors.lift_on(colors.surfaces.content, 0.5);
            fill(scene, hex_band(a, b), wash);
            fill(scene, ascii_band(a, b), wash);
        }

        // Find matches: one span per row a match covers, over its cells only.
        let row_end = row_off.saturating_add(bpr);
        let first = self.matches.partition_point(|m| m.end <= row_off);
        for (index, found) in self.matches.iter().enumerate().skip(first) {
            if found.start >= row_end {
                break;
            }
            let Some((a, b)) = span_in_row(found.start, found.end.saturating_sub(1), row_off, bpr)
            else {
                continue;
            };
            let current = self.match_idx == Some(index);
            ui::widget::paint_find_match(scene, transform, hex_band(a, b), current, theme);
            ui::widget::paint_find_match(scene, transform, ascii_band(a, b), current, theme);
        }

        if let Some((lo, hi)) = ctx.selection
            && let Some((a, b)) = span_in_row(lo, hi, row_off, bpr)
        {
            fill(scene, hex_band(a, b), colors.intent.selection_tint);
            fill(scene, ascii_band(a, b), colors.intent.selection_tint);
        }

        // The cursor: your colour round the cell of the column you type in,
        // a quiet outline on its mirror in the other column.
        if let Some(c) = ctx.cursor.checked_sub(row_off).filter(|&c| c < bpr) {
            let (active, mirror) = if self.model.column() == Column::Hex {
                (hex_band(c, c), ascii_band(c, c))
            } else {
                (ascii_band(c, c), hex_band(c, c))
            };
            scene.stroke(
                &Stroke::new(1.5),
                transform,
                colors.intent.you,
                None,
                &RoundedRect::from_rect(active, radius),
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                colors.text_tertiary,
                None,
                &RoundedRect::from_rect(mirror, radius),
            );
        }
    }

    /// Byte range the inspector reads: up to eight bytes from the cursor.
    pub(super) fn inspector_span(&self) -> Option<(usize, usize)> {
        let cur = self.model.cursor();
        if self.model.is_empty() {
            return None;
        }
        let width = (self.model.len() - cur).clamp(1, 8);
        Some((cur, cur + width - 1))
    }

    /// The persistent readout: where the cursor is, what is selected and
    /// edited, and how big the buffer is.
    fn paint_status(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let cursor = self.model.cursor();
        let mut left = format!("Offset 0x{cursor:08X} ({cursor})");
        let selected = self.model.selection_len();
        if selected > 0 {
            let _ = write!(left, "    {} selected", byte_count(selected));
        }
        let edited = self.model.dirty_count();
        if edited > 0 {
            let _ = write!(left, "    {} edited", byte_count(edited));
        }
        let right = byte_count(self.model.len());
        ui::chrome::status_bar::draw(scene, transform, tr, theme, self.geo.status, &left, &right);
    }

    /// The inspector: the region it stands on, its header, the endianness
    /// strip and the interpretations of the bytes at the cursor. The toggle
    /// that shows and hides it is drawn here too, on whichever ground it
    /// sits.
    fn paint_inspector_region(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let g = self.geo.clone();
        let mut toggle_theme = theme.clone();
        if g.inspector_visible {
            self.drawer.draw_scrim(scene, transform, theme);
            let local = self.drawer.paint_panel(scene, transform, theme);
            self.paint_inspector(scene, transform, tr, &local, presence, &g);
            if g.inspector.contains(g.toggle.center()) {
                toggle_theme = local;
            }
        }
        self.drawer.draw_toggle(
            scene,
            transform,
            tr,
            &toggle_theme,
            g.toggle,
            self.toggle_hovered,
        );
    }

    fn paint_inspector(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        g: &Geo,
    ) {
        let colors = &theme.tokens.colors;
        let inset = f64::from(theme.tokens.spacing.s4);
        let header_h = g.endian.y0 - g.inspector.y0;
        let style = theme.text(TextRole::Heading);
        let (_, line) = tr.measure_style("Inspector", None, &style);
        tr.draw_style(
            scene,
            "Inspector",
            colors.text_primary,
            transform
                * Affine::translate(Vec2::new(
                    g.inspector.x0 + inset,
                    g.inspector.y0 + (header_h - f64::from(line)) / 2.0,
                )),
            None,
            &style,
        );

        self.endian_group
            .set_selected(super::layout::endian_index(self.model.endian()));
        Widget::draw(
            &mut self.endian_group,
            scene,
            transform,
            tr,
            theme,
            presence,
        );

        let rows: Vec<Interpretation> =
            inspector::inspect(self.model.bytes(), self.model.cursor(), self.model.endian());
        let properties: Vec<PropertyRow<'_>> = rows
            .iter()
            .map(|r| {
                let kind = match r.label {
                    "bin" | "oct" | "char" => TextKind::Code,
                    _ => TextKind::Numeric,
                };
                PropertyRow::new(r.label, &r.value).with_kind(kind)
            })
            .collect();
        let face = PropertyListFace::new(&properties).striped();
        let area = g.properties;
        let total = face.height(tr, theme, area.width());
        let travel = (total - area.height()).max(0.0);
        self.inspector_scroll = self.inspector_scroll.clamp(0.0, travel);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &area);
        face.paint(
            &mut PaintCtx::new(scene, transform, tr, theme),
            Rect::new(
                area.x0,
                area.y0 - self.inspector_scroll,
                area.x1,
                area.y0 - self.inspector_scroll + total,
            ),
        );
        scene.pop_layer();
    }

    /// The no-file zero-state: one sentence and the one action.
    fn paint_empty_state(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.empty.apply_layout_external(self.rect);
        Widget::draw(&mut self.empty, scene, transform, tr, theme, presence);
    }

    /// Transient notices over the content: "Saved" in the bubble, a failed
    /// open or save as a toast.
    fn paint_notices(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let now = ui::time::now();
        let area = if self.has_file {
            self.geo.bubble_area
        } else {
            self.rect
        };
        self.status_bubble
            .paint(scene, transform, tr, theme, area, now);
        if let Some((title, detail, _)) = &self.problem {
            ui::widget::toast::paint_transient(
                scene,
                transform,
                tr,
                theme,
                area,
                ToastAnchor::BottomRight,
                &ToastCard::new(title, ToastKind::Error).body(detail),
            );
        }
    }
}

/// Narrows a bounded on-screen width to the renderer's `f32`.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a title room is a few thousand logical pixels at most, exact in f32"
)]
fn narrow(px: f64) -> f32 {
    px as f32
}
