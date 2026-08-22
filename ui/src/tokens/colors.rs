use vello::peniko::Color;
use crate::color_compat::ColorCompat;

/// Index into the accent ramp that maps to the theme's dominant accent
/// fill. Used wherever a widget needs the "primary accent" colour (active
/// dot, slider active track, checkbox tick, toggle on-state, etc.). The
/// *hover* shade lives one step earlier, at [`ACCENT_HOVER_IDX`].
pub const ACCENT_PRIMARY_IDX: usize = 5;

/// Index into the accent ramp that maps to the theme's hover-state accent
/// (one step darker than [`ACCENT_PRIMARY_IDX`]). Bound to a const so future
/// ramp swaps stay coherent with [`ACCENT_PRIMARY_IDX`].
pub const ACCENT_HOVER_IDX: usize = 4;

/// How far [`Colors::legible_fill`] may deepen a chip's fill toward black
/// before it gives up and re-picks the label instead. At 35 % a mid-tone
/// accent still reads as itself; past it the pale accents (Nord's frost,
/// Gruvbox's yellow, Rosé Pine's rose) go muddy, and those are exactly the
/// ones a dark label suits anyway.
pub const MAX_CHIP_DEEPEN: f32 = 0.35;

/// Translucent black scrim painted over an interactive widget whose
/// [`PermissionScope`](crate::permissions::PermissionScope) is denied, to
/// signal it is not actionable. Single source of truth so the dim is
/// identical across Button / Checkbox / Toggle / Slider / Container / …
/// (it was previously a `Color::from_rgba8(0,0,0,77)` literal copy-pasted
/// into 13 widgets).
pub const PERMISSION_DENIED_SCRIM: Color = Color::from_rgba8(0, 0, 0, 77);

/// Mix two colours in straight (non-premultiplied) 8-bit sRGB space.
/// `t = 0` → `a`, `t = 1` → `b`. This is the same space the Themes app and
/// most widget draw paths blend in, so tints stay consistent.
fn mix8(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| ((x as f32) * (1.0 - t) + (y as f32) * t).round() as u8;
    Color::from_rgb8(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Same colour with an explicit 8-bit alpha.
fn with_a(c: Color, a: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), a)
}

#[derive(Clone, Debug)]
pub struct ColorRamp { pub stops: [Color; 11] }

impl ColorRamp {
    pub fn len(&self) -> usize { self.stops.len() }

    /// Build an 11-stop ramp interpolating `a` (index 0) → `b` (index 10) in
    /// straight sRGB, eased so the low indices cluster near `a`. The easing
    /// keeps the dense-at-the-dark-end feel of a hand-tuned ramp (like
    /// Tailwind's slate) so `neutral[2..5]` — the hover / pressed / muted
    /// surface tints most widgets read — stay close to the base rather than
    /// racing to the light end on a straight lerp.
    pub fn eased(a: Color, b: Color) -> Self {
        let mut stops = [a; 11];
        for (i, stop) in stops.iter_mut().enumerate() {
            let t = (i as f32 / 10.0).powf(1.5);
            *stop = mix8(a, b, t);
        }
        Self { stops }
    }
}

impl std::ops::Index<usize> for ColorRamp {
    type Output = Color;
    fn index(&self, i: usize) -> &Color { &self.stops[i] }
}

#[derive(Clone, Debug)]
pub struct Surfaces {
    pub base: Color,
    pub raised: Color,
    pub overlay: Color,
    /// Hue of the dim painted behind a modal surface — a palette, a
    /// settings sheet, the overview. Opaque: the *strength* of the dim
    /// belongs to whatever is being lifted (each overlay fades its own
    /// scrim in with itself), while the hue belongs to the theme, so a
    /// scrim recedes into the chrome's own colour family instead of
    /// punching a black hole through it.
    pub scrim: Color,
    pub glass: Color,
    pub glass_blur_px: f32,
    /// The card that frames each window tile. Sits on top of the shell
    /// backdrop; for dark themes a light card (dark app content reads on it),
    /// for light themes plain white.
    pub window_card: Color,
    /// Subtle border around the window card.
    pub window_card_border: Color,
}

#[derive(Clone, Debug)]
pub struct SemanticColors { pub success: Color, pub warning: Color, pub danger: Color, pub info: Color }

#[derive(Clone, Debug)]
pub struct Colors {
    pub neutral: ColorRamp,
    pub accent: ColorRamp,
    pub semantic: SemanticColors,
    pub surfaces: Surfaces,
    pub text_primary: Color,
    /// Secondary text — captions, sub-labels, de-emphasised copy. One clear
    /// step down from [`Self::text_primary`] but still comfortably legible.
    pub text_dim: Color,
    /// Tertiary text — the quietest readable tier (timestamps, hints,
    /// placeholder-adjacent metadata). Sits below [`Self::text_dim`]; still
    /// clears large-text contrast against the base surface, so it reads as
    /// "quiet", never "broken".
    pub text_tertiary: Color,
    pub border_standard: Color,
    pub border_focus: Color,

    // --- state veneers ----------------------------------------------------
    //
    // The translucent washes widgets paint over whatever surface they sit on
    // to signal hover / press / selection. They are **directional**: a veneer
    // has to *lighten* a dark ground and *darken* a light one, otherwise it
    // vanishes. Painting white over a light theme's `#FFFFFF` card is a
    // no-op, which is how hover feedback silently disappeared in every light
    // theme. [`Colors::themed`] therefore picks the polarity per theme; the
    // three alpha tiers are chosen so the perceptual step (ΔL*) of each tier
    // matches across light and dark, rather than the raw alpha matching.
    /// The quietest veneer — a "barely there" hover wash or faint divider.
    /// ≈ 5 % on a dark ground, ≈ 6 % on a light one.
    pub overlay_subtle: Color,
    /// The middle veneer — a settled hover, a selected row, a resting chip.
    pub overlay_medium: Color,
    /// The strongest veneer — pressed states and emphatic fills.
    pub overlay_strong: Color,
    /// Hairline structure that has to read against the local surface rather
    /// than against the theme ground (a divider inside a tinted panel).
    pub overlay_border_subtle: Color,
    /// The more assertive hairline — an active tab's outline, a focused chip.
    pub overlay_border_strong: Color,

