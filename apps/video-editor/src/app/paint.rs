//! Stateless paint, text-fit, and canvas geometry helpers for the editor.

#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;
pub(super) use ui::paint::{fill_rect, fill_round, stroke_round};

/// A label colour guaranteed legible on `fill`, and the (possibly deepened)
/// fill to paint under it — the toolkit's own contrast rule, so a clip name on
/// a label colour reads in light and dark alike. The preferred ink is the
/// media ink: these fills are the person's own label colours, which sit on
/// the timeline like footage does.
pub(super) fn on_fill(theme: &Theme, fill: Color) -> (Color, Color) {
    let c = &theme.tokens.colors;
    c.legible_fill(fill, c.media.ink)
}

/// The `y` to pass to [`text`] so one line of `style` sits vertically centred
/// in `rect`, measured from the font's real line height.
pub(super) fn text_baseline(rect: Rect, style: &TextStyle, tr: &mut TextRenderer) -> f64 {
    rect.center().y - line_height(tr, style) / 2.0
}

/// Top-left corner for `s` drawn centred both ways inside `rect`.
pub(super) fn text_centre(rect: Rect, s: &str, style: &TextStyle, tr: &mut TextRenderer) -> Point {
    let (w, h) = tr.measure_style(s, None, style);
    Point::new(
        rect.center().x - f64::from(w) / 2.0,
        rect.center().y - f64::from(h) / 2.0,
    )
}

/// The width `s` takes set in `style`.
pub(super) fn text_width(tr: &mut TextRenderer, s: &str, style: &TextStyle) -> f64 {
    f64::from(tr.measure_style(s, None, style).0)
}

/// Cell width and count for a filmstrip drawn across `inner`. Shared by the
/// request pass and the draw pass so the two never disagree about how many
/// frames a strip needs — a mismatch would request frames at times the strip
/// never asks for, and leave the cells it does ask for undecoded.
pub(super) fn filmstrip_cells(inner: Rect) -> (f64, i64) {
    let cell_w = (inner.height() * 16.0 / 9.0).clamp(24.0, 56.0);
    let n = (inner.width() / (cell_w + 2.0)).floor().max(1.0) as i64;
    (cell_w, n)
}

/// The content rect inside a clip body, below its header strip. Shared for the
/// same reason as [`filmstrip_cells`].
pub(super) fn clip_inner_rect(cr: Rect) -> Rect {
    let strip_y1 = (cr.y0 + 18.0).min(cr.y1);
    Rect::new(cr.x0 + 2.0, strip_y1, cr.x1 - 2.0, cr.y1 - 2.0)
}

