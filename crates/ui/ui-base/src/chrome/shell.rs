//! The shared vocabulary the OS chrome is drawn from. Shell chrome does not invent appearance
//! (`crates/ui/ui-core/HIG.md` §9): it selects a **role**, which resolves to tokens in
//! [`crate::tokens`].

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, RoundedRectRadii, Shape};
use vello::peniko::{BlendMode, Color, Compose, Fill, Mix};

use crate::color_compat::ColorCompat;
use crate::theme::Theme;
use crate::tokens::Colors;
use crate::tokens::colors::HAIRLINE_MIN_DELTA;

/// Alpha of the hairline that gives a lifted control a defined edge.
pub const HAIRLINE_ALPHA: u8 = 26;

/// Alpha of the edge around a floating [`ChromeRole::Panel`]. Heavier than
/// a control's hairline because a panel must separate itself from whatever
/// content it floats over, not just a surface already in view — on
/// high-contrast it's the *only* thing that does (see [`panel_ground`]).
pub const PANEL_BORDER_ALPHA: u8 = 92;

/// Smallest relative-luminance gap for a panel to read as its surface
/// rather than a slightly different patch of the shell — the toolkit's
/// hairline threshold, spelled once.
const PANEL_MIN_DELTA: f64 = HAIRLINE_MIN_DELTA;

/// Alphas [`panel_ground`] tries, ascending, when the declared overlay
/// tier is swallowed by a ground the surface sits on — a ladder rather
/// than one constant because relative luminance compresses heavily at the
/// dark end, so the same delta costs far more RGB there than on light.
const PANEL_GROUND_STEPS: &[u8] = &[16, 24, 32, 40, 56, 72, 96];

/// Alpha of the inset stroke that gives an active control its inner glow.
pub const GLOW_ALPHA: u8 = 51;

/// Fraction of the full active lift a hovered control reaches: the stock theme's hover response
/// (`crate::tokens::ResponseStep::HOVER_LIFT`).
pub const HOVER_LIFT_T: f32 = crate::tokens::ResponseStep::HOVER_LIFT;

/// Uniform scale a pressed control takes, about its centre: the stock theme's press response.
pub const PRESS_SCALE: f64 = crate::tokens::ResponseStep::PRESS_SCALE as f64;

/// What a chrome surface *is*.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChromeRole {
    /// Docked chrome fixed to a screen edge — the top bar.
    Bar,
    /// A floating surface opened over content — quick settings, the palette, a popover, a sheet.
    Panel,
    /// Something inside a Bar or Panel that you press — an action tile, a list row, a slider, a field.
    Control,
    /// One of a set you choose between — a tab pill, the tray pill, a workspace chip, a strip segment,
    /// a scope chip.
    Selector,
    /// A thing that has app content — a window card or tile.
    Object,
}

impl ChromeRole {
    /// `height` is used only by [`ChromeRole::Selector`], a capsule that
    /// resolves to half its height rather than the degenerate
    /// `radii.full` (9999), which breaks the rounded-rect path on a short
    /// rect.
    pub fn radius(self, theme: &Theme, height: f64) -> f64 {
        let radii = &theme.tokens.radii;
        match self {
            ChromeRole::Bar => 0.0,
            ChromeRole::Panel => f64::from(radii.panel),
            ChromeRole::Object => f64::from(radii.tile),
            ChromeRole::Control => f64::from(radii.control),
            ChromeRole::Selector => (height / 2.0).max(0.0),
        }
    }

    /// The role's opaque surface colour. Reads the *semantic* surface
    /// tokens, never a ramp slot, so it survives a theme whose author never
    /// saw this code. A Panel's body is painted in the floating material
    /// ([`crate::part_paint::material::paint_material`]); this is the colour
    /// that material tints with, and what it is where blur is unavailable.
    pub fn surface(self, theme: &Theme) -> Color {
        let colors = &theme.tokens.colors;
        match self {
            ChromeRole::Bar | ChromeRole::Panel => panel_ground(theme),
            ChromeRole::Object => colors.surfaces.content,
            ChromeRole::Control | ChromeRole::Selector => Color::TRANSPARENT,
        }
    }
}

/// The three inputs that decide how a chosen-from control looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelectionState {
    pub active: bool,
    pub hover: bool,
    pub pressed: bool,
}

impl SelectionState {
    pub fn new(active: bool, hover: bool, pressed: bool) -> Self {
        Self {
            active,
            hover,
            pressed,
        }
    }
    pub fn active(active: bool) -> Self {
        Self {
            active,
            hover: false,
            pressed: false,
        }
    }
}

/// How one control paints for one frame.
#[derive(Clone, Copy, Debug)]
pub struct SelectionTier {
    /// Alpha 0 for an idle control — callers skip the fill call rather
    /// than emitting a no-op draw per control per frame.
    pub fill: Color,
    pub border: Option<Color>,
    pub inner_glow: Option<Color>,
    pub scale: f64,
}

impl SelectionTier {
    /// Paint this tier on `rect` rounded by `radius`: the fill unless it is
    /// transparent, the border on the edge, and the inner glow one pixel
    /// inside it.
    pub fn paint(&self, scene: &mut Scene, transform: Affine, rect: Rect, radius: f64) {
        let shape = RoundedRect::from_rect(rect, radius);
        if self.fill.components[3] > 0.0 {
            scene.fill(Fill::NonZero, transform, self.fill, None, &shape);
        }
        let hairline = vello::kurbo::Stroke::new(1.0);
        if let Some(border) = self.border {
            scene.stroke(&hairline, transform, border, None, &shape);
        }
        if let Some(glow) = self.inner_glow {
            let inner = RoundedRect::from_rect(rect.inset(-1.0), (radius - 1.0).max(0.0));
            scene.stroke(&hairline, transform, glow, None, &inner);
        }
    }
}

