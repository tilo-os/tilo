//! How a single-line field lays out and paints its one line: shaping it
//! into per-character offsets, scrolling the caret into view, and drawing
//! the selection, decorations, text and caret.

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};

use super::{LINE_HEIGHT_RATIO, TextInput};
use crate::paint::PaintCtx;
use crate::richtext::{RichTextLayout, TextAlign};
use crate::text_break::grapheme_indices;
use crate::text_render::{TextRenderer, TextStyle};
use crate::text_selection::TextPosition;
use crate::theme::Theme;

impl TextInput {
    /// Rebuilds `self.char_x_positions` (and, when unmasked, `self.single_layout`)
    /// for the current single-line text and returns the total shaped width.
    pub(super) fn build_single_line_metrics(
        &mut self,
        text_renderer: &mut TextRenderer,
        display: &str,
        text_size: f32,
        text_scale: f32,
    ) -> f32 {
        self.shaped_preparation = None;
        self.refresh_layout_epoch(text_renderer);
        self.char_x_positions.clear();
        self.char_x_positions
            .resize(self.text.len().saturating_add(1), 0.0);
        if self.masked() {
            self.drop_shaped_layouts();
            for (byte_idx, _) in grapheme_indices(&self.text) {
                let (w, _) =
                    self.measure_str(text_renderer, &self.display_prefix(byte_idx), text_size);
                if let Some(slot) = self.char_x_positions.get_mut(byte_idx) {
                    *slot = f64::from(w);
                }
            }
            let (width, _) = self.measure_str(text_renderer, display, text_size);
            if let Some(slot) = self.char_x_positions.get_mut(self.text.len()) {
                *slot = f64::from(width);
            }
            width
        } else {
            let family = self.family.clone().unwrap_or(self.role_family());
            let layout_size = text_size * text_scale;
            let mut style = TextStyle::new(layout_size)
                .with_line_height(LINE_HEIGHT_RATIO)
                .with_weight(self.text_role.weight())
                .with_family(family.clone());
            style.kind = self.kind.text_kind();
            let base = self.shaped_signature(display, &family, layout_size, None);
            let attributes = self.valid_single_attributes();
            let signature = attributes.map_or(base, |a| {
                Self::attributed_signature(base ^ Self::single_attributes_signature(a), &a.spans)
            });
            if self.single_layout.is_none() || self.shaped_layout_signature != signature {
                #[cfg(test)]
                let _phase = crate::profiling::frame_probe::span("single_line_shape");
                let browser_emoji = text_renderer.browser_color_emoji_installed();
                let mut layout = if let Some(attributes) = attributes {
                    RichTextLayout::shape_styled_browser(
                        &mut text_renderer.font_system,
                        &self.insert_preedit_span(attributes.spans.clone()),
                        &style,
                        None,
                        TextAlign::Left,
                        self.display_ranges(&attributes.underlines),
                        self.display_ranges(&attributes.strikes),
                        browser_emoji,
                    )
                } else {
                    RichTextLayout::shape_plain_browser(
                        &mut text_renderer.font_system,
                        display,
                        &style,
                        None,
                        browser_emoji,
                    )
                };
                if self.holds_secret() {
                    layout.discard_prepared_inputs();
                }
                text_renderer.note_browser_rich_layout(&layout);
                if self.core.has_preedit() {
                    let start = self.core.caret().min(self.text.len());
                    layout.add_underline(start..start.saturating_add(self.core.preedit().len()));
                }
                self.single_layout = Some(layout);
                self.shaped_layout_signature = signature;
            }
            let Some(layout) = self.single_layout.as_ref() else {
                return 0.0;
            };
            let end_byte = self.text_to_layout_byte(self.text.len());
            let bytes: Vec<_> = grapheme_indices(&self.text)
                .map(|(byte_idx, _)| byte_idx)
                .chain(std::iter::once(self.text.len()))
                .collect();
            // These bytes already come from the grapheme iterator. Re-clamping
            // each one would segment the full string again for every caret.
            // Resolve the composition insertion point once as well.
            let preedit = self.core.preedit_display_range(&self.text);
            let positions: Vec<_> = bytes
                .iter()
                .map(|byte| {
                    TextPosition::new(if *byte == self.text.len() {
                        end_byte
                    } else if let Some(range) = &preedit
                        && *byte > range.start
                    {
                        byte.saturating_add(range.len())
                    } else {
                        *byte
                    })
                })
                .collect();
            #[cfg(test)]
            let _phase = crate::profiling::frame_probe::span("single_line_caret_batch");
            let geometries = layout.caret_geometries(&positions);
            let width = layout.width();
            for (byte, geometry) in bytes.into_iter().zip(geometries) {
                if let Some(slot) = self.char_x_positions.get_mut(byte) {
                    *slot = f64::from(geometry.0);
                }
            }
            width
        }
    }

    pub(super) fn single_line_cursor_x_offset(&self, total_w: f32) -> f64 {
        let cursor_pos = self.core.caret().min(self.text.len());
        if self.core.has_preedit()
            && !self.masked()
            && let Some(layout) = &self.single_layout
        {
            f64::from(layout.caret_geometry(self.preedit_layout_caret()).0)
        } else if !self.masked()
            && let Some(layout) = &self.single_layout
        {
            f64::from(
                layout
                    .caret_geometry_at(self.text_to_layout_position(self.caret_position()))
                    .0,
            )
        } else if let Some(&x) = self.char_x_positions.get(cursor_pos) {
            x
        } else {
            f64::from(total_w)
        }
    }

