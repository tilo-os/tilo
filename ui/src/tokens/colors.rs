use crate::color_compat::ColorCompat;
use vello::peniko::Color;

/// Index into the accent ramp used as the dominant fill (active dot, slider
/// track, toggle on-state).
pub const ACCENT_PRIMARY_IDX: usize = 5;

/// One step darker than [`ACCENT_PRIMARY_IDX`].
pub const ACCENT_HOVER_IDX: usize = 4;

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

fn with_a(c: Color, a: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), a)
}

/// Composites `over` onto an opaque `ground`: a token's own RGBA says
/// nothing about how it reads (`white/20` differs over black vs white), so
/// every derivation below measures the composite, never the token.
fn composite(over: Color, ground: Color) -> Color {
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
const HAIRLINE_MIN_DELTA: f64 = 0.02;

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
    pub surface_white_5: Color,
    pub surface_white_10: Color,
    pub surface_white_20: Color,
    pub border_white_10: Color,
    pub border_white_20: Color,
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

/// sRGB → HSL, hue in degrees. Straight sRGB rather than a perceptual space
/// — the point here is *separation*, which the cheap conversion does fine.
#[allow(
    clippy::many_single_char_names,
    reason = "r/g/b/h/s/l/max/min/d are the standard RGB<->HSL conversion \
              variable names used throughout this file's colour math"
)]
fn rgb_to_hsl(c: Color) -> (f32, f32, f32) {
    let (r, g, b) = (
        f32::from(c.r()) / 255.0,
        f32::from(c.g()) / 255.0,
        f32::from(c.b()) / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = f32::midpoint(max, min);
    let d = max - min;
    if d <= f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs()).max(f32::EPSILON);
    #[allow(
        clippy::float_cmp,
        reason = "max is exactly one of r/g/b by construction (f32::max picks one \
                  of its inputs verbatim), so this selects a branch by identity, \
                  not by comparing independently-computed floats"
    )]
    let h = if max == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h.rem_euclid(360.0), s.clamp(0.0, 1.0), l)
}

