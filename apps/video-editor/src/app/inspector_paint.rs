#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;

impl VideoEditorApp {
    /// Draw the tone-curve editor: the grid, the "unchanged" diagonal, the
    /// curve, and its control points.
    ///
    /// This is canvas drawing rather than a toolkit widget for the same
    /// reason the timeline and the waveform are: a 2D control-point editor
    /// is content, not a control the fleet shares. Everything around it —
    /// the channel chooser, the amount slider, the reset — goes through the
    /// shared widgets.
    #[expect(
        clippy::many_single_char_names,
        reason = "curve plotting: colours c, sample fraction f and value v, and the x/y/h of each mark"
    )]
    fn draw_curve_editor(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        canvas: Rect,
        clip: &Clip,
    ) {
        const STEPS: usize = 64;
        let c = &theme.tokens.colors;
        let ch = self.curve_channel;
        let spline = clip.curves.spline(ch);
        well(scene, tf, theme, canvas, 8.0);

        // Quarter grid. Faint, because it is a reading aid and the curve is
        // the content.
        let grid = with_alpha(c.text_dim, 0.22);
        for i in 1..4 {
            let f = f64::from(i) / 4.0;
            let x = canvas.x0 + canvas.width() * f;
            let y = canvas.y0 + canvas.height() * f;
            let mut v = BezPath::new();
            v.move_to((x, canvas.y0 + 1.0));
            v.line_to((x, canvas.y1 - 1.0));
            let mut h = BezPath::new();
            h.move_to((canvas.x0 + 1.0, y));
            h.line_to((canvas.x1 - 1.0, y));
            scene.stroke(&Stroke::new(1.0), tf, grid, None, &v);
            scene.stroke(&Stroke::new(1.0), tf, grid, None, &h);
        }
        // The identity: where every tone would land if the curve did nothing.
        // Without it a lifted curve and a steep one look the same.
        let mut diag = BezPath::new();
        diag.move_to((canvas.x0, canvas.y1));
        diag.line_to((canvas.x1, canvas.y0));
        scene.stroke(
            &Stroke::new(1.0),
            tf,
            with_alpha(c.text_dim, 0.45),
            None,
            &diag,
        );

        // The curve itself, sampled densely enough that the cubic reads as a
        // curve rather than as the polyline it is drawn with.
        let tint = curve_channel_color(theme, ch);
        let mut path = BezPath::new();
        for i in 0..=STEPS {
            let t = i as f64 / STEPS as f64;
            let p = curve_point(canvas, t, spline.eval(t));
            if i == 0 {
                path.move_to((p.x, p.y));
            } else {
                path.line_to((p.x, p.y));
            }
        }
        scene.stroke(&Stroke::new(2.0), tf, tint, None, &path);

        // Control points. The one under the pointer (or being dragged) is
        // larger as well as brighter, so it is legible without relying on a
        // hue difference.
        let dragging = match self.drag {
            Some(Drag::CurvePoint { channel, idx }) if channel == ch => Some(idx),
            _ => None,
        };
        let hovered = canvas
            .contains(self.last_mouse)
            .then(|| {
                let (mx, my) = curve_coords(canvas, self.last_mouse);
                spline.point_near(mx, my, POINT_GRAB)
            })
            .flatten();
        for (i, cp) in spline.points.iter().enumerate() {
            let at = curve_point(canvas, cp.x, cp.y);
            let live = dragging == Some(i) || (dragging.is_none() && hovered == Some(i));
            let r = if live { 6.0 } else { 4.0 };
            scene.fill(Fill::NonZero, tf, tint, None, &Circle::new(at, r));
            scene.stroke(
                &Stroke::new(1.5),
                tf,
                if live {
                    c.text_primary
                } else {
                    c.surfaces.base
                },
                None,
                &Circle::new(at, r),
            );
        }

        // What the gesture is, stated rather than left to be discovered. It
        // sits under the box so it never covers the curve.
        let hint = if dragging.is_some() {
            "Drag out of the box to remove"
        } else {
            "Click to add a point · drag to shape"
        };
        text(
            scene,
            tr,
            tf,
            hint,
            10.5,
            c.text_dim,
            canvas.x0,
            canvas.y1 + 12.0,
        );
    }

    /// Draw the marker list: one row per marked instant, with rename and
    /// delete revealed under the pointer. Shared by both inspector states.
    /// Paint the project's format section: what the frame is now, the size
    /// and rate presets with the one in force marked, and the match button.
    fn draw_project_section(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        l: &InspectorLayout,
        x0: f64,
    ) {
        let c = &theme.tokens.colors;
        if l.project_format_y > 0.0 {
            // The cached words, not a fresh format!: `draw` runs for every
            // animation frame and this string only changes when the project's
            // format does.
            text_w(
                scene,
                tr,
                tf,
                &self.project_format_shown,
                &TextStyle::new(12.0).with_weight(TextWeight::Medium),
                c.text_primary,
                Point::new(x0, l.project_format_y),
            );
        }
        if !l.project_frames.is_empty() {
            let rects: Vec<Rect> = l.project_frames.iter().map(|(_, r)| *r).collect();
            let labels: Vec<&str> = FRAME_PRESETS.iter().map(|(name, _, _)| *name).collect();
            // Nothing is marked unless the frame is exactly a preset: a
            // segmented control that always highlights something would claim
            // a 1440x1080 project was 1080p.
            let sel = l
                .project_frames
                .iter()
                .position(|((w, h), _)| *w == self.project.width && *h == self.project.height)
                .unwrap_or(usize::MAX);
            draw_segmented(scene, tf, tr, theme, &rects, &labels, sel, self.last_mouse);
        }
        if l.project_rate_y > 0.0 {
            text(
                scene,
                tr,
                tf,
                "Rate",
                INSPECTOR_LABEL_SIZE,
                c.text_dim,
                x0,
                l.project_rate_y,
            );
        }
        if !l.project_rates.is_empty() {
            let rects: Vec<Rect> = l.project_rates.iter().map(|(_, r)| *r).collect();
            let labels: Vec<String> = RATE_PRESETS.iter().map(|r| rate_words(*r)).collect();
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            let sel = l
                .project_rates
                .iter()
                .position(|(rate, _)| (rate - self.project.fps).abs() < 1e-6)
                .unwrap_or(usize::MAX);
            draw_segmented(scene, tf, tr, theme, &rects, &refs, sel, self.last_mouse);
        }
        if let Some(rect) = l.project_match {
            // The button names the source it will copy, so pressing it is not
            // a guess about which of several sources it meant.
            let label = self.format_source().map_or_else(
                || "Match media".to_string(),
                |m| format!("Match {}", m.name),
            );
            let shown = elide_w(&label, (rect.width() - 20.0) as f32, tr);
            draw_button(
                scene,
                tf,
                tr,
                theme,
                rect,
                &shown,
                false,
                rect.contains(self.last_mouse),
            );
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "marker rows share one measured scroll coordinate system for rendering, hit targets, and empty-state actions"
    )]
    fn draw_marker_rows(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        l: &InspectorLayout,
        x0: f64,
    ) {
        let c = &theme.tokens.colors;
        let inspector_scale = self.inspector_ui_scale();
        for (m, row) in &l.marker_rows {
            let (name_rect, note_btn, rename, del) = marker_row_parts(*row);
            let hovered = row.contains(self.last_mouse);
            selection_row(
                scene,
                tf,
                theme,
                *row,
                6.0,
                ui::chrome::SelectionState::new(false, hovered, false),
            );
            let Some((_, mk, _)) = self
                .project
                .all_markers()
                .into_iter()
                .find(|(_, k, _)| k.id == m.id)
            else {
                continue;
            };
            let col = clip_color(theme, mk.palette);
            scene.fill(
                Fill::NonZero,
                tf,
                col,
                None,
                &Circle::new(Point::new(row.x0 + 7.0, row.y0 + 13.0), 4.0),
            );
            let editing = self.marker_rename.as_ref().map(|(r, _)| r.id) == Some(m.id);
            let renaming = editing && self.marker_field == MarkerField::Name;
            if !renaming {
                let name = if mk.name.trim().is_empty() {
                    "Marker"
                } else {
                    mk.name.as_str()
                };
                let shown = elide_w(name, name_rect.width() as f32, tr);
                let baseline = text_baseline(name_rect, 12.0, tr);
                text(
                    scene,
                    tr,
                    tf,
                    &shown,
                    12.0,
                    c.text_primary,
                    name_rect.x0,
                    baseline,
                );
                // A clip marker says which clip it travels with, because
                // moving that clip moves the marker and nothing else would
                // explain why the time changed.
                let stamp = timecode(m.at, self.project.fps);
                let (sw2, _) = tr.measure(&stamp, 11.0, None);
                let stamp_col = if m.clip.is_some() {
                    c.text_tertiary
                } else {
                    c.text_dim
                };
                text(
                    scene,
                    tr,
                    tf,
                    &stamp,
                    11.0,
                    stamp_col,
                    note_btn.x0 - 8.0 - f64::from(sw2),
                    baseline,
                );
            }
            // The note, on its own line under the name. Present only when
            // there is one, so an unannotated list stays a list of names.
            let editing_note = editing && self.marker_field == MarkerField::Note;
            if !mk.note.trim().is_empty() && !editing_note {
                let shown = elide_w(mk.note.trim(), (row.width() - 34.0) as f32, tr);
                let line = Rect::new(row.x0 + 16.0, row.y0 + 26.0, row.x1, row.y0 + 44.0);
                let base = text_baseline(line, 11.0, tr);
                text(scene, tr, tf, &shown, 11.0, c.text_tertiary, line.x0, base);
            }
            // Rename and delete only appear under the pointer, so a resting
            // list reads as names and times rather than as a wall of icons.
            if hovered {
                for (rect, icon, tint) in [
                    (note_btn, ui::icons::Icon::FileText, c.text_primary),
                    (rename, ui::icons::Icon::Pencil, c.text_primary),
                    (del, ui::icons::Icon::X, c.semantic.danger),
                ] {
                    selection_row(
                        scene,
                        tf,
                        theme,
                        rect,
                        4.0,
                        ui::chrome::SelectionState::new(
                            false,
                            rect.contains(self.last_mouse),
                            false,
                        ),
                    );
                    let g = 11.0;
                    let box_ = Rect::new(
                        rect.center().x - g / 2.0,
                        rect.center().y - g / 2.0,
                        rect.center().x + g / 2.0,
                        rect.center().y + g / 2.0,
                    );
                    ui::icons::draw_icon(
                        scene,
                        tf,
                        box_,
                        icon,
                        tint,
                        ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                    );
                }
            }
        }
        if let Some(rect) = l.add_marker {
            if l.marker_rows.is_empty() {
                text(
                    scene,
                    tr,
                    tf,
                    "Nothing marked yet",
                    INSPECTOR_LABEL_SIZE,
                    c.text_dim,
                    x0,
                    rect.y0 - 16.0 * inspector_scale,
                );
            }
            draw_button(
                scene,
                tf,
                tr,
                theme,
                rect,
                ADD_MARKER_LABEL,
                false,
                rect.contains(self.last_mouse),
            );
        }
    }

    pub(super) fn draw_inspector(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let c = &theme.tokens.colors;
        fill_rect(scene, tf, r.inspector, c.surfaces.raised);
        stroke_line(
            scene,
            tf,
            pane_hairline(theme),
            r.inspector.x0 + 0.5,
            r.inspector.y0,
            r.inspector.x0 + 0.5,
            r.inspector.y1,
            1.0,
        );
        // Clip everything to the inspector rect so controls never bleed into
        // the timeline if the selected clip's control stack runs tall.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            tf,
            &r.inspector,
        );
        // The scroll body gets its own clip, below the fixed identity card.
        // Painting an opaque header over it is not enough: antialiased glyph
        // edges and controls halfway through a translate can otherwise still
        // show through at the seam.
        let body_view = self.inspector_viewport(r);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            tf,
            &body_view,
        );
        self.draw_inspector_body(scene, tf, tr, theme, r);
        scene.pop_layer();
        // The body may be translated all the way to its final action row.
        // Paint the persistent context last, over that translated content, so
        // a clipped control can never masquerade as part of the header.
        self.draw_inspector_fixed_header(scene, tf, tr, theme, r);
        // A clip on a locked track has inert controls — every edit path checks
        // the lock — so they are veiled rather than left looking live. The
        // header above stays crisp, because that is where the reason is
        // written ("on V2 · locked").
        let locked = self
            .selected()
            .is_some_and(|id| !self.project.clip_editable(id));
        if locked {
            let l = self.inspector_layout(r);
            let top = l
                .opacity
                .or(l.volume)
                .map_or(r.inspector.y0 + 170.0, |t| t.y0 - 24.0);
            let veil = Rect::new(
                r.inspector.x0,
                top.max(r.inspector.y0),
                r.inspector.x1,
                r.inspector.y1,
            );
            fill_rect(scene, tf, veil, with_alpha(c.surfaces.raised, 0.6));
        }
        // The keyboard cursor, drawn over the lock veil rather than under it:
        // where the keyboard *is* stays legible even where the controls are
        // inert, because the walk still visits them. Inside the pane's clip
        // layer, so a row scrolled out of view keeps its ring off the timeline.
        if self.shows_focus_cursor(Pane::Inspector) {
            let rows = self.a11y_inspector_rows();
            if let Some(row) = rows.get(self.focus.inspector).copied()
                && let Some(band) = Self::inspector_focus_band(&self.inspector_layout(r), row)
                && band.intersect(self.inspector_viewport(r)).area() > 0.0
            {
                draw_focus_ring(scene, tf, theme, band, 5.0);
            }
        }
        // Soft edges where the pane cuts its content. A row sliced in half by
        // a hard boundary reads as a broken control; the same row fading out
        // reads as "this scrolls", which is what it is. Only drawn on the edge
        // that actually has content beyond it.
        let max = self.inspector_max_scroll(r);
        let fade = |scene: &mut Scene, from_y: f64, dir: f64| {
            const STEPS: i32 = 14;
            const H: f64 = 22.0;
            for i in 0..STEPS {
                let f = f64::from(i) / f64::from(STEPS);
                let y0 = from_y + dir * H * f;
                let y1 = y0 + dir * (H / f64::from(STEPS));
                let band = Rect::new(r.inspector.x0, y0.min(y1), r.inspector.x1, y0.max(y1));
                fill_rect(
                    scene,
                    tf,
                    band,
                    with_alpha(c.surfaces.raised, 0.10 + 0.9 * f),
                );
            }
        };
        if self.inspector_scroll > 0.5 {
            fade(scene, self.inspector_viewport(r).y0, 1.0);
        }
        if self.inspector_scroll < max - 0.5 {
            fade(scene, self.inspector_viewport(r).y1, -1.0);
        }
        scene.pop_layer();
        // Real, grabbable scrollbar. What stood here was a painted indicator
        // with no hit-test, so the EFFECTS section below the fold could be
        // seen to exist and never reached.
        self.draw_scrollbar(scene, tf, theme, ScrollPane::Inspector, r);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the fixed identity card's measured text, clip metadata, and viewport boundary must be composed together"
    )]
    fn draw_inspector_fixed_header(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let c = &theme.tokens.colors;
        let s = self.inspector_ui_scale();
        let header_bottom = self.inspector_body_origin(r);
        let header = Rect::new(
            r.inspector.x0,
            r.inspector.y0,
            r.inspector.x1,
            header_bottom,
        );
        fill_rect(scene, tf, header, c.surfaces.raised);
        let x0 = r.inspector.x0 + PAD * s;
        let x1 = r.inspector.x1 - PAD * s;
        let top = r.inspector.y0;
        text_w(
            scene,
            tr,
            tf,
            "INSPECTOR",
            &TextStyle::new(11.0).with_weight(TextWeight::Semibold),
            c.text_dim,
            Point::new(x0, top + 14.0 * s),
        );
        if self.selection.len() > 1 {
            let label = format!("{} clips selected", self.selection.len());
            let (lw, _) = tr.measure_weighted(&label, 10.5, None, TextWeight::Medium);
            let pillr = Rect::new(
                x1 - f64::from(lw) - 14.0 * s,
                top + 4.0 * s,
                x1,
                top + 24.0 * s,
            );
            fill_round(scene, tf, pillr, 10.0 * s, with_alpha(c.accent[5], 0.22));
            text_w(
                scene,
                tr,
                tf,
                &label,
                &TextStyle::new(10.5).with_weight(TextWeight::Medium),
                c.accent[5],
                Point::new(pillr.x0 + 7.0 * s, top + 9.0 * s),
            );
        }
        let Some(clip) = self.selected().and_then(|id| self.project.clip(id)) else {
            return;
        };
        let tag = Rect::new(x0, top + 36.0 * s, x0 + 6.0 * s, top + 68.0 * s);
        fill_round(scene, tf, tag, 3.0 * s, clip_color(theme, clip.palette));
        let name = self
            .project
            .media(clip.media)
            .map_or("Missing media", |item| item.name.as_str());
        let shown = elide_w(name, (x1 - x0 - 16.0 * s) as f32, tr);
        text_w(
            scene,
            tr,
            tf,
            &shown,
            &TextStyle::new(15.0).with_weight(TextWeight::Semibold),
            c.text_primary,
            Point::new(x0 + 14.0 * s, top + 39.0 * s),
        );
        let track_name = self
            .project
            .track(clip.track)
            .map_or("track", |track| track.name.as_str());
        let sub = if self.selection_has_locked() {
            format!("on {track_name} · locked")
        } else {
            format!("on {track_name}")
        };
        text(
            scene,
            tr,
            tf,
            &sub,
            11.5,
            if self.selection_has_locked() {
                c.semantic.warning
            } else {
                c.text_dim
            },
            x0 + 14.0 * s,
            top + 62.0 * s,
        );
        let mut prop = |label: &str, value: String, y: f64| {
            text(scene, tr, tf, label, 12.0, c.text_dim, x0, y);
            let (w, _) = tr.measure_weighted(&value, 12.0, None, TextWeight::Medium);
            text_w(
                scene,
                tr,
                tf,
                &value,
                &TextStyle::new(12.0).with_weight(TextWeight::Medium),
                c.text_primary,
                Point::new(x1 - f64::from(w), y),
            );
        };
        let fps = self.project.fps;
        prop("Position", timecode(clip.start, fps), top + 84.0 * s);
        prop("Duration", timecode(clip.duration(), fps), top + 106.0 * s);
        prop("Source In", timecode(clip.in_point, fps), top + 128.0 * s);
        prop("Source Out", timecode(clip.out_point, fps), top + 150.0 * s);
        stroke_line(
            scene,
            tf,
            c.border_standard,
            r.inspector.x0,
            header_bottom,
            r.inspector.x1,
            header_bottom,
            1.0,
        );
    }

    #[allow(
        clippy::indexing_slicing,
        clippy::too_many_lines,
        reason = "inspector controls index fixed option tables and share one scroll coordinate system with their validated hit targets"
    )]
    #[expect(
        clippy::many_single_char_names,
        reason = "the paint pass's shorthands: colours c, regions r, layout l, value v and the y pen"
    )]
    fn draw_inspector_body(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: &Regions,
    ) {
        let c = &theme.tokens.colors;
        let inspector_scale = self.inspector_ui_scale();
        let x0 = r.inspector.x0 + PAD * inspector_scale;
        let x1 = r.inspector.x1 - PAD * inspector_scale;
        // The whole body scrolls together by `inspector_scroll` (clamped to 0
        // when nothing is selected, so the empty state stays put).
        let top = r.inspector.y0;
        // One size for every control label and caption in the panel. The rows
        // used to mix 12, 11.5 and 11: the fade and transition-length labels
        // read as a different, smaller kind of row from the opacity and grade
        // labels directly above them.
        let label_size = INSPECTOR_LABEL_SIZE;
        // The heading row shares the media bin's band and centre line, so the
        // two panes' headings — and the chips beside them — line up.
        let heading = Rect::new(
            x0,
            top + PANE_HEADING_Y0,
            x1,
            top + PANE_HEADING_Y0 + PANE_HEADING_H,
        );
        let heading_y = text_baseline(heading, 11.0, tr);
        text_w(
            scene,
            tr,
            tf,
            "INSPECTOR",
            &TextStyle::new(11.0).with_weight(TextWeight::Semibold),
            c.text_dim,
            Point::new(x0, heading_y),
        );
        // With more than one clip selected, say so where the count matters —
        // every edit below applies to all of them.
        if self.selection.len() > 1 {
            let label = format!("{} clips selected", self.selection.len());
            let (lw, _) = tr.measure_weighted(&label, 10.5, None, TextWeight::Medium);
            let pillr = Rect::new(x1 - f64::from(lw) - 14.0, heading.y0, x1, heading.y1);
            // A quiet capsule with its derived edge; the count keeps the
            // accent in its words, not as a wash under them.
            well(scene, tf, theme, pillr, PANE_HEADING_H / 2.0);
            let pill_y = text_baseline(pillr, 10.5, tr);
            text_w(
                scene,
                tr,
                tf,
                &label,
                &TextStyle::new(10.5).with_weight(TextWeight::Medium),
                c.accent[5],
                Point::new(pillr.x0 + 7.0, pill_y),
            );
        }

        let selected = self
            .selected()
            .and_then(|id| self.project.clip(id).cloned());
        let Some(clip) = selected else {
            // The markers still belong to the project, so they stay reachable
            // with nothing selected. Everything else here edits a clip.
            let l = self.inspector_layout(r);
            let blank = Clip::default();
            // The empty state gets the room above the first section header,
            // not the whole pane — the sections below are real controls and
            // must not be drawn through.
            let floor = l
                .sections
                .first()
                .map_or(r.inspector.y1, |(_, rect)| rect.y0 - 8.0);
            let area = Rect::new(
                x0,
                top + INSPECTOR_HEADER_H,
                x1,
                floor.max(top + INSPECTOR_HEADER_H),
            );
            // The shared empty state, so this pane says nothing is selected in
            // the same voice — icon, title, one line of what to do — that the
            // rest of the fleet uses. It was a pair of bare left-aligned lines
            // that read as a label for the sections beneath them rather than
            // as the state of the pane.
            ui::widget::EmptyState::paint(
                &mut ui::paint::PaintCtx::new(scene, tf, tr, theme),
                area,
                &ui::widget::EmptyStateContent {
                    icon: Some(ui::icons::Icon::Layers),
                    title: INSPECTOR_EMPTY_TITLE,
                    message: Some(INSPECTOR_EMPTY_HINT),
                    action_label: None,
                },
                Point::new(f64::MIN, f64::MIN),
            );
            for (s, rect) in &l.sections {
                self.draw_section_header(
                    &mut PaintCtx::new(scene, tf, tr, theme),
                    *rect,
                    *s,
                    &blank,
                    None,
                );
            }
            self.draw_marker_rows(scene, tf, tr, theme, &l, x0);
            self.draw_project_section(scene, tf, tr, theme, &l, x0);
            return;
        };
        let media = self.project.media(clip.media).cloned();
        let is_video = media.as_ref().is_none_or(|m| m.kind.is_visual());
        let track_name = self
            .project
            .track(clip.track)
            .map(|t| t.name.clone())
            .unwrap_or_default();

        // Header card with color tag + name (scrolls with the body).
        let tag = Rect::new(x0, top + 36.0, x0 + 6.0, top + 64.0);
        fill_round(scene, tf, tag, 3.0, clip_color(theme, clip.palette));
        let name = media
            .as_ref()
            .map_or_else(|| "clip".into(), |m| m.name.clone());
        let shown_name = elide_w(&name, (x1 - x0 - 14.0) as f32, tr);
        text_w(
            scene,
            tr,
            tf,
            &shown_name,
            &TextStyle::new(15.0).with_weight(TextWeight::Semibold),
            c.text_primary,
            Point::new(x0 + 14.0, top + 38.0),
        );
        // Name the lane, and say plainly when it refuses edits — the same
        // reason the timeline hatches it.
        let sub = if self.selection_has_locked() {
            format!("on {track_name} · locked")
        } else {
            format!("on {track_name}")
        };
        let sub_col = if self.selection_has_locked() {
            c.semantic.warning
        } else {
            c.text_dim
        };
        text(scene, tr, tf, &sub, 11.5, sub_col, x0 + 14.0, top + 58.0);

        // Property rows.
        let mut y = top + 78.0;
        let fps = self.project.fps;
        let prop = |scene: &mut Scene, tr: &mut TextRenderer, label: &str, value: &str, y: f64| {
            text(scene, tr, tf, label, 12.0, c.text_dim, x0, y);
            let (vw, _) = tr.measure_weighted(value, 12.0, None, TextWeight::Medium);
            text_w(
                scene,
                tr,
                tf,
                value,
                &TextStyle::new(12.0).with_weight(TextWeight::Medium),
                c.text_primary,
                Point::new(x1 - f64::from(vw), y),
            );
        };
        prop(scene, tr, "Position", &timecode(clip.start, fps), y);
        y += 18.0;
        prop(scene, tr, "Duration", &timecode(clip.duration(), fps), y);
        y += 18.0;
        prop(scene, tr, "Source In", &timecode(clip.in_point, fps), y);
        y += 18.0;
        prop(scene, tr, "Source Out", &timecode(clip.out_point, fps), y);

        let l = self.inspector_layout(r);
        // Every slider row: a label, a right-aligned readout measured like all
        // the others, and the shared slider face carrying hover / drag state.
        let row = |scene: &mut Scene,
                   tr: &mut TextRenderer,
                   track: Option<Rect>,
                   label: &str,
                   readout: String,
                   v: f64,
                   size: f32,
                   drag: bool,
                   // `Some` when this row carries a key control, whose box
                   // the label starts after. The indent is unconditional
                   // so keying a parameter does not shuffle its row
                   // sideways; an animated one takes the accent instead.
                   keyed: Option<bool>| {
            let Some(track) = track else { return };
            let lx = if keyed.is_some() {
                track.x0 + KEY_BOX + 6.0
            } else {
                track.x0
            };
            let col = if keyed == Some(true) {
                c.accent[5]
            } else {
                c.text_dim
            };
            text(scene, tr, tf, label, size, col, lx, track.y0 - 20.0);
            let (pw, _) = tr.measure(&readout, size, None);
            text(
                scene,
                tr,
                tf,
                &readout,
                size,
                c.text_primary,
                track.x1 - f64::from(pw),
                track.y0 - 20.0,
            );
            draw_track(
                scene,
                tf,
                theme,
                track,
                v,
                hit_slider(track, self.last_mouse),
                drag,
            );
        };
        let dragging = |d: Option<Drag>, want: Drag| d == Some(want);
        // Animatable rows read through `param_at`, the same door the
        // compositor uses, so a keyframed slider follows the playhead rather
        // than showing a constant the picture is already ignoring.
        let animated = |p: Param| self.key_state(p) != KeyState::Off;
        let at = self.playhead;
        let opacity = clip.param_at(Param::Opacity, at);
        row(
            scene,
            tr,
            l.opacity,
            "Opacity",
            format!("{:.0}%", opacity * 100.0),
            opacity,
            label_size,
            dragging(self.drag, Drag::Opacity),
            Some(animated(Param::Opacity)),
        );
        let volume = clip.param_at(Param::Volume, at);
        row(
            scene,
            tr,
            l.volume,
            "Volume",
            format!("{:.0}%", volume * 100.0),
            volume,
            label_size,
            dragging(self.drag, Drag::Volume),
            Some(animated(Param::Volume)),
        );
        // Colour-grade rows. The readout shows the multiplier (0.5 == 1.00×,
        // the identity).
        for (track, label, p, d) in [
            (
                l.brightness,
                "Brightness",
                Param::Brightness,
                Drag::Brightness,
            ),
            (l.contrast, "Contrast", Param::Contrast, Drag::Contrast),
            (
                l.saturation,
                "Saturation",
                Param::Saturation,
                Drag::Saturation,
            ),
        ] {
            let v = clip.param_at(p, at);
            row(
                scene,
                tr,
                track,
                label,
                format!("{:.2}×", Filters::gain(v)),
                v,
                label_size,
                dragging(self.drag, d),
                Some(animated(p)),
            );
        }
        // Fade in / out (seconds). Two half-width tracks share a row; their
        // readouts right-align by measurement like every other row, rather
        // than by a magic inset that only fitted one string.
        let cd = clip.duration().max(1e-6);
        row(
            scene,
            tr,
            l.fade_in,
            "Fade In",
            format!("{:.1}s", clip.fade_in),
            (clip.fade_in / cd).clamp(0.0, 1.0),
            label_size,
            dragging(self.drag, Drag::FadeIn),
            None,
        );
        row(
            scene,
            tr,
            l.fade_out,
            "Fade Out",
            format!("{:.1}s", clip.fade_out),
            (clip.fade_out / cd).clamp(0.0, 1.0),
            label_size,
            dragging(self.drag, Drag::FadeOut),
            None,
        );
        // Transition segmented control, through the shared painter.
        if !l.transitions.is_empty() {
            text(
                scene,
                tr,
                tf,
                "Transition",
                label_size,
                c.text_dim,
                x0,
                l.transition_y - 20.0,
            );
            let rects: Vec<Rect> = l.transitions.iter().map(|(_, r)| *r).collect();
            let labels: Vec<&str> = l.transitions.iter().map(|(t, _)| t.label()).collect();
            let sel = l
                .transitions
                .iter()
                .position(|(t, _)| *t == clip.transition)
                .unwrap_or(0);
            draw_segmented(scene, tf, tr, theme, &rects, &labels, sel, self.last_mouse);
        }
        // Transition length.
        if let Some(track) = l.transition_len {
            let max = clip.duration().max(0.01);
            row(
                scene,
                tr,
                Some(track),
                "Transition length",
                format!("{:.2}s", clip.transition_len()),
                (clip.transition_len() / max).clamp(0.0, 1.0),
                label_size,
                self.drag == Some(Drag::TransitionLen),
                None,
            );
        }
        // Colour swatches.
        if !l.colors.is_empty() {
            text(
                scene,
                tr,
                tf,
                "Label",
                label_size,
                c.text_dim,
                x0,
                l.colors_y - 20.0,
            );
            for (pal, rect) in &l.colors {
                let sw = clip_color(theme, *pal);
                ui::surface::card(scene, tf, *rect, 6.0, sw);
                if rect.contains(self.last_mouse) && clip.palette != *pal {
                    stroke_round(scene, tf, *rect, 6.0, with_alpha(c.text_primary, 0.5), 1.5);
                }
                if clip.palette == *pal {
                    // The selection ring has to read against whichever colour
                    // it rings, so it comes from the focus token rather than
                    // a hardcoded white that vanishes on the pale swatches.
                    stroke_round(scene, tf, rect.inset(1.5), 7.0, c.border_focus, 2.0);
                }
            }
        }
        // Link / unlink. The caption names the state so the button can name
        // the *change*: a control labelled only "Unlink" leaves a reader
        // guessing whether it describes what is true or what will happen.
        if let (Some(rect), Some(offer)) = (l.link, self.link_affordance()) {
            let linked = offer == LinkAffordance::Unlink;
            text(
                scene,
                tr,
                tf,
                offer.state(),
                12.0,
                if linked { c.accent[5] } else { c.text_dim },
                x0,
                l.link_label_y - 15.0,
            );
            draw_button_enabled(
                &mut PaintCtx::new(scene, tf, tr, theme),
                rect,
                offer.label(),
                false,
                rect.contains(self.last_mouse),
                true,
            );
        }
        // ---- collapsible sections ------------------------------------
        // Headers first, so a folded section still says what is inside it.
        for (s, rect) in &l.sections {
            self.draw_section_header(
                &mut PaintCtx::new(scene, tf, tr, theme),
                *rect,
                *s,
                &clip,
                media.as_ref(),
            );
        }
        // Every ranged control laid out this frame: geometry, rate, title
        // size. Each reads its value back through `num_value`, so a keyframed
        // one tracks the playhead rather than showing a dead constant.
        for (f, track) in &l.nums {
            let Some(v) = self.num_value(*f) else {
                continue;
            };
            let keyed = f
                .param()
                .is_some_and(|p| self.key_state(p) != KeyState::Off);
            let lx = if f.param().is_some() {
                track.x0 + KEY_BOX + 6.0
            } else {
                track.x0
            };
            let label_col = if keyed { c.accent[5] } else { c.text_dim };
            text(
                scene,
                tr,
                tf,
                f.label(),
                label_size,
                label_col,
                lx,
                track.y0 - 20.0,
            );
            let readout = f.readout(v);
            let (pw, _) = tr.measure(&readout, label_size, None);
            text(
                scene,
                tr,
                tf,
                &readout,
                label_size,
                c.text_primary,
                track.x1 - f64::from(pw),
                track.y0 - 20.0,
            );
            draw_track(
                scene,
                tf,
                theme,
                *track,
                f.to_frac(v),
                hit_slider(*track, self.last_mouse),
                self.drag == Some(Drag::Num(*f)),
            );
        }
        // Picture-in-picture presets. The glyph is the corner it puts the
        // picture in, which is the whole of what the button does.
        if !l.pip.is_empty() {
            let top = l.pip[0].2;
            text(
                scene,
                tr,
                tf,
                "Picture in picture",
                label_size,
                c.text_dim,
                x0,
                top.y0 - 16.0 * inspector_scale,
            );
            let inset = crate::model::Transform::pip(PIP_SCALE, (1.0, -1.0), PIP_INSET);
            let is_pip = (clip.transform.scale_x - inset.scale_x).abs() < 1e-6;
            for (_, corner, rect) in &l.pip {
                let on = is_pip
                    && (clip.transform.pos_x.signum() - corner.0).abs() < 1e-6
                    && (clip.transform.pos_y.signum() - corner.1).abs() < 1e-6;
                let hovered = rect.contains(self.last_mouse);
                // The shared button face — Primary for the preset in force —
                // with the corner glyph painted over it in the face's ink.
                let mut face = ui::widget::ButtonFace::new(*rect, "")
                    .radius(6.0)
                    .hovered(hovered);
                if on {
                    face = face.primary();
                }
                ui::widget::Button::paint(scene, tf, tr, theme, &face);
                // A miniature frame with the inset drawn in the named corner.
                let fg = if on {
                    on_fill(theme, theme.accent_primary).1
                } else {
                    c.text_primary
                };
                let g = Rect::new(
                    rect.center().x - 11.0,
                    rect.center().y - 7.0,
                    rect.center().x + 11.0,
                    rect.center().y + 7.0,
                );
                stroke_round(scene, tf, g, 2.0, with_alpha(fg, 0.5), 1.0);
                let iw = g.width() * 0.42;
                let ih = g.height() * 0.42;
                let ix = if corner.0 < 0.0 {
                    g.x0 + 2.0
                } else {
                    g.x1 - 2.0 - iw
                };
                let iy = if corner.1 < 0.0 {
                    g.y0 + 2.0
                } else {
                    g.y1 - 2.0 - ih
                };
                fill_round(scene, tf, Rect::new(ix, iy, ix + iw, iy + ih), 1.5, fg);
            }
        }
        if l.crop_label_y > 0.0 {
            text(
                scene,
                tr,
                tf,
                "Crop",
                label_size,
                c.text_dim,
                x0,
                l.crop_label_y,
            );
        }
        if l.anchor_label_y > 0.0 {
            text(
                scene,
                tr,
                tf,
                "Anchor",
                label_size,
                c.text_dim,
                x0,
                l.anchor_label_y,
            );
        }
        // Three segmented rows make the 3×3 grid, so each row is a real
        // shared toggle group rather than nine hand-drawn cells.
        for band in l.anchor.chunks(3) {
            let rects: Vec<Rect> = band.iter().map(|(_, r)| *r).collect();
            let labels: Vec<&str> = band
                .iter()
                .map(|(a, _)| {
                    #[expect(
                        clippy::float_cmp,
                        reason = "the anchor cells are laid out from `ANCHOR_STOPS` itself, so equality is the lookup"
                    )]
                    let col = ANCHOR_STOPS.iter().position(|s| *s == a.0).unwrap_or(1);
                    #[expect(
                        clippy::float_cmp,
                        reason = "the anchor cells are laid out from `ANCHOR_STOPS` itself, so equality is the lookup"
                    )]
                    let row = ANCHOR_STOPS.iter().position(|s| *s == a.1).unwrap_or(1);
                    ANCHOR_GLYPHS[row * 3 + col]
                })
                .collect();
            let sel = band
                .iter()
                .position(|(a, _)| {
                    (clip.transform.anchor_x - a.0).abs() < 1e-6
                        && (clip.transform.anchor_y - a.1).abs() < 1e-6
                })
                .unwrap_or(usize::MAX);
            draw_segmented(scene, tf, tr, theme, &rects, &labels, sel, self.last_mouse);
        }
        if let Some(rect) = l.reset_transform {
            let plain = clip.transform.is_identity();
            draw_button_enabled(
                &mut PaintCtx::new(scene, tf, tr, theme),
                rect,
                "Fill frame",
                false,
                rect.contains(self.last_mouse),
                !plain,
            );
        }
        // Rate presets + the reverse toggle.
        if !l.speed_presets.is_empty() {
            let rects: Vec<Rect> = l.speed_presets.iter().map(|(_, r)| *r).collect();
            let labels: Vec<String> = l
                .speed_presets
                .iter()
                .map(|(rate, _)| {
                    if (rate - rate.round()).abs() < 1e-6 {
                        format!("{}×", rate.round() as i64)
                    } else {
                        format!("{rate}×")
                    }
                })
                .collect();
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            // No preset is "selected" unless the rate is exactly on one —
            // a segmented control that always highlights something would
            // claim 1.37× was 1×.
            let sel = l
                .speed_presets
                .iter()
                .position(|(rate, _)| (rate - clip.speed.rate()).abs() < 1e-6)
                .unwrap_or(usize::MAX);
            draw_segmented(scene, tf, tr, theme, &rects, &refs, sel, self.last_mouse);
        }
        if let Some(rect) = l.reverse {
            let on = clip.speed.reverse;
            draw_button(
                scene,
                tf,
                tr,
                theme,
                rect,
                if on {
                    "Playing backwards"
                } else {
                    "Play backwards"
                },
                on,
                rect.contains(self.last_mouse),
            );
        }
        // ---- the lane's mix ---------------------------------------------
        // The panel is otherwise entirely about one clip, so the two knobs
        // that reach past it have to say what they reach — including that
        // everything else on the lane moves with them.
        if let Some(t) = self.selected_track() {
            let level = self.mix.track(t.id);
            if l.mix_track_y > 0.0 {
                let others = self.project.clips_on(t.id).len().saturating_sub(1);
                let words = match others {
                    0 => format!("{} — this lane", t.name),
                    1 => format!("{} — this lane, 1 other clip", t.name),
                    n => format!("{} — this lane, {n} other clips", t.name),
                };
                let words = elide_at(&words, (x1 - x0) as f32, label_size, tr);
                text(
                    scene,
                    tr,
                    tf,
                    &words,
                    label_size,
                    c.text_dim,
                    x0,
                    l.mix_track_y,
                );
            }
            if let Some(meter) = l.mix_meter {
                draw_level_meter(
                    scene,
                    tf,
                    theme,
                    meter,
                    level.map(|v| v.level).unwrap_or_default(),
                    level.map_or(0.0, |v| v.hold),
                    level.is_some_and(|v| v.clipped),
                );
            }
        }
        if let Some(rect) = l.mix_reset {
            draw_button(
                scene,
                tf,
                tr,
                theme,
                rect,
                "Reset to 0.0 dB, centre",
                false,
                rect.contains(self.last_mouse),
            );
        }
        // ---- tone curves ------------------------------------------------
        if !l.curve_channels.is_empty() {
            let rects: Vec<Rect> = l.curve_channels.iter().map(|(_, r)| *r).collect();
            let labels: Vec<&str> = l.curve_channels.iter().map(|(c, _)| c.label()).collect();
            let sel = l
                .curve_channels
                .iter()
                .position(|(c, _)| *c == self.curve_channel)
                .unwrap_or(0);
            draw_segmented(scene, tf, tr, theme, &rects, &labels, sel, self.last_mouse);
        }
        if let Some(canvas) = l.curve_canvas {
            self.draw_curve_editor(scene, tf, tr, theme, canvas, &clip);
        }
        if let Some(rect) = l.curve_reset {
            let ch = self.curve_channel;
            let shaped = !clip.curves.spline(ch).is_identity();
            draw_button_enabled(
                &mut PaintCtx::new(scene, tf, tr, theme),
                rect,
                &format!("Reset {}", ch.label()),
                false,
                rect.contains(self.last_mouse),
                shaped,
            );
        }
        // ---- colour key -----------------------------------------------
        if let Some(rect) = l.key_enable {
            let on = clip.key.enabled;
            draw_button(
                scene,
                tf,
                tr,
                theme,
                rect,
                if on {
                    "Keying this colour out"
                } else {
                    "Key a colour out"
                },
                on,
                rect.contains(self.last_mouse),
            );
        }
        if l.key_color_y > 0.0 {
            text(
                scene,
                tr,
                tf,
                "Key colour",
                label_size,
                c.text_dim,
                x0,
                l.key_color_y,
            );
        }
        for (col, rect) in &l.key_colors {
            let on = clip
                .key
                .color
                .iter()
                .zip(col)
                .all(|(a, b)| (a - b).abs() < 1e-6);
            let swatch = Color::new([col[0] as f32, col[1] as f32, col[2] as f32, 1.0]);
            ui::surface::card(scene, tf, *rect, 5.0, swatch);
            // The chosen swatch takes a ring rather than a tick, so the
            // colour it names is never covered by the mark that selects it.
            if on {
                stroke_round(scene, tf, rect.inflate(2.0, 2.0), 7.0, c.text_primary, 2.0);
            } else if rect.contains(self.last_mouse) {
                stroke_round(scene, tf, rect.inflate(1.0, 1.0), 6.0, c.border_focus, 1.0);
            }
        }
        if let Some(rect) = l.key_pick {
            // Sampling needs a picture to sample: with no decoded frame the
            // button says so rather than quietly handing back a colour the
            // footage never had.
            let have = self.preview_pixel(0.5, 0.5).is_some();
            draw_button_enabled(
                &mut PaintCtx::new(scene, tf, tr, theme),
                rect,
                if have {
                    "Sample centre"
                } else {
                    "No frame to sample"
                },
                false,
                rect.contains(self.last_mouse),
                have,
            );
        }
        // ---- mask -------------------------------------------------------
        if !l.mask_shapes.is_empty() {
            let rects: Vec<Rect> = l.mask_shapes.iter().map(|(_, r)| *r).collect();
            let labels: Vec<&str> = l.mask_shapes.iter().map(|(s, _)| s.label()).collect();
            let sel = l
                .mask_shapes
                .iter()
                .position(|(s, _)| *s == clip.mask.shape)
                .unwrap_or(usize::MAX);
            draw_segmented(scene, tf, tr, theme, &rects, &labels, sel, self.last_mouse);
        }
        if let Some(rect) = l.mask_fit {
            draw_button(
                scene,
                tf,
                tr,
                theme,
                rect,
                "Fit to picture",
                false,
                rect.contains(self.last_mouse),
            );
        }
        if let Some(rect) = l.mask_invert {
            let on = clip.mask.invert;
            draw_button(
                scene,
                tf,
                tr,
                theme,
                rect,
                if on { "Keeping outside" } else { "Invert" },
                on,
                rect.contains(self.last_mouse),
            );
        }
        // ---- title ----------------------------------------------------
        if let Some(rect) = l.title_text {
            let text_src = media
                .as_ref()
                .and_then(|m| m.text.clone())
                .unwrap_or_default();
            text(
                scene,
                tr,
                tf,
                "Words",
                label_size,
                c.text_dim,
                rect.x0,
                rect.y0 - 16.0,
            );
            let editing = self
                .title_edit
                .as_ref()
                .map(|(m, _)| Some(*m) == media.as_ref().map(|m| m.id));
            // A field: the well with its derived rest edge, and the focus
            // ring over it while the inline editor owns the words.
            well(scene, tf, theme, rect, 6.0);
            if editing == Some(true) {
                stroke_round(scene, tf, rect, 6.0, c.border_focus, FOCUS_RING_W);
            }
            // While the inline editor owns this box it draws the words itself
            // (with a caret); otherwise show them, or say plainly that there
            // are none rather than leaving an empty box that looks broken.
            if editing == Some(true) {
                // While the field is open, say what ends the edit — Enter is
                // a line break here, so it cannot also be Accept.
                text(
                    scene,
                    tr,
                    tf,
                    "Ctrl+Enter to accept · Esc to cancel",
                    10.5,
                    c.text_dim,
                    rect.x0,
                    rect.y1 + 14.0,
                );
            } else if text_src.content.trim().is_empty() {
                let baseline = text_baseline(rect, 13.0, tr);
                text(
                    scene,
                    tr,
                    tf,
                    "Click to type a title",
                    13.0,
                    c.text_dim,
                    rect.x0 + 10.0,
                    baseline,
                );
            } else {
                // Real line breaks, on their own lines. The field used to
                // fold them into a `⏎` glyph because it could not carry
                // one; it can now, so showing the words as written is
                // both honest and what the frame will look like.
                for (i, line) in text_src.content.lines().take(3).enumerate() {
                    let shown = elide_w(line, (rect.width() - 20.0) as f32, tr);
                    text(
                        scene,
                        tr,
                        tf,
                        &shown,
                        13.0,
                        c.text_primary,
                        rect.x0 + 10.0,
                        rect.y0 + 18.0 + i as f64 * 18.0,
                    );
                }
                if text_src.content.lines().count() > 3 {
                    text(
                        scene,
                        tr,
                        tf,
                        "…",
                        13.0,
                        c.text_dim,
                        rect.x0 + 10.0,
                        rect.y0 + 18.0 + 3.0 * 18.0,
                    );
                }
            }
        }
        if !l.title_align.is_empty() {
            let cur = media
                .as_ref()
                .and_then(|m| m.text.as_ref())
                .map(|t| t.align)
                .unwrap_or_default();
            let rects: Vec<Rect> = l.title_align.iter().map(|(_, r)| *r).collect();
            let labels: Vec<&str> = l.title_align.iter().map(|(a, _)| a.label()).collect();
            let sel = l
                .title_align
                .iter()
                .position(|(a, _)| *a == cur)
                .unwrap_or(0);
            draw_segmented(scene, tf, tr, theme, &rects, &labels, sel, self.last_mouse);
        }
        if let Some(rect) = l.title_font {
            let current = media
                .as_ref()
                .and_then(|m| m.text.as_ref())
                .map(|t| t.font.clone())
                .unwrap_or_default();
            text(
                scene,
                tr,
                tf,
                "Font",
                label_size,
                c.text_dim,
                rect.x0,
                rect.y0 - 16.0,
            );
            // The open list is drawn later, over the panel, because it floats
            // above everything under it. Here is the resting control: a
            // button naming the family in force. "The system's default sans"
            // is a real answer, not a missing one, so it is named rather than
            // left as an empty field.
            if self.font_picker.is_none() {
                let label = if current.trim().is_empty() {
                    Self::DEFAULT_FONT_LABEL.to_string()
                } else {
                    current
                };
                draw_button(
                    scene,
                    tf,
                    tr,
                    theme,
                    rect,
                    &label,
                    false,
                    rect.contains(self.last_mouse),
                );
            }
        }
        if !l.title_colors.is_empty() {
            let cur = media
                .as_ref()
                .and_then(|m| m.text.as_ref())
                .map_or([1.0; 4], |t| t.color);
            text(
                scene,
                tr,
                tf,
                "Colour",
                label_size,
                c.text_dim,
                x0,
                l.title_colors[0].1.y0 - 16.0,
            );
            for (col, rect) in &l.title_colors {
                let sw = title_color(*col);
                // A swatch can be white, near-black or translucent, so it is
                // drawn on a chequer-free plate with a border: without one the
                // white swatch vanishes on a light theme and the dark one on a
                // dark theme.
                // A plate under a swatch that may be translucent, then the
                // swatch with an edge derived against its own colour — so the
                // white swatch shows on a light pane and the black one on a
                // dark pane without a theme-blind ink wash.
                fill_round(scene, tf, *rect, 6.0, c.lift(0.35));
                ui::surface::card(scene, tf, *rect, 6.0, sw);
                if col
                    .iter()
                    .zip(cur.iter())
                    .all(|(a, b)| (a - b).abs() < 1e-6)
                {
                    stroke_round(scene, tf, rect.inset(1.5), 7.0, c.border_focus, 2.0);
                } else if rect.contains(self.last_mouse) {
                    stroke_round(scene, tf, *rect, 6.0, with_alpha(c.text_primary, 0.6), 1.5);
                }
            }
        }
        for (rect, on, label) in [
            (
                l.title_bold,
                media
                    .as_ref()
                    .and_then(|m| m.text.as_ref())
                    .is_some_and(|t| t.bold),
                "Bold",
            ),
            (
                l.title_italic,
                media
                    .as_ref()
                    .and_then(|m| m.text.as_ref())
                    .is_some_and(|t| t.italic),
                "Italic",
            ),
        ] {
            let Some(rect) = rect else { continue };
            draw_button(
                scene,
                tf,
                tr,
                theme,
                rect,
                label,
                on,
                rect.contains(self.last_mouse),
            );
        }
        self.draw_marker_rows(scene, tf, tr, theme, &l, x0);
        self.draw_project_section(scene, tf, tr, theme, &l, x0);
        // ---- key controls ---------------------------------------------
        // Drawn last so a diamond sits over its row rather than under it.
        for (param, rect) in &l.keys {
            draw_key_control(
                scene,
                tf,
                theme,
                *rect,
                self.key_state(*param),
                rect.contains(self.last_mouse),
            );
        }
        // Effects stack (video only).
        if is_video {
            for (i, row) in l.effect_rows.iter().enumerate() {
                let Some(e) = clip.effects.get(i) else {
                    continue;
                };
                let (_key, up, down, slider, remove) = Self::effect_row_parts(*row);
                well(scene, tf, theme, *row, 6.0);
                // Reorder / remove: real icons on hover-lit hit targets, sized to
                // the pointer rather than to the glyph. These were raw Unicode
                // arrows in 16x18px boxes with no hover state at all.
                let last = i + 1 == clip.effects.len();
                for (rect, icon, enabled) in [
                    (up, ui::icons::Icon::ChevronUp, i > 0),
                    (down, ui::icons::Icon::ChevronDown, !last),
                ] {
                    selection_row(
                        scene,
                        tf,
                        theme,
                        rect,
                        4.0,
                        ui::chrome::SelectionState::new(
                            false,
                            enabled && rect.contains(self.last_mouse),
                            false,
                        ),
                    );
                    let col = if enabled {
                        c.text_primary
                    } else {
                        c.text_tertiary
                    };
                    let g = 12.0;
                    let box_ = Rect::new(
                        rect.center().x - g / 2.0,
                        rect.center().y - g / 2.0,
                        rect.center().x + g / 2.0,
                        rect.center().y + g / 2.0,
                    );
                    ui::icons::draw_icon(
                        scene,
                        tf,
                        box_,
                        icon,
                        col,
                        ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                    );
                }
                // Name.
                let name_y = text_baseline(*row, 12.0, tr);
                text_w(
                    scene,
                    tr,
                    tf,
                    e.kind.label(),
                    &TextStyle::new(12.0).with_weight(TextWeight::Medium),
                    c.text_primary,
                    Point::new(down.x1 + 8.0, name_y),
                );
                // Intensity slider.
                draw_track(
                    scene,
                    tf,
                    theme,
                    slider,
                    e.amount,
                    hit_slider(slider, self.last_mouse),
                    matches!(self.drag, Some(Drag::EffectAmount { idx, .. }) if idx == i),
                );
                // Remove.
                selection_row(
                    scene,
                    tf,
                    theme,
                    remove,
                    4.0,
                    ui::chrome::SelectionState::new(false, remove.contains(self.last_mouse), false),
                );
                let g = 12.0;
                let xbox = Rect::new(
                    remove.center().x - g / 2.0,
                    remove.center().y - g / 2.0,
                    remove.center().x + g / 2.0,
                    remove.center().y + g / 2.0,
                );
                ui::icons::draw_icon(
                    scene,
                    tf,
                    xbox,
                    ui::icons::Icon::X,
                    c.semantic.danger,
                    ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
            // Add-effect button, through the shared face like every other
            // button in the panel. It was a local fill-and-stroke with its
            // label pinned to a font-size offset, and read as a different
            // control from the "+ Marker at playhead" button two rows down.
            draw_button(
                scene,
                tf,
                tr,
                theme,
                l.add_effect,
                "+ Add effect",
                false,
                l.add_effect.contains(self.last_mouse),
            );
        }

        if l.valid {
            draw_button(
                scene,
                tf,
                tr,
                theme,
                l.split,
                "Split",
                false,
                l.split.contains(self.last_mouse),
            );
            draw_danger_button(
                scene,
                tf,
                tr,
                theme,
                l.delete,
                "Delete",
                l.delete.contains(self.last_mouse),
            );
        }
    }

    /// One collapsible section header: a fold arrow, the name, and — when it
    /// is folded — a summary of what is inside it.
    ///
    /// The summary is the point. Folding a section away is a view choice, but
    /// a fold that hid "this clip plays at 4x in reverse" would turn a view
    /// choice into missing state, and the user would have no way to know the
    /// clip was re-timed without unfolding every section on every clip.
    fn draw_section_header(
        &self,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        section: Section,
        clip: &Clip,
        media: Option<&MediaItem>,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let open = !self.collapsed.contains(&section);
        selection_row(
            ctx.scene,
            ctx.transform,
            theme,
            rect,
            5.0,
            ui::chrome::SelectionState::new(false, rect.contains(self.last_mouse), false),
        );
        let g = 11.0;
        let arrow = Rect::new(
            rect.x0 + 1.0,
            rect.center().y - g / 2.0,
            rect.x0 + 1.0 + g,
            rect.center().y + g / 2.0,
        );
        ui::icons::draw_icon(
            ctx.scene,
            ctx.transform,
            arrow,
            if open {
                ui::icons::Icon::ChevronDown
            } else {
                ui::icons::Icon::ChevronRight
            },
            c.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        let baseline = text_baseline(rect, 11.0, ctx.text);
        text_w(
            ctx.scene,
            ctx.text,
            ctx.transform,
            section.label(),
            &TextStyle::new(11.0).with_weight(TextWeight::Semibold),
            c.text_dim,
            Point::new(rect.x0 + 18.0, baseline),
        );
        if open {
            return;
        }
        let Some(summary) = self.section_summary(section, clip, media) else {
            return;
        };
        let (w, _) = ctx.text.measure(&summary, 11.0, None);
        // A summary that reports something other than the default is worth
        // reading, so it takes the accent rather than the dim tone.
        text(
            ctx.scene,
            ctx.text,
            ctx.transform,
            &summary,
            11.0,
            c.accent[5],
            rect.x1 - f64::from(w),
            baseline,
        );
    }

    /// What a folded section reports about itself, or `None` when everything
    /// inside it is at its default and there is nothing to report.
    #[allow(
        clippy::indexing_slicing,
        reason = "section summaries read the paired fixed labels and values that their section builder created"
    )]
    pub(super) fn section_summary(
        &self,
        section: Section,
        clip: &Clip,
        media: Option<&MediaItem>,
    ) -> Option<String> {
        match section {
            Section::Curves => curve_summary(clip, self.playhead),
            Section::Transform => {
                // The geometry the clip is *playing*, not the constants under
                // it: a keyframe supersedes the constant for every other
                // reader, so a folded header reading `transform.scale_x`
                // reported 1.00× beside a canvas that was pushing in.
                let g = self.geometry_at_playhead(clip);
                // `is_identity()` on the constants alone was the whole gate,
                // and keying a parameter from its default leaves that constant
                // exactly at the aspect-fit — so a clip whose picture swings
                // across the frame between two keys reported nothing at all.
                let animated = Self::transform_is_animated(clip);
                // `is_identity()` still carries the crop and the anchor, which
                // the model does not animate; the two new disjuncts are what it
                // could never see — a keyed channel away from its rest value,
                // and the existence of a curve at all.
                if clip.transform.is_identity() && !g.is_placed() && !animated {
                    return None;
                }
                let mut parts = Vec::new();
                if (g.scale_x - 1.0).abs() > 1e-6 || (g.scale_y - 1.0).abs() > 1e-6 {
                    parts.push(format!("{:.2}×", g.scale_x));
                }
                if g.rotation.abs() > 1e-6 {
                    parts.push(format!("{:.0}°", g.rotation));
                }
                if clip.transform.is_cropped() {
                    parts.push("cropped".into());
                }
                if animated {
                    parts.push("animated".into());
                }
                if parts.is_empty() {
                    parts.push("moved".into());
                }
                Some(parts.join(" · "))
            }
            Section::Speed => (!clip.speed.is_normal()).then(|| clip.speed.label()),
            // The lane's fader, and whether it has been caught clipping. The
            // second is the part that must survive a fold: hiding the warning
            // because the panel is tidied away is exactly the silence this
            // section exists to end.
            Section::Mix => {
                let t = self.project.track(clip.track)?;
                let clipped = if self.mix.track(t.id).is_some_and(|level| level.clipped) {
                    " · CLIP"
                } else {
                    ""
                };
                if !t.mix_is_set() && clipped.is_empty() {
                    return None;
                }
                Some(format!("{} {}{clipped}", t.gain_words(), t.pan_words()))
            }
            // A key that is switched on but keying nothing still has to
            // say so: "on" and "removing something" are different states,
            // and a fold that reported neither would hide both. The tolerance
            // is read at the playhead, because it is animatable: a key ramping
            // on read as "keys nothing" for the whole clip.
            Section::Key => clip.key.enabled.then(|| {
                let tolerance = clip.param_at(Param::KeyTolerance, self.playhead);
                if tolerance <= 1e-6 {
                    "on · keys nothing".to_string()
                } else {
                    format!("keyed {:.0}%", tolerance * 100.0)
                }
            }),
            Section::Mask => clip.mask.is_active().then(|| {
                let shape = clip.mask.shape.label().to_lowercase();
                if clip.mask.invert {
                    format!("{shape} · inverted")
                } else {
                    shape
                }
            }),
            Section::Title => {
                let t = media?.text.as_ref()?;
                let first = t.content.lines().next().unwrap_or("").trim();
                Some(if first.is_empty() {
                    "no words yet".into()
                } else {
                    first.to_string()
                })
            }
            Section::Effects => (!clip.effects.is_empty()).then(|| {
                if clip.effects.len() == 1 {
                    clip.effects[0].kind.label().to_string()
                } else {
                    format!("{} effects", clip.effects.len())
                }
            }),
            Section::Markers => {
                let n = self.project.all_markers().len();
                (n > 0).then(|| {
                    if n == 1 {
                        "1 marker".into()
                    } else {
                        format!("{n} markers")
                    }
                })
            }
            // A folded PROJECT header is where the frame most needs stating:
            // the format is a fact about every clip below it, and this is the
            // one line that carries it when the section is away.
            Section::Project => Some(self.project_format_shown.clone()),
        }
    }
}
