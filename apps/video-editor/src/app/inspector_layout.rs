use super::*;

#[derive(Default)]
pub(super) struct InspectorLayout {
    pub(super) valid: bool,
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

impl InspectorLayout {
    /// Project a logical inspector layout into physical pixels.  The layout
    /// owns every interactive rectangle, so doing this once keeps painting,
    /// hit testing, the scrollbar's extent and AT focus rings identical.
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
