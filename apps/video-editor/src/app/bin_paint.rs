#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;

impl VideoEditorApp {
    pub(super) fn draw_bin_header(&self, ctx: &mut BinPaintCtx<'_>, r: &Regions) {
        let c = &ctx.theme.tokens.colors;
        fill_rect(ctx.scene, ctx.transform, r.bin, c.surfaces.raised);
        stroke_line(
            ctx.scene,
            ctx.transform,
            pane_hairline(ctx.theme),
            r.bin.x1 - 0.5,
            r.bin.y0,
            r.bin.x1 - 0.5,
            r.bin.y1,
            1.0,
        );
        let add = self.add_title_rect(r);
        let heading_y = text_baseline(add, 11.0, ctx.text);
        text_w(
            ctx.scene,
            ctx.text,
            ctx.transform,
            "MEDIA BIN",
            &TextStyle::new(11.0).with_weight(TextWeight::Semibold),
            c.text_dim,
            Point::new(r.bin.x0 + PAD * ctx.scale, heading_y),
        );
        let add = self.add_title_rect(r);
        let face = ui::widget::ButtonFace::new(add, "+ Title")
            .style(ui::widget::ButtonStyle::Neutral)
            .radius(5.0)
            .text_size(10.5)
            .weight(TextWeight::Medium)
            .hovered(add.contains(self.last_mouse));
        ui::widget::Button::paint(ctx.scene, ctx.transform, ctx.text, ctx.theme, &face);
        let count = format!("{} clips", self.project.media.len());
        let (count_width, _) = ctx.text.measure(&count, 11.0, None);
        let count_x = r.bin.x1 - 14.0 * ctx.scale - f64::from(count_width);
        if count_x > add.x1 + 8.0 * ctx.scale {
            text(
                ctx.scene,
                ctx.text,
                ctx.transform,
                &count,
                11.0,
                c.text_dim,
                count_x,
                heading_y,
            );
        }
    }