/// The file's own name, for a message about it — never the whole path, which
/// would run off the notice line.
pub(super) fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.to_string_lossy().into_owned(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Honest, human-readable summary of an import batch. Never claims a success
/// that did not happen — and never throws away *why* a file was refused.
///
/// A batch with any refusal in it reads as a failure, because it is one for
/// the files that did not make it. It used to collapse to "Imported 3 of 5
/// files": the two refusals and the probe's reasons for them were dropped on
/// the floor, and the line was painted in the success tint on top of that.
pub(super) fn import_report_notice(report: &ImportReport) -> Notice {
    if report.refused.is_empty() {
        return Notice::ok(format!(
            "Imported {} clip{}",
            report.added,
            if report.added == 1 { "" } else { "s" },
        ));
    }
    // Name the files and quote the probe. Two fit a one-line strip; the rest
    // are counted rather than silently dropped.
    let listed: Vec<String> = report
        .refused
        .iter()
        .take(IMPORT_REFUSALS_NAMED)
        .map(|(name, reason)| format!("{name}: {reason}"))
        .collect();
    let mut why = listed.join("; ");
    let hidden = report.refused.len() - listed.len();
    if hidden > 0 {
        why = format!("{why} and {hidden} more");
    }
    let head = if report.added == 0 {
        "No media could be imported".to_string()
    } else {
        format!("Imported {} of {}", report.added, report.total())
    };
    Notice::failed(format!("{head} — {why}"))
}

/// Blit an RGBA frame into `canvas`, scaled to fit (letterboxed), centred.
/// Carries the preview's composited picture in every build — the compositor
/// runs with or without a decoder, drawing real footage when it has any and
/// representative fills when it does not.
pub(super) fn blit_frame(scene: &mut Scene, tf: Affine, frame: &::media::VideoFrame, canvas: Rect) {
    use vello::peniko::{Blob, ImageAlphaType, ImageBrush, ImageData, ImageFormat};
    let iw = f64::from(frame.width);
    let ih = f64::from(frame.height);
    if iw <= 0.0 || ih <= 0.0 || canvas.width() <= 0.0 || canvas.height() <= 0.0 {
        return;
    }
    let img = ImageData {
        data: Blob::new(frame.rgba.clone()),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: frame.width,
        height: frame.height,
    };
    let scale = (canvas.width() / iw).min(canvas.height() / ih);
    let dw = iw * scale;
    let dh = ih * scale;
    let dx = canvas.x0 + (canvas.width() - dw) * 0.5;
    let dy = canvas.y0 + (canvas.height() - dh) * 0.5;
    let dest = Rect::new(dx, dy, dx + dw, dy + dh);
    let brush_tf = Affine::translate((dx, dy)) * Affine::scale_non_uniform(scale, scale);
    let brush = ImageBrush::new(img);
    scene.fill(Fill::NonZero, tf, &brush, Some(brush_tf), &dest);
}

/// Blit a thumbnail into `dest`, cover-fitting (fill + crop) with rounded
/// corners `radius`. Used for the bin swatch and timeline filmstrip cells; the
/// same path shows the generated representative frame or (under decode) the real
/// decoded frame.
pub(super) fn blit_thumb(
    scene: &mut Scene,
    tf: Affine,
    thumb: &crate::thumbnail::Thumb,
    dest: Rect,
    radius: f64,
) {
    use vello::peniko::ImageBrush;
    let iw = f64::from(thumb.width);
    let ih = f64::from(thumb.height);
    if iw <= 0.0 || ih <= 0.0 || dest.width() <= 0.0 || dest.height() <= 0.0 {
        return;
    }
    // The thumbnail cache builds the render handle once and hands it out by
    // reference, so painting a frame costs a clone of the handle rather than a
    // copy of the pixels.
    let img = thumb.image().clone();
    // Cover: scale so the frame fills the dest, cropping the overflow.
    let scale = (dest.width() / iw).max(dest.height() / ih);
    let dw = iw * scale;
    let dh = ih * scale;
    let dx = dest.x0 + (dest.width() - dw) * 0.5;
    let dy = dest.y0 + (dest.height() - dh) * 0.5;
    let brush_tf = Affine::translate((dx, dy)) * Affine::scale_non_uniform(scale, scale);
    let brush = ImageBrush::new(img);
    scene.fill(
        Fill::NonZero,
        tf,
        &brush,
        Some(brush_tf),
        &RoundedRect::from_rect(dest, radius),
    );
}
/// Diagonal hatching clipped to `rect`, for a surface whose content is a
/// stand-in rather than the real thing. Hatching (not a flat tint) so the
/// state survives a colour-blind reading and a greyscale screenshot.
pub(super) fn hatch_rect(scene: &mut Scene, tf: Affine, color: Color, rect: Rect, radius: f64) {
    if rect.width() <= 1.0 || rect.height() <= 1.0 {
        return;
    }
    scene.push_layer(
        Fill::NonZero,
        vello::peniko::BlendMode::default(),
        1.0,
        tf,
        &RoundedRect::from_rect(rect, radius),
    );
    let step = 9.0;
    let stroke = Stroke::new(2.0);
    let first = rect.x0 - rect.height();
    let stripes = (0u32..)
        .map(|k| first + f64::from(k) * step)
        .take_while(|&x| x < rect.x1);
    for x in stripes {
        scene.stroke(
            &stroke,
            tf,
            color,
            None,
            &Line::new(
                Point::new(x, rect.y1),
                Point::new(x + rect.height(), rect.y0),
            ),
        );
    }
    scene.pop_layer();
}
pub(super) fn stroke_line(
    scene: &mut Scene,
    tf: Affine,
    color: Color,
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    w: f64,
) {
    scene.stroke(
        &Stroke::new(w),
        tf,
        color,
        None,
        &Line::new(Point::new(x0, y0), Point::new(x1, y1)),
    );
}
/// One line of `s` in `style`, its line box's top-left at (`x`, `y`).
pub(super) fn text(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    tf: Affine,
    s: &str,
    style: &TextStyle,
    color: Color,
    x: f64,
    y: f64,
) {
    tr.draw_style(scene, s, color, tf * Affine::translate((x, y)), None, style);
}

/// A button in the editor's painted chrome: the toolkit's own face for the
/// role, at the control size the editor's rows lay out, so every command in
/// the window is the same button the rest of the system draws.
pub(super) fn draw_button(
    scene: &mut Scene,
    tf: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    role: ButtonRole,
    hovered: bool,
) {
    draw_button_enabled(
        &mut PaintCtx::new(scene, tf, tr, theme),
        rect,
        label,
        role,
        hovered,
        true,
    );
}

/// [`draw_button`] plus an explicit enabled flag, so a control the app cannot
/// actually perform is painted with the toolkit's own disabled treatment.
pub(super) fn draw_button_enabled(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    label: &str,
    role: ButtonRole,
    hovered: bool,
    enabled: bool,
) {
    let theme = ctx.theme;
    let face = ButtonFace::new(rect, label)
        .role(role)
        .control_size(control_size_for(rect, theme))
        .hovered(hovered && enabled)
        .disabled(!enabled);
    Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face);
}

/// An icon-only command (a compact topbar slot, a row's reorder arrow).
pub(super) fn draw_icon_button(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    icon: Icon,
    name: &str,
    role: ButtonRole,
    hovered: bool,
    enabled: bool,
) {
    let theme = ctx.theme;
    let face = ButtonFace::icon_only(rect, icon, name)
        .role(role)
        .control_size(control_size_for(rect, theme))
        .hovered(hovered && enabled)
        .disabled(!enabled);
    Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face);
}

