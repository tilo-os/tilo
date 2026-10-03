//! The colour half of the design language (`docs/design-language.md`).
//!
//! The interface is a quiet, near-colourless **neutral material**: every
//! surface and every ink tier is an OKLCH lightness at one shared hue with
//! chroma at or below [`NEUTRAL_MAX_CHROMA`]. Changing that hue (a theme's
//! or a workspace's temperature, [`Colors::with_neutral_hue`]) never moves
//! contrast, because lightness is what contrast is made of.
//!
//! Colour means that someone is there or that something is happening:
//!
//! - [`PresencePalette`]: twelve hues at equal lightness and chroma, one per
//!   participant, so nobody looks more important than anybody else;
//! - [`Intents::you`]: the local person's own presence colour, which is
//!   their focus ring, caret and text-selection tint;
//! - [`SemanticColors`]: muted status (danger, warning, success, info).
//!
//! There is no brand accent. A primary action is an *inverted* control
//! ([`Intents::inverted`] under [`Intents::on_inverted`]); a selection is a
//! *lift* ([`Intents::selection`]). A theme whose [`Emphasis`] is tinted
//! gives those two one hue; nothing else gains colour.
//!
//! Every colour here is derived from a [`Palette`] (what a theme document
//! says) in [`Colors::as_authored`]; [`Colors::of_palette`] then repairs any
//! pairing that misses a contrast floor (`super::contrast`).

use crate::color::{Oklch, oklch};
use crate::color_compat::ColorCompat;
use crate::paint::with_alpha;
use vello::peniko::Color;

pub use super::palette::{
    DEFAULT_NEUTRAL_HUE, Emphasis, NEUTRAL_MAX_CHROMA, Palette, STATUS_HUES, STATUS_MAX_CHROMA,
    TINTED_LIFT_CHROMA, Tone,
};

/// How far [`Colors::legible_fill`] may deepen a filled chip toward black
/// before it gives up and re-picks the label instead.
pub const MAX_CHIP_DEEPEN: f32 = 0.35;

pub const PERMISSION_DENIED_SCRIM: Color = Color::from_rgba8(0, 0, 0, 77);

/// Straight (non-premultiplied) 8-bit sRGB mix — the space most widget
/// draw paths blend in.
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

/// Smallest OKLCH lightness step that separates a window region (sidebar,
/// inspector) from the content ground by tone alone. A region tier closer
/// to content than this draws a hairline on the edge they share instead
/// (`surface::region`).
pub const REGION_MIN_STEP: f64 = 0.015;

/// Fallback wash alpha for [`Colors::lift`] when a material has no raised
/// tier to aim at. 15% — reads as a distinct body without blowing out
/// contrast.
const LIFT_FALLBACK_ALPHA: u8 = 38;

/// How far [`Colors::glass_film`] carries the raised tier past itself, so a
/// translucent film (which gives back strength through its own alpha) still
/// lands above an opaque surface instead of under it.
const GLASS_FILM_LIFT: u8 = 46;

/// Stock opacity of the floating-chrome material over its blurred backdrop:
/// the compositor blurs what is behind a panel and tints it with
/// [`Surfaces::overlay`] at 72 %. A theme sets its own
/// ([`Surfaces::material_opacity`]).
pub const MATERIAL_ALPHA: f32 = 0.72;

/// Stock backdrop blur of the floating material, in logical pixels.
pub const MATERIAL_BLUR_PX: f32 = 24.0;

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

    /// An 11-stop ramp of the neutral material from lightness `from` (index
    /// 0) to `to` (index 10), eased so low indices cluster near `from`
    /// (`powf(1.5)`): `neutral[2..5]` (hover, pressed, muted tints) stay
    /// close to the ground instead of racing toward the far end.
    pub fn neutral(from: f32, to: f32, chroma: f32, hue: f32) -> Self {
        let mut stops = [Color::BLACK; 11];
        for (i, stop) in stops.iter_mut().enumerate() {
            #[allow(
                clippy::cast_precision_loss,
                reason = "i is a 0..11 index into a fixed 11-element array, far below \
                          where f32 loses integer precision"
            )]
            let t = (i as f32 / 10.0).powf(1.5);
            *stop = oklch(from + (to - from) * t, chroma, hue);
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

/// The neutral material's surfaces. Every one is opaque except
/// [`Self::material`], and every one shares [`Colors::neutral_hue`].
#[derive(Clone, Debug)]
pub struct Surfaces {
    /// The desktop ground behind tiles.
    pub base: Color,
    /// The window ground: every app's background, owned by the framework.
    /// The ink tiers ([`Colors::text_primary`] and friends) are defined on
    /// this surface.
    pub content: Color,
    /// The navigation region of a window.
    pub sidebar: Color,
    /// The detail / inspector region of a window.
    pub inspector: Color,
    /// Fields, wells and objects inside content.
    pub raised: Color,
    /// Floating chrome: menus, popovers, the palette, sheets. Opaque; the
    /// blurred form is [`Self::material`].
    pub overlay: Color,
    /// Opaque dim behind a modal. Strength belongs to the thing being
    /// lifted, hue belongs to the material, so the scrim recedes into the
    /// neutral family rather than punching a black hole through it.
    pub scrim: Color,
    /// The blurred material floating chrome is made of: [`Self::overlay`] at
    /// [`Self::material_opacity`]. Only for a surface the compositor blurs
    /// behind; without a backdrop blur, paint the opaque [`Self::overlay`]
    /// instead — unblurred translucency is never correct.
    pub material: Color,
    /// Backdrop blur radius of [`Self::material`], in logical pixels. `0`
    /// keeps floating chrome opaque.
    pub material_blur_px: f32,
    /// Opacity of the [`Self::overlay`] tint over the blurred backdrop.
    pub material_opacity: f32,
    /// The declared edge between a window tile and the desktop. Read it
    /// through [`Colors::window_edge`], which flips its polarity if the
    /// ground would swallow it.
    pub content_edge: Color,
}

/// Ink tiers for content painted onto a surface, resolved against that
/// surface's actual luminance ([`Colors::on_surface`]).
#[derive(Clone, Copy, Debug)]
pub struct OnSurface {
    /// Body / title ink. Clears WCAG AA (4.5:1) against the surface.
    pub ink: Color,
    pub ink_dim: Color,
    pub ink_tertiary: Color,
    /// Whisper-tinted surface *on* the surface (gutter strip, zebra row):
    /// stepped toward the surface's own ink so it works either direction.
    pub tint: Color,
}

/// WCAG 2.1 AA for body text.
const BODY_MIN_CONTRAST: f64 = 4.5;

/// Floor for text at the bottom of the type scale. A real anti-aliased
/// glyph at 12px and below doesn't reach WCAG's ideal-glyph 4.5:1 — a label
/// measured at 4.56:1 landed at 3.44:1 on screen. AAA leaves headroom to
/// still clear AA once rendered.
pub const SMALL_TEXT_MIN_CONTRAST: f64 = 7.0;

const ON_SURFACE_TINT_T: f32 = 0.07;

/// WCAG 2.1 non-text contrast (a pill's edge, a toggle's knob).
pub const NON_TEXT_MIN_CONTRAST: f64 = 3.0;

/// Minimum contrast a control's *label* must clear against the control's own
/// fill (not the surface behind it).
pub const ON_FILL_MIN_CONTRAST: f64 = 4.5;

