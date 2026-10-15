//! The shared vocabulary the OS's own chrome is drawn from.
//!
//! Shell chrome does not invent appearance (`crates/sdk/ui-core/HIG.md` §9): it selects a
//! **role**, which resolves to tokens in [`crate::tokens`]. [`Colors::lift`]
//! and [`Colors::hairline_on`] measure their move against the ground by
//! relative luminance, so they behave on dark, light and high-contrast
//! without a per-theme branch.

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

/// Smallest relative-luminance gap for a panel to read as its own surface
/// rather than a slightly different patch of the desktop — the toolkit's
/// hairline threshold, spelled once.
const PANEL_MIN_DELTA: f64 = HAIRLINE_MIN_DELTA;

/// Alphas [`panel_ground`] tries, ascending, when the declared overlay
/// tier is swallowed by a ground the surface sits on — a ladder rather
/// than one constant because relative luminance compresses heavily at the
/// dark end, so the same delta costs far more RGB there than on light.
const PANEL_GROUND_STEPS: &[u8] = &[16, 24, 32, 40, 56, 72, 96];

/// Alpha of the inset stroke that gives an active control its inner glow.
pub const GLOW_ALPHA: u8 = 51;

/// Fraction of the full active lift a hovered control reaches.
pub const HOVER_LIFT_T: f32 = 0.5;

/// Uniform scale a pressed control takes, about its own centre.
pub const PRESS_SCALE: f64 = 0.97;

/// What a chrome surface *is*. Picking the role fixes radius, surface and
/// border together, so two surfaces of the same role are consistent
/// without their authors having to coordinate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChromeRole {
    /// Docked chrome fixed to a screen edge — the top bar.
    Bar,
    /// A floating surface opened over content — quick settings, the
    /// palette, a popover, a sheet.
    Panel,
    /// Something inside a Bar or Panel that you press — an action tile, a
    /// list row, a slider, a field.
    Control,
    /// One of a set you choose between — a tab pill, the tray pill, a
    /// workspace chip, a strip segment, a scope chip.
    Selector,
    /// A thing that holds app content — a window card or tile.
    Object,
}

impl ChromeRole {
    /// `height` is used only by [`ChromeRole::Selector`], a capsule that
    /// resolves to half its own height rather than the degenerate
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
    /// ([`crate::floating_surface::paint_material`]); this is the colour
    /// that material tints with, and what it is where blur is unavailable.
    pub fn surface(self, theme: &Theme) -> Color {
        let colors = &theme.tokens.colors;
        match self {
            ChromeRole::Bar | ChromeRole::Panel => panel_ground(theme),
            ChromeRole::Object => colors.surfaces.content,
            // Body is state: see `selection_tier`.
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
    /// 1 px outer border. `None` unless lifted.
    pub border: Option<Color>,
    /// 1 px inset stroke giving a lifted control a subtle inner glow.
    /// `None` unless lifted.
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
    let fill = if lifted {
        colors.lift(1.0)
    } else if state.hover {
        colors.lift(HOVER_LIFT_T)
    } else {
        Color::TRANSPARENT
    };
    let ground = colors.surfaces.base;
    SelectionTier {
        fill,
        border: lifted.then(|| Colors::hairline_on(ground, HAIRLINE_ALPHA)),
        inner_glow: lifted.then(|| Colors::hairline_on(ground, GLOW_ALPHA)),
        scale: if state.pressed { PRESS_SCALE } else { 1.0 },
    }
}

/// Smallest OKLCH lightness step between a selected row and the ground it
/// stands on for the selection to read. Lightness, not relative
/// luminance: luminance compresses at the dark end, where a perfectly
/// visible step measures under a light-theme threshold.
pub const SELECTION_MIN_STEP: f32 = 0.06;

/// Where the rest face of a control with its own floor sits on the ramp
/// [`selection_floor_ramp`] walks from the ground.
pub const FLOOR_IDLE_T: f32 = 0.35;
/// Where its hovered face sits.
pub const FLOOR_HOVER_T: f32 = 0.65;

/// [`selection_tier`] for a control that sits on `ground` rather than on
/// `surfaces.base`: a list row on a sidebar, a menu row on
/// [`panel_ground`], a card on a sheet. The active fill is one surface
/// step of that ground ([`Colors::lift_on`]) and hover is half a step, so
/// a selection is the same lift on every ground, including a raised or
/// white one where a lift of the desktop would vanish into it.
pub fn selection_tier_on(theme: &Theme, state: SelectionState, ground: Color) -> SelectionTier {
    let colors = &theme.tokens.colors;
    let lifted = state.active || state.pressed;
    let fill = if lifted {
        colors.lift_on(ground, 1.0)
    } else if state.hover {
        colors.lift_on(ground, HOVER_LIFT_T)
    } else {
        Color::TRANSPARENT
    };
    SelectionTier {
        fill,
        border: lifted.then(|| Colors::hairline_on(ground, HAIRLINE_ALPHA)),
        inner_glow: lifted.then(|| Colors::hairline_on(ground, GLOW_ALPHA)),
        scale: if state.pressed { PRESS_SCALE } else { 1.0 },
    }
}

/// How far a row's hover and selection lift, and a zebra stripe, bleed past
/// the column's content edge on each side (`s2`).
///
/// One edge rule holds for every content list: a row's text, a section's
/// title, a property list's labels and a table's first column all start on
/// the column's content edge, and the lift stands `row_bleed` outside it, so
/// text never moves when its row is selected and a list lines up with the
/// headings around it. A face takes the row's outer rect (the content edge
/// grown by the bleed); a retained row grows its own layout box by it.
pub fn row_bleed(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s2)
}

/// The content edge of a window's overview or content column, from the
/// column's leading edge: where the shell starts the column's header title,
/// and so where the column's rows, headings and fields start their text.
/// A list painted edge to edge in a column puts its rows' outer rects at
/// `column_gutter - row_bleed`.
pub fn column_gutter(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.rhythm.group)
}

/// Paint the selection of one row of a list: the hover or active lift of
/// `ground` ([`selection_tier_on`]) on `rect` with the item radius
/// (`radii.item`), and nothing for an idle row. A row is a lift with
/// primary ink and nothing else: no hue, no border, no leading bar.
///
/// `rect` is the row's outer rect: the column's content edge grown by
/// [`row_bleed`]. `NavList`, `TreeView`, [`super::paint_nav_row`], `ListRow`,
/// `VirtualList` and `DataTable` all paint through here, and a painted app's
/// own rows do too.
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

/// Which neighbours of a selected row are selected too. A run of selected
/// rows reads as one lift: the corners where two lifted rows meet are
/// square, so the run has no notch between its rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowJoin {
    /// The row above is lifted too.
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

/// [`paint_row_selection`] for a row inside a run of selected rows: the
/// corners shared with a lifted neighbour are square, so adjacent selected
/// rows join into one lift (a multi-selection in a mail list).
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