    /// Alias of [`Self::overlay_subtle`], kept because the whole app fleet
    /// references it by this name. The value is no longer literally white —
    /// it is whichever polarity the theme needs. Prefer the semantic name in
    /// new code.
    pub surface_white_5: Color,
    /// Alias of [`Self::overlay_medium`]. See [`Self::surface_white_5`].
    pub surface_white_10: Color,
    /// Alias of [`Self::overlay_strong`]. See [`Self::surface_white_5`].
    pub surface_white_20: Color,
    /// Alias of [`Self::overlay_border_subtle`]. See [`Self::surface_white_5`].
    pub border_white_10: Color,
    /// Alias of [`Self::overlay_border_strong`]. See [`Self::surface_white_5`].
    pub border_white_20: Color,
}

/// The five state-veneer values for one theme, in tier order:
/// `(subtle, medium, strong, border_subtle, border_strong)`.
pub type OverlayTiers = (Color, Color, Color, Color, Color);

/// The veneer tiers for a **dark** ground: white at 5 % / 10 % / 20 %.
pub fn dark_overlays() -> OverlayTiers {
    let w = |a: u8| Color::from_rgba8(255, 255, 255, a);
    (w(13), w(26), w(51), w(26), w(51))
}

/// The veneer tiers for a **light** ground: black at ≈ 6 % / 12 % / 24 %.
///
/// The alphas are not the dark ones mirrored. Equal alpha does not buy equal
/// visibility, because a veneer's perceptual step depends on where the ground
/// sits on the lightness curve. These were solved for: black at 15 / 31 / 60
/// reproduces the dark tiers' ΔL* (≈ 5 / 10.5 / 21) to within half a unit on
/// the ground *and* card colour of every light theme qOS ships — see
/// `light_overlays_match_the_dark_perceptual_step`.
pub fn light_overlays() -> OverlayTiers {
    let k = |a: u8| Color::from_rgba8(0, 0, 0, a);
    (k(15), k(31), k(60), k(31), k(60))
}

/// A compact description of a full palette. Every qOS theme is expressed as
/// one of these and expanded into a coherent [`Colors`] token set by
/// [`Colors::themed`] — so a preset only states the ~15 colours that carry
/// its identity, and the ramps / translucent surfaces / scrim / glass are
/// derived consistently for every theme.
#[derive(Clone, Debug)]
pub struct PaletteSpec {
    /// Dark themes ramp their neutrals darkest→lightest; light themes invert.
    pub dark: bool,
    /// The window / page ground.
    pub base: Color,
    /// Raised cards, panels, controls — one elevation above [`Self::base`].
    pub raised: Color,
    /// The deep, rich surface for popovers / menus / the top-bar backdrop.
    /// This is the "near-black context menu" tier — usually the darkest
    /// (dark themes) or a crisp elevated white (light themes).
    pub popover: Color,
    pub text: Color,
    pub text_dim: Color,
    pub text_tertiary: Color,
    pub border: Color,
    /// Focus ring / selected-outline colour (often, but not always, the accent).
    pub focus: Color,
    /// Primary accent fill (interactive / selected states).
    pub accent: Color,
    /// Hover accent — one step from [`Self::accent`].
    pub accent_hover: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub info: Color,
    /// Override the window-card colour; `None` derives a sensible default
    /// (light card on dark themes, white on light themes).
    pub window_card: Option<Color>,
}

impl Colors {
    /// Expand a [`PaletteSpec`] into a full, coherent token set. Surfaces,
    /// text, accent and semantic colours are taken verbatim from the spec;
    /// the neutral ramp, accent ramp, scrim, glass and translucent-white
    /// veneers are derived so every theme has the same structural richness
    /// without hand-authoring 40 values per preset.
    pub fn themed(p: PaletteSpec) -> Self {
        let black = Color::from_rgb8(0, 0, 0);

        // Neutral ramp: dark themes run a deep shade → the text colour;
        // light themes run the (light) base → the (dark) text colour, which
        // naturally inverts the ramp direction.
        let (n_lo, n_hi) = if p.dark {
            (mix8(p.base, black, 0.45), p.text)
        } else {
            (p.base, p.text)
        };
        let neutral = ColorRamp::eased(n_lo, n_hi);

        // Accent ramp: a full shade→tint spread with the primary and hover
        // stops pinned to the spec so [`ACCENT_PRIMARY_IDX`] / [`ACCENT_HOVER_IDX`]
        // land exactly on the theme's chosen accent.
        let mut accent = ColorRamp::eased(mix8(p.accent, black, 0.62), mix8(p.accent, Color::WHITE, 0.85));
        accent.stops[ACCENT_HOVER_IDX] = p.accent_hover;
        accent.stops[ACCENT_PRIMARY_IDX] = p.accent;

        let window_card = p.window_card.unwrap_or_else(|| {
            if p.dark { Color::from_rgb8(0xF4, 0xF5, 0xF7) } else { Color::WHITE }
        });
        let window_card_border = if p.dark {
            Color::from_rgba8(255, 255, 255, 51)
        } else {
            Color::from_rgba8(0, 0, 0, 30)
        };

        let scrim = if p.dark { mix8(p.base, black, 0.55) } else { Color::from_rgb8(12, 12, 14) };

        // A state veneer must move the ground it lands on, so its polarity
        // follows the theme rather than being fixed white.
        let (o_subtle, o_medium, o_strong, ob_subtle, ob_strong) =
            if p.dark { dark_overlays() } else { light_overlays() };

        Self {
            neutral,
            accent,
            semantic: SemanticColors {
                success: p.success,
                warning: p.warning,
                danger: p.danger,
                info: p.info,
            },
            surfaces: Surfaces {
                base: p.base,
                raised: p.raised,
                // Fully opaque: popovers, context menus, dropdowns and toast
                // surfaces must read as a solid deep surface with no page
                // content bleeding through. The translucent, blurred tier is
                // `glass` (below); the overlay tier is deliberately solid.
                overlay: p.popover,
                scrim,
                glass: with_a(mix8(p.base, p.raised, 0.4), 214),
                glass_blur_px: 24.0,
                window_card,
                window_card_border,
            },
            text_primary: p.text,
            text_dim: p.text_dim,
            text_tertiary: p.text_tertiary,
            border_standard: p.border,
            border_focus: p.focus,
            overlay_subtle: o_subtle,
            overlay_medium: o_medium,
            overlay_strong: o_strong,
            overlay_border_subtle: ob_subtle,
            overlay_border_strong: ob_strong,
            surface_white_5:  o_subtle,
            surface_white_10: o_medium,
            surface_white_20: o_strong,
            border_white_10:  ob_subtle,
            border_white_20:  ob_strong,
        }
    }

