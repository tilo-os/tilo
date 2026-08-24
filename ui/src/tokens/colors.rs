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
pub(crate) fn mix8(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| ((x as f32) * (1.0 - t) + (y as f32) * t).round() as u8;
    Color::from_rgb8(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Same colour with an explicit 8-bit alpha.
fn with_a(c: Color, a: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), a)
}

/// Composite a (possibly translucent) `over` onto an opaque `ground` in
/// straight sRGB and return the opaque result — the colour the eye actually
/// receives where the two meet.
///
/// Needed because a token's own RGBA says nothing about how it will read:
/// `white/20` is the same token over a black desktop and over a white page,
/// and only the composite distinguishes "a quiet rim light" from "a glaring
/// line". Every derivation below measures the composite, never the token.
fn composite(over: Color, ground: Color) -> Color {
    let a = over.a() as f32 / 255.0;
    let ch = |o: u8, g: u8| ((o as f32) * a + (g as f32) * (1.0 - a)).round() as u8;
    Color::from_rgb8(
        ch(over.r(), ground.r()),
        ch(over.g(), ground.g()),
        ch(over.b(), ground.b()),
    )
}

/// How much a hairline must move the ground it is painted on before a person
/// can see it, in WCAG relative-luminance units.
///
/// A hairline is meant to be quiet, so this sits far below any text threshold.
/// It only has to catch the failure that makes an edge *not an edge*: a wash
/// that composites to within rounding distance of the surface under it.
const HAIRLINE_MIN_DELTA: f64 = 0.02;

/// Alpha of the wash [`Colors::lift`] falls back to when a preset's surface
/// family gives it no raised tier to aim at. 15 % — enough to read as a
/// distinct body, quiet enough that ink painted on it keeps its contrast.
const LIFT_FALLBACK_ALPHA: u8 = 38;

/// How far [`Colors::glass_film`] carries the raised tier past itself.
///
/// The raised tier is where a *card* sits — one quiet step off the ground.
/// A film you can see through has to start further out than that, because
/// most of its strength is then given straight back by its own alpha: a
/// translucent body painted in the raised colour lands between the ground
/// and the card, which is under where an opaque card already is. This is
/// the step that buys the material somewhere to go.
const GLASS_FILM_LIFT: u8 = 46;

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
    /// The card that frames each window tile — the ground a window shows
    /// where the app it hosts has painted nothing.
    ///
    /// It belongs to the theme's own surface family, so an empty window
    /// reads as *this desktop's* window rather than as a sheet of paper
    /// dropped onto it. That matters because it is the only window ground
    /// the user ever compares side by side: apps that fill their own
    /// background (the terminal, the file explorer, the word processor's
    /// desk) cover the card entirely, so if the card came from a different
    /// colour family than they do, switching tabs would flip the window
    /// between two grounds.
    ///
    /// A *document* surface that wants to read as paper — a page in the
    /// word processor, a page in the PDF viewer — draws that page itself,
    /// inset on its own canvas with an edge and a shadow. Paper is content
    /// there, and looks deliberate; a whole window bleached to paper-white
    /// just looks like the app failed to paint.
    ///
    /// Ink for text painted straight onto this surface comes from
    /// [`Colors::on_card`], never from an assumption about which way round
    /// the card is.
    pub window_card: Color,
    /// Subtle border around the window card.
    pub window_card_border: Color,
}

/// The ink tiers for content painted **directly onto the window card**
/// ([`Surfaces::window_card`]) — a hosted app that fills no background of
/// its own, a stub tile, the placeholder a foreign surface shows before its
/// first frame.
///
/// This exists because "what colour is text on the card" is not answerable
/// from the chrome's palette alone: the card is a surface in its own right,
/// and across the preset set it lands on both sides of the light/dark line
/// (`#F4F5F7` on the graphite default, black under high contrast, near-black
/// on OLED, white on the preserved concept palette). Every call site used to
/// answer it by hard-coding one ramp end, which is correct for exactly the
/// themes whose card matches that guess and renders the text invisible on
/// the rest. Derive it here, once, from the card's actual luminance.
#[derive(Clone, Copy, Debug)]
pub struct OnCard {
    /// Body / title ink. Clears WCAG AA (4.5:1) against the card.
    pub ink: Color,
    /// Secondary ink — sub-labels, captions, a gutter's line numbers.
    pub ink_dim: Color,
    /// Tertiary ink — the quietest legible tier (hints, comments).
    pub ink_tertiary: Color,
    /// A whisper-tinted surface *on* the card: a gutter strip, a zebra row.
    /// Always a small step from the card toward its own ink, so it reads as
    /// a tint in either direction instead of a light-theme assumption.
    pub tint: Color,
}

