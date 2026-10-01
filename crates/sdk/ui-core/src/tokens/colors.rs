use crate::color::{hsl_to_rgb, rgb_to_hsl};
use crate::color_compat::ColorCompat;
use crate::paint::with_alpha;
use vello::peniko::Color;

/// Index into the accent ramp used as the dominant fill (active dot, slider
/// track, toggle on-state).
pub const ACCENT_PRIMARY_IDX: usize = 5;

/// One step darker than [`ACCENT_PRIMARY_IDX`].
pub const ACCENT_HOVER_IDX: usize = 4;

/// How far [`Colors::legible_fill`] may deepen a chip's fill toward black
/// before it gives up and re-picks the label instead. At 35 % a mid-tone
/// accent still reads as itself; past it the pale accents (Nord's frost,
/// Gruvbox's yellow, Rosé Pine's rose) go muddy, and those are exactly the
/// ones a dark label suits anyway.
pub const MAX_CHIP_DEEPEN: f32 = 0.35;

pub const PERMISSION_DENIED_SCRIM: Color = Color::from_rgba8(0, 0, 0, 77);

/// Straight (non-premultiplied) 8-bit sRGB mix — the space the Themes app
/// and most widget draw paths blend in.
pub(crate) fn mix8(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a weighted average of two u8 (0..=255) channel values with non-negative weights summing to 1.0 always rounds to a value already within 0..=255"
    )]
    let l = |x: u8, y: u8| (f32::from(x) * (1.0 - t) + f32::from(y) * t).round() as u8;
    Color::from_rgb8(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// The accent ramp grown from one accent, with the primary and hover stops
/// pinned so [`ACCENT_PRIMARY_IDX`] / [`ACCENT_HOVER_IDX`] land exactly on the
/// chosen colours.
fn accent_ramp(accent: Color, hover: Color) -> ColorRamp {
    let mut ramp = ColorRamp::eased(
        mix8(accent, Color::from_rgb8(0, 0, 0), 0.62),
        mix8(accent, Color::WHITE, 0.85),
    );
    ramp.stops[ACCENT_HOVER_IDX] = hover;
    ramp.stops[ACCENT_PRIMARY_IDX] = accent;
    ramp
}

/// Composites `over` onto an opaque `ground`: a token's own RGBA says
/// nothing about how it reads (`white/20` differs over black vs white), so
/// every derivation below measures the composite, never the token.
pub(crate) fn composite(over: Color, ground: Color) -> Color {
    let a = f32::from(over.a()) / 255.0;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a weighted average of two u8 (0..=255) channel values with non-negative weights summing to 1.0 always rounds to a value already within 0..=255"
    )]
    let ch = |o: u8, g: u8| (f32::from(o) * a + f32::from(g) * (1.0 - a)).round() as u8;
    Color::from_rgb8(
        ch(over.r(), ground.r()),
        ch(over.g(), ground.g()),
        ch(over.b(), ground.b()),
    )
}

/// Minimum ground movement (WCAG relative-luminance units) for a hairline to
/// count as visible — deliberately far below any text threshold, since it
/// only has to catch a wash landing within rounding distance of its surface.
pub const HAIRLINE_MIN_DELTA: f64 = 0.02;

/// Fallback wash alpha for [`Colors::lift`] when a preset has no raised tier
/// to aim at. 15% — reads as a distinct body without blowing out contrast.
const LIFT_FALLBACK_ALPHA: u8 = 38;

/// How far [`Colors::glass_film`] carries the raised tier past itself, so a
/// translucent film (which gives back strength through its own alpha) still
/// lands above an opaque card instead of under it.
const GLASS_FILM_LIFT: u8 = 46;

#[derive(Clone, Debug)]
pub struct ColorRamp {
    pub stops: [Color; 11],
}

impl ColorRamp {
    pub fn len(&self) -> usize {
        self.stops.len()
    }
    pub fn is_empty(&self) -> bool {
        self.stops.is_empty()
    }

    /// Builds an 11-stop ramp from `a` (index 0) to `b` (index 10) eased so
    /// low indices cluster near `a` (`powf(1.5)`) — keeps `neutral[2..5]`
    /// (hover/pressed/muted tints) close to the base instead of a plain lerp
    /// racing them toward the light end.
    pub fn eased(a: Color, b: Color) -> Self {
        let mut stops = [a; 11];
        for (i, stop) in stops.iter_mut().enumerate() {
            #[allow(
                clippy::cast_precision_loss,
                reason = "i is a 0..11 index into a fixed 11-element array, far below \
                          where f32 loses integer precision"
            )]
            let t = (i as f32 / 10.0).powf(1.5);
            *stop = mix8(a, b, t);
        }
        Self { stops }
    }
}

impl std::ops::Index<usize> for ColorRamp {
    type Output = Color;
    #[allow(
        clippy::indexing_slicing,
        reason = "Index is documented to panic on out-of-bounds, matching std's own \
                  slice/Vec Index impl -- that is this trait's contract"
    )]
    fn index(&self, i: usize) -> &Color {
        &self.stops[i]
    }
}

#[derive(Clone, Debug)]
pub struct Surfaces {
    pub base: Color,
    pub raised: Color,
    pub overlay: Color,
    /// Opaque dim behind a modal: strength belongs to the thing being
    /// lifted, hue belongs to the theme, so the scrim recedes into the
    /// chrome's colour family rather than punching a black hole through it.
    pub scrim: Color,
    pub glass: Color,
    pub glass_blur_px: f32,
    /// The card an empty window shows through. Stays in the theme's own
    /// surface family (not bleached to paper-white) so it matches the
    /// window beside it once an app fills its own background — a document
    /// surface wanting a paper look draws that itself.
    pub window_card: Color,
    pub window_card_border: Color,
}

/// Ink tiers for content painted directly onto [`Surfaces::window_card`].
/// Not answerable from the chrome's palette alone — the card lands on both
/// sides of the light/dark line across presets — so it's derived here, once,
/// from the card's actual luminance.
#[derive(Clone, Copy, Debug)]
pub struct OnCard {
    /// Body / title ink. Clears WCAG AA (4.5:1) against the card.
    pub ink: Color,
    pub ink_dim: Color,
    pub ink_tertiary: Color,
    /// Whisper-tinted surface *on* the card (gutter strip, zebra row):
    /// stepped toward the card's own ink so it works either direction.
    pub tint: Color,
}

/// WCAG 2.1 AA for body text.
const ON_CARD_MIN_CONTRAST: f64 = 4.5;

/// Floor for text at the bottom of the type scale. A real anti-aliased
/// glyph at 12px and below doesn't reach WCAG's ideal-glyph 4.5:1 — a label
/// measured at 4.56:1 landed at 3.44:1 on screen. AAA leaves headroom to
/// still clear AA once rendered.
pub const ON_CARD_SMALL_TEXT_CONTRAST: f64 = 7.0;

const ON_CARD_TINT_T: f32 = 0.07;

/// WCAG 2.1 non-text contrast (a pill's edge, a toggle's knob).
pub const NON_TEXT_MIN_CONTRAST: f64 = 3.0;