/// Resolve one control's appearance. **This is the only way the shell
/// expresses "which one is chosen"** — active tab, active workspace,
/// active strip, selected row, chosen scope chip, current power profile.
///
/// | State | Fill | Border | Inner glow | Scale |
/// |---|---|---|---|---|
/// | idle | transparent | none | none | 1.0 |
/// | hover | `lift(0.5)` | none | none | 1.0 |
/// | active | `lift(1.0)` | hairline | hairline | 1.0 |
/// | pressed | as active | as active | as active | 0.97 |
///
/// Selection is a **lift**, never an accent fill: the accent is reserved
/// for live state the user did not choose (recording, sharing, a peer
/// joining, unread), and a selected control using it would collide with
/// that. An active control never gets more prominent on hover — the
/// contrast gain belongs to inactive controls brightening toward it —
/// and pressed implies the lifted tier before activation propagates, so a
/// click feels instant.
///
/// No shadow: elevation and selection are different vocabularies, and
/// rows of idle controls each casting one is what made the tab strip
/// read as muddy rather than as chrome.
pub fn selection_tier(theme: &Theme, state: SelectionState) -> SelectionTier {
    let colors = &theme.tokens.colors;
    let lifted = state.active || state.pressed;
    let response = theme.tokens.response;
    let step = response.step(state.hover, state.pressed);
    let base = colors.surfaces.base;
    let fill = if lifted && !state.pressed {
        colors.lift_on(base, 1.0)
    } else if state.pressed {
        colors.lift_tinted_on(base, response.press.lift, response.press.tint)
    } else if state.hover {
        colors.lift_tinted_on(base, response.hover.lift, response.hover.tint)
    } else {
        Color::TRANSPARENT
    };
    let ground = colors.surfaces.base;
    SelectionTier {
        fill,
        border: lifted.then(|| Colors::hairline_on(ground, HAIRLINE_ALPHA)),
        inner_glow: (lifted && step.glow > 0.0)
            .then(|| Colors::hairline_on(ground, glow_alpha(step.glow))),
        scale: f64::from(step.scale),
    }
}

/// Smallest OKLCH lightness step between a selected row and the ground it
/// sits on for the selection to read. Lightness, not relative
/// luminance: luminance compresses at the dark end, where a perfectly
/// visible step measures under a light-theme threshold.
pub const SELECTION_MIN_STEP: f32 = 0.06;

/// Share of the theme's hover lift a floored control's rest face sits at.
/// A theme whose hover is quieter than the stock half step
/// ([`crate::tokens::ResponseStep::LIFT_ADMITTED`] admits `0.25`) takes the
/// rest face down with it, so a hovered control never reads quieter than an
/// idle one on any theme.
pub const FLOOR_REST_SHARE: f32 = 0.7;

/// Where the rest face of a control with its floor sits on the ramp [`selection_floor_ramp`] walks from
/// the ground, in surface steps: under the stock hover lift, the calmest a floor goes while the theme's
/// hover stays at the stock half step.
pub const FLOOR_IDLE_T: f32 = crate::tokens::ResponseStep::HOVER_LIFT * FLOOR_REST_SHARE;

/// [`selection_tier`] for a control that sits on `ground` rather than on `surfaces.base`: a list row on
/// a sidebar, a menu row on [`panel_ground`], a card on a sheet.
pub fn selection_tier_on(theme: &Theme, state: SelectionState, ground: Color) -> SelectionTier {
    let colors = &theme.tokens.colors;
    let response = theme.tokens.response;
    let lifted = state.active || state.pressed;
    // The pointer's answer is the theme's response role; the active lift is
    // the selection tier and never moves with it.
    let step = response.step(state.hover, state.pressed);
    let fill = if state.active {
        colors.selected_on(ground)
    } else if state.pressed {
        colors.lift_tinted_on(ground, response.press.lift, response.press.tint)
    } else if state.hover {
        colors.lift_tinted_on(ground, response.hover.lift, response.hover.tint)
    } else {
        Color::TRANSPARENT
    };
    SelectionTier {
        fill,
        border: lifted.then(|| Colors::hairline_on(ground, HAIRLINE_ALPHA)),
        inner_glow: (lifted && step.glow > 0.0)
            .then(|| Colors::hairline_on(ground, glow_alpha(step.glow))),
        scale: f64::from(step.scale),
    }
}

/// A response's glow as the hairline alpha it draws at: `glow` of an
/// opaque line, `0..=0.5` of 255.
fn glow_alpha(glow: f32) -> u8 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "glow is 0..=0.5, so the product is 0..=128, which fits u8"
    )]
    let alpha = (glow.clamp(0.0, 0.5) * 255.0).round() as u8;
    alpha
}

/// How far a row's hover and selection lift, and a zebra stripe, bleed past
/// the column's content edge on each side (`s2`).
///
/// One edge rule has for every content list: a row's text, a section's
/// title, a property list's labels and a table's first column all start on
/// the column's content edge, and the lift is `row_bleed` outside it, so
/// text never moves when its row is selected and a list lines up with the
/// headings around it. A face takes the row's outer rect (the content edge
/// grown by the bleed); a retained row grows its layout box by it.
pub fn row_bleed(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s2)
}

/// The gutter a disclosure chevron hangs in, left of the content it opens (`icon_sm` and the gap to
/// what follows it): the one step a list indents a nested row by, so a row's chevron is in the gutter
/// of it depth and the rows' content keeps one column.
pub fn disclosure_gutter(theme: &Theme) -> f64 {
    f64::from(theme.tokens.metrics.icon_sm) + f64::from(theme.tokens.spacing.rhythm.related)
}

/// The content edge of a window's overview or content column, from the column's leading edge: where the
/// shell starts the column's header title, and so where the column's rows, headings and fields start
/// their text.
pub fn column_gutter(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.rhythm.group)
}

/// Paint the selection of one row of a list: the hover or active lift of `ground`
/// ([`selection_tier_on`]) on `rect` with the item radius (`radii.item`), and nothing for an idle row.
pub fn paint_row_selection(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    state: SelectionState,
    ground: Color,
    theme: &Theme,
) {
    paint_row_selection_joined(scene, transform, rect, state, ground, theme, RowJoin::NONE);
}

/// Which neighbours of a selected row are selected too.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowJoin {
    pub above: bool,
    /// The row below is lifted too.
    pub below: bool,
}

impl RowJoin {
    pub const NONE: Self = Self {
        above: false,
        below: false,
    };

    pub fn new(above: bool, below: bool) -> Self {
        Self { above, below }
    }
}

/// [`paint_row_selection`] for a row inside a run of selected rows: the corners shared with a lifted
/// neighbour are square, so adjacent selected rows join into one lift (a multi-selection in a mail
/// list).
pub fn paint_row_selection_joined(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    state: SelectionState,
    ground: Color,
    theme: &Theme,
    join: RowJoin,
) {
    if !(state.active || state.pressed || state.hover) || rect.width() <= 0.0 {
        return;
    }
    let fill = selection_tier_on(theme, state, ground).fill;
    let radius = f64::from(theme.tokens.radii.item).min(rect.height() / 2.0);
    let top = if join.above { 0.0 } else { radius };
    let bottom = if join.below { 0.0 } else { radius };
    scene.fill(
        Fill::NonZero,
        transform,
        fill,
        None,
        &RoundedRect::from_rect(
            rect,
            vello::kurbo::RoundedRectRadii::new(top, top, bottom, bottom),
        ),
    );
}

