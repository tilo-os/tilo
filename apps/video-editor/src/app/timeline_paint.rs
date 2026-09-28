#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;

impl VideoEditorApp {
    /// The master meter and the standing clipping warning.
    ///
    /// Two states and they read completely differently, which is the point.
    /// Resting, it is a stereo bar and the loudest the mix has been, in
    /// decibels — the reading a person mixing wants anyway. Once the mix has
    /// gone over, the right-hand chip turns into a red **CLIP** stating how
    /// far over, and it stays that way: it survives the transport stopping,
    /// the playhead moving and the panel being folded, and it is cleared only
    /// by changing the levels it is a report about.
    pub(super) fn draw_master_strip(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let c = &theme.tokens.colors;
        let strip = VideoEditorApp::master_strip_rect(r);
        if strip.width() < 60.0 || strip.height() < 20.0 {
            return;
        }
        fill_rect(scene, tf, strip, c.surfaces.raised);
        // The readout line and the meter under it, centred as one block a
        // step apart (`chrome_layout::master_strip`): the meter used to sit two
        // pixels under the words.
        let m = chrome_layout::master_strip(strip);
        // Nothing to mix, nothing to meter. A dead meter on a picture-only
        // timeline would read as an output that is broken.
        if !crate::audio::has_audio(&self.project) {
            let y = text_baseline(strip, 9.0, tr);
            text(scene, tr, tf, "NO AUDIO", 9.0, c.text_dim, m.readout.x0, y);
            return;
        }
        let readout_y = text_baseline(m.readout, 9.0, tr);
        text(
            scene,
            tr,
            tf,
            "MIX",
            9.0,
            c.text_dim,
            m.readout.x0,
            readout_y,
        );
        let flag = VideoEditorApp::clip_flag_rect(r);
        if self.mix.clipped() {
            let hot = flag.contains(self.last_mouse);
            let bg = if hot {
                lighten(c.semantic.danger, 0.15)
            } else {
                c.semantic.danger
            };
            let (bg, fg) = on_fill(theme, bg);
            fill_round(scene, tf, flag, 4.0, bg);
            let words = format!("CLIP {:+.1}", self.mix.worst_over_db());
            let at = text_centre(flag, &words, 9.0, TextWeight::Semibold, tr);
            text_w(
                scene,
                tr,
                tf,
                &words,
                &TextStyle::new(9.0).with_weight(TextWeight::Semibold),
                fg,
                Point::new(at.x, at.y),
            );
        } else {
            // The peak-hold, or an honest blank. "-∞" would say the mix is
            // silent; nothing has been measured, which is a different claim.
            let words = self
                .mix
                .hold_db()
                .map_or_else(|| "—".to_string(), |db| format!("{db:.1} dB"));
            let (w, _) = tr.measure(&words, 9.0, None);
            text(
                scene,
                tr,
                tf,
                &words,
                9.0,
                c.text_dim,
                flag.x1 - f64::from(w),
                readout_y,
            );
        }
        draw_level_meter(
            scene,
            tf,
            theme,
            m.meter,
            self.mix.master,
            self.mix.master_hold,
            self.mix.clipped(),
        );
    }