/// Minimum contrast a control's *label* must clear against the control's own
/// fill (not the surface behind it) — an accent bright enough to read at a
/// glance is one white text can disappear into.
pub const ON_FILL_MIN_CONTRAST: f64 = 4.5;

const ACCENT_CHIP_WASH: f32 = 0.18;

/// The opaque colour an opaque `wash` painted at `alpha` over `ground`
/// leaves behind — contrast is a property of what reaches the screen, so a
/// caller checking legibility on a washed row has to composite first.
pub fn over(ground: Color, wash: Color, alpha: f32) -> Color {
    mix8(ground, wash, alpha)
}

/// An accent-tinted pill, resolved against the ground it paints on. Every
/// field is opaque rather than a token plus alpha — measuring the token
/// instead of the composite is how a chip ends up as accent ink on an
/// accent wash on an accent selection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AccentChip {
    /// Opaque body: the wash already composited over the row's ground.
    pub fill: Color,
    /// Pill edge, clearing [`NON_TEXT_MIN_CONTRAST`] against the **ground**
    /// so the shape stays findable when the wash inside is within a shade
    /// of the ground.
    pub border: Color,
    pub ink: Color,
}

#[derive(Clone, Debug)]
pub struct SemanticColors {
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub info: Color,
}

#[derive(Clone, Debug)]
pub struct Colors {
    pub neutral: ColorRamp,
    pub accent: ColorRamp,
    pub semantic: SemanticColors,
    pub surfaces: Surfaces,
    pub text_primary: Color,
    pub text_dim: Color,
    /// Quietest readable tier; still clears large-text contrast against the
    /// base surface, so it reads as "quiet", never "broken".
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
/// the ground *and* card colour of every light theme Tilo ships — see
/// `overlays_and_state_veneers_track_their_ground_in_every_theme` (phase
/// `light_overlays_match_the_dark_perceptual_step`).
pub fn light_overlays() -> OverlayTiers {
    let k = |a: u8| Color::from_rgba8(0, 0, 0, a);
    (k(15), k(31), k(60), k(31), k(60))
}

/// Colours [`Colors::categorical`] cycles through before repeating. Six
/// keeps neighbours 60° of hue apart — a plain difference at a glance.
pub const CATEGORICAL_LEN: usize = 6;

/// Total hue sweep of the categorical series, centred on the accent: wide
/// enough to tell six workspaces apart, narrow enough to stay one family.
const CATEGORICAL_HUE_ARC_DEG: f32 = 70.0;

/// Lightness spread across the series. Carries most of the separation, and
/// unlike hue it survives a desaturated theme.
const CATEGORICAL_LIGHT_SPREAD: f32 = 0.13;

impl Colors {
    /// A categorical colour series grown from the theme's own accent, for
    /// things that differ rather than rank (workspaces, tags, participants).
    /// Index 0 is the accent; later indices rotate hue while holding its
    /// saturation/lightness, so a restrained theme gets a restrained series.
    /// Hue carries the separation because it survives every ground; walking
    /// lightness instead collapses into "several similar greys".
    #[allow(
        clippy::many_single_char_names,
        reason = "h/s/l/i/k are the standard HSL-rotation variable names used \
                  throughout this file's colour math"
    )]
    pub fn categorical(&self, idx: usize) -> Color {
        let base = self.accent[ACCENT_PRIMARY_IDX];
        let (h, s, l) = rgb_to_hsl(base);
        let i = idx % CATEGORICAL_LEN;
        // Fan outward from the accent (+1, -1, +2, -2, …) rather than a full
        // 360/N rotation, which reaches hues with nothing to do with theme.
        let k = i.div_ceil(2);
        let sign = if i % 2 == 1 { 1.0 } else { -1.0 };
        #[allow(
            clippy::cast_precision_loss,
            reason = "k is at most CATEGORICAL_LEN.div_ceil(2), a tiny constant, far \
                      below where f32 loses integer precision"
        )]
        let step = sign * k as f32 / (CATEGORICAL_LEN / 2) as f32; // -1 … +1
        let hue = (h + step * CATEGORICAL_HUE_ARC_DEG / 2.0).rem_euclid(360.0);
        let light = (l + step * CATEGORICAL_LIGHT_SPREAD).clamp(0.22, 0.78);
        hsl_to_rgb(hue, s, light)
    }
}

/// A compact description of a full palette, expanded into a coherent
/// [`Colors`] token set by [`Colors::themed`] — a preset states only the
/// ~15 colours that carry its identity; ramps, translucent surfaces, scrim
/// and glass are derived consistently for every theme.
#[derive(Clone, Debug)]
pub struct PaletteSpec {
    /// Dark themes ramp their neutrals darkest→lightest; light themes invert.
    pub dark: bool,
    pub base: Color,
    pub raised: Color,
    /// Deep, rich surface for popovers / menus / the top-bar backdrop.
    pub popover: Color,
    pub text: Color,
    pub text_dim: Color,
    pub text_tertiary: Color,
    pub border: Color,
    pub focus: Color,
    pub accent: Color,
    pub accent_hover: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub info: Color,
    /// `None` derives a default (light card on dark themes, white on light).
    pub window_card: Option<Color>,
}