/// A command that stays down while its state is on (mute, lock, solo, the
/// bin and inspector toggles): a plain button standing on the selection
/// lift while it is on, exactly as a toolbar toggle is drawn.
pub(super) fn draw_toggle_button(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    content: ToggleContent<'_>,
    on: bool,
    hovered: bool,
) {
    let theme = ctx.theme;
    let face = match content {
        ToggleContent::Icon(icon, name) => ButtonFace::icon_only(rect, icon, name),
        ToggleContent::Label(label) => ButtonFace::new(rect, label).role(ButtonRole::Plain),
    }
    .control_size(control_size_for(rect, theme))
    .toggled(on)
    .hovered(hovered);
    Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face);
}

/// What a [`draw_toggle_button`] shows.
#[derive(Clone, Copy)]
pub(super) enum ToggleContent<'a> {
    /// An icon and the name a reader hears for it.
    Icon(Icon, &'a str),
    /// A short label.
    Label(&'a str),
}

/// The control size whose height a laid-out rect was given: the editor lays
/// its rows out before it paints, so the face is told which size it is in.
pub(super) fn control_size_for(rect: Rect, theme: &Theme) -> ControlSize {
    let regular = ButtonFace::height(ControlSize::Regular, theme);
    if rect.height() + 0.5 < regular {
        ControlSize::Small
    } else {
        ControlSize::Regular
    }
}

/// A switch beside its label across `rect`: the on/off settings of a clip
/// (play backwards, key a colour out, invert the mask) read as a state, not
/// as a button whose wording flips.
pub(super) fn draw_switch_row(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    label: &str,
    on: bool,
    hovered: bool,
) {
    let theme = ctx.theme;
    let track = switch_rect(rect, theme);
    let ink = theme.tokens.colors.text_primary;
    let role = TextRole::Label;
    let room = (track.x0 - rect.x0 - f64::from(theme.tokens.spacing.s2)).max(0.0);
    let style = theme.text(role);
    let shown = elide_at(label, room as f32, &style, ctx.text);
    ctx.text_role_in_band(&shown, role, ink, rect.x0, rect, None);
    let face = ui::widget::ToggleFace::new(track, on).hovered(hovered);
    ui::widget::Toggle::paint(ctx.scene, ctx.transform, theme, &face);
}

/// Where a [`draw_switch_row`]'s switch sits: trailing, vertically centred,
/// in the retained switch's 44×24 proportions — the knob (`icon_md`) plus
/// its edge gap either side.
pub(super) fn switch_rect(rect: Rect, theme: &Theme) -> Rect {
    let knob = f64::from(theme.tokens.metrics.icon_md);
    let h = (knob * 1.2).min(rect.height());
    let w = h * 44.0 / 24.0;
    let cy = rect.center().y;
    Rect::new(rect.x1 - w, cy - h / 2.0, rect.x1, cy + h / 2.0)
}

/// The keyboard cursor: a ring around the control the next key will reach.
///
/// The editor paints its own panes, so it has no child widgets to inherit a
/// focus treatment from — this is the toolkit's own ring for a painted
/// surface, so the cursor reads the same here as anywhere else. It is drawn
/// just inside `rect`, so a row or band that butts against its neighbour
/// never lends the ring to the next one.
pub(super) fn draw_focus_ring(
    scene: &mut Scene,
    tf: Affine,
    theme: &Theme,
    rect: Rect,
    radius: f64,
) {
    if rect.width() <= 1.0 || rect.height() <= 1.0 {
        return;
    }
    let inset = 2.0;
    ui::widget::paint_focus_ring(
        scene,
        tf,
        rect.inflate(-inset, -inset),
        (radius - inset).max(0.0),
        theme,
    );
}

/// How far a slider's rail stays inside the row it is laid out across: the
/// knob at its largest (pressed) reaches past the rail's end by its radius,
/// and must stay inside the inspector's column rather than overhang it.
pub(super) fn slider_rail(track: Rect) -> Rect {
    let reach = S3;
    Rect::new(
        track.x0 + reach,
        track.y0,
        (track.x1 - reach).max(track.x0 + reach),
        track.y1,
    )
}

/// The 0..1 value a pointer at `x` sets on the slider laid out across
/// `track`: measured along the same rail the knob travels.
pub(super) fn slider_value_at(track: Rect, x: f64) -> f64 {
    let rail = slider_rail(track);
    ((x - rail.x0) / rail.width().max(1e-6)).clamp(0.0, 1.0)
}

/// Draw a slider across `track` through the toolkit's shared
/// [`ui::widget::Slider::paint`]. The knob travels [`slider_rail`], so at
/// either end it stays inside the row it belongs to.
pub(super) fn draw_track(
    scene: &mut Scene,
    tf: Affine,
    theme: &Theme,
    track: Rect,
    value: f64,
    hovered: bool,
    dragging: bool,
) {
    let rail = slider_rail(track);
    let cy = track.center().y;
    let half = f64::from(theme.tokens.metrics.icon_md) / 2.0 + 1.0;
    // The face insets its own track by a fixed padding; hand it the rail
    // grown by exactly that much so its track *is* the rail.
    let pad = 5.0;
    let box_rect = Rect::new(rail.x0 - pad, cy - half, rail.x1 + pad, cy + half);
    let face = ui::widget::SliderFace::new(box_rect, value)
        .hovered(hovered)
        .dragging(dragging);
    ui::widget::Slider::paint(scene, tf, theme, &face);
}

/// Draw a segmented control over caller-owned rects through the toolkit's
/// shared [`ui::widget::ToggleGroup::paint`], so the transition row and the
/// export dialog's option groups look like every other segmented control.
pub(super) fn draw_segmented(
    scene: &mut Scene,
    tf: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    segments: &[Rect],
    labels: &[&str],
    selected: usize,
    cursor: Point,
) {
    let hovered = segments.iter().position(|r| r.contains(cursor));
    let size = segments
        .first()
        .map_or(ControlSize::Regular, |r| control_size_for(*r, theme));
    let face = ui::widget::SegmentedFace::new(segments, labels, selected)
        .hovered(hovered)
        .control_size(size);
    ui::widget::ToggleGroup::paint(scene, tf, tr, theme, &face);
}

/// Where each part of the transport's readout goes, or `None` for a part
/// that does not fit **whole**.
///
/// The rule this encodes: a number the pane edge cuts mid-digit does not read
/// as a clipped number, it reads as a shorter one — a 42-second project
/// arriving as "00:4" states a length that is simply false. So a part that
/// cannot be drawn in full is not drawn at all, and the parts are dropped in
/// reverse order of what the user cannot get anywhere else: the playhead's
/// own time is the last to go, then the total, then what the playhead is
/// *doing*, and the static frame size first — the frame is also stated in the
/// inspector's PROJECT section, while "shuttling backwards at 8x" is stated
/// nowhere else at all.
#[derive(Debug, Default, PartialEq)]
#[expect(
    clippy::struct_field_names,
    reason = "each field is the x position of one readout part, named for the part"
)]
pub(super) struct ReadoutFit {
    pub(super) big_x: Option<f64>,
    pub(super) total_x: Option<f64>,
    /// What the playhead is doing, beside the total rather than over the top
    /// of the playhead time. The old "PLAYING" line was drawn 16px above the
    /// scrubber, which on this 58px strip is inside the 18px timecode's own
    /// line box — the two overlapped, and a longer shuttle readout made a
    /// pre-existing collision unmissable.
    pub(super) motion_x: Option<f64>,
    pub(super) meta_x: Option<f64>,
}

