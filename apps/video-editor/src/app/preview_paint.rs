#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;

impl VideoEditorApp {
    /// At normalised source coordinates `(u, v)`, or `None` when the canvas is not showing
    /// a decoded picture of it: on a build with no decoder, on an un-hydrated reference, and
    /// before the pipeline has prerolled the canvas shows the representative fill, and its
    /// colour as "the colour of your footage" would be a fabrication.
    pub fn preview_pixel(&self, normalized_x: f64, normalized_y: f64) -> Option<[f64; 3]> {
        #[cfg(feature = "decode")]
        {
            let media = self.project.top_video_clip_at(self.playhead)?.media;
            self.frame_source
                .as_ref()?
                .pixel(media, normalized_x, normalized_y)
        }
        #[cfg(not(feature = "decode"))]
        {
            let _ = (normalized_x, normalized_y);
            None
        }
    }

    /// Whether [`Self::preview_pixel`] has a decoded picture to sample.
    pub fn has_preview_pixel(&self) -> bool {
        #[cfg(feature = "decode")]
        {
            self.project
                .top_video_clip_at(self.playhead)
                .zip(self.frame_source.as_ref())
                .is_some_and(|(clip, source)| source.has_picture(clip.media))
        }
        #[cfg(not(feature = "decode"))]
        false
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the canvas paint pass, drawn back to front: letterbox, picture, overlays"
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
        // A short window crushes the canvas to a few pixels, and the HUD and safe-area guide
        // drawn at fixed offsets would land outside it.
        if canvas.width() < MIN_CANVAS || canvas.height() < MIN_CANVAS {
            let msg = &crate::resource::CATALOG.t("video-editor-preview-too-small");
            let style = theme.text(TextRole::Caption);
            let area = Rect::new(r.preview.x0, r.preview.y0, r.preview.x1, r.transport.y0);
            if area.height() > line_height(tr, &style) {
                let at = text_centre(area, msg, &style, tr);
                text(scene, tr, tf, msg, &style, c.text_dim, at.x, at.y);
            }
            return;
        }
        // Clipped to the pane, so an overlay at a fixed offset never bleeds into the bin, the
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
        // The *same* renderer the export runs, so the preview is what the export writes. It
        // resolves every layer, grade, fade, transition and effect; nothing here
        // re-implements any of it.
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
        // No clip under the playhead: whatever was composited is the bare
        // letterbox, so it says so, as it does when nothing was.
        let between_clips = !self.project.clips.is_empty()
            && self.project.top_video_clip_at(self.playhead).is_none();
        // An empty project says nothing here: the timeline below already shows what to do
        // with it.
        if !self.project.clips.is_empty() && (!showed_real || between_clips) {
            // Say why in words on the letterbox, in the media inks: text only, never an
            // invented picture.
            let title = if self.project.top_video_clip_at(self.playhead).is_none() {
                crate::resource::CATALOG.t(super::PREVIEW_EMPTY_TITLE)
            } else {
                // Not an empty state — a capability the box does not have.
                crate::resource::CATALOG.t(super::PREVIEW_UNAVAILABLE)
            };
            let title_style = theme.text(TextRole::Body).strong();
            let th = line_height(tr, &title_style);
            let offer = self.preview_offers_go_to_clip();
            let (top, go_to_clip) = Self::preview_hint_stack(canvas, tr, theme, th, offer);
            let room = (canvas.width() - S6) as f32;
            let title = elide_at(&title, room, &title_style, tr);
            let tw = text_width(tr, &title, &title_style);
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
            if let Some(button) = go_to_clip {
                let label = crate::resource::CATALOG.t(super::go_to_clip::GO_TO_CLIP_LABEL);
                let face = ButtonFace::new(button, &label)
                    .control_size(ControlSize::Small)
                    .hovered(button.contains(self.last_mouse));
                Button::paint(scene, tf, tr, theme, &face);
            }
        }
        // The picture behind this line is a stand-in generated from metadata, not the
        // user's footage.
        if let Some(clip) = self.project.top_video_clip_at(self.playhead) {
            let unusable = if self.media_is_unreadable(clip.media) {
                Some(crate::resource::CATALOG.t(MEDIA_UNREADABLE))
            } else if self.media_is_projected(clip.media) {
                Some(crate::resource::CATALOG.t(MEDIA_NOT_LOCAL))
            } else {
                None
            };
            if let Some(msg) = unusable {
                // On the floating material over the picture, so it reads over
                // a bright frame as well as a dark one.
                let hud = theme.on_ground(ui::chrome::panel_ground(theme));
                let style = theme.text(TextRole::Label);
                let mw = text_width(tr, &msg, &style);
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
                    &msg,
                    &style,
                    hud.text_primary,
                    chip.x0 + S2,
                    top,
                );
            }
        }
        // A faint line in the media ink, as every monitor draws it. Shown while something is
        // being placed in the frame: over a plain full-frame shot it would only be a line
        // across the picture.
        let placing = self
            .selection
            .primary()
            .and_then(|id| self.project.clip(id))
            .is_some_and(|clip| {
                !clip.transform.is_identity()
                    || self
                        .project
                        .media(clip.media)
                        .is_some_and(|m| m.kind == MediaKind::Text)
            });
        if placing {
            stroke_round(
                scene,
                tf,
                canvas.inset(-canvas.width() * 0.05),
                0.0,
                with_alpha(c.media.ink, 0.14),
                1.0,
            );
        }
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
            let (icon, message) = self.transport_icon(action);
            let name = crate::resource::CATALOG.t(message);
            // Play is the one main action of the strip; the rest are plain
            // glyph buttons beside it.
            let role = if action == TransportAction::PlayPause {
                ButtonRole::Primary
            } else {
                ButtonRole::Plain
            };
            // `focus.transport` indexes `TRANSPORT_NAMES`, which is this array's order.
            let face = ButtonFace::icon_only(rect, icon, &name)
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
            let (name, _) = self.history_words(which);
            let face = ButtonFace::icon_only(rect, icon, &name)
                // A hover state on a dead button reads as one about to work.
                .hovered(rect.contains(self.last_mouse) && can)
                .disabled(!can);
            Button::paint(scene, tf, tr, theme, &face);
        }
        // Each part is drawn only when it fits *whole*: "00:42:00" arriving as "00:4" states
        // a project length that is false. Parts go in order of what the user cannot get
        // elsewhere.
        let big = timecode(self.playhead, self.project.fps);
        // An empty timeline is no time long, whatever floor the scrubber's
        // arithmetic keeps under its length.
        let length = if self.project.clips.is_empty() {
            0.0
        } else {
            self.project.duration()
        };
        let total = format!("/ {}", timecode(length, self.project.fps));
        // Project format — the app's own content area, rather than the OS's
        // title slot.
        let meta = if self.project.width == 0 || self.project.height == 0 {
            String::new()
        } else {
            super::format_words(self.project.width, self.project.height, self.project.fps)
        };
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

    /// Step-by-frame is a chevron with no bar; jump-to-end is the skip glyph against a bar, so
    /// the two never read as the same button twice.
    pub(super) fn transport_icon(&self, action: TransportAction) -> (Icon, &'static str) {
        match action {
            // A play triangle over a moving playhead offers to start something already
            // going.
            TransportAction::PlayPause if self.is_moving() => (Icon::Pause, "video-editor-pause"),
            TransportAction::PlayPause => (Icon::Play, "video-editor-transport-play"),
            TransportAction::StepBack => (Icon::ChevronLeft, "video-editor-transport-step-back"),
            TransportAction::StepFwd => (Icon::ChevronRight, "video-editor-transport-step-forward"),
            TransportAction::ToStart => (Icon::SkipBack, "video-editor-transport-jump-start"),
            TransportAction::ToEnd => (Icon::SkipForward, "video-editor-transport-jump-end"),
        }
    }
}
