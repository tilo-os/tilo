#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;

#[derive(Default)]
pub(super) struct InspectorLayout {
    pub(super) valid: bool,
    /// The selected clip's facts (position, duration, source in and out),
    /// at the head of the body.
    pub(super) properties: Option<Rect>,
    pub(super) opacity: Option<Rect>,
    pub(super) volume: Option<Rect>,
    pub(super) brightness: Option<Rect>,
    pub(super) contrast: Option<Rect>,
    pub(super) saturation: Option<Rect>,
    pub(super) fade_in: Option<Rect>,
    pub(super) fade_out: Option<Rect>,
    pub(super) transition_y: f64,
    pub(super) transitions: Vec<(Transition, Rect)>,
    /// Length slider for the incoming transition. `None` when the clip's
    /// transition is `None` — there is no ramp to size.
    pub(super) transition_len: Option<Rect>,
    pub(super) colors_y: f64,
    /// The link / unlink button, laid out only when the selection actually has
    /// one thing or the other to offer — a control that is present and inert
    /// says nothing.
    pub(super) link: Option<Rect>,
    /// Baseline of the caption above it.
    pub(super) link_label_y: f64,
    pub(super) colors: Vec<(u8, Rect)>,
    /// y of the "EFFECTS" label; each row rect is a full-width effect row.
    pub(super) effects_label_y: f64,
    pub(super) effect_rows: Vec<Rect>,
    pub(super) add_effect: Rect,
    pub(super) split: Rect,
    pub(super) delete: Rect,
    // ---- collapsible sections -------------------------------------------
    /// Section header rows, in draw order. Clicking one folds or unfolds it.
    pub(super) sections: Vec<(Section, Rect)>,
    /// Every ranged control currently laid out, with the track to drag.
    pub(super) nums: Vec<(NumField, Rect)>,
    /// Picture-in-picture presets: label, corner in `-1..1`, and the button.
    pub(super) pip: Vec<(&'static str, (f64, f64), Rect)>,
    /// "Fill frame" — clears the transform back to the plain aspect-fit.
    pub(super) reset_transform: Option<Rect>,
    /// Baseline y of the "Crop" caption above the two crop rows.
    pub(super) crop_label_y: f64,
    /// Baseline y of the "Anchor" caption above the 3×3 grid.
    pub(super) anchor_label_y: f64,
    /// The nine anchor presets: the point in source uv, and its cell.
    pub(super) anchor: Vec<((f64, f64), Rect)>,
    /// Rate presets (`0.5×`, `1×`, …) as a segmented row.
    pub(super) speed_presets: Vec<(f64, Rect)>,
    // ---- the lane's mix ---------------------------------------------------
    /// Baseline y of the line naming which lane these two knobs belong to.
    /// The panel is otherwise entirely about one clip, so a control that
    /// quietly edits something shared has to say what it is editing.
    pub(super) mix_track_y: f64,
    /// The lane's level meter, at full panel width above its fader — the same
    /// measurement the track head shows, where the number is being set.
    pub(super) mix_meter: Option<Rect>,
    /// Back to unity and centre. Laid out only when the lane is somewhere
    /// else, because a reset that resets nothing is a control that lies about
    /// having something to undo.
    pub(super) mix_reset: Option<Rect>,
    /// The reverse toggle.
    pub(super) reverse: Option<Rect>,
    // ---- colour key --------------------------------------------------------
    /// Switches the key on and off. The sliders below it only appear once it
    /// is on, so an unkeyed clip does not carry four dead controls.
    pub(super) key_enable: Option<Rect>,
    /// Baseline y of the "Key colour" caption over the swatches.
    pub(super) key_color_y: f64,
    /// Preset key colours, and the swatch each is picked from.
    pub(super) key_colors: Vec<([f64; 3], Rect)>,
    /// Samples the key colour out of the frame under the playhead.
    pub(super) key_pick: Option<Rect>,
    // ---- mask ---------------------------------------------------------------
    /// Shape choice, as a segmented row: Off / Rect / Ellipse.
    pub(super) mask_shapes: Vec<(crate::model::MaskShape, Rect)>,
    /// Snaps the mask onto whatever the transform currently puts on screen.
    pub(super) mask_fit: Option<Rect>,
    /// Keep the outside rather than the inside.
    pub(super) mask_invert: Option<Rect>,
    // ---- title ------------------------------------------------------------
    /// The words, edited inline through the shared editable label.
    pub(super) title_text: Option<Rect>,
    pub(super) title_align: Vec<(TextAlign, Rect)>,
    /// The font-family chooser: a button that opens the shared dropdown.
    pub(super) title_font: Option<Rect>,
    /// Preset colours for the title's own (authored, non-palette) colour.
    pub(super) title_colors: Vec<([f64; 4], Rect)>,
    pub(super) title_bold: Option<Rect>,
    pub(super) title_italic: Option<Rect>,
    // ---- tone curves ------------------------------------------------------
    /// Which channel the editor is showing, as a segmented row.
    pub(super) curve_channels: Vec<(crate::curve::Channel, Rect)>,
    /// The square the curve is drawn and dragged in.
    pub(super) curve_canvas: Option<Rect>,
    /// Puts the shown channel back to the straight line.
    pub(super) curve_reset: Option<Rect>,
    // ---- keyframes --------------------------------------------------------
    /// The key control at the head of each animatable row.
    pub(super) keys: Vec<(Param, Rect)>,
    // ---- project format ---------------------------------------------------
    /// Baseline y of the readout that states the project's current format.
    pub(super) project_format_y: f64,
    /// Frame-size presets as a segmented row: the dimensions each writes.
    pub(super) project_frames: Vec<((u32, u32), Rect)>,
    /// Baseline y of the "Rate" caption over the rate row.
    pub(super) project_rate_y: f64,
    pub(super) project_rates: Vec<(f64, Rect)>,
    /// Takes the selected source's shape as the project's -- the button
    /// behind the one thing a preset row cannot offer, which is whatever the
    /// footage actually is.
    pub(super) project_match: Option<Rect>,
    // ---- markers ----------------------------------------------------------
    pub(super) marker_rows: Vec<(MarkerRef, Rect)>,
    /// Sub-rects of the row above: the name (click to rename) and the delete.
    pub(super) add_marker: Option<Rect>,
    /// Total content extent (px) from the inspector top, for scroll clamping.
    pub(super) content_height: f64,
}
impl VideoEditorApp {
    /// Lay out the markers section — its header, one row per marked instant,
    /// and the add button — advancing `y` past it.
    ///
    /// Shared by both inspector states: the list is the same whether a clip
    /// is selected or not, because a marker belongs to the project.
    pub(super) fn layout_markers(&self, l: &mut InspectorLayout, x0: f64, x1: f64, y: &mut f64) {
        l.sections
            .push((Section::Markers, Rect::new(x0, *y, x1, *y + SECTION_H)));
        *y += SECTION_H + 4.0;
        if self.collapsed.contains(&Section::Markers) {
            return;
        }
        let markers = self.project.all_markers();
        if markers.is_empty() {
            // Room for the "nothing marked yet" line, which would otherwise
            // land on the section header it sits under.
            *y += 18.0;
        }
        for (at, m, on_clip) in markers {
            // A row grows for a note it has — and for one being typed into it
            // for the first time, or the editor would open over the row below.
            let writing_note = self.marker_field == MarkerField::Note
                && self.marker_rename.as_ref().map(|(r, _)| r.id) == Some(m.id);
            let h = marker_row_height(!m.note.trim().is_empty() || writing_note);
            l.marker_rows.push((
                MarkerRef {
                    id: m.id,
                    clip: on_clip,
                    at,
                },
                Rect::new(x0, *y, x1, *y + h),
            ));
            *y += h + 4.0;
        }
        l.add_marker = Some(Rect::new(x0, *y, x1, *y + 26.0));
        *y += 34.0;
    }