    #[allow(
        clippy::indexing_slicing,
        clippy::too_many_lines,
        reason = "timeline rendering indexes validated lane geometry and keeps ruler, headers, clips, and scrollbars in one coordinate pass"
    )]
    pub(super) fn draw_timeline(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        r: &Regions,
    ) {
        let c = &theme.tokens.colors;
        fill_rect(scene, tf, r.timeline, c.surfaces.base);
        // The ruler row's top pixel: derived against the raised ruler and
        // master strip it is drawn on, so the seam shows against the raised
        // side panes above it too.
        stroke_line(
            scene,
            tf,
            pane_hairline(theme),
            r.timeline.x0,
            r.timeline.y0 + 0.5,
            r.timeline.x1,
            r.timeline.y0 + 0.5,
            1.0,
        );

        // --- master strip (the corner above the track heads) ---
        self.draw_master_strip(scene, tf, tr, theme, r);

        // --- ruler ticks ---
        fill_rect(scene, tf, r.ruler, c.surfaces.raised);
        // Ruler labels are rendered at the user's text scale while time is
        // measured in physical timeline pixels.  Ask for proportionally more
        // spacing so large labels remain individual readings instead of a
        // single unreadable run of digits.
        let tick_step = nice_tick(self.zoom / self.text_scale);
        let start_t = self.scroll.max(0.0);
        let end_t = self.x_to_time(r.lanes.x1, &r.lanes);
        let first_tick = (start_t / tick_step).floor();
        let ticks = (0u32..)
            .map(|k| (first_tick + f64::from(k)) * tick_step)
            .take_while(|&t| t <= end_t);
        for t in ticks {
            let x = self.time_to_x(t, &r.lanes);
            if x >= r.ruler.x0 - 1.0 && x <= r.ruler.x1 {
                stroke_line(
                    scene,
                    tf,
                    pane_hairline(theme),
                    x,
                    r.ruler.y1 - 8.0,
                    x,
                    r.ruler.y1,
                    1.0,
                );
                let label = timecode(t, self.project.fps);
                let (label_w, _) = tr.measure(&label, 10.0, None);
                // The fit-to-window chip owns the ruler's right end.  Tick
                // labels are helpful context, not a reason to draw through a
                // control (especially when text scaling widens both).
                if x + 4.0 + f64::from(label_w) <= self.zoom_chip_rect(r).x0 - 4.0 {
                    text(
                        scene,
                        tr,
                        tf,
                        &label,
                        10.0,
                        c.text_dim,
                        x + 4.0,
                        r.ruler.y0 + 6.0,
                    );
                }
                // faint grid line down the lanes
                stroke_line(
                    scene,
                    tf,
                    ui::surface::edge_on_with(c.surfaces.base, ui::chrome::HAIRLINE_ALPHA / 2),
                    x,
                    r.lanes.y0,
                    x,
                    r.timeline.y1,
                    1.0,
                );
            }
        }

        // --- where the mix went over ---
        // The other half of the warning. Knowing the mix clipped and not
        // knowing where is knowing you have to scrub ninety seconds; these
        // marks put it on the ruler, in the same place a person already looks
        // to find a moment, and they stay there until the levels change.
        for over in &self.mix.overs {
            let x = self.time_to_x(*over, &r.lanes);
            if x < r.ruler.x0 || x > r.ruler.x1 {
                continue;
            }
            fill_rect(
                scene,
                tf,
                Rect::new(x - 1.5, r.ruler.y0 + 2.0, x + 1.5, r.ruler.y0 + 9.0),
                c.semantic.danger,
            );
        }

        // --- render range ---
        // The in / out marks the export honours, drawn before the markers so
        // a marker flag sits over the band rather than under it. A range that
        // is set has to be visible: an export that is eight seconds long when
        // the timeline is ninety needs a reason on screen.
        if self.project.has_marks() {
            let (from, to) = self.project.export_range();
            let ax = self.time_to_x(from, &r.lanes).clamp(r.lanes.x0, r.lanes.x1);
            let bx = self.time_to_x(to, &r.lanes).clamp(r.lanes.x0, r.lanes.x1);
            if bx > ax {
                fill_rect(
                    scene,
                    tf,
                    Rect::new(ax, r.ruler.y0, bx, r.ruler.y0 + 3.0),
                    c.accent[5],
                );
            }
            // A bracket at each mark, opening towards the span it keeps, and
            // a hairline down the lanes so the mark can be lined up against
            // every track at once — the same story the marker flags tell.
            for (mark, towards) in [(self.project.mark_in, 1.0), (self.project.mark_out, -1.0)] {
                let Some(t) = mark else { continue };
                let x = self.time_to_x(t, &r.lanes);
                if x < r.lanes.x0 - 8.0 || x > r.lanes.x1 + 8.0 {
                    continue;
                }
                let far = x + towards * 8.0;
                fill_rect(
                    scene,
                    tf,
                    Rect::new(x.min(far), r.ruler.y0, x.max(far), r.ruler.y0 + 8.0),
                    c.accent[5],
                );
                // Only down the ruler. A line carried into the lanes is
                // what the playhead and a marker are, and a third one in the
                // same colour reads as one of those rather than as a mark;
                // the scrim below already draws the boundary across the
                // lanes.
                stroke_line(scene, tf, c.accent[5], x, r.ruler.y0, x, r.ruler.y1, 2.0);
            }
        }

        // --- markers ---
        // Every marker on one absolute timeline: the project's own, plus
        // each clip's rebased onto where that clip sits. A flag on the
        // ruler with a hairline down the lanes, so a marked instant can be
        // lined up against every track at once.
        for (at, marker, on_clip) in self.project.all_markers() {
            let x = self.time_to_x(at, &r.lanes);
            if x < r.ruler.x0 - 1.0 || x > r.ruler.x1 {
                continue;
            }
            let col = clip_color(theme, marker.palette);
            stroke_line(
                scene,
                tf,
                with_alpha(col, 0.55),
                x,
                r.lanes.y0,
                x,
                r.timeline.y1,
                1.0,
            );
            // A clip's marker is drawn as a smaller tick than a project
            // one: it belongs to the footage, not to the timeline.
            let h = if on_clip.is_some() { 6.0 } else { 10.0 };
            let flag = Rect::new(x, r.ruler.y1 - h, x + 3.0, r.ruler.y1);
            fill_rect(scene, tf, flag, col);
            if on_clip.is_none() && !marker.name.is_empty() {
                text(
                    scene,
                    tr,
                    tf,
                    &marker.name,
                    9.0,
                    col,
                    x + 6.0,
                    r.ruler.y1 - 11.0,
                );
            }
        }

        // --- lanes + headers + clips ---
        let lanes = self.lane_rects(r);
        for (tid, kind, lane) in &lanes {
            // Lane stripe: a lifted band with its derived edge, so the lane
            // reads against the base in light as well as dark — the sound
            // lanes used to be a 5 % success wash that vanished on white.
            // Both kinds take the same lift; a sound lane keeps a hint of its
            // kind in it.
            let stripe_rect = Rect::new(r.lanes.x0, lane.y0, r.lanes.x1, lane.y1);
            let stripe = if *kind == TrackKind::Video {
                c.lift(0.5)
            } else {
                tinted(c.lift(0.5), c.semantic.success, 0.08)
            };
            ui::surface::card(scene, tf, stripe_rect, 0.0, stripe);
            // The head: a raised card a step clear of its lane stripe, with
            // its name, pills, kind line and meter on the measured bands of
            // `chrome_layout::track_head` — the same bands the hit-test reads.
            let header = VideoEditorApp::track_head_rect(r, lane);
            let head = self.track_head_layout(header);
            ui::surface::card(
                scene,
                tf,
                header,
                f64::from(theme.tokens.radii.sm),
                c.surfaces.raised,
            );
            let track = self.project.track(*tid);
            let name = track.map_or("?", |t| t.name.as_str());
            // Elided to its band, which ends a step short of the pills: a
            // lane a collaborator named "Voiceover" must not run under them.
            let name = elide_at(name, head.name.width() as f32, 13.0, tr);
            let name_y = text_baseline(head.name, 13.0, tr);
            text_w(
                scene,
                tr,
                tf,
                &name,
                &TextStyle::new(13.0).with_weight(TextWeight::Semibold),
                c.text_primary,
                Point::new(head.name.x0, name_y),
            );
            // What the lane is, and — when it has been moved off unity or
            // centre — where its fader is. The reading belongs *here*, on the
            // lane, rather than only in a panel: a lane at +6 dB that says so
            // nowhere is how a mix ends up loud for a reason nobody can find.
            let knd = if *kind == TrackKind::Video {
                "VIDEO"
            } else {
                "AUDIO"
            };
            let mut line = knd.to_string();
            if let Some(t) = track.filter(|t| t.mix_is_set()) {
                if t.gain_db.abs() > 1e-9 {
                    line.push_str(" · ");
                    line.push_str(&t.gain_words());
                }
                if t.pan.abs() > 1e-9 {
                    line.push_str(" · ");
                    line.push_str(&t.pan_words());
                }
            }
            let line = elide_at(&line, head.kind.width() as f32, 9.0, tr);
            let kind_y = text_baseline(head.kind, 9.0, tr);
            text(scene, tr, tf, &line, 9.0, c.text_dim, head.kind.x0, kind_y);
            // The lane's level. Fed from the mix that made the sound, so a
            // bar moving here is the same measurement the master meter and
            // the clip warning are reading — never a second opinion.
            //
            // Drawn only on a lane that can carry sound. A picture lane whose
            // footage is silent has no level and never will, and a bar that
            // can never move does not read as "quiet" — it reads as an output
            // that is broken, which is the same lie the master strip already
            // refuses to tell on a timeline with no audio in it.
            if crate::audio::track_carries_sound(&self.project, *tid) {
                let level = self.mix.track(*tid);
                draw_level_meter(
                    scene,
                    tf,
                    theme,
                    head.meter,
                    level.map(|l| l.level).unwrap_or_default(),
                    level.map_or(0.0, |l| l.hold),
                    level.is_some_and(|l| l.clipped),
                );
            }
            // mute + lock pills (clickable toggles)
            let muted = track.is_some_and(|t| t.muted);
            let locked = track.is_some_and(|t| t.locked);
            let solo = track.is_some_and(|t| t.solo);
            let pill_radius = f64::from(theme.tokens.radii.sm);
            for (pill, on, label, on_bg) in [
                (head.mute, muted, "M", c.semantic.danger),
                (head.lock, locked, "L", c.semantic.warning),
                (head.solo, solo, "S", c.semantic.info),
            ] {
                let hovered = pill.contains(self.last_mouse);
                if on {
                    // Lit: the state's own semantic colour — a plain step
                    // against the head — with ink resolved to read on it.
                    let bg = if hovered { lighten(on_bg, 0.12) } else { on_bg };
                    let (bg, fg) = on_fill(theme, bg);
                    fill_round(scene, tf, pill, pill_radius, bg);
                    let at = text_centre(pill, label, 10.0, TextWeight::Semibold, tr);
                    text_w(
                        scene,
                        tr,
                        tf,
                        label,
                        &TextStyle::new(10.0).with_weight(TextWeight::Semibold),
                        fg,
                        Point::new(at.x, at.y),
                    );
                } else {
                    // Off: the shared neutral face, so the toggle carries the
                    // fleet's rest edge and hover rather than a bare ramp
                    // fill.
                    let face = ui::widget::ButtonFace::new(pill, label)
                        .radius(pill_radius)
                        .text_size(10.0)
                        .weight(TextWeight::Regular)
                        .hovered(hovered);
                    ui::widget::Button::paint(scene, tf, tr, theme, &face);
                }
            }
        }

        // Clip bodies clipped to the lane area so they don't bleed over the gutter/ruler.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            tf,
            &Rect::new(r.lanes.x0, r.lanes.y0, r.lanes.x1, r.timeline.y1),
        );
        let links = LinkMarks::build(&self.project, &self.selection);
        let mut drawn: Vec<(ClipId, Rect)> = Vec::new();
        let mut tied: Vec<(Uuid, Rect)> = Vec::new();
        for (tid, kind, lane) in &lanes {
            for clip in self.project.clips_on(*tid) {
                self.draw_clip(
                    &mut PaintCtx::new(scene, tf, tr, theme),
                    clip,
                    lane,
                    *kind,
                    r,
                    &links,
                );
                let rect = self.clip_rect(clip, lane, r);
                drawn.push((clip.id, rect));
                if let Some(link) = clip.link.filter(|_| links.group_selected(clip)) {
                    tied.push((link, rect));
                }
            }
        }
        // The tie between the halves of a selected shot: a hairline down the
        // gap between their lanes, capped at each end. Drawn only while the
        // group is selected — including all the way through a drag, which is
        // what makes "dragging one drags both" legible while it happens rather
        // than only after it lands.
        tied.sort_by(|a, b| {
            a.0.cmp(&b.0).then_with(|| {
                a.1.y0
                    .partial_cmp(&b.1.y0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });
        for pair in tied.windows(2) {
            let (link_a, above) = pair[0];
            let (link_b, below) = pair[1];
            if link_a != link_b {
                continue;
            }
            let x = above.x0.max(below.x0).clamp(r.lanes.x0, r.lanes.x1) + 1.0;
            if x <= r.lanes.x0 || x >= r.lanes.x1 {
                continue;
            }
            stroke_line(
                scene,
                tf,
                with_alpha(c.border_focus, 0.85),
                x,
                above.y1,
                x,
                below.y0,
                1.5,
            );
            for y in [above.y1, below.y0] {
                fill_round(
                    scene,
                    tf,
                    Rect::new(x - 2.5, y - 1.25, x + 2.5, y + 1.25),
                    1.25,
                    with_alpha(c.border_focus, 0.9),
                );
            }
        }
        // A lane that will not be heard or seen — muted, or not soloed while
        // something else of its kind is — greys back, so the pill state is
        // legible from the clips themselves and not only from the header.
        for (tid, _kind, lane) in &lanes {
            if self.project.track_audible(*tid) {
                continue;
            }
            let full = Rect::new(r.lanes.x0, lane.y0, r.lanes.x1, lane.y1);
            fill_rect(scene, tf, full, with_alpha(c.surfaces.base, 0.45));
        }
        // Everything outside the render range greys back, over the clips it
        // covers: the range is what will be written to a file, and a clip
        // that will not be in it must not look like one that will.
        if self.project.has_marks() {
            let (from, to) = self.project.export_range();
            let ax = self.time_to_x(from, &r.lanes).clamp(r.lanes.x0, r.lanes.x1);
            let bx = self.time_to_x(to, &r.lanes).clamp(r.lanes.x0, r.lanes.x1);
            for out in [
                Rect::new(r.lanes.x0, r.lanes.y0, ax, r.timeline.y1),
                Rect::new(bx, r.lanes.y0, r.lanes.x1, r.timeline.y1),
            ] {
                if out.width() > 0.5 {
                    fill_rect(scene, tf, out, with_alpha(c.surfaces.base, 0.55));
                }
            }
        }
        // A locked lane says so across its whole width, drawn *over* its clips:
        // they grey back under a diagonal hatch, so "my drag did nothing"
        // always has a visible reason rather than reading as a dead app.
        for (tid, _kind, lane) in &lanes {
            if !self.project.track(*tid).is_some_and(|t| t.locked) {
                continue;
            }
            let full = Rect::new(r.lanes.x0, lane.y0, r.lanes.x1, lane.y1);
            fill_rect(scene, tf, full, with_alpha(c.surfaces.base, 0.5));
            let h = full.height();
            let first = full.x0 - h;
            let stripes = (0u32..)
                .map(|k| first + f64::from(k) * 12.0)
                .take_while(|&x| x < full.x1);
            for x in stripes {
                stroke_line(
                    scene,
                    tf,
                    with_alpha(c.text_dim, 0.16),
                    x,
                    full.y1,
                    x + h,
                    full.y0,
                    1.0,
                );
            }
        }
        // Which clips other participants are holding, outlined in their
        // colours. Over the mute/lock scrims, because a peer's selection has
        // to stay legible on a lane that is greyed back -- knowing someone
        // else is working on a locked clip is exactly when it matters.
        crate::presence::draw_peer_clips(
            &mut crate::presence::PeerPaintCtx::new(scene, tf, tr, theme, presence),
            &drawn,
            Rect::new(r.lanes.x0, r.lanes.y0, r.lanes.x1, r.timeline.y1),
        );

        // Live rubber band, over the clips it is sweeping.
        if let Some(band) = self.marquee {
            fill_rect(scene, tf, band, with_alpha(c.accent[5], 0.24));
            stroke_round(scene, tf, band, 2.0, c.border_focus, 1.5);
        }
        scene.pop_layer();

        // Zoom readout, doubling as fit-to-window. Without it there was no way
        // to tell how far in you were, and no way back out but wheeling.
        //
        // Under the playhead, not over it: the chip sits at the ruler's right
        // edge, exactly where the playhead's time tag ends up on a narrow
        // window or at a large text scale, and the tag is the reading the
        // ruler exists to give. The chip keeps its hit rect and its keyboard
        // route (Z, zoom to fit) while the tag is over it.
        let chip = self.zoom_chip_rect(r);
        let zl = if self.zoom >= 100.0 {
            format!("{:.0} px/s", self.zoom)
        } else {
            format!("{:.1} px/s", self.zoom)
        };
        // A button (it zooms to fit), so it wears the shared face rather than
        // a ramp fill under a declared border.
        let face = ui::widget::ButtonFace::new(chip, &zl)
            .radius(f64::from(theme.tokens.radii.sm))
            .text_size(10.0)
            .weight(TextWeight::Regular)
            .hovered(chip.contains(self.last_mouse));
        ui::widget::Button::paint(scene, tf, tr, theme, &face);

        // --- playhead ---
        let px = self.time_to_x(self.playhead, &r.lanes);
        if px >= r.lanes.x0 - 0.5 && px <= r.lanes.x1 + 0.5 {
            stroke_line(
                scene,
                tf,
                c.accent[5],
                px,
                r.ruler.y0 + 4.0,
                px,
                r.timeline.y1,
                1.5,
            );
            let head = triangle_down(px, r.ruler.y0 + 4.0, 10.0);
            scene.fill(Fill::NonZero, tf, c.accent[5], None, &head);
        }
        // playhead timecode tag in ruler
        let tag = timecode(self.playhead, self.project.fps);
        let (tw, _) = tr.measure_weighted(&tag, 10.0, None, TextWeight::Bold);
        // The tag stops short of the zoom chip rather than running under it,
        // so the chip's hit rect is never covered by a reading it does not
        // answer for; the playhead's own head still reaches the ruler's end.
        let tagx = (px + 6.0)
            .min(self.zoom_chip_rect(r).x0 - f64::from(tw) - 8.0)
            .max(r.ruler.x0);
        let tagrect = Rect::new(
            tagx - 3.0,
            r.ruler.y0 + 2.0,
            tagx + f64::from(tw) + 5.0,
            r.ruler.y0 + 16.0,
        );
        fill_round(scene, tf, tagrect, 3.0, c.accent[5]);
        text_w(
            scene,
            tr,
            tf,
            &tag,
            &TextStyle::new(10.0).with_weight(TextWeight::Bold),
            on_fill(theme, c.accent[5]).1,
            Point::new(tagx, r.ruler.y0 + 3.0),
        );

        // Everyone else's playhead, after the local one so a peer parked on
        // the same frame reads as an addition to it rather than as something
        // that replaced it. Drawn through this editor's own seconds-to-pixels
        // mapping, so a peer's line lands on the pixel the local one would at
        // the same instant under the same zoom and scroll.
        crate::presence::draw_peer_playheads(
            &mut crate::presence::PeerPaintCtx::new(scene, tf, tr, theme, presence),
            r.lanes,
            r.timeline.y1,
            |t| self.time_to_x(t, &r.lanes),
        );

        // Empty timeline: say what to do, rather than showing bare lanes.
        // Through the same shared empty state the preview and the inspector
        // use, so the three empty surfaces of one window are one design.
        if self.project.clips.is_empty() {
            let area = Rect::new(r.lanes.x0, r.lanes.y0, r.lanes.x1, r.timeline.y1);
            let (title, hint) = if self.project.media.is_empty() {
                ("Nothing on the timeline", "Import media, then drag it here")
            } else {
                (
                    "Nothing on the timeline",
                    "Drag a clip from the media bin onto a track",
                )
            };
            ui::widget::EmptyState::paint(
                &mut ui::paint::PaintCtx::new(scene, tf, tr, theme),
                area,
                &ui::widget::EmptyStateContent {
                    icon: Some(ui::icons::Icon::Layers),
                    title,
                    message: Some(hint),
                    action_label: None,
                },
                Point::new(f64::MIN, f64::MIN),
            );
        }

        // Add-track buttons in the gutter footer.
        for (kind, rect) in self.add_track_buttons(r) {
            // The footer reserves two equally-sized real targets.  At large
            // type the full words exceed those targets and were drawn into
            // one another; V/A remain familiar track vocabulary while the
            // accessible controls continue to say the full action.
            let label = if self.text_scale > 1.25 {
                if kind == TrackKind::Video {
                    "+ V"
                } else {
                    "+ A"
                }
            } else if kind == TrackKind::Video {
                "+ Video"
            } else {
                "+ Audio"
            };
            let face = ui::widget::ButtonFace::new(rect, label)
                .radius(f64::from(theme.tokens.radii.sm))
                .text_size(11.0)
                .weight(TextWeight::Regular)
                .hovered(rect.contains(self.last_mouse));
            ui::widget::Button::paint(scene, tf, tr, theme, &face);
        }

        // Horizontal scrollbar, floated over the bottom of the lane area.
        self.draw_scrollbar(scene, tf, theme, ScrollPane::Timeline, r);
    }

    /// Paint one pane's overlay scrollbar. Geometry and thumb come from the
    /// toolkit's shared scrollbar model and the colours from the same theme
    /// tokens `DocumentViewport` reads, so all three bars here are the fleet's
    /// overlay scrollbar rather than three lookalikes.
    pub(super) fn draw_scrollbar(
        &self,
        scene: &mut Scene,
        tf: Affine,
        theme: &Theme,
        pane: ScrollPane,
        r: &Regions,
    ) {
        let c = &theme.tokens.colors;
        let Some((track, m)) = self.scroll_pane(pane, r) else {
            return;
        };
        let bar = self.scroll_bar(pane);
        let Some(thumb) = bar.thumb_rect(track, m) else {
            return;
        };
        let radius = SCROLLBAR_THICKNESS / 2.0;
        fill_round(scene, tf, track, radius, with_alpha(c.surfaces.base, 0.55));
        let col = if bar.is_active() {
            with_alpha(c.text_dim, 0.85)
        } else {
            with_alpha(c.text_dim, 0.55)
        };
        fill_round(scene, tf, thumb, radius, col);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "each clip's linked, selected, media, and accessibility treatments must share the exact same lane geometry"
    )]
    #[expect(
        clippy::many_single_char_names,
        reason = "the paint pass's shorthands: colours c, the clip rect r, and the x/y of each mark"
    )]
    pub(super) fn draw_clip(
        &self,
        ctx: &mut PaintCtx<'_>,
        clip: &Clip,
        lane: &Rect,
        kind: TrackKind,
        r: &Regions,
        links: &LinkMarks,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let cr = self.clip_rect(clip, lane, r);
        if cr.x1 < r.lanes.x0 || cr.x0 > r.lanes.x1 {
            return;
        }
        let base = clip_color(theme, clip.palette);
        let selected = self.selection.contains(clip.id);
        let hovered = self.hover_clip == Some(clip.id);
        let body = if kind == TrackKind::Video {
            base
        } else {
            darken(base, 0.05)
        };
        let body = if hovered { lighten(body, 0.08) } else { body };
        fill_round(
            ctx.scene,
            ctx.transform,
            cr,
            6.0,
            with_alpha(
                body,
                if kind == TrackKind::Video {
                    clip.param_at(Param::Opacity, self.playhead) * 0.85 + 0.15
                } else {
                    0.92
                },
            ),
        );
        // header strip
        let chrome_scale = self.chrome_scale();
        let strip = self.clip_header_rect(cr);
        fill_round(ctx.scene, ctx.transform, strip, 6.0, darken(base, 0.3));

        // content: filmstrip or waveform
        let inner = Rect::new(cr.x0 + 2.0, strip.y1, cr.x1 - 2.0, cr.y1 - 2.0);
        if inner.height() > 4.0 && inner.width() > 4.0 {
            if kind == TrackKind::Video {
                // A filmstrip: one frame **per cell**, walking the clip's
                // trimmed source window, so the strip shows the shot changing.
                // Tiling one thumbnail across every cell is not a filmstrip;
                // it is one picture repeated. The frames themselves are
                // requested in `request_filmstrips` (which needs `&mut self`).
                let (cell_w, n) = filmstrip_cells(inner);
                let span = clip.out_point - clip.in_point;
                for i in 0..n {
                    let bx = inner.x0 + 4.0 + i as f64 * (cell_w + 2.0);
                    if bx + cell_w > inner.x1 {
                        break;
                    }
                    let cell = Rect::new(bx, inner.y0 + 3.0, bx + cell_w, inner.y1 - 3.0);
                    let t = clip.in_point + ((i as f64 + 0.5) / n as f64) * span;
                    // `frame_at` already degrades exact → nearby-decoded →
                    // the generated representative, so a cell only comes back
                    // empty when the source has no picture at all.
                    match self.thumbnails.frame_at(clip.media, t) {
                        Some(th) => blit_thumb(ctx.scene, ctx.transform, th, cell, 2.0),
                        None => {
                            fill_round(ctx.scene, ctx.transform, cell, 2.0, lighten(base, 0.16));
                        }
                    }
                }
                // A source that could not be decoded says so once, over the
                // strip, rather than leaving a row of blanks to be read as
                // "this clip has no picture".
                if self.thumbnails.is_undecodable(clip.media) && cr.width() > 90.0 {
                    let msg = "No preview";
                    let (mw, _) = ctx.text.measure(msg, 10.0, None);
                    text(
                        ctx.scene,
                        ctx.text,
                        ctx.transform,
                        msg,
                        10.0,
                        with_alpha(Color::WHITE, 0.7),
                        inner.center().x - f64::from(mw) / 2.0,
                        inner.center().y - 6.0,
                    );
                }
            } else {
                let color = lighten(base, 0.3);
                let media_dur = self.project.media(clip.media).map_or(0.0, |m| m.duration);
                // Only real peaks are drawn as a waveform. Every other state —
                // still computing, never asked, or unreadable — draws the flat
                // line, because none of them know what the audio looks like.
                match (
                    self.waveforms.state(clip.media),
                    self.waveforms.peaks(clip.media),
                ) {
                    (crate::waveform::WaveformState::Ready, Some(peaks)) => draw_waveform(
                        ctx.scene,
                        ctx.transform,
                        WaveformPaint {
                            area: inner,
                            peaks,
                            source_in: clip.in_point,
                            source_out: clip.out_point,
                            media_duration: media_dur,
                            // The animated level, for the same reason as the clip
                            // body: a keyframed ramp is heard rising and was drawn
                            // flat.
                            volume: clip.param_at(Param::Volume, self.playhead),
                            color,
                        },
                    ),
                    _ => draw_flat_signal(ctx.scene, ctx.transform, inner, color),
                }
            }
        }
        // A source the clip cannot draw from: hatch the body so it cannot be
        // mistaken for one whose footage is here, and name which kind of
        // problem it is when the clip is wide enough to hold the words.
        let unusable = if self.media_is_unreadable(clip.media) {
            Some(MEDIA_UNREADABLE)
        } else if self.media_is_projected(clip.media) {
            Some(MEDIA_NOT_LOCAL)
        } else {
            None
        };
        if let Some(msg) = unusable {
            hatch_rect(
                ctx.scene,
                ctx.transform,
                with_alpha(c.semantic.warning, 0.30),
                cr,
                7.0,
            );
            if cr.width() > 118.0 && inner.height() > 14.0 {
                let (mw, mh) = ctx.text.measure(msg, 10.0, None);
                let pill = Rect::new(
                    inner.center().x - f64::from(mw) / 2.0 - 6.0,
                    inner.center().y - f64::from(mh) / 2.0 - 2.0,
                    inner.center().x + f64::from(mw) / 2.0 + 6.0,
                    inner.center().y + f64::from(mh) / 2.0 + 2.0,
                );
                fill_round(
                    ctx.scene,
                    ctx.transform,
                    pill,
                    4.0,
                    with_alpha(Color::BLACK, 0.55),
                );
                text(
                    ctx.scene,
                    ctx.text,
                    ctx.transform,
                    msg,
                    10.0,
                    with_alpha(Color::WHITE, 0.95),
                    pill.x0 + 6.0,
                    pill.y0 + 2.0,
                );
            }
        }
        // The part of the clip the lane actually shows. The lane layer is
        // clipped, so a clip whose start is scrolled off to the left had its
        // name, its speed badge and its head marker all drawn outside the
        // visible strip — and a clip wider than the viewport rendered as an
        // anonymous coloured bar. Everything below anchors to this, not to
        // the clip's own edges.
        let vx0 = cr.x0.max(r.lanes.x0);
        let vx1 = cr.x1.min(r.lanes.x1);
        let seen = (vx1 - vx0).max(0.0);

        // name label
        let label = self
            .project
            .media(clip.media)
            .map(|m| m.name.clone())
            .unwrap_or_default();
        // A re-timed clip carries a rate badge in the header's right end, so
        // the name has to elide before it rather than running underneath it.
        let badge_w = if clip.speed.is_normal() {
            0.0
        } else {
            f64::from(
                ctx.text
                    .measure_weighted(&clip.speed.label(), 9.5, None, TextWeight::Bold)
                    .0,
            ) + 18.0
        };
        // A linked clip wears a chain at the head of its header, so "these two
        // move together" is legible standing still — before anything is
        // dragged and without anything being selected. The name starts after
        // it, and after the transition triangle when the head also carries
        // one, rather than running underneath either.
        let head_col = on_fill(theme, darken(base, 0.3)).1;
        let linked = links.linked(clip);
        let corner = if clip.transition == Transition::None {
            0.0
        } else {
            9.0
        };
        let chain_w = if linked { CHAIN_W } else { 0.0 };
        if linked && seen > corner + CHAIN_W + 6.0 {
            draw_chain(
                ctx.scene,
                ctx.transform,
                with_alpha(head_col, 0.85),
                vx0 + 4.0 + corner,
                strip.center().y,
            );
        }
        // The width available is the width that can be *seen*, measured from
        // where the name is actually drawn.
        let avail = (seen - 10.0 - corner - chain_w - badge_w).max(4.0) as f32;
        if seen > 30.0 + corner + chain_w {
            let shown = elide_w(&label, avail, ctx.text);
            // The name sits on the clip's header strip, whose colour comes
            // from the user's chosen label — so the text colour has to be
            // resolved against it rather than assumed white.
            text_w(
                ctx.scene,
                ctx.text,
                ctx.transform,
                &shown,
                &TextStyle::new(11.0).with_weight(TextWeight::Medium),
                head_col,
                Point::new(vx0 + 5.0 + corner + chain_w, cr.y0 + 3.0 * chrome_scale),
            );
        }
        // transition marker at head
        if clip.transition != Transition::None && seen > 16.0 {
            let mk = triangle_corner(vx0, cr.y0, 12.0);
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                with_alpha(on_fill(theme, base).1, 0.85),
                None,
                &mk,
            );
        }
        // A re-timed clip says its rate on its own body. Speed changes the
        // clip's *length*, so without a badge a 2x clip is indistinguishable
        // from one that was simply trimmed shorter.
        if !clip.speed.is_normal() && seen > 52.0 {
            let label = clip.speed.label();
            let (lw, _) = ctx
                .text
                .measure_weighted(&label, 9.5, None, TextWeight::Bold);
            let pill = Rect::new(
                vx1 - 8.0 - f64::from(lw) - 10.0,
                cr.y0 + 2.0 * chrome_scale,
                vx1 - 4.0,
                cr.y0 + 16.0 * chrome_scale,
            );
            if pill.x0 > vx0 + 4.0 {
                let bg = darken(base, 0.45);
                fill_round(ctx.scene, ctx.transform, pill, 4.0, with_alpha(bg, 0.9));
                let baseline = text_baseline(pill, 9.5, ctx.text);
                text_w(
                    ctx.scene,
                    ctx.text,
                    ctx.transform,
                    &label,
                    &TextStyle::new(9.5).with_weight(TextWeight::Bold),
                    on_fill(theme, bg).1,
                    Point::new(pill.x0 + 5.0, baseline),
                );
            }
        }
        // Keyframes, as diamonds on a lane along the foot of the clip. This is
        // where animation becomes visible without opening the inspector — and
        // clicking one takes the playhead there, which makes stepping through
        // a curve direct rather than a menu round-trip.
        //
        // The foot, not the header: the header carries the clip's name, and
        // marks laid over it turned "Drone_Coastline.mov" into
        // "Drone_Coastli◆e.mov".
        let key_times = clip.key_times();
        if cr.width() > 12.0 && !key_times.is_empty() && cr.height() > 24.0 {
            let mark = on_fill(theme, base).1;
            let y = cr.y1 - 7.0;
            stroke_line(
                ctx.scene,
                ctx.transform,
                with_alpha(darken(base, 0.45), 0.75),
                (cr.x0 + 3.0).max(r.lanes.x0),
                y,
                (cr.x1 - 3.0).min(r.lanes.x1),
                y,
                1.0,
            );
            for t in key_times {
                let x = self.time_to_x(t, &r.lanes);
                if x < cr.x0 - 1.0 || x > cr.x1 + 1.0 || x < r.lanes.x0 || x > r.lanes.x1 {
                    continue;
                }
                let s = 3.5;
                let mut d = BezPath::new();
                d.move_to((x, y - s));
                d.line_to((x + s, y));
                d.line_to((x, y + s));
                d.line_to((x - s, y));
                d.close_path();
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    with_alpha(mark, 0.95),
                    None,
                    &d,
                );
                ctx.scene.stroke(
                    &Stroke::new(1.0),
                    ctx.transform,
                    with_alpha(darken(base, 0.5), 0.9),
                    None,
                    &d,
                );
            }
        }
        // Selection. The primary (anchor) clip — the one the inspector edits —
        // keeps the full white outline and trim handles; the rest of a
        // multi-selection gets a lighter outline and no handles, so it is
        // always clear which clip a trim or a grade will land on.
        let primary = self.selection.primary() == Some(clip.id);
        if primary {
            stroke_round(ctx.scene, ctx.transform, cr, 6.0, c.border_focus, 2.0);
            for hx in [cr.x0, cr.x1] {
                let hrect = Rect::new(hx - 3.0, cr.y0 + 2.0, hx + 3.0, cr.y1 - 2.0);
                fill_round(ctx.scene, ctx.transform, hrect, 2.0, c.accent[5]);
            }
        } else if selected {
            stroke_round(
                ctx.scene,
                ctx.transform,
                cr,
                6.0,
                with_alpha(c.border_focus, 0.6),
                2.0,
            );
        } else if links.group_selected(clip) {
            // The other half of a selected shot. Dashed rather than solid, so
            // "this will move too" never reads as "this is selected" — an edit
            // aimed at it (a grade, a level) still lands on the anchor.
            ctx.scene.stroke(
                &Stroke::new(1.6).with_dashes(0.0, [4.0, 3.0]),
                ctx.transform,
                with_alpha(c.border_focus, 0.75),
                None,
                &RoundedRect::from_rect(cr, 6.0),
            );
        } else {
            // The rest edge, derived against the clip's own body so it shows
            // on a pale label in light as on a deep one in dark; the old
            // half-alpha base wash was a light ring on white.
            stroke_round(
                ctx.scene,
                ctx.transform,
                cr.inset(-0.5),
                5.5,
                ui::surface::edge_on(body),
                1.0,
            );
        }
    }
}
