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
use vello::kurbo::{Affine, BezPath, Rect, RoundedRect, Shape};
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