    /// Lay out the project's format section — the readout of what the frame
    /// currently is, the size presets, the rate presets, and the button that
    /// takes the selected source's shape — advancing `y` past it.
    ///
    /// Project-level like the markers above it, so it is laid out in both
    /// inspector states. The frame was hard-wired to 1920x1080 at 30: there
    /// was exactly one place in the model that ever wrote those three numbers
    /// and every other reference read them, so a vertical or 4K shoot had no
    /// way to say so and came out letterboxed into 16:9.
    pub(super) fn layout_project(&self, l: &mut InspectorLayout, x0: f64, x1: f64, y: &mut f64) {
        l.sections
            .push((Section::Project, Rect::new(x0, *y, x1, *y + SECTION_H)));
        *y += SECTION_H + 4.0;
        if self.collapsed.contains(&Section::Project) {
            return;
        }
        // The readout's own line, then the two segmented rows under their
        // captions -- the same 20px a caption takes everywhere else in this
        // panel, so the three read as one convention.
        // The readout is drawn from `y` downwards, like every caption in this
        // panel, so it needs a caption's worth of room: at a 12px gap its
        // descenders sat on the preset row's top border.
        l.project_format_y = *y;
        *y += 24.0;
        let labels: Vec<String> = FRAME_PRESETS
            .iter()
            .map(|(n, _, _)| (*n).to_string())
            .collect();
        for ((_, w, h), rect) in FRAME_PRESETS
            .iter()
            .zip(self.segment_rects(&labels, x0, x1, *y, 24.0))
        {
            l.project_frames.push(((*w, *h), rect));
        }
        *y += 32.0;
        l.project_rate_y = *y;
        *y += 20.0;
        let labels: Vec<String> = RATE_PRESETS.iter().map(|r| rate_words(*r)).collect();
        for (rate, rect) in RATE_PRESETS
            .iter()
            .zip(self.segment_rects(&labels, x0, x1, *y, 24.0))
        {
            l.project_rates.push((*rate, rect));
        }
        *y += 32.0;
        // Offered only when there is a source to copy a shape from, and only
        // when that source has one -- audio and titles carry no frame.
        if self.format_source().is_some() {
            l.project_match = Some(Rect::new(x0, *y, x1, *y + 26.0));
            *y += 34.0;
        }
    }