/// The face of a control that has its floor (a filled rest face rather
/// than a transparent one), for `state`, on `ground`: one ordered
/// ramp from the ground, so rest, hover and active are each further from
/// the ground than the last on every material, and no state mixes a floor
/// with the transparent tier.
///
/// Every rung is the ground [`Colors::lift_on`] — the same OKLCH ramp
/// and the same theme response the hover tier takes
/// ([`selection_tier`], [`selection_tier_on`]) — and the rest face sits at a
/// [`FLOOR_REST_SHARE`] of the theme's hover lift, never above it. A control
/// with a floor and a control without therefore agree on how loud a hover
/// is, and a palette whose surface ladder is squeezed near black cannot lift
/// an idle face past a hovered one.
pub fn selection_floor_ramp(theme: &Theme, state: SelectionState, ground: Color) -> Color {
    let colors = &theme.tokens.colors;
    // The chosen face is the selection tier, never the response's.
    let t = if state.active || state.pressed {
        1.0
    } else if state.hover {
        theme.tokens.response.hover.lift
    } else {
        FLOOR_IDLE_T.min(theme.tokens.response.hover.lift * FLOOR_REST_SHARE)
    };
    colors.lift_on(ground, t)
}

/// The rest face of a control with its floor on `ground`: the idle rung of [`selection_floor_ramp`],
/// for the painters that keep the floor under every state themselves.
pub fn selection_floor(theme: &Theme, ground: Color) -> Color {
    selection_floor_ramp(theme, SelectionState::default(), ground)
}

/// An alignment guide: the line a dragged object lines up with while it is near enough to snap (a
/// display on the layout canvas, a shape on a slide), from `from` to `to`.
pub fn paint_alignment_guide(
    scene: &mut Scene,
    transform: Affine,
    from: Point,
    to: Point,
    theme: &Theme,
) {
    scene.stroke(
        &vello::kurbo::Stroke::new(f64::from(theme.tokens.metrics.chrome_rule_px)),
        transform,
        theme.tokens.colors.intent.accent_ink,
        None,
        &vello::kurbo::Line::new(from, to),
    );
}

/// OKLCH lightness distance between two colours.
pub fn lightness_step(a: Color, b: Color) -> f32 {
    use crate::color::Oklch;
    (Oklch::from_color(a).l - Oklch::from_color(b).l).abs()
}

/// What a display's edge says ([`paint_display_edge`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayEdge {
    /// A call rings on another display.
    Ringing,
    /// A visual alert, along every display.
    Alert,
}

/// Paint `edge` along the inside of `display`'s rectangle.
pub fn paint_display_edge(
    scene: &mut Scene,
    transform: Affine,
    display: Rect,
    edge: DisplayEdge,
    theme: &Theme,
) {
    let metrics = &theme.tokens.metrics;
    let (width, colour) = match edge {
        DisplayEdge::Ringing => (
            f64::from(metrics.call_edge_px),
            theme.tokens.colors.border_focus,
        ),
        DisplayEdge::Alert => (
            f64::from(metrics.alert_edge_px),
            theme.tokens.colors.intent.accent,
        ),
    };
    scene.stroke(
        &vello::kurbo::Stroke::new(width),
        transform,
        colour,
        None,
        &display.inset(-width / 2.0),
    );
}

/// The shell ground: what shell chrome drawn over the whole shell (the overview's pictured windows
/// and their captions) sits on.
#[must_use]
pub fn desktop_ground(theme: &Theme) -> Color {
    theme.tokens.colors.surfaces.base
}

/// The opaque colour a chrome surface is built from: the floating level,
/// `surfaces.overlay`, verbatim (`docs/design-language.md`, "Surfaces"). A
/// floating surface separates by its level, its two-layer shadow and its
/// inner hairline together, never by drifting off its level.
///
/// High contrast is the exception: there the floating level is the ground
/// itself, so the ground is walked away from it (same luminance reasoning
/// as [`Colors::hairline_on`]) until it clears the shell and the window;
/// [`PANEL_BORDER_ALPHA`] is the edge half of the same fix.
pub fn panel_ground(theme: &Theme) -> Color {
    use crate::a11y::relative_luminance;
    let colors = &theme.tokens.colors;
    let base = colors.surfaces.base;
    let declared = colors.surfaces.overlay;
    if !colors.palette.high_contrast {
        return declared;
    }
    let grounds = [base, colors.surfaces.content];
    let clears = |c: Color| {
        grounds
            .iter()
            .all(|g| (relative_luminance(c) - relative_luminance(*g)).abs() >= PANEL_MIN_DELTA)
    };
    if clears(declared) {
        return declared;
    }
    let mut last = declared;
    for alpha in PANEL_GROUND_STEPS {
        let step = Colors::hairline_on(base, *alpha);
        let a = step.components[3];
        let d = declared.components;
        last = Color::new([
            step.components[0] * a + d[0] * (1.0 - a),
            step.components[1] * a + d[1] * (1.0 - a),
            step.components[2] * a + d[2] * (1.0 - a),
            1.0,
        ]);
        if clears(last) {
            return last;
        }
    }
    last
}

/// A Panel's derived edge: [`panel_border`], stroked half a pixel inside
/// its outline so the 1 px edge is crisp and stays the panel
/// outermost pixel row.
pub fn paint_panel_edge(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    radius: f64,
    theme: &Theme,
) {
    scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        transform,
        panel_border(theme),
        None,
        &crate::surface::edge_outline(rect, radius),
    );
}

/// A derived hairline round `shape` on `ground`: a whole-row target that should read as one without a
/// body of it.
pub fn paint_hairline_outline(
    scene: &mut Scene,
    transform: Affine,
    shape: &RoundedRect,
    ground: Color,
) {
    scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        transform,
        Colors::hairline_on(ground, HAIRLINE_ALPHA),
        None,
        shape,
    );
}

/// The edge of a floating [`ChromeRole::Panel`], stroked on its outermost
/// pixel row: the design language's inner hairline (white at 7 % on a dark
/// material, black at 8 % on a light one,
/// [`crate::tokens::Elevation::floating_edge`]). High contrast draws the
/// full-strength edge ([`PANEL_BORDER_ALPHA`]), where it is the only thing
/// separating a panel.
pub fn panel_border(theme: &Theme) -> Color {
    if theme.tokens.colors.palette.high_contrast {
        Colors::hairline_on(theme.tokens.colors.surfaces.base, PANEL_BORDER_ALPHA)
    } else {
        theme.tokens.elevation.floating_edge
    }
}

