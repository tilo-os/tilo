#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;

impl VideoEditorApp {
    #[expect(
        clippy::too_many_lines,
        reason = "the canvas paint pass, drawn back to front: letterbox, picture, overlays, readouts"
    )]
    pub(super) fn draw_preview(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let c = &theme.tokens.colors;
        let canvas = r.canvas;
        // A short window crushes the letterboxed canvas to a few pixels. There
        // is nothing to show at that size, and the HUD and safe-area guide
        // drawn at fixed offsets would land outside it; say so instead.
        if canvas.width() < MIN_CANVAS || canvas.height() < MIN_CANVAS {
            let msg = "Not enough room for the preview";
            let style = theme.text(TextRole::Caption);
            let area = Rect::new(r.preview.x0, r.preview.y0, r.preview.x1, r.transport.y0);
            if area.height() > line_height(tr, &style) {
                let at = text_centre(area, msg, &style, tr);
                text(scene, tr, tf, msg, &style, c.text_dim, at.x, at.y);
            }
            return;
        }
        // Everything below is clipped to the preview pane, so an overlay drawn
        // at a fixed offset from the canvas can never bleed into the bin, the
        // inspector or the transport.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            tf,
            &Rect::new(r.preview.x0, r.preview.y0, r.preview.x1, r.transport.y0),
        );
        // The frame's own ground: the media letterbox, the same in every theme.
        fill_rect(scene, tf, canvas, c.media.letterbox);
        // One picture, produced by the GPU compositor in `crate::gpu` — the
        // *same* renderer the export runs, so the preview is byte-for-byte
        // what the export writes. It resolves every layer, grade, fade,
        // transition and effect itself; nothing here re-implements any of it.
        let showed_real = match &self.composited {
            Some((w, h, rgba)) if *w > 0 && *h > 0 => {
                let frame = ::media::VideoFrame {
                    width: *w,
                    height: *h,
                    rgba: rgba.clone(),
                    pts_ms: 0,
                };
                blit_frame(scene, tf, &frame, canvas);
                true
            }
            _ => false,
        };
        if !showed_real {
            // Nothing was composited, so there is no picture. Say why in
            // words on the letterbox, in the media inks — text only, never an
            // invented picture or a glyph that looks like a control.
            let (title, hint) = if self.project.top_video_clip_at(self.playhead).is_none() {
                (PREVIEW_EMPTY_TITLE, PREVIEW_EMPTY_HINT)
            } else {
                // Not an empty state — a capability the box does not have.
                // Same treatment, different words, and it must not suggest
                // there is anything to do about it here.
                (
                    "Preview needs a GPU adapter",
                    "The timeline and export still work",
                )
            };
            let title_style = theme.text(TextRole::Body).strong();
            let hint_style = theme.text(TextRole::Caption);
            let th = line_height(tr, &title_style);
            let hh = line_height(tr, &hint_style);
            let top = canvas.center().y - (th + S1 + hh) / 2.0;
            let room = (canvas.width() - S6) as f32;
            let title = elide_at(title, room, &title_style, tr);
            let hint = elide_at(hint, room, &hint_style, tr);
            let tw = text_width(tr, &title, &title_style);
            let hw = text_width(tr, &hint, &hint_style);
            text(
                scene,
                tr,
                tf,
                &title,
                &title_style,
                c.media.ink,
                canvas.center().x - tw / 2.0,
                top,
            );
            text(
                scene,
                tr,
                tf,
                &hint,
                &hint_style,
                c.media.ink_dim,
                canvas.center().x - hw / 2.0,
                top + th + S1,
            );
        }
        // HUD overlays: the playhead's time, and nothing else, on the floating
        // material over the picture so it reads over a bright frame as well
        // as a dark one.
        let hud = theme.on_ground(ui::chrome::panel_ground(theme));
        let tc = timecode(self.playhead, self.project.fps);
        let tc_style = theme.text(TextRole::Label).numeric();
        let tcw = text_width(tr, &tc, &tc_style);
        let tch = line_height(tr, &tc_style);
        let tc_chip = Rect::new(
            canvas.x0 + S2,
            canvas.y0 + S2,
            canvas.x0 + S2 + tcw + S4,
            canvas.y0 + S2 + tch + S2,
        );
        if tc_chip.x1 < canvas.x1 && tc_chip.y1 < canvas.y1 {
            ui::widget::floating_surface::paint(scene, tf, tc_chip, theme);
            let top = text_baseline(tc_chip, &tc_style, tr);
            text(
                scene,
                tr,
                tf,
                &tc,
                &tc_style,
                hud.text_primary,
                tc_chip.x0 + S2,
                top,
            );
        }
        // The clip under the playhead has no bytes on this device: say so on the
        // canvas, because the picture behind this line is a stand-in generated
        // from metadata, not the user's footage.
        if let Some(clip) = self.project.top_video_clip_at(self.playhead) {
            let unusable = if self.media_is_unreadable(clip.media) {
                Some(MEDIA_UNREADABLE)
            } else if self.media_is_projected(clip.media) {
                Some(MEDIA_NOT_LOCAL)
            } else {
                None
            };
            if let Some(msg) = unusable {
                let style = theme.text(TextRole::Label);
                let mw = text_width(tr, msg, &style);
                let mh = line_height(tr, &style);
                let chip = Rect::new(
                    canvas.x0 + S2,
                    canvas.y1 - S2 - mh - S2,
                    canvas.x0 + S2 + mw + S4,
                    canvas.y1 - S2,
                );
                ui::widget::floating_surface::paint(scene, tf, chip, theme);
                let top = text_baseline(chip, &style, tr);
                text(
                    scene,
                    tr,
                    tf,
                    msg,
                    &style,
                    hud.text_primary,
                    chip.x0 + S2,
                    top,
                );
            }
        }
        // Safe-area guide: a faint line in the media ink, as every monitor
        // draws it, over the footage and the letterbox alike.
        stroke_round(
            scene,
            tf,
            canvas.inset(-canvas.width() * 0.05),
            0.0,
            with_alpha(c.media.ink, 0.14),
            1.0,
        );
        scene.pop_layer();
    }

    #[allow(
        clippy::too_many_lines,
        reason = "transport controls, scaled time readouts, and scrubber share one collision-free composition"
    )]
    pub(super) fn draw_transport(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let c = &theme.tokens.colors;
        let chrome_scale = self.chrome_scale();
        // Everything below is clipped to the transport, so no control drawn
        // at a fixed offset can reach into the pane beside it.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            tf,
            &r.transport,
        );
        let cursor = self.shows_focus_cursor(Pane::Transport);
        for (i, (action, rect)) in self.transport_buttons(r).into_iter().enumerate() {
            let hovered = rect.contains(self.last_mouse);
            let (icon, name) = self.transport_icon(action);
            // Play is the one main action of the strip; the rest are plain
            // glyph buttons beside it.
            let role = if action == TransportAction::PlayPause {
                ButtonRole::Primary
            } else {
                ButtonRole::Plain
            };
            // The keyboard cursor is the face's own focus ring.
            // `focus.transport` indexes `TRANSPORT_NAMES`, which is this
            // array's order, so the ring lands on the button a reader is told
            // it is on.
            let face = ButtonFace::icon_only(rect, icon, name)
                .role(role)
                .hovered(hovered)
                .focused(cursor && self.focus.transport == i);
            Button::paint(scene, tf, tr, theme, &face);
        }
        // Undo / redo: the same plain glyph buttons as the transport's, set
        // apart by the gap before them rather than by a different shape.
        for (which, rect) in self.history_buttons(r) {
            let can = self.undo.status().can(which);
            let icon = match which {
                ui::UndoRequest::Undo => Icon::Undo2,
                ui::UndoRequest::Redo => Icon::Redo2,
            };
            let face = ButtonFace::icon_only(rect, icon, which.title())
                // A control that cannot act must not light up under the
                // pointer — a hover state on a dead button reads as one
                // that is about to work.
                .hovered(rect.contains(self.last_mouse) && can)
                .disabled(!can);
            Button::paint(scene, tf, tr, theme, &face);
        }
        // Timecode readout. Each part is drawn only when it fits *whole*
        // inside the transport: a number the pane edge cuts mid-digit does not
        // read as a clipped number, it reads as a shorter one — "00:42:00"
        // arriving as "00:4" states a project length that is simply false.
        // Dropping the part is the honest narrow-window answer, and the parts
        // go in order of what the user cannot get elsewhere.
        let big = timecode(self.playhead, self.project.fps);
        let total = format!("/ {}", timecode(self.project.duration(), self.project.fps));
        // Project format — the app's own content area, rather than the OS's
        // title slot.
        let meta = format!(
            "{}×{}, {:.0} fps",
            self.project.width, self.project.height, self.project.fps
        );
        // What the playhead is *doing* — playing, or shuttling in some
        // direction at some rate. It shares the readout's line.
        let motion = self.motion_words();
        let big_style = theme.text(TextRole::Title).numeric();
        let total_style = theme.text(TextRole::Body).numeric();
        let meta_style = theme.text(TextRole::Caption).numeric();
        let motion_style = theme.text(TextRole::Caption).numeric();
        let (bw, bh) = (
            text_width(tr, &big, &big_style),
            line_height(tr, &big_style),
        );
        let (tw, th) = (
            text_width(tr, &total, &total_style),
            line_height(tr, &total_style),
        );
        let (mw, mh) = (
            text_width(tr, &meta, &meta_style),
            line_height(tr, &meta_style),
        );
        let (motion_w, motion_h) = motion.as_deref().map_or((0.0, 0.0), |s| {
            (
                text_width(tr, s, &motion_style),
                line_height(tr, &motion_style),
            )
        });
        // Every part's top from one rule — see `readout_tops`.
        let [big_y, total_y, motion_y, meta_y] = readout_tops(
            r.transport,
            r.transport.y0 + TRANSPORT_CONTROL_CY * chrome_scale,
            [bh, th, motion_h, mh],
        );
        let fit = readout_fit(
            r.transport,
            self.transport_content_x(r),
            bw,
            tw,
            motion_w,
            mw,
        );
        if let Some(x) = fit.big_x {
            text(scene, tr, tf, &big, &big_style, c.text_primary, x, big_y);
        }
        if let Some(x) = fit.total_x {
            text(scene, tr, tf, &total, &total_style, c.text_dim, x, total_y);
        }
        // Something is happening: the playhead running, forwards or at a
        // shuttle rate. A shuttle running at 16× used to report itself
        // nowhere at all.
        if let (Some(x), Some(words)) = (fit.motion_x, motion.as_deref()) {
            text(
                scene,
                tr,
                tf,
                words,
                &motion_style,
                c.semantic.info,
                x,
                motion_y,
            );
        }
        if let Some(x) = fit.meta_x {
            text(scene, tr, tf, &meta, &meta_style, c.text_dim, x, meta_y);
        }
        // Scrubber.
        let scrub = self.scrubber_rect(r);
        if scrub.width() <= 1.0 {
            scene.pop_layer();
            return;
        }
        // The shared slider face, as the inspector's rows use.
        let frac = (self.playhead / self.project.duration()).clamp(0.0, 1.0);
        draw_track(
            scene,
            tf,
            theme,
            scrub,
            frac,
            hit_slider(scrub, self.last_mouse),
            self.drag == Some(Drag::Scrubber),
        );
        scene.pop_layer();
    }

    /// The icon a transport button wears and the name a reader hears for it.
    /// Step-by-frame is a chevron with no bar; jump-to-end is the skip glyph
    /// against a bar, so the two never read as the same button twice.
    pub(super) fn transport_icon(&self, action: TransportAction) -> (Icon, &'static str) {
        match action {
            // Pause whenever the playhead is running, shuttle included: a play
            // triangle over a moving playhead offers to start something that
            // is already going.
            TransportAction::PlayPause if self.is_moving() => (Icon::Pause, "Pause"),
            TransportAction::PlayPause => (Icon::Play, "Play"),
            TransportAction::StepBack => (Icon::ChevronLeft, "Step back one frame"),
            TransportAction::StepFwd => (Icon::ChevronRight, "Step forward one frame"),
            TransportAction::ToStart => (Icon::SkipBack, "Jump to start"),
            TransportAction::ToEnd => (Icon::SkipForward, "Jump to end"),
        }
    }
}