    /// The source whose shape "Match media" would take: the bin selection
    /// when it has a frame, else the media under the selected clip, else the
    /// first source in the bin that has one.
    ///
    /// One resolver so the button, the press that runs it and the accessible
    /// node all name the same source -- a button that copied a shape from
    /// something other than the one it named would be worse than absent.
    pub(super) fn format_source(&self) -> Option<&MediaItem> {
        let has_frame = |m: &&MediaItem| m.width > 0 && m.height > 0;
        self.selected_media
            .and_then(|id| self.project.media(id))
            .filter(has_frame)
            .or_else(|| {
                self.selected()
                    .and_then(|id| self.project.clip(id))
                    .and_then(|c| self.project.media(c.media))
                    .filter(has_frame)
            })
            .or_else(|| self.project.media.iter().find(has_frame))
    }

    /// Sub-rects of one effect row:
    /// `(key, reorder-up, reorder-down, slider, remove)`.
    ///
    /// The key control leads the row, exactly as it leads every other
    /// animatable row in the panel. An effect's intensity is as animatable as
    /// a scale or a mask edge, and it was the one such control with no
    /// diamond — so it was keyframable only from the command palette, which
    /// is not somewhere a user looking at the slider would think to go. What
    /// it costs is width off the effect's name, and the names are one word.
    pub(super) fn effect_row_parts(row: Rect) -> (Rect, Rect, Rect, Rect, Rect) {
        let cy = row.center().y;
        let scale = (row.height() / 24.0).clamp(1.0, 2.0);
        // 22px tall targets — the row is 24px, so this is as close to the
        // toolkit's minimum as the row allows, and a real improvement on the
        // 16x18 boxes these were.
        let h = 11.0 * scale;
        let key = Rect::new(
            row.x0,
            cy - 9.0 * scale,
            row.x0 + KEY_BOX * scale,
            cy + 9.0 * scale,
        );
        let up = Rect::new(key.x1 + S1 * scale, cy - h, key.x1 + 26.0 * scale, cy + h);
        let down = Rect::new(
            up.x1 + S1 * scale,
            cy - h,
            up.x1 + (S1 + 22.0) * scale,
            cy + h,
        );
        let remove = Rect::new(row.x1 - 22.0 * scale, cy - h, row.x1, cy + h);
        let slider = Rect::new(
            row.x1 - 96.0 * scale,
            cy - 4.0 * scale,
            row.x1 - 30.0 * scale,
            cy + 4.0 * scale,
        );
        (key, up, down, slider, remove)
    }

    /// One segmented row across `x0..x1`, `h` tall at `y`, in the panel's
    /// logical pixels: each segment its label's width plus an equal share of
    /// the room the labels leave, so "Dissolve" or "1080p" is never elided
    /// beside a two-letter neighbour. Equal segments when the labels cannot
    /// be measured, or would not all fit.
    pub(super) fn segment_rects(
        &self,
        labels: &[String],
        x0: f64,
        x1: f64,
        y: f64,
        h: f64,
    ) -> Vec<Rect> {
        let n = labels.len().max(1) as f64;
        let avail = (x1 - x0).max(0.0);
        let scale = self.inspector_ui_scale();
        // Each label at the weight a chosen segment is set in, so choosing
        // one never makes it elide.
        let labelled: Option<Vec<f64>> = self.with_dialog_layout(|_, tr, theme| {
            let style = theme.text(TextRole::Label).strong();
            labels
                .iter()
                .map(|label| text_width(tr, label, &style) / scale)
                .collect()
        });
        let widths: Vec<f64> = match labelled {
            Some(labelled) if labelled.iter().sum::<f64>() < avail => {
                // The room the labels leave, shared equally: every segment
                // gets the same padding around its own label.
                let spare = (avail - labelled.iter().sum::<f64>()) / n;
                labelled.iter().map(|w| w + spare).collect()
            }
            _ => vec![avail / n; labels.len()],
        };
        let mut x = x0;
        widths
            .into_iter()
            .map(|w| {
                let rect = Rect::new(x, y, x + w, y + h);
                x += w;
                rect
            })
            .collect()
    }