/// Lay down a floating panel's elevation, clipped to the ring *outside*
/// its silhouette. The shadow is [`crate::surface::shadow`], a blur of
/// the panel rounded rect (so it stays even around a corner instead
/// of showing a flat-edged ghost of a square one). The ring clip stays
/// even though chrome is opaque: a blurred rect is solid in the middle,
/// and a half-faded panel (`intensity`) must not darken its interior
/// on the way in.
pub fn panel_shadow(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    radius: f64,
    theme: &Theme,
    intensity: f32,
) {
    if intensity <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let e = &theme.tokens.elevation.floating;
    // The visible falloff ends about 2.5 standard deviations out; the ring
    // is given a little more than that so nothing is clipped mid-ramp.
    let reach = crate::surface::std_dev_for(e.blur) * 3.0 + f64::from(e.offset_y);
    let outer = Rect::new(
        rect.x0 - reach,
        rect.y0 - reach,
        rect.x1 + reach,
        rect.y1 + reach,
    );
    let mut ring = BezPath::new();
    ring.extend(outer.path_elements(0.1));
    ring.extend(RoundedRect::from_rect(rect, radius).path_elements(0.1));
    scene.push_layer(Fill::EvenOdd, BlendMode::default(), 1.0, transform, &ring);
    crate::surface::floating_shadow(
        scene,
        transform,
        rect,
        radius,
        intensity.clamp(0.0, 1.0),
        theme,
    );
    scene.pop_layer();
}

/// A painted floating Panel: its outline, for callers that clip their
/// content to it, and the theme to draw that content with.
#[must_use = "draw the panel's contents with `theme`, not the window's"]
pub struct PaintedPanel {
    /// The body's outline at the Panel radius.
    pub body: RoundedRect,
    /// The window theme as seen from the [`panel_ground`] ([`Theme::on_ground`]): text tiers measured
    /// against the panel, and fields and secondary buttons stepped off it as they step off a window.
    pub theme: Theme,
}

/// Paint a floating [`ChromeRole::Panel`] body: its shadow at
/// `shadow_intensity`, the [`panel_ground`] fill and the 1 px
/// [`panel_border`], all at the Panel radius. Returns the body's outline
/// and the theme the panel's contents are drawn with.
pub fn paint_panel(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    theme: &Theme,
    shadow_intensity: f32,
) -> PaintedPanel {
    let radius = ChromeRole::Panel.radius(theme, rect.height());
    panel_shadow(scene, transform, rect, radius, theme, shadow_intensity);
    let body = RoundedRect::from_rect(rect, radius);
    scene.fill(Fill::NonZero, transform, panel_ground(theme), None, &body);
    scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        transform,
        panel_border(theme),
        None,
        &body,
    );
    PaintedPanel {
        body,
        theme: on_panel(theme),
    }
}

/// The theme a floating Panel's contents are drawn with: `theme` as seen from the [`panel_ground`]
/// ([`Theme::on_ground`]).
#[must_use]
pub fn on_panel(theme: &Theme) -> Theme {
    theme.on_ground(panel_ground(theme))
}

/// A separator or edge drawn on a chrome surface.
pub fn chrome_hairline(theme: &Theme) -> Color {
    Colors::hairline_on(theme.tokens.colors.surfaces.overlay, HAIRLINE_ALPHA)
}

/// Share of a scrim's strength that the flat *level* stage takes; the *tone* stage takes the whole of
/// it.
pub const SCRIM_LEVEL_SHARE: f32 = 0.55;

/// Paint a scrim over `rect` at `strength`, in the theme scrim hue.
///
/// A flat wash dims by an amount that depends on the ground
/// lightness: measured on a real frame, a flat 0.45 wash took 39.6 L* off
/// a `bg-white` page (252→143) and 1.5 L* off the shell beside it
/// (20→16) — the page read as blanked out and nothing else read as dimmed
/// — and flattened the 12 code values between the page and a rule on it
/// down to 7.
///
/// So this is two fills instead of one: a soft-light **tone** stage (`Cb²`
/// against black, a proportional move that dims the middle/bottom without
/// flattening the top) and a flat **level** stage in the theme's scrim hue
/// at [`SCRIM_LEVEL_SHARE`] of `strength`. Together they take a uniform
/// 21-23 L* off every ground from `#ffffff` to mid grey. Blend modes need
/// a backdrop, so call this after whatever it dims is already drawn into
/// the same `Scene`.
pub fn fill_scrim(scene: &mut Scene, transform: Affine, rect: Rect, strength: f32, theme: &Theme) {
    let strength = strength.clamp(0.0, 1.0);
    if strength <= 0.0 {
        return;
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "explicitly clamped to 0.0..=255.0 immediately above the cast"
    )]
    let tone = (strength * 255.0).round().clamp(0.0, 255.0) as u8;
    if tone > 0 {
        scene.push_layer(
            Fill::NonZero,
            BlendMode::new(Mix::SoftLight, Compose::SrcOver),
            1.0,
            transform,
            &rect,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            Color::from_rgba8(0, 0, 0, tone),
            None,
            &rect,
        );
        scene.pop_layer();
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "explicitly clamped to 0.0..=255.0 immediately above the cast"
    )]
    let level = (strength * SCRIM_LEVEL_SHARE * 255.0)
        .round()
        .clamp(0.0, 255.0) as u8;
    if level > 0 {
        let c = theme.tokens.colors.surfaces.scrim.to_rgba8();
        scene.fill(
            Fill::NonZero,
            transform,
            Color::from_rgba8(c.r, c.g, c.b, level),
            None,
            &rect,
        );
    }
}

/// What [`fill_scrim`] does to one 8-bit grey behind it, as a pure function
/// so the property the two fills are for can be asserted without a GPU.
/// Must mirror the shader: soft light against black is `Cb²` (W3C
/// compositing) composited `lerp(Cb, Cb², αs)`, then an ordinary
/// source-over of the theme's scrim hue for the level stage.
pub fn scrim_maps(value: u8, strength: f32, theme: &Theme) -> u8 {
    let hue = theme.tokens.colors.surfaces.scrim.to_rgba8();
    let hue_grey =
        (0.2126 * f64::from(hue.r) + 0.7152 * f64::from(hue.g) + 0.0722 * f64::from(hue.b)) / 255.0;
    scrim_value(f64::from(value) / 255.0, strength, hue_grey)
}