/// The opaque colour an opaque `wash` painted at `alpha` over `ground`
/// leaves behind — contrast is a property of what reaches the screen, so a
/// caller checking legibility on a washed row has to composite first.
pub fn over(ground: Color, wash: Color, alpha: f32) -> Color {
    mix8(ground, wash, alpha)
}

/// A tinted pill (a status badge, a presence chip), resolved against the
/// ground it paints on. Every field is opaque rather than a token plus
/// alpha — measuring the token instead of the composite is how a chip ends
/// up as tinted ink on a tinted wash.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TintedChip {
    /// Opaque body: the wash already composited over the row's ground.
    pub fill: Color,
    /// Pill edge, clearing [`NON_TEXT_MIN_CONTRAST`] against the **ground**
    /// so the shape stays findable when the wash inside is within a shade
    /// of the ground.
    pub border: Color,
    pub ink: Color,
}

/// Status colours. Muted (chroma at most [`STATUS_MAX_CHROMA`]) and used
/// only for status: never as decoration, never as a brand.
#[derive(Clone, Debug)]
pub struct SemanticColors {
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    /// Something is happening that is neither good nor bad: a sync in
    /// flight, an informational notice.
    pub info: Color,
}

/// What controls paint with, by intent rather than by hue.
#[derive(Clone, Copy, Debug)]
pub struct Intents {
    /// Fill of a primary action, a switch that is on, a checked box, a
    /// slider's filled track. Under [`Emphasis::Inverted`] (the default) it
    /// is primary ink, inverted onto the control; under
    /// [`Emphasis::Tinted`] it is the theme's one tint.
    pub inverted: Color,
    /// [`Self::inverted`] under the pointer or while pressed: one step back
    /// toward the content surface.
    pub inverted_hover: Color,
    /// Label and glyph on an [`Self::inverted`] fill: the content surface.
    pub on_inverted: Color,
    /// A selected row, tab or segment on the content surface: one surface
    /// step, colourless unless the emphasis is tinted. Other grounds lift
    /// with [`Colors::lift_on`].
    pub selection: Color,
    /// Halfway to [`Self::selection`]: an unselected row under the pointer.
    pub selection_hover: Color,
    /// The local person's presence colour: their focus ring, text caret and
    /// the colour collaborators see their cursor in.
    pub you: Color,
    /// Text-selection wash: [`Self::you`] at low alpha, painted under the
    /// selected glyphs.
    pub selection_tint: Color,
}

/// How many hues the presence palette has.
pub const PRESENCE_LEN: usize = 12;

/// The presence palette's hues, in OKLCH degrees: twelve evenly spaced, so
/// neighbours sit 30° apart.
pub const PRESENCE_HUES: [f32; PRESENCE_LEN] = [
    25.0, 55.0, 85.0, 115.0, 145.0, 175.0, 205.0, 235.0, 265.0, 295.0, 325.0, 355.0,
];

/// One of the twelve presence hues: a person's colour, as they choose it.
///
/// A colour is part of someone's identity, not of a theme: it is stored as
/// this choice and painted through whatever theme each viewer uses
/// ([`PresencePalette::hue`]), so it reads at that theme's presence
/// lightness and chroma everywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceHue {
    Red,
    Orange,
    Amber,
    Olive,
    Green,
    Teal,
    Cyan,
    Sky,
    Blue,
    Violet,
    Magenta,
    Pink,
}

impl PresenceHue {
    /// Every hue, in [`PRESENCE_HUES`] order.
    pub const ALL: [Self; PRESENCE_LEN] = [
        Self::Red,
        Self::Orange,
        Self::Amber,
        Self::Olive,
        Self::Green,
        Self::Teal,
        Self::Cyan,
        Self::Sky,
        Self::Blue,
        Self::Violet,
        Self::Magenta,
        Self::Pink,
    ];

    /// The palette slot, `0..PRESENCE_LEN`.
    #[must_use]
    pub fn slot(self) -> usize {
        Self::ALL.iter().position(|h| *h == self).unwrap_or(0)
    }

    /// The hue at `slot`, wrapping past [`PRESENCE_LEN`].
    #[must_use]
    pub fn from_slot(slot: usize) -> Self {
        Self::ALL
            .get(slot % PRESENCE_LEN)
            .copied()
            .unwrap_or(Self::Blue)
    }

    /// The OKLCH hue, in degrees.
    #[must_use]
    pub fn degrees(self) -> f32 {
        PRESENCE_HUES.get(self.slot()).copied().unwrap_or(265.0)
    }

    /// The stored name (`"violet"`), as settings and actions spell it.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Orange => "orange",
            Self::Amber => "amber",
            Self::Olive => "olive",
            Self::Green => "green",
            Self::Teal => "teal",
            Self::Cyan => "cyan",
            Self::Sky => "sky",
            Self::Blue => "blue",
            Self::Violet => "violet",
            Self::Magenta => "magenta",
            Self::Pink => "pink",
        }
    }

    /// The name a person reads in a picker.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Red => "Red",
            Self::Orange => "Orange",
            Self::Amber => "Amber",
            Self::Olive => "Olive",
            Self::Green => "Green",
            Self::Teal => "Teal",
            Self::Cyan => "Cyan",
            Self::Sky => "Sky",
            Self::Blue => "Blue",
            Self::Violet => "Violet",
            Self::Magenta => "Magenta",
            Self::Pink => "Pink",
        }
    }

    /// Parse [`Self::id`].
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|h| h.id() == id)
    }

    /// The hue nearest to `colour`'s, for a colour that arrives from outside
    /// (a host's accent). A grey has no hue and gives `None`.
    #[must_use]
    pub fn nearest(colour: Color) -> Option<Self> {
        let source = Oklch::from_color(colour);
        if source.c < 0.02 {
            return None;
        }
        let distance = |h: Self| {
            let d = (h.degrees() - source.h).rem_euclid(360.0);
            d.min(360.0 - d)
        };
        Self::ALL
            .into_iter()
            .min_by(|a, b| distance(*a).total_cmp(&distance(*b)))
    }

    /// The colour someone wears until they choose one: a pure function of
    /// their identity, so every host paints them the same without asking.
    /// The fixture names `me`, `alice`, `bob` and `charlie` are pinned so
    /// demos and tests stay recognisable.
    #[must_use]
    pub fn of_identity(id: &str) -> Self {
        match id {
            "me" => Self::Blue,
            "alice" => Self::Pink,
            "bob" => Self::Orange,
            "charlie" => Self::Green,
            _ => {
                // FNV-1a 64-bit: cheap, deterministic, no RNG state.
                let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
                for byte in id.as_bytes() {
                    hash ^= u64::from(*byte);
                    hash = hash.wrapping_mul(0x0100_0000_01b3);
                }
                let slot = hash.checked_rem(PRESENCE_LEN as u64).unwrap_or(0);
                Self::from_slot(usize::try_from(slot).unwrap_or(0))
            }
        }
    }

    /// The person's stored choice in a settings document
    /// (`identity.colour`), if they made one.
    #[must_use]
    pub fn chosen_in(settings: &serde_json::Value) -> Option<Self> {
        settings
            .get("identity")?
            .get("colour")?
            .as_str()
            .and_then(Self::from_id)
    }
}

