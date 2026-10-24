#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;

impl VideoEditorApp {
    /// The bin's ground — the sidebar region docked, the floating drawer as an
    /// overlay — and the theme for what is drawn on it.
    pub(super) fn paint_bin_panel(
        scene: &mut Scene,
        tf: Affine,
        theme: &Theme,
        r: &Regions,
    ) -> Theme {
        if r.bin_overlay {
            paint_floating_panel(scene, tf, r.bin, theme)
        } else {
            ui::surface::region(scene, tf, r.bin, ui::surface::Region::Sidebar, theme);
            theme.in_region(ui::surface::Region::Sidebar)
        }
    }

    pub(super) fn draw_bin_header(&self, ctx: &mut BinPaintCtx<'_>, r: &Regions) {
        let c = &ctx.theme.tokens.colors;
        let add = self.add_title_rect(r);
        let band = Rect::new(r.bin.x0, add.y0, add.x0, add.y1);
        let heading = ctx.theme.text(TextRole::Subheading);
        let heading_y = text_baseline(band, &heading, ctx.text);
        let x0 = r.bin.x0 + PAD * ctx.scale;
        text(
            ctx.scene,
            ctx.text,
            ctx.transform,
            "Media",
            &heading,
            c.text_primary,
            x0,
            heading_y,
        );
        // How many sources the bin holds, as a quiet count beside its name.
        let count = self.project.media.len().to_string();
        let count_style = ctx.theme.text(TextRole::Subheading).numeric();
        let count_x = x0 + text_width(ctx.text, "Media", &heading) + S2;
        if count_x + text_width(ctx.text, &count, &count_style) < add.x0 - S2 {
            text(
                ctx.scene,
                ctx.text,
                ctx.transform,
                &count,
                &count_style,
                c.text_tertiary,
                count_x,
                heading_y,
            );
        }
        let face = ButtonFace::new(add, "Title")
            .icon(Icon::Plus)
            .role(ButtonRole::Plain)
            .control_size(ControlSize::Small)
            .accessible_name("New title")
            .hovered(add.contains(self.last_mouse));
        Button::paint(ctx.scene, ctx.transform, ctx.text, ctx.theme, &face);
    }