/// What [`fill_scrim`] does to one 8-bit channel behind it, given that
/// channel of the scrim hue: the same tone and level stages, for a host
/// that composites a scrim on the CPU (a cached frame) instead of through
/// the shader.
#[must_use]
pub fn scrim_channel(value: u8, strength: f32, hue: u8) -> u8 {
    scrim_value(f64::from(value) / 255.0, strength, f64::from(hue) / 255.0)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "explicitly clamped to 0.0..=1.0 immediately before the *255.0 scale, \
              so the final value is always within 0..=255"
)]
fn scrim_value(v: f64, strength: f32, hue: f64) -> u8 {
    let strength = f64::from(strength.clamp(0.0, 1.0));
    let tone = (strength * 255.0).round().clamp(0.0, 255.0) / 255.0;
    let toned = v * (1.0 - tone) + v * v * tone;
    let level = (strength * f64::from(SCRIM_LEVEL_SHARE) * 255.0)
        .round()
        .clamp(0.0, 255.0)
        / 255.0;
    let out = toned * (1.0 - level) + hue * level;
    (out.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y::relative_luminance;

    fn themes() -> Vec<(&'static str, Theme)> {
        vec![
            ("dark", Theme::dark()),
            ("light", Theme::light()),
            ("high_contrast", Theme::high_contrast()),
        ]
    }

    tilo_test_env::scenario! {
        fn chrome_surfaces_selection_and_scrim_follow_the_theme;

        // A secondary button or field on a floating panel has the same visible step off the panel in dark
        // as in light: the panel hands its contents a theme whose ground is the panel, so `raised` steps
        // off the panel instead of the window (the dark overlay rung sits right under the raised one).
        phase a_panels_contents_stand_on_the_panel;
        {
            use crate::color::Oklch;
            let l = |c: Color| Oklch::from_color(c).l;
            for (name, theme) in themes() {
                let colors = &theme.tokens.colors;
                let window_step = (l(colors.surfaces.raised) - l(colors.surfaces.content)).abs();
                let mut scene = Scene::new();
                let painted = paint_panel(
                    &mut scene,
                    Affine::IDENTITY,
                    Rect::new(0.0, 0.0, 320.0, 200.0),
                    &theme,
                    1.0,
                );
                let ground = panel_ground(&theme);
                let on = &painted.theme.tokens.colors;
                assert_eq!(
                    on.surfaces.content, ground,
                    "{name}: contents stand on the panel"
                );
                let panel_step = (l(on.surfaces.raised) - l(ground)).abs();
                assert!(
                    panel_step + 1e-4 >= window_step / 2.0,
                    "{name}: a secondary face stands {panel_step:.3} off the panel, \
                     {window_step:.3} off the window"
                );
            }
        }

        // A control with its floor (an idle tile, a list row on a panel)
        // must never look louder at rest than it does under the pointer, and
        // every other control answers the pointer with the hover tier. So the
        // floor's rest face is measured against that tier on the same ground —
        // including for a theme whose own hover is quieter than the stock one.
        phase a_floored_rest_face_is_quieter_than_a_hovered_one;
        {
            use crate::theme::{Preset, PresetDesign};
            use crate::tokens::{Response, ResponseStep};
            let quiet = {
                let mut document = Preset::Dark.document();
                document.response = Some(Response {
                    hover: ResponseStep {
                        lift: *ResponseStep::LIFT_ADMITTED.start(),
                        ..Response::STOCK.hover
                    },
                    ..Response::STOCK
                });
                Theme::from_document(&document).expect("a hover lift inside its admitted band")
            };
            for (name, theme) in themes().into_iter().chain([("quiet_hover", quiet)]) {
                let colors = &theme.tokens.colors;
                let hover_state = SelectionState::new(false, true, false);
                for (ground_name, ground) in [
                    ("base", colors.surfaces.base),
                    ("content", colors.surfaces.content),
                    ("panel", panel_ground(&theme)),
                    ("white", Color::WHITE),
                    ("black", Color::BLACK),
                ] {
                    let dist = |c: Color| (relative_luminance(c) - relative_luminance(ground)).abs();
                    let rest = selection_floor(&theme, ground);
                    for (tier_name, hovered) in [
                        (
                            "floor ramp",
                            selection_floor_ramp(&theme, hover_state, ground),
                        ),
                        (
                            "hover tier",
                            selection_tier_on(&theme, hover_state, ground).fill,
                        ),
                    ] {
                        // At the black end a half step can round to the ground
                        // itself; the rest face still never passes it.
                        assert!(
                            dist(rest) <= dist(hovered),
                            "{name}/{ground_name}: rest ({:.4}) must stay under the {tier_name} \
                             ({:.4}) from its ground",
                            dist(rest),
                            dist(hovered)
                        );
                        assert!(
                            dist(hovered) <= 0.0 || dist(rest) < dist(hovered),
                            "{name}/{ground_name}: a {tier_name} that registers ({:.4}) must read \
                             above the rest face ({:.4})",
                            dist(hovered),
                            dist(rest)
                        );
                    }
                }
            }
        }

        phase a_dark_theme_scrim_dims_the_mid_tones_of_dark_content;
        {
            // Over dark content the grounds are near black and cannot give up
            // much; a sheet reads as lifted only when the rows, panels and map
            // roads in the mid tones move. The dark theme's strength makes them.
            let dark = Theme::dark();
            let light = Theme::light();
            let strength = |t: &Theme| t.tokens.colors.surfaces.scrim_strength;
            assert!(strength(&dark) > strength(&light));
            for v in [64u8, 96, 128] {
                let drop = lstar(v) - lstar(scrim_maps(v, strength(&dark), &dark));
                assert!(
                    drop >= 15.0,
                    "a dark-theme scrim takes only {drop:.1} L* off a mid tone at {v}"
                );
            }
        }
    }

    /// Perceived lightness of an 8-bit grey, CIE L\*. A scrim's job is
    /// judged in what the eye loses, not the code value — a flat wash takes
    /// the same fraction of every code value, which is why it reads as two
    /// different amounts of dimming on a light page versus a dark shell.
    fn lstar(v: u8) -> f64 {
        let s = f64::from(v) / 255.0;
        let y = if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        };
        if y > 0.008_856 {
            116.0 * y.cbrt() - 16.0
        } else {
            903.3 * y
        }
    }
}

/// Geometry and appearance for the **strip ribbon** — the indicator for
/// which strip of the current workspace has focus, and how many there are
/// (HIG §9.10, §9.11). Every strip renders at once sharing the content
/// height, so this means **the strip with focus** (where a new window
/// joins, where the keyboard is aimed), not "the strip you can see".
///
/// Sits on the content's right edge because the workspace disc owns the
/// left and horizontal scrolling belongs to groups and tabs — one level
/// of the model per edge.
#[derive(Clone, Copy, Debug)]
pub struct StripRibbon {
    pub content: Rect,
    pub count: usize,
    pub focused: usize,
    /// How long the New strip button is along the edge: its plus and its words, as long as they
    /// measure in the person's language where it is drawn. Nothing stands there until they did.
    pub new_len: f64,
}

