//! The shared vocabulary the OS's own chrome is drawn from.
//!
//! Everything here exists because the shell's surfaces — the top bar,
//! window decorations, the workspace disc, quick settings, the palette,
//! the collaborator surfaces — were each transliterated from a separate
//! mockup component and carried that mockup's literal values inward.
//! `bg-black/80` for the bar, `bg-slate-900/80` for quick settings,
//! `rounded-3xl` on one panel and a 4 px radius on another, `text-green-400`
//! for a CPU label. The token vocabulary in [`crate::tokens`] was already
//! good; the chrome simply was not reading from it, so surfaces that share
//! a screen did not share a language.
//!
//! The rule this module encodes is in `ui/HIG.md` §9:
//!
//! > Shell chrome does not invent appearance. It selects a **role**, and
//! > the role resolves to tokens.
//!
//! Two pieces were already correct and merely unreachable. [`Colors::lift`]
//! and [`Colors::hairline_on`] measure their move against the ground they
//! are drawn on by relative luminance, so they behave on dark, light and
//! high-contrast without a per-theme branch — and the tab strip's
//! appearance tiers were built on them. But those tiers lived `pub(crate)`
//! inside `widget::tabs`, so the shell could not call them and grew its own
//! incompatible answers instead. Promoting them is most of what this module
//! is: one selection model, reachable from both crates.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Shape};
use vello::peniko::{BlendMode, Color, Fill};

use crate::color_compat::ColorCompat;
use crate::theme::Theme;
use crate::tokens::Colors;

/// Translucency shared by every [`ChromeRole::Bar`] and
/// [`ChromeRole::Panel`] surface, so the whole chrome reads as one
/// material at one opacity.
///
/// Per-surface alpha literals are what made quick settings look like a
/// different substance from the bar it hangs off.
pub const CHROME_SURFACE_ALPHA: f32 = 0.80;

/// Alpha of the hairline that gives a lifted control a defined edge.
pub const HAIRLINE_ALPHA: u8 = 26;

/// Alpha of the edge around a floating [`ChromeRole::Panel`].
///
/// Heavier than a control's hairline because it is doing a different
/// job: a control's edge defines it against a surface the user is
/// already looking at, while a panel's edge has to separate it from
/// whatever content it floats over. On the high-contrast palette it is
/// the *only* thing that separates them — see [`panel_ground`].
pub const PANEL_BORDER_ALPHA: u8 = 92;

/// Smallest relative-luminance gap at which a panel is legible as a
/// distinct surface rather than as a slightly different patch of the
/// desktop. Matches the toolkit's hairline threshold.
const PANEL_MIN_DELTA: f64 = 0.02;

/// Alpha of the inset stroke that gives an active control its inner glow.
pub const GLOW_ALPHA: u8 = 51;

/// How far a hovered control lifts, as a fraction of the full active
/// lift. Hover and active are the same *kind* of change at two strengths,
/// so a control reads as one continuous affordance rather than two
/// unrelated looks.
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
    /// The corner radius for this role.
    ///
    /// `height` is the surface's own height, used only by
    /// [`ChromeRole::Selector`], which is a capsule: asking for
    /// `radii.full` (9999) on a short rect degenerates the rounded-rect
    /// path, so a Selector resolves to half its height instead.
    pub fn radius(self, theme: &Theme, height: f64) -> f64 {
        let radii = &theme.tokens.radii;
        match self {
            // The bar spans its edge; only its corners against the
            // screen edge exist, and those are square.
            ChromeRole::Bar => 0.0,
            ChromeRole::Panel | ChromeRole::Object => radii.xl as f64,
            ChromeRole::Control => radii.lg as f64,
            ChromeRole::Selector => (height / 2.0).max(0.0),
        }
    }

    /// The surface colour for this role, at chrome opacity where the role
    /// is translucent.
    ///
    /// Chrome reads the *semantic* surface tokens, never a ramp slot.
    /// A ramp slot is a colour; a surface token is a promise about
    /// contrast that each theme keeps in its own way, and only the latter
    /// survives a theme its author never saw. Quick settings reaching for
    /// `neutral[1]` is why it looked like a dark slab on a light desktop.
    pub fn surface(self, theme: &Theme) -> Color {
        let colors = &theme.tokens.colors;
        match self {
            ChromeRole::Bar | ChromeRole::Panel => {
                panel_ground(theme).with_alpha_factor(CHROME_SURFACE_ALPHA)
            }
            ChromeRole::Object => colors.surfaces.window_card,
            // Control and Selector bodies come from the selection tiers
            // (see `selection_tier`) rather than from a fixed surface,
            // because their body *is* their state.
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
        Self { active, hover, pressed }
    }
    pub fn active(active: bool) -> Self {
        Self { active, hover: false, pressed: false }
    }
}