pub(super) fn readout_fit(
    transport: Rect,
    content_x: f64,
    big_w: f64,
    total_w: f64,
    motion_w: f64,
    meta_w: f64,
) -> ReadoutFit {
    let right_edge = transport.x1 - 12.0;
    // `content_x` is where the caller says the strip's right-hand content
    // may begin — past the history buttons when they are showing. Taking it
    // as an argument rather than assuming an offset is what keeps the
    // buttons and the timecode off each other; laid out apart, the readout
    // drew straight over them.
    let big_x = transport.x0 + content_x;
    let mut fit = ReadoutFit::default();
    // Nothing at all when even the playhead time would overrun.
    if big_x + big_w > right_edge {
        return fit;
    }
    fit.big_x = Some(big_x);
    let mut used_to = big_x + big_w;
    let total_x = used_to + 4.0;
    if total_x + total_w <= right_edge {
        fit.total_x = Some(total_x);
        used_to = total_x + total_w;
    }
    // Zero width is "the playhead is standing still", which places nothing.
    if motion_w > 0.0 {
        let motion_x = used_to + 14.0;
        if motion_x + motion_w <= right_edge {
            fit.motion_x = Some(motion_x);
            used_to = motion_x + motion_w;
        }
    }
    let meta_x = transport.x1 - 16.0 - meta_w;
    if meta_x > used_to + 16.0 {
        fit.meta_x = Some(meta_x);
    }
    fit
}

/// The key control at the head of an animatable inspector row: a diamond,
/// the standard mark for a keyframe.
///
/// Its three states have to be told apart at a glance, so they differ in
/// **two** ways each rather than only in colour: not animated is a small dim
/// outline, animated-but-between-keys is a larger primary-ink outline, and a key
/// on the playhead is a filled diamond with a ring. A control that
/// only changed hue would be unreadable to anyone who cannot separate those
/// hues, and indistinguishable in a screenshot.
pub(super) fn draw_key_control(
    scene: &mut Scene,
    tf: Affine,
    theme: &Theme,
    rect: Rect,
    state: KeyState,
    hovered: bool,
) {
    let c = &theme.tokens.colors;
    let ctr = rect.center();
    selection_row(
        scene,
        tf,
        theme,
        rect,
        ui::chrome::SelectionState::new(false, hovered, false),
    );
    let (r, col, filled) = match state {
        KeyState::Off => (4.0, c.text_tertiary, false),
        KeyState::Between => (5.5, c.text_primary, false),
        KeyState::OnKey => (5.5, c.text_primary, true),
    };
    let mut d = BezPath::new();
    d.move_to((ctr.x, ctr.y - r));
    d.line_to((ctr.x + r, ctr.y));
    d.line_to((ctr.x, ctr.y + r));
    d.line_to((ctr.x - r, ctr.y));
    d.close_path();
    if filled {
        scene.fill(Fill::NonZero, tf, col, None, &d);
        // The ring says "the playhead is exactly here", which is what makes
        // this the state that edits a key rather than making one.
        scene.stroke(
            &Stroke::new(1.0),
            tf,
            with_alpha(col, 0.5),
            None,
            &Circle::new(ctr, r + 3.0),
        );
    } else {
        scene.stroke(&Stroke::new(1.4), tf, col, None, &d);
    }
}