/// The face of a control that has its own floor (a filled rest face rather
/// than a transparent one), for `state`, standing on `ground`: one ordered
/// ramp from the ground, so rest, hover and active are each further from
/// the ground than the last on every material, and no state mixes a floor
/// with the transparent tier.
pub fn selection_floor_ramp(theme: &Theme, state: SelectionState, ground: Color) -> Color {
    let t = if state.active || state.pressed {
        1.0
    } else if state.hover {
        FLOOR_HOVER_T
    } else {
        FLOOR_IDLE_T
    };
    theme.tokens.colors.lift_on(ground, t)
}

/// OKLCH lightness distance between two colours.
pub fn lightness_step(a: Color, b: Color) -> f32 {
    use crate::color::Oklch;
    (Oklch::from_color(a).l - Oklch::from_color(b).l).abs()
}

// ---------- glass ----------
//
// What a selector is *made of* (the material), as opposed to
// `selection_tier` above, which is the semantics (how far each state is
// pushed). A surface reads as glass when: it's translucent and tinted from
// its ground rather than washed with white, it has a light direction (one
// end lit, one seated), and its edge is slightly stronger than its fill.
// All four are derived per theme, so the material survives a paper-white
// desktop, where "brighter" points the other way.

/// Film alpha of a hovered selector.
pub const GLASS_HOVER_ALPHA: u8 = 44;

/// Film alpha of an active or pressed selector. Well short of opaque — the
/// ground has to keep coming through, or the material stops being glass.
pub const GLASS_ACTIVE_ALPHA: u8 = 132;

/// Film density at the lit end of a selector, relative to its own alpha.
pub const GLASS_LIT_GAIN: f32 = 1.9;

/// Film density at the seated end, relative to its own alpha.
pub const GLASS_SEAT_FALLOFF: f32 = 0.68;

/// How far down a selector the lit end's extra density decays to the
/// body's own value. Spread over half the pill this would read as "a
/// lighter grey"; concentrated near the edge it reads as light landing on
/// a surface.
pub const GLASS_LIT_SPAN: f32 = 0.2;

/// Edge alpha of a hovered selector. Below [`HAIRLINE_ALPHA`] — a hover is
/// pointed at, not chosen, and a full hairline reads as a second active
/// tab. Carries more of the state than it looks like: the light preset's
/// translucent body only lands ~3 levels off its ground (bar sits ~19
/// below white) versus ~12 on a graphite ground, but [`Colors::hairline_on`]
/// picks whichever polarity moves the ground it's actually drawn on.
pub const GLASS_EDGE_HOVER_ALPHA: u8 = 28;

/// Edge alpha of an active selector at the lit end; runs down to
/// [`GLASS_EDGE_SEAT_SCALE`] of itself at the seated end, making the edge a
/// light rather than a halo.
pub const GLASS_EDGE_ACTIVE_ALPHA: u8 = 40;

/// How much of its lit-end strength a selector's edge keeps at the seated
/// end.
pub const GLASS_EDGE_SEAT_SCALE: f32 = 0.34;

/// Fraction of a selector's height over which its inner glow survives. A
/// lit *rim*, not a ring — stroked at constant alpha all the way round it
/// becomes the milky halo that made the active pill look cheap.
pub const GLASS_GLOW_SPAN: f64 = 0.55;

/// Same three states and ordering as [`selection_tier`] — not a second
/// opinion about what "active" means — but the bodies are films rather
/// than washes, carrying the light direction the tier table has no field
/// for. Kept separate rather than replacing it: a translucent fill handed
/// to a caller that fills it flat is *weaker* than an opaque wash, so the
/// swap must happen per surface, together with its painter.
pub fn glass_selector(theme: &Theme, state: SelectionState) -> SelectionTier {
    let colors = &theme.tokens.colors;
    let ground = colors.surfaces.base;
    let lifted = state.active || state.pressed;
    let film = colors.glass_film();
    let alpha = if lifted {
        GLASS_ACTIVE_ALPHA
    } else if state.hover {
        GLASS_HOVER_ALPHA
    } else {
        0
    };
    let edge_alpha = if lifted {
        GLASS_EDGE_ACTIVE_ALPHA
    } else {
        GLASS_EDGE_HOVER_ALPHA
    };
    SelectionTier {
        fill: if alpha == 0 {
            Color::TRANSPARENT
        } else {
            Color::from_rgba8(film.r(), film.g(), film.b(), alpha)
        },
        border: (alpha > 0).then(|| Colors::hairline_on(ground, edge_alpha)),
        inner_glow: lifted.then(|| Colors::hairline_on(ground, GLOW_ALPHA)),
        scale: if state.pressed { PRESS_SCALE } else { 1.0 },
    }
}

/// `(lit, seated)` offsets in 0..1 down the surface. Light comes from
/// above: on a palette that lifts by brightening, the lit end is the top;
/// on paper-white, a film can only darken, so the same story (top catches
/// the light) is told by thinning the film at the top instead, and the
/// ends swap. Getting this wrong doesn't just look flat — it looks lit
/// from below.
fn light_ends(theme: &Theme) -> (f64, f64) {
    if theme.tokens.colors.lifts_bright() {
        (0.0, 1.0)
    } else {
        (1.0, 0.0)
    }
}

/// `base` with its alpha scaled by `gain`, clamped to opaque. The clamp
/// matters: in a premultiplying renderer, an alpha pushed past 1.0 doesn't
/// no-op, it silently brightens the colour components too.
fn scaled(base: Color, gain: f32) -> Color {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "explicitly clamped to 0.0..=255.0 immediately above the cast"
    )]
    let a = (f32::from(base.a()) * gain).clamp(0.0, 255.0).round() as u8;
    Color::from_rgba8(base.r(), base.g(), base.b(), a)
}

/// A vertical gradient over `rect` running from its dense end to its thin
/// one, with `(offset, gain)` stops measured along that run.
fn vertical(
    rect: Rect,
    ends: (f64, f64),
    color: Color,
    stops: [(f32, f32); 3],
) -> vello::peniko::Gradient {
    let y = |t: f64| Point::new(rect.x0, rect.y0 + rect.height() * t);
    let (dense, thin) = ends;
    vello::peniko::Gradient::new_linear(y(dense), y(thin))
        .with_extend(vello::peniko::Extend::Pad)
        .with_stops(
            &stops.map(|(offset, gain)| vello::peniko::ColorStop {
                offset,
                color: scaled(color, gain).into(),
            })[..],
        )
}