/// A tab at the screen's right edge, wide enough to carry its strip's number: a
/// sliver there read as a scrollbar's thumb.
pub const RIBBON_SEG_W: f64 = 24.0;
pub const RIBBON_SEG_H: f64 = 22.0;
/// Longer than [`RIBBON_SEG_H`], so the ribbon reads at a glance without needing colour to carry the
/// signal — what keeps it legible on high-contrast.
pub const RIBBON_SEG_H_FOCUSED: f64 = 40.0;
/// Sized so segment height plus gap equals [`RIBBON_HIT_W`]: adjacent hit rects tile without
/// overlapping, so a click near a boundary belongs to exactly one strip.
pub const RIBBON_GAP: f64 = RIBBON_HIT_W - RIBBON_SEG_H;
/// Flush with the screen's right edge: the segments are an edge mark, and a gap between them and the
/// edge read as a stray, misplaced control.
pub const RIBBON_MARGIN: f64 = 0.0;
/// Minimum pointer target (HIG §9.6), reaching inward past the tab.
pub const RIBBON_HIT_W: f64 = 32.0;
/// Between the last tab's pointer target and the New strip button under it.
pub const RIBBON_NEW_GAP: f64 = crate::tokens::Spacing::BASE.s2 as f64;

const RIBBON_IDLE_ALPHA: u8 = 46;
const RIBBON_HOVER_ALPHA: u8 = 96;
const RIBBON_FOCUSED_ALPHA: u8 = 224;

/// Can't reuse [`selection_tier`] unmodified — it lets "unselected" be
/// transparent because a row of controls supplies context, but a ribbon
/// segment has no row, so an unfocused one at zero alpha would be the only
/// evidence of a second strip, invisible. A floor alone isn't enough
/// either: on high-contrast, `surfaces.base`/`raised` are both pure black,
/// so a floored unfocused segment came out *brighter than the focused
/// one*. Hence three explicit, hand-ordered weights instead of a derived
/// tier.
fn ribbon_fill(theme: &Theme, focused: f64, hover: bool) -> Color {
    let rest = if hover {
        RIBBON_HOVER_ALPHA
    } else {
        RIBBON_IDLE_ALPHA
    };
    Colors::hairline_on(
        theme.tokens.colors.surfaces.base,
        weighted_alpha(rest, RIBBON_FOCUSED_ALPHA, focused),
    )
}

fn weighted_alpha(rest: u8, focused: u8, weight: f64) -> u8 {
    let blended = f64::from(rest) + (f64::from(focused) - f64::from(rest)) * weight.clamp(0.0, 1.0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a blend of two u8 values, rounded, stays inside u8"
    )]
    let blended = blended.round() as u8;
    blended
}

fn focus_weight(i: usize, focus: f64) -> f64 {
    (1.0 - (crate::num::f64_of_usize(i) - focus).abs()).clamp(0.0, 1.0)
}

fn segments_extent(
    count: usize,
    focused_index: usize,
    segment: f64,
    focused_segment: f64,
    gap: f64,
) -> f64 {
    if count == 0 {
        return 0.0;
    }
    let focused = usize::from(focused_index < count);
    // `count >= 1` here (checked above), and `focused` is 0 or 1, so this
    // never underflows; saturating_sub documents that rather than relying on
    // the reader to re-derive it.
    let plain = count.saturating_sub(focused);
    #[allow(
        clippy::cast_precision_loss,
        reason = "tab/strip counts are tiny, far below where f64 loses \
                  integer precision"
    )]
    {
        plain as f64 * segment
            + focused as f64 * focused_segment
            + (count.saturating_sub(1)) as f64 * gap
    }
}

fn sliding_segment_length(i: usize, focus: f64, segment: f64, focused_segment: f64) -> f64 {
    segment + (focused_segment - segment) * focus_weight(i, focus)
}

fn sliding_segment_span(
    i: usize,
    (count, focus): (usize, f64),
    center: f64,
    (segment, focused_segment, gap): (f64, f64, f64),
) -> (f64, f64) {
    let extent = segments_extent(count, 0, segment, focused_segment, gap);
    let mut start = center - extent / 2.0;
    for k in 0..i {
        start += sliding_segment_length(k, focus, segment, focused_segment) + gap;
    }
    (
        start,
        sliding_segment_length(i, focus, segment, focused_segment),
    )
}

impl StripRibbon {
    pub fn extent(&self) -> f64 {
        segments_extent(
            self.count,
            self.focused,
            RIBBON_SEG_H,
            RIBBON_SEG_H_FOCUSED,
            RIBBON_GAP,
        )
    }

    pub fn segment_rect(&self, i: usize) -> Option<Rect> {
        self.segment_rect_at(i, crate::num::f64_of_usize(self.focused))
    }

    /// [`Self::segment_rect`] while the focus slides: `focus` is between two segments (the motion's
    /// value), and the long segment travels from one to the other.
    pub fn segment_rect_at(&self, i: usize, focus: f64) -> Option<Rect> {
        if i >= self.count {
            return None;
        }
        let x1 = self.content.x1 - RIBBON_MARGIN;
        let x0 = x1 - RIBBON_SEG_W;
        let (y, h) = sliding_segment_span(
            i,
            (self.count, focus),
            self.content.center().y,
            (RIBBON_SEG_H, RIBBON_SEG_H_FOCUSED, RIBBON_GAP),
        );
        Some(Rect::new(x0, y, x1, y + h))
    }

    /// Widened past the visible segment to the minimum pointer target, inward from the screen edge the
    /// segment sits on.
    pub fn hit_rect(&self, i: usize) -> Option<Rect> {
        let seg = self.segment_rect(i)?;
        let x1 = self.content.x1;
        Some(Rect::new(
            x1 - RIBBON_HIT_W,
            seg.y0 - RIBBON_GAP / 2.0,
            x1,
            seg.y1 + RIBBON_GAP / 2.0,
        ))
    }

    pub fn hit(&self, p: vello::kurbo::Point) -> Option<usize> {
        (0..self.count).find(|i| self.hit_rect(*i).is_some_and(|r| r.contains(p)))
    }

    /// Where the New strip button stands: a tab as wide as the strips' own, flush with the edge,
    /// running [`Self::new_len`] down it a small step under the last tab's pointer target; in a
    /// workspace with no strip yet, in the middle of the edge. Nowhere until its words measured.
    pub fn new_strip_rect(&self) -> Option<Rect> {
        if self.new_len <= 0.0 {
            return None;
        }
        let y0 = match self.count.checked_sub(1) {
            Some(last) => self.hit_rect(last)?.y1 + RIBBON_NEW_GAP,
            None => self.content.center().y - self.new_len / 2.0,
        };
        let x1 = self.content.x1 - RIBBON_MARGIN;
        Some(Rect::new(x1 - RIBBON_SEG_W, y0, x1, y0 + self.new_len))
    }