/// Whether this event ends a title edit, and whether it accepts.
///
/// `Some(true)` accepts, `Some(false)` cancels, `None` leaves the field to
/// handle the event itself. Kept out of the handler so the key contract is
/// one readable thing that a test can drive directly.
pub(super) fn title_edit_verdict(
    event: &WindowEvent,
    modifiers: winit::event::Modifiers,
) -> Option<bool> {
    let WindowEvent::KeyboardInput { event: key, .. } = event else {
        return None;
    };
    if key.state != ElementState::Pressed {
        return None;
    }
    match &key.logical_key {
        Key::Named(NamedKey::Escape) => Some(false),
        // Enter alone is a line break; with Ctrl (or Cmd) it accepts.
        Key::Named(NamedKey::Enter)
            if modifiers.state().control_key() || modifiers.state().super_key() =>
        {
            Some(true)
        }
        _ => None,
    }
}

/// The three positions each anchor axis can take, in source uv.
pub(super) const ANCHOR_STOPS: [f64; 3] = [0.0, 0.5, 1.0];

/// What a clip's tone curves amount to, in one line — or `None` when they
/// amount to nothing.
///
/// One function so the folded header and the reader's summary cannot drift
/// apart. It reports which channels have been shaped, and says plainly when
/// a shaped curve is turned down or bypassed: "RGB" and "RGB, bypassed" are
/// different states, and a summary that showed only the first would hide the
/// second.
pub(super) fn curve_summary(clip: &Clip, at: f64) -> Option<String> {
    use std::fmt::Write as _;
    let shaped = clip.curves.shaped_channels();
    if shaped.is_empty() {
        return None;
    }
    let names: Vec<&str> = shaped.iter().map(|c| c.label()).collect();
    let mut s = names.join(", ");
    // How much of the curve is mixed in *at the playhead*. The amount is an
    // animatable parameter, so the raw `curves.mix` is dead the moment it is
    // keyed — a grade ramping on reported itself as bypassed for its whole
    // length while the canvas beside it graded.
    let mix = clip.param_at(Param::CurveMix, at);
    if mix <= 1e-9 {
        s.push_str(", bypassed");
    } else if mix < 0.999 {
        // Writing into a `String` cannot fail.
        let _ = write!(s, " at {:.0}%", mix * 100.0);
    }
    Some(s)
}

/// How close the pointer has to be to a curve point, in curve units (the box
/// is 0..1 on both axes), to grab it rather than add a new one.
pub(super) const POINT_GRAB: f64 = 0.055;

/// How far outside the curve box a drag has to end for the point to be
/// removed. Generous, because a small overshoot at the edge of the box is an
/// ordinary way to pin a point to 0 or 1 and must not delete it.
pub(super) const POINT_DROP_MARGIN: f64 = 22.0;

/// Where curve coordinates land on the editor's box. Output runs **up**, so
/// the y axis is flipped: a curve that lifts is a curve that rises.
pub(super) fn curve_point(canvas: Rect, x: f64, y: f64) -> Point {
    Point::new(
        canvas.x0 + x.clamp(0.0, 1.0) * canvas.width(),
        canvas.y1 - y.clamp(0.0, 1.0) * canvas.height(),
    )
}

/// The inverse of [`curve_point`]: a pointer position as curve coordinates,
/// unclamped so a drag past the edge still reports where it went.
pub(super) fn curve_coords(canvas: Rect, p: Point) -> (f64, f64) {
    let w = canvas.width().max(1e-6);
    let h = canvas.height().max(1e-6);
    ((p.x - canvas.x0) / w, (canvas.y1 - p.y) / h)
}

/// The colour a channel's curve is drawn in.
///
/// Resolved from the theme rather than hardcoded to pure red/green/blue: the
/// OS owns the palette, and a raw `#f00` line is both off-theme and hard to
/// read on a light surface. The three are still unmistakably red-, green- and
/// blue-ish, because that is what the letters on the chooser say.
pub(super) fn curve_channel_color(theme: &Theme, ch: crate::curve::Channel) -> Color {
    let c = &theme.tokens.colors;
    match ch {
        crate::curve::Channel::Master => c.text_primary,
        crate::curve::Channel::Red => c.semantic.danger,
        crate::curve::Channel::Green => c.semantic.success,
        crate::curve::Channel::Blue => c.semantic.info,
    }
}

/// A title's authored colour as a Vello colour. Unlike a clip's palette slot
/// this is a real stored value — the user picked it and it must survive a
/// theme change — so it is converted, not resolved.
pub(super) fn title_color(c: [f64; 4]) -> Color {
    Color::new([c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32])
}

