#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;

/// A control's label above it, and the readout beside it: `Label`, the role
/// the system sets every control label in.
fn label_style(theme: &Theme) -> TextStyle {
    theme.text(TextRole::Label)
}

/// One caption line above a control, its line box's top `line_h` plus a
/// tight step above `y`, where `y` is the top of the control it labels.
fn caption_above(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    tf: Affine,
    theme: &Theme,
    words: &str,
    x: f64,
    y: f64,
) {
    let style = label_style(theme);
    let h = line_height(tr, &style);
    text(
        scene,
        tr,
        tf,
        words,
        &style,
        theme.tokens.colors.text_dim,
        x,
        y - h - S1,
    );
}

impl VideoEditorApp {
    /// Draw the tone-curve editor: the grid, the "unchanged" diagonal, the
    /// curve, and its control points.
    ///
    /// This is canvas drawing rather than a toolkit widget for the same
    /// reason the timeline and the waveform are: a 2D control-point editor
    /// is content, not a control the system shares. Everything around it —
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
        well(
            scene,
            tf,
            theme,
            canvas,
            f64::from(theme.tokens.radii.control),
        );
        let ground = c.surfaces.raised;

        // Quarter grid. Faint, because it is a reading aid and the curve is
        // the content.
        let grid = ui::surface::edge_on(ground);
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
            ui::surface::edge_on_with(ground, ui::chrome::PANEL_BORDER_ALPHA),
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
                if live { c.text_primary } else { ground },
                None,
                &Circle::new(at, r),
            );
        }

        // What the gesture is, stated rather than left to be discovered. It
        // sits under the box so it never covers the curve.
        let hint = if dragging.is_some() {
            "Drag out of the box to remove"
        } else {
            "Click to add a point, drag to shape it"
        };
        let style = theme.text(TextRole::Caption);
        text(
            scene,
            tr,
            tf,
            hint,
            &style,
            c.text_dim,
            canvas.x0,
            canvas.y1 + S2,
        );
    }

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
            let style = theme.text(TextRole::Body).numeric();
            text(
                scene,
                tr,
                tf,
                &self.project_format_shown,
                &style,
                c.text_primary,
                x0,
                l.project_format_y,
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
        if let Some((_, first)) = l.project_rates.first() {
            caption_above(scene, tr, tf, theme, "Frame rate", x0, first.y0);
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
            draw_button(
                scene,
                tf,
                tr,
                theme,
                rect,
                &label,
                ButtonRole::Secondary,
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
        let name_style = theme.text(TextRole::Label);
        let stamp_style = theme.text(TextRole::Caption).numeric();
        let note_style = theme.text(TextRole::Caption);
        for (m, row) in &l.marker_rows {
            let (name_rect, note_btn, rename, del) = marker_row_parts(*row);
            let hovered = row.contains(self.last_mouse);
            selection_row(
                scene,
                tf,
                theme,
                *row,
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
            let first_line = Rect::new(row.x0, row.y0, row.x1, name_rect.y1);
            let col = clip_color(theme, mk.palette);
            scene.fill(
                Fill::NonZero,
                tf,
                col,
                None,
                &Circle::new(Point::new(row.x0 + 7.0, first_line.center().y), 4.0),
            );
            let editing = self.marker_rename.as_ref().map(|(r, _)| r.id) == Some(m.id);
            let renaming = editing && self.marker_field == MarkerField::Name;
            if !renaming {
                let name = if mk.name.trim().is_empty() {
                    "Marker"
                } else {
                    mk.name.as_str()
                };
                let shown = elide_at(name, name_rect.width() as f32, &name_style, tr);
                let top = text_baseline(first_line, &name_style, tr);
                text(
                    scene,
                    tr,
                    tf,
                    &shown,
                    &name_style,
                    c.text_primary,
                    name_rect.x0,
                    top,
                );
                // A clip marker says which clip it travels with, because
                // moving that clip moves the marker and nothing else would
                // explain why the time changed.
                let stamp = timecode(m.at, self.project.fps);
                let sw = text_width(tr, &stamp, &stamp_style);
                let stamp_col = if m.clip.is_some() {
                    c.text_tertiary
                } else {
                    c.text_dim
                };
                let stamp_top = text_baseline(first_line, &stamp_style, tr);
                text(
                    scene,
                    tr,
                    tf,
                    &stamp,
                    &stamp_style,
                    stamp_col,
                    note_btn.x0 - S2 - sw,
                    stamp_top,
                );
            }
            // The note, on its own line under the name. Present only when
            // there is one, so an unannotated list stays a list of names.
            let editing_note = editing && self.marker_field == MarkerField::Note;
            if !mk.note.trim().is_empty() && !editing_note {
                let shown = elide_at(mk.note.trim(), (row.width() - 34.0) as f32, &note_style, tr);
                let line = Rect::new(row.x0 + 16.0, name_rect.y1, row.x1, row.y1);
                let top = text_baseline(line, &note_style, tr);
                text(
                    scene,
                    tr,
                    tf,
                    &shown,
                    &note_style,
                    c.text_tertiary,
                    line.x0,
                    top,
                );
            }
            // Note, rename and delete only appear under the pointer, so a
            // resting list reads as names and times rather than as a wall of
            // icons.
            if hovered {
                for (rect, icon, name) in [
                    (note_btn, Icon::FileText, "Edit note"),
                    (rename, Icon::Pencil, "Rename"),
                    (del, Icon::Trash2, "Delete"),
                ] {
                    draw_icon_button(
                        &mut PaintCtx::new(scene, tf, tr, theme),
                        rect,
                        icon,
                        name,
                        ButtonRole::Plain,
                        rect.contains(self.last_mouse),
                        true,
                    );
                }
            }
        }
        if let Some(rect) = l.add_marker {
            if l.marker_rows.is_empty() {
                caption_above(scene, tr, tf, theme, "Nothing marked yet.", x0, rect.y0);
            }
            let face = ButtonFace::new(rect, ADD_MARKER_LABEL)
                .icon(Icon::Plus)
                .control_size(control_size_for(rect, theme))
                .hovered(rect.contains(self.last_mouse));
            Button::paint(scene, tf, tr, theme, &face);
        }
    }

    /// The inspector's ground — the inspector region docked, the floating
    /// material when it is summoned over a narrow window — and the theme for
    /// what is drawn on it.
    fn paint_inspector_panel(scene: &mut Scene, tf: Affine, theme: &Theme, r: &Regions) -> Theme {
        if r.inspector_overlay {
            bin_paint::paint_floating_panel(scene, tf, r.inspector, theme)
        } else {
            ui::surface::region(
                scene,
                tf,
                r.inspector,
                ui::surface::Region::Inspector,
                theme,
            );
            theme.in_region(ui::surface::Region::Inspector)
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
        let panel = Self::paint_inspector_panel(scene, tf, theme, r);
        let theme = &panel;
        let ground = theme.tokens.colors.surfaces.content;
        let clip_shape = if r.inspector_overlay {
            RoundedRect::from_rect(r.inspector, f64::from(theme.tokens.radii.panel))
        } else {
            RoundedRect::from_rect(r.inspector, 0.0)
        };
        // Clip everything to the inspector so controls never bleed into the
        // timeline if the selected clip's control stack runs tall.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            tf,
            &clip_shape,
        );
        // The scroll body gets its own clip, below the fixed header and pulled
        // in to whole rows, so a row scrolled part-way out is left out rather
        // than drawn sliced under the header or at the panel's foot.
        let body_view = self.inspector_shown_body(r, &self.inspector_layout(r));
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            tf,
            &body_view,
        );
        self.draw_inspector_body(scene, tf, tr, theme, r);
        // A clip on a locked track has inert controls — every edit path checks
        // the lock — so they are veiled rather than left looking live. The
        // header above stays crisp, because that is where the reason is
        // written ("On V2, locked").
        let locked = self
            .selected()
            .is_some_and(|id| !self.project.clip_editable(id));
        if locked {
            fill_rect(scene, tf, body_view, with_alpha(ground, 0.6));
        }
        // The keyboard cursor, drawn over the lock veil rather than under it:
        // where the keyboard *is* stays legible even where the controls are
        // inert, because the walk still visits them.
        if self.shows_focus_cursor(Pane::Inspector) {
            let rows = self.a11y_inspector_rows();
            if let Some(row) = rows.get(self.focus.inspector).copied()
                && let Some(band) = Self::inspector_focus_band(&self.inspector_layout(r), row)
                && band.intersect(body_view).area() > 0.0
            {
                draw_focus_ring(scene, tf, theme, band, f64::from(theme.tokens.radii.item));
            }
        }
        scene.pop_layer();
        self.draw_inspector_fixed_header(scene, tf, tr, theme, r);
        scene.pop_layer();
        // Real, grabbable scrollbar.
        self.draw_scrollbar(scene, tf, theme, ScrollPane::Inspector, r);
    }

    #[expect(
        clippy::many_single_char_names,
        reason = "the paint pass's shorthands: colours c, regions r, the panel's scale s and the y pen"
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
        let header = self.inspector_header(r);
        if header.height <= 0.0 {
            return;
        }
        let clip = self.selected().and_then(|id| self.project.clip(id));
        let s = self.inspector_ui_scale();
        let header_rect = Rect::new(
            r.inspector.x0,
            r.inspector.y0,
            r.inspector.x1,
            r.inspector.y0 + header.height,
        );
        // Clipped to its own measured extent: whatever the window height, the
        // header never reaches into the body under it.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            tf,
            &header_rect,
        );
        let x0 = r.inspector.x0 + PAD * s;
        let x1 = r.inspector.x1 - PAD * s;
        let mut y = r.inspector.y0 + S3;
        let name_style = theme.text(TextRole::Heading);
        let name_h = line_height(tr, &name_style);
        let sub_style = theme.text(TextRole::Caption);
        let Some(clip) = clip else {
            // The empty state: what is (not) selected, and the one thing to
            // do about it.
            let shown = elide_at(INSPECTOR_EMPTY_TITLE, (x1 - x0) as f32, &name_style, tr);
            text(scene, tr, tf, &shown, &name_style, c.text_dim, x0, y);
            y += name_h + S1;
            let shown = elide_at(INSPECTOR_EMPTY_HINT, (x1 - x0) as f32, &sub_style, tr);
            text(scene, tr, tf, &shown, &sub_style, c.text_dim, x0, y);
            scene.pop_layer();
            return;
        };
        let name = self
            .project
            .media(clip.media)
            .map_or("Missing media", |item| item.name.as_str());
        let name_x = x0;
        let shown = elide_at(name, (x1 - name_x) as f32, &name_style, tr);
        text(
            scene,
            tr,
            tf,
            &shown,
            &name_style,
            c.text_primary,
            name_x,
            y,
        );
        y += name_h + S1;
        let track_name = self
            .project
            .track(clip.track)
            .map_or("its track", |track| track.name.as_str());
        let n = self.selection.len();
        let sub = match (n > 1, self.selection_has_locked()) {
            (true, true) => format!("{n} clips selected, some locked"),
            (true, false) => format!("{n} clips selected"),
            (false, true) => format!("On {track_name}, locked"),
            (false, false) => format!("On {track_name}"),
        };
        let sub_col = if self.selection_has_locked() {
            c.semantic.warning
        } else {
            c.text_dim
        };
        let shown = elide_at(&sub, (x1 - name_x) as f32, &sub_style, tr);
        text(scene, tr, tf, &shown, &sub_style, sub_col, name_x, y);
        scene.pop_layer();
        // Where the fixed part ends and the scrolling part begins.
        ui::surface::separator(
            scene,
            tf,
            r.inspector.x0,
            r.inspector.x1,
            header_rect.y1,
            0.0,
            c.surfaces.content,
        );
    }

    #[allow(
        clippy::indexing_slicing,
        clippy::too_many_lines,
        reason = "inspector controls index fixed option tables and share one scroll coordinate system with their validated hit targets"
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
        let label = label_style(theme);
        let readout_style = label_style(theme).numeric();
        let label_h = line_height(tr, &label);

        let selected = self
            .selected()
            .and_then(|id| self.project.clip(id).cloned());
        let Some(clip) = selected else {
            // The markers still belong to the project, so they stay reachable
            // with nothing selected. Everything else here edits a clip.
            let l = self.inspector_layout(r);
            let blank = Clip::default();
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

        let l = self.inspector_layout(r);
        // The clip's facts — where it sits and what it sources — as the
        // toolkit's property list, at the head of the body.
        if let Some(rect) = l.properties {
            let values = Self::inspector_property_values(&clip, self.project.fps);
            let rows = inspector_property_rows(&values);
            ui::widget::PropertyListFace::new(&rows)
                .paint(&mut PaintCtx::new(scene, tf, tr, theme), rect);
        }
        // Every slider row: a label, a right-aligned readout measured like all
        // the others, and the shared slider face carrying hover / drag state.
        // The label line sits clear above the knob's reach.
        let label_top = |track: Rect| track.y0 - S1 - label_h;
        let row = |scene: &mut Scene,
                   tr: &mut TextRenderer,
                   track: Option<Rect>,
                   words: &str,
                   readout: String,
                   v: f64,
                   drag: bool,
                   // `Some` when this row carries a key control, whose box
                   // the label starts after. The indent is unconditional
                   // so keying a parameter does not shuffle its row
                   // sideways; an animated one reads in primary ink.
                   keyed: Option<bool>| {
            let Some(track) = track else { return };
            let lx = if keyed.is_some() {
                track.x0 + KEY_BOX + S1
            } else {
                track.x0
            };
            let col = if keyed == Some(true) {
                c.text_primary
            } else {
                c.text_dim
            };
            let top = label_top(track);
            text(scene, tr, tf, words, &label, col, lx, top);
            let pw = text_width(tr, &readout, &readout_style);
            text(
                scene,
                tr,
                tf,
                &readout,
                &readout_style,
                c.text_primary,
                track.x1 - pw,
                top,
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
            dragging(self.drag, Drag::Volume),
            Some(animated(Param::Volume)),
        );
        // Colour-grade rows. The readout shows the multiplier (0.5 == 1.00×,
        // the identity).
        for (track, words, p, d) in [
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
                words,
                format!("{:.2}×", Filters::gain(v)),
                v,
                dragging(self.drag, d),
                Some(animated(p)),
            );
        }
        // Fade in / out (seconds). Two half-width tracks share a row.
        let cd = clip.duration().max(1e-6);
        row(
            scene,
            tr,
            l.fade_in,
            "Fade in",
            format!("{:.1}s", clip.fade_in),
            (clip.fade_in / cd).clamp(0.0, 1.0),
            dragging(self.drag, Drag::FadeIn),
            None,
        );
        row(
            scene,
            tr,
            l.fade_out,
            "Fade out",
            format!("{:.1}s", clip.fade_out),
            (clip.fade_out / cd).clamp(0.0, 1.0),
            dragging(self.drag, Drag::FadeOut),
            None,
        );
        // Transition segmented control, through the shared painter.
        if let Some((_, first)) = l.transitions.first() {
            caption_above(scene, tr, tf, theme, "Transition", x0, first.y0);
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
                self.drag == Some(Drag::TransitionLen),
                None,
            );
        }
        // Label colour swatches: the toolkit's swatch face, ringed when chosen.
        if let Some((_, first)) = l.colors.first() {
            caption_above(scene, tr, tf, theme, "Label", x0, first.y0);
            for (pal, rect) in &l.colors {
                let face =
                    ui::widget::ColorSwatchFace::new(*rect, clip_color(theme, *pal), "Label")
                        .selected(clip.palette == *pal)
                        .hovered(rect.contains(self.last_mouse));
                face.paint(scene, tf, tr, theme);
            }
        }
        // Link / unlink. The caption names the state so the button can name
        // the *change*: a control labelled only "Unlink" leaves a reader
        // guessing whether it describes what is true or what will happen.
        if let (Some(rect), Some(offer)) = (l.link, self.link_affordance()) {
            caption_above(scene, tr, tf, theme, offer.state(), x0, rect.y0);
            draw_button(
                scene,
                tf,
                tr,
                theme,
                rect,
                offer.label(),
                ButtonRole::Secondary,
                rect.contains(self.last_mouse),
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
            let keyed = f.param().map(|p| self.key_state(p) != KeyState::Off);
            row(
                scene,
                tr,
                Some(*track),
                f.label(),
                f.readout(v),
                f.to_frac(v),
                self.drag == Some(Drag::Num(*f)),
                keyed,
            );
        }
        // Picture-in-picture presets: each button carries a miniature frame
        // with the inset drawn in the corner it puts the picture in, which is
        // the whole of what the button does.
        if !l.pip.is_empty() {
            let top = l.pip[0].2;
            caption_above(scene, tr, tf, theme, "Picture in picture", x0, top.y0);
            let inset = crate::model::Transform::pip(PIP_SCALE, (1.0, -1.0), PIP_INSET);
            let is_pip = (clip.transform.scale_x - inset.scale_x).abs() < 1e-6;
            for (_, corner, rect) in &l.pip {
                let on = is_pip
                    && (clip.transform.pos_x.signum() - corner.0).abs() < 1e-6
                    && (clip.transform.pos_y.signum() - corner.1).abs() < 1e-6;
                let hovered = rect.contains(self.last_mouse);
                draw_toggle_button(
                    &mut PaintCtx::new(scene, tf, tr, theme),
                    *rect,
                    ToggleContent::Label(""),
                    on,
                    hovered,
                );
                let fg = if on { c.text_primary } else { c.text_dim };
                let g = Rect::new(
                    rect.center().x - 11.0,
                    rect.center().y - 7.0,
                    rect.center().x + 11.0,
                    rect.center().y + 7.0,
                );
                stroke_round(scene, tf, g, 2.0, fg, 1.0);
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
                &label,
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
                &label,
                c.text_dim,
                x0,
                l.anchor_label_y,
            );
        }
        // Three segmented rows make the 3×3 grid, so each row is a real
        // shared toggle group rather than nine hand-drawn cells. Each cell
        // carries a miniature frame with a dot where the anchor sits, as the
        // picture-in-picture presets carry theirs: the picture of what the
        // choice does, rather than an arrow standing in for it.
        for band in l.anchor.chunks(3) {
            let rects: Vec<Rect> = band.iter().map(|(_, r)| *r).collect();
            let labels = ["", "", ""];
            let sel = band
                .iter()
                .position(|(a, _)| {
                    (clip.transform.anchor_x - a.0).abs() < 1e-6
                        && (clip.transform.anchor_y - a.1).abs() < 1e-6
                })
                .unwrap_or(usize::MAX);
            draw_segmented(
                scene,
                tf,
                tr,
                theme,
                &rects,
                &labels[..rects.len().min(3)],
                sel,
                self.last_mouse,
            );
            for (i, (a, rect)) in band.iter().enumerate() {
                let ink = if i == sel { c.text_primary } else { c.text_dim };
                let frame = Rect::from_center_size(rect.center(), (18.0, 12.0));
                stroke_round(scene, tf, frame, 2.0, ink, 1.0);
                let dot = Point::new(
                    frame.x0 + 2.5 + a.0 * (frame.width() - 5.0),
                    frame.y0 + 2.5 + a.1 * (frame.height() - 5.0),
                );
                // A dot: the detail radius rounds the 4px square fully.
                fill_round(
                    scene,
                    tf,
                    Rect::from_center_size(dot, (4.0, 4.0)),
                    f64::from(theme.tokens.radii.detail),
                    ink,
                );
            }
        }
        if let Some(rect) = l.reset_transform {
            let plain = clip.transform.is_identity();
            draw_button_enabled(
                &mut PaintCtx::new(scene, tf, tr, theme),
                rect,
                "Fill frame",
                ButtonRole::Secondary,
                rect.contains(self.last_mouse),
                !plain,
            );
        }
        // Rate presets + the reverse switch.
        if !l.speed_presets.is_empty() {
            let rects: Vec<Rect> = l.speed_presets.iter().map(|(_, r)| *r).collect();
            let labels: Vec<String> = l
                .speed_presets
                .iter()
                .map(|(rate, _)| speed_words(*rate))
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
            draw_switch_row(
                &mut PaintCtx::new(scene, tf, tr, theme),
                rect,
                "Play backwards",
                clip.speed.reverse,
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
                    0 => format!("Lane {}, this clip only", t.name),
                    1 => format!("Lane {}, with 1 other clip", t.name),
                    n => format!("Lane {}, with {n} other clips", t.name),
                };
                let words = elide_at(&words, (x1 - x0) as f32, &label, tr);
                text(scene, tr, tf, &words, &label, c.text_dim, x0, l.mix_track_y);
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
                ButtonRole::Secondary,
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
                ButtonRole::Secondary,
                rect.contains(self.last_mouse),
                shaped,
            );
        }
        // ---- colour key -----------------------------------------------
        if let Some(rect) = l.key_enable {
            draw_switch_row(
                &mut PaintCtx::new(scene, tf, tr, theme),
                rect,
                "Key a colour out",
                clip.key.enabled,
                rect.contains(self.last_mouse),
            );
        }
        if l.key_color_y > 0.0 {
            text(
                scene,
                tr,
                tf,
                "Key colour",
                &label,
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
            // The key colour is the footage's own colour, so it is shown as it
            // is, on the toolkit's swatch.
            let swatch = Color::new([col[0] as f32, col[1] as f32, col[2] as f32, 1.0]);
            let face = ui::widget::ColorSwatchFace::new(*rect, swatch, "Key colour")
                .selected(on)
                .hovered(rect.contains(self.last_mouse));
            face.paint(scene, tf, tr, theme);
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
                ButtonRole::Secondary,
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
                ButtonRole::Secondary,
                rect.contains(self.last_mouse),
            );
        }
        if let Some(rect) = l.mask_invert {
            draw_switch_row(
                &mut PaintCtx::new(scene, tf, tr, theme),
                rect,
                "Invert",
                clip.mask.invert,
                rect.contains(self.last_mouse),
            );
        }
        // ---- title ----------------------------------------------------
        if let Some(rect) = l.title_text {
            let text_src = media
                .as_ref()
                .and_then(|m| m.text.clone())
                .unwrap_or_default();
            caption_above(scene, tr, tf, theme, "Words", rect.x0, rect.y0);
            let editing = self
                .title_edit
                .as_ref()
                .map(|(m, _)| Some(*m) == media.as_ref().map(|m| m.id));
            let radius = f64::from(theme.tokens.radii.control);
            // A field: the raised well with its derived rest edge, and the
            // focus ring over it while the inline editor owns the words.
            well(scene, tf, theme, rect, radius);
            if editing == Some(true) {
                draw_focus_ring(scene, tf, theme, rect, radius);
            }
            let body = theme.text(TextRole::Body);
            let body_h = line_height(tr, &body);
            // While the inline editor owns this box it draws the words itself
            // (with a caret); otherwise show them, or say plainly that there
            // are none rather than leaving an empty box that looks broken.
            if editing == Some(true) {
                // While the field is open, say what ends the edit — Enter is
                // a line break here, so it cannot also be Accept.
                let hint = theme.text(TextRole::Caption);
                text(
                    scene,
                    tr,
                    tf,
                    "Ctrl+Enter to accept, Esc to cancel",
                    &hint,
                    c.text_dim,
                    rect.x0,
                    rect.y1 + S1,
                );
            } else if text_src.content.trim().is_empty() {
                text(
                    scene,
                    tr,
                    tf,
                    "Click to type a title",
                    &body,
                    c.text_tertiary,
                    rect.x0 + S2,
                    rect.y0 + S2,
                );
            } else {
                // Real line breaks, on their own lines.
                let lines: Vec<&str> = text_src.content.lines().collect();
                for (i, line) in lines.iter().take(3).enumerate() {
                    let more = i == 2 && lines.len() > 3;
                    let line = if more {
                        format!("{line}…")
                    } else {
                        (*line).to_string()
                    };
                    let shown = elide_at(&line, (rect.width() - S4) as f32, &body, tr);
                    text(
                        scene,
                        tr,
                        tf,
                        &shown,
                        &body,
                        c.text_primary,
                        rect.x0 + S2,
                        rect.y0 + S2 + i as f64 * body_h,
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
            caption_above(scene, tr, tf, theme, "Font", rect.x0, rect.y0);
            // The open list is drawn later, over the panel, because it floats
            // above everything under it. Here is the resting control: the
            // shared dropdown face naming the family in force.
            if self.font_picker.is_none() {
                let words = if current.trim().is_empty() {
                    Self::DEFAULT_FONT_LABEL.to_string()
                } else {
                    current
                };
                let face = ButtonFace::new(rect, &words)
                    .control_size(control_size_for(rect, theme))
                    .hovered(rect.contains(self.last_mouse));
                Button::paint(scene, tf, tr, theme, &face);
            }
        }
        if let Some((_, first)) = l.title_colors.first() {
            let cur = media
                .as_ref()
                .and_then(|m| m.text.as_ref())
                .map_or([1.0; 4], |t| t.color);
            caption_above(scene, tr, tf, theme, "Colour", x0, first.y0);
            for (col, rect) in &l.title_colors {
                let chosen = col
                    .iter()
                    .zip(cur.iter())
                    .all(|(a, b)| (a - b).abs() < 1e-6);
                let face =
                    ui::widget::ColorSwatchFace::new(*rect, title_color(*col), "Title colour")
                        .selected(chosen)
                        .hovered(rect.contains(self.last_mouse));
                face.paint(scene, tf, tr, theme);
            }
        }
        for (rect, on, words) in [
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
            draw_toggle_button(
                &mut PaintCtx::new(scene, tf, tr, theme),
                rect,
                ToggleContent::Label(words),
                on,
                rect.contains(self.last_mouse),
            );
        }
        self.draw_marker_rows(scene, tf, tr, theme, &l, x0);
        self.draw_project_section(scene, tf, tr, theme, &l, x0);
        // Effects stack (video only).
        if is_video {
            for (i, row_rect) in l.effect_rows.iter().enumerate() {
                let Some(e) = clip.effects.get(i) else {
                    continue;
                };
                let (_key, up, down, slider, remove) = Self::effect_row_parts(*row_rect);
                well(
                    scene,
                    tf,
                    theme,
                    *row_rect,
                    f64::from(theme.tokens.radii.item),
                );
                // Reorder / remove: the toolkit's icon buttons.
                let last = i + 1 == clip.effects.len();
                for (rect, icon, name, enabled) in [
                    (up, Icon::ChevronUp, "Move up", i > 0),
                    (down, Icon::ChevronDown, "Move down", !last),
                    (remove, Icon::X, "Remove effect", true),
                ] {
                    draw_icon_button(
                        &mut PaintCtx::new(scene, tf, tr, theme),
                        rect,
                        icon,
                        name,
                        ButtonRole::Plain,
                        rect.contains(self.last_mouse),
                        enabled,
                    );
                }
                // Name.
                let top = text_baseline(*row_rect, &label, tr);
                let name_x = down.x1 + S2;
                let shown = elide_at(e.kind.label(), (slider.x0 - S2 - name_x) as f32, &label, tr);
                text(scene, tr, tf, &shown, &label, c.text_primary, name_x, top);
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
            }
            let add = l.add_effect;
            let face = ButtonFace::new(add, "Add effect")
                .icon(Icon::Plus)
                .control_size(control_size_for(add, theme))
                .hovered(add.contains(self.last_mouse));
            Button::paint(scene, tf, tr, theme, &face);
        }

        // ---- key controls ---------------------------------------------
        // Drawn after the rows and effect wells so a diamond sits over its row
        // rather than under it.
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
        if l.valid {
            draw_button(
                scene,
                tf,
                tr,
                theme,
                l.split,
                "Split",
                ButtonRole::Secondary,
                l.split.contains(self.last_mouse),
            );
            draw_button(
                scene,
                tf,
                tr,
                theme,
                l.delete,
                "Delete",
                ButtonRole::Destructive,
                l.delete.contains(self.last_mouse),
            );
        }
    }

    /// One collapsible section header: a fold chevron, the name, and — when it
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
            ui::chrome::SelectionState::new(false, rect.contains(self.last_mouse), false),
        );
        let g = f64::from(theme.tokens.metrics.icon_sm);
        let arrow = Rect::new(
            rect.x0,
            rect.center().y - g / 2.0,
            rect.x0 + g,
            rect.center().y + g / 2.0,
        );
        let icon = if open {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        icon.draw(ctx.scene, ctx.transform, arrow, c.text_dim);
        let heading = theme.text(TextRole::Subheading);
        let top = text_baseline(rect, &heading, ctx.text);
        let name_x = arrow.x1 + S2;
        text(
            ctx.scene,
            ctx.text,
            ctx.transform,
            section.label(),
            &heading,
            c.text_primary,
            name_x,
            top,
        );
        if open {
            return;
        }
        let Some(summary) = self.section_summary(section, clip, media) else {
            return;
        };
        let style = theme.text(TextRole::Caption);
        let room = rect.x1 - name_x - text_width(ctx.text, section.label(), &heading) - S3;
        let summary = elide_at(&summary, room.max(0.0) as f32, &style, ctx.text);
        let w = text_width(ctx.text, &summary, &style);
        let top = text_baseline(rect, &style, ctx.text);
        text(
            ctx.scene,
            ctx.text,
            ctx.transform,
            &summary,
            &style,
            c.text_dim,
            rect.x1 - w,
            top,
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
                let animated = Self::transform_is_animated(clip);
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
                Some(parts.join(", "))
            }
            Section::Speed => (!clip.speed.is_normal()).then(|| clip.speed.label()),
            // The lane's fader, and whether it has been caught clipping. The
            // second is the part that must survive a fold: hiding the warning
            // because the panel is tidied away is exactly the silence this
            // section exists to end.
            Section::Mix => {
                let t = self.project.track(clip.track)?;
                let clipped = self.mix.track(t.id).is_some_and(|level| level.clipped);
                if !t.mix_is_set() && !clipped {
                    return None;
                }
                let level = format!("{} {}", t.gain_words(), t.pan_words());
                Some(if clipped {
                    format!("{level}, clipping")
                } else {
                    level
                })
            }
            // A key that is switched on but keying nothing still has to
            // say so: "on" and "removing something" are different states,
            // and a fold that reported neither would hide both. The tolerance
            // is read at the playhead, because it is animatable.
            Section::Key => clip.key.enabled.then(|| {
                let tolerance = clip.param_at(Param::KeyTolerance, self.playhead);
                if tolerance <= 1e-6 {
                    "on, keys nothing".to_string()
                } else {
                    format!("keyed {:.0}%", tolerance * 100.0)
                }
            }),
            Section::Mask => clip.mask.is_active().then(|| {
                let shape = clip.mask.shape.label().to_lowercase();
                if clip.mask.invert {
                    format!("{shape}, inverted")
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
            // A folded Project header is where the frame most needs stating:
            // the format is a fact about every clip below it, and this is the
            // one line that carries it when the section is away.
            Section::Project => Some(self.project_format_shown.clone()),
        }
    }
}