    /// The New strip button's pointer target: reaching inward as the tabs' do.
    pub fn new_strip_hit_rect(&self) -> Option<Rect> {
        let face = self.new_strip_rect()?;
        Some(Rect::new(
            self.content.x1 - RIBBON_HIT_W,
            face.y0,
            self.content.x1,
            face.y1,
        ))
    }

    pub fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        hover: Option<usize>,
        alpha: f32,
    ) {
        self.draw_at(
            scene,
            transform,
            theme,
            (hover, alpha),
            crate::num::f64_of_usize(self.focused),
        );
    }

    /// [`Self::draw`] with the focus at `focus`, sliding between segments.
    pub fn draw_at(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        (hover, alpha): (Option<usize>, f32),
        focus: f64,
    ) {
        if self.count == 0 || alpha <= 0.0 {
            return;
        }
        for i in 0..self.count {
            let Some(seg) = self.segment_rect_at(i, focus) else {
                continue;
            };
            let tier = selection_tier(
                theme,
                SelectionState::new(i == self.focused, hover == Some(i), false),
            );
            let r = ChromeRole::Selector.radius(theme, seg.width());
            let shape = RoundedRect::from_rect(seg, RoundedRectRadii::new(r, 0.0, 0.0, r));
            let fill = ribbon_fill(theme, focus_weight(i, focus), hover == Some(i));
            scene.fill(
                Fill::NonZero,
                transform,
                fill.with_alpha_factor(alpha),
                None,
                &shape,
            );
            if let Some(edge) = tier.border {
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.0),
                    transform,
                    edge.with_alpha_factor(alpha),
                    None,
                    &shape,
                );
            }
        }
    }
}

/// A map of the windows along one strip, and where on it the user is,
/// the counterpart of [`StripRibbon`] for the axis a strip can
/// overflow. Three facts in order of importance: how many windows the
/// strip has (one segment each — the fact that makes travel worth
/// attempting), which has focus (the long, bright segment), and which are
/// on screen right now (the middle weight, sliding as the strip pans).
///
/// A control, not a readout: clicking a segment focuses that window.
/// Bottom edge, because the disc owns the left and the strip ribbon owns
/// the right.
#[derive(Clone, Debug)]
pub struct StripRail {
    pub content: Rect,
    pub count: usize,
    pub focused: usize,
    /// Empty before the first layout pass has anything to report.
    pub visible: Vec<usize>,
}

/// Matches [`RIBBON_SEG_W`] on its axis, so the two controls read as
/// the same object turned ninety degrees.
pub const RAIL_SEG_H: f64 = 6.0;
pub const RAIL_SEG_W: f64 = 22.0;
pub const RAIL_SEG_W_FOCUSED: f64 = 40.0;
pub const RAIL_GAP: f64 = RAIL_HIT_H - RAIL_SEG_W;
/// How far the rail keeps inside the content's bottom edge, within the strip's bottom gutter: flush
/// with the screen's edge, six pixels of mark on the last rows read as a pager the screen had cropped,
/// and further up it floated over the windows' own bottom edge.
pub const RAIL_MARGIN: f64 = crate::tokens::Spacing::BASE.s1 as f64;
/// Minimum pointer target (HIG §9.6) across the rail's short axis.
pub const RAIL_HIT_H: f64 = 32.0;

const RAIL_ABSENT_ALPHA: u8 = 64;
const RAIL_PRESENT_ALPHA: u8 = 110;
const RAIL_HOVER_ALPHA: u8 = 160;
const RAIL_FOCUSED_ALPHA: u8 = 224;

fn rail_fill(theme: &Theme, focused: f64, hover: bool, present: bool) -> Color {
    let rest = if hover {
        RAIL_HOVER_ALPHA
    } else if present {
        RAIL_PRESENT_ALPHA
    } else {
        RAIL_ABSENT_ALPHA
    };
    Colors::hairline_on(
        theme.tokens.colors.surfaces.base,
        weighted_alpha(rest, RAIL_FOCUSED_ALPHA, focused),
    )
}

impl StripRail {
    /// Drawn for any strip with a window: the rail says where the windows on screen stand and takes a
    /// press to focus one, which a lone window still wants.
    pub fn is_meaningful(&self) -> bool {
        self.count >= 1
    }

    pub fn extent(&self) -> f64 {
        segments_extent(
            self.count,
            self.focused,
            RAIL_SEG_W,
            RAIL_SEG_W_FOCUSED,
            RAIL_GAP,
        )
    }

    pub fn segment_rect(&self, i: usize) -> Option<Rect> {
        self.segment_rect_at(i, crate::num::f64_of_usize(self.focused))
    }

    /// [`Self::segment_rect`] while the focus slides to another window.
    pub fn segment_rect_at(&self, i: usize, focus: f64) -> Option<Rect> {
        if i >= self.count {
            return None;
        }
        let y1 = self.content.y1 - RAIL_MARGIN;
        let y0 = y1 - RAIL_SEG_H;
        let (x, w) = sliding_segment_span(
            i,
            (self.count, focus),
            self.content.center().x,
            (RAIL_SEG_W, RAIL_SEG_W_FOCUSED, RAIL_GAP),
        );
        Some(Rect::new(x, y0, x + w, y1))
    }

    /// Widened past the visible segment to the minimum pointer target across the rail's short axis,
    /// upward from the screen's bottom edge.
    pub fn hit_rect(&self, i: usize) -> Option<Rect> {
        let seg = self.segment_rect(i)?;
        let y1 = self.content.y1;
        Some(Rect::new(
            seg.x0 - RAIL_GAP / 2.0,
            y1 - RAIL_HIT_H,
            seg.x1 + RAIL_GAP / 2.0,
            y1,
        ))
    }

    pub fn hit(&self, p: Point) -> Option<usize> {
        if !self.is_meaningful() {
            return None;
        }
        (0..self.count).find(|i| self.hit_rect(*i).is_some_and(|r| r.contains(p)))
    }