    pub(super) fn draw_bin_row_visual(
        &self,
        ctx: &mut BinPaintCtx<'_>,
        media_id: MediaId,
        media: &MediaItem,
        rect: Rect,
    ) -> Rect {
        let colors = &ctx.theme.tokens.colors;
        selection_row(
            ctx.scene,
            ctx.transform,
            ctx.theme,
            rect,
            f64::from(ctx.theme.tokens.radii.md),
            ui::chrome::SelectionState::new(
                self.selected_media == Some(media_id),
                self.hover_media == Some(media_id),
                false,
            ),
        );

        let row_inset = 8.0 * ctx.scale;
        let thumb_height = (rect.height() - 2.0 * row_inset).clamp(0.0, 64.0);
        let thumb_y0 = rect.center().y - thumb_height / 2.0;
        let thumb = Rect::new(
            rect.x0 + row_inset,
            thumb_y0,
            rect.x0 + row_inset + 64.0,
            thumb_y0 + thumb_height,
        );
        let base = clip_color(ctx.theme, media.palette);
        fill_round(ctx.scene, ctx.transform, thumb, 5.0, darken(base, 0.25));
        if media.kind.is_visual() {
            if let Some(thumbnail) = self.thumbnails.get(media_id) {
                blit_thumb(ctx.scene, ctx.transform, thumbnail, thumb, 5.0);
            } else {
                for index in 0..3 {
                    let x = thumb.x0 + 6.0 + f64::from(index) * 18.0;
                    let block = Rect::new(x, thumb.y0 + 6.0, x + 14.0, thumb.y1 - 6.0);
                    fill_round(ctx.scene, ctx.transform, block, 2.0, lighten(base, 0.18));
                }
            }
        } else {
            let color = lighten(base, 0.25);
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
                        color,
                    },
                ),
                _ => draw_flat_signal(ctx.scene, ctx.transform, inner, color),
            }
        }

        let pending = if media.kind.is_visual() {
            self.thumbnails.get(media_id).is_none()
        } else {
            matches!(
                self.waveforms.state(media.id),
                crate::waveform::WaveformState::Pending
                    | crate::waveform::WaveformState::Unrequested
            )
        };
        if pending {
            for index in 0..3 {
                let x = thumb.x0 + 10.0 + f64::from(index) * 8.0;
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    with_alpha(Color::WHITE, 0.35 + 0.2 * f64::from(index)),
                    None,
                    &Circle::new(Point::new(x, thumb.y1 - 8.0), 2.5),
                );
            }
        }
        let kind_color = match media.kind {
            MediaKind::Video => colors.semantic.info,
            MediaKind::Audio => colors.semantic.success,
            MediaKind::Image => colors.semantic.warning,
            MediaKind::Text => colors.accent[5],
        };
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            kind_color,
            None,
            &Circle::new(Point::new(thumb.x1 - 8.0, thumb.y0 + 8.0), 4.0),
        );
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
        let text_x = thumb.x1 + 12.0 * ctx.scale;
        let name_h = line_height(ctx.text, 13.0);
        let sub_h = line_height(ctx.text, 11.0);
        let block_top = rect.y0 + (rect.height() - (name_h + S1 + sub_h)) / 2.0;
        let sub_top = block_top + name_h + S1;
        let renaming = self.bin_rename.as_ref().map(|(id, _)| *id) == Some(media_id);
        if !renaming {
            let available = (rect.x1 - 8.0 - text_x) as f32;
            let shown = elide_w(&media.name, available, ctx.text);
            text_w(
                ctx.scene,
                ctx.text,
                ctx.transform,
                &shown,
                &TextStyle::new(13.0).with_weight(TextWeight::Medium),
                colors.text_primary,
                Point::new(text_x, block_top),
            );
        }
        let subtitle = VideoEditorApp::bin_subtitle(media);
        let (long, short) = if self.media_is_unreadable(media_id) {
            (Some(MEDIA_UNREADABLE), Some(MEDIA_UNREADABLE_SHORT))
        } else if self.media_is_projected(media_id) {
            (Some(MEDIA_NOT_LOCAL), Some(MEDIA_NOT_LOCAL_SHORT))
        } else {
            (None, None)
        };
        let (subtitle_width, _) = ctx.text.measure(&subtitle, 11.0, None);
        let right = rect.x1 - 8.0 * ctx.scale;
        let chip = long.zip(short).and_then(|(long, short)| {
            [
                (text_x + f64::from(subtitle_width) + 8.0, long),
                (text_x + f64::from(subtitle_width) + 8.0, short),
                (text_x, long),
                (text_x, short),
            ]
            .into_iter()
            .find(|(x0, label)| {
                x0 + f64::from(ctx.text.measure(label, 10.5, None).0) + 14.0 <= right
            })
        });
        if chip.is_none_or(|(x0, _)| x0 > text_x) {
            text(
                ctx.scene,
                ctx.text,
                ctx.transform,
                &subtitle,
                11.0,
                colors.text_dim,
                text_x,
                sub_top,
            );
        }
        if let Some((x0, label)) = chip {
            let width = f64::from(ctx.text.measure(label, 10.5, None).0) + 14.0 * ctx.scale;
            let top = sub_top - 1.5;
            let chip = Rect::new(x0, top, x0 + width, sub_top + sub_h + 1.5);
            fill_round(
                ctx.scene,
                ctx.transform,
                chip,
                4.0,
                with_alpha(colors.semantic.warning, 0.20),
            );
            stroke_round(
                ctx.scene,
                ctx.transform,
                chip,
                4.0,
                with_alpha(colors.semantic.warning, 0.65),
                1.0,
            );
            let baseline = text_baseline(chip, 10.5, ctx.text);
            text(
                ctx.scene,
                ctx.text,
                ctx.transform,
                label,
                10.5,
                colors.semantic.warning,
                chip.x0 + 7.0 * ctx.scale,
                baseline,
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
                row.inset(-FOCUS_RING_INSET),
                8.0 - FOCUS_RING_INSET,
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
        let c = &theme.tokens.colors;

        // Clip bin content so a long list can't bleed over the timeline.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            tf,
            &Rect::new(r.bin.x0, r.bin.y0 + 30.0, r.bin.x1, r.bin.y1),
        );
        if self.project.media.is_empty() {
            // Empty state: a real "Import media" affordance (the button opens the
            // host file picker) plus an honest status line.
            let cx = r.bin.center().x;
            let cy = r.bin.y0 + 120.0;
            let line1 = "No media yet";
            let (w1, _) = tr.measure(line1, 14.0, None);
            text(
                scene,
                tr,
                tf,
                line1,
                14.0,
                c.text_primary,
                cx - f64::from(w1) / 2.0,
                cy,
            );
            if let Some(btn) = self.empty_bin_import_rect(r) {
                draw_button(
                    scene,
                    tf,
                    tr,
                    theme,
                    btn,
                    "Import media",
                    true,
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
        // Last import outcome, pinned to the foot of the pane. It used to be
        // drawn only inside the empty-bin branch, so once the bin held
        // anything at all every import error was silent — six sites wrote it
        // and one read it. Errors tint danger; a plain count stays quiet.
        if let Some(notice) = &self.import_notice {
            let failed = notice.is_failure();
            let strip = Rect::new(r.bin.x0, r.bin.y1 - 34.0, r.bin.x1, r.bin.y1);
            // The severity came with the message from the place that knew the
            // outcome — a portal refusal is tinted as a refusal even though
            // its wording looks nothing like a probe's.
            let tint = notice.level.tint(theme);
            fill_rect(scene, tf, strip, with_alpha(tint, 0.16));
            stroke_line(
                scene,
                tf,
                with_alpha(tint, 0.5),
                strip.x0,
                strip.y0,
                strip.x1,
                strip.y0,
                1.0,
            );
            let col = if failed {
                c.semantic.danger
            } else {
                c.text_primary
            };
            let shown = elide_w(&notice.text, (strip.width() - 24.0) as f32, tr);
            let baseline = text_baseline(strip, 11.0, tr);
            text(scene, tr, tf, &shown, 11.0, col, strip.x0 + 12.0, baseline);
        }
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
        // Drop-lane highlight + insertion marker at the snapped drop time.
        if let Some((_, _, lane)) = self.bin_drop_lane(r) {
            fill_rect(
                scene,
                tf,
                Rect::new(r.lanes.x0, lane.y0, r.lanes.x1, lane.y1),
                with_alpha(c.accent[5], 0.16),
            );
            let t = snap_to_frame(self.x_to_time(p.x, &r.lanes), self.project.fps);
            let x = self.time_to_x(t, &r.lanes).clamp(r.lanes.x0, r.lanes.x1);
            stroke_line(scene, tf, c.accent[5], x, lane.y0, x, lane.y1, 2.0);
        }
        // Ghost chip following the cursor.
        if let Some(m) = self.project.media(bd.media) {
            let (w, h) = (154.0, 30.0);
            let gr = Rect::new(p.x + 12.0, p.y - h / 2.0, p.x + 12.0 + w, p.y + h / 2.0);
            let ghost_fill = clip_color(theme, m.palette);
            fill_round(scene, tf, gr, 6.0, with_alpha(ghost_fill, 0.92));
            stroke_round(scene, tf, gr, 6.0, with_alpha(c.border_focus, 0.9), 1.0);
            let ghost_label = elide_w(&m.name, (gr.width() - 20.0) as f32, tr);
            let ghost_y = text_baseline(gr, 12.0, tr);
            text_w(
                scene,
                tr,
                tf,
                &ghost_label,
                &TextStyle::new(12.0).with_weight(TextWeight::Medium),
                on_fill(theme, ghost_fill).1,
                Point::new(gr.x0 + 10.0, ghost_y),
            );
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
        if let Some((_, label)) = self.bin_rename.as_mut() {
            label.apply_layout_external(rect);
            label.draw(scene, tf, tr, theme, presence);
        }
    }
}
