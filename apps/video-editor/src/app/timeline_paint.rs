#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;

/// The labels along the ruler, on the clips and in the track heads: captions,
/// in tabular figures wherever they carry a time or a level.
fn caption(theme: &Theme) -> TextStyle {
    theme.text(TextRole::Caption)
}

impl VideoEditorApp {
    /// The master meter and the standing clipping warning.
    ///
    /// Two states and they read completely differently, which is the point.
    /// Resting, it is a stereo bar and the loudest the mix has been, in
    /// decibels — the reading a person mixing wants anyway. Once the mix has
    /// gone over, the right-hand flag turns into a red "Clip" stating how far
    /// over, and it stays that way: it survives the transport stopping, the
    /// playhead moving and the panel being folded, and it is cleared only by
    /// changing the levels it is a report about.
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
        // The readout line and the meter under it, centred as one block a
        // step apart (`chrome_layout::master_strip`).
        let m = chrome_layout::master_strip(strip);
        let label = caption(theme);
        // Nothing to mix, nothing to meter. A dead meter on a picture-only
        // timeline would read as an output that is broken.
        if !crate::audio::has_audio(&self.project) {
            let y = text_baseline(strip, &label, tr);
            text(
                scene,
                tr,
                tf,
                "No audio",
                &label,
                c.text_dim,
                m.readout.x0,
                y,
            );
            return;
        }
        let readout_y = text_baseline(m.readout, &label, tr);
        text(
            scene,
            tr,
            tf,
            "Mix",
            &label,
            c.text_dim,
            m.readout.x0,
            readout_y,
        );
        let flag = self.clip_flag_rect(r);
        let numeric = label.clone().numeric();
        if self.mix.clipped() {
            // A status, so a status badge: danger, in the toolkit's own pill.
            let words = self.clip_flag_words();
            ui::widget::BadgeFace::new(flag, &words)
                .style(ui::widget::ChipStyle::Tinted(ui::widget::Tone::Danger))
                .paint(scene, tf, tr, theme);
        } else {
            // The peak-hold, or an honest blank. "-∞" would say the mix is
            // silent; nothing has been measured, which is a different claim.
            let words = self
                .mix
                .hold_db()
                .map_or_else(|| "—".to_string(), |db| format!("{db:.1} dB"));
            let w = text_width(tr, &words, &numeric);
            text(
                scene,
                tr,
                tf,
                &words,
                &numeric,
                c.text_dim,
                flag.x1 - w,
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
    #[expect(
        clippy::many_single_char_names,
        reason = "the paint pass's shorthands: colours c, regions r, and the t/x/h of each tick and mark"
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
        let ground = c.surfaces.content;
        // The timeline stands on the window's own ground; one hairline says
        // where the panes above it end, and one more under the ruler band.
        ui::surface::separator(
            scene,
            tf,
            r.timeline.x0,
            r.timeline.x1,
            r.timeline.y0 + 0.5,
            0.0,
            ground,
        );
        ui::surface::separator(
            scene,
            tf,
            r.timeline.x0,
            r.timeline.x1,
            r.ruler.y1,
            0.0,
            ground,
        );

        // --- master strip (the corner above the track heads) ---
        self.draw_master_strip(scene, tf, tr, theme, r);

        // --- ruler ticks ---
        let label = caption(theme).numeric();
        let label_h = line_height(tr, &label);
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
        let tick_ink = ui::surface::edge_on(ground);
        let grid_ink = ui::surface::edge_on_with(ground, ui::chrome::HAIRLINE_ALPHA / 2);
        let label_top = r.ruler.y0 + (r.ruler.height() - label_h) / 2.0;
        let chip_x0 = self.zoom_chip_rect(r).x0;
        // The ruler's one row of readings, in order of who owns it: the
        // playhead's time tag, then everyone else's playhead names, then the
        // named markers, then the tick times. A reading that would run into
        // one before it is left out rather than printed through it.
        let (tag, tag_rect) = self.playhead_tag(r, tr, theme);
        let mut claimed: Vec<(f64, f64)> = vec![(tag_rect.x0, tag_rect.x1)];
        let peer_name = caption(theme);
        let peer_tags = crate::presence::place_peer_tags(
            presence,
            r.lanes,
            &claimed,
            |t| self.time_to_x(t, &r.lanes),
            // The chip's label plus its insets either side.
            |name| text_width(tr, name, &peer_name) + 2.0 * S2,
        );
        claimed.extend(peer_tags.iter().filter(|p| p.row == 0).map(|p| p.span));
        let name_style = caption(theme);
        let clear_of = |claimed: &[(f64, f64)], a: f64, b: f64| {
            !claimed.iter().any(|(m0, m1)| a < m1 + S2 && b > m0 - S2)
        };
        let mut marker_names: Vec<(f64, f64)> = Vec::new();
        let mut shown_names: Vec<(f64, &str)> = Vec::new();
        let markers = self.project.all_markers();
        for (at, m, on_clip) in &markers {
            if on_clip.is_some() || m.name.is_empty() {
                continue;
            }
            let x = self.time_to_x(*at, &r.lanes) + S2;
            let span = (x, x + text_width(tr, &m.name, &name_style));
            let all: Vec<(f64, f64)> = claimed.iter().chain(&marker_names).copied().collect();
            if clear_of(&all, span.0, span.1) && span.1 <= chip_x0 - S1 {
                marker_names.push(span);
                shown_names.push((*at, m.name.as_str()));
            }
        }
        claimed.extend(marker_names);
        for t in ticks {
            let x = self.time_to_x(t, &r.lanes);
            if x >= r.ruler.x0 - 1.0 && x <= r.ruler.x1 {
                stroke_line(scene, tf, tick_ink, x, r.ruler.y1 - S2, x, r.ruler.y1, 1.0);
                let words = timecode(t, self.project.fps);
                let label_w = text_width(tr, &words, &label);
                // The fit-to-window chip owns the ruler's right end.  Tick
                // labels are helpful context, not a reason to draw through a
                // control (especially when text scaling widens both).
                let (a, b) = (x + S1, x + S1 + label_w);
                if b <= chip_x0 - S1 && clear_of(&claimed, a, b) {
                    text(scene, tr, tf, &words, &label, c.text_dim, x + S1, label_top);
                }
                // faint grid line down the lanes
                stroke_line(scene, tf, grid_ink, x, r.lanes.y0, x, r.timeline.y1, 1.0);
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
            let mark = c.text_primary;
            if bx > ax {
                fill_rect(
                    scene,
                    tf,
                    Rect::new(ax, r.ruler.y0, bx, r.ruler.y0 + 3.0),
                    mark,
                );
            }
            // A bracket at each mark, opening towards the span it keeps.
            for (at, towards) in [(self.project.mark_in, 1.0), (self.project.mark_out, -1.0)] {
                let Some(t) = at else { continue };
                let x = self.time_to_x(t, &r.lanes);
                if x < r.lanes.x0 - S2 || x > r.lanes.x1 + S2 {
                    continue;
                }
                let far = x + towards * S2;
                fill_rect(
                    scene,
                    tf,
                    Rect::new(x.min(far), r.ruler.y0, x.max(far), r.ruler.y0 + S2),
                    mark,
                );
                // Only down the ruler: the veil below already draws the
                // boundary across the lanes.
                stroke_line(scene, tf, mark, x, r.ruler.y0, x, r.ruler.y1, 2.0);
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
            #[expect(
                clippy::float_cmp,
                reason = "the shown names were taken from this very marker list, so equality is the lookup"
            )]
            let shown = on_clip.is_none()
                && shown_names
                    .iter()
                    .any(|(t, n)| *t == at && *n == marker.name);
            if shown {
                text(
                    scene,
                    tr,
                    tf,
                    &marker.name,
                    &name_style,
                    c.text_primary,
                    x + S2,
                    label_top,
                );
            }
        }

        // --- lanes + headers + clips ---
        let lanes = self.lane_rects(r);
        // One lift of the ground for a lane and its head, so the two read as
        // one row of the timeline.
        let stripe = c.lift_on(ground, 0.5);
        let head_radius = f64::from(theme.tokens.radii.item);
        for (tid, kind, lane) in &lanes {
            let stripe_rect = Rect::new(r.lanes.x0, lane.y0, r.lanes.x1, lane.y1);
            fill_rect(scene, tf, stripe_rect, stripe);
            // The head: the same lift a step clear of its lane stripe, with
            // its name, toggles, kind line and meter on the measured bands of
            // `chrome_layout::track_head` — the same bands the hit-test reads.
            let header = VideoEditorApp::track_head_rect(r, lane);
            let head = self.track_head_layout(header);
            fill_round(scene, tf, header, head_radius, stripe);
            let track = self.project.track(*tid);
            let name = track.map_or("?", |t| t.name.as_str());
            // Elided to its band, which ends a step short of the toggles: a
            // lane a collaborator named "Voiceover" must not run under them.
            let name_style = theme.text(TextRole::Label).strong();
            let name = elide_at(name, head.name.width() as f32, &name_style, tr);
            let name_y = text_baseline(head.name, &name_style, tr);
            text(
                scene,
                tr,
                tf,
                &name,
                &name_style,
                c.text_primary,
                head.name.x0,
                name_y,
            );
            // What the lane is — or, once its fader has moved off unity or
            // centre, where the fader is. The reading belongs *here*, on the
            // lane, rather than only in a panel: a lane at +6 dB that says so
            // nowhere is how a mix ends up loud for a reason nobody can find.
            let line = match track.filter(|t| t.mix_is_set()) {
                Some(t) if t.gain_db.abs() > 1e-9 && t.pan.abs() > 1e-9 => {
                    format!("{}, {}", t.gain_words(), t.pan_words())
                }
                Some(t) if t.gain_db.abs() > 1e-9 => t.gain_words(),
                Some(t) => t.pan_words(),
                None if *kind == TrackKind::Video => "Video".to_string(),
                None => "Audio".to_string(),
            };
            let kind_style = caption(theme).numeric();
            let line = elide_at(&line, head.kind.width() as f32, &kind_style, tr);
            let kind_y = text_baseline(head.kind, &kind_style, tr);
            text(
                scene,
                tr,
                tf,
                &line,
                &kind_style,
                c.text_dim,
                head.kind.x0,
                kind_y,
            );
            // The lane's level. Fed from the mix that made the sound, so a
            // bar moving here is the same measurement the master meter and
            // the clip warning are reading — never a second opinion.
            //
            // Drawn only on a lane that can carry sound: a bar that can never
            // move reads as an output that is broken.
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
            // Mute, solo and lock: toggles that stay down while they are on.
            // Lettered M, S and L, the mixing desk's own shorthand, so the
            // three never read as one another; their full names are what a
            // reader hears and what the hover says. The head is too narrow
            // for a labelled face's insets, so the letter is set centred on
            // the toolkit's toggle face rather than elided to nothing.
            let muted = track.is_some_and(|t| t.muted);
            let locked = track.is_some_and(|t| t.locked);
            let solo = track.is_some_and(|t| t.solo);
            for (pill, on, letter) in [
                (head.mute, muted, "M"),
                (head.solo, solo, "S"),
                (head.lock, locked, "L"),
            ] {
                let hovered = pill.contains(self.last_mouse);
                draw_toggle_button(
                    &mut PaintCtx::new(scene, tf, tr, theme),
                    pill,
                    ToggleContent::Label(""),
                    on,
                    hovered,
                );
                let style = theme.text(TextRole::Label).strong();
                let at = text_centre(pill, letter, &style, tr);
                let ink = if on || hovered {
                    c.text_primary
                } else {
                    c.text_dim
                };
                text(scene, tr, tf, letter, &style, ink, at.x, at.y);
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
        // A locked lane says so across its whole width: a diagonal hatch on
        // the lane itself, under its clips, so "my drag did nothing" always
        // has a visible reason rather than reading as a dead app. The clips
        // on it grey back (see `draw_clip`) with their names kept legible.
        for (tid, _kind, lane) in &lanes {
            if self.project.track(*tid).is_some_and(|t| t.locked) {
                let full = Rect::new(r.lanes.x0, lane.y0, r.lanes.x1, lane.y1);
                hatch_rect(scene, tf, ui::surface::edge_on(ground), full, 0.0);
            }
        }
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
            stroke_line(scene, tf, c.intent.you, x, above.y1, x, below.y0, 1.5);
            for y in [above.y1, below.y0] {
                fill_round(
                    scene,
                    tf,
                    Rect::new(x - 2.5, y - 1.25, x + 2.5, y + 1.25),
                    1.25,
                    c.intent.you,
                );
            }
        }
        // What will not be written greys back toward the ground, over the
        // clips it covers.
        let veil = clip_veil(theme);
        // Everything outside the render range: the range is what will be
        // written to a file, and a clip that will not be in it must not look
        // like one that will.
        if self.project.has_marks() {
            let (from, to) = self.project.export_range();
            let ax = self.time_to_x(from, &r.lanes).clamp(r.lanes.x0, r.lanes.x1);
            let bx = self.time_to_x(to, &r.lanes).clamp(r.lanes.x0, r.lanes.x1);
            for out in [
                Rect::new(r.lanes.x0, r.lanes.y0, ax, r.timeline.y1),
                Rect::new(bx, r.lanes.y0, r.lanes.x1, r.timeline.y1),
            ] {
                if out.width() > 0.5 {
                    fill_rect(scene, tf, out, veil);
                }
            }
        }
        // Which clips other participants are holding, outlined in their
        // colours. Over the mute/lock veils, because a peer's selection has
        // to stay legible on a lane that is greyed back -- knowing someone
        // else is working on a locked clip is exactly when it matters.
        crate::presence::draw_peer_clips(
            &mut crate::presence::PeerPaintCtx::new(scene, tf, tr, theme, presence),
            &drawn,
            Rect::new(r.lanes.x0, r.lanes.y0, r.lanes.x1, r.timeline.y1),
        );

        // Live rubber band, over the clips it is sweeping: your colour, as a
        // text selection is.
        if let Some(band) = self.marquee {
            fill_rect(scene, tf, band, c.intent.selection_tint);
            stroke_round(scene, tf, band, 2.0, c.intent.you, 1.5);
        }
        scene.pop_layer();

        // Zoom readout, doubling as fit-to-window. Without it there was no way
        // to tell how far in you were, and no way back out but wheeling.
        //
        // Under the playhead, not over it: the chip sits at the ruler's right
        // edge, exactly where the playhead's time tag ends up on a narrow
        // window or at a large text scale, and the tag is the reading the
        // ruler exists to give.
        let chip = self.zoom_chip_rect(r);
        let zl = self.zoom_words();
        let face = ButtonFace::new(chip, &zl)
            .control_size(ControlSize::Small)
            .hovered(chip.contains(self.last_mouse));
        Button::paint(scene, tf, tr, theme, &face);

        // --- playhead ---
        // The editor's own cursor through time: primary ink, the strongest
        // mark the timeline draws that is not a person.
        let head_ink = c.text_primary;
        let px = self.time_to_x(self.playhead, &r.lanes);
        if px >= r.lanes.x0 - 0.5 && px <= r.lanes.x1 + 0.5 {
            stroke_line(
                scene,
                tf,
                head_ink,
                px,
                r.ruler.y0 + S1,
                px,
                r.timeline.y1,
                1.5,
            );
            let head = triangle_down(px, r.ruler.y0 + S1, 10.0);
            scene.fill(Fill::NonZero, tf, head_ink, None, &head);
        }
        // playhead timecode tag in ruler: an inverted chip, ink on its fill.
        fill_round(
            scene,
            tf,
            tag_rect,
            f64::from(theme.tokens.radii.detail),
            c.intent.inverted,
        );
        text(
            scene,
            tr,
            tf,
            &tag,
            &caption(theme).numeric().strong(),
            c.intent.on_inverted,
            tag_rect.x0 + S1,
            tag_rect.y0,
        );

        // Everyone else's playhead, after the local one so a peer parked on
        // the same frame reads as an addition to it rather than as something
        // that replaced it.
        crate::presence::draw_peer_playheads(
            &mut crate::presence::PeerPaintCtx::new(scene, tf, tr, theme, presence),
            &peer_tags,
            r.timeline.y1,
        );

        // Empty timeline: say what to do, rather than showing bare lanes.
        // Through the same shared empty state the preview and the inspector
        // use, so the three empty surfaces of one window are one design.
        if self.project.clips.is_empty() {
            let area = Rect::new(r.lanes.x0, r.lanes.y0, r.lanes.x1, r.timeline.y1);
            let message = if self.project.media.is_empty() {
                "Import media, then drag it here."
            } else {
                "Drag a clip from the media bin onto a track."
            };
            ui::widget::EmptyState::paint(
                &mut ui::paint::PaintCtx::new(scene, tf, tr, theme),
                area,
                &ui::widget::EmptyStateContent {
                    icon: Some(Icon::Layers),
                    message: Some(message),
                    ..ui::widget::EmptyStateContent::new("Nothing on the timeline")
                },
                Point::new(f64::MIN, f64::MIN),
            );
        }

        // Add-track buttons under the last lane.
        for (kind, rect) in self.add_track_buttons(r) {
            let face = ButtonFace::new(rect, add_track_label(kind))
                .icon(Icon::Plus)
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small)
                .hovered(rect.contains(self.last_mouse));
            Button::paint(scene, tf, tr, theme, &face);
        }

        // Horizontal scrollbar, floated over the bottom of the lane area.
        self.draw_scrollbar(scene, tf, theme, ScrollPane::Timeline, r);
    }

    /// The playhead's time tag in the ruler: its words and its chip.
    ///
    /// The tag follows the playhead but stops a sibling step short of the zoom
    /// chip rather than running under it, so the chip's hit rect is never
    /// covered by a reading it does not answer for; the playhead's own head
    /// still reaches the ruler's end.
    pub(super) fn playhead_tag(
        &self,
        r: &Regions,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> (String, Rect) {
        let tag = timecode(self.playhead, self.project.fps);
        let tag_style = caption(theme).numeric().strong();
        let tw = text_width(tr, &tag, &tag_style);
        let th = line_height(tr, &tag_style);
        let px = self.time_to_x(self.playhead, &r.lanes);
        let chip = self.zoom_chip_rect(r);
        let tagx = (px + S2).min(chip.x0 - tw - S1 - S2).max(r.ruler.x0 + S1);
        let tag_top = r.ruler.y0 + (r.ruler.height() - th) / 2.0;
        (
            tag,
            Rect::new(tagx - S1, tag_top, tagx + tw + S1, tag_top + th),
        )
    }

    /// What the zoom chip says: how many pixels a second of timeline takes.
    pub(super) fn zoom_words(&self) -> String {
        if self.zoom >= 100.0 {
            format!("{:.0} px/s", self.zoom)
        } else {
            format!("{:.1} px/s", self.zoom)
        }
    }

    /// Paint one pane's overlay scrollbar through the toolkit's own painter,
    /// so all three bars here are the system's scrollbar rather than three
    /// lookalikes.
    pub(super) fn draw_scrollbar(
        &self,
        scene: &mut Scene,
        tf: Affine,
        theme: &Theme,
        pane: ScrollPane,
        r: &Regions,
    ) {
        let Some((track, m)) = self.scroll_pane(pane, r) else {
            return;
        };
        let bar = self.scroll_bar(pane);
        bar.draw(scene, tf, track, m, theme, false);
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
        let radius = f64::from(theme.tokens.radii.item);
        let base = clip_color(theme, clip.palette);
        let selected = self.selection.contains(clip.id);
        let hovered = self.hover_clip == Some(clip.id);
        // The body is the person's label colour, as see-through as the clip
        // is: a clip faded to half opacity is drawn half there.
        let body_alpha = if kind == TrackKind::Video {
            clip.param_at(Param::Opacity, self.playhead) * 0.85 + 0.15
        } else {
            0.92
        };
        fill_round(
            ctx.scene,
            ctx.transform,
            cr,
            radius,
            with_alpha(base, body_alpha),
        );
        // The header strip: the label colour deepened until its name reads.
        let strip = self.clip_header_rect(cr);
        let (strip_fill, mut head_col) = on_fill(theme, base);
        fill_round(ctx.scene, ctx.transform, strip, radius, strip_fill);

        // content: filmstrip or waveform
        let inner = Rect::new(cr.x0 + 2.0, strip.y1, cr.x1 - 2.0, cr.y1 - 2.0);
        if inner.height() > 4.0 && inner.width() > 4.0 {
            if kind == TrackKind::Video {
                // A filmstrip: one frame **per cell**, walking the clip's
                // trimmed source window, so the strip shows the shot changing.
                // The frames themselves are requested in `request_filmstrips`
                // (which needs `&mut self`).
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
                            fill_round(ctx.scene, ctx.transform, cell, 2.0, c.media.letterbox);
                        }
                    }
                }
                // A source that could not be decoded says so once, over the
                // strip, rather than leaving a row of blanks to be read as
                // "this clip has no picture".
                if self.thumbnails.is_undecodable(clip.media) && cr.width() > 90.0 {
                    let msg = "No preview";
                    let style = caption(theme);
                    let at = text_centre(inner, msg, &style, ctx.text);
                    text(
                        ctx.scene,
                        ctx.text,
                        ctx.transform,
                        msg,
                        &style,
                        head_col,
                        at.x,
                        at.y,
                    );
                }
            } else {
                let color = head_col;
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
                radius,
            );
            if cr.width() > 118.0 && inner.height() > 14.0 {
                let w = ui::widget::BadgeFace::intrinsic_width(msg, ctx.text, theme);
                let h = ui::widget::BadgeFace::height(theme);
                let ctr = inner.center();
                let pill = Rect::new(
                    ctr.x - w / 2.0,
                    ctr.y - h / 2.0,
                    ctr.x + w / 2.0,
                    ctr.y + h / 2.0,
                );
                ui::widget::BadgeFace::new(pill, msg)
                    .style(ui::widget::ChipStyle::Tinted(ui::widget::Tone::Warning))
                    .paint(ctx.scene, ctx.transform, ctx.text, theme);
            }
        }
        // A clip on a lane that will not be heard or seen (muted, or not
        // soloed while something else of its kind is) or that is locked greys
        // back toward the ground, so the lane's state is legible from the
        // clips themselves and not only from the header. Its name and marks
        // are then lettered in the ground's own ink, over the veil, so they
        // stay as readable as anything else on the timeline.
        let dimmed = !self.project.track_audible(clip.track)
            || self.project.track(clip.track).is_some_and(|t| t.locked);
        if dimmed {
            fill_round(ctx.scene, ctx.transform, cr, radius, clip_veil(theme));
            head_col = c.text_primary;
        }
        // The part of the clip the lane actually shows. The lane layer is
        // clipped, so a clip whose start is scrolled off to the left had its
        // name, its speed badge and its head marker all drawn outside the
        // visible strip. Everything below anchors to this, not to the clip's
        // own edges.
        let vx0 = cr.x0.max(r.lanes.x0);
        let vx1 = cr.x1.min(r.lanes.x1);
        let seen = (vx1 - vx0).max(0.0);