    pub fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        hover: Option<usize>,
        alpha: f32,
    ) {
        self.draw_at(
            scene,
            transform,
            theme,
            (hover, alpha),
            crate::num::f64_of_usize(self.focused),
        );
    }

    /// [`Self::draw`] with the focus at `focus`, sliding between windows.
    pub fn draw_at(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        (hover, alpha): (Option<usize>, f32),
        focus: f64,
    ) {
        if !self.is_meaningful() || alpha <= 0.0 {
            return;
        }
        for i in 0..self.count {
            let Some(seg) = self.segment_rect_at(i, focus) else {
                continue;
            };
            let present = self.visible.contains(&i);
            let r = ChromeRole::Selector.radius(theme, seg.height());
            let shape = RoundedRect::from_rect(seg, RoundedRectRadii::new(r, r, r, r));
            let fill = rail_fill(theme, focus_weight(i, focus), hover == Some(i), present);
            scene.fill(
                Fill::NonZero,
                transform,
                fill.with_alpha_factor(alpha),
                None,
                &shape,
            );
            let tier = selection_tier(
                theme,
                SelectionState::new(i == self.focused, hover == Some(i), false),
            );
            if let Some(edge) = tier.border {
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.0),
                    transform,
                    edge.with_alpha_factor(alpha),
                    None,
                    &shape,
                );
            }
        }
    }
}

#[cfg(test)]
mod rail_tests {
    use super::*;

    fn rail(count: usize, focused: usize, visible: Vec<usize>) -> StripRail {
        StripRail {
            content: Rect::new(0.0, 40.0, 1600.0, 900.0),
            count,
            focused,
            visible,
        }
    }

    tilo_test_env::scenario! {
        fn the_strip_rail_orders_its_segments_and_slides_focus;

        phase a_sliding_focus_moves_the_long_segment_and_keeps_the_run_in_place;
        {
            let r = rail(4, 1, vec![1]);
            let run = |focus: f64| -> Vec<Rect> {
                (0..4)
                    .map(|i| r.segment_rect_at(i, focus).unwrap())
                    .collect()
            };
            let resting: Vec<Rect> = (0..4).map(|i| r.segment_rect(i).unwrap()).collect();
            assert_eq!(run(1.0), resting, "at rest it is the resting run");
            let half = run(1.5);
            assert!((half[1].width() - half[2].width()).abs() < 1e-9);
            assert!(half[1].width() > RAIL_SEG_W && half[1].width() < RAIL_SEG_W_FOCUSED);
            for focus in [1.0, 1.25, 1.5, 2.0] {
                let rects = run(focus);
                let extent = rects[3].x1 - rects[0].x0;
                assert!((extent - r.extent()).abs() < 1e-9, "extent kept at {focus}");
                assert!(
                    ((rects[0].x0 + rects[3].x1) / 2.0 - 800.0).abs() < 1e-9,
                    "centre kept at {focus}"
                );
            }
            let ribbon = StripRibbon {
                content: Rect::new(0.0, 40.0, 1600.0, 900.0),
                count: 3,
                focused: 0,
                new_len: 0.0,
            };
            let half = (
                ribbon.segment_rect_at(0, 0.5).unwrap(),
                ribbon.segment_rect_at(1, 0.5).unwrap(),
            );
            assert!((half.0.height() - half.1.height()).abs() < 1e-9);
        }
    }
}

#[cfg(test)]
mod ribbon_tests {
    use super::*;
    use vello::kurbo::Point;

    /// A New strip button's length, as its words might measure.
    const NEW_LEN: f64 = 116.0;

    fn ribbon(count: usize, focused: usize) -> StripRibbon {
        StripRibbon {
            content: Rect::new(0.0, 40.0, 1600.0, 900.0),
            count,
            focused,
            new_len: NEW_LEN,
        }
    }

    tilo_test_env::scenario! {
        fn the_strip_ribbon_orders_its_segments_and_targets;

        phase ribbon_pointer_targets_are_disjoint_and_minimum_sized;
        {
            {
                let r = ribbon(3, 0);
                for i in 0..3 {
                    let hit = r.hit_rect(i).unwrap();
                    assert!(
                        hit.width() >= 32.0 && hit.height() >= 32.0,
                        "segment {i} target is {}x{}, under the 32 px minimum",
                        hit.width(),
                        hit.height()
                    );
                    assert_eq!(
                        r.hit(hit.center()),
                        Some(i),
                        "segment {i} is not hittable at its centre"
                    );
                }
            }

            {
                let r = ribbon(4, 2);
                let hits: Vec<Rect> = (0..4).map(|i| r.hit_rect(i).unwrap()).collect();
                for (i, w) in hits.windows(2).enumerate() {
                    assert!(
                        w[1].y0 >= w[0].y1 - 0.001,
                        "targets {i} and {} overlap: {:?} then {:?}",
                        i + 1,
                        w[0],
                        w[1]
                    );
                }
            }

            {
                let r = ribbon(3, 1);
                assert_eq!(r.hit(Point::new(400.0, 470.0)), None);
            }

            {
                let r = ribbon(3, 1);
                for i in 0..3 {
                    let seg = r.segment_rect(i).unwrap();
                    let hit = r.hit_rect(i).unwrap();
                    assert!(
                        (seg.x1 - r.content.x1).abs() < f64::EPSILON,
                        "segment {i} {seg:?}"
                    );
                    assert!(
                        (hit.x1 - r.content.x1).abs() < f64::EPSILON,
                        "target {i} {hit:?}"
                    );
                }
            }
        }

        phase the_new_strip_button_stands_under_the_last_tab_on_the_edge;
        {
            use crate::tokens::{Metrics, Spacing};
            for count in [1usize, 3] {
                let r = ribbon(count, 0);
                let last = r.hit_rect(count - 1).unwrap();
                let face = r.new_strip_rect().expect("a tab to stand under");
                let hit = r.new_strip_hit_rect().unwrap();
                assert!(
                    (face.width() - RIBBON_SEG_W).abs() < 1e-6
                        && (face.height() - NEW_LEN).abs() < 1e-6,
                    "as wide as the tabs, long enough for its words"
                );
                assert!(
                    (face.x1 - r.content.x1).abs() < 1e-6,
                    "flush with the edge, as the tabs are"
                );
                assert!(
                    (face.y0 - (last.y1 + f64::from(Spacing::BASE.s2))).abs() < 1e-6,
                    "a step under the last tab's target"
                );
                assert!(
                    hit.width() >= f64::from(Metrics::BASE.min_pointer_target)
                        && hit.height() >= f64::from(Metrics::BASE.min_pointer_target),
                    "a target no smaller than the minimum"
                );
                assert!(hit.contains(face.center()), "the target takes the whole tab");
                assert!(
                    hit.y0 >= last.y1,
                    "the button's target never reaches into the last tab"
                );
            }
            let none = ribbon(0, 0);
            let face = none
                .new_strip_rect()
                .expect("a workspace with no strip yet offers one too");
            assert!(
                (face.center().y - none.content.center().y).abs() < 1e-6,
                "in the middle of the edge"
            );
        }
    }
}