/// Paint one selector's body in the glass material: film, edge, and — for
/// a lifted one — a lit inner rim.
///
/// `opacity` scales the whole material, for a caller animating a body in
/// or out. It multiplies rather than replaces so a half-revealed pill is
/// half of *this* material, not a different one.
///
/// Deliberately lays down **no shadow** — elevation and selection are
/// different vocabularies (see [`selection_tier`]). What replaces it is the
/// edge: a surface that reports its own light does not need a shadow to
/// say it is a surface.
pub fn paint_glass_body(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    radius: f64,
    theme: &Theme,
    tier: &SelectionTier,
    opacity: f32,
) {
    if opacity <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let shape = RoundedRect::from_rect(rect, radius);
    let ends = light_ends(theme);

    if tier.fill.a() > 0 {
        let film = tier.fill.with_alpha_factor(opacity);
        scene.fill(
            Fill::NonZero,
            transform,
            &vertical(
                rect,
                ends,
                film,
                [
                    (0.0, GLASS_LIT_GAIN),
                    (GLASS_LIT_SPAN, 1.0),
                    (1.0, GLASS_SEAT_FALLOFF),
                ],
            ),
            None,
            &shape,
        );
    }

    if let Some(edge) = tier.border {
        let edge = edge.with_alpha_factor(opacity);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            &vertical(
                rect,
                ends,
                edge,
                [
                    (0.0, 1.0),
                    (0.5, f32::midpoint(1.0, GLASS_EDGE_SEAT_SCALE)),
                    (1.0, GLASS_EDGE_SEAT_SCALE),
                ],
            ),
            None,
            &shape,
        );
    }

    // Lit inner rim. One pixel in from the edge, and gone by
    // `GLASS_GLOW_SPAN` down the surface, so it reads as the light
    // catching the inside of the top curve rather than as a ring.
    if let Some(glow) = tier.inner_glow {
        let inset = Rect::new(rect.x0 + 1.0, rect.y0 + 1.0, rect.x1 - 1.0, rect.y1 - 1.0);
        if inset.width() <= 0.0 || inset.height() <= 0.0 {
            return; // a pill this narrow has no inside to light
        }
        let glow = scaled(glow, opacity);
        let (lit, seated) = ends;
        let y = |t: f64| Point::new(inset.x0, inset.y0 + inset.height() * t);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "GLASS_GLOW_SPAN is a fixed compile-time constant; the f64->f32 \
                      narrowing is deterministic, not data-dependent"
        )]
        let glow_span_offset = GLASS_GLOW_SPAN as f32;
        let grad = vello::peniko::Gradient::new_linear(y(lit), y(seated))
            .with_extend(vello::peniko::Extend::Pad)
            .with_stops(
                &[
                    vello::peniko::ColorStop {
                        offset: 0.0,
                        color: glow.into(),
                    },
                    vello::peniko::ColorStop {
                        offset: glow_span_offset,
                        color: glow.with_alpha_factor(0.0).into(),
                    },
                ][..],
            );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            &grad,
            None,
            &RoundedRect::from_rect(inset, (radius - 1.0).max(0.0)),
        );
    }
}

/// The opaque colour a chrome surface is built from, guaranteed to
/// register against both the desktop and the window card (Bar/Panel share
/// one material, and a Panel floats over app content).
///
/// `surfaces.overlay` is used verbatim where it separates, but not always:
/// on high-contrast it's identical to the ground, and on light both it and
/// `window_card` are `#ffffff` — painted verbatim there, a panel is the
/// desktop with a faint line around it. Where it's swallowed, the ground
/// is walked away from it (same luminance reasoning as
/// [`Colors::hairline_on`]) until it clears both; [`PANEL_BORDER_ALPHA`]
/// is the edge half of the same fix.
pub fn panel_ground(theme: &Theme) -> Color {
    use crate::a11y::relative_luminance;
    let colors = &theme.tokens.colors;
    let base = colors.surfaces.base;
    let declared = colors.surfaces.overlay;
    let grounds = [base, colors.surfaces.content];
    let clears = |c: Color| {
        grounds
            .iter()
            .all(|g| (relative_luminance(c) - relative_luminance(*g)).abs() >= PANEL_MIN_DELTA)
    };
    if clears(declared) {
        return declared;
    }
    // Composite each step over the declared tier so the result stays in
    // the theme's own colour family instead of jumping to a grey.
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

/// The edge around a floating [`ChromeRole::Panel`]. See
/// [`PANEL_BORDER_ALPHA`].
pub fn panel_border(theme: &Theme) -> Color {
    Colors::hairline_on(theme.tokens.colors.surfaces.base, PANEL_BORDER_ALPHA)
}

/// Lay down a floating panel's elevation, clipped to the ring *outside*
/// its own silhouette. The shadow is [`crate::surface::shadow`], a blur of
/// the panel's own rounded rect (so it stays even around a corner instead
/// of showing a flat-edged ghost of a square one). The ring clip stays
/// even though chrome is opaque: a blurred rect is solid in the middle,
/// and a half-faded panel (`intensity`) must not darken its own interior
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
    // Even-odd across [outer, card]: a point inside both winds twice and
    // is excluded, so only the ring around the card paints.
    let mut ring = BezPath::new();
    ring.extend(outer.path_elements(0.1));
    ring.extend(RoundedRect::from_rect(rect, radius).path_elements(0.1));
    scene.push_layer(Fill::EvenOdd, BlendMode::default(), 1.0, transform, &ring);
    let mut token = *e;
    token.color = token.color.with_alpha_factor(intensity.clamp(0.0, 1.0));
    crate::surface::shadow(scene, transform, rect, radius, &token);
    scene.pop_layer();
}

/// Paint a floating [`ChromeRole::Panel`] body: its shadow at
/// `shadow_intensity`, the [`panel_ground`] fill and the 1 px
/// [`panel_border`], all at the Panel radius. Returns the body's outline
/// for callers that clip their content to it.
pub fn paint_panel(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    theme: &Theme,
    shadow_intensity: f32,
) -> RoundedRect {
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
    body
}

/// A separator or edge drawn on a chrome surface. Derived against the
/// ground it sits on rather than a declared `border_standard`, which
/// doesn't track what's under a translucent panel and either vanishes on
/// one theme or glares on another.
pub fn chrome_hairline(theme: &Theme) -> Color {
    Colors::hairline_on(theme.tokens.colors.surfaces.overlay, HAIRLINE_ALPHA)
}

/// Share of a scrim's strength that the flat *level* stage takes; the *tone*
/// stage takes the whole of it. See [`fill_scrim`].
pub const SCRIM_LEVEL_SHARE: f32 = 0.55;