/// One colour per participant: [`PRESENCE_HUES`] at one OKLCH lightness and
/// chroma, so they differ in hue only and no participant looks more
/// important than another. Where sRGB cannot carry the full chroma at a hue
/// (a light cyan) only that hue's chroma drops; lightness never does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PresencePalette {
    pub lightness: f32,
    pub chroma: f32,
    pub colors: [Color; PRESENCE_LEN],
}

impl PresencePalette {
    #[must_use]
    pub fn new(lightness: f32, chroma: f32) -> Self {
        Self {
            lightness,
            chroma,
            colors: PRESENCE_HUES.map(|h| oklch(lightness, chroma, h)),
        }
    }

    /// Slot `slot`, wrapping past [`PRESENCE_LEN`].
    #[must_use]
    pub fn get(&self, slot: usize) -> Color {
        self.colors
            .get(slot % PRESENCE_LEN)
            .copied()
            .unwrap_or(Color::BLACK)
    }

    /// The `index`th colour of a data series (chart categories, tag
    /// colours): the same twelve hues, walked so consecutive series sit about
    /// 150° apart instead of 30°, and wrapping past [`PRESENCE_LEN`].
    /// Equal lightness and chroma keep every series equally loud.
    #[must_use]
    pub fn series(&self, index: usize) -> Color {
        const WALK: [usize; PRESENCE_LEN] = [8, 1, 4, 10, 6, 0, 3, 9, 5, 11, 2, 7];
        let slot = WALK.get(index % PRESENCE_LEN).copied().unwrap_or(0);
        self.get(slot)
    }

    /// A person's colour as this palette paints it.
    #[must_use]
    pub fn hue(&self, hue: PresenceHue) -> Color {
        self.get(hue.slot())
    }

    /// An identity colour (a roster choice, a hashed id, an avatar colour)
    /// as this palette paints it: its hue at the palette's lightness and
    /// chroma. Identities travel between devices and themes as colours;
    /// painting them through here keeps every participant equal on every
    /// ground. A grey identity has no hue and takes [`PresenceHue::Blue`].
    #[must_use]
    pub fn of(&self, identity: Color) -> Color {
        let source = Oklch::from_color(identity);
        if source.c < 0.02 {
            return self.hue(PresenceHue::Blue);
        }
        oklch(self.lightness, self.chroma, source.h)
    }

    /// [`Self::of`] for the `(r, g, b)` tuples presence identities are
    /// stored as.
    #[must_use]
    pub fn of_rgb(&self, (r, g, b): (u8, u8, u8)) -> Color {
        self.of(Color::from_rgb8(r, g, b))
    }
}

#[derive(Clone, Debug)]
pub struct Colors {
    /// The palette these colours were made from.
    pub palette: Palette,
    /// How primary actions, selection and "on" states show.
    pub emphasis: Emphasis,
    /// The local person's own colour.
    pub you: PresenceHue,
    pub neutral: ColorRamp,
    pub semantic: SemanticColors,
    pub surfaces: Surfaces,
    pub intent: Intents,
    pub presence: PresencePalette,
    /// Primary ink on [`Surfaces::content`].
    pub text_primary: Color,
    /// Secondary ink on [`Surfaces::content`].
    pub text_dim: Color,
    /// Tertiary ink: still clears large-text contrast against every
    /// surface, so it reads as "quiet", never "broken".
    pub text_tertiary: Color,
    /// A hairline where two equal surfaces meet. Nowhere else: groups are
    /// separated by space.
    pub border_standard: Color,
    /// The focus ring: always [`Intents::you`].
    pub border_focus: Color,

    // --- state veneers ----------------------------------------------------
    //
    // The translucent washes widgets paint over whatever surface they sit on
    // to signal hover / press. They are **directional**: a veneer has to
    // *lighten* a dark ground and *darken* a light one, otherwise it
    // vanishes. The three alpha tiers are chosen so the perceptual step (ΔL*)
    // of each tier matches across light and dark, rather than the raw alpha
    // matching.
    /// The quietest veneer — a "barely there" hover wash or faint divider.
    pub overlay_subtle: Color,
    /// The middle veneer — a settled hover, a resting chip.
    pub overlay_medium: Color,
    /// The strongest veneer — pressed states and emphatic fills.
    pub overlay_strong: Color,
    /// Hairline structure that has to read against the local surface rather
    /// than against the theme ground (a divider inside a tinted panel).
    pub overlay_border_subtle: Color,
    /// The more assertive hairline — an active tab's outline, a focused chip.
    pub overlay_border_strong: Color,
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
/// sits on the lightness curve. Black at 15 / 31 / 60 reproduces the dark
/// tiers' ΔL* (≈ 5 / 10.5 / 21) to within two units on the light palette's
/// grounds — see `state_veneers_track_their_ground_in_every_palette`.
pub fn light_overlays() -> OverlayTiers {
    let k = |a: u8| Color::from_rgba8(0, 0, 0, a);
    (k(15), k(31), k(60), k(31), k(60))
}

/// High contrast trades "barely there" for visible: against black the dark
/// tiers collapse to a 1.08:1 step, which is exactly the feedback a
/// low-vision user cannot see. These measure 1.7:1 / 2.5:1 / 4.1:1 off the
/// ground and still host primary text at AA. The two hairline tiers go
/// further still: nothing hosts text on them. `ink` is white on a black
/// ground and black on a white one.
fn high_contrast_overlays(ink: u8) -> OverlayTiers {
    let w = |a: u8| Color::from_rgba8(ink, ink, ink, a);
    (w(51), w(77), w(110), w(153), w(217))
}

/// OKLCH lightness and chroma of the tinted emphasis fill on a palette of
/// each polarity: light enough on dark palettes (and dark enough on light
/// ones) that the content surface reads as its label.
fn tinted_fill(palette: &Palette) -> Tone {
    match (palette.dark, palette.high_contrast) {
        (true, false) => Tone::new(0.72, 0.13),
        (false, false) => Tone::new(0.50, 0.13),
        (true, true) => Tone::new(0.86, 0.12),
        (false, true) => Tone::new(0.36, 0.13),
    }
}

impl Colors {
    /// The whole colour token set for a palette, as authored: nothing is
    /// measured or repaired. [`Self::of_palette`] is what a theme paints
    /// with; this is what Theme Studio audits.
    #[must_use]
    pub fn as_authored(
        palette: &Palette,
        emphasis: Emphasis,
        presence: Tone,
        you: PresenceHue,
    ) -> Self {
        let hue = palette.hue.rem_euclid(360.0);
        let chroma = palette.chroma.clamp(0.0, NEUTRAL_MAX_CHROMA);
        let n = |lightness: f32| oklch(lightness, chroma, hue);
        let ladder = palette.surfaces;
        let content = n(ladder.content);
        let overlay = n(ladder.overlay);
        let primary = n(palette.ink.primary);
        let presence = PresencePalette::new(presence.lightness, presence.chroma);
        let [danger, warning, success, info] = {
            let tones = palette.status.in_hue_order();
            let mut out = [Color::BLACK; 4];
            for ((slot, tone), status_hue) in out.iter_mut().zip(tones).zip(STATUS_HUES) {
                *slot = oklch(
                    tone.lightness,
                    tone.chroma.min(STATUS_MAX_CHROMA),
                    status_hue,
                );
            }
            out
        };
        let (subtle, medium, strong, border_subtle, border_strong) =
            match (palette.dark, palette.high_contrast) {
                (true, false) => dark_overlays(),
                (false, false) => light_overlays(),
                (true, true) => high_contrast_overlays(255),
                (false, true) => high_contrast_overlays(0),
            };
        let content_edge = match (palette.dark, palette.high_contrast) {
            (true, false) => Color::from_rgba8(255, 255, 255, 36),
            (false, false) => Color::from_rgba8(0, 0, 0, 26),
            (true, true) => Color::WHITE,
            (false, true) => Color::BLACK,
        };
        let you_colour = presence.hue(you);
        let (inverted, inverted_hover, lift_chroma, lift_hue) = match emphasis {
            Emphasis::Inverted => {
                let hover = oklch(
                    palette.ink.primary + (ladder.content - palette.ink.primary) * 0.12,
                    chroma,
                    hue,
                );
                (primary, hover, chroma, hue)
            }
            Emphasis::Tinted { hue: tint } => {
                let tint = tint.rem_euclid(360.0);
                let fill = tinted_fill(palette);
                // Under the pointer a tinted fill moves away from its label,
                // so hover only ever adds contrast.
                let hover =
                    (fill.lightness - (ladder.content - fill.lightness) * 0.08).clamp(0.0, 1.0);
                (
                    oklch(fill.lightness, fill.chroma, tint),
                    oklch(hover, fill.chroma, tint),
                    TINTED_LIFT_CHROMA,
                    tint,
                )
            }
        };
        let lift = |t: f32| oklch(ladder.content + palette.lift() * t, lift_chroma, lift_hue);
        let opaque = palette.high_contrast;
        let ramp = palette.ramp();
        Self {
            palette: *palette,
            emphasis,
            you,
            neutral: ColorRamp::neutral(ramp.0, ramp.1, chroma, hue),
            semantic: SemanticColors {
                success,
                warning,
                danger,
                info,
            },
            surfaces: Surfaces {
                base: n(ladder.base),
                content,
                sidebar: n(ladder.sidebar),
                inspector: n(ladder.inspector),
                raised: n(ladder.raised),
                overlay,
                scrim: n(palette.scrim()),
                material: if opaque {
                    overlay
                } else {
                    overlay.with_alpha(MATERIAL_ALPHA)
                },
                material_blur_px: if opaque { 0.0 } else { MATERIAL_BLUR_PX },
                material_opacity: if opaque { 1.0 } else { MATERIAL_ALPHA },
                content_edge,
            },
            intent: Intents {
                inverted,
                inverted_hover,
                on_inverted: content,
                selection: lift(1.0),
                selection_hover: lift(0.5),
                you: you_colour,
                selection_tint: Self::selection_tint_of(you_colour),
            },
            presence,
            text_primary: primary,
            text_dim: n(palette.ink.secondary),
            text_tertiary: n(palette.ink.tertiary),
            border_standard: n(palette.border()),
            border_focus: you_colour,
            overlay_subtle: subtle,
            overlay_medium: medium,
            overlay_strong: strong,
            overlay_border_subtle: border_subtle,
            overlay_border_strong: border_strong,
        }
    }