    /// Replace the state-veneer tiers wholesale. Themes assembled outside
    /// [`Self::themed`] — the high-contrast preset, which starts from the dark
    /// tokens and overrides surfaces to pure black — use this so the veneers
    /// stay coherent with the surfaces they will be painted onto, instead of
    /// inheriting a polarity that was right for a different ground.
    pub fn set_overlays(&mut self, tiers: OverlayTiers) {
        let (subtle, medium, strong, border_subtle, border_strong) = tiers;
        self.overlay_subtle = subtle;
        self.overlay_medium = medium;
        self.overlay_strong = strong;
        self.overlay_border_subtle = border_subtle;
        self.overlay_border_strong = border_strong;
        self.surface_white_5 = subtle;
        self.surface_white_10 = medium;
        self.surface_white_20 = strong;
        self.border_white_10 = border_subtle;
        self.border_white_20 = border_strong;
    }

    /// Pick a foreground colour that stays legible on `bg`.
    ///
    /// Returns `preferred` whenever it already clears WCAG AA for body text
    /// (4.5:1) against `bg`, so a design's intended quiet tier survives
    /// wherever it is actually readable. Where it does not — a dim label on a
    /// mid-tone fill, a white label on a pale accent — this falls back to the
    /// theme's primary text, and finally to plain white or black, whichever
    /// wins. The last step cannot fail: one of the two always clears 4.5:1
    /// against any colour, because their ratio to each other is 21:1.
    ///
    /// `bg` must be the colour that will actually be *seen*: composite any
    /// translucent fill over its ground before passing it in.
    pub fn readable_on(&self, bg: Color, preferred: Color) -> Color {
        const AA_BODY: f64 = 4.5;
        for candidate in [preferred, self.text_primary] {
            if crate::a11y::contrast_ratio(candidate, bg) >= AA_BODY {
                return candidate;
            }
        }
        let white = Color::WHITE;
        let black = Color::BLACK;
        if crate::a11y::contrast_ratio(white, bg) >= crate::a11y::contrast_ratio(black, bg) {
            white
        } else {
            black
        }
    }

    /// Resolve a **filled chip** — an accent button, a danger button, the
    /// selected segment of a control — into a `(fill, label)` pair whose
    /// label clears WCAG AA (4.5:1) against the fill.
    ///
    /// A light label on a saturated fill is the fleet's design language, so
    /// this preserves it wherever it can: if `preferred_label` already clears
    /// AA the pair is returned untouched, and otherwise the **fill is deepened
    /// toward black** until the label clears, rather than the label being
    /// swapped. A slightly deeper accent still reads as the same component; a
    /// black label on an accent chip reads as a different one.
    ///
    /// Deepening stops at [`MAX_CHIP_DEEPEN`]. Past that the fill has lost the
    /// identity that made it the accent — Gruvbox's yellow needs 41 % to host
    /// white and lands on mustard, Rosé Pine's rose lands on a muddy mauve —
    /// and for those pale accents a dark label is both legible and the more
    /// idiomatic look, so the fill is kept and the label re-picked via
    /// [`Self::readable_on`].
    ///
    /// `fill` must be opaque: it is what the label will actually sit on.
    pub fn legible_fill(&self, fill: Color, preferred_label: Color) -> (Color, Color) {
        const AA_BODY: f64 = 4.5;
        if crate::a11y::contrast_ratio(preferred_label, fill) >= AA_BODY {
            return (fill, preferred_label);
        }
        let black = Color::BLACK;
        // Walk in 1 % steps: the smallest deepening that clears the bar keeps
        // the chip as close to the theme's own accent as legibility allows.
        let mut step = 1u32;
        while (step as f32) / 100.0 <= MAX_CHIP_DEEPEN {
            let deepened = mix8(fill, black, (step as f32) / 100.0);
            if crate::a11y::contrast_ratio(preferred_label, deepened) >= AA_BODY {
                return (deepened, preferred_label);
            }
            step += 1;
        }
        (fill, self.readable_on(fill, preferred_label))
    }

    /// The default dark palette — a calm, neutral **graphite** ground with a
    /// faint cool bias (deliberately *not* a blue-drenched slate), soft
    /// off-white typography in three legible tiers, deep near-black popovers,
    /// and a single restrained blue accent reserved for interactive and
    /// selected states.
    pub fn dark() -> Self {
        Self::themed(PaletteSpec {
            dark: true,
            base:          Color::from_rgb8(0x16, 0x17, 0x1B), // graphite ground
            raised:        Color::from_rgb8(0x1F, 0x20, 0x25), // cards / panels
            popover:       Color::from_rgb8(0x0D, 0x0E, 0x11), // rich near-black menu
            text:          Color::from_rgb8(0xEC, 0xED, 0xF0), // soft near-white
            text_dim:      Color::from_rgb8(0x9A, 0x9C, 0xA6), // secondary
            text_tertiary: Color::from_rgb8(0x6B, 0x6D, 0x77), // tertiary / hints
            border:        Color::from_rgb8(0x2B, 0x2C, 0x33), // subtle structure
            focus:         Color::from_rgb8(0x58, 0x91, 0xE8), // accent ring
            accent:        Color::from_rgb8(0x58, 0x91, 0xE8), // restrained blue
            accent_hover:  Color::from_rgb8(0x48, 0x7B, 0xD1),
            success:       Color::from_rgb8(0x4F, 0xB4, 0x77),
            warning:       Color::from_rgb8(0xE2, 0xA9, 0x3C),
            danger:        Color::from_rgb8(0xE5, 0x61, 0x5F),
            info:          Color::from_rgb8(0x5E, 0x9B, 0xEA),
            window_card:   None,
        })
    }