/// The quietest level a meter shows. Below -48 dBFS a signal is under the
/// noise of anything it is sitting with, so the bar has nothing useful to say
/// about it; spending half the bar's length down there would cost the top of
/// the scale, which is where every decision is made.
pub(super) const METER_FLOOR_DB: f64 = -48.0;

/// Where a meter changes colour, as fractions of its length.
///
/// -12 dB and -3 dB, which is the convention: amber says "you are into the
/// last twelve", red says "three left". Resolved through the toolkit's shared
/// [`ui::widget::GaugeThresholds`], so a meter's green, amber and red are the
/// same three tokens every other gauge in the fleet uses — including the
/// high-contrast theme, where they are not simply green, amber and red.
pub(super) fn meter_thresholds() -> ui::widget::GaugeThresholds {
    ui::widget::GaugeThresholds {
        warning: meter_frac(-12.0) as f32,
        danger: meter_frac(-3.0) as f32,
    }
}

/// A level, in decibels, as a fraction of a meter's length. Full scale is the
/// far end; anything over it pins there and lights the clip flag instead,
/// which is what makes "over" a state rather than a longer bar.
pub(super) fn meter_frac(db: f64) -> f64 {
    ((db - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0)
}

/// A linear amplitude as that fraction.
pub(super) fn meter_frac_linear(level: f32) -> f64 {
    if level <= 0.0 {
        return 0.0;
    }
    meter_frac(crate::model::linear_to_db(f64::from(level)))
}

/// A stereo level meter: two bars in `area`, left over right, with the
/// peak-hold marked and a clip flag at the far end.
///
/// Everything it draws it is *given* — the mixer measured these numbers on
/// the thread that made the sound. Nothing here reads a sample, allocates or
/// analyses anything, because this runs on the paint path.
///
/// The clip flag is latched by the meter, not by this function: it stays lit
/// after the level has fallen away, which is the whole difference between a
/// warning and a flicker you were not looking at.
pub(super) fn draw_level_meter(
    scene: &mut Scene,
    tf: Affine,
    theme: &Theme,
    area: Rect,
    level: [f32; 2],
    hold: f32,
    clipped: bool,
) {
    let c = &theme.tokens.colors;
    // The flag keeps the far end of the strip whether or not it is lit, so a
    // bar does not grow into it the moment the mix goes over.
    let flag_w = (area.width() * 0.06).clamp(3.0, 6.0);
    let bars = Rect::new(
        area.x0,
        area.y0,
        (area.x1 - flag_w - 2.0).max(area.x0),
        area.y1,
    );
    fill_round(scene, tf, bars, 1.5, c.intent.selection);
    let h = ((area.height() - 2.0) / 2.0).max(1.0);
    let thresholds = meter_thresholds();
    for (i, l) in level.iter().enumerate() {
        let frac = meter_frac_linear(*l);
        if frac <= 0.0 {
            continue;
        }
        let y0 = area.y0 + i as f64 * (h + 2.0);
        let bar = Rect::new(bars.x0, y0, bars.x0 + bars.width() * frac, y0 + h);
        fill_round(
            scene,
            tf,
            bar,
            (h / 2.0).min(1.5),
            thresholds.color(theme, frac as f32),
        );
    }
    // Peak-hold: the loudest this has been since the meter was cleared. It is
    // the number a person actually wants — the bar is where you are, the hold
    // is where you have been.
    let hold_frac = meter_frac_linear(hold);
    if hold_frac > 0.01 {
        let x = bars.x0 + bars.width() * hold_frac;
        fill_rect(
            scene,
            tf,
            Rect::new(x - 1.0, area.y0, x, area.y1),
            c.text_primary,
        );
    }
    let flag = Rect::new(area.x1 - flag_w, area.y0, area.x1, area.y1);
    let (fill, _) = if clipped {
        on_fill(theme, c.semantic.danger)
    } else {
        (c.intent.selection, c.text_dim)
    };
    fill_round(scene, tf, flag, 1.5, fill);
}

pub(super) fn hit_slider(track: Rect, p: Point) -> bool {
    p.x >= track.x0 - 8.0 && p.x <= track.x1 + 8.0 && (p.y - track.center().y).abs() < 14.0
}

pub(super) fn triangle_down(cx: f64, top: f64, s: f64) -> BezPath {
    let mut p = BezPath::new();
    p.move_to((cx - s / 2.0, top));
    p.line_to((cx + s / 2.0, top));
    p.line_to((cx, top + s));
    p.close_path();
    p
}
/// What one timeline paint needs to know about link groups: how many clips
/// each group holds and whether any of them is selected.
///
/// Built once per paint and handed down rather than asked per clip. "Is
/// anything linked to me" is a scan of every clip in the project, and asking it
/// inside the per-clip draw would make painting the lanes quadratic in the
/// project — the one thing the draw pass is not allowed to become.
#[derive(Default)]
pub(super) struct LinkMarks {
    /// `(group, members, any member selected)`, sorted by group so a lookup is
    /// a binary search rather than a scan.
    groups: Vec<(Uuid, u32, bool)>,
}

impl LinkMarks {
    #[allow(
        clippy::indexing_slicing,
        reason = "keyboard timeline indices are derived from the current non-empty project vectors before their paired reads"
    )]
    pub(super) fn build(project: &Project, selection: &Selection) -> Self {
        let mut groups: Vec<(Uuid, u32, bool)> = Vec::new();
        for clip in &project.clips {
            let Some(link) = clip.link else { continue };
            let chosen = selection.contains(clip.id);
            match groups.binary_search_by(|(g, _, _)| g.cmp(&link)) {
                Ok(i) => {
                    groups[i].1 += 1;
                    groups[i].2 |= chosen;
                }
                Err(i) => groups.insert(i, (link, 1, chosen)),
            }
        }
        Self { groups }
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "entry reads a fixed tuple encoded by build after the tuple length has been established"
    )]
    pub(super) fn entry(&self, clip: &Clip) -> Option<(u32, bool)> {
        let link = clip.link?;
        self.groups
            .binary_search_by(|(g, _, _)| g.cmp(&link))
            .ok()
            .map(|i| (self.groups[i].1, self.groups[i].2))
    }

    /// Whether this clip has at least one partner. A group of one is a clip,
    /// not a link, so it wears no mark.
    pub(super) fn linked(&self, clip: &Clip) -> bool {
        self.entry(clip).is_some_and(|(n, _)| n > 1)
    }

    /// Whether something in this clip's group is selected — what makes the
    /// *other* half show that it is implicated in the edit.
    pub(super) fn group_selected(&self, clip: &Clip) -> bool {
        self.entry(clip).is_some_and(|(n, sel)| n > 1 && sel)
    }
}