        // name label
        let label = self
            .project
            .media(clip.media)
            .map(|m| m.name.clone())
            .unwrap_or_default();
        let name_style = caption(theme);
        let badge_style = caption(theme).numeric().strong();
        // A re-timed clip carries a rate badge in the header's right end, so
        // the name has to elide before it rather than running underneath it.
        let badge_w = if clip.speed.is_normal() {
            0.0
        } else {
            text_width(ctx.text, &clip.speed.label(), &badge_style) + S4
        };
        // A linked clip wears a chain at the head of its header, so "these two
        // move together" is legible standing still. The name starts after it,
        // and after the transition triangle when the head also carries one.
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
                head_col,
                vx0 + 4.0 + corner,
                strip.center().y,
            );
        }
        // The width available is the width that can be *seen*, measured from
        // where the name is actually drawn.
        let avail = (seen - 10.0 - corner - chain_w - badge_w).max(4.0) as f32;
        if seen > 30.0 + corner + chain_w {
            let shown = elide_at(&label, avail, &name_style, ctx.text);
            let name_top = text_baseline(strip, &name_style, ctx.text);
            text(
                ctx.scene,
                ctx.text,
                ctx.transform,
                &shown,
                &name_style,
                head_col,
                vx0 + 5.0 + corner + chain_w,
                name_top,
            );
        }
        // transition marker at head
        if clip.transition != Transition::None && seen > 16.0 {
            let mk = triangle_corner(vx0, cr.y0, 12.0);
            ctx.scene
                .fill(Fill::NonZero, ctx.transform, head_col, None, &mk);
        }
        // A re-timed clip says its rate on its own body. Speed changes the
        // clip's *length*, so without a badge a 2x clip is indistinguishable
        // from one that was simply trimmed shorter.
        if !clip.speed.is_normal() && seen > 52.0 {
            let words = clip.speed.label();
            let lw = text_width(ctx.text, &words, &badge_style);
            let pill = Rect::new(vx1 - S2 - lw - S2, strip.y0, vx1 - S1, strip.y1);
            if pill.x0 > vx0 + 4.0 {
                let top = text_baseline(pill, &badge_style, ctx.text);
                text(
                    ctx.scene,
                    ctx.text,
                    ctx.transform,
                    &words,
                    &badge_style,
                    head_col,
                    pill.x0 + S1,
                    top,
                );
            }
        }
        // Keyframes, as diamonds on a lane along the foot of the clip. This is
        // where animation becomes visible without opening the inspector — and
        // clicking one takes the playhead there, which makes stepping through
        // a curve direct rather than a menu round-trip.
        let key_times = clip.key_times();
        if cr.width() > 12.0 && !key_times.is_empty() && cr.height() > 24.0 {
            let y = cr.y1 - 7.0;
            stroke_line(
                ctx.scene,
                ctx.transform,
                strip_fill,
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
                ctx.scene
                    .fill(Fill::NonZero, ctx.transform, head_col, None, &d);
                ctx.scene
                    .stroke(&Stroke::new(1.0), ctx.transform, strip_fill, None, &d);
            }
        }
        // Selection, in your colour. The primary (anchor) clip — the one the
        // inspector edits — keeps the full outline and trim handles; the rest
        // of a multi-selection gets a lighter outline and no handles, so it is
        // always clear which clip a trim or a grade will land on.
        let you = c.intent.you;
        let primary = self.selection.primary() == Some(clip.id);
        // Clips abut, and your colour can be any clip's label colour too, so a
        // selected clip is parted from its neighbours by a line of the ground
        // just outside its outline: the ring never merges into the next clip.
        if selected {
            stroke_round(
                ctx.scene,
                ctx.transform,
                cr.inflate(1.0, 1.0),
                radius + 1.0,
                theme.tokens.colors.surfaces.content,
                2.0,
            );
        }
        if primary {
            stroke_round(ctx.scene, ctx.transform, cr, radius, you, 2.0);
            for hx in [cr.x0, cr.x1] {
                let hrect = Rect::new(hx - 3.0, cr.y0 + 2.0, hx + 3.0, cr.y1 - 2.0);
                fill_round(ctx.scene, ctx.transform, hrect, 2.0, you);
            }
        } else if selected {
            stroke_round(
                ctx.scene,
                ctx.transform,
                cr,
                radius,
                with_alpha(you, 0.6),
                2.0,
            );
        } else if links.group_selected(clip) {
            // The other half of a selected shot. Dashed rather than solid, so
            // "this will move too" never reads as "this is selected" — an edit
            // aimed at it (a grade, a level) still lands on the anchor.
            ctx.scene.stroke(
                &Stroke::new(1.6).with_dashes(0.0, [4.0, 3.0]),
                ctx.transform,
                you,
                None,
                &RoundedRect::from_rect(cr, radius),
            );
        } else {
            // The rest edge, derived against the clip's own body; firmer under
            // the pointer, which is the clip a press would pick up.
            let edge = if hovered {
                ui::surface::edge_on_with(base, ui::chrome::PANEL_BORDER_ALPHA)
            } else {
                ui::surface::edge_on(base)
            };
            stroke_round(
                ctx.scene,
                ctx.transform,
                cr.inset(-0.5),
                radius - 0.5,
                edge,
                1.0,
            );
        }
    }
}

/// What an add-track button says.
pub(super) fn add_track_label(kind: TrackKind) -> &'static str {
    if kind == TrackKind::Video {
        "Video track"
    } else {
        "Audio track"
    }
}

/// The ground pulled over what will not be heard, seen or written.
fn clip_veil(theme: &Theme) -> Color {
    with_alpha(
        theme.tokens.colors.surfaces.content,
        f64::from(ui::overlay::scrim::MODAL_STRENGTH) + 0.1,
    )
}
