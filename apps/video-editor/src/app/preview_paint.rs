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
        fill_rect(scene, tf, r.preview, c.surfaces.base);
        let canvas = r.canvas;
        // A short window crushes the letterboxed canvas to a few pixels. There
        // is nothing to show at that size, and the HUD and safe-area guide
        // drawn at fixed offsets would land outside it; say so instead.
        if canvas.width() < MIN_CANVAS || canvas.height() < MIN_CANVAS {
            let msg = "Not enough room for the preview";
            let (w, _) = tr.measure(msg, 11.5, None);
            let area = Rect::new(r.preview.x0, r.preview.y0, r.preview.x1, r.transport.y0);
            if area.height() > 16.0 {
                text(
                    scene,
                    tr,
                    tf,
                    msg,
                    11.5,
                    c.text_dim,
                    area.center().x - f64::from(w) / 2.0,
                    area.center().y - 7.0,
                );
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
        // Canvas backdrop (letterbox black).
        fill_rect(scene, tf, canvas, Color::BLACK);
        // One picture, produced by the GPU compositor in `crate::gpu` — the
        // *same* renderer the export runs, so the preview is byte-for-byte
        // what the export writes. It resolves every layer, grade, fade,
        // transition and effect itself; nothing here re-implements any of it.
        // The HUD below still draws on top, which is why this reads the frame
        // back and blits it rather than hooking the render target.
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
            // Nothing was composited, so there is no picture — and with no
            // picture the letterbox black is not standing for anything. Repaint
            // the canvas in the theme's own surface so the words below are
            // legible: theme text on a hard-coded black is dark-on-dark in
            // every light theme, which is how "No clip at playhead" came to be
            // near-invisible on a white desktop.
            fill_rect(scene, tf, canvas, c.surfaces.base);
            stroke_round(
                scene,
                tf,
                canvas.inset(-0.5),
                1.5,
                ui::surface::edge_on(c.surfaces.base),
                1.0,
            );
            // Nothing to composite, or nothing to composite *with*. Both are
            // said in words rather than papered over with an invented picture,
            // and both go through the shared empty state so the preview reads
            // like every other empty surface in the fleet instead of like a
            // stray caption on a black rectangle.
            let (icon, title, hint) = if self.project.top_video_clip_at(self.playhead).is_none() {
                (
                    ui::icons::Icon::Film,
                    PREVIEW_EMPTY_TITLE,
                    PREVIEW_EMPTY_HINT,
                )
            } else {
                // Not an empty state — a capability the box does not have.
                // Same treatment, different words, and it must not suggest
                // there is anything to do about it here.
                (
                    ui::icons::Icon::Monitor,
                    "Preview needs a GPU adapter",
                    "The timeline and export still work",
                )
            };
            ui::widget::EmptyState::paint(
                &mut ui::paint::PaintCtx::new(scene, tf, tr, theme),
                canvas,
                &ui::widget::EmptyStateContent {
                    icon: Some(icon),
                    title,
                    message: Some(hint),
                    action_label: None,
                },
                Point::new(f64::MIN, f64::MIN),
            );
        }
        // HUD overlays: the playhead's time, and nothing else.
        //
        // The frame size used to be painted in the canvas's top-right, which
        // is exactly where a corner picture-in-picture sits — and now that
        // `Transform` is real and `Transform::pip()` puts a layer there by
        // design, that collision is systematic rather than unlucky. Frame size
        // is static project metadata, so it belongs beside the timecode in the
        // transport (which states it) and in the export dialog, not painted
        // over the user's composition for the life of the project.
        //
        // What stays is per-frame information, and it takes a chip so it reads
        // over a bright picture as well as a dark one.
        let tc = timecode(self.playhead, self.project.fps);
        let (tcw, _) = tr.measure_weighted(&tc, 13.0, None, TextWeight::Bold);
        let tc_chip = Rect::new(
            canvas.x0 + 8.0,
            canvas.y0 + 6.0,
            canvas.x0 + 8.0 + f64::from(tcw) + 14.0,
            canvas.y0 + 28.0,
        );
        if tc_chip.x1 < canvas.x1 {
            fill_round(scene, tf, tc_chip, 5.0, with_alpha(Color::BLACK, 0.55));
            let baseline = text_baseline(tc_chip, 13.0, tr);
            text_w(
                scene,
                tr,
                tf,
                &tc,
                &TextStyle::new(13.0).with_weight(TextWeight::Bold),
                Color::WHITE,
                Point::new(tc_chip.x0 + 7.0, baseline),
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
                let (mw, _) = tr.measure_weighted(msg, 12.0, None, TextWeight::Semibold);
                let pad = 8.0;
                let chip = Rect::new(
                    canvas.x0 + 10.0,
                    canvas.y1 - 32.0,
                    canvas.x0 + 10.0 + f64::from(mw) + pad * 2.0,
                    canvas.y1 - 10.0,
                );
                fill_round(scene, tf, chip, 5.0, with_alpha(Color::BLACK, 0.62));
                stroke_round(scene, tf, chip, 5.0, with_alpha(Color::WHITE, 0.35), 1.0);
                let baseline = text_baseline(chip, 12.0, tr);
                text_w(
                    scene,
                    tr,
                    tf,
                    msg,
                    &TextStyle::new(12.0).with_weight(TextWeight::Semibold),
                    Color::WHITE,
                    Point::new(chip.x0 + pad, baseline),
                );
            }
        }
        // Safe-area guide: a faint white over footage, as every monitor draws
        // it; over the empty canvas — the theme's own surface — a derived
        // hairline, because white on a paper-white canvas is no guide at all.
        let guide = if showed_real {
            with_alpha(Color::WHITE, 0.12)
        } else {
            ui::surface::edge_on(c.surfaces.base)
        };
        stroke_round(
            scene,
            tf,
            canvas.inset(-canvas.width() * 0.05),
            0.0,
            guide,
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
        fill_rect(scene, tf, r.transport, c.surfaces.raised);
        stroke_line(
            scene,
            tf,
            pane_hairline(theme),
            r.transport.x0,
            r.transport.y0 + 0.5,
            r.transport.x1,
            r.transport.y0 + 0.5,
            1.0,
        );
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
            let is_play = action == TransportAction::PlayPause;
            let hovered = rect.contains(self.last_mouse);
            // The shared button face at a radius of half its width, so each
            // control is the fleet's button as a disc — Primary for play, the
            // neutral rest face with its edge for the rest — instead of a bare
            // ramp fill that had no edge and no press state.
            let mut face = ui::widget::ButtonFace::new(rect, "")
                .radius(rect.width() / 2.0)
                .hovered(hovered);
            if is_play {
                face = face.primary();
            }
            ui::widget::Button::paint(scene, tf, tr, theme, &face);
            let col = if is_play {
                on_fill(theme, theme.accent_primary).1
            } else {
                c.text_primary
            };
            self.draw_transport_icon(scene, tf, action, rect.center(), col);
            // The keyboard cursor. `focus.transport` indexes `TRANSPORT_NAMES`,
            // which is this array's order, so the ring lands on the button a
            // reader is told it is on.
            if cursor && self.focus.transport == i {
                draw_focus_ring_circle(scene, tf, theme, rect.center(), rect.width() / 2.0);
            }
        }
        // Undo / redo. Ghost squares rather than the transport's filled
        // circles: nothing here moves the playhead, and the difference in
        // shape is what says so before the icon is read.
        for (which, rect) in self.history_buttons(r) {
            let can = self.undo.status().can(which);
            let icon = match which {
                ui::UndoRequest::Undo => ui::icons::Icon::Undo2,
                ui::UndoRequest::Redo => ui::icons::Icon::Redo2,
            };
            let face = ui::widget::ButtonFace::new(rect, "")
                .ghost()
                .icon(icon)
                .radius(6.0)
                .accessible_name(which.title())
                // A control that cannot act must not light up under the
                // pointer — a hover state on a dead button reads as one
                // that is about to work.
                .hovered(rect.contains(self.last_mouse) && can)
                .disabled(!can);
            ui::widget::Button::paint(scene, tf, tr, theme, &face);
        }
        // Timecode readout. Each part is drawn only when it fits *whole*
        // inside the transport: a number the pane edge cuts mid-digit does not
        // read as a clipped number, it reads as a shorter one — "00:42:00"
        // arriving as "00:4" states a project length that is simply false.
        // Dropping the part is the honest narrow-window answer, and the parts
        // go in order of what the user cannot get elsewhere.
        let big = timecode(self.playhead, self.project.fps);
        let total = format!("/  {}", timecode(self.project.duration(), self.project.fps));
        // Project format — the app's own content area, rather than the OS's
        // title slot. This is the only place the frame size is stated now that
        // the canvas HUD no longer paints it over the picture.
        let meta = format!(
            "{}×{} · {:.0} fps",
            self.project.width, self.project.height, self.project.fps
        );
        // What the playhead is *doing* — playing, or shuttling in some
        // direction at some rate. It shares the readout's line rather than the
        // 16px band above the scrubber the old "PLAYING" used, which on a 58px
        // strip sits inside the 18px timecode's own line box.
        let motion = self.motion_words();
        let (bw, bh) = tr.measure_weighted(&big, 18.0, None, TextWeight::Bold);
        let (tw, th) = tr.measure(&total, 13.0, None);
        let (mw, mh) = tr.measure(&meta, 11.5, None);
        let (motion_w, motion_h) = motion.as_deref().map_or((0.0, 0.0), |s| {
            let (w, h) = tr.measure(s, 10.0, None);
            (f64::from(w), f64::from(h))
        });
        // Every part's top from one rule — see `readout_tops` — rather than
        // four hand-tuned offsets that were each right for one size at one
        // text scale.
        let [big_y, total_y, motion_y, meta_y] = readout_tops(
            r.transport,
            r.transport.y0 + TRANSPORT_CONTROL_CY * chrome_scale,
            [f64::from(bh), f64::from(th), motion_h, f64::from(mh)],
        );
        let fit = readout_fit(
            r.transport,
            self.transport_content_x(r),
            f64::from(bw),
            f64::from(tw),
            motion_w,
            f64::from(mw),
        );
        if let Some(x) = fit.big_x {
            text_w(
                scene,
                tr,
                tf,
                &big,
                &TextStyle::new(18.0).with_weight(TextWeight::Bold),
                c.text_primary,
                Point::new(x, big_y),
            );
        }
        if let Some(x) = fit.total_x {
            text(scene, tr, tf, &total, 13.0, c.text_dim, x, total_y);
        }
        // Playing is reported in the success tint it always was; a shuttle
        // takes the accent, so the two states are not one word apart. This used
        // to be gated on `playing` alone, which meant a shuttle running at 16×
        // reported itself nowhere at all: the playhead moved under its own
        // steam with nothing on screen naming why, or how fast, or which way.
        if let (Some(x), Some(words)) = (fit.motion_x, motion.as_deref()) {
            let tint = if self.playback.playing {
                c.semantic.success
            } else {
                c.accent[5]
            };
            text(scene, tr, tf, words, 10.0, tint, x, motion_y);
        }
        if let Some(x) = fit.meta_x {
            text(scene, tr, tf, &meta, 11.5, c.text_dim, x, meta_y);
        }
        // Scrubber.
        let scrub = self.scrubber_rect(r);
        if scrub.width() <= 1.0 {
            scene.pop_layer();
            return;
        }
        // The shared slider face, as the inspector's rows use: the fleet's
        // track, fill, knob and hover rather than a bare ramp bar under a
        // local knob.
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

    pub(super) fn draw_transport_icon(
        &self,
        scene: &mut Scene,
        tf: Affine,
        action: TransportAction,
        ctr: Point,
        col: Color,
    ) {
        let s = 6.0;
        match action {
            TransportAction::PlayPause => {
                // Pause bars whenever the playhead is running, shuttle
                // included: a play triangle over a moving playhead offers to
                // start something that is already going.
                if self.is_moving() {
                    fill_rect(
                        scene,
                        tf,
                        Rect::new(ctr.x - 5.0, ctr.y - 6.0, ctr.x - 1.0, ctr.y + 6.0),
                        col,
                    );
                    fill_rect(
                        scene,
                        tf,
                        Rect::new(ctr.x + 1.0, ctr.y - 6.0, ctr.x + 5.0, ctr.y + 6.0),
                        col,
                    );
                } else {
                    let mut p = BezPath::new();
                    p.move_to((ctr.x - 5.0, ctr.y - 7.0));
                    p.line_to((ctr.x + 7.0, ctr.y));
                    p.line_to((ctr.x - 5.0, ctr.y + 7.0));
                    p.close_path();
                    scene.fill(Fill::NonZero, tf, col, None, &p);
                }
            }
            // Step-by-frame is a *double* chevron with no bar. Jump-to-end is
            // a single chevron against a bar. They used to differ by one pixel
            // of bar position and read as the same button twice.
            TransportAction::StepBack => {
                for dx in [1.0, 7.0] {
                    let mut p = BezPath::new();
                    p.move_to((ctr.x - dx + s, ctr.y - s));
                    p.line_to((ctr.x - dx, ctr.y));
                    p.line_to((ctr.x - dx + s, ctr.y + s));
                    p.close_path();
                    scene.fill(Fill::NonZero, tf, col, None, &p);
                }
            }
            TransportAction::StepFwd => {
                for dx in [1.0, 7.0] {
                    let mut p = BezPath::new();
                    p.move_to((ctr.x + dx - s, ctr.y - s));
                    p.line_to((ctr.x + dx, ctr.y));
                    p.line_to((ctr.x + dx - s, ctr.y + s));
                    p.close_path();
                    scene.fill(Fill::NonZero, tf, col, None, &p);
                }
            }
            TransportAction::ToStart => {
                fill_rect(
                    scene,
                    tf,
                    Rect::new(ctr.x - 7.0, ctr.y - s, ctr.x - 4.5, ctr.y + s),
                    col,
                );
                let mut p = BezPath::new();
                p.move_to((ctr.x + 7.0, ctr.y - s));
                p.line_to((ctr.x - 3.0, ctr.y));
                p.line_to((ctr.x + 7.0, ctr.y + s));
                p.close_path();
                scene.fill(Fill::NonZero, tf, col, None, &p);
            }
            TransportAction::ToEnd => {
                fill_rect(
                    scene,
                    tf,
                    Rect::new(ctr.x + 4.5, ctr.y - s, ctr.x + 7.0, ctr.y + s),
                    col,
                );
                let mut p = BezPath::new();
                p.move_to((ctr.x - 7.0, ctr.y - s));
                p.line_to((ctr.x + 3.0, ctr.y));
                p.line_to((ctr.x - 7.0, ctr.y + s));
                p.close_path();
                scene.fill(Fill::NonZero, tf, col, None, &p);
            }
        }
    }
}