/// The chain glyph that marks a linked clip: two interlocking links, drawn
/// with the header's own foreground so it reads on every label colour and in
/// every theme.
pub(super) fn draw_chain(scene: &mut Scene, tf: Affine, color: Color, x: f64, y: f64) {
    let stroke = Stroke::new(1.3);
    for dx in [0.0, 5.0] {
        scene.stroke(
            &stroke,
            tf,
            color,
            None,
            &RoundedRect::from_rect(Rect::new(x + dx, y - 3.0, x + dx + 7.0, y + 3.0), 3.0),
        );
    }
}

/// The width the chain glyph reserves at the head of a clip's header.
pub(super) const CHAIN_W: f64 = 16.0;

pub(super) fn triangle_corner(x: f64, y: f64, s: f64) -> BezPath {
    let mut p = BezPath::new();
    p.move_to((x, y));
    p.line_to((x + s, y));
    p.line_to((x, y + s));
    p.close_path();
    p
}

/// A clip's, marker's or media item's label colour: the person's own tag on
/// their content, drawn from the presence series so every label is as
/// legible and as loud as every other.
pub(super) fn clip_color(theme: &Theme, palette: u8) -> Color {
    theme.tokens.colors.presence.series(usize::from(palette))
}

pub(super) fn with_alpha(c: Color, a: f64) -> Color {
    let alpha = a.clamp(0.0, 1.0) as f32;
    c.with_alpha(alpha)
}

/// Everything needed to paint one sampled window of an audio waveform.
///
/// Keeping the source interval and its media duration together prevents the
/// three adjacent time values at call sites from being silently transposed.
#[derive(Clone, Copy)]
pub(super) struct WaveformPaint<'a> {
    pub(super) area: Rect,
    pub(super) peaks: &'a [f32],
    pub(super) source_in: f64,
    pub(super) source_out: f64,
    pub(super) media_duration: f64,
    pub(super) volume: f64,
    pub(super) color: Color,
}

/// Draw a real audio waveform from normalized peaks into its target area,
/// mapping the requested source window into the peaks array so a trimmed clip
/// shows its own portion.
#[allow(
    clippy::indexing_slicing,
    reason = "waveform draw buckets are paired with their validated sample interval before each read"
)]
pub(super) fn draw_waveform(scene: &mut Scene, tf: Affine, paint: WaveformPaint<'_>) {
    if paint.area.width() < 3.0 || paint.area.height() < 2.0 || paint.peaks.is_empty() {
        return;
    }
    let bars = (paint.area.width() / 3.0).floor().max(1.0) as i64;
    let midy = paint.area.center().y;
    let half = (paint.area.height() / 2.0 - 2.0).max(1.0);
    let dur = paint.media_duration.max(1e-6);
    let vscale = 0.25 + paint.volume.clamp(0.0, 1.0) * 0.75;
    for i in 0..bars {
        let fx = (i as f64 + 0.5) / bars as f64;
        let x = paint.area.x0 + 2.0 + fx * (paint.area.width() - 4.0);
        let t = paint.source_in + fx * (paint.source_out - paint.source_in);
        let frac = (t / dur).clamp(0.0, 1.0);
        let idx = ((frac * paint.peaks.len() as f64) as usize).min(paint.peaks.len() - 1);
        let amp = (f64::from(paint.peaks[idx]) * half * vscale).max(0.5);
        fill_rect(
            scene,
            tf,
            Rect::new(x, midy - amp, x + 2.0, midy + amp),
            paint.color,
        );
    }
}