/// How one control paints for one frame.
#[derive(Clone, Copy, Debug)]
pub struct SelectionTier {
    /// Body fill. Alpha 0 for an idle control — callers skip the fill
    /// call rather than emitting a no-op draw per control per frame.
    pub fill: Color,
    /// 1 px outer border. `None` unless lifted.
    pub border: Option<Color>,
    /// 1 px inset stroke giving a lifted control a subtle inner glow.
    /// `None` unless lifted.
    pub inner_glow: Option<Color>,
    /// Uniform scale about the control's centre.
    pub scale: f64,
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
/// Selection is a **lift**, never an accent fill. `lift` measures against
/// the chrome's own ground, so it works in every theme; and it leaves the
/// accent free to mean one thing — live state the user did not choose and
/// should notice (recording, sharing, a peer joining, unread). A control
/// that expressed "selected" with the accent would collide with that.
///
/// An active control never gets *more* prominent on hover: the contrast
/// gain belongs to the inactive controls brightening toward the active
/// tier. Pressed implies the lifted tier even before activation
/// propagates, so a click feels instant.
///
/// Note what is **absent**: a shadow. Elevation and selection are
/// different vocabularies — a shadow means "off the surface", a lift means
/// "chosen on the surface". A selected control is not floating, it is
/// picked. Rows of idle controls each laying down a soft shadow is what
/// made the tab strip read as muddy rather than as chrome.
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

// ---------- glass ----------
//
// What a selector *is made of*, as opposed to `selection_tier` above, which
// says how far each state is pushed. The two are separate on purpose: the
// tier table is the semantics ("active is further out than hover") and this
// is the material ("and both are the same substance").
//
// The tab strip is the surface that made the split necessary. Its three
// states were, in practice, three unrelated recipes: an idle pill painted
// nothing, a hovered pill painted a flat wash of literal white at 5 %, and
// an active pill painted an **opaque** body ringed by two white strokes.
// Opaque is the important word — a body you cannot see through is not a
// window onto the desktop, it is a slab sitting on it, and the one light
// cue the hovered pill had (a faint top-down alpha ramp) is silently a
// no-op on it, because scaling the alpha of an already-opaque colour does
// nothing. So the state the strip most needed to look deliberate was the
// only one with no light on it at all.
//
// A surface reads as glass when four things are true at once, and the old
// hover had exactly one of them:
//
//   1. it is translucent, so its colour is partly the ground's,
//   2. its fill is tinted from that ground rather than washed with white,
//   3. there is a light direction — one end of it is lit, the other seated,
//   4. it has an edge, slightly stronger than its fill, that holds the
//      silhouette.
//
// All four are derived per theme here, so the same material survives a
// paper-white desktop, where "brighter" points the other way.

/// Film alpha of a hovered selector.
///
/// Chosen so the composite over the default dark ground lands where the
/// old literal-white hover landed: hover was the one state that read
/// acceptably, and it should not move while the material under it is
/// replaced. On a light theme it now lands somewhere *visible*, which the
/// literal never did.
pub const GLASS_HOVER_ALPHA: u8 = 44;

/// Film alpha of an active or pressed selector.
///
/// Well past hover so the two never read as the same tier, and well short
/// of opaque so the pill keeps the property that makes it glass. Glass is
/// the reason this is a fraction rather than a full body: the ground has
/// to come through, or the material is over.
pub const GLASS_ACTIVE_ALPHA: u8 = 132;

/// Film density at the lit end of a selector, relative to its own alpha.
pub const GLASS_LIT_GAIN: f32 = 1.9;

/// Film density at the seated end, relative to its own alpha.
pub const GLASS_SEAT_FALLOFF: f32 = 0.68;

/// How far down a selector the lit end's extra density has decayed to the
/// body's own value.
///
/// This is what separates a highlight from a wash. Spread over half the
/// pill, the extra film reads as "this pill is a lighter grey"; concentrated
/// near the edge it reads as light landing on a surface, and the rest of the
/// body stays dark enough to keep looking like something you can see through.
pub const GLASS_LIT_SPAN: f32 = 0.2;

/// Edge alpha of a hovered selector. Below [`HAIRLINE_ALPHA`] — a hovered
/// pill is being pointed at, not chosen, and a full hairline on it reads
/// as a second active tab.
///
/// It carries more of the state than it looks like it should, and that is
/// deliberate. A translucent body can only move the ground as far as the
/// palette leaves room for: on the light preset the bar sits about 19
/// levels below white, so a hover film lands about 3 levels off its ground
/// no matter how the film is derived, while the same film moves a graphite
/// ground by twelve. The edge does not have that problem, because
/// [`Colors::hairline_on`] picks whichever polarity moves the ground it is
/// actually drawn on — the same division of labour a floating panel already
/// makes, where a stark palette leaves nearly all the work to its border.
pub const GLASS_EDGE_HOVER_ALPHA: u8 = 28;

/// Edge alpha of an active selector, at the lit end. Runs down to
/// [`GLASS_EDGE_SEAT_SCALE`] of itself at the seated end, which is what
/// makes the edge a light rather than a halo.
pub const GLASS_EDGE_ACTIVE_ALPHA: u8 = 40;

/// How much of its lit-end strength a selector's edge keeps at the seated
/// end.
pub const GLASS_EDGE_SEAT_SCALE: f32 = 0.34;

/// Fraction of a selector's height over which its inner glow survives.
///
/// The glow is a lit *rim*, not a ring: it belongs where the light lands
/// and has no business tracing the bottom of the pill, which is the part
/// in shadow. Stroked all the way round at a constant alpha — which is
/// what an inset stroke in a flat colour does — it is exactly the milky
/// halo that made the active pill look cheap.
pub const GLASS_GLOW_SPAN: f64 = 0.55;

/// How one selector paints, as a material rather than as a tier.
///
/// Same three states and the same ordering as [`selection_tier`] — this is
/// deliberately not a second opinion about what "active" means — but the
/// bodies are films rather than washes, and they carry the light direction
/// the tier table has no field for.
///
/// Kept beside [`selection_tier`] rather than replacing it because the two
/// have different call sites and only one of them paints through
/// [`paint_glass_body`]. A translucent fill handed to a caller that fills
/// it flat is *weaker* than the opaque wash it replaced, so the swap has to
/// happen per surface, together with its painter. The tab strip is the
/// first; the bar's own selectors are the obvious next.
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

/// Where the light falls on a selector, as a gradient offset pair.
///
/// `(lit, seated)` in the 0..1 range down the surface. Light comes from
/// above, so on a palette whose surfaces lift by getting brighter the lit
/// end is the top. On a paper-white palette a film can only ever make the
/// surface *darker*, so the same physical story — top catches the light —
/// is told by thinning the film at the top instead, and the ends swap.
/// Getting this wrong does not merely look flat; it looks lit from below.
fn light_ends(theme: &Theme) -> (f64, f64) {
    if theme.tokens.colors.lifts_bright() {
        (0.0, 1.0)
    } else {
        (1.0, 0.0)
    }
}

/// `base` with its alpha scaled by `gain`, saturating at opaque.
///
/// The clamp is not decoration. Scaling a colour's alpha past 1.0 is not a
/// no-op in a renderer that premultiplies — the components come out
/// multiplied too, so an out-of-range gain silently *brightens* rather than
/// doing nothing, and the amount depends on how close to opaque the input
/// already was. The strip's old body gradient depended on exactly that
/// accident for its highlight. A material should not be built on a
/// library's saturation behaviour.
fn scaled(base: Color, gain: f32) -> Color {
    let a = (base.a() as f32 * gain).clamp(0.0, 255.0).round() as u8;
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
            &stops
                .map(|(offset, gain)| vello::peniko::ColorStop {
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
/// Deliberately lays down **no shadow**. Elevation and selection are
/// separate vocabularies (see [`selection_tier`]), and a row of pills each
/// casting its own soft rings is what made this strip read as muddy in the
/// first place. What replaces it is the edge: a surface that reports its
/// own light does not need a shadow to say it is a surface.
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
                    (0.5, (1.0 + GLASS_EDGE_SEAT_SCALE) / 2.0),
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
        let grad = vello::peniko::Gradient::new_linear(y(lit), y(seated))
            .with_extend(vello::peniko::Extend::Pad)
            .with_stops(
                &[
                    vello::peniko::ColorStop { offset: 0.0, color: glow.into() },
                    vello::peniko::ColorStop {
                        offset: GLASS_GLOW_SPAN as f32,
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

/// The opaque colour a floating chrome surface is built from, guaranteed
/// to register against the desktop behind it.
///
/// `surfaces.overlay` is the theme's declared popover tier and is used
/// verbatim wherever it actually separates from `surfaces.base`. It does
/// not always: on the default light palette `overlay` is `#ffffff` against
/// a `#f3f3f1` ground — a contrast ratio of about 1.06:1 — and on the
/// high-contrast palette the two are *the same colour*. Painting the
/// declared token there gives a panel that is, literally, the desktop
/// with a faint line around it. Rendered side by side, the light and
/// high-contrast quick-settings panels were all but invisible while the
/// dark one looked deliberate, which is what surfaced this.
///
/// Where the declared tier would be swallowed, the ground is moved away
/// from the desktop by the same luminance reasoning
/// [`Colors::hairline_on`] uses, so the panel keeps a body on every
/// theme. This is the fill half; [`PANEL_BORDER_ALPHA`] is the edge half,
/// and a panel needs both — on high-contrast the edge does nearly all the
/// work, because a stark ground with a bright edge *is* that palette's
/// idiom, and lifting its fill would fight it.
pub fn panel_ground(theme: &Theme) -> Color {
    use crate::a11y::relative_luminance;
    let colors = &theme.tokens.colors;
    let base = colors.surfaces.base;
    let declared = colors.surfaces.overlay;
    if (relative_luminance(declared) - relative_luminance(base)).abs() >= PANEL_MIN_DELTA {
        return declared;
    }
    // Composite a derived step over the declared tier so the result stays
    // in the theme's own colour family rather than jumping to a grey.
    let step = Colors::hairline_on(base, 40);
    let a = step.components[3];
    let d = declared.components;
    Color::new([
        step.components[0] * a + d[0] * (1.0 - a),
        step.components[1] * a + d[1] * (1.0 - a),
        step.components[2] * a + d[2] * (1.0 - a),
        1.0,
    ])
}

/// The edge around a floating [`ChromeRole::Panel`]. See
/// [`PANEL_BORDER_ALPHA`].
pub fn panel_border(theme: &Theme) -> Color {
    Colors::hairline_on(theme.tokens.colors.surfaces.base, PANEL_BORDER_ALPHA)
}

/// Lay down a floating panel's elevation, clipped to the ring *outside*
/// its own silhouette.
///
/// The clip is the point. Chrome panels are translucent
/// ([`CHROME_SURFACE_ALPHA`]), and the shared soft-shadow painter ramps
/// its bands inward from the rect's edges as well as outward — which is
/// invisible under an opaque modal, and shows straight through a panel
/// you can see through. Painted unclipped, the quick-settings panel grew
/// pale horizontal bands across its own interior where the shadow's
/// inner ramp met the gaps between its sections.
///
/// Panels are elevated rather than merely tinted because on some
/// palettes the tint alone cannot separate them: see [`panel_ground`].
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
    let e = &theme.tokens.elevation.e4;
    let spread = e.blur as f64;
    let offset_y = e.offset_y as f64;
    let outer = Rect::new(
        rect.x0 - spread * 2.0,
        rect.y0 - spread * 2.0,
        rect.x1 + spread * 2.0,
        rect.y1 + spread * 2.0 + offset_y,
    );
    // Even-odd across [outer, card]: a point inside both winds twice and
    // is excluded, so only the ring around the card paints.
    let mut ring = BezPath::new();
    ring.extend(outer.path_elements(0.1));
    ring.extend(RoundedRect::from_rect(rect, radius).path_elements(0.1));
    scene.push_layer(Fill::EvenOdd, BlendMode::default(), 1.0, transform, &ring);
    crate::surface::soft_shadow(scene, transform, rect, e.color, spread, offset_y, intensity);
    scene.pop_layer();
}

/// A separator or edge drawn on a chrome surface.
///
/// Derived against the ground it sits on rather than declared, so it
/// registers on every theme. A declared `border_standard` stroked onto a
/// translucent panel does not track what is under it and either vanishes
/// on one theme or glares on another.
pub fn chrome_hairline(theme: &Theme) -> Color {
    Colors::hairline_on(theme.tokens.colors.surfaces.overlay, HAIRLINE_ALPHA)
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

    /// How many paint operations the material emits for one selector, by
    /// state, counted off the real encoding.
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

    /// The material paints a body, an edge and (when lifted) a rim — and
    /// nothing else.
    ///
    /// Pinned against the encoding rather than against a predicate,
    /// because the thing worth forbidding is a *paint*, and a helper that
    /// returns `false` is only ever a restatement of itself. The strip used
    /// to lay four stacked black rings under every pill with a body; if
    /// they ever come back here the count moves and this fails, on the dark
    /// theme where the rings are almost invisible as well as on the light
    /// one where they are a grey slab.
    ///
    /// Elevation and selection are different vocabularies. A chosen tab is
    /// picked on the bar, not floating above it, so what reports that a
    /// pill is a surface is its own lit edge.
    #[test]
    fn the_material_paints_a_body_an_edge_and_a_rim_and_nothing_else() {
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

    /// Every state a glass selector paints has to keep the ground
    /// visible through it.
    ///
    /// This is the failure the material was built to fix, and it is worth
    /// stating as a property rather than a look: the old active pill was
    /// **opaque**. That is not a matter of taste — it silently disabled
    /// the one light cue the strip had, because the painter expressed
    /// "brighter at the lit end" by scaling the fill's alpha, and scaling
    /// the alpha of an opaque colour is a no-op. So the state that most
    /// needed to look deliberate was the only one rendered flat, and
    /// nothing in the code said so.
    #[test]
    fn a_glass_body_is_never_opaque_and_always_has_somewhere_to_lighten() {
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

    /// A body and its edge arrive and leave together.
    ///
    /// An edge without a body is a loose outline on the bar; a body
    /// without an edge is a smudge with no silhouette. Tying them to the
    /// same condition is what lets the strip's reveal animation fade one
    /// material rather than two things that happen to agree.
    #[test]
    fn a_glass_body_and_its_edge_are_the_same_decision() {
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

    /// The film is made of the theme's own surface family, and it moves
    /// the ground the same way a flat lift does.
    ///
    /// `surface_white_*` is this idea written as a literal, and it is why
    /// the old hover was invisible on a light theme: white over paper is
    /// paper. Deriving the film instead means the only thing that changes
    /// across the light/dark line is which way "away from the ground"
    /// points — and this asserts the material and [`Colors::lift`] agree
    /// on that, because a strip whose glass lifts one way and whose flat
    /// chrome lifts the other is worse than either.
    #[test]
    fn the_film_lifts_the_same_way_a_flat_lift_does_on_every_preset() {
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

    /// Light comes from above on every palette.
    ///
    /// On a graphite desktop the film brightens, so its dense end is the
    /// top. On a paper desktop a film can only darken, so the same story —
    /// the top catches the light — is told by thinning the film there
    /// instead, and the gradient's ends swap. Painted with a single fixed
    /// orientation, one of the two themes gets a pill lit from below,
    /// which reads as broken rather than as subtle.
    #[test]
    fn the_lit_end_of_a_selector_is_its_top_on_every_preset() {
        for theme in crate::theme::ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
            let (lit, seated) = light_ends(&theme);
            let brightens = theme.tokens.colors.lifts_bright();
            let dense_end_is_top = lit < seated;
            assert_eq!(
                dense_end_is_top, brightens,
                "{}: the film is densest at the {} on a palette whose \
                 surfaces lift {} — that is a pill lit from below",
                theme.name,
                if dense_end_is_top { "top" } else { "bottom" },
                if brightens { "brighter" } else { "darker" },
            );
        }
    }

    /// The tiers are ordered: an active control is further from the
    /// ground than a hovered one, which is further than an idle one.
    /// Asserted per theme, because a fixed white wash satisfies this on
    /// dark and fails on light — which is exactly the bug `lift` exists
    /// to prevent.
    #[test]
    fn selection_tiers_move_progressively_away_from_the_ground() {
        for (name, theme) in themes() {
            let ground = relative_luminance(theme.tokens.colors.surfaces.base);
            let dist = |s: SelectionState| {
                let t = selection_tier(&theme, s);
                // Composite the tier's fill over the ground before
                // measuring: an idle tier is transparent, and its
                // *effective* luminance is the ground's.
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

    /// Selection never reaches for the accent. The accent means live
    /// state; if a selected control used it, "this is chosen" and "this
    /// is live" would be the same pixel.
    #[test]
    fn selection_never_uses_the_accent() {
        for (name, theme) in themes() {
            let accent = theme.tokens.colors.accent[5];
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

    /// An active control does not gain prominence on hover — the whole
    /// contrast budget goes to inactive controls brightening toward it.
    #[test]
    fn hovering_an_active_control_changes_nothing() {
        for (name, theme) in themes() {
            let a = selection_tier(&theme, SelectionState::active(true));
            let ah = selection_tier(&theme, SelectionState::new(true, true, false));
            assert_eq!(
                a.fill.to_rgba8().to_u32(),
                ah.fill.to_rgba8().to_u32(),
                "{name}: an active control brightened on hover"
            );
            assert_eq!(a.scale, ah.scale, "{name}: an active control resized on hover");
        }
    }

    /// A Selector is a capsule at any height, and never asks for the
    /// degenerate `radii.full` value on a short rect.
    #[test]
    fn a_selector_is_a_capsule_at_every_height() {
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

    /// Radii come off the token scale. 16 px is deliberately absent from
    /// that scale (see `tokens::radii`), so no role may resolve to it.
    #[test]
    fn no_role_resolves_to_an_off_scale_radius() {
        let theme = Theme::dark();
        let radii = &theme.tokens.radii;
        let on_scale =
            [0.0, radii.sm as f64, radii.md as f64, radii.lg as f64, radii.xl as f64];
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
            assert!((r - 16.0).abs() > f64::EPSILON, "{role:?} resolved to the absent 16 px step");
        }
    }

    /// Bar and Panel are the same material at the same opacity — that is
    /// what makes a popover read as belonging to the bar it hangs off
    /// rather than as a foreign slab.
    #[test]
    fn bar_and_panel_share_one_material() {
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

    /// A panel is always legible as its own surface, on every theme.
    ///
    /// The declared `surfaces.overlay` token does not guarantee this: on
    /// the light palette it is #ffffff against a #f3f3f1 ground, and on
    /// high-contrast it is the same colour as the ground. A panel that
    /// paints the declared token there is the desktop with a line round
    /// it. Fill *or* edge must carry the separation, and this asserts at
    /// least one of them does.
    #[test]
    fn a_panel_separates_from_the_desktop_on_every_theme() {
        for (name, theme) in themes() {
            let base = theme.tokens.colors.surfaces.base;
            let ground = panel_ground(&theme);
            let border = panel_border(&theme);
            let fill_delta =
                (relative_luminance(ground) - relative_luminance(base)).abs();
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
            let edge_delta =
                (relative_luminance(edge) - relative_luminance(ground)).abs();
            assert!(
                fill_delta >= PANEL_MIN_DELTA || edge_delta >= PANEL_MIN_DELTA,
                "{name}: a panel is indistinguishable from the desktop \
                 (fill delta {fill_delta:.4}, edge delta {edge_delta:.4})"
            );
        }
    }

    /// Chrome surfaces track the theme. A chrome that paints the same
    /// pixels on a light desktop as on a dark one is ignoring the user's
    /// theme, which is the defect this module exists to stop.
    #[test]
    fn chrome_surfaces_differ_between_light_and_dark() {
        let dark = ChromeRole::Panel.surface(&Theme::dark());
        let light = ChromeRole::Panel.surface(&Theme::light());
        assert_ne!(
            dark.to_rgba8().to_u32(),
            light.to_rgba8().to_u32(),
            "the panel surface is identical on light and dark"
        );
    }
}

// ---------------------------------------------------------------------------
// Strip ribbon
// ---------------------------------------------------------------------------

/// Geometry and appearance for the **strip ribbon** — the indicator that
/// says which strip of the current workspace has focus, and how many
/// there are (HIG §9.10, §9.11).
///
/// Strips had no representation on screen whatsoever. A workspace could
/// hold four of them with nothing indicating that a second one existed,
/// no key bound to move between them, and no animation when focus did
/// move. A user could not discover the level of the model they were
/// standing in.
///
/// What the ribbon indicates is worth being precise about. Every strip in
/// a workspace renders at once, sharing the content height between them,
/// so the ribbon does not mean "this is the strip you can see". It means
/// **this is the strip with focus** — the one a new window joins and the
/// one the keyboard is aimed at — which is exactly the fact that was
/// invisible.
///
/// It sits on the content's right edge because the workspace disc owns
/// the left and horizontal scrolling inside the content belongs to
/// groups and tabs; keeping one level of the model per edge is what lets
/// a user tell, from the control alone, what a gesture is about to
/// change.
#[derive(Clone, Copy, Debug)]
pub struct StripRibbon {
    /// Content rect the ribbon is anchored to the right edge of.
    pub content: Rect,
    /// How many strips the workspace holds.
    pub count: usize,
    /// Index of the focused strip.
    pub focused: usize,
}

/// Width of a ribbon segment.
pub const RIBBON_SEG_W: f64 = 6.0;
/// Height of an unfocused segment.
pub const RIBBON_SEG_H: f64 = 22.0;
/// Height of the focused segment. Longer, so the ribbon reads at a glance
/// without needing colour to carry the whole signal — which is what keeps
/// it legible on the high-contrast palette.
pub const RIBBON_SEG_H_FOCUSED: f64 = 40.0;
/// Gap between segments.
///
/// Sized so that segment height plus gap clears the minimum pointer
/// target: an unfocused segment is [`RIBBON_SEG_H`] tall, and its hit
/// rect claims half the gap either side, so the pitch is exactly
/// [`RIBBON_HIT_W`]. Adjacent targets therefore tile without overlapping
/// — a click near a boundary belongs to exactly one strip.
pub const RIBBON_GAP: f64 = RIBBON_HIT_W - RIBBON_SEG_H;
/// Distance from the content's right edge to the ribbon.
///
/// Small, so the ribbon hugs the screen edge and reads as chrome the
/// shell owns rather than as something attached to the window beside it.
/// Rendered at a larger margin it sat a few pixels from the Notes app's
/// own scrollbar, and the two vertical marks read as a pair.
pub const RIBBON_MARGIN: f64 = 5.0;
/// Minimum pointer target (HIG §9.6). The segment itself is 6 px wide;
/// the hit rect around it is not.
pub const RIBBON_HIT_W: f64 = 32.0;

/// Weight of an unfocused ribbon segment against the desktop.
const RIBBON_IDLE_ALPHA: u8 = 46;
/// Weight of a hovered but unfocused segment.
const RIBBON_HOVER_ALPHA: u8 = 96;
/// Weight of the focused segment.
const RIBBON_FOCUSED_ALPHA: u8 = 224;

/// Body of one ribbon segment.
///
/// The ribbon cannot use [`selection_tier`] unmodified, and the reason is
/// worth recording. That tier is built for a control sitting in a row of
/// its own, where "unselected" can legitimately be transparent because
/// the row around it supplies the context. A ribbon segment has no row:
/// an unfocused segment painted at zero alpha would mean the only
/// evidence that a second strip exists is invisible.
///
/// Giving unfocused segments a floor is not enough on its own either.
/// [`Colors::lift`] has nothing to move toward on the high-contrast
/// palette — `surfaces.base` and `surfaces.raised` are both pure black —
/// so it falls back to a weak derived step, and a floored unfocused
/// segment came out *brighter than the focused one*. Rendered, the
/// high-contrast ribbon read inverted: the strip with focus was the
/// dimmest of the three.
///
/// So the ribbon states are three explicitly ordered weights against the
/// desktop's own ground, which keeps the ordering true on every palette
/// and gives high-contrast what it actually wants — a bright mark on
/// black.
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

impl StripRibbon {
    /// Total height the whole ribbon occupies.
    pub fn extent(&self) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        let focused = if self.focused < self.count { 1 } else { 0 };
        let plain = self.count - focused;
        plain as f64 * RIBBON_SEG_H
            + focused as f64 * RIBBON_SEG_H_FOCUSED
            + (self.count.saturating_sub(1)) as f64 * RIBBON_GAP
    }

    /// Rect of segment `i`, or `None` if out of range.
    pub fn segment_rect(&self, i: usize) -> Option<Rect> {
        if i >= self.count {
            return None;
        }
        let x1 = self.content.x1 - RIBBON_MARGIN;
        let x0 = x1 - RIBBON_SEG_W;
        let mut y = self.content.center().y - self.extent() / 2.0;
        for k in 0..i {
            y += if k == self.focused { RIBBON_SEG_H_FOCUSED } else { RIBBON_SEG_H };
            y += RIBBON_GAP;
        }
        let h = if i == self.focused { RIBBON_SEG_H_FOCUSED } else { RIBBON_SEG_H };
        Some(Rect::new(x0, y, x1, y + h))
    }

    /// Pointer target for segment `i` — widened to the minimum target
    /// size and half the gap on each side, so the ribbon is clickable
    /// rather than merely visible.
    pub fn hit_rect(&self, i: usize) -> Option<Rect> {
        let seg = self.segment_rect(i)?;
        let cx = seg.center().x;
        Some(Rect::new(
            cx - RIBBON_HIT_W / 2.0,
            seg.y0 - RIBBON_GAP / 2.0,
            cx + RIBBON_HIT_W / 2.0,
            seg.y1 + RIBBON_GAP / 2.0,
        ))
    }

    /// Which segment, if any, contains `p`.
    pub fn hit(&self, p: vello::kurbo::Point) -> Option<usize> {
        (0..self.count).find(|i| self.hit_rect(*i).is_some_and(|r| r.contains(p)))
    }

    /// Paint the ribbon. `hover` is the segment under the pointer.
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
            let Some(seg) = self.segment_rect(i) else { continue };
            let tier = selection_tier(
                theme,
                SelectionState::new(i == self.focused, hover == Some(i), false),
            );
            let shape = RoundedRect::from_rect(seg, ChromeRole::Selector.radius(theme, seg.height()));
            let fill = ribbon_fill(theme, i == self.focused, hover == Some(i));
            scene.fill(Fill::NonZero, transform, fill.with_alpha_factor(alpha), None, &shape);
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
/// standing.
///
/// The counterpart of [`StripRibbon`], for the axis that had nothing.
/// A strip is a horizontal run of window groups wider than the display,
/// so at any moment most of it is off screen — and nothing on screen said
/// so. A user looking at two windows had no way to tell whether that was
/// the whole workspace or the middle of six, and therefore no reason to
/// look for a way to travel. That is the state a live session produced
/// the question "how do I change windows / scroll left to right in a
/// strip?" from.
///
/// Three facts, in the order they matter:
///
///   * **How many windows this strip holds** — one segment each. This is
///     the fact that makes travel worth attempting.
///   * **Which one has focus** — the long, bright segment.
///   * **Which ones are on screen right now** — the middle weight. This
///     is what turns the rail into a position readout rather than a
///     count: the lit run slides as the strip pans.
///
/// It is a control, not a readout: clicking a segment focuses that
/// window. A user who cannot yet name the gesture can still get where
/// they are going by pointing at the map, which is the whole reason the
/// rail is clickable rather than decorative.
///
/// Bottom edge, because the disc owns the left and the strip ribbon owns
/// the right — one level of the model per edge, so the control alone says
/// what a gesture is about to change.
#[derive(Clone, Debug)]
pub struct StripRail {
    /// Content rect the rail is anchored to the bottom edge of.
    pub content: Rect,
    /// How many window groups this strip holds.
    pub count: usize,
    /// Index of the focused group.
    pub focused: usize,
    /// Indices currently at least partly on screen. Empty before the
    /// first layout pass has anything to report.
    pub visible: Vec<usize>,
}

/// Thickness of the rail. Matches [`RIBBON_SEG_W`] on its own axis, so
/// the two controls read as the same object turned ninety degrees.
pub const RAIL_SEG_H: f64 = 6.0;
/// Length of a segment for a group that does not have focus.
pub const RAIL_SEG_W: f64 = 22.0;
/// Length of the focused segment. Longer, so focus survives a palette
/// that has no colour to spare — the same reason the ribbon's focused
/// segment is taller.
pub const RAIL_SEG_W_FOCUSED: f64 = 40.0;
/// Gap between segments. Sized like the ribbon's so each segment's hit
/// rect claims half the gap either side and adjacent targets tile
/// without overlapping.
pub const RAIL_GAP: f64 = RAIL_HIT_H - RAIL_SEG_W;
/// Distance from the content's bottom edge to the rail.
pub const RAIL_MARGIN: f64 = 10.0;
/// Minimum pointer target (HIG §9.6) across the rail's short axis.
pub const RAIL_HIT_H: f64 = 32.0;

/// Weight of a segment whose group is off screen.
///
/// Deliberately heavier than the ribbon's idle step. An unfocused
/// *strip* segment only has to hint that another strip exists; an
/// off-screen *window* segment is the rail's entire message — it is the
/// mark that says the strip continues past the edge of the display, and
/// a mark nobody notices does not deliver it.
const RAIL_ABSENT_ALPHA: u8 = 64;
/// Weight of a segment whose group is on screen but not focused.
const RAIL_PRESENT_ALPHA: u8 = 110;
/// Weight of a hovered segment, whether or not its group is on screen.
const RAIL_HOVER_ALPHA: u8 = 160;
/// Weight of the focused segment.
const RAIL_FOCUSED_ALPHA: u8 = 224;

/// Body of one rail segment.
///
/// Four explicitly ordered weights against the desktop's own ground, for
/// the reason [`ribbon_fill`] records at length: [`Colors::lift`] has
/// nothing to move toward on the high-contrast palette, where
/// `surfaces.base` and `surfaces.raised` are both pure black, so a tier
/// system that derives its steps can invert. Ordering weights by hand
/// keeps focus the brightest mark on every palette.
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
    /// Whether the rail has anything to say. One window is not a strip
    /// you can travel along, and a single segment would be noise.
    pub fn is_meaningful(&self) -> bool {
        self.count >= 2
    }

    /// Total width the whole rail occupies.
    pub fn extent(&self) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        let focused = if self.focused < self.count { 1 } else { 0 };
        let plain = self.count - focused;
        plain as f64 * RAIL_SEG_W
            + focused as f64 * RAIL_SEG_W_FOCUSED
            + (self.count.saturating_sub(1)) as f64 * RAIL_GAP
    }

    /// Rect of segment `i`, or `None` if out of range.
    pub fn segment_rect(&self, i: usize) -> Option<Rect> {
        if i >= self.count {
            return None;
        }
        let y1 = self.content.y1 - RAIL_MARGIN;
        let y0 = y1 - RAIL_SEG_H;
        let mut x = self.content.center().x - self.extent() / 2.0;
        for k in 0..i {
            x += if k == self.focused { RAIL_SEG_W_FOCUSED } else { RAIL_SEG_W };
            x += RAIL_GAP;
        }
        let w = if i == self.focused { RAIL_SEG_W_FOCUSED } else { RAIL_SEG_W };
        Some(Rect::new(x, y0, x + w, y1))
    }

    /// Pointer target for segment `i` — widened to the minimum target
    /// size across the rail's short axis and half the gap either side,
    /// so the rail is clickable rather than merely visible.
    pub fn hit_rect(&self, i: usize) -> Option<Rect> {
        let seg = self.segment_rect(i)?;
        let cy = seg.center().y;
        Some(Rect::new(
            seg.x0 - RAIL_GAP / 2.0,
            cy - RAIL_HIT_H / 2.0,
            seg.x1 + RAIL_GAP / 2.0,
            cy + RAIL_HIT_H / 2.0,
        ))
    }

    /// Which segment, if any, contains `p`.
    pub fn hit(&self, p: Point) -> Option<usize> {
        if !self.is_meaningful() {
            return None;
        }
        (0..self.count).find(|i| self.hit_rect(*i).is_some_and(|r| r.contains(p)))
    }

    /// Paint the rail. `hover` is the segment under the pointer.
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
            let Some(seg) = self.segment_rect(i) else { continue };
            let present = self.visible.contains(&i);
            let shape =
                RoundedRect::from_rect(seg, ChromeRole::Selector.radius(theme, seg.height()));
            let fill = rail_fill(theme, i == self.focused, hover == Some(i), present);
            scene.fill(Fill::NonZero, transform, fill.with_alpha_factor(alpha), None, &shape);
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

    /// One window is not a strip you can travel along.
    #[test]
    fn a_single_group_draws_no_rail() {
        assert!(!rail(1, 0, vec![0]).is_meaningful());
        assert!(rail(2, 0, vec![0]).is_meaningful());
        assert!(rail(1, 0, vec![0]).hit(Point::new(800.0, 884.0)).is_none());
    }

    /// Segments tile left to right without overlapping, and the focused
    /// one is the long one.
    #[test]
    fn segments_run_in_order_with_the_focused_one_longest() {
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
            rects.iter().all(|s| (s.y1 - (r.content.y1 - RAIL_MARGIN)).abs() < f64::EPSILON),
            "every segment sits on the rail's baseline"
        );
    }

    /// Focus is the strongest mark on every palette, and a group that is
    /// off screen is fainter than one that is on it. Both orderings are
    /// what let the rail be read as a position rather than a count.
    ///
    /// [`Colors::hairline_on`] resolves the direction to move once per
    /// ground, so on a given palette all four weights are the same
    /// colour and differ only in alpha — the assertion checks exactly
    /// that (a weight that came out a different colour would not be
    /// comparable by alpha at all) and then that the alphas rise. This
    /// is the ordering the ribbon's own notes record going wrong on
    /// high-contrast, where `surfaces.base` and `surfaces.raised` are
    /// both pure black and a derived tier system inverts.
    #[test]
    fn weights_stay_ordered_on_every_palette() {
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

    /// Every segment is reachable by pointer at the HIG's minimum target
    /// size, and a click near a boundary belongs to exactly one of them.
    #[test]
    fn hit_targets_tile_and_clear_the_minimum_size() {
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

#[cfg(test)]
mod ribbon_tests {
    use super::*;
    use vello::kurbo::Point;

    fn ribbon(count: usize, focused: usize) -> StripRibbon {
        StripRibbon { content: Rect::new(0.0, 40.0, 1600.0, 900.0), count, focused }
    }

    /// Segments appear in strip order, top to bottom, and never overlap.
    #[test]
    fn segments_are_ordered_and_disjoint() {
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

    /// The focused segment is visibly longer than the others, so the
    /// ribbon reads without relying on colour alone — which is what keeps
    /// it legible on the high-contrast palette.
    #[test]
    fn the_focused_segment_is_longer() {
        let r = ribbon(3, 1);
        let focused = r.segment_rect(1).unwrap().height();
        for i in [0usize, 2] {
            assert!(
                focused > r.segment_rect(i).unwrap().height(),
                "focused segment is not longer than segment {i}"
            );
        }
    }

    /// The ribbon is centred in the content it belongs to, whichever
    /// segment has focus — so changing strips does not shift the whole
    /// control up or down the screen.
    #[test]
    fn the_ribbon_stays_centred_whichever_strip_has_focus() {
        let content = Rect::new(0.0, 40.0, 1600.0, 900.0);
        for focused in 0..4 {
            let r = StripRibbon { content, count: 4, focused };
            let first = r.segment_rect(0).unwrap();
            let last = r.segment_rect(3).unwrap();
            let mid = (first.y0 + last.y1) / 2.0;
            assert!(
                (mid - content.center().y).abs() < 0.001,
                "focus {focused}: ribbon centre {mid} drifted off {}",
                content.center().y
            );
        }
    }

    /// Every segment is clickable at the minimum pointer target, even
    /// though the painted segment is 6 px wide.
    #[test]
    fn every_segment_is_clickable_at_the_minimum_target() {
        let r = ribbon(3, 0);
        for i in 0..3 {
            let hit = r.hit_rect(i).unwrap();
            assert!(
                hit.width() >= 32.0 && hit.height() >= 32.0,
                "segment {i} target is {}x{}, under the 32 px minimum",
                hit.width(),
                hit.height()
            );
            assert_eq!(r.hit(hit.center()), Some(i), "segment {i} is not hittable at its centre");
        }
    }

    /// Pointer targets tile without overlapping, so a click near a
    /// boundary resolves to exactly one strip rather than to whichever
    /// the hit-test happened to check first.
    #[test]
    fn pointer_targets_do_not_overlap() {
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

    /// A click in the gutter beside the ribbon is not a strip switch.
    #[test]
    fn a_click_away_from_the_ribbon_hits_nothing() {
        let r = ribbon(3, 1);
        assert_eq!(r.hit(Point::new(400.0, 470.0)), None);
    }

    /// The focused segment is the *most* prominent one, on every theme.
    ///
    /// This is the assertion that was missing when the ribbon first
    /// rendered: on the high-contrast palette the focused segment came
    /// out dimmer than its unfocused neighbours, because `lift` has
    /// nothing to move toward when base and raised are both black while
    /// the unfocused floor was a fixed weight. The ribbon read inverted
    /// and only a rendered crop showed it.
    #[test]
    fn the_focused_segment_is_the_most_prominent_on_every_theme() {
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
            assert!(hover > idle, "{name}: hover ({hover:.4}) does not lift off idle ({idle:.4})");
            assert!(
                focused > hover,
                "{name}: the focused segment ({focused:.4}) is less prominent than a \
                 merely hovered one ({hover:.4}) — the ribbon reads inverted"
            );
        }
    }

    /// A workspace with one strip still shows its ribbon: that single
    /// segment is how the control is discovered at all.
    #[test]
    fn one_strip_still_draws_a_segment() {
        let r = ribbon(1, 0);
        assert!(r.segment_rect(0).is_some());
        assert!(r.extent() > 0.0);
    }
}