    pub(super) fn draw_bin_row_visual(
        &self,
        ctx: &mut BinPaintCtx<'_>,
        media_id: MediaId,
        media: &MediaItem,
        rect: Rect,
    ) -> Rect {
        let colors = &ctx.theme.tokens.colors;
        ui::chrome::paint_row_selection(
            ctx.scene,
            ctx.transform,
            rect,
            ui::chrome::SelectionState::new(
                self.selected_media == Some(media_id),
                self.hover_media == Some(media_id),
                false,
            ),
            colors.surfaces.content,
            ctx.theme,
        );

        let row_inset = S2 * ctx.scale;
        let thumb_width = 64.0 * ctx.scale;
        let thumb_height = (rect.height() - 2.0 * row_inset).clamp(0.0, thumb_width);
        let thumb_y0 = rect.center().y - thumb_height / 2.0;
        let thumb = Rect::new(
            rect.x0 + row_inset,
            thumb_y0,
            rect.x0 + row_inset + thumb_width,
            thumb_y0 + thumb_height,
        );
        let radius = f64::from(ctx.theme.tokens.radii.detail);
        let base = clip_color(ctx.theme, media.palette);
        // The source's own picture on the media ground, as footage is shown
        // everywhere else; audio draws its waveform there in its label colour.
        fill_round(
            ctx.scene,
            ctx.transform,
            thumb,
            radius,
            colors.media.letterbox,
        );
        if media.kind.is_generated() {
            let size = f64::from(ctx.theme.tokens.metrics.icon_lg);
            Icon::Type.draw(
                ctx.scene,
                ctx.transform,
                Rect::from_center_size(thumb.center(), (size, size)),
                colors.media.ink,
            );
        } else if media.kind.is_visual() {
            if let Some(thumbnail) = self.thumbnails.get(media_id) {
                blit_thumb(ctx.scene, ctx.transform, thumbnail, thumb, radius);
            }
        } else {
            let inner = Rect::new(
                thumb.x0 + 4.0,
                thumb.y0 + 6.0,
                thumb.x1 - 4.0,
                thumb.y1 - 6.0,
            );
            match (
                self.waveforms.state(media.id),
                self.waveforms.peaks(media.id),
            ) {
                (crate::waveform::WaveformState::Ready, Some(peaks)) => draw_waveform(
                    ctx.scene,
                    ctx.transform,
                    WaveformPaint {
                        area: inner,
                        peaks,
                        source_in: 0.0,
                        source_out: media.duration,
                        media_duration: media.duration,
                        volume: 1.0,
                        color: base,
                    },
                ),
                _ => draw_flat_signal(ctx.scene, ctx.transform, inner, base),
            }
        }

        let pending = if media.kind.is_generated() {
            false
        } else if media.kind.is_visual() {
            self.thumbnails.get(media_id).is_none()
        } else {
            matches!(
                self.waveforms.state(media.id),
                crate::waveform::WaveformState::Pending
                    | crate::waveform::WaveformState::Unrequested
            )
        };
        if pending {
            // Still being read: three quiet dots on the media ground.
            for index in 0..3 {
                let x = thumb.center().x + f64::from(index - 1) * S2;
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    colors.media.ink_dim,
                    None,
                    &Circle::new(Point::new(x, thumb.center().y), 2.0),
                );
            }
        }
        thumb
    }

    pub(super) fn draw_bin_row_text(
        &self,
        ctx: &mut BinPaintCtx<'_>,
        media_id: MediaId,
        media: &MediaItem,
        rect: Rect,
        thumb: Rect,
    ) {
        let colors = &ctx.theme.tokens.colors;
        let name_style = ctx.theme.text(TextRole::Label);
        let sub_style = ctx.theme.text(TextRole::Caption);
        let meta_style = ctx.theme.text(TextRole::Caption).numeric();
        let text_x = thumb.x1 + S2 * ctx.scale;
        let right = rect.x1 - S2 * ctx.scale;
        let name_h = line_height(ctx.text, &name_style);
        let sub_h = line_height(ctx.text, &sub_style);
        let block_top = rect.y0 + (rect.height() - (name_h + S1 + sub_h)) / 2.0;
        let sub_top = block_top + name_h + S1;
        // The source's length, trailing on the secondary line, so the name
        // above it has the row's whole width.
        let length = VideoEditorApp::bin_length(media);
        let length_w = text_width(ctx.text, &length, &meta_style);
        let meta_top = sub_top + (sub_h - line_height(ctx.text, &meta_style)) / 2.0;
        text(
            ctx.scene,
            ctx.text,
            ctx.transform,
            &length,
            &meta_style,
            colors.text_tertiary,
            right - length_w,
            meta_top,
        );
        let renaming = self.bin_rename.as_ref().map(|(id, _)| *id) == Some(media_id);
        if !renaming {
            let available = (right - text_x) as f32;
            let shown = elide_at(&media.name, available, &name_style, ctx.text);
            text(
                ctx.scene,
                ctx.text,
                ctx.transform,
                &shown,
                &name_style,
                colors.text_primary,
                text_x,
                block_top,
            );
        }
        let right = right - length_w - S2;
        // One secondary fact — what the source is — or, when its bytes cannot
        // be used, a warning badge saying why in its place.
        let problem = if self.media_is_unreadable(media_id) {
            Some((MEDIA_UNREADABLE, MEDIA_UNREADABLE_SHORT))
        } else if self.media_is_projected(media_id) {
            Some((MEDIA_NOT_LOCAL, MEDIA_NOT_LOCAL_SHORT))
        } else {
            None
        };
        if let Some((long, short)) = problem {
            let room = right - text_x;
            let label = [long, short]
                .into_iter()
                .find(|l| ui::widget::BadgeFace::intrinsic_width(l, ctx.text, ctx.theme) <= room)
                .unwrap_or(short);
            let w = ui::widget::BadgeFace::intrinsic_width(label, ctx.text, ctx.theme).min(room);
            let h = ui::widget::BadgeFace::height(ctx.theme);
            let cy = sub_top + sub_h / 2.0;
            let badge = Rect::new(text_x, cy - h / 2.0, text_x + w, cy + h / 2.0);
            ui::widget::BadgeFace::new(badge, label)
                .style(ui::widget::ChipStyle::Tinted(ui::widget::Tone::Warning))
                .paint(ctx.scene, ctx.transform, ctx.text, ctx.theme);
        } else {
            let kind = VideoEditorApp::bin_kind_words(media);
            let shown = elide_at(&kind, (right - text_x) as f32, &sub_style, ctx.text);
            text(
                ctx.scene,
                ctx.text,
                ctx.transform,
                &shown,
                &sub_style,
                colors.text_dim,
                text_x,
                sub_top,
            );
        }
    }

    pub(super) fn draw_bin_rows(&self, ctx: &mut BinPaintCtx<'_>, r: &Regions) {
        for (media_id, rect) in self.bin_item_rects(r) {
            let Some(media) = self.project.media(media_id) else {
                continue;
            };
            let thumb = self.draw_bin_row_visual(ctx, media_id, media, rect);
            self.draw_bin_row_text(ctx, media_id, media, rect, thumb);
        }
        if self.shows_focus_cursor(Pane::Bin)
            && let Some((_, row)) = self.bin_item_rects(r).get(self.focus.bin)
        {
            draw_focus_ring(
                ctx.scene,
                ctx.transform,
                ctx.theme,
                *row,
                f64::from(ctx.theme.tokens.radii.item),
            );
        }
    }

    pub(super) fn draw_bin(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let panel = Self::paint_bin_panel(scene, tf, theme, r);
        let theme = &panel;
        let bin_scale = self.bin_ui_scale();
        self.draw_bin_header(
            &mut BinPaintCtx {
                scene,
                transform: tf,
                text: tr,
                theme,
                scale: bin_scale,
            },
            r,
        );

        // Clip bin content so a long list can't bleed over the timeline.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            tf,
            &VideoEditorApp::bin_viewport(r),
        );
        if self.project.media.is_empty() {
            // One sentence and the one action that answers it.
            let area = Rect::new(r.bin.x0, r.bin.y0 + self.bin_list_top(), r.bin.x1, r.bin.y1);
            if let Some(btn) = self.empty_bin_import_rect(r) {
                let msg = theme.text(TextRole::Body);
                let h = line_height(tr, &msg);
                let w = text_width(tr, "No media yet", &msg);
                text(
                    scene,
                    tr,
                    tf,
                    "No media yet",
                    &msg,
                    theme.tokens.colors.text_dim,
                    area.center().x - w / 2.0,
                    btn.y0 - S3 - h,
                );
                draw_button(
                    scene,
                    tf,
                    tr,
                    theme,
                    btn,
                    "Import media",
                    ButtonRole::Primary,
                    btn.contains(self.last_mouse),
                );
            }
        }
        self.draw_bin_rows(
            &mut BinPaintCtx {
                scene,
                transform: tf,
                text: tr,
                theme,
                scale: bin_scale,
            },
            r,
        );
        self.draw_scrollbar(scene, tf, theme, ScrollPane::Bin, r);
        scene.pop_layer();
    }

    /// Draw feedback for a live bin→timeline drag: highlight the drop lane, mark
    /// the insertion point at the drop time, and float a ghost chip at the cursor.
    #[expect(
        clippy::many_single_char_names,
        reason = "the paint pass's shorthands: colours c, regions r, pointer p, drop time t and its x"
    )]
    pub(super) fn draw_bin_drag(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let Some(bd) = self.bin_drag else { return };
        if !bd.active {
            return;
        }
        let c = &theme.tokens.colors;
        let p = self.last_mouse;
        // Drop-lane highlight + insertion marker at the snapped drop time: an
        // insertion indicator, in your colour.
        if let Some((_, _, lane)) = self.bin_drop_lane(r) {
            fill_rect(
                scene,
                tf,
                Rect::new(r.lanes.x0, lane.y0, r.lanes.x1, lane.y1),
                c.intent.selection_tint,
            );
            let t = snap_to_frame(self.x_to_time(p.x, &r.lanes), self.project.fps);
            let x = self.time_to_x(t, &r.lanes).clamp(r.lanes.x0, r.lanes.x1);
            stroke_line(scene, tf, c.intent.you, x, lane.y0, x, lane.y1, 2.0);
        }
        // Ghost chip following the cursor: the clip it will become.
        if let Some(m) = self.project.media(bd.media) {
            let style = theme.text(TextRole::Label);
            let h = line_height(tr, &style) + S3;
            let w = (text_width(tr, &m.name, &style) + S4).min(self.bin_width());
            let gr = Rect::new(p.x + S3, p.y - h / 2.0, p.x + S3 + w, p.y + h / 2.0);
            let radius = f64::from(theme.tokens.radii.item);
            let (fill, ink) = on_fill(theme, clip_color(theme, m.palette));
            ui::surface::shadow(scene, tf, gr, radius, &theme.tokens.elevation.floating);
            fill_round(scene, tf, gr, radius, fill);
            let label = elide_at(&m.name, (gr.width() - S4) as f32, &style, tr);
            let y = text_baseline(gr, &style, tr);
            text(scene, tr, tf, &label, &style, ink, gr.x0 + S2, y);
        }
    }

    /// Draw the in-flight inline rename field over its bin row's name band.
    pub(super) fn draw_bin_rename(
        &mut self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        r: &Regions,
    ) {
        let mid = match &self.bin_rename {
            Some((m, _)) => *m,
            None => return,
        };
        let bin_visible = self.bin_is_visible(r);
        if !bin_visible {
            return;
        }
        let Some(row) = self
            .bin_item_rects(r)
            .into_iter()
            .find(|(m, _)| *m == mid)
            .map(|(_, row)| row)
        else {
            return;
        };
        let rect = VideoEditorApp::bin_name_rect(row);
        let panel = if r.bin_overlay {
            theme.on_ground(ui::chrome::panel_ground(theme))
        } else {
            theme.in_region(ui::surface::Region::Sidebar)
        };
        if let Some((_, label)) = self.bin_rename.as_mut() {
            label.apply_layout_external(rect);
            label.draw(scene, tf, tr, &panel, presence);
        }
    }
}

/// The floating surface a drawer or a summoned side panel stands on: the
/// floating shadow, then the floating material with its edge at the panel
/// radius. Returns the theme for what is drawn on it.
pub(super) fn paint_floating_panel(
    scene: &mut Scene,
    tf: Affine,
    rect: Rect,
    theme: &Theme,
) -> Theme {
    if rect.width() > 0.0 && rect.height() > 0.0 {
        ui::widget::floating_surface::paint(scene, tf, rect, theme);
    }
    theme.on_ground(ui::chrome::panel_ground(theme))
}