    /// The maximum inspector scroll offset for the current selection.
    /// `content_height` is scroll-independent (the scroll is a pure translation),
    /// so the current layout's extent is the intrinsic content height.
    pub(super) fn inspector_max_scroll(&self, r: &Regions) -> f64 {
        (self.inspector_layout(r).content_height - self.inspector_viewport(r).height()).max(0.0)
    }
}

impl VideoEditorApp {
    #[allow(
        clippy::too_many_lines,
        reason = "one geometry pass keeps every inspector section aligned to the same scroll coordinate system"
    )]
    pub(super) fn inspector_layout(&self, r: &Regions) -> InspectorLayout {
        // Layout is authored in logical inspector pixels, then projected into
        // the physical column.  This keeps text, rows, hit targets and the
        // accessibility focus bands in one coordinate system at every text
        // scale instead of merely making the glyphs larger inside 1× rows.
        let scale = self.inspector_ui_scale();
        let physical_body_origin = self.inspector_body_origin(r);
        let body_origin = r.inspector.y0 + (physical_body_origin - r.inspector.y0) / scale;
        let mut logical = *r;
        logical.inspector = Rect::new(
            r.inspector.x0,
            r.inspector.y0,
            r.inspector.x0 + r.inspector.width() / scale,
            r.inspector.y0 + r.inspector.height() / scale,
        );
        let r = &logical;
        let mut l = InspectorLayout::default();
        // Markers belong to the project, not to any clip, so the list is laid
        // out with nothing selected too. Without this the only way to see or
        // remove a marker was to first pick a clip that had nothing to do
        // with it.
        let selected = self.selected().and_then(|id| self.project.clip(id));
        if selected.is_none() {
            let x0 = r.inspector.x0 + PAD;
            let x1 = r.inspector.x1 - PAD;
            let mut y = body_origin - self.inspector_scroll / scale + 16.0;
            self.layout_markers(&mut l, x0, x1, &mut y);
            self.layout_project(&mut l, x0, x1, &mut y);
            l.content_height = (y - (body_origin - self.inspector_scroll / scale)) * scale;
            l.scale_from(r.inspector.x0, r.inspector.y0, scale);
            return l;
        }
        let Some(clip) = selected else {
            return l;
        };
        let media = self.project.media(clip.media);
        let is_video = media.is_none_or(|item| item.kind.is_visual());
        let is_title = media.is_some_and(|item| item.text.is_some());
        let x0 = r.inspector.x0 + PAD;
        let x1 = r.inspector.x1 - PAD;
        // The fixed header (name, lane and the clip's facts) is measured by
        // `inspector_header`; the editable controls start below it, and
        // everything here scrolls together by `inspector_scroll` (the pane
        // clips + is wheel-scrollable) so the effects stack can grow without
        // clipping the actions.
        let top = body_origin - self.inspector_scroll / scale;
        let mut y = top + S3;
        // The clip's facts first, as tall as the property list draws them.
        let facts_h = self
            .with_dialog_layout(|_, tr, theme| {
                let values = Self::inspector_property_values(clip, self.project.fps);
                let rows = inspector_property_rows(&values);
                ui::widget::PropertyListFace::new(&rows).height(tr, theme, (x1 - x0) * scale)
                    / scale
            })
            .unwrap_or(128.0);
        l.properties = Some(Rect::new(x0, y, x1, y + facts_h));
        y += facts_h + S2;
        let row = 40.0; // the label line at y, the 8px track under it with room for the knob
        let track_at = |y: f64| Rect::new(x0, y + 25.0, x1, y + 33.0);
        // An animatable row indents its label past a key control at the head.
        // The indent is unconditional so the row does not shuffle sideways the
        // moment a parameter is keyed.
        let key_at = |y: f64| Rect::new(x0, y, x0 + KEY_BOX, y + 18.0);

        if is_video {
            l.opacity = Some(track_at(y));
            l.keys.push((Param::Opacity, key_at(y)));
            y += row;
            l.brightness = Some(track_at(y));
            l.keys.push((Param::Brightness, key_at(y)));
            y += row;
            l.contrast = Some(track_at(y));
            l.keys.push((Param::Contrast, key_at(y)));
            y += row;
            l.saturation = Some(track_at(y));
            l.keys.push((Param::Saturation, key_at(y)));
            y += row;
        } else {
            l.volume = Some(track_at(y));
            l.keys.push((Param::Volume, key_at(y)));
            y += row;
        }
        // Fade in / out share a row (two half-width tracks).
        let half = (x1 - x0) / 2.0 - 6.0;
        l.fade_in = Some(Rect::new(x0, y + 25.0, x0 + half, y + 33.0));
        l.fade_out = Some(Rect::new(x1 - half, y + 25.0, x1, y + 33.0));
        // A captioned control row takes a slider row's geometry: its control
        // where a track would be, its caption where a label would be, so the
        // caption keeps the clearance every label has over the knobs above.
        y += row;

        // Transition segmented control.
        l.transition_y = y + 25.0;
        let labels: Vec<String> = Transition::ALL
            .iter()
            .map(|t| t.label().to_string())
            .collect();
        for (t, rect) in
            Transition::ALL
                .iter()
                .zip(self.segment_rects(&labels, x0, x1, y + 25.0, 24.0))
        {
            l.transitions.push((*t, rect));
        }
        y += 56.0;
        // Transition length, only when there is a transition to size.
        if clip.transition != Transition::None {
            l.transition_len = Some(Rect::new(x0, y + 25.0, x1, y + 33.0));
            y += row;
        }
        // Label color swatches.
        l.colors_y = y + 25.0;
        let sw = 24.0;
        for i in 0..7u8 {
            let sx0 = x0 + f64::from(i) * (sw + 4.0);
            if sx0 + sw > x1 {
                break;
            }
            l.colors
                .push((i, Rect::new(sx0, y + 25.0, sx0 + sw, y + 25.0 + sw)));
        }
        y += 56.0;
        // The link control. It sits in the top block, with the clip's own
        // identity rather than down among the picture sections, because it
        // says what this clip *is* — half of a shot, or a clip on its own —
        // and because an audio clip has to be able to reach it too.
        if self.link_affordance().is_some() {
            l.link_label_y = y + 15.0;
            l.link = Some(Rect::new(x0, y + 20.0, x1, y + 46.0));
            y += 56.0;
        }

        // ---- collapsible sections ------------------------------------
        // Each opens with a header row that folds it; a folded section
        // contributes only its header (and still states what is inside it).
        let header = |l: &mut InspectorLayout, y: &mut f64, s: Section| -> bool {
            l.sections.push((s, Rect::new(x0, *y, x1, *y + SECTION_H)));
            *y += SECTION_H + 4.0;
            !self.collapsed.contains(&s)
        };
        // Tone curves, video only — it is the same grade the three sliders
        // above start, so it sits directly under them.
        if is_video && header(&mut l, &mut y, Section::Curves) {
            let labels: Vec<String> = crate::curve::Channel::ALL
                .iter()
                .map(|c| c.label().to_string())
                .collect();
            for (ch, rect) in crate::curve::Channel::ALL
                .iter()
                .zip(self.segment_rects(&labels, x0, x1, y, 24.0))
            {
                l.curve_channels.push((*ch, rect));
            }
            y += 32.0;
            // Square, so the diagonal that means "unchanged" really is a
            // diagonal — on a squashed box a curve reads as steeper or
            // flatter than it is.
            let side = (x1 - x0).min(200.0);
            l.curve_canvas = Some(Rect::new(x0, y, x0 + side, y + side));
            // Room for the gesture hint under the box *and* clear air before
            // the amount row's own label: at the bare 12px gap the two shared
            // a baseline and drew through each other.
            y += side + 34.0;
            l.nums.push((NumField::CurveMix, track_at(y)));
            l.keys.push((Param::CurveMix, key_at(y)));
            y += row;
            l.curve_reset = Some(Rect::new(x0, y, x1, y + 26.0));
            y += 34.0;
        }
        // Geometry, video only — an audio clip has no picture to place.
        if is_video && header(&mut l, &mut y, Section::Transform) {
            for f in [
                NumField::ScaleX,
                NumField::ScaleY,
                NumField::PosX,
                NumField::PosY,
                NumField::Rotation,
            ] {
                l.nums.push((f, track_at(y)));
                if let Some(p) = f.param() {
                    l.keys.push((p, key_at(y)));
                }
                y += row;
            }
            // Crop: two half-width pairs under their own caption. No key
            // control — crop frames the shot rather than moving it, and the
            // model does not animate it.
            y += 12.0;
            l.crop_label_y = y;
            y += 20.0;
            let half = (x1 - x0) / 2.0 - 6.0;
            for pair in [
                [NumField::CropLeft, NumField::CropRight],
                [NumField::CropTop, NumField::CropBottom],
            ] {
                l.nums
                    .push((pair[0], Rect::new(x0, y + 25.0, x0 + half, y + 33.0)));
                l.nums
                    .push((pair[1], Rect::new(x1 - half, y + 25.0, x1, y + 33.0)));
                y += row;
            }
            // Anchor: the point scale and rotation turn about. Nine presets
            // as three segmented rows rather than a free pair of sliders —
            // an anchor is chosen, not dialled, and every value anyone wants
            // is a corner, an edge or the middle.
            y += 12.0;
            l.anchor_label_y = y;
            y += 20.0;
            let cell = (x1 - x0) / 3.0;
            // Three segmented rows an `S1` apart: each row is one shared
            // control whose segments abut (the face draws the separators),
            // and the rows are neighbours that must not touch.
            for ay in ANCHOR_STOPS {
                for (col, ax) in ANCHOR_STOPS.iter().enumerate() {
                    let cx = x0 + col as f64 * cell;
                    l.anchor
                        .push(((*ax, ay), Rect::new(cx, y, cx + cell, y + 22.0)));
                }
                y += 22.0 + S1;
            }
            y += 6.0;
            // Picture-in-picture, as the four corners it is actually used for.
            y += 16.0;
            // Four sibling buttons, `S2` apart.
            let bw = (x1 - x0 - 3.0 * S2) / 4.0;
            for (i, (label, corner)) in [
                ("↖", (-1.0, -1.0)),
                ("↗", (1.0, -1.0)),
                ("↙", (-1.0, 1.0)),
                ("↘", (1.0, 1.0)),
            ]
            .into_iter()
            .enumerate()
            {
                let bx = x0 + i as f64 * (bw + S2);
                l.pip
                    .push((label, corner, Rect::new(bx, y, bx + bw, y + 26.0)));
            }
            y += 32.0;
            l.reset_transform = Some(Rect::new(x0, y, x1, y + 26.0));
            y += 34.0;
        }
        // Rate and direction.
        if header(&mut l, &mut y, Section::Speed) {
            l.nums.push((NumField::Speed, track_at(y)));
            y += row;
            let labels: Vec<String> = SPEED_PRESETS.iter().map(|r| speed_words(*r)).collect();
            for (rate, rect) in SPEED_PRESETS
                .iter()
                .zip(self.segment_rects(&labels, x0, x1, y, 24.0))
            {
                l.speed_presets.push((*rate, rect));
            }
            y += 32.0;
            l.reverse = Some(Rect::new(x0, y, x1, y + 26.0));
            y += 34.0;
        }
        // The lane's own fader and pan. Present for a picture clip as well as
        // an audio one: a shot on a video track carries its source's sound,
        // so that lane has a level whether or not anyone has split it off.
        //
        // No key control on either row. A fader is a mix decision, not an
        // animation — a level that has to move through a shot is what the
        // clip's own volume curve is for, and two animatable levels stacked
        // on one signal is two places to look for why it faded.
        if header(&mut l, &mut y, Section::Mix) {
            l.mix_track_y = y;
            y += 18.0;
            l.mix_meter = Some(Rect::new(x0, y, x1, y + 12.0));
            y += 22.0;
            for f in [NumField::TrackGain, NumField::TrackPan] {
                l.nums.push((f, track_at(y)));
                y += row;
            }
            if self
                .selected_track()
                .is_some_and(crate::model::Track::mix_is_set)
            {
                l.mix_reset = Some(Rect::new(x0, y, x1, y + 26.0));
                y += 34.0;
            } else {
                y += 4.0;
            }
        }
        // Colour key, video only — there is nothing to key out of audio.
        // The four sliders appear only once the key is switched on, so an
        // unkeyed clip does not carry a row of controls that do nothing.
        if is_video {
            let open = header(&mut l, &mut y, Section::Key);
            if open {
                l.key_enable = Some(Rect::new(x0, y, x1, y + 26.0));
                y += 34.0;
                if clip.key.enabled {
                    // The caption's own line, then the swatches under it —
                    // the same 20px the Crop caption takes, so the two read
                    // as one convention rather than two.
                    l.key_color_y = y;
                    y += 20.0;
                    let sw = 26.0;
                    for (i, (_, col)) in crate::model::KEY_PRESETS.iter().enumerate() {
                        let sx = x0 + i as f64 * (sw + S1);
                        if sx + sw > x1 {
                            break;
                        }
                        l.key_colors.push((*col, Rect::new(sx, y, sx + sw, y + sw)));
                    }
                    // Sampling the plate is how a key is really set; the
                    // presets are only a starting point. It starts a sibling
                    // step past the swatches.
                    let pick_x = x0 + 3.0 * (sw + S1) + S2;
                    if pick_x + 96.0 <= x1 {
                        l.key_pick = Some(Rect::new(pick_x, y, x1, y + sw));
                    }
                    y += sw + 14.0;
                    for f in [
                        NumField::KeyTolerance,
                        NumField::KeySoftness,
                        NumField::KeySpill,
                    ] {
                        l.nums.push((f, track_at(y)));
                        if let Some(p) = f.param() {
                            l.keys.push((p, key_at(y)));
                        }
                        y += row;
                    }
                    y += 4.0;
                }
            }
        }
        // Shape mask, video only. The geometry rows appear once a shape is
        // chosen — with the mask off there is nothing for them to move.
        if is_video {
            let open = header(&mut l, &mut y, Section::Mask);
            if open {
                let labels: Vec<String> = crate::model::MaskShape::ALL
                    .iter()
                    .map(|s| s.label().to_string())
                    .collect();
                for (shape, rect) in crate::model::MaskShape::ALL
                    .iter()
                    .zip(self.segment_rects(&labels, x0, x1, y, 24.0))
                {
                    l.mask_shapes.push((*shape, rect));
                }
                y += 32.0;
                if clip.mask.is_active() {
                    for f in [
                        NumField::MaskX,
                        NumField::MaskY,
                        NumField::MaskW,
                        NumField::MaskH,
                        NumField::MaskRotation,
                        NumField::MaskFeather,
                    ] {
                        l.nums.push((f, track_at(y)));
                        if let Some(p) = f.param() {
                            l.keys.push((p, key_at(y)));
                        }
                        y += row;
                    }
                    let hw = (x1 - x0) / 2.0 - 4.0;
                    l.mask_fit = Some(Rect::new(x0, y, x0 + hw, y + 26.0));
                    l.mask_invert = Some(Rect::new(x1 - hw, y, x1, y + 26.0));
                    y += 34.0;
                }
            }
        }
        // A title's words and setting — only for a clip whose source is one.
        if is_title && header(&mut l, &mut y, Section::Title) {
            // Captioned controls need the caption's line above them; without
            // it the caption lands on the section header it sits under.
            y += 16.0;
            // Three lines' worth: a title is two or three lines as often as
            // it is one, and a one-line box could not show what was typed.
            l.title_text = Some(Rect::new(x0, y, x1, y + 66.0));
            y += 92.0;
            l.nums.push((NumField::TitleSize, track_at(y)));
            y += row;
            let labels: Vec<String> = TextAlign::ALL
                .iter()
                .map(|a| a.label().to_string())
                .collect();
            for (a, rect) in TextAlign::ALL
                .iter()
                .zip(self.segment_rects(&labels, x0, x1, y, 24.0))
            {
                l.title_align.push((*a, rect));
            }
            // The caption above the chooser needs its own line: at the
            // ordinary 16px gap it landed on the alignment row's buttons.
            y += 32.0 + 22.0;
            l.title_font = Some(Rect::new(x0, y, x1, y + 28.0));
            y += 36.0 + 16.0;
            let sw = 24.0;
            for (i, col) in TITLE_COLORS.iter().enumerate() {
                let sx = x0 + i as f64 * (sw + 4.0);
                if sx + sw > x1 {
                    break;
                }
                l.title_colors
                    .push((*col, Rect::new(sx, y, sx + sw, y + sw)));
            }
            y += sw + 12.0;
            let hw = (x1 - x0) / 2.0 - 4.0;
            l.title_bold = Some(Rect::new(x0, y, x0 + hw, y + 26.0));
            l.title_italic = Some(Rect::new(x1 - hw, y, x1, y + 26.0));
            y += 34.0;
            l.nums.push((NumField::TitleBackdrop, track_at(y)));
            y += row + 4.0;
        }
        // Effects stack (video only): one row per effect, then an "Add
        // effect" button.
        if is_video {
            let open = header(&mut l, &mut y, Section::Effects);
            l.effects_label_y = y;
            if open {
                for i in 0..clip.effects.len() {
                    let row = Rect::new(x0, y, x1, y + 24.0);
                    l.effect_rows.push(row);
                    // The row's key control joins the panel's one list of
                    // them, so drawing, hit-testing and the keyframe menu all
                    // reach it without a second code path.
                    l.keys
                        .push((Param::EffectAmount(i as u32), Self::effect_row_parts(row).0));
                    y += 28.0;
                }
                l.add_effect = Rect::new(x0, y, x0 + 132.0, y + 24.0);
                y += 34.0;
            }
        }
        // Markers: every marked instant on the timeline, the project's own and
        // each clip's, so one list is the whole answer to "what is marked".
        self.layout_markers(&mut l, x0, x1, &mut y);
        self.layout_project(&mut l, x0, x1, &mut y);
        // Actions.
        y += 6.0;
        l.split = Rect::new(x0, y, x0.midpoint(x1) - 4.0, y + 28.0);
        l.delete = Rect::new(x0.midpoint(x1) + 4.0, y, x1, y + 28.0);
        y += 34.0;
        l.content_height = (y - top) * scale;
        l.valid = true;
        l.scale_from(r.inspector.x0, r.inspector.y0, scale);
        l
    }
}