/// The placeholder an audio body shows when there is no real signal to draw:
/// a flat line down the middle.
///
/// It is deliberately flat. What stood here before was a sum of three sines
/// dressed up as an envelope — invented data drawn in the same ink as measured
/// data, which is the one thing a waveform must never do. A flat line says
/// "nothing measured yet"; a wiggle says "this is your audio", and would be a
/// lie whether the peaks were still computing or could never be read at all.
pub(super) fn draw_flat_signal(scene: &mut Scene, tf: Affine, area: Rect, color: Color) {
    if area.width() <= 2.0 || area.height() <= 2.0 {
        return;
    }
    let midy = area.center().y;
    fill_rect(
        scene,
        tf,
        Rect::new(area.x0 + 2.0, midy - 0.75, area.x1 - 2.0, midy + 0.75),
        with_alpha(color, 0.55),
    );
}

/// Fit a w:h aspect rect centered inside `area`.
pub(super) fn fit_aspect(area: Rect, w: f64, h: f64) -> Rect {
    if area.width() <= 0.0 || area.height() <= 0.0 {
        return area;
    }
    let target = w / h;
    let avail = area.width() / area.height();
    let (rw, rh) = if avail > target {
        (area.height() * target, area.height())
    } else {
        (area.width(), area.width() / target)
    };
    let cx = area.center().x;
    let cy = area.center().y;
    Rect::new(cx - rw / 2.0, cy - rh / 2.0, cx + rw / 2.0, cy + rh / 2.0)
}

/// `s` elided to `max_px` in `style`. Measured in the style it is drawn in:
/// measuring in one size and drawing in another under-elides.
pub(super) fn elide_at(s: &str, max_px: f32, style: &TextStyle, tr: &mut TextRenderer) -> String {
    tr.elide_to_width_style(s, max_px, style)
}

/// Choose a ruler tick spacing (seconds) so labels stay ~70px apart.
pub(super) fn nice_tick(px_per_sec: f64) -> f64 {
    let target_px = 76.0;
    let raw = target_px / px_per_sec.max(0.001);
    for step in [0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0] {
        if step >= raw {
            return step;
        }
    }
    600.0
}

/// The roles the editor's sheets set their text in.
pub(super) fn dialog_type(theme: &Theme) -> DialogType {
    DialogType {
        sm: theme.text(TextRole::Body),
        xs: theme.text(TextRole::Caption),
        xl: theme.text(TextRole::Title).numeric(),
    }
}

/// The measured height of one line set in `style`.
pub(super) fn line_height(tr: &mut TextRenderer, style: &TextStyle) -> f64 {
    f64::from(tr.measure_style("Hg", None, style).1)
}

pub(super) fn modal_layout_with_stacking_actions(
    viewport: Rect,
    spec: &ModalSpec<'_>,
    body_height: f64,
    tr: &mut TextRenderer,
    theme: &Theme,
) -> ModalLayout {
    let spec = spec.clone().body_height(body_height);
    let mut layout = modal_dialog::layout(viewport, &spec, tr, theme);
    let overflows = layout.actions.iter().any(|r| r.x0 < layout.body.x0 - 0.5);
    if !overflows || layout.actions.len() < 2 {
        return layout;
    }
    let gap = f64::from(theme.tokens.spacing.s2);
    let n = layout.actions.len() as f64;
    let height = layout.actions.first().map_or(0.0, Rect::height);
    let extra = (height + gap) * (n - 1.0);
    layout = modal_dialog::layout(viewport, &spec.body_height(body_height + extra), tr, theme);
    let bottom = layout.actions.first().map_or(layout.card.y1, |r| r.y1);
    // A grown card the viewport had to clamp shares what is left between
    // the stacked actions rather than letting them consume the whole body.
    let available = (bottom - layout.body.y0 - gap * (n - 1.0)).max(0.0);
    let height = height.min(available / n);
    let top = bottom - height * n - gap * (n - 1.0);
    layout.body.y1 = (top - gap).max(layout.body.y0);
    for (i, rect) in layout.actions.iter_mut().enumerate() {
        let y = top + i as f64 * (height + gap);
        *rect = Rect::new(layout.body.x0, y, layout.body.x1, y + height);
    }
    layout
}

/// A well inside content (the curve box, an effect row, the title's words):
/// the raised surface with the edge derived from it.
pub(super) fn well(scene: &mut Scene, tf: Affine, theme: &Theme, rect: Rect, radius: f64) {
    ui::surface::card(scene, tf, rect, radius, theme.tokens.colors.surfaces.raised);
}

/// The hover and selection lift of a row in one of the editor's lists.
pub(super) fn selection_row(
    scene: &mut Scene,
    tf: Affine,
    theme: &Theme,
    rect: Rect,
    state: ui::chrome::SelectionState,
) {
    ui::chrome::paint_row_selection(
        scene,
        tf,
        rect,
        state,
        theme.tokens.colors.surfaces.content,
        theme,
    );
}

pub(super) fn readout_tops(transport: Rect, cy: f64, heights: [f64; 4]) -> [f64; 4] {
    let [big, total, motion, meta] = heights;
    let big_top = (cy - big / 2.0).max(transport.y0 + 2.0);
    let foot = big_top + big;
    [big_top, foot - total, foot - motion, foot - meta]
}