    /// A clean, faintly-warm neutral **light** theme — a paper-white ground
    /// (not a clinical pure white), dark warm-neutral typography in three
    /// tiers, and the same restrained blue accent as the dark default.
    pub fn light() -> Self {
        Self::themed(PaletteSpec {
            dark: false,
            base:          Color::from_rgb8(0xF3, 0xF3, 0xF1), // soft paper
            raised:        Color::from_rgb8(0xFF, 0xFF, 0xFF), // crisp card
            popover:       Color::from_rgb8(0xFF, 0xFF, 0xFF),
            text:          Color::from_rgb8(0x1B, 0x1C, 0x1E),
            text_dim:      Color::from_rgb8(0x5B, 0x5D, 0x63),
            text_tertiary: Color::from_rgb8(0x8A, 0x8C, 0x92),
            border:        Color::from_rgb8(0xDE, 0xDE, 0xDB),
            focus:         Color::from_rgb8(0x2F, 0x6F, 0xD6),
            accent:        Color::from_rgb8(0x2F, 0x6F, 0xD6),
            accent_hover:  Color::from_rgb8(0x25, 0x5C, 0xB8),
            success:       Color::from_rgb8(0x1E, 0x9E, 0x5A),
            warning:       Color::from_rgb8(0xB5, 0x7A, 0x00),
            danger:        Color::from_rgb8(0xCB, 0x3B, 0x3B),
            info:          Color::from_rgb8(0x1E, 0x74, 0xC4),
            window_card:   None,
        })
    }