impl InspectorLayout {
    /// Project a logical inspector layout into physical pixels.  The layout
    /// owns every interactive rectangle, so doing this once keeps painting,
    /// hit testing, the scrollbar's extent and AT focus rings identical.
    #[allow(
        clippy::too_many_lines,
        reason = "one mapping per laid-out field, listed together so a new field cannot be left unscaled"
    )]
    pub(super) fn scale_from(&mut self, ox: f64, oy: f64, scale: f64) {
        if (scale - 1.0).abs() < f64::EPSILON {
            return;
        }
        let map = |r: Rect| {
            Rect::new(
                ox + (r.x0 - ox) * scale,
                oy + (r.y0 - oy) * scale,
                ox + (r.x1 - ox) * scale,
                oy + (r.y1 - oy) * scale,
            )
        };
        let one = |r: &mut Option<Rect>| *r = r.map(map);
        one(&mut self.properties);
        one(&mut self.opacity);
        one(&mut self.volume);
        one(&mut self.brightness);
        one(&mut self.contrast);
        one(&mut self.saturation);
        one(&mut self.fade_in);
        one(&mut self.fade_out);
        one(&mut self.transition_len);
        one(&mut self.link);
        one(&mut self.add_marker);
        one(&mut self.reset_transform);
        one(&mut self.mix_meter);
        one(&mut self.mix_reset);
        one(&mut self.reverse);
        one(&mut self.key_enable);
        one(&mut self.key_pick);
        one(&mut self.mask_fit);
        one(&mut self.mask_invert);
        one(&mut self.title_text);
        one(&mut self.title_font);
        one(&mut self.title_bold);
        one(&mut self.title_italic);
        one(&mut self.curve_canvas);
        one(&mut self.curve_reset);
        one(&mut self.project_match);
        self.split = map(self.split);
        self.delete = map(self.delete);
        self.transition_y = oy + (self.transition_y - oy) * scale;
        self.colors_y = oy + (self.colors_y - oy) * scale;
        self.link_label_y = oy + (self.link_label_y - oy) * scale;
        self.effects_label_y = oy + (self.effects_label_y - oy) * scale;
        self.crop_label_y = oy + (self.crop_label_y - oy) * scale;
        self.anchor_label_y = oy + (self.anchor_label_y - oy) * scale;
        self.mix_track_y = oy + (self.mix_track_y - oy) * scale;
        self.key_color_y = oy + (self.key_color_y - oy) * scale;
        self.project_format_y = oy + (self.project_format_y - oy) * scale;
        self.project_rate_y = oy + (self.project_rate_y - oy) * scale;
        for (_, r) in &mut self.transitions {
            *r = map(*r);
        }
        for (_, r) in &mut self.colors {
            *r = map(*r);
        }
        for r in &mut self.effect_rows {
            *r = map(*r);
        }
        for (_, r) in &mut self.sections {
            *r = map(*r);
        }
        for (_, r) in &mut self.nums {
            *r = map(*r);
        }
        for (_, _, r) in &mut self.pip {
            *r = map(*r);
        }
        for (_, r) in &mut self.anchor {
            *r = map(*r);
        }
        for (_, r) in &mut self.speed_presets {
            *r = map(*r);
        }
        for (_, r) in &mut self.key_colors {
            *r = map(*r);
        }
        for (_, r) in &mut self.mask_shapes {
            *r = map(*r);
        }
        for (_, r) in &mut self.title_align {
            *r = map(*r);
        }
        for (_, r) in &mut self.title_colors {
            *r = map(*r);
        }
        for (_, r) in &mut self.curve_channels {
            *r = map(*r);
        }
        for (_, r) in &mut self.keys {
            *r = map(*r);
        }
        for (_, r) in &mut self.project_frames {
            *r = map(*r);
        }
        for (_, r) in &mut self.project_rates {
            *r = map(*r);
        }
        for (_, r) in &mut self.marker_rows {
            *r = map(*r);
        }
    }

    /// The track laid out for `field`, if it is on screen right now.
    pub(super) fn num(&self, field: NumField) -> Option<Rect> {
        self.nums.iter().find(|(f, _)| *f == field).map(|(_, r)| *r)
    }
}