impl Colors {
    /// Expands a [`PaletteSpec`] into a full token set: surfaces, text,
    /// accent and semantic colours pass through verbatim; the neutral ramp,
    /// accent ramp, scrim, glass and translucent-white veneers are derived
    /// so a preset needn't hand-author 40 values.
    pub fn themed(p: &PaletteSpec) -> Self {
        let black = Color::from_rgb8(0, 0, 0);

        // Dark themes run a deep shade → text; light themes run base → text.
        let (n_lo, n_hi) = if p.dark {
            (mix8(p.base, black, 0.45), p.text)
        } else {
            (p.base, p.text)
        };
        let neutral = ColorRamp::eased(n_lo, n_hi);

        let accent = accent_ramp(p.accent, p.accent_hover);

        // Window ground = theme ground, so a window whose app painted
        // nothing matches its neighbour once that app fills `surfaces.base`.
        let window_card = p
            .window_card
            .unwrap_or(if p.dark { p.base } else { Color::WHITE });
        let window_card_border = if p.dark {
            Color::from_rgba8(255, 255, 255, 51)
        } else {
            Color::from_rgba8(0, 0, 0, 30)
        };

        let scrim = if p.dark {
            mix8(p.base, black, 0.55)
        } else {
            Color::from_rgb8(12, 12, 14)
        };

        // A state veneer must move the ground it lands on, so its polarity
        // follows the theme rather than being fixed white.
        let (
            overlay_subtle,
            overlay_medium,
            overlay_strong,
            border_overlay_subtle,
            border_overlay_strong,
        ) = if p.dark {
            dark_overlays()
        } else {
            light_overlays()
        };

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
                // Fully opaque: menus/dropdowns/toasts read as solid with no
                // content bleeding through. `glass` below is the translucent
                // tier instead.
                overlay: p.popover,
                scrim,
                glass: with_alpha(mix8(p.base, p.raised, 0.4), 214),
                glass_blur_px: 24.0,
                window_card,
                window_card_border,
            },
            text_primary: p.text,
            text_dim: p.text_dim,
            text_tertiary: p.text_tertiary,
            border_standard: p.border,
            border_focus: p.focus,
            overlay_subtle,
            overlay_medium,
            overlay_strong,
            overlay_border_subtle: border_overlay_subtle,
            overlay_border_strong: border_overlay_strong,
            // White at every strength failed on light themes (white over
            // `raised` measured 1.01:1 — why filter chips lost their pill),
            // so the aliases carry the same directional tiers.
            surface_white_5: overlay_subtle,
            surface_white_10: overlay_medium,
            surface_white_20: overlay_strong,
            border_white_10: border_overlay_subtle,
            border_white_20: border_overlay_strong,
        }
    }

    /// Re-accents the palette the way [`Self::themed`] accents a preset: the
    /// ramp and the focus ring follow `accent`.
    pub fn set_accent(&mut self, accent: Color, hover: Color) {
        self.accent = accent_ramp(accent, hover);
        self.border_focus = accent;
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

    /// Decided by measurement, not a `dark` flag: when the card is in the
    /// same family as the chrome, the chrome's own three text tiers are
    /// reused; when it's the opposite family (white card under dark
    /// chrome), ink comes from whichever end of the neutral ramp contrasts.
    pub fn on_card(&self) -> OnCard {
        self.on_surface(self.surfaces.window_card)
    }

    /// [`Self::on_card`] for any surface a widget paints on. Measured
    /// against that surface rather than darkened a fixed fraction, which
    /// yields roughly the same ratio whatever the palette — so a theme
    /// meant to raise contrast doesn't.
    pub fn on_surface(&self, surface: Color) -> OnCard {
        self.on_surface_at(surface, ON_CARD_MIN_CONTRAST)
    }

    /// [`Self::on_surface`] against a caller-chosen floor. [`OnCard::ink`]
    /// and `ink_dim` step out until they clear `min_ratio`;
    /// [`OnCard::ink_tertiary`] passes through as authored — how quiet the
    /// quietest tier is belongs to the palette (held to 3:1 on its own
    /// ground by `contrast::FLOORS`), and forcing a body-text floor would
    /// erase the tier.
    pub fn on_surface_at(&self, surface: Color, min_ratio: f64) -> OnCard {
        use crate::a11y::contrast_ratio;

        let chrome_tiers_read = contrast_ratio(self.text_primary, surface) >= min_ratio
            && contrast_ratio(self.text_dim, surface) >= min_ratio;

        let (ink, ink_dim, ink_tertiary) = if chrome_tiers_read {
            (self.text_primary, self.text_dim, self.text_tertiary)
        } else {
            // One step in from the ramp end: pure black/white is harsher
            // than the typography the rest of the UI uses.
            let from_low = contrast_ratio(self.neutral[0], surface)
                >= contrast_ratio(self.neutral[10], surface);
            if from_low {
                (self.neutral[1], self.neutral[4], self.neutral[4])
            } else {
                (self.neutral[9], self.neutral[6], self.neutral[6])
            }
        };

        // The ramp's inner steps aren't guaranteed to clear against an
        // arbitrary surface — a mid-grey placeholder under a shallow ramp
        // leaves `neutral[4]` short.
        let ink = Self::step_out_until_legible(ink, surface, min_ratio);
        let ink_dim = Self::step_out_until_legible(ink_dim, surface, min_ratio);

        OnCard {
            ink,
            ink_dim,
            ink_tertiary,
            tint: mix8(surface, ink, ON_CARD_TINT_T),
        }
    }

    /// An authored `tone` made legible on `ground`: untouched if it already
    /// clears `min_ratio`, otherwise stepped toward the contrasting extreme.
    pub fn ink_on(&self, ground: Color, tone: Color, min_ratio: f64) -> Color {
        Self::step_out_until_legible(tone, ground, min_ratio)
    }

    /// One `tone`, made legible on *every* ground in `grounds` at once.
    /// Resolving a control's label separately per state (rest/hover/press)
    /// lets it flip polarity mid-range — white to near-black on hover reads
    /// as a fault however well each frame measures — so ink is chosen once,
    /// against the worst ground, and held for the whole set.
    pub fn ink_on_any(&self, grounds: &[Color], tone: Color, min_ratio: f64) -> Color {
        use crate::a11y::contrast_ratio;
        let worst = |c: Color| {
            grounds
                .iter()
                .map(|g| contrast_ratio(c, *g))
                .fold(f64::INFINITY, f64::min)
        };
        if grounds.is_empty() || worst(tone) >= min_ratio {
            return tone;
        }
        // Polarity decided against the hardest ground, since a control's
        // states sit within a shade of each other.
        let hardest = grounds
            .iter()
            .copied()
            .min_by(|a, b| {
                contrast_ratio(tone, *a)
                    .partial_cmp(&contrast_ratio(tone, *b))
                    .unwrap_or(core::cmp::Ordering::Equal)
            })
            // `grounds` is non-empty here — the early return above already
            // handled that case — but fall back to `tone` unchanged rather
            // than panic if that invariant is ever violated.
            .unwrap_or(tone);
        let target = Self::contrasting_extreme(hardest);
        for step in 1..=16 {
            #[allow(
                clippy::cast_precision_loss,
                reason = "step ranges 1..=16, far below where f32 loses integer precision"
            )]
            let candidate = mix8(tone, target, step as f32 / 16.0);
            if worst(candidate) >= min_ratio {
                return candidate;
            }
        }
        target
    }

    /// Ink for anything drawn *on* the accent-primary fill (selected row,
    /// highlighted menu item). White reads right on a dark accent and wrong
    /// on a bright one, so this keeps a selected row and a primary button
    /// from disagreeing inside one frame.
    pub fn on_accent(&self, min_ratio: f64) -> Color {
        self.ink_on_any(&[self.accent[ACCENT_PRIMARY_IDX]], Color::WHITE, min_ratio)
    }

    pub fn accent_chip_on(&self, ground: Color, min_ratio: f64) -> AccentChip {
        self.tinted_chip_on(
            ground,
            self.accent[ACCENT_PRIMARY_IDX],
            ACCENT_CHIP_WASH,
            min_ratio,
        )
    }

    /// [`Self::accent_chip_on`] for any `tint` at any `wash` strength (a
    /// danger-tinted button, a semantic status pill).
    ///
    /// The body composites **first**, so ink is measured against what the
    /// screen holds, not the token the wash was made from: a label in the
    /// same tint as its own wash can never reach a text floor — both sides
    /// move together (`accent × 0.18` fill with an `accent` label measures
    /// 3.95:1 on the dark card, 3.79:1 on the light one). Ink walks out of
    /// the tint only as far as the floor demands. The edge is measured
    /// against the *ground*, since its job is separating pill from row.
    pub fn tinted_chip_on(
        &self,
        ground: Color,
        tint: Color,
        wash: f32,
        min_ratio: f64,
    ) -> AccentChip {
        let fill = mix8(ground, tint, wash);
        AccentChip {
            fill,
            border: self.ink_on(ground, tint, NON_TEXT_MIN_CONTRAST),
            ink: self.ink_on(fill, tint, min_ratio),
        }
    }

    /// Returns `ink` untouched if it already clears `min_ratio`, otherwise
    /// steps it toward whichever extreme contrasts with `surface`.
    fn step_out_until_legible(ink: Color, surface: Color, min_ratio: f64) -> Color {
        use crate::a11y::contrast_ratio;
        if contrast_ratio(ink, surface) >= min_ratio {
            return ink;
        }
        let target = Self::contrasting_extreme(surface);
        // Sixteenths: close to the authored tone, few enough comparisons.
        for step in 1..=16 {
            #[allow(
                clippy::cast_precision_loss,
                reason = "step ranges 1..=16, far below where f32 loses integer precision"
            )]
            let candidate = mix8(ink, target, step as f32 / 16.0);
            if contrast_ratio(candidate, surface) >= min_ratio {
                return candidate;
            }
        }
        target
    }

    fn contrasting_extreme(ground: Color) -> Color {
        use crate::a11y::contrast_ratio;
        let black = Color::from_rgb8(0, 0, 0);
        let white = Color::from_rgb8(255, 255, 255);
        if contrast_ratio(black, ground) >= contrast_ratio(white, ground) {
            black
        } else {
            white
        }
    }

    /// A hairline visible on `ground` whichever side of the light/dark line
    /// it falls on. White at low alpha (what every `*_white_*` token
    /// assumes) disappears over a light ground, and branching on the
    /// palette's `dark` flag doesn't fix it either — a preset can be dark
    /// overall while one surface sits the other way (a white card under
    /// dark chrome). Tries both polarities at `alpha`, keeps whichever
    /// moves `ground` further.
    pub fn hairline_on(ground: Color, alpha: u8) -> Color {
        use crate::a11y::relative_luminance;
        let g = relative_luminance(ground);
        let light = Color::from_rgba8(255, 255, 255, alpha);
        let dark = Color::from_rgba8(0, 0, 0, alpha);
        let moved = |c: Color| (relative_luminance(composite(c, ground)) - g).abs();
        if moved(light) >= moved(dark) {
            light
        } else {
            dark
        }
    }

    /// The body a control takes when **lifted** off the ground (hovered tab
    /// pill, selected row). `t` runs 0.0 (flat) to 1.0 (full lift).
    ///
    /// A fixed `white/5`-`white/10` wash only lifts over a dark ground; over
    /// a light one "active" stops looking active. Interpolating
    /// `surfaces.base` → `surfaces.raised` instead needs no `dark` flag.
    /// Where a preset has no raised tier (high contrast paints everything
    /// pure black, so `raised` *is* `base`), falls back to a wash polarity
    /// chosen the same way [`Colors::hairline_on`] picks one.
    pub fn lift(&self, t: f32) -> Color {
        mix8(self.surfaces.base, self.lift_target(), t)
    }

    /// The colour a lift moves *toward*. Named apart from [`Colors::lift`]
    /// because it also anchors [`Colors::glass_film`]: a material and a flat
    /// wash must agree which way is "away from the ground", or a glass pill
    /// and a solid one lift opposite ways.
    pub fn lift_target(&self) -> Color {
        use crate::a11y::relative_luminance;
        let base = self.surfaces.base;
        let g = relative_luminance(base);
        let raised = self.surfaces.raised;
        if (relative_luminance(raised) - g).abs() >= HAIRLINE_MIN_DELTA {
            raised
        } else {
            composite(Self::hairline_on(base, LIFT_FALLBACK_ALPHA), base)
        }
    }

    /// Whether this palette's surfaces lift by getting *brighter* — asked
    /// per ground rather than off the preset's `dark` flag, since "brighter"
    /// is a different direction on a paper-white desktop than a graphite one.
    pub fn lifts_bright(&self) -> bool {
        use crate::a11y::relative_luminance;
        let base = self.surfaces.base;
        relative_luminance(self.lift_target()) >= relative_luminance(base)
    }

    /// The colour of the film a glass surface is made of. **Not white** —
    /// `surface_white_*` only works over a dark ground. Glass takes its
    /// colour from what it's made of, so the film is the theme's raised
    /// tier carried one derived step further (`GLASS_FILM_LIFT`), opaque
    /// (alpha belongs to the state painting it, not the material).
    ///
    /// The step follows [`Colors::lifts_bright`], not [`Colors::hairline_on`]'s
    /// rule — an edge only needs to be *seen* (whichever polarity moves it
    /// more), but a film must agree with the cards around it which way is
    /// "up" (a paper palette is brighter even though black moves it more).
    pub fn glass_film(&self) -> Color {
        let step = if self.lifts_bright() {
            Color::WHITE
        } else {
            Color::BLACK
        };
        composite(with_alpha(step, GLASS_FILM_LIFT), self.lift_target())
    }

    /// The hairline separating a window from the desktop. Deliberately
    /// **not** a property of the window card — painted outside the card's
    /// silhouette since the shell never samples the app's pixels inside it,
    /// so one constant stroked over app content would read as a soft rim
    /// light over a dark app and a glaring line over a light one.
    /// `crate::widget::tile_host::TileHost` owns the matching geometry.
    ///
    /// A preset's declared [`Surfaces::window_card_border`] is honoured
    /// wherever it registers; only a declaration the ground would swallow
    /// gets its polarity flipped.
    pub fn window_edge(&self) -> Color {
        use crate::a11y::relative_luminance;
        let ground = self.surfaces.base;
        let declared = self.surfaces.window_card_border;
        let g = relative_luminance(ground);
        if (relative_luminance(composite(declared, ground)) - g).abs() >= HAIRLINE_MIN_DELTA {
            declared
        } else {
            Self::hairline_on(ground, declared.a())
        }
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
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "MAX_CHIP_DEEPEN is a non-negative 0.0..=1.0 design token, so its percentage fits exactly in u8"
        )]
        let max_step = (MAX_CHIP_DEEPEN * 100.0).floor() as u8;
        for step in 1..=max_step {
            let deepened = mix8(fill, black, f32::from(step) / 100.0);
            if crate::a11y::contrast_ratio(preferred_label, deepened) >= AA_BODY {
                return (deepened, preferred_label);
            }
        }
        (fill, self.readable_on(fill, preferred_label))
    }

    /// The default dark palette: neutral graphite (deliberately *not* a
    /// blue-drenched slate) with a single restrained blue accent.
    pub fn dark() -> Self {
        Self::themed(&PaletteSpec {
            dark: true,
            base: Color::from_rgb8(0x16, 0x17, 0x1B),
            raised: Color::from_rgb8(0x1F, 0x20, 0x25),
            popover: Color::from_rgb8(0x0D, 0x0E, 0x11),
            text: Color::from_rgb8(0xEC, 0xED, 0xF0),
            text_dim: Color::from_rgb8(0x9A, 0x9C, 0xA6),
            text_tertiary: Color::from_rgb8(0x6B, 0x6D, 0x77),
            border: Color::from_rgb8(0x2B, 0x2C, 0x33),
            focus: Color::from_rgb8(0x58, 0x91, 0xE8),
            accent: Color::from_rgb8(0x58, 0x91, 0xE8),
            accent_hover: Color::from_rgb8(0x48, 0x7B, 0xD1),
            success: Color::from_rgb8(0x4F, 0xB4, 0x77),
            warning: Color::from_rgb8(0xE2, 0xA9, 0x3C),
            danger: Color::from_rgb8(0xE5, 0x61, 0x5F),
            info: Color::from_rgb8(0x5E, 0x9B, 0xEA),
            window_card: None,
        })
    }

    /// Paper-white light theme with the same restrained blue accent as the
    /// dark default.
    pub fn light() -> Self {
        Self::themed(&PaletteSpec {
            dark: false,
            base: Color::from_rgb8(0xF3, 0xF3, 0xF1),
            raised: Color::from_rgb8(0xFF, 0xFF, 0xFF),
            popover: Color::from_rgb8(0xFF, 0xFF, 0xFF),
            text: Color::from_rgb8(0x1B, 0x1C, 0x1E),
            text_dim: Color::from_rgb8(0x5B, 0x5D, 0x63),
            text_tertiary: Color::from_rgb8(0x8A, 0x8C, 0x92),
            border: Color::from_rgb8(0xDE, 0xDE, 0xDB),
            focus: Color::from_rgb8(0x2F, 0x6F, 0xD6),
            accent: Color::from_rgb8(0x2F, 0x6F, 0xD6),
            accent_hover: Color::from_rgb8(0x25, 0x5C, 0xB8),
            success: Color::from_rgb8(0x1E, 0x9E, 0x5A),
            warning: Color::from_rgb8(0xB5, 0x7A, 0x00),
            danger: Color::from_rgb8(0xCB, 0x3B, 0x3B),
            info: Color::from_rgb8(0x1E, 0x74, 0xC4),
            window_card: None,
        })
    }

    /// The original Tilo look — the slate-and-indigo "tilo concept"
    /// palette, preserved verbatim as a selectable preset. Every value here
    /// is Tailwind ground truth; see the tests for the mapping.
    pub fn tilo_blue() -> Self {
        Self {
            neutral: ColorRamp {
                stops: [
                    Color::from_rgb8(2, 6, 23),
                    Color::from_rgb8(15, 23, 42),
                    Color::from_rgb8(30, 41, 59),
                    Color::from_rgb8(51, 65, 85),
                    Color::from_rgb8(71, 85, 105),
                    Color::from_rgb8(100, 116, 139),
                    Color::from_rgb8(148, 163, 184),
                    Color::from_rgb8(203, 213, 225),
                    Color::from_rgb8(226, 232, 240),
                    Color::from_rgb8(241, 245, 249),
                    Color::from_rgb8(248, 250, 252),
                ],
            },
            accent: ColorRamp {
                stops: [
                    Color::from_rgb8(30, 27, 75),
                    Color::from_rgb8(49, 46, 129),
                    Color::from_rgb8(55, 48, 163),
                    Color::from_rgb8(67, 56, 202),
                    Color::from_rgb8(79, 70, 229),
                    Color::from_rgb8(99, 102, 241),
                    Color::from_rgb8(129, 140, 248),
                    Color::from_rgb8(165, 180, 252),
                    Color::from_rgb8(199, 210, 254),
                    Color::from_rgb8(224, 231, 255),
                    Color::from_rgb8(238, 242, 255),
                ],
            },
            semantic: SemanticColors {
                success: Color::from_rgb8(16, 185, 129),
                warning: Color::from_rgb8(245, 158, 11),
                danger: Color::from_rgb8(239, 68, 68),
                info: Color::from_rgb8(96, 165, 250),
            },
            surfaces: Surfaces {
                base: Color::from_rgb8(15, 23, 42),
                raised: Color::from_rgb8(30, 41, 59),
                overlay: Color::from_rgba8(0, 0, 0, 255),
                scrim: Color::from_rgb8(2, 6, 23),
                glass: Color::from_rgba8(15, 23, 42, 204),
                glass_blur_px: 24.0,
                window_card: Color::WHITE,
                window_card_border: Color::from_rgba8(255, 255, 255, 51),
            },
            text_primary: Color::WHITE,
            text_dim: Color::from_rgb8(148, 163, 184),
            text_tertiary: Color::from_rgb8(100, 116, 139),
            border_standard: Color::from_rgb8(51, 65, 85),
            border_focus: Color::from_rgb8(99, 102, 241),
            // A dark palette, so the veneers lighten.
            overlay_subtle: Color::from_rgba8(255, 255, 255, 13),
            overlay_medium: Color::from_rgba8(255, 255, 255, 26),
            overlay_strong: Color::from_rgba8(255, 255, 255, 51),
            overlay_border_subtle: Color::from_rgba8(255, 255, 255, 26),
            overlay_border_strong: Color::from_rgba8(255, 255, 255, 51),
            surface_white_5: Color::from_rgba8(255, 255, 255, 13),
            surface_white_10: Color::from_rgba8(255, 255, 255, 26),
            surface_white_20: Color::from_rgba8(255, 255, 255, 51),
            border_white_10: Color::from_rgba8(255, 255, 255, 26),
            border_white_20: Color::from_rgba8(255, 255, 255, 51),
        }
    }
}