    /// The original qOS look — the slate-and-indigo "nirios concept" palette,
    /// preserved verbatim as a selectable preset so nothing from the first
    /// design is lost when the neutral graphite default takes over. Every
    /// value here is Tailwind ground truth; see the tests for the mapping.
    pub fn qos_blue() -> Self {
        // Slate-950 → slate-50, one Tailwind step per ramp slot.
        Self {
            neutral: ColorRamp { stops: [
                Color::from_rgb8(  2,   6,  23), // slate-950
                Color::from_rgb8( 15,  23,  42), // slate-900
                Color::from_rgb8( 30,  41,  59), // slate-800
                Color::from_rgb8( 51,  65,  85), // slate-700
                Color::from_rgb8( 71,  85, 105), // slate-600
                Color::from_rgb8(100, 116, 139), // slate-500
                Color::from_rgb8(148, 163, 184), // slate-400
                Color::from_rgb8(203, 213, 225), // slate-300
                Color::from_rgb8(226, 232, 240), // slate-200
                Color::from_rgb8(241, 245, 249), // slate-100
                Color::from_rgb8(248, 250, 252), // slate-50
            ]},
            // Indigo-950 → indigo-50; index 5 is the concept's `indigo-500`
            // accent primary (#6366f1).
            accent: ColorRamp { stops: [
                Color::from_rgb8( 30,  27,  75), // indigo-950
                Color::from_rgb8( 49,  46, 129), // indigo-900
                Color::from_rgb8( 55,  48, 163), // indigo-800
                Color::from_rgb8( 67,  56, 202), // indigo-700
                Color::from_rgb8( 79,  70, 229), // indigo-600
                Color::from_rgb8( 99, 102, 241), // indigo-500 ← accent primary
                Color::from_rgb8(129, 140, 248), // indigo-400
                Color::from_rgb8(165, 180, 252), // indigo-300
                Color::from_rgb8(199, 210, 254), // indigo-200
                Color::from_rgb8(224, 231, 255), // indigo-100
                Color::from_rgb8(238, 242, 255), // indigo-50
            ]},
            semantic: SemanticColors {
                success: Color::from_rgb8( 16, 185, 129), // emerald-500
                warning: Color::from_rgb8(245, 158,  11), // amber-500
                danger:  Color::from_rgb8(239,  68,  68), // red-500
                info:    Color::from_rgb8( 96, 165, 250), // blue-400
            },
            surfaces: Surfaces {
                base:   Color::from_rgb8(15, 23, 42),       // slate-900
                raised: Color::from_rgb8(30, 41, 59),       // slate-800
                overlay: Color::from_rgba8(0, 0, 0, 255),   // opaque deep menu surface
                scrim: Color::from_rgb8(2, 6, 23),          // slate-950
                glass:   Color::from_rgba8(15, 23, 42, 204),// slate-900/80
                glass_blur_px: 24.0,
                window_card: Color::WHITE,
                window_card_border: Color::from_rgba8(255, 255, 255, 51), // white/20
            },
            text_primary: Color::WHITE,
            text_dim: Color::from_rgb8(148, 163, 184), // slate-400
            text_tertiary: Color::from_rgb8(100, 116, 139), // slate-500
            border_standard: Color::from_rgb8(51, 65, 85), // slate-700
            border_focus: Color::from_rgb8(99, 102, 241),  // indigo-500
            // A dark palette, so the veneers lighten.
            overlay_subtle:        Color::from_rgba8(255, 255, 255, 13),
            overlay_medium:        Color::from_rgba8(255, 255, 255, 26),
            overlay_strong:        Color::from_rgba8(255, 255, 255, 51),
            overlay_border_subtle: Color::from_rgba8(255, 255, 255, 26),
            overlay_border_strong: Color::from_rgba8(255, 255, 255, 51),
            surface_white_5:  Color::from_rgba8(255, 255, 255, 13),
            surface_white_10: Color::from_rgba8(255, 255, 255, 26),
            surface_white_20: Color::from_rgba8(255, 255, 255, 51),
            border_white_10:  Color::from_rgba8(255, 255, 255, 26),
            border_white_20:  Color::from_rgba8(255, 255, 255, 51),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Two kinds of pin here:
    //!
    //! * The **new graphite default** ([`Colors::dark`]) — pin its identity
    //!   surfaces / text / accent so a drift away from the neutral,
    //!   restrained-blue design becomes a visible failure, and assert its
    //!   text tiers clear WCAG AA against the ground.
    //! * The **preserved concept palette** ([`Colors::qos_blue`]) — pin it to
    //!   Tailwind ground truth (slate + indigo) so the original look is kept
    //!   byte-for-byte as a selectable preset.
    use super::*;
    use crate::a11y::contrast_ratio;
    use crate::color_compat::ColorCompat;

    fn rgb(c: Color) -> [u8; 3] { [c.r(), c.g(), c.b()] }
    fn rgba(c: Color) -> [u8; 4] { [c.r(), c.g(), c.b(), c.a()] }

    // --- the new graphite default -----------------------------------------

    #[test]
    fn dark_default_is_neutral_graphite_not_slate_blue() {
        let c = Colors::dark();
        assert_eq!(rgb(c.surfaces.base),   [0x16, 0x17, 0x1B], "graphite ground");
        assert_eq!(rgb(c.surfaces.raised), [0x1F, 0x20, 0x25], "raised card");
        // The ground is near-neutral: the blue channel leads red by only a
        // hair (a faint cool bias), nothing like slate-900's 2.8× blue lead.
        let b = c.surfaces.base;
        assert!(
            (b.b() as i32 - b.r() as i32) <= 6,
            "the default ground must be a near-neutral graphite, not blue-drenched",
        );
    }

    #[test]
    fn dark_default_accent_is_a_restrained_blue() {
        let c = Colors::dark();
        assert_eq!(rgb(c.accent[ACCENT_PRIMARY_IDX]), [0x58, 0x91, 0xE8], "restrained blue accent");
        assert_eq!(rgb(c.accent[ACCENT_HOVER_IDX]),   [0x48, 0x7B, 0xD1], "hover one step darker");
        // The accent is not used as a surface: the ground stays graphite.
        assert_ne!(rgb(c.surfaces.base), rgb(c.accent[ACCENT_PRIMARY_IDX]));
    }

    #[test]
    fn dark_default_has_three_legible_text_tiers() {
        let c = Colors::dark();
        let base = c.surfaces.base;
        let primary = contrast_ratio(c.text_primary, base);
        let dim = contrast_ratio(c.text_dim, base);
        let tertiary = contrast_ratio(c.text_tertiary, base);

        // A real hierarchy: each tier is quieter than the last.
        assert!(primary > dim, "primary ({primary:.2}) must out-contrast secondary ({dim:.2})");
        assert!(dim > tertiary, "secondary ({dim:.2}) must out-contrast tertiary ({tertiary:.2})");

        // …yet all three stay legible. Primary + secondary clear AA for body
        // text (4.5:1); tertiary clears AA for large / UI text (3:1) so it
        // reads as "quiet", never "broken".
        assert!(primary >= 4.5, "primary text clears AA (got {primary:.2})");
        assert!(dim >= 4.5, "secondary text clears AA (got {dim:.2})");
        assert!(tertiary >= 3.0, "tertiary text clears AA-large (got {tertiary:.2})");
    }

    #[test]
    fn dark_default_body_text_clears_aa_on_raised_surfaces_too() {
        // Cards and popovers are the other surfaces body text lands on.
        let c = Colors::dark();
        assert!(contrast_ratio(c.text_primary, c.surfaces.raised) >= 4.5, "primary on raised");
        assert!(contrast_ratio(c.text_dim, c.surfaces.raised) >= 4.5, "secondary on raised");
    }

    #[test]
    fn dark_default_popover_is_a_rich_near_black() {
        // The context-menu / popover surface is the deep tier the design
        // leans on. It must be darker than the ground.
        let c = Colors::dark();
        let base = c.surfaces.base;
        let over = c.surfaces.overlay;
        let lum = |x: Color| 0.2126 * x.r() as f32 + 0.7152 * x.g() as f32 + 0.0722 * x.b() as f32;
        assert!(lum(over) < lum(base), "popover surface is deeper than the ground");
    }

    #[test]
    fn overlay_surface_is_fully_opaque_in_every_theme() {
        // Context menus, popovers, dropdowns and toasts paint on the overlay
        // tier; it must be solid so page content never bleeds through.
        for (name, c) in [
            ("dark", Colors::dark()),
            ("light", Colors::light()),
            ("qos_blue", Colors::qos_blue()),
        ] {
            assert_eq!(c.surfaces.overlay.a(), 255, "{name} overlay must be opaque");
        }
    }

    // --- the preserved concept palette (qOS Blue) -------------------------

    #[test]
    fn qos_blue_neutral_ramp_is_tailwind_slate_950_through_50() {
        let n = &Colors::qos_blue().neutral;
        assert_eq!(rgb(n[0]),  [  2,   6,  23], "slate-950 #020617");
        assert_eq!(rgb(n[1]),  [ 15,  23,  42], "slate-900 #0f172a");
        assert_eq!(rgb(n[2]),  [ 30,  41,  59], "slate-800 #1e293b");
        assert_eq!(rgb(n[3]),  [ 51,  65,  85], "slate-700 #334155");
        assert_eq!(rgb(n[4]),  [ 71,  85, 105], "slate-600 #475569");
        assert_eq!(rgb(n[5]),  [100, 116, 139], "slate-500 #64748b");
        assert_eq!(rgb(n[6]),  [148, 163, 184], "slate-400 #94a3b8");
        assert_eq!(rgb(n[7]),  [203, 213, 225], "slate-300 #cbd5e1");
        assert_eq!(rgb(n[8]),  [226, 232, 240], "slate-200 #e2e8f0");
        assert_eq!(rgb(n[9]),  [241, 245, 249], "slate-100 #f1f5f9");
        assert_eq!(rgb(n[10]), [248, 250, 252], "slate-50  #f8fafc");
    }

    #[test]
    fn qos_blue_accent_ramp_is_tailwind_indigo_950_through_50() {
        let a = &Colors::qos_blue().accent;
        assert_eq!(rgb(a[0]),  [ 30,  27,  75], "indigo-950 #1e1b4b");
        assert_eq!(rgb(a[1]),  [ 49,  46, 129], "indigo-900 #312e81");
        assert_eq!(rgb(a[2]),  [ 55,  48, 163], "indigo-800 #3730a3");
        assert_eq!(rgb(a[3]),  [ 67,  56, 202], "indigo-700 #4338ca");
        assert_eq!(rgb(a[4]),  [ 79,  70, 229], "indigo-600 #4f46e5");
        assert_eq!(rgb(a[5]),  [ 99, 102, 241], "indigo-500 #6366f1");
        assert_eq!(rgb(a[6]),  [129, 140, 248], "indigo-400 #818cf8");
        assert_eq!(rgb(a[7]),  [165, 180, 252], "indigo-300 #a5b4fc");
        assert_eq!(rgb(a[8]),  [199, 210, 254], "indigo-200 #c7d2fe");
        assert_eq!(rgb(a[9]),  [224, 231, 255], "indigo-100 #e0e7ff");
        assert_eq!(rgb(a[10]), [238, 242, 255], "indigo-50  #eef2ff");
    }

    #[test]
    fn qos_blue_semantic_colors_match_tailwind() {
        let s = &Colors::qos_blue().semantic;
        assert_eq!(rgb(s.success), [ 16, 185, 129], "emerald-500 #10b981");
        assert_eq!(rgb(s.warning), [245, 158,  11], "amber-500   #f59e0b");
        assert_eq!(rgb(s.danger),  [239,  68,  68], "red-500     #ef4444");
        assert_eq!(rgb(s.info),    [ 96, 165, 250], "blue-400    #60a5fa");
    }

    #[test]
    fn qos_blue_surfaces_match_concept_layered_palette() {
        let s = &Colors::qos_blue().surfaces;
        assert_eq!(rgb(s.base),   [ 15,  23,  42], "slate-900 #0f172a");
        assert_eq!(rgb(s.raised), [ 30,  41,  59], "slate-800 #1e293b");
        // Menus/popovers read as solid: the overlay tier is fully opaque in
        // every theme (no page content bleeding through).
        assert_eq!(rgba(s.overlay), [0, 0, 0, 255], "opaque deep menu surface");
        assert_eq!(rgba(s.glass), [ 15,  23,  42, 204], "slate-900/80");
        assert_eq!(rgba(s.window_card), [255, 255, 255, 255], "white");
        assert_eq!(rgba(s.window_card_border), [255, 255, 255, 51], "white/20");
    }

    #[test]
    fn qos_blue_text_and_border_tokens_match_concept() {
        let c = Colors::qos_blue();
        assert_eq!(rgba(c.text_primary), [255, 255, 255, 255], "white");
        assert_eq!(rgb(c.text_dim), [148, 163, 184], "slate-400 #94a3b8");
        assert_eq!(rgb(c.border_standard), [ 51,  65,  85], "slate-700 #334155");
        assert_eq!(rgb(c.border_focus), [ 99, 102, 241], "indigo-500 #6366f1");
    }

    // --- structural invariants shared by every themed palette --------------

    #[test]
    fn themed_pins_the_accent_and_hover_stops_exactly() {
        // The eased ramp must not drift the two stops widgets actually read.
        let c = Colors::dark();
        assert_eq!(rgb(c.accent[ACCENT_PRIMARY_IDX]), [0x58, 0x91, 0xE8]);
        assert_eq!(rgb(c.accent[ACCENT_HOVER_IDX]),   [0x48, 0x7B, 0xD1]);
    }

    #[test]
    fn translucent_white_tokens_match_alpha_bytes() {
        let c = Colors::dark();
        assert_eq!(rgba(c.surface_white_5),  [255, 255, 255, 13], "white/5");
        assert_eq!(rgba(c.surface_white_10), [255, 255, 255, 26], "white/10");
        assert_eq!(rgba(c.surface_white_20), [255, 255, 255, 51], "white/20");
        assert_eq!(rgba(c.border_white_10),  [255, 255, 255, 26], "border-white/10");
        assert_eq!(rgba(c.border_white_20),  [255, 255, 255, 51], "border-white/20");
    }

    // --- state veneers ----------------------------------------------------

    /// Straight-alpha composite of `fg` over an opaque `bg` — what the eye
    /// actually receives when a translucent veneer is painted on a surface.
    fn composite(fg: Color, bg: Color) -> Color {
        let a = fg.a() as f32 / 255.0;
        let ch = |f: u8, b: u8| ((f as f32) * a + (b as f32) * (1.0 - a)).round() as u8;
        Color::from_rgb8(ch(fg.r(), bg.r()), ch(fg.g(), bg.g()), ch(fg.b(), bg.b()))
    }

    /// CIE L* (0..=100) — perceptual lightness, so veneer strength can be
    /// compared across grounds of very different luminance.
    fn lstar(c: Color) -> f64 {
        let y = crate::a11y::relative_luminance(c);
        if y > 0.008856 { 116.0 * y.cbrt() - 16.0 } else { 903.3 * y }
    }

    /// The alias fields must never drift from the semantic ones they name —
    /// the fleet reads both, and a widget picking the stale pair would paint a
    /// veneer of the wrong polarity.
    #[test]
    fn overlay_aliases_track_the_semantic_tokens_in_every_theme() {
        for name in crate::theme::Theme::ALL {
            let c = &crate::theme::Theme::named(name).unwrap().tokens.colors;
            assert_eq!(rgba(c.surface_white_5), rgba(c.overlay_subtle), "{name} subtle");
            assert_eq!(rgba(c.surface_white_10), rgba(c.overlay_medium), "{name} medium");
            assert_eq!(rgba(c.surface_white_20), rgba(c.overlay_strong), "{name} strong");
            assert_eq!(rgba(c.border_white_10), rgba(c.overlay_border_subtle), "{name} border subtle");
            assert_eq!(rgba(c.border_white_20), rgba(c.overlay_border_strong), "{name} border strong");
        }
    }

    /// The defect this token model exists to prevent: a veneer that does not
    /// move the surface it is painted on. Light themes used to inherit white
    /// veneers, so `surface_white_5` over the Light theme's `#FFFFFF` card was
    /// a literal no-op (1.000:1) and hover feedback did not render at all.
    ///
    /// Checked on both grounds a veneer realistically lands on, in every
    /// theme, so no polarity can regress unnoticed.
    #[test]
    fn state_veneers_move_every_ground_in_every_theme() {
        for name in crate::theme::Theme::ALL {
            let c = &crate::theme::Theme::named(name).unwrap().tokens.colors;
            for (ground_name, ground) in
                [("base", c.surfaces.base), ("raised", c.surfaces.raised)]
            {
                for (tier_name, tier, floor) in [
                    ("subtle", c.overlay_subtle, 1.07),
                    ("medium", c.overlay_medium, 1.17),
                    ("strong", c.overlay_strong, 1.45),
                    ("border_subtle", c.overlay_border_subtle, 1.17),
                    ("border_strong", c.overlay_border_strong, 1.45),
                ] {
                    let seen = composite(tier, ground);
                    let step = contrast_ratio(seen, ground);
                    assert!(
                        step >= floor,
                        "{name}: {tier_name} veneer over {ground_name} is invisible \
                         (step {step:.3}, need {floor}) — hover/press feedback would not render",
                    );
                }
            }
        }
    }

    /// A veneer has to push the ground *away* from itself: lighten a dark
    /// surface, darken a light one. Asserted on lightness rather than on the
    /// literal colour so a future theme is free to tint its veneers.
    #[test]
    fn veneer_polarity_follows_the_ground() {
        for name in crate::theme::Theme::ALL {
            let c = &crate::theme::Theme::named(name).unwrap().tokens.colors;
            let ground = c.surfaces.raised;
            let ground_is_dark = lstar(ground) < 50.0;
            for (tier_name, tier) in [
                ("subtle", c.overlay_subtle),
                ("medium", c.overlay_medium),
                ("strong", c.overlay_strong),
            ] {
                let delta = lstar(composite(tier, ground)) - lstar(ground);
                if ground_is_dark {
                    assert!(delta > 0.0, "{name}: {tier_name} must lighten a dark ground (got {delta:.2})");
                } else {
                    assert!(delta < 0.0, "{name}: {tier_name} must darken a light ground (got {delta:.2})");
                }
            }
        }
    }

    /// Equal alpha does not buy equal visibility on grounds at opposite ends
    /// of the lightness curve, so the light tiers are solved for the *step*
    /// the dark tiers produce, not mirrored from their alpha. This pins that
    /// solve: every light theme's veneer lands within 2 L* of the dark target.
    #[test]
    fn light_overlays_match_the_dark_perceptual_step() {
        let targets = [("subtle", 5.0), ("medium", 10.5), ("strong", 21.0)];
        for name in crate::theme::Theme::ALL {
            let c = &crate::theme::Theme::named(name).unwrap().tokens.colors;
            if lstar(c.surfaces.raised) < 50.0 {
                continue; // dark ground — the reference, not the thing solved for
            }
            for (ground_name, ground) in
                [("base", c.surfaces.base), ("raised", c.surfaces.raised)]
            {
                let tiers = [c.overlay_subtle, c.overlay_medium, c.overlay_strong];
                for ((tier_name, want), tier) in targets.iter().zip(tiers) {
                    let got = lstar(ground) - lstar(composite(tier, ground));
                    assert!(
                        (got - want).abs() <= 2.0,
                        "{name}/{ground_name}: {tier_name} step is {got:.2} L*, \
                         dark themes get {want:.1} — the tiers are out of step",
                    );
                }
            }
        }
    }

    /// The high-contrast preset re-solves its own tiers: it overrides its
    /// surfaces to pure black *after* inheriting the dark tokens, and against
    /// black the dark tiers collapse to a 1.08:1 step — invisible to exactly
    /// the users this preset exists for. It must be markedly stronger than the
    /// default dark theme's, while still hosting the theme's white text at AA.
    #[test]
    fn high_contrast_veneers_are_stronger_and_still_host_white_text() {
        let hc = crate::theme::Theme::high_contrast();
        let c = &hc.tokens.colors;
        let dark = Colors::dark();
        let ground = c.surfaces.raised;

        for (tier_name, tier, dark_tier) in [
            ("subtle", c.overlay_subtle, dark.overlay_subtle),
            ("medium", c.overlay_medium, dark.overlay_medium),
            ("strong", c.overlay_strong, dark.overlay_strong),
        ] {
            let seen = composite(tier, ground);
            let step = contrast_ratio(seen, ground);
            let inherited = contrast_ratio(composite(dark_tier, ground), ground);
            assert!(
                step > inherited * 1.4,
                "{tier_name}: high contrast must not inherit the dark tier's step \
                 ({step:.2} vs {inherited:.2})",
            );
            let on_top = contrast_ratio(c.text_primary, seen);
            assert!(
                on_top >= 4.5,
                "{tier_name}: primary text must stay AA on the veneer (got {on_top:.2})",
            );
        }
    }

    // --- readable_on ------------------------------------------------------

    #[test]
    fn readable_on_keeps_a_preferred_colour_that_already_passes() {
        let c = Colors::dark();
        // Primary text on the card is 13.89:1 — nothing to correct.
        let got = c.readable_on(c.surfaces.raised, c.text_primary);
        assert_eq!(rgba(got), rgba(c.text_primary));
    }

    #[test]
    fn readable_on_lifts_a_preferred_colour_that_fails() {
        // The exact pairing the audit measured: the `neutral[3]` fill tier
        // under a `text_dim` label. It clears AA in the dark default (4.69:1)
        // and fails in light (4.26:1) — one theme's pass is what kept it in.
        let light = Colors::light();
        let fill = light.neutral[3];
        let before = contrast_ratio(light.text_dim, fill);
        assert!(before < 4.5, "precondition: the light pairing fails AA (got {before:.2})");

        let after = contrast_ratio(light.readable_on(fill, light.text_dim), fill);
        assert!(after >= 4.5, "the lifted label must clear AA (got {after:.2})");
        assert!(after > before, "and must actually be an improvement");

        // Dark already passed, so it is left exactly as designed — the fix
        // must not repaint a theme that was already correct.
        let dark = Colors::dark();
        assert_eq!(
            rgba(dark.readable_on(dark.neutral[3], dark.text_dim)),
            rgba(dark.text_dim),
            "dark's 4.69:1 pairing is untouched",
        );
    }

    /// The fallback arm cannot fail: white and black are 21:1 apart, so at
    /// least one of them clears AA against any colour whatsoever.
    #[test]
    fn readable_on_always_returns_an_aa_pairing() {
        let c = Colors::light();
        // A mid grey defeats text_dim *and* text_primary in some themes; walk
        // the whole grey axis plus every theme's accent to be sure.
        let mut probes: Vec<Color> = (0..=255)
            .step_by(5)
            .map(|v| Color::from_rgb8(v, v, v))
            .collect();
        for name in crate::theme::Theme::ALL {
            let t = crate::theme::Theme::named(name).unwrap();
            probes.push(t.accent_primary);
            probes.push(t.tokens.colors.surfaces.raised);
        }
        for bg in probes {
            let fg = c.readable_on(bg, c.text_dim);
            let r = contrast_ratio(fg, bg);
            assert!(r >= 4.5, "readable_on returned a {r:.2}:1 pairing on {:?}", rgb(bg));
        }
    }


    // --- legible_fill -----------------------------------------------------

    /// A filled chip must host its label at AA in every theme. White on the
    /// raw accent fails in 11 of the 14 — 1.69:1 on Rosé Pine, 1.70:1 on
    /// Gruvbox, 2.00:1 on Nord, and 3.17:1 on the shipped dark default.
    #[test]
    fn legible_fill_clears_aa_on_every_accent_and_danger() {
        for name in crate::theme::Theme::ALL {
            let t = crate::theme::Theme::named(name).unwrap();
            let c = &t.tokens.colors;
            for (kind, raw) in [
                ("accent", t.accent_primary),
                ("accent_hover", t.accent_hover),
                ("danger", c.semantic.danger),
            ] {
                let (fill, label) = c.legible_fill(raw, Color::WHITE);
                let r = contrast_ratio(label, fill);
                assert!(r >= 4.5, "{name}/{kind}: chip label is {r:.2}:1, below AA");
            }
        }
    }

    /// Deepening is the preferred repair because it keeps the chip reading as
    /// the same component. Assert it is actually what happens for a mid-tone
    /// accent, and that the deepened fill stays recognisably the theme's hue.
    #[test]
    fn legible_fill_deepens_rather_than_repainting_the_label() {
        let t = crate::theme::Theme::dark();
        let c = &t.tokens.colors;
        let raw = t.accent_primary;
        let (fill, label) = c.legible_fill(raw, Color::WHITE);

        assert_eq!(rgba(label), rgba(Color::WHITE), "the white label is kept");
        assert_ne!(rgba(fill), rgba(raw), "the fill is deepened to earn it");
        assert!(
            crate::a11y::relative_luminance(fill) < crate::a11y::relative_luminance(raw),
            "deepening means darker",
        );
        assert!(fill.b() > fill.g() && fill.g() > fill.r(), "still the theme's blue");
    }

    /// A pale accent cannot be deepened into a white-label chip without going
    /// muddy, so those keep their fill and take a dark label instead. Gruvbox's
    /// yellow is the canonical case: 41 % deepening would be needed, past the
    /// cap, and a black-on-yellow chip is the idiomatic look anyway.
    #[test]
    fn legible_fill_keeps_pale_accents_and_repaints_their_label() {
        let t = crate::theme::Theme::named("Gruvbox").unwrap();
        let c = &t.tokens.colors;
        let raw = t.accent_primary;
        let (fill, label) = c.legible_fill(raw, Color::WHITE);

        assert_eq!(rgba(fill), rgba(raw), "the yellow is left alone");
        assert_ne!(rgba(label), rgba(Color::WHITE), "the label is what moves");
        assert!(contrast_ratio(label, fill) >= 4.5);
    }

    /// Where white already clears AA the pair is returned untouched, so a
    /// theme designed correctly is not repainted.
    #[test]
    fn legible_fill_is_inert_where_the_pairing_already_passes() {
        let t = crate::theme::Theme::light();
        let c = &t.tokens.colors;
        let raw = t.accent_primary;
        assert!(contrast_ratio(Color::WHITE, raw) >= 4.5, "precondition");
        let (fill, label) = c.legible_fill(raw, Color::WHITE);
        assert_eq!(rgba(fill), rgba(raw));
        assert_eq!(rgba(label), rgba(Color::WHITE));
    }

    // --- the permission-denied scrim --------------------------------------

    /// The scrim is one const shared by every theme, which looks like the
    /// defect the overlay tiers had — and measured against a widget's
    /// *background* it reads that way: black over the High Contrast preset's
    /// pure-black card is a literal 1.00:1.
    ///
    /// That is the wrong surface. The scrim is painted over the widget after
    /// it has drawn itself, so it lands on the border, label and knob too, and
    /// those are what carry "you cannot act on this". Measured there it is
    /// strongest exactly where the background reading said it was absent: High
    /// Contrast drops white content from 21.00:1 to 9.90:1.
    ///
    /// So this pins the property that actually matters — a denied control's
    /// content loses contrast against its own body in every theme — rather
    /// than a polarity the scrim does not need. Making it directional would
    /// invert the meaning on dark themes, where a lightening scrim would leave
    /// a disabled control brighter than an enabled one.
    #[test]
    fn the_denied_scrim_mutes_a_control_in_every_theme() {
        for name in crate::theme::Theme::ALL {
            let t = crate::theme::Theme::named(name).unwrap();
            let c = &t.tokens.colors;
            // The two bodies a control realistically has: a neutral card under
            // primary text, and an accent fill under a white label.
            // The accent case must be measured on the pair a widget actually
            // paints — `legible_fill` may have deepened the fill and/or
            // re-picked the label — not on the raw accent.
            let (accent_fill, accent_label) = c.legible_fill(t.accent_primary, Color::WHITE);
            for (kind, body, content) in [
                ("neutral", c.surfaces.raised, c.text_primary),
                ("accent", accent_fill, accent_label),
            ] {
                let before = contrast_ratio(content, body);
                let after = contrast_ratio(
                    composite(PERMISSION_DENIED_SCRIM, content),
                    composite(PERMISSION_DENIED_SCRIM, body),
                );
                assert!(
                    after < before / 1.08,
                    "{name}/{kind}: the scrim must cost the content contrast \
                     ({before:.2} -> {after:.2})",
                );
                // …and the control must visibly move, not merely lose ratio.
                // Either half can carry that: a light label darkens under the
                // scrim, while a chip that took a dark label instead keeps its
                // label put and shifts its fill. Requiring the content
                // specifically would be asserting one repair strategy.
                let moved = |x: Color| {
                    (lstar(x) - lstar(composite(PERMISSION_DENIED_SCRIM, x))).abs()
                };
                let shift = moved(content).max(moved(body));
                assert!(
                    shift >= 3.0,
                    "{name}/{kind}: the denied control must visibly shift (got {shift:.1} L*)",
                );
            }
        }
    }

    /// Accent-filled controls are the weakest case by construction: the scrim
    /// darkens fill and label together, so their ratio is largely preserved.
    /// Pinned so that if it ever degrades to "no signal at all" a test says so
    /// — the shots show it still reads today, but only just.
    #[test]
    fn the_denied_scrim_is_weakest_on_accent_fills() {
        let mut worst = f64::INFINITY;
        for name in crate::theme::Theme::ALL {
            let t = crate::theme::Theme::named(name).unwrap();
            let (fill, label) = t.tokens.colors.legible_fill(t.accent_primary, Color::WHITE);
            let before = contrast_ratio(label, fill);
            let after = contrast_ratio(
                composite(PERMISSION_DENIED_SCRIM, label),
                composite(PERMISSION_DENIED_SCRIM, fill),
            );
            worst = worst.min(before / after);
        }
        assert!(worst > 1.08, "an accent chip must still mute (worst drop x{worst:.2})");
    }
}