    /// Keep the caret in view, scrolled no further than the text needs: a
    /// field that widens shows the start of its text again.
    pub(super) fn update_single_line_scroll(
        &mut self,
        cursor_x_offset: f64,
        content_w: f64,
        padding_x: f64,
        caret_width: f64,
    ) {
        // Include the caret's painted width, especially for padding-free code
        // lines: putting its left edge on the clip boundary hides it entirely.
        let visible_width = (self.rect.width()
            - padding_x
            - self.trailing_text_inset(padding_x)
            - self.accessory_room()
            - caret_width * 2.0)
            .max(0.0);
        if self.is_focused {
            if cursor_x_offset - self.scroll_x < 0.0 {
                self.scroll_x = cursor_x_offset;
            } else if cursor_x_offset - self.scroll_x > visible_width {
                self.scroll_x = cursor_x_offset - visible_width;
            }
            self.scroll_x = self.scroll_x.min((content_w - visible_width).max(0.0));
        } else {
            // Unfocused (e.g. a block-editor cell after load) must not scroll to the caret.
            self.scroll_x = 0.0;
        }
    }

    pub(super) fn draw_single_line_selection(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
    ) {
        let Some((sel_start, sel_end)) = self.selection_range() else {
            return;
        };
        let sel_color = theme.tokens.colors.intent.selection_tint;
        for sel_rect in self.selection_rects(theme, sel_start, sel_end) {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                sel_color,
                None,
                &sel_rect,
            );
        }
    }

    /// Code chips precede selection and glyphs; accent link rules follow
    /// glyphs. All bounds come from the exact layout used for interaction.
    pub(super) fn draw_single_line_decorations(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        backgrounds: bool,
    ) {
        if self.masked() {
            return;
        }
        let (Some(attributes), Some(layout)) =
            (self.valid_single_attributes(), &self.single_layout)
        else {
            return;
        };
        let ranges = if backgrounds {
            &attributes.chips
        } else {
            &attributes.links
        };
        for (start, end) in self.display_ranges(ranges) {
            for (x0, y0, x1, y1) in layout.selection_rects(start, end) {
                let x = self.single_origin.x - self.scroll_x;
                let y = self.single_origin.y;
                let rect = if backgrounds {
                    Rect::new(
                        x + f64::from(x0) - 2.0,
                        y + f64::from(y0) + 1.0,
                        x + f64::from(x1) + 2.0,
                        y + f64::from(y1) - 1.0,
                    )
                } else {
                    Rect::new(
                        x + f64::from(x0),
                        y + f64::from(y1) - 2.0,
                        x + f64::from(x1),
                        y + f64::from(y1) - 1.0,
                    )
                };
                let color = if backgrounds {
                    theme.tokens.colors.surfaces.raised
                } else {
                    theme.tokens.colors.text_primary
                };
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    color,
                    None,
                    &RoundedRect::from_rect(rect, if backgrounds { 3.0 } else { 0.0 }),
                );
            }
        }
    }

    /// Paint the placeholder and the field's text from `single_origin`, as
    /// laid out by `draw`; `display` is the masked text of a secret field.
    pub(super) fn draw_single_line_text(&self, ctx: &mut PaintCtx<'_>, display: &str) {
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let text_size = self.effective_font_size();
        let text_y = self.single_origin.y;
        let text_color = if self.effectively_disabled() {
            theme.text_dim
        } else {
            theme.text_primary
        };
        if self.shows_placeholder() {
            let placeholder = crate::i18n!(&self.placeholder);
            // `Affine * Affine` composes two geometric transforms over f64
            // coordinates; it has no integer overflow/wrap semantics.
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine transform composition over f64 screen coordinates"
            )]
            let placeholder_origin = transform * Affine::translate((self.single_origin.x, text_y));
            // The placeholder sets in the field's own style, in the tertiary
            // ink: a prompt, clearly weaker than any value.
            text_renderer.draw_style(
                scene,
                &placeholder,
                theme.tokens.colors.text_tertiary,
                placeholder_origin,
                None,
                &self.placeholder_style(text_size),
            );
        }
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine transform composition over f64 screen coordinates"
        )]
        let text_origin =
            transform * Affine::translate((self.single_origin.x - self.scroll_x, text_y));
        if !self.masked() {
            if let Some(layout) = &self.single_layout {
                text_renderer.draw_rich(scene, layout, text_origin, text_color);
                self.tint_selected_emoji(scene, text_origin, theme, layout);
            }
        } else if let Some(fam) = &self.family {
            text_renderer.draw_style(
                scene,
                display,
                text_color,
                text_origin,
                None,
                &TextStyle::new(text_size).with_family(fam.clone()),
            );
        } else {
            text_renderer.draw_style(
                scene,
                display,
                text_color,
                text_origin,
                None,
                &TextStyle::new(text_size).with_family(self.role_family()),
            );
        }
    }

    pub(super) fn draw_single_line_caret(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
    ) {
        if self.caret_paint_suppressed {
            return;
        }
        let cursor_rect = if self.core.has_preedit()
            && !self.masked()
            && let Some(layout) = &self.single_layout
        {
            let (x, top, height) = layout.caret_geometry(self.preedit_layout_caret());
            Rect::new(
                self.single_origin.x + f64::from(x) - self.scroll_x,
                self.single_origin.y + f64::from(top),
                self.single_origin.x + f64::from(x) - self.scroll_x + theme.caret_width,
                self.single_origin.y + f64::from(top + height),
            )
        } else {
            self.caret_rect_at(theme, self.caret_position())
        };
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.intent.accent_ink,
            None,
            &cursor_rect,
        );
    }
}