/// Minimum contrast the chrome's own ink must clear against the card before
/// it is reused there. WCAG 2.1 AA for body text.
const ON_CARD_MIN_CONTRAST: f64 = 4.5;

/// The floor to ask for when the text will be drawn at the smallest
/// size the type scale offers.
///
/// WCAG's 4.5:1 is stated for an ideal glyph. A real one is
/// anti-aliased, and at 12 px and below no pixel of a lowercase word
/// reaches full coverage — the strongest pixel of a label rendered from
/// an ink measured at 4.56:1 came out at 3.44:1, a whole tier under the
/// floor its colour was chosen to clear. Asking for AAA leaves enough
/// headroom that what lands on the screen still clears AA.
///
/// This is a property of the size the caller draws at, which is why it
/// is the caller that picks the floor rather than the derivation
/// guessing.
pub const ON_CARD_SMALL_TEXT_CONTRAST: f64 = 7.0;

/// How far a [`OnCard::tint`] surface is pulled from the card toward its ink.
const ON_CARD_TINT_T: f32 = 0.07;

/// The opaque colour an opaque `wash` painted at `alpha` over `ground`
/// leaves behind — source-over, resolved so it can be measured.
///
/// Contrast is a property of what reaches the screen, and a translucent
/// fill does not reach it alone. A caller that needs to know whether its
/// text will read on a washed row has to composite first; doing it here
/// means the two places that need the answer agree on it.
pub fn over(ground: Color, wash: Color, alpha: f32) -> Color {
    mix8(ground, wash, alpha)
}

/// Minimum contrast a shape that is not text — a pill's edge, a control's
/// boundary — must clear against what it is drawn on before it counts as
/// visible. WCAG 2.1 non-text contrast.
pub const NON_TEXT_MIN_CONTRAST: f64 = 3.0;

/// Minimum contrast a control's *label* must clear against the control's own
/// fill. WCAG 2.1 AA for body text — the same number as the card floor, named
/// separately because the ground is different: a button's label is measured
/// against the accent it is painted on, not against the surface behind it.
///
/// A label on a filled control is the one piece of text whose ground is not a
/// surface the palette authored for text. Accent and semantic fills are picked
/// to carry meaning at a glance, and a fill bright enough to do that is a fill
/// white text disappears into.
pub const ON_FILL_MIN_CONTRAST: f64 = 4.5;

/// How much accent an [`AccentChip`]'s body carries over its ground.
///
/// Low on purpose: the chip is a fact about a row, not a second selection,
/// and a saturated pill would out-shout the row highlight it sits inside.
/// The legibility of the label is bought by deriving the ink against the
/// composite this produces, not by making the body louder.
const ACCENT_CHIP_WASH: f32 = 0.18;

/// An accent-tinted chip — a small pill stating a fact about the row it
/// sits on — resolved against the ground it will actually be painted on.
///
/// Every field is an opaque colour rather than a token plus an alpha,
/// because a wash is not a colour until it has a ground: the same
/// `accent × 0.18` is one thing over a panel and another over a selected
/// row, and only the composite can be measured. Measuring the token
/// instead is how a chip ends up as accent ink on an accent wash on an
/// accent selection — three steps of one hue, none of them separated from
/// the next.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AccentChip {
    /// Opaque body of the pill: the accent wash already composited over
    /// the row's ground.
    pub fill: Color,
    /// The pill's edge. Clears [`NON_TEXT_MIN_CONTRAST`] against the
    /// **ground**, so the shape stays findable where the wash inside it is
    /// within a shade of the ground's own colour.
    pub border: Color,
    /// Label ink. Clears the caller's floor against [`Self::fill`].
    pub ink: Color,
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
    /// Translucent surface ≈ `white/5` (alpha 13 ≈ 0.05·255). Use for
    /// "barely there" hover veneers and faint dividers.
    pub surface_white_5: Color,
    /// Translucent surface ≈ `white/10` (alpha 26 ≈ 0.10·255).
    pub surface_white_10: Color,
    /// Translucent surface ≈ `white/20` (alpha 51 ≈ 0.20·255).
    pub surface_white_20: Color,
    /// Translucent border ≈ `border-white/10` (alpha 26).
    pub border_white_10: Color,
    /// Translucent border ≈ `border-white/20` (alpha 51).
    pub border_white_20: Color,
}