#[allow(
    clippy::many_single_char_names,
    reason = "h/s/l/c/x/m are the standard HSL<->RGB conversion variable names \
              used throughout this file's colour math"
)]
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> Color {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "hp is h.rem_euclid(360.0) / 60.0, always within 0.0..6.0, so the \
                  truncating cast always lands in 0..=5"
    )]
    let (r1, g1, b1) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "explicitly clamped to 0.0..=255.0 immediately above the cast"
    )]
    let q = |v: f32| (((v + m) * 255.0).round().clamp(0.0, 255.0)) as u8;
    Color::from_rgb8(q(r1), q(g1), q(b1))
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

        // Primary/hover stops pinned to the spec so ACCENT_PRIMARY_IDX /
        // ACCENT_HOVER_IDX land exactly on the theme's chosen accent.
        let mut accent = ColorRamp::eased(
            mix8(p.accent, black, 0.62),
            mix8(p.accent, Color::WHITE, 0.85),
        );
        accent.stops[ACCENT_HOVER_IDX] = p.accent_hover;
        accent.stops[ACCENT_PRIMARY_IDX] = p.accent;

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

        let veneer = if p.dark { Color::WHITE } else { black };

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
            // White at every strength failed on light themes (white over
            // `raised` measured 1.01:1 — why filter chips lost their pill),
            // so veneers use the contrasting extreme instead.
            surface_white_5: with_a(veneer, 13),
            surface_white_10: with_a(veneer, 26),
            surface_white_20: with_a(veneer, 51),
            border_white_10: with_a(veneer, 26),
            border_white_20: with_a(veneer, 51),
        }
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
    /// quietest tier is belongs to the palette (Solarized runs it at 2.8:1
    /// on purpose), and forcing a body-text floor would erase the tier.
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
    /// `surface_white_*` only works over a dark ground. Real glass takes its
    /// colour from what it's made of, so the film is the theme's raised
    /// tier carried one derived step further ([`GLASS_FILM_LIFT`]), opaque
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
        composite(with_a(step, GLASS_FILM_LIFT), self.lift_target())
    }

    /// The hairline separating a window from the desktop. Deliberately
    /// **not** a property of the window card — painted outside the card's
    /// silhouette since the shell never samples the app's pixels inside it,
    /// so one constant stroked over app content would read as a soft rim
    /// light over a dark app and a glaring line over a light one.
    /// [`crate::widget::tile_host::TileHost`] owns the matching geometry.
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

    /// The original qOS look — the slate-and-indigo "nirios concept"
    /// palette, preserved verbatim as a selectable preset. Every value here
    /// is Tailwind ground truth; see the tests for the mapping.
    pub fn qos_blue() -> Self {
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
    //! palette** ([`Colors::qos_blue`]) pins Tailwind ground truth
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
    fn dark_default_is_neutral_graphite_not_slate_blue() {
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

    #[test]
    fn dark_default_accent_is_a_restrained_blue() {
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

    #[test]
    fn dark_default_has_three_legible_text_tiers() {
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

    #[test]
    fn dark_default_body_text_clears_aa_on_raised_surfaces_too() {
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

    #[test]
    fn dark_default_popover_is_a_rich_near_black() {
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

    #[test]
    fn overlay_surface_is_fully_opaque_in_every_theme() {
        for (name, c) in [
            ("dark", Colors::dark()),
            ("light", Colors::light()),
            ("qos_blue", Colors::qos_blue()),
        ] {
            assert_eq!(c.surfaces.overlay.a(), 255, "{name} overlay must be opaque");
        }
    }

    #[test]
    fn qos_blue_neutral_ramp_is_tailwind_slate_950_through_50() {
        let n = &Colors::qos_blue().neutral;
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

    #[test]
    fn qos_blue_accent_ramp_is_tailwind_indigo_950_through_50() {
        let a = &Colors::qos_blue().accent;
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

    #[test]
    fn qos_blue_semantic_colors_match_tailwind() {
        let s = &Colors::qos_blue().semantic;
        assert_eq!(rgb(s.success), [16, 185, 129], "emerald-500 #10b981");
        assert_eq!(rgb(s.warning), [245, 158, 11], "amber-500   #f59e0b");
        assert_eq!(rgb(s.danger), [239, 68, 68], "red-500     #ef4444");
        assert_eq!(rgb(s.info), [96, 165, 250], "blue-400    #60a5fa");
    }

    #[test]
    fn qos_blue_surfaces_match_concept_layered_palette() {
        let s = &Colors::qos_blue().surfaces;
        assert_eq!(rgb(s.base), [15, 23, 42], "slate-900 #0f172a");
        assert_eq!(rgb(s.raised), [30, 41, 59], "slate-800 #1e293b");
        assert_eq!(rgba(s.overlay), [0, 0, 0, 255], "opaque deep menu surface");
        assert_eq!(rgba(s.glass), [15, 23, 42, 204], "slate-900/80");
        assert_eq!(rgba(s.window_card), [255, 255, 255, 255], "white");
        assert_eq!(rgba(s.window_card_border), [255, 255, 255, 51], "white/20");
    }

    #[test]
    fn qos_blue_text_and_border_tokens_match_concept() {
        let c = Colors::qos_blue();
        assert_eq!(rgba(c.text_primary), [255, 255, 255, 255], "white");
        assert_eq!(rgb(c.text_dim), [148, 163, 184], "slate-400 #94a3b8");
        assert_eq!(rgb(c.border_standard), [51, 65, 85], "slate-700 #334155");
        assert_eq!(rgb(c.border_focus), [99, 102, 241], "indigo-500 #6366f1");
    }

    #[test]
    fn themed_pins_the_accent_and_hover_stops_exactly() {
        let c = Colors::dark();
        assert_eq!(rgb(c.accent[ACCENT_PRIMARY_IDX]), [0x58, 0x91, 0xE8]);
        assert_eq!(rgb(c.accent[ACCENT_HOVER_IDX]), [0x48, 0x7B, 0xD1]);
    }

    #[test]
    fn translucent_white_tokens_match_alpha_bytes() {
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

    #[test]
    fn categorical_series_stays_distinguishable_in_every_theme() {
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

    #[test]
    fn hsl_round_trips_within_one_step() {
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