    /// The colour token set a theme paints with: [`Self::as_authored`],
    /// with any pairing that misses a contrast floor stepped out until it
    /// holds (`crate::tokens::contrast::repair`). The shipped palettes need
    /// no repair; a hand-edited document that slipped past Theme Studio
    /// still renders legibly.
    #[must_use]
    pub fn of_palette(
        palette: &Palette,
        emphasis: Emphasis,
        presence: Tone,
        you: PresenceHue,
    ) -> Self {
        let mut colors = Self::as_authored(palette, emphasis, presence, you);
        super::contrast::repair(&mut colors);
        colors
    }

    /// A palette with its stock presence tone, inverted emphasis and the
    /// default person's colour.
    #[must_use]
    pub fn stock(palette: &Palette) -> Self {
        Self::of_palette(
            palette,
            Emphasis::Inverted,
            palette.stock_presence(),
            PresenceHue::Blue,
        )
    }

    /// The default dark palette.
    #[must_use]
    pub fn dark() -> Self {
        Self::stock(&Palette::dark())
    }

    /// The light palette.
    #[must_use]
    pub fn light() -> Self {
        Self::stock(&Palette::light())
    }

    /// The dark high-contrast palette.
    #[must_use]
    pub fn high_contrast() -> Self {
        Self::stock(&Palette::high_contrast_dark())
    }

    /// The same colours with the neutral material at temperature `hue`:
    /// presence, status, emphasis and the person's own colour keep their
    /// hues. A palette with no chroma shows no temperature and is returned
    /// unchanged.
    #[must_use]
    pub fn with_neutral_hue(&self, hue: f32) -> Self {
        if !hue.is_finite() || self.palette.chroma <= 0.0 {
            return self.clone();
        }
        let palette = Palette {
            hue,
            ..self.palette
        };
        let presence = Tone::new(self.presence.lightness, self.presence.chroma);
        let mut next = Self::of_palette(&palette, self.emphasis, presence, self.you);
        next.set_material(
            self.surfaces.material_blur_px,
            self.surfaces.material_opacity,
        );
        next
    }

    /// Make `hue` the person's colour: focus ring, caret and text-selection
    /// tint, painted at this palette's presence lightness and chroma.
    pub fn set_you(&mut self, hue: PresenceHue) {
        let you = self.presence.hue(hue);
        self.you = hue;
        self.intent.you = you;
        self.intent.selection_tint = Self::selection_tint_of(you);
        self.border_focus = you;
    }

    /// The floating material: backdrop blur (logical pixels) and the
    /// opacity of the overlay tint over it. A high-contrast palette stays
    /// opaque whatever it is asked.
    pub fn set_material(&mut self, blur_px: f32, opacity: f32) {
        if self.palette.high_contrast {
            return;
        }
        let opacity = opacity.clamp(0.0, 1.0);
        self.surfaces.material_blur_px = blur_px.max(0.0);
        self.surfaces.material_opacity = opacity;
        self.surfaces.material = self.surfaces.overlay.with_alpha(opacity);
    }

    fn selection_tint_of(you: Color) -> Color {
        with_alpha(you, 82)
    }

    /// The selection lift on any `ground`: `t` 1.0 is one surface step
    /// (lighter on a dark palette, darker on a light one), 0.5 the hover
    /// halfway. Colourless under inverted emphasis; faintly the tint under
    /// tinted emphasis.
    #[must_use]
    pub fn lift_on(&self, ground: Color, t: f32) -> Color {
        let g = Oklch::from_color(ground);
        let (chroma, hue) = match self.emphasis {
            Emphasis::Inverted => (self.palette.chroma, self.palette.hue),
            Emphasis::Tinted { hue } => (TINTED_LIFT_CHROMA, hue),
        };
        oklch(g.l + self.palette.lift() * t, chroma, hue)
    }

    /// Ink tiers for content on [`Surfaces::content`] — the theme's own text
    /// tiers, since those are defined there.
    pub fn on_content(&self) -> OnSurface {
        self.on_surface(self.surfaces.content)
    }

    /// [`Self::on_content`] for any surface a widget paints on. Measured
    /// against that surface rather than darkened a fixed fraction, which
    /// yields roughly the same ratio whatever the material.
    pub fn on_surface(&self, surface: Color) -> OnSurface {
        self.on_surface_at(surface, BODY_MIN_CONTRAST)
    }