/// How many distinct colours [`Colors::categorical`] cycles through before
/// repeating. Six keeps neighbours 60° of hue apart, which is a plain
/// difference at a glance rather than a shade you have to compare.
pub const CATEGORICAL_LEN: usize = 6;

/// Total hue sweep of the categorical series, centred on the accent. Wide
/// enough to tell six workspaces apart, narrow enough that the last one is
/// still the same family as the first.
const CATEGORICAL_HUE_ARC_DEG: f32 = 70.0;

/// How far the series walks in lightness across its span. Carries most of
/// the separation, and unlike hue it survives a desaturated theme.
const CATEGORICAL_LIGHT_SPREAD: f32 = 0.13;

impl Colors {
    /// A categorical colour series grown from the theme's own accent.
    ///
    /// For things that are *different from each other* rather than
    /// ranked — workspaces, calendars, participants, tags. Index `0` is
    /// the accent itself; each subsequent index rotates the hue by a
    /// full share of the colour wheel while keeping the accent's
    /// saturation and lightness exactly.
    ///
    /// Holding saturation and lightness is what makes this a theme
    /// token rather than a fixed list. A theme with a restrained blue
    /// accent gets a restrained series; the high-contrast theme's vivid
    /// accent gets a vivid one that still reads on pure black; a light
    /// theme's accent keeps the series dark enough to see on paper. A
    /// hardcoded palette can be right for at most one of those, and the
    /// theme cannot reach it to fix the others.
    ///
    /// Hue is the axis that varies because it is the one that survives
    /// every ground: two colours of equal lightness are still obviously
    /// different colours, whereas a series built by walking a ramp's
    /// lightness collapses into "several similar greys" the moment the
    /// theme lowers its contrast.
    pub fn categorical(&self, idx: usize) -> Color {
        let base = self.accent[ACCENT_PRIMARY_IDX];
        let (h, s, l) = rgb_to_hsl(base);
        let i = idx % CATEGORICAL_LEN;
        // Fan around the accent, do not walk the wheel.
        //
        // Rotating a full 360/N per entry made every hue reachable, which
        // is exactly the problem: from a blue accent the second workspace
        // landed on purple and the third on pink — colours with nothing to
        // do with the theme, indistinguishable in kind from the hardcoded
        // palette this replaced. Sweeping a bounded arc keeps the whole
        // series recognisably one family, and the separation the eye
        // actually needs comes from lightness as much as hue.
        // Index 0 is the accent itself; the rest alternate outward from it
        // (+1, -1, +2, -2, …) so the first workspace a user sees is their
        // own accent and later ones fan symmetrically either side.
        let k = (i + 1) / 2;
        let sign = if i % 2 == 1 { 1.0 } else { -1.0 };
        let step = sign * k as f32 / (CATEGORICAL_LEN / 2) as f32; // -1 … +1
        let hue = (h + step * CATEGORICAL_HUE_ARC_DEG / 2.0).rem_euclid(360.0);
        let light = (l + step * CATEGORICAL_LIGHT_SPREAD).clamp(0.22, 0.78);
        hsl_to_rgb(hue, s, light)
    }
}

/// sRGB → HSL, hue in degrees. Straight sRGB rather than a perceptual
/// space: the point here is *separation*, and the cheap conversion
/// separates hues perfectly well.
fn rgb_to_hsl(c: Color) -> (f32, f32, f32) {
    let (r, g, b) = (
        c.r() as f32 / 255.0,
        c.g() as f32 / 255.0,
        c.b() as f32 / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d <= f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs()).max(f32::EPSILON);
    let h = if max == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h.rem_euclid(360.0), s.clamp(0.0, 1.0), l)
}