/// Paint a scrim over `rect` at `strength`, in the theme's own scrim hue.
///
/// A flat wash dims by an amount that depends on the ground's own
/// lightness: measured on a real frame, a flat 0.45 wash took 39.6 L* off
/// a `bg-white` page (252→143) and 1.5 L* off the desktop beside it
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
        // Black is not a palette choice here: soft light is only the `Cb²`
        // curve the model below mirrors when its source is black.
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
    // The scrim hue is a near-neutral by construction, so its own relative
    // luminance is the proxy for "the grey it adds".
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

    fn glass_fill_count(theme: &Theme, state: SelectionState) -> usize {
        let rect = Rect::new(0.0, 0.0, 120.0, 28.0);
        let mut scene = Scene::new();
        paint_glass_body(
            &mut scene,
            Affine::IDENTITY,
            rect,
            rect.height() / 2.0,
            theme,
            &glass_selector(theme, state),
            1.0,
        );
        // Every paint, not only the flat ones: the material's body and
        // edge are gradient brushes, and a shadow ring would be a plain
        // fill, so counting one kind would miss whichever kind came back.
        scene.encoding().draw_tags.len()
    }

    #[test]
    fn glass_material_paints_body_edge_and_rim_as_one_decision() {
        // Case `the_material_paints_a_body_an_edge_and_a_rim_and_nothing_else`.
        {
            for (name, theme) in themes() {
                assert_eq!(
                    glass_fill_count(&theme, SelectionState::default()),
                    0,
                    "{name}: an idle selector has no body, so it must paint nothing \
                 at all — not an edge, and certainly not a shadow of nothing",
                );
                assert_eq!(
                    glass_fill_count(&theme, SelectionState::new(false, true, false)),
                    2,
                    "{name}: a hovered selector is exactly a film and its edge",
                );
                assert_eq!(
                    glass_fill_count(&theme, SelectionState::active(true)),
                    3,
                    "{name}: a lifted selector is exactly a film, its edge and its \
                 lit rim — anything more is elevation smuggled into selection",
                );
            }
        }

        // Case `a_glass_body_is_never_opaque_and_always_has_somewhere_to_lighten`.
        {
            for (name, theme) in themes() {
                for (state_name, state) in [
                    ("hover", SelectionState::new(false, true, false)),
                    ("active", SelectionState::active(true)),
                    ("pressed", SelectionState::new(false, false, true)),
                ] {
                    let tier = glass_selector(&theme, state);
                    assert!(
                        tier.fill.a() > 0,
                        "{name}/{state_name}: a state with a body must paint one",
                    );
                    assert!(
                        tier.fill.a() < u8::MAX,
                        "{name}/{state_name}: an opaque body is not glass — the \
                     desktop has to come through it, and an opaque fill also \
                     makes the lit-end alpha ramp a silent no-op",
                    );
                    let lit = tier.fill.with_alpha_factor(GLASS_LIT_GAIN);
                    assert!(
                        lit.a() > tier.fill.a(),
                        "{name}/{state_name}: the lit end of the film ({}) is no \
                     denser than its middle ({}), so the body paints flat and \
                     has no light direction at all",
                        lit.a(),
                        tier.fill.a(),
                    );
                    let seated = tier.fill.with_alpha_factor(GLASS_SEAT_FALLOFF);
                    assert!(
                        seated.a() < tier.fill.a(),
                        "{name}/{state_name}: the seated end is no thinner than \
                     the middle — a surface lit from above is darker where \
                     the light does not reach",
                    );
                }
            }
        }

        // Case `a_glass_body_and_its_edge_are_the_same_decision`.
        {
            for (name, theme) in themes() {
                for (state_name, state) in [
                    ("idle", SelectionState::default()),
                    ("hover", SelectionState::new(false, true, false)),
                    ("active", SelectionState::active(true)),
                    ("pressed", SelectionState::new(false, false, true)),
                ] {
                    let tier = glass_selector(&theme, state);
                    assert_eq!(
                        tier.fill.a() > 0,
                        tier.border.is_some(),
                        "{name}/{state_name}: body and edge disagree — one paints \
                     and the other does not",
                    );
                }
            }
        }

        // Case `the_film_lifts_the_same_way_a_flat_lift_does_on_every_preset`.
        {
            for theme in crate::theme::ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
                let colors = &theme.tokens.colors;
                let ground = relative_luminance(colors.surfaces.base);
                let film = relative_luminance(colors.glass_film());
                let flat = relative_luminance(colors.lift(1.0));
                assert!(
                    (film - ground).abs() > f64::EPSILON,
                    "{}: the film does not move the ground at all",
                    theme.name,
                );
                assert_eq!(
                    film > ground,
                    flat > ground,
                    "{}: the glass film lifts {} while a flat lift goes {} — \
                 chrome painted in the two materials would move in opposite \
                 directions on the same desktop",
                    theme.name,
                    if film > ground { "brighter" } else { "darker" },
                    if flat > ground { "brighter" } else { "darker" },
                );
                assert_eq!(
                    colors.lifts_bright(),
                    flat > ground,
                    "{}: `lifts_bright` disagrees with where a lift actually \
                 goes, so the material would be lit from the wrong end",
                    theme.name,
                );
            }
        }

        // Case `the_lit_end_of_a_selector_is_its_top_on_every_preset`.
        {
            for theme in crate::theme::ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
                let (lit, seated) = light_ends(&theme);
                let brightens = theme.tokens.colors.lifts_bright();
                let dense_end_is_top = lit < seated;
                assert_eq!(
                    dense_end_is_top,
                    brightens,
                    "{}: the film is densest at the {} on a palette whose \
                 surfaces lift {} — that is a pill lit from below",
                    theme.name,
                    if dense_end_is_top { "top" } else { "bottom" },
                    if brightens { "brighter" } else { "darker" },
                );
            }
        }
    }

    #[test]
    fn selection_tiers_register_progressively_without_the_accent() {
        // Case `selection_tier_on_registers_on_every_ground_it_is_asked_about`:
        // per theme and per ground a list can stand on, the active lift is a
        // visible lightness step and hover sits between it and the ground. A
        // fixed white wash would pass on dark and vanish on a white well.
        {
            for theme in crate::theme::ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
                let name = theme.name.clone();
                let colors = &theme.tokens.colors;
                let grounds = [
                    ("base", colors.surfaces.base),
                    ("content", colors.surfaces.content),
                    ("sidebar", colors.surfaces.sidebar),
                    ("inspector", colors.surfaces.inspector),
                    ("raised", colors.surfaces.raised),
                    ("panel_ground", panel_ground(&theme)),
                    ("white", Color::WHITE),
                    ("black", Color::BLACK),
                ];
                for (ground_name, ground) in grounds {
                    let active =
                        selection_tier_on(&theme, SelectionState::active(true), ground).fill;
                    let hover =
                        selection_tier_on(&theme, SelectionState::new(false, true, false), ground)
                            .fill;
                    let idle =
                        selection_tier_on(&theme, SelectionState::new(false, false, false), ground)
                            .fill;
                    assert_eq!(
                        idle.components[3], 0.0,
                        "{name}/{ground_name}: idle is transparent"
                    );
                    let step = lightness_step(active, ground);
                    assert!(
                        step >= SELECTION_MIN_STEP,
                        "{name}/{ground_name}: active lift must read on its ground (ΔL {step:.3})"
                    );
                    // Half a step can round to the ground itself in 8-bit
                    // sRGB at the black end; it never passes the full step.
                    let half = lightness_step(hover, ground);
                    assert!(
                        half < step,
                        "{name}/{ground_name}: hover sits between the ground and the active tier"
                    );
                    // A floored control's ramp is ordered from the same ground.
                    let ramp =
                        |state| lightness_step(selection_floor_ramp(&theme, state, ground), ground);
                    let rest = ramp(SelectionState::default());
                    let hovered = ramp(SelectionState::new(false, true, false));
                    let chosen = ramp(SelectionState::active(true));
                    assert!(
                        rest < hovered && hovered < chosen,
                        "{name}/{ground_name}: floor ramp {rest} {hovered} {chosen}"
                    );
                }
            }
        }

        // Case `selection_tiers_move_progressively_away_from_the_ground`.
        {
            for (name, theme) in themes() {
                let ground = relative_luminance(theme.tokens.colors.surfaces.base);
                let dist = |s: SelectionState| {
                    let t = selection_tier(&theme, s);
                    // An idle tier is transparent, so composite over the
                    // ground first — its effective luminance is the ground's.
                    let f = t.fill;
                    let a = f.components[3];
                    let over = |c: f32, g: f32| c * a + g * (1.0 - a);
                    let g = theme.tokens.colors.surfaces.base.components;
                    let composited = Color::new([
                        over(f.components[0], g[0]),
                        over(f.components[1], g[1]),
                        over(f.components[2], g[2]),
                        1.0,
                    ]);
                    (relative_luminance(composited) - ground).abs()
                };
                let idle = dist(SelectionState::default());
                let hover = dist(SelectionState::new(false, true, false));
                let active = dist(SelectionState::active(true));
                assert!(
                    hover > idle,
                    "{name}: hover ({hover:.4}) must read as lifted off idle ({idle:.4})"
                );
                assert!(
                    active > hover,
                    "{name}: active ({active:.4}) must read as lifted off hover ({hover:.4})"
                );
            }
        }

        // Case `selection_never_uses_the_accent`.
        {
            for (name, theme) in themes() {
                let accent = theme.tokens.colors.intent.inverted;
                for state in [
                    SelectionState::active(true),
                    SelectionState::new(false, true, false),
                    SelectionState::new(true, true, true),
                ] {
                    let tier = selection_tier(&theme, state);
                    assert_ne!(
                        tier.fill.to_rgba8().to_u32(),
                        accent.to_rgba8().to_u32(),
                        "{name}: {state:?} filled with the accent"
                    );
                }
            }
        }

        // Case `hovering_an_active_control_changes_nothing`.
        {
            for (name, theme) in themes() {
                let a = selection_tier(&theme, SelectionState::active(true));
                let ah = selection_tier(&theme, SelectionState::new(true, true, false));
                assert_eq!(
                    a.fill.to_rgba8().to_u32(),
                    ah.fill.to_rgba8().to_u32(),
                    "{name}: an active control brightened on hover"
                );
                assert_eq!(
                    a.scale, ah.scale,
                    "{name}: an active control resized on hover"
                );
            }
        }
    }

    #[test]
    fn chrome_radii_and_surfaces_follow_the_theme() {
        // Case `a_selector_is_a_capsule_at_every_height`.
        {
            let theme = Theme::dark();
            for h in [12.0_f64, 28.0, 44.0, 120.0] {
                let r = ChromeRole::Selector.radius(&theme, h);
                assert!(
                    (r - h / 2.0).abs() < f64::EPSILON,
                    "height {h} resolved to radius {r}, not a capsule"
                );
                assert!(r <= h / 2.0, "radius {r} exceeds half of height {h}");
            }
        }

        // Case `no_role_resolves_to_an_off_scale_radius`:
        // 16 px is deliberately absent from the radius scale (`tokens::radii`);
        // no role may resolve to it.
        {
            let theme = Theme::dark();
            let radii = &theme.tokens.radii;
            let on_scale = [
                0.0,
                f64::from(radii.detail),
                f64::from(radii.control),
                f64::from(radii.tile),
                f64::from(radii.panel),
            ];
            for role in [
                ChromeRole::Bar,
                ChromeRole::Panel,
                ChromeRole::Control,
                ChromeRole::Object,
            ] {
                let r = role.radius(&theme, 44.0);
                assert!(
                    on_scale.iter().any(|s| (s - r).abs() < f64::EPSILON),
                    "{role:?} resolved to {r}, which is not on the radius scale"
                );
                assert!(
                    (r - 16.0).abs() > f64::EPSILON,
                    "{role:?} resolved to the absent 16 px step"
                );
            }
        }

        // Case `bar_and_panel_share_one_material`.
        {
            for (name, theme) in themes() {
                let bar = ChromeRole::Bar.surface(&theme);
                let panel = ChromeRole::Panel.surface(&theme);
                assert_eq!(
                    bar.to_rgba8().to_u32(),
                    panel.to_rgba8().to_u32(),
                    "{name}: the bar and a panel are painted in different materials"
                );
            }
        }

        // Case `a_panel_separates_from_the_desktop_on_every_theme`:
        // On high-contrast, `surfaces.overlay` is the same colour as the
        // ground, so fill *or* edge must carry the separation — this asserts
        // at least one does.
        {
            for (name, theme) in themes() {
                let base = theme.tokens.colors.surfaces.base;
                let ground = panel_ground(&theme);
                let border = panel_border(&theme);
                let fill_delta = (relative_luminance(ground) - relative_luminance(base)).abs();
                // The border is translucent; composite it over the panel
                // ground to judge what the eye actually receives.
                let a = border.components[3];
                let g = ground.components;
                let edge = Color::new([
                    border.components[0] * a + g[0] * (1.0 - a),
                    border.components[1] * a + g[1] * (1.0 - a),
                    border.components[2] * a + g[2] * (1.0 - a),
                    1.0,
                ]);
                let edge_delta = (relative_luminance(edge) - relative_luminance(ground)).abs();
                assert!(
                    fill_delta >= PANEL_MIN_DELTA || edge_delta >= PANEL_MIN_DELTA,
                    "{name}: a panel is indistinguishable from the desktop \
                 (fill delta {fill_delta:.4}, edge delta {edge_delta:.4})"
                );
            }
        }

        // Case `chrome_surfaces_differ_between_light_and_dark`.
        {
            let dark = ChromeRole::Panel.surface(&Theme::dark());
            let light = ChromeRole::Panel.surface(&Theme::light());
            assert_ne!(
                dark.to_rgba8().to_u32(),
                light.to_rgba8().to_u32(),
                "the panel surface is identical on light and dark"
            );
        }
    }

    /// Perceived lightness of an 8-bit grey, CIE L\*. A scrim's job is
    /// judged in what the eye loses, not the code value — a flat wash takes
    /// the same fraction of every code value, which is why it reads as two
    /// different amounts of dimming on a light page versus a dark desktop.
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

    /// The flat fill this replaced, so comparisons below are against what
    /// was actually on screen rather than a remembered number.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "explicitly clamped to 0.0..=1.0 immediately before the *255.0 scale, \
                  so the final value is always within 0..=255"
    )]
    fn flat_wash(v: u8, strength: f32, theme: &Theme) -> u8 {
        let a = f64::from(strength.clamp(0.0, 1.0));
        let hue = theme.tokens.colors.surfaces.scrim;
        let grey = (0.2126 * f64::from(hue.r())
            + 0.7152 * f64::from(hue.g())
            + 0.0722 * f64::from(hue.b()))
            / 255.0;
        let out = (f64::from(v) / 255.0) * (1.0 - a) + grey * a;
        (out.clamp(0.0, 1.0) * 255.0).round() as u8
    }

    #[test]
    fn a_dark_theme_scrim_dims_the_mid_tones_of_dark_content() {
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

    #[test]
    fn scrim_dims_every_ground_while_keeping_its_structure() {
        // Case `a_scrim_dims_every_bright_ground_by_the_same_amount`:
        // Asserted only from `#ffffff` down to mid grey — those grounds *can*
        // lose a fixed amount; below that a ground has less lightness than
        // the scrim wants, and that's the separate requirement below.
        {
            for (name, theme) in themes() {
                let drops: Vec<(u8, f64)> = [255u8, 252, 240, 220, 180, 128]
                    .iter()
                    .map(|&v| (v, lstar(v) - lstar(scrim_maps(v, 0.45, &theme))))
                    .collect();
                let lo = drops.iter().map(|d| d.1).fold(f64::MAX, f64::min);
                let hi = drops.iter().map(|d| d.1).fold(f64::MIN, f64::max);
                assert!(
                    hi - lo <= 4.0,
                    "on {name} the scrim takes between {lo:.1} and {hi:.1} L* off \
                 grounds that are all bright enough to give it up, so it is \
                 not one gesture — it is a different one per app: {drops:?}"
                );
                assert!(
                    lo >= 15.0,
                    "on {name} the scrim only takes {lo:.1} L* off the ground it \
                 moves least, which is not a dim, it is a tint: {drops:?}"
                );
            }
        }

        // Case `a_scrim_does_not_erase_the_brightest_ground`.
        {
            for (name, theme) in themes() {
                let page = scrim_maps(252, 0.45, &theme);
                let drop = lstar(252) - lstar(page);
                assert!(
                    drop <= 28.0,
                    "on {name} the scrim takes {drop:.1} L* off a bg-white page \
                 (252 -> {page}); past about 28 the page stops reading as a \
                 page and starts reading as a grey slab"
                );
            }
        }

        // Case `a_scrim_keeps_the_structure_of_what_is_behind_it`:
        // A scrim must lower what's behind it without flattening it — a flat
        // wash closed the 12 code values between a page and a rule on it down
        // to 7. Squaring the backdrop trades the opposite way at the dark end,
        // bounded here so that can't quietly become a collapse either.
        {
            for (name, theme) in themes() {
                for (hi, lo) in [(252u8, 240u8), (255, 245)] {
                    let span_before = i32::from(hi) - i32::from(lo);
                    let span_after = i32::from(scrim_maps(hi, 0.45, &theme))
                        - i32::from(scrim_maps(lo, 0.45, &theme));
                    let flat_after = i32::from(flat_wash(hi, 0.45, &theme))
                        - i32::from(flat_wash(lo, 0.45, &theme));
                    assert!(
                        span_after >= flat_after,
                        "on {name} the scrim compresses {lo}..{hi} harder than the \
                     flat wash it replaced ({span_before} -> {span_after}, flat \
                     managed {flat_after}) — the bright end is exactly where a \
                     wash goes flat and where this must not"
                    );
                }
                for (hi, lo) in [(64u8, 52u8), (40, 28)] {
                    let span_before = i32::from(hi) - i32::from(lo);
                    let span_after = i32::from(scrim_maps(hi, 0.45, &theme))
                        - i32::from(scrim_maps(lo, 0.45, &theme));
                    assert!(
                        span_after * 2 >= span_before,
                        "on {name} a dark span {lo}..{hi} came out at {span_after} \
                     from {span_before}; the tone pass is allowed to cost the \
                     dark end a code value, not half of it"
                    );
                }
            }
        }

        // Case `a_scrim_reaches_the_dark_grounds_a_flat_wash_could_not`:
        // Relative, not absolute: a ground at L\* 6 cannot lose 20 L\*, but it
        // must move further than the flat wash moved it.
        {
            for (name, theme) in themes() {
                for v in [96u8, 64, 40, 32, 23, 20, 10] {
                    let now = lstar(v) - lstar(scrim_maps(v, 0.45, &theme));
                    let was = lstar(v) - lstar(flat_wash(v, 0.45, &theme));
                    assert!(
                        now > was,
                        "on {name} a ground at {v} loses {now:.1} L* to the scrim \
                     where the flat wash it replaced took {was:.1} — the dark \
                     half of the screen is the half that never showed the dim"
                    );
                }
            }
        }

        // Case `a_scrim_fades_monotonically_with_its_openness`:
        // Both fills must fade with the surface's openness, or the dim arrives
        // before the panel does.
        {
            for (name, theme) in themes() {
                let mut last = 253u8;
                for step in 0..=8 {
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "step ranges 0..=8, far below where f32 loses integer precision"
                    )]
                    let openness = step as f32 / 8.0;
                    let page = scrim_maps(252, 0.45 * openness, &theme);
                    assert!(
                        page <= last,
                        "on {name} the scrim got *lighter* between openness steps \
                     ({last} -> {page} at {openness})"
                    );
                    last = page;
                }
                assert!(
                    scrim_maps(252, 0.0, &theme) == 252,
                    "on {name} a closed scrim still moved the page"
                );
            }
        }

        // Case `a_scrim_leaves_high_contrast_high_contrast`:
        // White on black must not drop below the 7:1 (AAA) that palette exists
        // to clear.
        {
            let theme = Theme::high_contrast();
            let ink = scrim_maps(255, 0.45, &theme);
            let ground = scrim_maps(0, 0.45, &theme);
            let lum = |v: u8| {
                let s = f64::from(v) / 255.0;
                if s <= 0.03928 {
                    s / 12.92
                } else {
                    ((s + 0.055) / 1.055).powf(2.4)
                }
            };
            let ratio = (lum(ink) + 0.05) / (lum(ground) + 0.05);
            assert!(
                ratio >= 7.0,
                "white ink behind the scrim is {ink} on {ground} = {ratio:.1}:1; \
             the high-contrast palette is chosen to clear AAA (7:1) and a \
             scrim must not take that away"
            );
        }

        // Case `a_scrim_paints_the_two_stages_the_model_describes`:
        // The paint path and [`scrim_maps`] must agree on the fill count, or
        // the model describes something nobody sees.
        {
            let rect = Rect::new(0.0, 0.0, 200.0, 120.0);
            for (name, theme) in themes() {
                let mut scene = Scene::new();
                fill_scrim(&mut scene, Affine::IDENTITY, rect, 0.45, &theme);
                let n = scene.encoding().draw_tags.len();
                assert!(
                    n >= 3,
                    "on {name} the scrim encoded {n} draws; it is a soft-light \
                 layer, the tone fill inside it, and the level fill after it"
                );
                let mut closed = Scene::new();
                fill_scrim(&mut closed, Affine::IDENTITY, rect, 0.0, &theme);
                assert_eq!(
                    closed.encoding().draw_tags.len(),
                    0,
                    "on {name} a scrim at zero strength still painted something"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Strip ribbon
// ---------------------------------------------------------------------------

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
}

pub const RIBBON_SEG_W: f64 = 6.0;
pub const RIBBON_SEG_H: f64 = 22.0;
/// Longer than [`RIBBON_SEG_H`], so the ribbon reads at a glance without
/// needing colour to carry the signal — what keeps it legible on
/// high-contrast.
pub const RIBBON_SEG_H_FOCUSED: f64 = 40.0;
/// Sized so segment height plus gap equals [`RIBBON_HIT_W`]: adjacent hit
/// rects tile without overlapping, so a click near a boundary belongs to
/// exactly one strip.
pub const RIBBON_GAP: f64 = RIBBON_HIT_W - RIBBON_SEG_H;
/// Flush with the screen's right edge: the segments are an edge mark, and
/// a gap between them and the edge read as a stray, misplaced control.
pub const RIBBON_MARGIN: f64 = 0.0;
/// Minimum pointer target (HIG §9.6) — the segment itself is 6 px wide.
pub const RIBBON_HIT_W: f64 = 32.0;

/// Weight of an unfocused ribbon segment against the desktop.
const RIBBON_IDLE_ALPHA: u8 = 46;
/// Weight of a hovered but unfocused segment.
const RIBBON_HOVER_ALPHA: u8 = 96;
/// Weight of the focused segment.
const RIBBON_FOCUSED_ALPHA: u8 = 224;

/// Can't reuse [`selection_tier`] unmodified — it lets "unselected" be
/// transparent because a row of controls supplies context, but a ribbon
/// segment has no row, so an unfocused one at zero alpha would be the only
/// evidence of a second strip, invisible. A floor alone isn't enough
/// either: on high-contrast, `surfaces.base`/`raised` are both pure black,
/// so a floored unfocused segment came out *brighter than the focused
/// one*. Hence three explicit, hand-ordered weights instead of a derived
/// tier.
fn ribbon_fill(theme: &Theme, focused: bool, hover: bool) -> Color {
    let alpha = if focused {
        RIBBON_FOCUSED_ALPHA
    } else if hover {
        RIBBON_HOVER_ALPHA
    } else {
        RIBBON_IDLE_ALPHA
    };
    Colors::hairline_on(theme.tokens.colors.surfaces.base, alpha)
}

/// Length of a run of `count` segments `gap` apart along a ribbon or rail,
/// each `segment` long except the one at `focused_index`, which is
/// `focused_segment`.
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
        if i >= self.count {
            return None;
        }
        let x1 = self.content.x1 - RIBBON_MARGIN;
        let x0 = x1 - RIBBON_SEG_W;
        let mut y = self.content.center().y - self.extent() / 2.0;
        for k in 0..i {
            y += if k == self.focused {
                RIBBON_SEG_H_FOCUSED
            } else {
                RIBBON_SEG_H
            };
            y += RIBBON_GAP;
        }
        let h = if i == self.focused {
            RIBBON_SEG_H_FOCUSED
        } else {
            RIBBON_SEG_H
        };
        Some(Rect::new(x0, y, x1, y + h))
    }

    /// Widened past the visible segment to the minimum pointer target,
    /// inward from the screen edge the segment sits on.
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

    pub fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        hover: Option<usize>,
        alpha: f32,
    ) {
        if self.count == 0 || alpha <= 0.0 {
            return;
        }
        for i in 0..self.count {
            let Some(seg) = self.segment_rect(i) else {
                continue;
            };
            let tier = selection_tier(
                theme,
                SelectionState::new(i == self.focused, hover == Some(i), false),
            );
            // Square where it meets the screen edge, rounded inward.
            let r = ChromeRole::Selector.radius(theme, seg.width());
            let shape = RoundedRect::from_rect(seg, RoundedRectRadii::new(r, 0.0, 0.0, r));
            let fill = ribbon_fill(theme, i == self.focused, hover == Some(i));
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

/// A map of the windows along one strip, and where on it the user is
/// standing — the counterpart of [`StripRibbon`] for the axis a strip can
/// overflow. Three facts in order of importance: how many windows the
/// strip holds (one segment each — the fact that makes travel worth
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

/// Matches [`RIBBON_SEG_W`] on its own axis, so the two controls read as
/// the same object turned ninety degrees.
pub const RAIL_SEG_H: f64 = 6.0;
pub const RAIL_SEG_W: f64 = 22.0;
pub const RAIL_SEG_W_FOCUSED: f64 = 40.0;
pub const RAIL_GAP: f64 = RAIL_HIT_H - RAIL_SEG_W;
/// Flush with the screen's bottom edge, for the same reason as
/// [`RIBBON_MARGIN`]; ten pixels up it floated over the windows' bottom margin.
pub const RAIL_MARGIN: f64 = 0.0;
/// Minimum pointer target (HIG §9.6) across the rail's short axis.
pub const RAIL_HIT_H: f64 = 32.0;

/// Heavier than the ribbon's idle step: an off-screen *window* segment is
/// the rail's entire message (the strip continues past the display edge),
/// where an unfocused *strip* segment only has to hint another exists.
const RAIL_ABSENT_ALPHA: u8 = 64;
const RAIL_PRESENT_ALPHA: u8 = 110;
const RAIL_HOVER_ALPHA: u8 = 160;
const RAIL_FOCUSED_ALPHA: u8 = 224;

/// Four hand-ordered weights, same reason as [`ribbon_fill`]: a derived
/// tier has nothing to lift toward on high-contrast and can invert.
fn rail_fill(theme: &Theme, focused: bool, hover: bool, present: bool) -> Color {
    let alpha = if focused {
        RAIL_FOCUSED_ALPHA
    } else if hover {
        RAIL_HOVER_ALPHA
    } else if present {
        RAIL_PRESENT_ALPHA
    } else {
        RAIL_ABSENT_ALPHA
    };
    Colors::hairline_on(theme.tokens.colors.surfaces.base, alpha)
}

impl StripRail {
    /// One window is not a strip you can travel along, and a single
    /// segment would be noise.
    pub fn is_meaningful(&self) -> bool {
        self.count >= 2
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
        if i >= self.count {
            return None;
        }
        let y1 = self.content.y1 - RAIL_MARGIN;
        let y0 = y1 - RAIL_SEG_H;
        let mut x = self.content.center().x - self.extent() / 2.0;
        for k in 0..i {
            x += if k == self.focused {
                RAIL_SEG_W_FOCUSED
            } else {
                RAIL_SEG_W
            };
            x += RAIL_GAP;
        }
        let w = if i == self.focused {
            RAIL_SEG_W_FOCUSED
        } else {
            RAIL_SEG_W
        };
        Some(Rect::new(x, y0, x + w, y1))
    }

    /// Widened past the visible segment to the minimum pointer target
    /// across the rail's short axis, upward from the screen's bottom edge.
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
        if !self.is_meaningful() || alpha <= 0.0 {
            return;
        }
        for i in 0..self.count {
            let Some(seg) = self.segment_rect(i) else {
                continue;
            };
            let present = self.visible.contains(&i);
            // Square where it meets the screen edge, rounded inward.
            let r = ChromeRole::Selector.radius(theme, seg.height());
            let shape = RoundedRect::from_rect(seg, RoundedRectRadii::new(r, r, 0.0, 0.0));
            let fill = rail_fill(theme, i == self.focused, hover == Some(i), present);
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

    #[test]
    fn rail_segments_are_ordered_weighted_and_hittable() {
        // Case `a_single_group_draws_no_rail`.
        {
            assert!(!rail(1, 0, vec![0]).is_meaningful());
            assert!(rail(2, 0, vec![0]).is_meaningful());
            assert!(rail(1, 0, vec![0]).hit(Point::new(800.0, 884.0)).is_none());
        }

        // Case `segments_run_in_order_with_the_focused_one_longest`.
        {
            let r = rail(4, 2, vec![1, 2]);
            let rects: Vec<Rect> = (0..4).map(|i| r.segment_rect(i).unwrap()).collect();
            for pair in rects.windows(2) {
                assert!(
                    pair[0].x1 <= pair[1].x0,
                    "segments must not overlap: {:?} then {:?}",
                    pair[0],
                    pair[1]
                );
            }
            assert!(
                rects[2].width() > rects[0].width(),
                "the focused segment must be longer than an unfocused one"
            );
            assert!(
                rects
                    .iter()
                    .all(|s| (s.y1 - r.content.y1).abs() < f64::EPSILON),
                "every segment sits flush on the screen's bottom edge"
            );
            assert!(
                (0..4).all(|i| r
                    .hit_rect(i)
                    .is_some_and(|h| (h.y1 - r.content.y1).abs() < f64::EPSILON)),
                "each target runs to the edge, so the pointer pinned there hits it"
            );
        }

        // Case `weights_stay_ordered_on_every_palette`:
        // All four weights resolve to the same colour and differ only in
        // alpha, so this checks that and then that the alphas rise.
        {
            for (name, theme) in [
                ("dark", Theme::dark()),
                ("light", Theme::light()),
                ("high-contrast", Theme::high_contrast()),
            ] {
                let weights = [
                    ("off-screen", rail_fill(&theme, false, false, false)),
                    ("on-screen", rail_fill(&theme, false, false, true)),
                    ("hover", rail_fill(&theme, false, true, true)),
                    ("focused", rail_fill(&theme, true, false, true)),
                ];
                let rgb = |c: Color| {
                    let p = c.to_rgba8();
                    (p.r, p.g, p.b)
                };
                for (label, c) in &weights[1..] {
                    assert_eq!(
                        rgb(*c),
                        rgb(weights[0].1),
                        "{name}: the {label} weight resolved to a different colour than \
                     off-screen, so the four are not one ramp"
                    );
                }
                let alphas: Vec<u8> = weights.iter().map(|(_, c)| c.to_rgba8().a).collect();
                assert!(
                    alphas.windows(2).all(|w| w[0] < w[1]),
                    "{name}: rail weights must rise off-screen < on-screen < hover < focused, \
                 got {alphas:?}"
                );
            }
        }

        // Case `hit_targets_tile_and_clear_the_minimum_size`.
        {
            let r = rail(5, 0, vec![0, 1]);
            for i in 0..5 {
                let hit = r.hit_rect(i).unwrap();
                assert!(
                    hit.height() >= RAIL_HIT_H,
                    "segment {i}'s target is {} tall, below the minimum {RAIL_HIT_H}",
                    hit.height()
                );
                let c = hit.center();
                assert_eq!(r.hit(c), Some(i), "segment {i} must claim its own centre");
            }
        }
    }
}

#[cfg(test)]
mod ribbon_tests {
    use super::*;
    use vello::kurbo::Point;

    fn ribbon(count: usize, focused: usize) -> StripRibbon {
        StripRibbon {
            content: Rect::new(0.0, 40.0, 1600.0, 900.0),
            count,
            focused,
        }
    }

    #[test]
    fn ribbon_segments_are_ordered_centred_and_prominent_when_focused() {
        // Case `segments_are_ordered_and_disjoint`.
        {
            let r = ribbon(4, 2);
            let rects: Vec<Rect> = (0..4).map(|i| r.segment_rect(i).unwrap()).collect();
            for w in rects.windows(2) {
                assert!(
                    w[1].y0 >= w[0].y1,
                    "segments overlap: {:?} then {:?}",
                    w[0],
                    w[1]
                );
            }
        }

        // Case `the_focused_segment_is_longer`.
        {
            let r = ribbon(3, 1);
            let focused = r.segment_rect(1).unwrap().height();
            for i in [0usize, 2] {
                assert!(
                    focused > r.segment_rect(i).unwrap().height(),
                    "focused segment is not longer than segment {i}"
                );
            }
        }

        // Case `the_ribbon_stays_centred_whichever_strip_has_focus`.
        {
            let content = Rect::new(0.0, 40.0, 1600.0, 900.0);
            for focused in 0..4 {
                let r = StripRibbon {
                    content,
                    count: 4,
                    focused,
                };
                let first = r.segment_rect(0).unwrap();
                let last = r.segment_rect(3).unwrap();
                let mid = f64::midpoint(first.y0, last.y1);
                assert!(
                    (mid - content.center().y).abs() < 0.001,
                    "focus {focused}: ribbon centre {mid} drifted off {}",
                    content.center().y
                );
            }
        }

        // Case `the_focused_segment_is_the_most_prominent_on_every_theme`:
        // `lift` has nothing to move toward on high-contrast, where base and
        // raised are both black, so this pins the ordering explicitly rather
        // than trusting a derived tier.
        {
            use crate::a11y::relative_luminance;
            for (name, theme) in [
                ("dark", Theme::dark()),
                ("light", Theme::light()),
                ("high_contrast", Theme::high_contrast()),
            ] {
                let base = theme.tokens.colors.surfaces.base;
                let g = relative_luminance(base);
                let contrast = |c: Color| {
                    let a = c.components[3];
                    let b = base.components;
                    let over = Color::new([
                        c.components[0] * a + b[0] * (1.0 - a),
                        c.components[1] * a + b[1] * (1.0 - a),
                        c.components[2] * a + b[2] * (1.0 - a),
                        1.0,
                    ]);
                    (relative_luminance(over) - g).abs()
                };
                let idle = contrast(ribbon_fill(&theme, false, false));
                let hover = contrast(ribbon_fill(&theme, false, true));
                let focused = contrast(ribbon_fill(&theme, true, false));
                assert!(
                    idle > 0.0,
                    "{name}: an unfocused segment is invisible, so a second strip \
                 cannot be discovered by looking"
                );
                assert!(
                    hover > idle,
                    "{name}: hover ({hover:.4}) does not lift off idle ({idle:.4})"
                );
                assert!(
                    focused > hover,
                    "{name}: the focused segment ({focused:.4}) is less prominent than a \
                 merely hovered one ({hover:.4}) — the ribbon reads inverted"
                );
            }
        }

        // Case `one_strip_still_draws_a_segment`:
        // Unlike [`StripRail`], a single strip still shows its ribbon segment
        // — it's how the control is discovered at all.
        {
            let r = ribbon(1, 0);
            assert!(r.segment_rect(0).is_some());
            assert!(r.extent() > 0.0);
        }
    }

    #[test]
    fn ribbon_pointer_targets_are_disjoint_and_minimum_sized() {
        // Case `every_segment_is_clickable_at_the_minimum_target`.
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

        // Case `pointer_targets_do_not_overlap`.
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

        // Case `a_click_away_from_the_ribbon_hits_nothing`.
        {
            let r = ribbon(3, 1);
            assert_eq!(r.hit(Point::new(400.0, 470.0)), None);
        }

        // Case `segments_and_targets_sit_flush_on_the_right_edge`.
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
}