    /// [`Self::on_surface`] against a caller-chosen floor. [`OnSurface::ink`]
    /// and `ink_dim` step out until they clear `min_ratio`;
    /// [`OnSurface::ink_tertiary`] passes through as authored — how quiet
    /// the quietest tier is belongs to the material (held to 3:1 on every
    /// surface by `contrast::FLOORS`), and forcing a body-text floor would
    /// erase the tier.
    pub fn on_surface_at(&self, surface: Color, min_ratio: f64) -> OnSurface {
        use crate::a11y::contrast_ratio;

        let tiers_read = contrast_ratio(self.text_primary, surface) >= min_ratio
            && contrast_ratio(self.text_dim, surface) >= min_ratio;

        let (ink, ink_dim, ink_tertiary) = if tiers_read {
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

        let ink = Self::step_out_until_legible(ink, surface, min_ratio);
        let ink_dim = Self::step_out_until_legible(ink_dim, surface, min_ratio);

        OnSurface {
            ink,
            ink_dim,
            ink_tertiary,
            tint: mix8(surface, ink, ON_SURFACE_TINT_T),
        }
    }

    /// An authored `tone` made legible on `ground`: untouched if it already
    /// clears `min_ratio`, otherwise stepped toward the contrasting extreme.
    pub fn ink_on(&self, ground: Color, tone: Color, min_ratio: f64) -> Color {
        Self::step_out_until_legible(tone, ground, min_ratio)
    }

    /// One `tone`, made legible on *every* ground in `grounds` at once.
    /// Resolving a control's label separately per state (rest/hover/press)
    /// lets it flip polarity mid-range, so ink is chosen once, against the
    /// worst ground, and held for the whole set.
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
        let hardest = grounds
            .iter()
            .copied()
            .min_by(|a, b| {
                contrast_ratio(tone, *a)
                    .partial_cmp(&contrast_ratio(tone, *b))
                    .unwrap_or(core::cmp::Ordering::Equal)
            })
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

    /// A `tint` washed over `ground` at `wash` strength (a status pill, a
    /// presence chip), resolved into an opaque body, an edge and a label.
    ///
    /// The body composites **first**, so ink is measured against what the
    /// screen holds, not the token the wash was made from. Ink walks out of
    /// the tint only as far as the floor demands. The edge is measured
    /// against the *ground*, since its job is separating pill from row.
    pub fn tinted_chip_on(
        &self,
        ground: Color,
        tint: Color,
        wash: f32,
        min_ratio: f64,
    ) -> TintedChip {
        let fill = mix8(ground, tint, wash);
        TintedChip {
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
    /// it falls on. Tries both polarities at `alpha`, keeps whichever moves
    /// `ground` further.
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

    /// The body a chrome control takes when **lifted** off the desktop
    /// ground (hovered tab pill, active workspace chip). `t` runs 0.0 (flat)
    /// to 1.0 (full lift), interpolating `surfaces.base` toward
    /// `surfaces.raised`. Where a material has no raised tier (high contrast
    /// paints everything black) falls back to a wash polarity chosen the
    /// same way [`Colors::hairline_on`] picks one. Rows inside a window lift
    /// with [`Intents::selection`] / [`Self::lift_on`] instead.
    pub fn lift(&self, t: f32) -> Color {
        mix8(self.surfaces.base, self.lift_target(), t)
    }

    /// The colour a lift moves *toward*. Named apart from [`Colors::lift`]
    /// because it also anchors [`Colors::glass_film`]: a material and a flat
    /// wash must agree which way is "away from the ground".
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

    /// Whether this material's surfaces lift by getting *brighter*.
    pub fn lifts_bright(&self) -> bool {
        use crate::a11y::relative_luminance;
        let base = self.surfaces.base;
        relative_luminance(self.lift_target()) >= relative_luminance(base)
    }

    /// The colour of the film a glass selector is made of: the raised tier
    /// carried one derived step further (`GLASS_FILM_LIFT`), opaque (alpha
    /// belongs to the state painting it, not the material).
    pub fn glass_film(&self) -> Color {
        let step = if self.lifts_bright() {
            Color::WHITE
        } else {
            Color::BLACK
        };
        composite(with_alpha(step, GLASS_FILM_LIFT), self.lift_target())
    }

    /// The hairline separating a window tile from the desktop. Painted
    /// outside the tile's silhouette since the shell never samples the app's
    /// pixels inside it. `crate::widget::tile_host::TileHost` owns the
    /// matching geometry.
    ///
    /// The declared [`Surfaces::content_edge`] is honoured wherever it
    /// registers; only a declaration the ground would swallow gets its
    /// polarity flipped.
    pub fn window_edge(&self) -> Color {
        use crate::a11y::relative_luminance;
        let ground = self.surfaces.base;
        let declared = self.surfaces.content_edge;
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
    /// (4.5:1) against `bg`. Where it does not, this falls back to the
    /// theme's primary text, and finally to plain white or black, whichever
    /// wins — one of the two always clears 4.5:1 against any colour.
    ///
    /// `bg` must be the colour that will actually be *seen*: composite any
    /// translucent fill over its ground before passing it in.
    pub fn readable_on(&self, bg: Color, preferred: Color) -> Color {
        for candidate in [preferred, self.text_primary] {
            if crate::a11y::contrast_ratio(candidate, bg) >= BODY_MIN_CONTRAST {
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

    /// Resolve a **filled chip** — the destructive fill inside a
    /// confirmation step, a status badge — into a `(fill, label)` pair whose
    /// label clears WCAG AA (4.5:1) against the fill.
    ///
    /// If `preferred_label` already clears AA the pair is returned
    /// untouched; otherwise the **fill is deepened toward black** until the
    /// label clears, up to [`MAX_CHIP_DEEPEN`]. Past that the fill is kept
    /// and the label re-picked via [`Self::readable_on`].
    ///
    /// `fill` must be opaque: it is what the label will actually sit on.
    pub fn legible_fill(&self, fill: Color, preferred_label: Color) -> (Color, Color) {
        if crate::a11y::contrast_ratio(preferred_label, fill) >= BODY_MIN_CONTRAST {
            return (fill, preferred_label);
        }
        let black = Color::BLACK;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "MAX_CHIP_DEEPEN is a non-negative 0.0..=1.0 design token, so its percentage fits exactly in u8"
        )]
        let max_step = (MAX_CHIP_DEEPEN * 100.0).floor() as u8;
        for step in 1..=max_step {
            let deepened = mix8(fill, black, f32::from(step) / 100.0);
            if crate::a11y::contrast_ratio(preferred_label, deepened) >= BODY_MIN_CONTRAST {
                return (deepened, preferred_label);
            }
        }
        (fill, self.readable_on(fill, preferred_label))
    }
}

#[cfg(test)]
mod tests {
    //! The neutral material is pinned to the design language's lightness
    //! table and swept for contrast at every temperature; presence, status
    //! and intents are held to the promises their docs make.
    use super::*;
    use crate::a11y::contrast_ratio;

    fn rgba(c: Color) -> [u8; 4] {
        [c.r(), c.g(), c.b(), c.a()]
    }

    /// Every shipped palette, named.
    fn palettes() -> [(&'static str, Palette); 7] {
        [
            ("dark", Palette::dark()),
            ("dark cool", Palette::dark_cool()),
            ("dark graphite", Palette::dark_graphite()),
            ("dark soft", Palette::dark_soft()),
            ("light", Palette::light()),
            ("high contrast", Palette::high_contrast_dark()),
            ("high contrast light", Palette::high_contrast_light()),
        ]
    }

    /// Temperatures swept by every hue-dependent assertion.
    fn hues() -> impl Iterator<Item = f32> {
        (0..24).map(|i| {
            #[allow(clippy::cast_precision_loss, reason = "i < 24")]
            let h = i as f32 * 15.0;
            h
        })
    }

    fn at_hue(palette: &Palette, hue: f32) -> Colors {
        Colors::stock(&Palette { hue, ..*palette })
    }

    fn surfaces(c: &Colors) -> [(&'static str, Color); 6] {
        [
            ("base", c.surfaces.base),
            ("content", c.surfaces.content),
            ("sidebar", c.surfaces.sidebar),
            ("inspector", c.surfaces.inspector),
            ("raised", c.surfaces.raised),
            ("overlay", c.surfaces.overlay),
        ]
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

    #[test]
    fn the_neutral_material_follows_the_lightness_table_at_one_hue() {
        // phase: surfaces_and_ink_sit_at_the_documented_lightness
        {
            let table = [
                (
                    Palette::dark(),
                    [0.16, 0.215, 0.195, 0.235, 0.265, 0.285],
                    [0.96, 0.76, 0.62],
                ),
                (
                    Palette::dark_soft(),
                    [0.19, 0.25, 0.228, 0.27, 0.30, 0.32],
                    [0.96, 0.78, 0.65],
                ),
                (
                    Palette::light(),
                    [0.90, 0.995, 0.965, 0.975, 1.0, 1.0],
                    [0.18, 0.43, 0.56],
                ),
            ];
            for (palette, surface_l, ink_l) in table {
                let c = Colors::stock(&palette);
                for ((name, surface), want) in surfaces(&c).into_iter().zip(surface_l) {
                    let got = Oklch::from_color(surface).l;
                    assert!(
                        (got - want).abs() <= 0.006,
                        "{name}: L {got:.3}, the design language says {want}"
                    );
                }
                for ((name, ink), want) in [
                    ("primary", c.text_primary),
                    ("secondary", c.text_dim),
                    ("tertiary", c.text_tertiary),
                ]
                .into_iter()
                .zip(ink_l)
                {
                    let got = Oklch::from_color(ink).l;
                    assert!(
                        (got - want).abs() <= 0.006,
                        "{name} ink: L {got:.3}, the design language says {want}"
                    );
                }
            }
        }
        // phase: every_neutral_is_low_chroma_at_the_theme_hue
        {
            for (label, palette) in palettes() {
                for hue in hues() {
                    let c = at_hue(&palette, hue);
                    let mut neutrals: Vec<(&str, Color)> = surfaces(&c).to_vec();
                    neutrals.extend([
                        ("primary", c.text_primary),
                        ("secondary", c.text_dim),
                        ("tertiary", c.text_tertiary),
                        ("border", c.border_standard),
                        ("selection", c.intent.selection),
                    ]);
                    neutrals.extend(c.neutral.stops.iter().map(|s| ("ramp", *s)));
                    for (name, colour) in neutrals {
                        let o = Oklch::from_color(colour);
                        // 8-bit quantisation alone is worth a few thousandths
                        // of chroma near black.
                        assert!(
                            o.c <= palette.chroma + 0.004,
                            "{label}@{hue}: {name} has chroma {:.4}",
                            o.c
                        );
                        // Hue only means something where there is chroma to
                        // carry it past 8-bit quantisation.
                        if palette.chroma >= 0.008 && o.c >= 0.006 && o.l > 0.1 && o.l < 0.97 {
                            let d = (o.h - hue).rem_euclid(360.0);
                            let d = d.min(360.0 - d);
                            assert!(d <= 25.0, "{label}@{hue}: {name} drifted to hue {:.0}", o.h);
                        }
                    }
                }
            }
        }
        // phase: the_default_dark_is_pure_grey
        {
            let c = Colors::dark();
            for (name, colour) in surfaces(&c) {
                assert!(
                    colour.r() == colour.g() && colour.g() == colour.b(),
                    "{name} is {colour:?}"
                );
            }
        }
        // phase: regions_separate_from_content_by_tone_or_by_an_edge
        {
            for (label, palette) in palettes() {
                for hue in hues() {
                    let c = at_hue(&palette, hue);
                    let s = &c.surfaces;
                    for (name, tier) in [("sidebar", s.sidebar), ("inspector", s.inspector)] {
                        let step = f64::from(
                            (Oklch::from_color(tier).l - Oklch::from_color(s.content).l).abs(),
                        );
                        if palette.high_contrast {
                            assert!(
                                crate::surface::region_needs_edge(tier, s.content),
                                "{label}: the {name} shares the content ground and \
                                 separates by its edge"
                            );
                            let edge = crate::surface::edge_on(tier);
                            assert!(
                                crate::surface::edge_delta(edge, tier) >= HAIRLINE_MIN_DELTA,
                                "{label}: the {name} edge does not register"
                            );
                        } else {
                            assert!(
                                step >= REGION_MIN_STEP,
                                "{label}@{hue}: {name} is ΔL {step:.3} from content"
                            );
                            assert!(!crate::surface::region_needs_edge(tier, s.content));
                        }
                    }
                }
            }
        }
        // phase: floating_chrome_walks_lighter_than_the_window_on_dark
        {
            for (label, palette) in palettes() {
                if !palette.dark || palette.high_contrast {
                    continue;
                }
                for hue in hues() {
                    let c = at_hue(&palette, hue);
                    let l = |x: Color| Oklch::from_color(x).l;
                    let s = &c.surfaces;
                    for (name, under) in [
                        ("base", s.base),
                        ("content", s.content),
                        ("raised", s.raised),
                    ] {
                        assert!(
                            l(s.overlay) > l(under),
                            "{label}@{hue}: overlay is not lighter than {name}"
                        );
                    }
                }
            }
        }
        // phase: surfaces_order_by_lightness_as_documented
        {
            for (label, palette) in palettes() {
                if palette.high_contrast {
                    continue;
                }
                let c = Colors::stock(&palette);
                let l = |x: Color| Oklch::from_color(x).l;
                let s = &c.surfaces;
                let order = if palette.dark {
                    [
                        s.base,
                        s.sidebar,
                        s.content,
                        s.inspector,
                        s.raised,
                        s.overlay,
                    ]
                } else {
                    [
                        s.base,
                        s.sidebar,
                        s.inspector,
                        s.content,
                        s.raised,
                        s.overlay,
                    ]
                };
                for pair in order.windows(2) {
                    if let [a, b] = pair {
                        assert!(l(*a) <= l(*b), "{label}: surfaces out of order");
                    }
                }
            }
        }
    }

    #[test]
    fn every_shipped_palette_needs_no_repair_at_any_temperature_or_emphasis() {
        for (label, palette) in palettes() {
            for hue in hues() {
                let palette = Palette { hue, ..palette };
                let mut emphases = vec![Emphasis::Inverted];
                emphases.extend(hues().map(|tint| Emphasis::Tinted { hue: tint }));
                for emphasis in emphases {
                    let authored = Colors::as_authored(
                        &palette,
                        emphasis,
                        palette.stock_presence(),
                        PresenceHue::Blue,
                    );
                    let findings = crate::tokens::contrast::audit(&authored);
                    assert!(
                        findings.is_empty(),
                        "{label}@{hue} {emphasis:?}: {}",
                        findings
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
            }
        }
    }

    #[test]
    fn hover_states_sit_between_rest_and_selected() {
        for (label, palette) in palettes() {
            for emphasis in [Emphasis::Inverted, Emphasis::Tinted { hue: 150.0 }] {
                let c = Colors::of_palette(
                    &palette,
                    emphasis,
                    palette.stock_presence(),
                    PresenceHue::Blue,
                );
                let step = contrast_ratio(c.intent.selection, c.surfaces.content);
                assert!(
                    step >= 1.15,
                    "{label}: selection lift is a {step:.3}:1 step"
                );
                let hover = contrast_ratio(c.intent.selection_hover, c.surfaces.content);
                assert!(
                    hover > 1.0 && hover < step,
                    "{label} {emphasis:?}: hover must sit between rest and selected"
                );
            }
        }
    }

    #[test]
    fn tinted_emphasis_colours_actions_and_selection_and_nothing_else() {
        let tinted = Colors::of_palette(
            &Palette::dark(),
            Emphasis::Tinted { hue: 150.0 },
            Palette::dark().stock_presence(),
            PresenceHue::Blue,
        );
        let plain = Colors::dark();
        let fill = Oklch::from_color(tinted.intent.inverted);
        assert!(fill.c > 0.08, "the primary fill carries the tint");
        assert!((fill.h - 150.0).abs() < 6.0, "at its hue: {:.0}", fill.h);
        let lift = Oklch::from_color(tinted.intent.selection);
        assert!(lift.c > 0.02, "the selection lift is tinted");
        assert_eq!(rgba(tinted.surfaces.content), rgba(plain.surfaces.content));
        assert_eq!(rgba(tinted.text_primary), rgba(plain.text_primary));
        assert_eq!(rgba(tinted.intent.you), rgba(plain.intent.you));
        let plain_fill = Oklch::from_color(plain.intent.inverted);
        assert!(plain_fill.c < 0.005, "inverted emphasis has no hue");
    }

    #[test]
    fn status_is_muted_and_presence_differs_in_hue_only() {
        // phase: status_chroma_stays_muted
        for (label, palette) in palettes() {
            let s = Colors::stock(&palette).semantic;
            for (name, colour) in [
                ("danger", s.danger),
                ("warning", s.warning),
                ("success", s.success),
                ("info", s.info),
            ] {
                let chroma = Oklch::from_color(colour).c;
                assert!(
                    chroma <= STATUS_MAX_CHROMA + 0.004,
                    "{label}: {name} has chroma {chroma:.3}"
                );
            }
        }
        // phase: presence_hues_share_lightness_and_chroma
        for (palette, want_l, want_c) in [
            (Palette::dark(), 0.74, 0.14),
            (Palette::light(), 0.58, 0.15),
        ] {
            let p = Colors::stock(&palette).presence;
            for (i, colour) in p.colors.iter().enumerate() {
                let o = Oklch::from_color(*colour);
                assert!((o.l - want_l).abs() <= 0.006, "slot {i}: L {:.3}", o.l);
                // Gamut mapping may only ever take chroma away, and never so
                // much that a slot stops reading as its hue.
                assert!(
                    o.c <= want_c + 0.004 && o.c >= 0.09,
                    "slot {i}: C {:.3}",
                    o.c
                );
                let hue = PRESENCE_HUES.get(i).copied().unwrap_or_default();
                let d = (o.h - hue).rem_euclid(360.0);
                assert!(d.min(360.0 - d) <= 3.0, "slot {i}: hue {:.1}", o.h);
            }
        }
        // phase: presence_neighbours_stay_apart
        for (label, palette) in palettes() {
            let p = Colors::stock(&palette).presence;
            for i in 0..PRESENCE_LEN {
                let (a, b) = (Oklch::from_color(p.get(i)), Oklch::from_color(p.get(i + 1)));
                let (ax, ay) = (a.c * a.h.to_radians().cos(), a.c * a.h.to_radians().sin());
                let (bx, by) = (b.c * b.h.to_radians().cos(), b.c * b.h.to_radians().sin());
                let distance = (ax - bx).hypot(ay - by).hypot(a.l - b.l);
                assert!(
                    distance >= 0.05,
                    "{label}: slots {i} and {} are {distance:.3} apart",
                    (i + 1) % PRESENCE_LEN
                );
            }
        }
        // phase: identities_reproject_onto_the_palette
        {
            let dark = Colors::dark();
            let light = Colors::light();
            for slot in 0..PRESENCE_LEN {
                // A dark slot read on light lands on light's own slot.
                let got = light.presence.of(dark.presence.get(slot));
                let want = light.presence.get(slot);
                for (g, w) in [
                    (got.r(), want.r()),
                    (got.g(), want.g()),
                    (got.b(), want.b()),
                ] {
                    assert!(g.abs_diff(w) <= 2, "slot {slot}: {got:?} vs {want:?}");
                }
            }
            // Any colour lands at the palette's lightness, whatever its own.
            let loud = Color::from_rgb8(255, 0, 0);
            let painted = Oklch::from_color(dark.presence.of(loud));
            assert!((painted.l - 0.74).abs() <= 0.006);
            assert!(painted.c <= 0.144);
        }
    }

    #[test]
    fn presence_hues_are_named_stable_and_derived_from_identity() {
        for hue in PresenceHue::ALL {
            assert_eq!(PresenceHue::from_id(hue.id()), Some(hue));
            assert_eq!(PresenceHue::from_slot(hue.slot()), hue);
        }
        assert_eq!(PresenceHue::of_identity("me"), PresenceHue::Blue);
        let a = PresenceHue::of_identity("6f1c2e9a");
        assert_eq!(a, PresenceHue::of_identity("6f1c2e9a"), "deterministic");
        let spread: std::collections::HashSet<_> = (0..64)
            .map(|i| PresenceHue::of_identity(&format!("person-{i}")))
            .collect();
        assert!(spread.len() >= 8, "identities spread over the ring");
        let orange = Color::from_rgb8(0xE0, 0x80, 0x30);
        assert_eq!(PresenceHue::nearest(orange), Some(PresenceHue::Orange));
        assert_eq!(PresenceHue::nearest(Color::from_rgb8(128, 128, 128)), None);
        let settings = serde_json::json!({"identity": {"colour": "violet"}});
        assert_eq!(PresenceHue::chosen_in(&settings), Some(PresenceHue::Violet));
        assert_eq!(PresenceHue::chosen_in(&serde_json::json!({})), None);
    }

    #[test]
    fn temperature_and_your_colour_move_only_what_they_name() {
        // phase: a_hue_change_moves_the_neutrals_and_nothing_else
        {
            let mut cool = Colors::stock(&Palette::dark_cool());
            cool.set_you(PresenceHue::Orange);
            let warm = cool.with_neutral_hue(40.0);
            assert!((warm.palette.hue - 40.0).abs() < f32::EPSILON);
            let h = Oklch::from_color(warm.surfaces.content).h;
            assert!((h - 40.0).abs() <= 25.0, "content hue {h:.0}");
            assert_ne!(rgba(warm.surfaces.content), rgba(cool.surfaces.content));
            assert_eq!(rgba(warm.intent.you), rgba(cool.intent.you), "you is kept");
            assert_eq!(rgba(warm.semantic.danger), rgba(cool.semantic.danger));
            assert_eq!(warm.presence, cool.presence);
            let before = contrast_ratio(cool.text_dim, cool.surfaces.content);
            let after = contrast_ratio(warm.text_dim, warm.surfaces.content);
            assert!(
                (before - after).abs() < 0.25,
                "temperature must not move contrast ({before:.2} -> {after:.2})"
            );
        }
        // phase: a_neutral_palette_shows_no_temperature
        {
            let grey = Colors::dark();
            let warm = grey.with_neutral_hue(40.0);
            assert_eq!(rgba(warm.surfaces.content), rgba(grey.surfaces.content));
            assert!((warm.palette.hue - grey.palette.hue).abs() < f32::EPSILON);
        }
        // phase: your_colour_is_focus_caret_and_selection
        {
            let mut c = Colors::light();
            c.set_you(PresenceHue::Teal);
            assert_eq!(c.you, PresenceHue::Teal);
            assert_eq!(rgba(c.border_focus), rgba(c.intent.you));
            assert_eq!(rgba(c.intent.you), rgba(c.presence.hue(PresenceHue::Teal)));
            assert_eq!(c.intent.selection_tint.a(), 82);
            let tint = c.intent.selection_tint;
            assert_eq!(
                [tint.r(), tint.g(), tint.b()],
                [c.intent.you.r(), c.intent.you.g(), c.intent.you.b()]
            );
            let seen = composite(tint, c.surfaces.content);
            let r = contrast_ratio(c.text_primary, seen);
            assert!(r >= 4.5, "text on its selection tint is {r:.2}:1");
        }
        // phase: the_material_is_a_theme_role_and_high_contrast_stays_opaque
        {
            let mut c = Colors::dark();
            assert_eq!(c.surfaces.overlay.a(), 255);
            assert_eq!(c.surfaces.material.a(), 184);
            c.set_material(8.0, 0.9);
            assert!((c.surfaces.material_blur_px - 8.0).abs() < f32::EPSILON);
            assert_eq!(c.surfaces.material.a(), 230);
            let mut hc = Colors::high_contrast();
            hc.set_material(24.0, 0.5);
            assert_eq!(hc.surfaces.material.a(), 255);
            assert!(hc.surfaces.material_blur_px.abs() < f32::EPSILON);
        }
    }

    #[test]
    fn state_veneers_track_their_ground_in_every_palette() {
        // phase: state_veneers_move_every_ground
        for (label, palette) in palettes() {
            let c = Colors::stock(&palette);
            for (ground_name, ground) in surfaces(&c) {
                for (tier_name, tier, floor) in [
                    ("subtle", c.overlay_subtle, 1.07),
                    ("medium", c.overlay_medium, 1.17),
                    ("strong", c.overlay_strong, 1.45),
                    ("border_subtle", c.overlay_border_subtle, 1.17),
                    ("border_strong", c.overlay_border_strong, 1.45),
                ] {
                    let step = contrast_ratio(composite(tier, ground), ground);
                    assert!(
                        step >= floor,
                        "{label}: {tier_name} veneer over {ground_name} is a {step:.3} step, needs {floor}",
                    );
                }
            }
        }
        // phase: veneer_polarity_follows_the_ground
        for (label, palette) in palettes() {
            let c = Colors::stock(&palette);
            let ground = c.surfaces.content;
            let dark = lstar(ground) < 50.0;
            for (tier_name, tier) in [
                ("subtle", c.overlay_subtle),
                ("medium", c.overlay_medium),
                ("strong", c.overlay_strong),
            ] {
                let delta = lstar(composite(tier, ground)) - lstar(ground);
                assert!(
                    if dark { delta > 0.0 } else { delta < 0.0 },
                    "{label}: {tier_name} must push the ground away (got {delta:.2})"
                );
            }
        }
        // phase: light_overlays_match_the_dark_perceptual_step
        {
            let c = Colors::light();
            let targets = [("subtle", 5.0), ("medium", 10.5), ("strong", 21.0)];
            for (ground_name, ground) in [
                ("content", c.surfaces.content),
                ("raised", c.surfaces.raised),
            ] {
                let tiers = [c.overlay_subtle, c.overlay_medium, c.overlay_strong];
                for ((tier_name, want), tier) in targets.iter().zip(tiers) {
                    let got = lstar(ground) - lstar(composite(tier, ground));
                    assert!(
                        (got - want).abs() <= 2.0,
                        "{ground_name}: {tier_name} step is {got:.2} L*, dark gets {want:.1}",
                    );
                }
            }
        }
        // phase: high_contrast_veneers_still_host_primary_text
        for palette in [
            Palette::high_contrast_dark(),
            Palette::high_contrast_light(),
        ] {
            let c = Colors::stock(&palette);
            for tier in [c.overlay_subtle, c.overlay_medium, c.overlay_strong] {
                let r = contrast_ratio(c.text_primary, composite(tier, c.surfaces.content));
                assert!(r >= 4.5, "primary text on a veneer is {r:.2}:1");
            }
        }
    }

    #[test]
    fn readable_pairings_and_legible_fills_always_clear_aa() {
        // phase: readable_on_keeps_a_preferred_colour_that_already_passes
        {
            let c = Colors::dark();
            let got = c.readable_on(c.surfaces.raised, c.text_primary);
            assert_eq!(rgba(got), rgba(c.text_primary));
        }
        // phase: readable_on_always_returns_an_aa_pairing
        {
            let c = Colors::light();
            let mut probes: Vec<Color> = (0..=255)
                .step_by(5)
                .map(|v| Color::from_rgb8(v, v, v))
                .collect();
            for (_, palette) in palettes() {
                let m = Colors::stock(&palette);
                probes.extend(m.presence.colors);
                probes.push(m.semantic.danger);
            }
            for bg in probes {
                let fg = c.readable_on(bg, c.text_dim);
                let r = contrast_ratio(fg, bg);
                assert!(
                    r >= 4.5,
                    "readable_on returned a {r:.2}:1 pairing on {bg:?}"
                );
            }
        }
        // phase: legible_fill_clears_aa_on_every_status_fill
        for (label, palette) in palettes() {
            let c = Colors::stock(&palette);
            for (kind, raw) in [
                ("danger", c.semantic.danger),
                ("success", c.semantic.success),
                ("you", c.intent.you),
            ] {
                let (fill, label_ink) = c.legible_fill(raw, c.surfaces.content);
                let r = contrast_ratio(label_ink, fill);
                assert!(r >= 4.5, "{label}/{kind}: chip label is {r:.2}:1, below AA");
            }
        }
    }

    #[test]
    fn the_denied_scrim_mutes_neutral_and_inverted_controls() {
        for (label, palette) in palettes() {
            let c = Colors::stock(&palette);
            for (kind, body, content) in [
                ("neutral", c.surfaces.raised, c.text_primary),
                ("inverted", c.intent.inverted, c.intent.on_inverted),
            ] {
                let before = contrast_ratio(content, body);
                let after = contrast_ratio(
                    composite(PERMISSION_DENIED_SCRIM, content),
                    composite(PERMISSION_DENIED_SCRIM, body),
                );
                assert!(
                    after < before / 1.08,
                    "{label}/{kind}: the scrim must cost the content contrast \
                     ({before:.2} -> {after:.2})",
                );
                let moved =
                    |x: Color| (lstar(x) - lstar(composite(PERMISSION_DENIED_SCRIM, x))).abs();
                let shift = moved(content).max(moved(body));
                assert!(
                    shift >= 3.0,
                    "{label}/{kind}: the denied control must visibly shift (got {shift:.1} L*)",
                );
            }
        }
    }
}