/// HSL → sRGB, the inverse of [`rgb_to_hsl`].
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> Color {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let q = |v: f32| (((v + m) * 255.0).round().clamp(0.0, 255.0)) as u8;
    Color::from_rgb8(q(r1), q(g1), q(b1))
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

        // The window ground is the theme's ground. A window whose app has
        // painted nothing then matches the window beside it whose app filled
        // `surfaces.base`, so moving between tabs does not flip the frame
        // between a light and a dark surface. (The OLED preset reached the
        // same conclusion by hand before this was derived — see its
        // `window_card` override.)
        let window_card = p.window_card.unwrap_or(if p.dark { p.base } else { Color::WHITE });
        let window_card_border = if p.dark {
            Color::from_rgba8(255, 255, 255, 51)
        } else {
            Color::from_rgba8(0, 0, 0, 30)
        };

        let scrim = if p.dark { mix8(p.base, black, 0.55) } else { Color::from_rgb8(12, 12, 14) };

        // Which way a translucent surface/border tier has to lean to be seen
        // at all. See the `surface_white_*` fields below.
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
            // A veneer has to move the surface it is laid on, and which way
            // that is depends on the palette. These tiers were white at every
            // strength on both sides of the light/dark line, and white over
            // `raised` — `#ffffff` on the light theme — is nothing at all:
            // the faint tier measured 1.01:1 there. That is why every
            // unselected filter chip lost its pill and read as bare text, and
            // why the collaborators Join button had no body. Wash toward the
            // contrasting extreme instead, so a tier means the same thing on
            // both sides. `window_card_border` above already flips for this
            // reason; these are the same decision made for the surfaces
            // rather than for the edge.
            //
            // Strength is unchanged, and deliberately so: these are quiet
            // tiers, not shapes a person has to find. A pill that must stay
            // findable takes an edge clearing [`NON_TEXT_MIN_CONTRAST`]
            // against its ground — see [`AccentChip::border`] — rather than a
            // louder body.
            surface_white_5:  with_a(veneer, 13),
            surface_white_10: with_a(veneer, 26),
            surface_white_20: with_a(veneer, 51),
            border_white_10:  with_a(veneer, 26),
            border_white_20:  with_a(veneer, 51),
        }
    }

    /// Ink for content painted directly on [`Surfaces::window_card`].
    ///
    /// Two cases, decided by measurement rather than by a `dark` flag:
    ///
    /// * The card is in the **same family as the chrome** (a dark card on a
    ///   dark theme, a white card on a light one). The chrome's own three
    ///   text tiers already read on it, so they are what a widget gets —
    ///   which keeps a hosted app's typography identical to the rest of the
    ///   UI instead of quietly forking into a second scale.
    /// * The card is in the **opposite family** (the preserved concept
    ///   palette's white card under a dark chrome). The chrome's near-white
    ///   ink would vanish, so ink comes from whichever end of the neutral
    ///   ramp actually contrasts with the card.
    ///
    /// Either way the result clears AA against the card, which is the
    /// property the three former copies of this logic did not have: each
    /// assumed a light card and derived dark ink from it, so under the
    /// high-contrast preset (black card) and OLED (`#0A0A0A` card) they
    /// painted dark-on-dark and the text disappeared.
    pub fn on_card(&self) -> OnCard {
        self.on_surface(self.surfaces.window_card)
    }

    /// [`Self::on_card`] for any surface a widget actually paints on.
    ///
    /// The window card is not the only ground content lands on: a document's
    /// paper is deliberately light in every theme, a placeholder box is a tint
    /// of that paper, a chart plots on its own panel. Each of those is a
    /// different background, and ink that clears AA on the card can sit at
    /// 4.4:1 on a surface a few percent away from it — which is where a
    /// hand-derived tone lands when it is computed by darkening the
    /// *background* by a fixed fraction instead of being measured against it.
    /// A fixed fraction produces roughly the same ratio whatever the palette,
    /// so the theme that exists to raise contrast does not.
    ///
    /// So the surface is a parameter and the ratio is checked, both tiers of
    /// it: `ink` **and** `ink_dim` have to clear
    /// [`ON_CARD_MIN_CONTRAST`] before the chrome's own tiers are handed
    /// back. `text_primary` clearing says nothing about `text_dim`, and
    /// `ink_dim` is the tier a caption or a placeholder label uses — the very
    /// text most likely to be the only explanation on screen.
    pub fn on_surface(&self, surface: Color) -> OnCard {
        self.on_surface_at(surface, ON_CARD_MIN_CONTRAST)
    }

    /// [`Colors::on_surface`] against a caller-chosen contrast floor.
    ///
    /// The floor is not a property of the palette or of the surface —
    /// it is a property of how the text will be drawn. A caption at the
    /// bottom of the type scale needs more headroom than body text to
    /// arrive at the same legibility, because anti-aliasing spends some
    /// of it (see [`ON_CARD_SMALL_TEXT_CONTRAST`]). So a caller that
    /// knows it is drawing small says so, and gets ink derived for that
    /// rather than a shade picked by hand at the call site — which is
    /// the version that reads correctly until the next palette edit.
    pub fn on_surface_at(&self, surface: Color, min_ratio: f64) -> OnCard {
        use crate::a11y::contrast_ratio;

        let chrome_tiers_read = contrast_ratio(self.text_primary, surface) >= min_ratio
            && contrast_ratio(self.text_dim, surface) >= min_ratio;

        let (ink, ink_dim, ink_tertiary) = if chrome_tiers_read {
            (self.text_primary, self.text_dim, self.text_tertiary)
        } else {
            // One step in from the ramp end, not the end itself: pure
            // black on white (or pure white on black) is harsher than
            // the typography the rest of the UI uses.
            let from_low = contrast_ratio(self.neutral[0], surface)
                >= contrast_ratio(self.neutral[10], surface);
            if from_low {
                (self.neutral[1], self.neutral[4], self.neutral[4])
            } else {
                (self.neutral[9], self.neutral[6], self.neutral[6])
            }
        };

        // The ramp is the palette's own, so its inner steps are not guaranteed
        // to clear against an arbitrary surface — a mid-grey placeholder under
        // a palette whose ramp is shallow can leave `neutral[4]` short. Walk
        // outward to the end that contrasts until the floor is met, rather
        // than returning a tier that reads on the card this was written for
        // and nowhere else.
        let ink = self.step_out_until_legible(ink, surface, min_ratio);
        let ink_dim = self.step_out_until_legible(ink_dim, surface, min_ratio);

        OnCard {
            ink,
            ink_dim,
            ink_tertiary,
            tint: mix8(surface, ink, ON_CARD_TINT_T),
        }
    }

    /// An authored `tone` made legible on `ground`: returned untouched when
    /// it already clears `min_ratio` there, and otherwise the smallest step
    /// from it toward the contrasting extreme that does.
    ///
    /// [`Self::on_surface_at`] answers "what ink do the *chrome's tiers*
    /// become here". This answers the narrower question a widget asks when
    /// it has already chosen a tone for a reason — an accent, a dim
    /// secondary — and needs that tone to survive a ground the palette did
    /// not have in mind. Same walk, so a caller that needs one colour does
    /// not have to take a tier set it will discard, and does not reach for
    /// a hand-picked shade instead.
    pub fn ink_on(&self, ground: Color, tone: Color, min_ratio: f64) -> Color {
        self.step_out_until_legible(tone, ground, min_ratio)
    }

    /// One `tone`, made legible on *every* ground in `grounds` at once.
    ///
    /// [`Self::ink_on`] answers for a single ground. A control asks this
    /// instead, because a control is not one ground: a button's fill moves
    /// between rest, hover and press, and resolving the label separately
    /// against each lets it cross the floor somewhere in the middle of that
    /// range and flip polarity under the pointer. A label that turns from
    /// white to near-black on hover reads as a fault however well each
    /// individual frame measures, so the ink is chosen once, against the
    /// ground it reads worst on, and then held for the whole set.
    ///
    /// Returns `tone` untouched when it already clears everywhere, so a
    /// palette that authored a legible fill keeps the colour it chose.
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
        // Polarity is decided against the hardest ground rather than
        // per-ground: the states of one control sit within a shade of each
        // other, so the ground that defeats the tone is the one that
        // decides which way there is room to walk.
        let hardest = grounds
            .iter()
            .copied()
            .min_by(|a, b| {
                contrast_ratio(tone, *a)
                    .partial_cmp(&contrast_ratio(tone, *b))
                    .unwrap_or(core::cmp::Ordering::Equal)
            })
            .expect("non-empty");
        let black = Color::from_rgb8(0, 0, 0);
        let white = Color::from_rgb8(255, 255, 255);
        let target = if contrast_ratio(black, hardest) >= contrast_ratio(white, hardest) {
            black
        } else {
            white
        };
        for step in 1..=16 {
            let candidate = mix8(tone, target, step as f32 / 16.0);
            if worst(candidate) >= min_ratio {
                return candidate;
            }
        }
        target
    }

    /// The ink for anything drawn *on* the accent-primary fill — a selected
    /// row, a highlighted menu item, a chosen day in a calendar.
    ///
    /// The accent is the one fill that shows up under a label all over the
    /// product, and every widget that used it reached for the same literal
    /// white. That is right on a dark accent and wrong on a bright one, and
    /// which of the two a palette ships is not something a widget can know.
    /// Asking here means the answer is the same everywhere it is asked, so a
    /// selected row and a primary button do not disagree inside one frame.
    pub fn on_accent(&self, min_ratio: f64) -> Color {
        self.ink_on_any(&[self.accent[ACCENT_PRIMARY_IDX]], Color::WHITE, min_ratio)
    }

    /// An [`AccentChip`] resolved for `ground`, with its label clearing
    /// `min_ratio` against the pill's own body.
    ///
    /// The order matters and is the whole point. The body is composited
    /// **first**, so the ink is measured against the colour the screen will
    /// hold rather than against the accent token the wash was made from —
    /// the mistake that leaves a chip at 2.4:1 while every value in it came
    /// from the theme. The ink then walks out of the accent only as far as
    /// the floor demands, so the pill keeps its hue while its label stops
    /// sharing it. The edge is measured against the *ground* instead of the
    /// body, because an edge's job is to separate the pill from the row,
    /// and over a selected row the body alone barely does.
    ///
    /// `min_ratio` belongs to the caller for the reason
    /// [`Self::on_surface_at`]'s does: a chip drawn at the bottom of the
    /// type scale needs the headroom anti-aliasing will spend (see
    /// [`ON_CARD_SMALL_TEXT_CONTRAST`]), and the derivation cannot know the
    /// size it is being drawn at.
    pub fn accent_chip_on(&self, ground: Color, min_ratio: f64) -> AccentChip {
        self.tinted_chip_on(
            ground,
            self.accent[ACCENT_PRIMARY_IDX],
            ACCENT_CHIP_WASH,
            min_ratio,
        )
    }

    /// The same resolution as [`Self::accent_chip_on`], for a chip carrying
    /// any `tint` at any `wash` strength — a danger-tinted button, a
    /// semantic status pill, a tray glyph on a tinted tray.
    ///
    /// This exists because the accent is not the only tint a body gets made
    /// from, and the failure it prevents is not about the accent at all. A
    /// label drawn in the *same* tint as its own wash of that tint cannot
    /// reach a text floor at any wash strength: both sides of the ratio move
    /// together, so the fill and the label stay a fixed fraction apart no
    /// matter how the theme is tuned. Every hand-rolled `tint × 0.20` fill
    /// with a `tint` label in the shell measured between 2.6:1 and 3.6:1 for
    /// exactly that reason, and no palette change could have lifted them.
    ///
    /// So the ink is derived against the composited body rather than against
    /// the tint the body was mixed from, and walks out of the tint only as
    /// far as `min_ratio` demands — the pill keeps its hue, its label stops
    /// sharing it. `min_ratio` belongs to the caller because the headroom
    /// anti-aliasing spends is a property of the size being drawn at; see
    /// [`ON_CARD_SMALL_TEXT_CONTRAST`].
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

    /// Darken or lighten `ink` toward whichever extreme contrasts with
    /// `surface` until it clears `min_ratio`.
    ///
    /// Returns `ink` untouched when it already clears, so a palette's authored
    /// tone is preferred and only adjusted when it would not be readable. The
    /// walk is bounded and ends at the extreme, which always clears against
    /// any surface it was chosen for.
    fn step_out_until_legible(&self, ink: Color, surface: Color, min_ratio: f64) -> Color {
        use crate::a11y::contrast_ratio;
        if contrast_ratio(ink, surface) >= min_ratio {
            return ink;
        }
        let black = Color::from_rgb8(0, 0, 0);
        let white = Color::from_rgb8(255, 255, 255);
        let target = if contrast_ratio(black, surface) >= contrast_ratio(white, surface) {
            black
        } else {
            white
        };
        // Sixteenths: fine enough that the result stays close to the authored
        // tone, coarse enough to be a handful of comparisons rather than a
        // search.
        for step in 1..=16 {
            let candidate = mix8(ink, target, step as f32 / 16.0);
            if contrast_ratio(candidate, surface) >= min_ratio {
                return candidate;
            }
        }
        target
    }

    /// A hairline that stays visible on `ground`, whichever side of the
    /// light/dark line `ground` happens to fall on.
    ///
    /// White at a low alpha is the usual way to draw a quiet edge, and it is
    /// what every `*_white_*` token in this file assumes. That assumption
    /// holds only over a dark ground; over a light one the same wash
    /// composites to within a shade of the surface and the edge disappears.
    /// Branching on the palette's `dark` flag would not fix it either — a
    /// preset can be dark overall while an individual surface sits the other
    /// way round, which is exactly how the concept palette ends up with a
    /// white card under dark chrome. So try both polarities at `alpha` and
    /// keep whichever moves `ground` further.
    pub fn hairline_on(ground: Color, alpha: u8) -> Color {
        use crate::a11y::relative_luminance;
        let g = relative_luminance(ground);
        let light = Color::from_rgba8(255, 255, 255, alpha);
        let dark = Color::from_rgba8(0, 0, 0, alpha);
        let moved = |c: Color| (relative_luminance(composite(c, ground)) - g).abs();
        if moved(light) >= moved(dark) { light } else { dark }
    }

    /// The hairline that separates a window from the desktop behind it.
    ///
    /// This is deliberately **not** a property of the window card. The edge is
    /// painted on the desktop, outside the card's silhouette, because the
    /// pixels inside the card belong to the hosted app and the shell never
    /// samples them: a constant stroked over app content lands on whatever the
    /// app happened to paint, so one token reads as a soft rim light over a
    /// dark app and as a glaring near-white line over one painting light
    /// chrome. No alpha satisfies both — the two grounds are at opposite ends
    /// of the ramp. Deriving the edge from the one ground the shell *does*
    /// know, and keeping it off the app's pixels entirely, makes the edge
    /// legible without the shell having to guess what is underneath it.
    /// [`crate::widget::tile_host::TileHost`] owns the matching geometry.
    ///
    /// A preset's declared [`Surfaces::window_card_border`] is honoured
    /// wherever it actually registers against that ground; only a declaration
    /// the ground would swallow gets its polarity flipped.
    /// The body a control takes when it is **lifted** off the chrome's own
    /// ground — a hovered or active tab pill, a selected row. `t` runs from
    /// `0.0` (flat on the ground) to `1.0` (the full lift).
    ///
    /// The concept expressed this as `bg-white/5` / `bg-white/10`, which only
    /// performs the lift over a dark ground: composited over a light one the
    /// same wash lands within a shade of the surface, so "active" stops
    /// looking active. `surfaces.base` and `surfaces.raised` already name both
    /// ends of the move — the ground, and the tier of cards and panels above
    /// it — and every preset defines them on its own side of the light/dark
    /// line, so interpolating between them keeps the lift pointing the right
    /// way with no `dark` flag to get wrong.
    ///
    /// Some presets have no tier to aim at: high contrast paints every surface
    /// pure black on purpose, so `raised` *is* `base`. Those fall back to a
    /// wash whose polarity is chosen against the ground the same way
    /// [`Colors::hairline_on`] chooses an edge, which keeps a lift available
    /// even where the surface family is deliberately flat.
    pub fn lift(&self, t: f32) -> Color {
        mix8(self.surfaces.base, self.lift_target(), t)
    }

    /// The colour a lift moves *toward* — the preset's raised tier where it
    /// registers against the ground, and a derived step where it does not.
    ///
    /// Named separately from [`Colors::lift`] because it is also the anchor
    /// for [`Colors::glass_film`]: a material and a flat wash have to agree
    /// on which direction "away from the ground" is, or a glass pill and a
    /// solid one lift opposite ways on the same theme.
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

    /// Whether this palette's surfaces lift by getting *brighter*.
    ///
    /// Every other derivation in this file is careful to pick its polarity
    /// per ground rather than off the preset's `dark` flag, and this is the
    /// same question asked once so a material can orient itself: a lit
    /// surface is brighter where the light falls, and "brighter" is a
    /// different direction on a paper-white desktop than on a graphite one.
    pub fn lifts_bright(&self) -> bool {
        use crate::a11y::relative_luminance;
        let base = self.surfaces.base;
        relative_luminance(self.lift_target()) >= relative_luminance(base)
    }

    /// The colour of the film a glass surface is made of.
    ///
    /// **Not white.** `surface_white_*` is this idea written down as a
    /// literal, and it only works over a dark ground: over a light one the
    /// same wash lands within a shade of the surface and the body vanishes.
    /// Real glass takes its colour from what it is made of and lets the
    /// ground show through its alpha, so the film here is the theme's own
    /// raised tier carried one derived step further out
    /// ([`GLASS_FILM_LIFT`]) — opaque, because the alpha that makes it a
    /// film belongs to the state painting it, not to the material.
    ///
    /// The step is taken along the direction the palette's own surfaces
    /// lift, which is **not** the same question [`Colors::hairline_on`]
    /// answers. That one picks whichever polarity moves the ground more,
    /// which is what an edge wants — it only has to be seen. A film has to
    /// agree with the cards and panels around it about which way "up" is,
    /// and on a paper palette those go *brighter* (a white card on a paper
    /// ground) while the polarity that moves that ground more is black.
    /// Deriving the film from the edge rule gave a light theme a body that
    /// darkened while every other raised surface lightened, and landed it
    /// within a level of the bar it was drawn on into the bargain.
    pub fn glass_film(&self) -> Color {
        let step = if self.lifts_bright() { Color::WHITE } else { Color::BLACK };
        composite(with_a(step, GLASS_FILM_LIFT), self.lift_target())
    }

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

    /// The series has to be *legible as different colours* in every
    /// theme, which is the whole reason it is derived instead of fixed.
    /// Checks the two properties that carry that: adjacent entries are
    /// far apart in hue, and every entry keeps enough contrast against
    /// the theme's own ground to be seen at all.
    #[test]
    fn categorical_series_stays_distinguishable_in_every_theme() {
        use crate::theme::Theme;

        let hue = |c: Color| super::rgb_to_hsl(c).0;
        let rel_luma = |c: Color| {
            let f = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
        };

        for (name, theme) in [
            ("dark", Theme::dark()),
            ("light", Theme::light()),
            ("high_contrast", Theme::high_contrast()),
        ] {
            let c = &theme.tokens.colors;
            // Index 0 is the theme's own accent, untouched.
            assert_eq!(
                c.categorical(0),
                c.accent[ACCENT_PRIMARY_IDX],
                "{name}: the series must start from the theme's accent"
            );
            // It cycles rather than running off the end.
            assert_eq!(
                c.categorical(CATEGORICAL_LEN),
                c.categorical(0),
                "{name}: the series must wrap at CATEGORICAL_LEN"
            );

            for i in 0..CATEGORICAL_LEN {
                let a = c.categorical(i);
                let b = c.categorical((i + 1) % CATEGORICAL_LEN);
                // Distinguishable — by whatever combination of hue and
                // lightness the theme affords, not by hue alone. Hue-only
                // separation is what drove the series around the whole
                // wheel and produced colours foreign to the theme.
                let d = |x: Color, y: Color| {
                    let (dr, dg, db) = (
                        x.r() as f32 - y.r() as f32,
                        x.g() as f32 - y.g() as f32,
                        x.b() as f32 - y.b() as f32,
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

                // …and still this theme's family. This is the property the
                // old hue-only rule gave up: it let the series reach any
                // colour at all, which is how a blue accent produced a
                // purple workspace.
                let dh_acc = (hue(a) - hue(c.accent[ACCENT_PRIMARY_IDX])).abs();
                let from_accent = dh_acc.min(360.0 - dh_acc);
                assert!(
                    from_accent <= CATEGORICAL_HUE_ARC_DEG / 2.0 + 1.0,
                    "{name}: workspace colour {i} sits {from_accent:.0}° from the \
                     accent — outside the theme's own family"
                );

                // Visible against the ground the theme actually paints.
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

    /// The HSL round-trip has to be tight, or a colour changes every
    /// time it passes through the derivation.
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
                    (a as i32 - b as i32).abs() <= 1,
                    "round-trip drifted: {c:?} -> {back:?}"
                );
            }
        }
    }
}