#[cfg(test)]
mod tests {
    //! The **new graphite default** ([`Colors::dark`]) pins its identity
    //! surfaces/text/accent and asserts WCAG AA; the **preserved concept
    //! palette** ([`Colors::tilo_blue`]) pins Tailwind ground truth
    //! byte-for-byte.
    use super::*;
    use crate::a11y::contrast_ratio;
    use crate::color_compat::ColorCompat;

    fn rgb(c: Color) -> [u8; 3] {
        [c.r(), c.g(), c.b()]
    }
    fn rgba(c: Color) -> [u8; 4] {
        [c.r(), c.g(), c.b(), c.a()]
    }

    #[test]
    fn the_dark_default_is_neutral_graphite_with_legible_tiers() {
        // phase: dark_default_is_neutral_graphite_not_slate_blue
        {
            let c = Colors::dark();
            assert_eq!(rgb(c.surfaces.base), [0x16, 0x17, 0x1B], "graphite ground");
            assert_eq!(rgb(c.surfaces.raised), [0x1F, 0x20, 0x25], "raised card");
            // Blue channel leads red by only a hair, nothing like slate-900's
            // 2.8x blue lead.
            let b = c.surfaces.base;
            assert!(
                (i32::from(b.b()) - i32::from(b.r())) <= 6,
                "the default ground must be a near-neutral graphite, not blue-drenched",
            );
        }
        // phase: dark_default_accent_is_a_restrained_blue
        {
            let c = Colors::dark();
            assert_eq!(
                rgb(c.accent[ACCENT_PRIMARY_IDX]),
                [0x58, 0x91, 0xE8],
                "restrained blue accent"
            );
            assert_eq!(
                rgb(c.accent[ACCENT_HOVER_IDX]),
                [0x48, 0x7B, 0xD1],
                "hover one step darker"
            );
            assert_ne!(rgb(c.surfaces.base), rgb(c.accent[ACCENT_PRIMARY_IDX]));
        }
        // phase: dark_default_has_three_legible_text_tiers
        {
            let c = Colors::dark();
            let base = c.surfaces.base;
            let primary = contrast_ratio(c.text_primary, base);
            let dim = contrast_ratio(c.text_dim, base);
            let tertiary = contrast_ratio(c.text_tertiary, base);

            assert!(
                primary > dim,
                "primary ({primary:.2}) must out-contrast secondary ({dim:.2})"
            );
            assert!(
                dim > tertiary,
                "secondary ({dim:.2}) must out-contrast tertiary ({tertiary:.2})"
            );

            assert!(primary >= 4.5, "primary text clears AA (got {primary:.2})");
            assert!(dim >= 4.5, "secondary text clears AA (got {dim:.2})");
            assert!(
                tertiary >= 3.0,
                "tertiary text clears AA-large (got {tertiary:.2})"
            );
        }
        // phase: dark_default_body_text_clears_aa_on_raised_surfaces_too
        {
            let c = Colors::dark();
            assert!(
                contrast_ratio(c.text_primary, c.surfaces.raised) >= 4.5,
                "primary on raised"
            );
            assert!(
                contrast_ratio(c.text_dim, c.surfaces.raised) >= 4.5,
                "secondary on raised"
            );
        }
        // phase: dark_default_popover_is_a_rich_near_black
        {
            let c = Colors::dark();
            let base = c.surfaces.base;
            let over = c.surfaces.overlay;
            let lum = |x: Color| {
                0.2126 * f32::from(x.r()) + 0.7152 * f32::from(x.g()) + 0.0722 * f32::from(x.b())
            };
            assert!(
                lum(over) < lum(base),
                "popover surface is deeper than the ground"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn overlays_and_state_veneers_track_their_ground_in_every_theme() {
        // phase: overlay_surface_is_fully_opaque_in_every_theme
        {
            for (name, c) in [
                ("dark", Colors::dark()),
                ("light", Colors::light()),
                ("tilo_blue", Colors::tilo_blue()),
            ] {
                assert_eq!(c.surfaces.overlay.a(), 255, "{name} overlay must be opaque");
            }
        }
        // phase: overlay_aliases_track_the_semantic_tokens_in_every_theme
        // The alias fields must never drift from the semantic ones they name —
        // the fleet reads both, and a widget picking the stale pair would paint a
        // veneer of the wrong polarity.
        {
            for name in crate::theme::Theme::ALL {
                let c = &crate::theme::Theme::named(name).unwrap().tokens.colors;
                assert_eq!(
                    rgba(c.surface_white_5),
                    rgba(c.overlay_subtle),
                    "{name} subtle"
                );
                assert_eq!(
                    rgba(c.surface_white_10),
                    rgba(c.overlay_medium),
                    "{name} medium"
                );
                assert_eq!(
                    rgba(c.surface_white_20),
                    rgba(c.overlay_strong),
                    "{name} strong"
                );
                assert_eq!(
                    rgba(c.border_white_10),
                    rgba(c.overlay_border_subtle),
                    "{name} border subtle"
                );
                assert_eq!(
                    rgba(c.border_white_20),
                    rgba(c.overlay_border_strong),
                    "{name} border strong"
                );
            }
        }
        // phase: state_veneers_move_every_ground_in_every_theme
        // The defect this token model exists to prevent: a veneer that does not
        // move the surface it is painted on. Light themes used to inherit white
        // veneers, so `surface_white_5` over the Light theme's `#FFFFFF` card was
        // a literal no-op (1.000:1) and hover feedback did not render at all.
        //
        // Checked on both grounds a veneer realistically lands on, in every
        // theme, so no polarity can regress unnoticed.
        {
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
        // phase: veneer_polarity_follows_the_ground
        // A veneer has to push the ground *away* from itself: lighten a dark
        // surface, darken a light one. Asserted on lightness rather than on the
        // literal colour so a future theme is free to tint its veneers.
        {
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
                        assert!(
                            delta > 0.0,
                            "{name}: {tier_name} must lighten a dark ground (got {delta:.2})"
                        );
                    } else {
                        assert!(
                            delta < 0.0,
                            "{name}: {tier_name} must darken a light ground (got {delta:.2})"
                        );
                    }
                }
            }
        }
        // phase: light_overlays_match_the_dark_perceptual_step
        // Equal alpha does not buy equal visibility on grounds at opposite ends
        // of the lightness curve, so the light tiers are solved for the *step*
        // the dark tiers produce, not mirrored from their alpha. This pins that
        // solve: every light theme's veneer lands within 2 L* of the dark target.
        {
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
        // phase: high_contrast_veneers_are_stronger_and_still_host_white_text
        // The high-contrast preset re-solves its own tiers: it overrides its
        // surfaces to pure black *after* inheriting the dark tokens, and against
        // black the dark tiers collapse to a 1.08:1 step — invisible to exactly
        // the users this preset exists for. It must be markedly stronger than the
        // default dark theme's, while still hosting the theme's white text at AA.
        {
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
    }

    #[test]
    fn tilo_blue_matches_the_concept_and_tailwind_palettes() {
        // phase: tilo_blue_neutral_ramp_is_tailwind_slate_950_through_50
        {
            let n = &Colors::tilo_blue().neutral;
            assert_eq!(rgb(n[0]), [2, 6, 23], "slate-950 #020617");
            assert_eq!(rgb(n[1]), [15, 23, 42], "slate-900 #0f172a");
            assert_eq!(rgb(n[2]), [30, 41, 59], "slate-800 #1e293b");
            assert_eq!(rgb(n[3]), [51, 65, 85], "slate-700 #334155");
            assert_eq!(rgb(n[4]), [71, 85, 105], "slate-600 #475569");
            assert_eq!(rgb(n[5]), [100, 116, 139], "slate-500 #64748b");
            assert_eq!(rgb(n[6]), [148, 163, 184], "slate-400 #94a3b8");
            assert_eq!(rgb(n[7]), [203, 213, 225], "slate-300 #cbd5e1");
            assert_eq!(rgb(n[8]), [226, 232, 240], "slate-200 #e2e8f0");
            assert_eq!(rgb(n[9]), [241, 245, 249], "slate-100 #f1f5f9");
            assert_eq!(rgb(n[10]), [248, 250, 252], "slate-50  #f8fafc");
        }
        // phase: tilo_blue_accent_ramp_is_tailwind_indigo_950_through_50
        {
            let a = &Colors::tilo_blue().accent;
            assert_eq!(rgb(a[0]), [30, 27, 75], "indigo-950 #1e1b4b");
            assert_eq!(rgb(a[1]), [49, 46, 129], "indigo-900 #312e81");
            assert_eq!(rgb(a[2]), [55, 48, 163], "indigo-800 #3730a3");
            assert_eq!(rgb(a[3]), [67, 56, 202], "indigo-700 #4338ca");
            assert_eq!(rgb(a[4]), [79, 70, 229], "indigo-600 #4f46e5");
            assert_eq!(rgb(a[5]), [99, 102, 241], "indigo-500 #6366f1");
            assert_eq!(rgb(a[6]), [129, 140, 248], "indigo-400 #818cf8");
            assert_eq!(rgb(a[7]), [165, 180, 252], "indigo-300 #a5b4fc");
            assert_eq!(rgb(a[8]), [199, 210, 254], "indigo-200 #c7d2fe");
            assert_eq!(rgb(a[9]), [224, 231, 255], "indigo-100 #e0e7ff");
            assert_eq!(rgb(a[10]), [238, 242, 255], "indigo-50  #eef2ff");
        }
        // phase: tilo_blue_semantic_colors_match_tailwind
        {
            let s = &Colors::tilo_blue().semantic;
            assert_eq!(rgb(s.success), [16, 185, 129], "emerald-500 #10b981");
            assert_eq!(rgb(s.warning), [245, 158, 11], "amber-500   #f59e0b");
            assert_eq!(rgb(s.danger), [239, 68, 68], "red-500     #ef4444");
            assert_eq!(rgb(s.info), [96, 165, 250], "blue-400    #60a5fa");
        }
        // phase: tilo_blue_surfaces_match_concept_layered_palette
        {
            let s = &Colors::tilo_blue().surfaces;
            assert_eq!(rgb(s.base), [15, 23, 42], "slate-900 #0f172a");
            assert_eq!(rgb(s.raised), [30, 41, 59], "slate-800 #1e293b");
            assert_eq!(rgba(s.overlay), [0, 0, 0, 255], "opaque deep menu surface");
            assert_eq!(rgba(s.glass), [15, 23, 42, 204], "slate-900/80");
            assert_eq!(rgba(s.window_card), [255, 255, 255, 255], "white");
            assert_eq!(rgba(s.window_card_border), [255, 255, 255, 51], "white/20");
        }
        // phase: tilo_blue_text_and_border_tokens_match_concept
        {
            let c = Colors::tilo_blue();
            assert_eq!(rgba(c.text_primary), [255, 255, 255, 255], "white");
            assert_eq!(rgb(c.text_dim), [148, 163, 184], "slate-400 #94a3b8");
            assert_eq!(rgb(c.border_standard), [51, 65, 85], "slate-700 #334155");
            assert_eq!(rgb(c.border_focus), [99, 102, 241], "indigo-500 #6366f1");
        }
        // phase: themed_pins_the_accent_and_hover_stops_exactly
        {
            let c = Colors::dark();
            assert_eq!(rgb(c.accent[ACCENT_PRIMARY_IDX]), [0x58, 0x91, 0xE8]);
            assert_eq!(rgb(c.accent[ACCENT_HOVER_IDX]), [0x48, 0x7B, 0xD1]);
        }
        // phase: translucent_white_tokens_match_alpha_bytes
        {
            let c = Colors::dark();
            assert_eq!(rgba(c.surface_white_5), [255, 255, 255, 13], "white/5");
            assert_eq!(rgba(c.surface_white_10), [255, 255, 255, 26], "white/10");
            assert_eq!(rgba(c.surface_white_20), [255, 255, 255, 51], "white/20");
            assert_eq!(
                rgba(c.border_white_10),
                [255, 255, 255, 26],
                "border-white/10"
            );
            assert_eq!(
                rgba(c.border_white_20),
                [255, 255, 255, 51],
                "border-white/20"
            );
        }
    }

    #[test]
    fn series_colours_stay_distinguishable_and_hsl_round_trips() {
        // phase: categorical_series_stays_distinguishable_in_every_theme
        {
            use crate::theme::Theme;

            let hue = |c: Color| super::rgb_to_hsl(c).0;
            let rel_luma = |c: Color| {
                let f = |v: u8| {
                    let v = f32::from(v) / 255.0;
                    if v <= 0.03928 {
                        v / 12.92
                    } else {
                        ((v + 0.055) / 1.055).powf(2.4)
                    }
                };
                0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
            };

            for (name, theme) in [
                ("dark", Theme::dark()),
                ("light", Theme::light()),
                ("high_contrast", Theme::high_contrast()),
            ] {
                let c = &theme.tokens.colors;
                assert_eq!(
                    c.categorical(0),
                    c.accent[ACCENT_PRIMARY_IDX],
                    "{name}: the series must start from the theme's accent"
                );
                assert_eq!(
                    c.categorical(CATEGORICAL_LEN),
                    c.categorical(0),
                    "{name}: the series must wrap at CATEGORICAL_LEN"
                );

                for i in 0..CATEGORICAL_LEN {
                    let a = c.categorical(i);
                    let b = c.categorical((i + 1) % CATEGORICAL_LEN);
                    // Weighted RGB distance, not hue alone — a hue-only check is
                    // what drove the series around the whole wheel and produced
                    // colours foreign to the theme.
                    let d = |x: Color, y: Color| {
                        let (dr, dg, db) = (
                            f32::from(x.r()) - f32::from(y.r()),
                            f32::from(x.g()) - f32::from(y.g()),
                            f32::from(x.b()) - f32::from(y.b()),
                        );
                        (2.0 * dr * dr + 4.0 * dg * dg + 3.0 * db * db).sqrt()
                    };
                    assert!(
                        d(a, b) > 28.0,
                        "{name}: workspaces {i} and {} are only {:.0} apart — \
                     adjacent sectors would read as the same colour",
                        (i + 1) % CATEGORICAL_LEN,
                        d(a, b)
                    );

                    // …and still this theme's family, not a hue reachable by a
                    // full rotation (which could turn a blue accent purple).
                    let dh_acc = (hue(a) - hue(c.accent[ACCENT_PRIMARY_IDX])).abs();
                    let from_accent = dh_acc.min(360.0 - dh_acc);
                    assert!(
                        from_accent <= CATEGORICAL_HUE_ARC_DEG / 2.0 + 1.0,
                        "{name}: workspace colour {i} sits {from_accent:.0}° from the \
                     accent — outside the theme's own family"
                    );

                    let ground = rel_luma(c.surfaces.base);
                    let fg = rel_luma(a);
                    let contrast = (fg.max(ground) + 0.05) / (fg.min(ground) + 0.05);
                    assert!(
                        contrast > 1.6,
                        "{name}: workspace colour {i} has only {contrast:.2}:1 against \
                     the base surface — it would disappear"
                    );
                }
            }
        }
        // phase: hsl_round_trips_within_one_step
        {
            for c in [
                Color::from_rgb8(0x58, 0x91, 0xE8),
                Color::from_rgb8(0x2F, 0x6F, 0xD6),
                Color::from_rgb8(0, 153, 255),
                Color::from_rgb8(0xFF, 0xFF, 0xFF),
                Color::from_rgb8(0x10, 0x10, 0x10),
                Color::from_rgb8(0xE2, 0xA9, 0x3C),
            ] {
                let (h, s, l) = super::rgb_to_hsl(c);
                let back = super::hsl_to_rgb(h, s, l);
                for (a, b) in [(c.r(), back.r()), (c.g(), back.g()), (c.b(), back.b())] {
                    assert!(
                        (i32::from(a) - i32::from(b)).abs() <= 1,
                        "round-trip drifted: {c:?} -> {back:?}"
                    );
                }
            }
        }
    }

    // --- state veneers ----------------------------------------------------

    /// Straight-alpha composite of `fg` over an opaque `bg` — what the eye
    /// actually receives when a translucent veneer is painted on a surface.
    fn composite(fg: Color, bg: Color) -> Color {
        let a = f32::from(fg.a()) / 255.0;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a weighted average of two u8 channels remains within 0..=255 before rounding"
        )]
        let ch = |f: u8, b: u8| (f32::from(f) * a + f32::from(b) * (1.0 - a)).round() as u8;
        Color::from_rgb8(ch(fg.r(), bg.r()), ch(fg.g(), bg.g()), ch(fg.b(), bg.b()))
    }

    /// CIE L* (0..=100) — perceptual lightness, so veneer strength can be
    /// compared across grounds of very different luminance.
    fn lstar(c: Color) -> f64 {
        let y = crate::a11y::relative_luminance(c);
        if y > 0.008_856 {
            116.0 * y.cbrt() - 16.0
        } else {
            903.3 * y
        }
    }

    // --- readable_on ------------------------------------------------------

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn readable_pairings_and_legible_fills_always_clear_aa() {
        // phase: readable_on_keeps_a_preferred_colour_that_already_passes
        {
            let c = Colors::dark();
            // Primary text on the card is 13.89:1 — nothing to correct.
            let got = c.readable_on(c.surfaces.raised, c.text_primary);
            assert_eq!(rgba(got), rgba(c.text_primary));
        }
        // phase: readable_on_lifts_a_preferred_colour_that_fails
        {
            // The exact pairing the audit measured: the `neutral[3]` fill tier
            // under a `text_dim` label. It clears AA in the dark default (4.69:1)
            // and fails in light (4.26:1) — one theme's pass is what kept it in.
            let light = Colors::light();
            let fill = light.neutral[3];
            let before = contrast_ratio(light.text_dim, fill);
            assert!(
                before < 4.5,
                "precondition: the light pairing fails AA (got {before:.2})"
            );

            let after = contrast_ratio(light.readable_on(fill, light.text_dim), fill);
            assert!(
                after >= 4.5,
                "the lifted label must clear AA (got {after:.2})"
            );
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
        // phase: readable_on_always_returns_an_aa_pairing
        // The fallback arm cannot fail: white and black are 21:1 apart, so at
        // least one of them clears AA against any colour whatsoever.
        {
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
                assert!(
                    r >= 4.5,
                    "readable_on returned a {r:.2}:1 pairing on {:?}",
                    rgb(bg)
                );
            }
        }
        // phase: legible_fill_clears_aa_on_every_accent_and_danger
        // A filled chip must host its label at AA in every theme. White on the
        // raw accent fails in 11 of the 14 — 1.69:1 on Rosé Pine, 1.70:1 on
        // Gruvbox, 2.00:1 on Nord, and 3.17:1 on the shipped dark default.
        {
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
        // phase: legible_fill_deepens_rather_than_repainting_the_label
        // Deepening is the preferred repair because it keeps the chip reading as
        // the same component. Assert it is actually what happens for a mid-tone
        // accent, and that the deepened fill stays recognisably the theme's hue.
        {
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
            assert!(
                fill.b() > fill.g() && fill.g() > fill.r(),
                "still the theme's blue"
            );
        }
        // phase: legible_fill_keeps_pale_accents_and_repaints_their_label
        // A pale accent cannot be deepened into a white-label chip without going
        // muddy, so those keep their fill and take a dark label instead. Gruvbox's
        // yellow is the canonical case: 41 % deepening would be needed, past the
        // cap, and a black-on-yellow chip is the idiomatic look anyway.
        {
            let t = crate::theme::Theme::named("Gruvbox").unwrap();
            let c = &t.tokens.colors;
            let raw = t.accent_primary;
            let (fill, label) = c.legible_fill(raw, Color::WHITE);

            assert_eq!(rgba(fill), rgba(raw), "the yellow is left alone");
            assert_ne!(rgba(label), rgba(Color::WHITE), "the label is what moves");
            assert!(contrast_ratio(label, fill) >= 4.5);
        }
        // phase: legible_fill_is_inert_where_the_pairing_already_passes
        // Where white already clears AA the pair is returned untouched, so a
        // theme designed correctly is not repainted.
        {
            let t = crate::theme::Theme::light();
            let c = &t.tokens.colors;
            let raw = t.accent_primary;
            assert!(contrast_ratio(Color::WHITE, raw) >= 4.5, "precondition");
            let (fill, label) = c.legible_fill(raw, Color::WHITE);
            assert_eq!(rgba(fill), rgba(raw));
            assert_eq!(rgba(label), rgba(Color::WHITE));
        }
    }

    // --- legible_fill -----------------------------------------------------

    // --- the permission-denied scrim --------------------------------------

    #[test]
    fn the_denied_scrim_mutes_every_control_weakest_on_accent_fills() {
        // phase: the_denied_scrim_mutes_a_control_in_every_theme
        // The scrim is one const shared by every theme, which looks like the
        // defect the overlay tiers had — and measured against a widget's
        // *background* it reads that way: black over the High Contrast preset's
        // pure-black card is a literal 1.00:1.
        //
        // That is the wrong surface. The scrim is painted over the widget after
        // it has drawn itself, so it lands on the border, label and knob too, and
        // those are what carry "you cannot act on this". Measured there it is
        // strongest exactly where the background reading said it was absent: High
        // Contrast drops white content from 21.00:1 to 9.90:1.
        //
        // So this pins the property that actually matters — a denied control's
        // content loses contrast against its own body in every theme — rather
        // than a polarity the scrim does not need. Making it directional would
        // invert the meaning on dark themes, where a lightening scrim would leave
        // a disabled control brighter than an enabled one.
        {
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
                    let moved =
                        |x: Color| (lstar(x) - lstar(composite(PERMISSION_DENIED_SCRIM, x))).abs();
                    let shift = moved(content).max(moved(body));
                    assert!(
                        shift >= 3.0,
                        "{name}/{kind}: the denied control must visibly shift (got {shift:.1} L*)",
                    );
                }
            }
        }
        // phase: the_denied_scrim_is_weakest_on_accent_fills
        // Accent-filled controls are the weakest case by construction: the scrim
        // darkens fill and label together, so their ratio is largely preserved.
        // Pinned so that if it ever degrades to "no signal at all" a test says so
        // — the shots show it still reads today, but only just.
        {
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
            assert!(
                worst > 1.08,
                "an accent chip must still mute (worst drop x{worst:.2})"
            );
        }
    }
}
