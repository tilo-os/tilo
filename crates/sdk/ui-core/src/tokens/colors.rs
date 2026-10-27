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
//! - [`Intents::you`]: the local person's own presence colour;
//! - the accent ([`Intents::accent`], [`Intents::accent_ink`]): one hue, by
//!   default the person's own colour ([`Emphasis::You`]), for primary
//!   actions, links, the focus ring, the caret, the text selection and the
//!   selected row's tint;
//! - the on state ([`Intents::on_state`]): green, for what is switched on;
//! - [`SemanticColors`]: muted status (danger, warning, success, info).
//!
//! Nothing else gains colour: hover, grips, window edges, icons at rest,
//! sliders and progress are neutral.
//!
//! Every colour here is derived from a [`Palette`] (what a theme document
//! says) in [`Colors::as_authored`]; [`Colors::of_palette`] then repairs any
//! pairing that misses a contrast floor (`super::contrast`).

use crate::color::{Oklch, oklch};
use crate::color_compat::ColorCompat;
use crate::paint::with_alpha;
use vello::peniko::Color;

pub use super::palette::{
    Emphasis, NEUTRAL_MAX_CHROMA, Palette, STATUS_HUES, STATUS_MAX_CHROMA, TINTED_LIFT_CHROMA, Tone,
};

/// How far [`Colors::legible_fill`] may deepen a filled chip toward black
/// before it gives up and re-picks the label instead.
pub const MAX_CHIP_DEEPEN: f32 = 0.35;

pub const PERMISSION_DENIED_SCRIM: Color = Color::from_rgba8(0, 0, 0, 77);

/// Straight (non-premultiplied) 8-bit sRGB mix — the space most widget
/// draw paths blend in.
pub fn mix8(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a weighted average of two u8 (0..=255) channel values with non-negative weights summing to 1.0 always rounds to a value already within 0..=255"
    )]
    let l = |x: u8, y: u8| (f32::from(x) * (1.0 - t) + f32::from(y) * t).round() as u8;
    Color::from_rgb8(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// How far a value's fill stands from the focus ring's ink, in OKLCH
/// lightness, away from the ground: a focused slider's ring never merges
/// with its own fill.
const VALUE_FILL_STEP: f32 = 0.1;

/// [`Intents::value_fill`] on `ground` beside `track` (the lift a bar's
/// unfilled part is): the accent hue a step further from the ground than
/// [`Intents::accent_ink`], then stepped on until it clears the non-text
/// contrast floor against both.
pub(crate) fn value_fill_on(accent_ink: Color, ground: Color, track: Color) -> Color {
    use crate::a11y::contrast_ratio;
    let ink = Oklch::from_color(accent_ink);
    let away = if Oklch::from_color(ground).l > 0.5 {
        -1.0
    } else {
        1.0
    };
    let reads = |fill: Color| {
        contrast_ratio(fill, ground) >= NON_TEXT_MIN_CONTRAST
            && contrast_ratio(fill, track) >= NON_TEXT_MIN_CONTRAST
    };
    let mut lightness = ink.l + away * VALUE_FILL_STEP;
    let mut fill = oklch(lightness.clamp(0.0, 1.0), ink.c, ink.h);
    while !reads(fill) && (0.0..=1.0).contains(&lightness) {
        lightness += away * 0.02;
        fill = oklch(lightness.clamp(0.0, 1.0), ink.c, ink.h);
    }
    fill
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

/// How far a selection lifts its ground, in surface steps
/// ([`Colors::lift_on`]). Past the hover and press lift so a selected row
/// moves the dark window's ground by at least [`HAIRLINE_MIN_DELTA`] and
/// stands clear of a table's zebra stripe on every ground.
pub const SELECTED_LIFT_T: f32 = 1.2;

/// How far a floating surface's inner hairline (white at 7 % on a dark
/// material, black at 8 % on a light one, `docs/design-language.md`,
/// "Surfaces") moves its fill. Quieter than [`HAIRLINE_MIN_DELTA`] by
/// design: a floating surface separates by its level, its two-layer shadow
/// and this hairline together, and a brighter line read as a frame drawn
/// around the panel.
pub const FLOATING_EDGE_MIN_DELTA: f64 = 0.01;
/// Weight of the focused window's neutral edge over the desktop ground.
const FOCUSED_WINDOW_EDGE_ALPHA: u8 = 168;

/// Smallest OKLCH lightness step that separates a window region (sidebar,
/// inspector) from the content ground by tone alone. A region tier closer
/// to content than this draws a hairline on the edge they share instead
/// (`surface::region`).
pub const REGION_MIN_STEP: f64 = 0.015;

/// Fallback wash alpha for [`Colors::lift`] when a material has no raised
/// tier to aim at. 15% — reads as a distinct body without blowing out
/// contrast.
const LIFT_FALLBACK_ALPHA: u8 = 38;

/// Stock opacity of the floating-chrome material over its blurred backdrop:
/// the compositor blurs what is behind a panel and tints it with
/// [`Surfaces::overlay`] at 72 %. A theme sets its own
/// ([`Surfaces::material_opacity`]).
pub const MATERIAL_ALPHA: f32 = 0.72;

/// Stock backdrop blur of the floating material, in logical pixels.
pub const MATERIAL_BLUR_PX: f32 = 24.0;

/// Stock opacity of the see-through material ([`Surfaces::see_through_opacity`]):
/// half. Chrome over pixels the person is choosing (the capture bar and its
/// selection controls) must leave those pixels visible, unblurred.
pub const SEE_THROUGH_ALPHA: f32 = 0.5;

#[derive(Clone, Debug, PartialEq)]
pub struct ColorRamp {
    pub stops: [Color; 11],
}

/// The neutral material at lightness `l`: `chroma` at `hue`, eased back
/// where 8-bit rounding near black would carry it past
/// [`NEUTRAL_MAX_CHROMA`]. A workspace hue moves the hue only; it never
/// buys chroma (design language, "Workspace temperature").
fn neutral_at(l: f32, chroma: f32, hue: f32) -> Color {
    let mut c = chroma.clamp(0.0, NEUTRAL_MAX_CHROMA);
    loop {
        let colour = oklch(l, c, hue);
        if c <= 0.0 || Oklch::from_color(colour).c <= NEUTRAL_MAX_CHROMA {
            return colour;
        }
        c = (c - 0.002).max(0.0);
    }
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
            *stop = neutral_at(from + (to - from) * t, chroma, hue);
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
/// [`Self::material`], and every one shares the hue [`Colors::with_neutral_hue`] sets.
#[derive(Clone, Debug, PartialEq)]
pub struct Surfaces {
    /// The desktop ground behind tiles.
    pub base: Color,
    /// The window ground: every app's background, owned by the framework.
    /// The ink tiers ([`Colors::text_primary`] and friends) are defined on
    /// this surface.
    pub content: Color,
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
    /// Strength of every scrim at full openness — behind a modal Panel, a
    /// sheet or an overlay drawer — as `chrome::fill_scrim` reads it. Per
    /// theme: dark themes dim harder, since their grounds are near black.
    pub scrim_strength: f32,
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
    /// Opacity of the see-through material: [`Self::overlay`] over an
    /// unblurred backdrop, for chrome that must not hide what is under it
    /// (the capture bar over the pixels being chosen). `1` where nothing may
    /// be translucent.
    pub see_through_opacity: f32,
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
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticColors {
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    /// Something is happening that is neither good nor bad: a sync in
    /// flight, an informational notice.
    pub info: Color,
}

/// What controls paint with, by intent rather than by hue
/// (`docs/design-language.md`, "Colour roles"). Nothing outside these roles
/// gets colour: the accent is for primary actions, links, the focus ring,
/// the caret, the text selection and the selected row's tint; the on state
/// is for a switch, a checked box or radio, an active tile, a "connected"
/// dot. Hover, resize grips, window edges, icons at rest, sliders and
/// progress stay neutral.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Intents {
    /// Fill of a primary action: the accent hue at a lightness its label
    /// reads on ([`Emphasis::You`] and [`Emphasis::Tinted`]), or primary ink
    /// inverted onto the control under [`Emphasis::Inverted`].
    pub accent: Color,
    /// [`Self::accent`] under the pointer: a step away from its label, so
    /// hover only ever adds contrast.
    pub accent_hover: Color,
    /// [`Self::accent`] while pressed: a step past the rest fill the other
    /// way from hover, so a press never reads as a hover.
    pub accent_pressed: Color,
    /// Label and glyph on an [`Self::accent`] fill.
    pub on_accent: Color,
    /// The accent as ink and as a mark on a surface: a link, the focus
    /// ring, the caret, the text-selection wash. Clears body-text contrast
    /// on the window.
    pub accent_ink: Color,
    /// Fill of a control that is on: a switch's track, a checked box or
    /// radio, an active Quick Settings tile, a "connected" dot. Green, the
    /// colour people read as "on"; state, not status, and never the accent.
    pub on_state: Color,
    /// The knob or mark standing on [`Self::on_state`].
    pub on_state_mark: Color,
    /// A value that is neither on nor an action: a slider's filled track,
    /// a progress bar, a gauge, a spinner, a chart's default series, a
    /// finished step. The accent as a mark ([`Self::accent_ink`]), held to
    /// the non-text contrast floor on the ground it stands on; never the
    /// text colour, so a filled bar never reads as a black slab.
    pub value_fill: Color,
    /// A selected row, tab or segment on the content surface: one surface
    /// step, faintly the accent (colourless under [`Emphasis::Inverted`]).
    /// Other grounds lift with [`Colors::lift_on`].
    pub selection: Color,
    /// Halfway to [`Self::selection`]: an unselected row under the pointer.
    pub selection_hover: Color,
    /// The local person's presence colour: the colour collaborators see
    /// their cursor, caret and selection in.
    pub you: Color,
    /// Text-selection wash: [`Self::accent_ink`] at low alpha, painted under
    /// the selected glyphs.
    pub selection_tint: Color,
}

pub use collab_types::presence::{PRESENCE_HUES, PRESENCE_LEN, PresenceHue};

/// The presence hue nearest to `colour`'s, for a colour that arrives from
/// outside (a host's accent, a roster's packed colour). A grey has no hue
/// and gives `None`.
#[must_use]
pub fn nearest_presence_hue(colour: Color) -> Option<PresenceHue> {
    let source = Oklch::from_color(colour);
    if source.c < 0.02 {
        return None;
    }
    Some(PresenceHue::nearest_to_degrees(source.h))
}

/// How much of a person's chroma their face keeps while they are idle or
/// away ([`PresencePalette::idle_on`]): enough to tell whose face it is,
/// little enough to read as quieter than everyone present.
pub const IDLE_CHROMA: f32 = 0.35;

/// How far toward the ground an idle face's colour moves at most
/// ([`PresencePalette::idle_on`]), as a share of the way.
pub const IDLE_FADE: f32 = 0.45;

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
    /// colours): the same twelve hues, walked so the first six are at least
    /// 60° apart from each other (consecutive ones mostly 120°–180°), the
    /// next six fill the gaps, and the walk wraps past [`PRESENCE_LEN`].
    /// Equal lightness and chroma keep every series equally loud.
    #[must_use]
    pub fn series(&self, index: usize) -> Color {
        const WALK: [usize; PRESENCE_LEN] = [8, 2, 4, 10, 6, 0, 7, 1, 3, 9, 5, 11];
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
    /// ground. A grey identity has no hue and stays grey at the palette's
    /// lightness: it is someone whose colour is not known yet
    /// (`ui::presence::UNSETTLED_RGB`), and a hue picked for them would be
    /// a colour they then change from.
    #[must_use]
    pub fn of(&self, identity: Color) -> Color {
        let source = Oklch::from_color(identity);
        if source.c < 0.02 {
            return oklch(self.lightness, 0.0, 0.0);
        }
        oklch(self.lightness, self.chroma, source.h)
    }

    /// A person's colour as the ring of their face on `ground` (`identity`
    /// as [`Self::of`] takes it): their palette colour, moved in lightness
    /// away from the ground, hue and chroma kept, only where it would not
    /// otherwise stand out from it as a mark ([`NON_TEXT_MIN_CONTRAST`]).
    /// The palette is tuned for windows and floating surfaces; a face on the
    /// desktop's ground (the top bar) keeps its ring legible there too.
    #[must_use]
    pub fn ring_on(&self, identity: Color, ground: Color) -> Color {
        use crate::color::contrast_ratio;
        let person = Oklch::from_color(self.of(identity));
        let ground_l = Oklch::from_color(ground).l;
        let at = |l: f32| oklch(l.clamp(0.0, 1.0), person.c, person.h);
        let holds = |l: f32| contrast_ratio(at(l), ground) >= NON_TEXT_MIN_CONTRAST;
        if holds(person.l) {
            return self.of(identity);
        }
        let away = if ground_l < 0.5 { 1.0 } else { 0.0 };
        let mut lightness = person.l;
        for step in 1..=40_u8 {
            lightness = person.l + (away - person.l) * f32::from(step) / 40.0;
            if holds(lightness) {
                break;
            }
        }
        at(lightness)
    }

    /// A person's colour while they are idle or away (`identity` as
    /// [`Self::of`] takes it), on `ground`: their hue kept, most of its
    /// chroma let go ([`IDLE_CHROMA`]), and moved toward the ground
    /// ([`IDLE_FADE`] of the way at most) only as far as it stays a mark
    /// against it ([`NON_TEXT_MIN_CONTRAST`]). A face dims without ever
    /// vanishing into the bar it sits on, whatever the hue and theme; where
    /// even the quieted colour would miss the floor, it moves away from the
    /// ground until it holds.
    #[must_use]
    pub fn idle_on(&self, identity: Color, ground: Color) -> Color {
        use crate::color::contrast_ratio;
        let person = Oklch::from_color(self.of(identity));
        let ground_l = Oklch::from_color(ground).l;
        let chroma = person.c * IDLE_CHROMA;
        let at = |l: f32| oklch(l.clamp(0.0, 1.0), chroma, person.h);
        let holds = |l: f32| contrast_ratio(at(l), ground) >= NON_TEXT_MIN_CONTRAST;
        let mut lightness = person.l;
        if holds(lightness) {
            for step in 1..=20_u8 {
                let toward = person.l + (ground_l - person.l) * IDLE_FADE * f32::from(step) / 20.0;
                if !holds(toward) {
                    break;
                }
                lightness = toward;
            }
        } else {
            let away = if ground_l < 0.5 { 1.0 } else { 0.0 };
            for step in 1..=20_u8 {
                lightness = person.l + (away - person.l) * f32::from(step) / 20.0;
                if holds(lightness) {
                    break;
                }
            }
        }
        at(lightness)
    }

    /// [`Self::of`] for the `(r, g, b)` tuples presence identities are
    /// stored as.
    #[must_use]
    pub fn of_rgb(&self, (r, g, b): (u8, u8, u8)) -> Color {
        self.of(Color::from_rgb8(r, g, b))
    }
}

/// What is drawn straight onto media (video, a camera viewfinder, a photo):
/// the letterbox around a frame, the fade under text that sits on the
/// picture, and that text. Media is dark or light regardless of the theme,
/// so these are the same in every theme; only the hue of the neutral
/// material follows it. Controls over media are a floating Panel on the
/// material instead, inked from the theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MediaColors {
    /// Behind a frame that does not fill its box. Near-black in every
    /// theme, so a light theme does not turn the letterbox mid-grey.
    pub letterbox: Color,
    /// The colour of a fade under text on media
    /// (`ui::overlay::scrim::vertical_fade`).
    pub scrim: Color,
    /// Primary ink on [`Self::scrim`] and on the letterbox.
    pub ink: Color,
    /// Secondary ink on [`Self::scrim`] and on the letterbox.
    pub ink_dim: Color,
    /// A composition guide drawn over a live picture (thirds, a level
    /// line, a crop frame's grid): media ink, translucent so the picture
    /// stays the subject.
    pub guide: Color,
}

impl MediaColors {
    /// Lightness of the letterbox and of the fade under media text.
    const GROUND_L: f32 = 0.13;
    /// Lightness of primary and secondary ink on it: AA for body text at
    /// the full fade (`media_ink_is_legible_on_its_scrim`).
    const INK_L: f32 = 0.97;
    const INK_DIM_L: f32 = 0.80;

    /// The media set in the neutral material's `hue` at no chroma: media
    /// takes no temperature from the workspace.
    #[must_use]
    pub fn new(hue: f32) -> Self {
        let n = |lightness: f32| oklch(lightness, 0.0, hue);
        Self {
            letterbox: n(Self::GROUND_L * 0.5),
            scrim: n(Self::GROUND_L),
            ink: n(Self::INK_L),
            ink_dim: n(Self::INK_DIM_L),
            guide: n(Self::INK_L).with_alpha(Self::GUIDE_ALPHA),
        }
    }

    /// How much of the picture a composition guide lets through.
    const GUIDE_ALPHA: f32 = 0.55;
}

/// How strongly a [`Colors::wash_on`] status wash shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WashStrength {
    /// A row at rest: an added line, a failed check.
    Rest,
    /// The same row while it is selected or under review: the selection
    /// lift, still in the status hue.
    Selected,
}

/// A status wash resolved on its ground: an opaque body and the inks that
/// read on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wash {
    /// Opaque body, already composited over the ground.
    pub fill: Color,
    /// Primary ink on [`Self::fill`] (body-text contrast).
    pub ink: Color,
    /// Secondary ink on [`Self::fill`] (body-text contrast).
    pub ink_dim: Color,
    /// The status colour itself as a mark on [`Self::fill`] (a `+` gutter
    /// sign, a status dot): non-text contrast.
    pub mark: Color,
}

#[derive(Clone, Debug, PartialEq)]
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
    /// Ink, scrim and letterbox for what is drawn straight onto media.
    pub media: MediaColors,
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

/// The accent fill on a palette, as `(rest, hover, pressed)` lightness, one
/// chroma, and whether its label is white (else the window ground). On the
/// stock grounds the label is white, so every fill stays dark enough for
/// white to read on it (WCAG AA) at every hue while standing off the window
/// (3:1); hover and press step away from the label. A lifted dark ladder
/// (Soft) has no such band, so its accent is light with the window ground
/// as its label. High contrast keeps the window ground as the label, at AAA.
fn accent_fill(palette: &Palette) -> ([f32; 3], f32, bool) {
    match (palette.dark, palette.high_contrast) {
        (true, false) if palette.surfaces.content <= 0.20 => ([0.525, 0.495, 0.465], 0.15, true),
        (true, false) => ([0.72, 0.76, 0.68], 0.13, false),
        (false, false) => ([0.50, 0.465, 0.43], 0.15, true),
        (true, true) => ([0.86, 0.90, 0.82], 0.12, false),
        (false, true) => ([0.36, 0.32, 0.29], 0.13, false),
    }
}

/// Lightness of [`Intents::accent_ink`]: the presence tone on dark, a step
/// darker on light so a link reads as body text on the window.
fn accent_ink_lightness(palette: &Palette, presence: Tone) -> f32 {
    if palette.dark {
        presence.lightness
    } else {
        presence.lightness.min(0.52)
    }
}

/// The on-state green's lightness and chroma on a palette of each polarity:
/// light enough to read as green on the window, dark enough that a white
/// knob stands on it (WCAG 1.4.11).
fn on_state_tone(palette: &Palette) -> Tone {
    match (palette.dark, palette.high_contrast) {
        (true, false) => Tone::new(0.62, 0.15),
        (false, false) => Tone::new(0.58, 0.15),
        (true, true) => Tone::new(0.52, 0.15),
        (false, true) => Tone::new(0.50, 0.15),
    }
}

/// The OKLCH hue of the on state: green.
pub const ON_STATE_HUE: f32 = 150.0;

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
        let (hue, chroma) = palette.temperature();
        let n = |lightness: f32| oklch(lightness, chroma, hue);
        let ladder = palette.surfaces;
        let content = n(ladder.content);
        let overlay = n(ladder.overlay);
        let primary = n(palette.ink.primary);
        let presence_tone = presence;
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
        let accent_hue = match emphasis {
            Emphasis::Inverted => None,
            Emphasis::You => Some(you.degrees()),
            Emphasis::Tinted { hue: tint } => Some(tint.rem_euclid(360.0)),
        };
        let (accent, accent_hover, accent_pressed, on_accent, accent_ink, lift_chroma, lift_hue) =
            match accent_hue {
                None => {
                    let hover = oklch(
                        palette.ink.primary + (ladder.content - palette.ink.primary) * 0.12,
                        chroma,
                        hue,
                    );
                    let pressed = oklch(
                        palette.ink.primary + (ladder.content - palette.ink.primary) * 0.24,
                        chroma,
                        hue,
                    );
                    let ink = oklch(
                        accent_ink_lightness(palette, presence_tone),
                        presence.chroma,
                        you.degrees(),
                    );
                    (primary, hover, pressed, content, ink, chroma, hue)
                }
                Some(accent_hue) => {
                    let ([rest, hover, pressed], fill_chroma, white_label) = accent_fill(palette);
                    let label = if white_label { Color::WHITE } else { content };
                    (
                        oklch(rest, fill_chroma, accent_hue),
                        oklch(hover, fill_chroma, accent_hue),
                        oklch(pressed, fill_chroma, accent_hue),
                        label,
                        oklch(
                            accent_ink_lightness(palette, presence_tone),
                            presence.chroma,
                            accent_hue,
                        ),
                        TINTED_LIFT_CHROMA,
                        accent_hue,
                    )
                }
            };
        let on_state = on_state_tone(palette);
        let lift = |t: f32, tinted: bool| {
            let (c, h) = if tinted {
                (lift_chroma, lift_hue)
            } else {
                (chroma, hue)
            };
            oklch(ladder.content + palette.lift() * t, c, h)
        };
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
                scrim_strength: palette.scrim_strength(),
                material: if opaque {
                    overlay
                } else {
                    overlay.with_alpha(MATERIAL_ALPHA)
                },
                material_blur_px: if opaque { 0.0 } else { MATERIAL_BLUR_PX },
                material_opacity: if opaque { 1.0 } else { MATERIAL_ALPHA },
                see_through_opacity: if opaque { 1.0 } else { SEE_THROUGH_ALPHA },
                content_edge,
            },
            intent: Intents {
                accent,
                accent_hover,
                accent_pressed,
                on_accent,
                accent_ink,
                on_state: oklch(on_state.lightness, on_state.chroma, ON_STATE_HUE),
                on_state_mark: Color::WHITE,
                value_fill: value_fill_on(accent_ink, content, lift(1.0, true)),
                selection: lift(1.0, true),
                selection_hover: lift(0.5, false),
                you: you_colour,
                selection_tint: Self::selection_tint_of(accent_ink),
            },
            presence,
            media: MediaColors::new(hue),
            text_primary: primary,
            text_dim: n(palette.ink.secondary),
            text_tertiary: n(palette.ink.tertiary),
            border_standard: n(palette.border()),
            border_focus: accent_ink,
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

    /// A palette with its stock presence tone, its stock emphasis and the
    /// default person's colour.
    #[must_use]
    pub fn stock(palette: &Palette) -> Self {
        Self::of_palette(
            palette,
            Emphasis::stock_for(palette),
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
            hue: Some(hue),
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

    /// Make `hue` the person's colour, painted at this palette's presence
    /// lightness and chroma. Under [`Emphasis::You`] it is the accent too,
    /// so primary actions, the focus ring, the caret and the selection take
    /// it.
    pub fn set_you(&mut self, hue: PresenceHue) {
        if self.emphasis == Emphasis::You {
            let presence = Tone::new(self.presence.lightness, self.presence.chroma);
            let mut next = Self::of_palette(&self.palette, self.emphasis, presence, hue);
            next.set_material(
                self.surfaces.material_blur_px,
                self.surfaces.material_opacity,
            );
            *self = next;
            return;
        }
        self.you = hue;
        self.intent.you = self.presence.hue(hue);
        if self.emphasis == Emphasis::Inverted {
            let ink = Oklch::from_color(self.intent.accent_ink);
            let ink = oklch(ink.l, self.presence.chroma, hue.degrees());
            self.intent.accent_ink = ink;
            self.intent.selection_tint = Self::selection_tint_of(ink);
            self.border_focus = ink;
        }
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

    /// The neutral lift on any `ground`: `t` 1.0 is one surface step
    /// (lighter on a dark palette, darker on a light one), 0.5 the hover
    /// halfway. Hover and press are lifts and never take the accent; a
    /// selected row takes it through [`Self::selected_on`].
    ///
    /// The step goes the palette's way unless the ground has no room for a
    /// whole step there (a white well on a dark palette, a near-black one on
    /// a light palette); then it goes the other way, so the lift always moves
    /// a full step away from the ground it stands on and stays visible.
    #[must_use]
    pub fn lift_on(&self, ground: Color, t: f32) -> Color {
        let g = Oklch::from_color(ground);
        let (hue, chroma) = self.palette.temperature();
        let step = self.palette.lift();
        let step = if (0.0..=1.0).contains(&(g.l + step)) {
            step
        } else {
            -step
        };
        oklch(g.l + step * t, chroma, hue)
    }

    /// The selected row's lift on `ground`: a fifth past one surface step
    /// ([`SELECTED_LIFT_T`]), faintly the accent hue (neutral under
    /// [`Emphasis::Inverted`]).
    #[must_use]
    pub fn selected_on(&self, ground: Color) -> Color {
        let lift = self.lift_on(ground, SELECTED_LIFT_T);
        match self.accent_hue() {
            Some(hue) => {
                let o = Oklch::from_color(lift);
                oklch(o.l, TINTED_LIFT_CHROMA, hue)
            }
            None => lift,
        }
    }

    /// The accent's hue, or `None` under [`Emphasis::Inverted`].
    #[must_use]
    pub fn accent_hue(&self) -> Option<f32> {
        match self.emphasis {
            Emphasis::Inverted => None,
            Emphasis::You => Some(self.you.degrees()),
            Emphasis::Tinted { hue } => Some(hue.rem_euclid(360.0)),
        }
    }

    /// A status wash of `tone` over `ground`: an added or removed line, a
    /// failed row, a reviewer's band. `Selected` takes the selection lift
    /// of `ground` and keeps the wash's hue on it, where
    /// [`Self::lift_on`] alone would drop to the neutral. The inks clear
    /// body-text contrast on the composited body, so text keeps reading
    /// under every strength on every ground.
    #[must_use]
    pub fn wash_on(
        &self,
        ground: Color,
        tone: crate::widget::Tone,
        strength: WashStrength,
    ) -> Wash {
        use crate::widget::Tone as T;
        let tint = match tone {
            T::Neutral => self.text_dim,
            T::Success => self.semantic.success,
            T::Warning => self.semantic.warning,
            T::Danger => self.semantic.danger,
            T::Info => self.semantic.info,
            T::Presence(hue) => self.presence.hue(hue),
        };
        let (base, amount) = match strength {
            WashStrength::Rest => (ground, Self::WASH_REST),
            WashStrength::Selected => (self.lift_on(ground, 1.0), Self::WASH_SELECTED),
        };
        let fill = mix8(base, tint, amount);
        let on = self.on_surface(fill);
        Wash {
            fill,
            ink: on.ink,
            ink_dim: on.ink_dim,
            mark: self.ink_on(fill, tint, NON_TEXT_MIN_CONTRAST),
        }
    }

    /// How much of the status hue a resting wash carries.
    const WASH_REST: f32 = 0.16;
    /// How much of it a selected wash carries over the lift.
    const WASH_SELECTED: f32 = 0.30;

    /// What [`Surfaces::raised`] is on `ground`: the raised tier itself
    /// while it stands off `ground` the same way it stands off the content
    /// surface, by at least half that step (so on every region of the
    /// window); otherwise that step taken from `ground` (the other way where
    /// the ground has no room). A field, a secondary button or a checkbox on
    /// a floating panel lighter than the raised tier stands off it as it
    /// does off content, instead of sinking into it.
    #[must_use]
    pub fn raised_on(&self, ground: Color) -> Color {
        let step =
            Oklch::from_color(self.surfaces.raised).l - Oklch::from_color(self.surfaces.content).l;
        let g = Oklch::from_color(ground);
        let off = Oklch::from_color(self.surfaces.raised).l - g.l;
        if ground == self.surfaces.content
            || step.abs() <= f32::EPSILON
            || (off * step > 0.0 && off.abs() >= step.abs() / 2.0)
        {
            return self.surfaces.raised;
        }
        let l = if (0.0..=1.0).contains(&(g.l + step)) {
            g.l + step
        } else {
            g.l - step
        };
        let raised = Oklch::from_color(self.surfaces.raised);
        oklch(l, raised.c, raised.h)
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

    /// The colour a lift moves *toward*.
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

    /// The edge of the window that has keyboard input: the same neutral
    /// hairline as [`Self::window_edge`], heavier and opaque. Neutral because
    /// which window has input is state, not you: in your colour it put the
    /// accent around every focused window all day (HIG 9.4).
    pub fn window_edge_focused(&self) -> Color {
        let ground = self.surfaces.base;
        composite(Self::hairline_on(ground, FOCUSED_WINDOW_EDGE_ALPHA), ground)
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

    #[test]
    fn media_ink_is_legible_on_its_scrim_in_every_theme() {
        // Media is the same picture under a light and a dark theme, so the
        // ink drawn on it may not follow the theme's lightness.
        let dark = Colors::stock(&Palette::dark()).media;
        for (name, palette) in palettes() {
            let media = Colors::stock(&palette).media;
            for (ink, floor, role) in [(media.ink, 7.0, "ink"), (media.ink_dim, 4.5, "ink_dim")] {
                for ground in [media.scrim, media.letterbox] {
                    let ratio = contrast_ratio(ink, ground);
                    assert!(
                        ratio >= floor,
                        "{name}: media {role} on its ground is {ratio:.2}:1, needs {floor}:1"
                    );
                }
            }
            assert_eq!(
                rgba(media.letterbox),
                rgba(dark.letterbox),
                "{name}: the letterbox may not follow the theme's lightness"
            );
        }
    }

    fn at_hue(palette: &Palette, hue: f32) -> Colors {
        Colors::stock(&Palette {
            hue: Some(hue),
            ..*palette
        })
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
                    [0.14, 0.19, 0.165, 0.165, 0.235, 0.225],
                    [0.96, 0.76, 0.62],
                ),
                (
                    Palette::dark_soft(),
                    [0.19, 0.25, 0.228, 0.27, 0.30, 0.32],
                    [0.96, 0.78, 0.65],
                ),
                (
                    Palette::light(),
                    [0.90, 0.995, 0.965, 0.965, 0.95, 1.0],
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
                    ]);
                    neutrals.extend(c.neutral.stops.iter().map(|s| ("ramp", *s)));
                    // The selection is the one lift that carries the accent's
                    // faint tint (design language, "Lifts"), never more.
                    let selection = Oklch::from_color(c.intent.selection);
                    assert!(
                        selection.c <= TINTED_LIFT_CHROMA.max(palette.chroma) + 0.004,
                        "{label}@{hue}: selection has chroma {:.4}",
                        selection.c
                    );
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
        // phase: the_default_carries_a_cool_temperature_not_a_colour
        {
            for c in [Colors::dark(), Colors::light()] {
                for (name, colour) in surfaces(&c) {
                    let o = Oklch::from_color(colour);
                    assert!(o.c <= 0.010, "{name} has chroma {:.4}", o.c);
                    if o.c >= 0.004 && o.l > 0.1 && o.l < 0.97 {
                        let d = (o.h - 260.0).rem_euclid(360.0);
                        let d = d.min(360.0 - d);
                        assert!(d <= 30.0, "{name} is at hue {:.0}, not cool", o.h);
                    }
                }
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
        // phase: floating_chrome_stands_lighter_than_the_window_on_dark
        {
            for (label, palette) in palettes() {
                if !palette.dark || palette.high_contrast {
                    continue;
                }
                for hue in hues() {
                    let c = at_hue(&palette, hue);
                    let l = |x: Color| Oklch::from_color(x).l;
                    let s = &c.surfaces;
                    for (name, under) in [("base", s.base), ("content", s.content)] {
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
                // Dark: ground, region (recessed), window, then the raised
                // wells and floating chrome above it. Light: ground, the
                // raised well (0.95, darker than the region), the region
                // (0.965), then the window, and floating as the one white
                // (design-system.md, "Surfaces"). Sidebar and inspector are
                // one level.
                let order: &[Color] = if palette.dark {
                    &[s.base, s.sidebar, s.content, s.overlay]
                } else {
                    &[s.base, s.raised, s.sidebar, s.content, s.overlay]
                };
                for pair in order.windows(2) {
                    if let [a, b] = pair {
                        assert!(l(*a) <= l(*b), "{label}: surfaces out of order");
                    }
                }
                if palette.dark {
                    assert!(
                        l(s.raised) > l(s.content),
                        "{label}: raised over the window"
                    );
                }
                if label == "dark" || label == "light" {
                    assert_eq!(
                        rgba(s.sidebar),
                        rgba(s.inspector),
                        "{label}: one region level"
                    );
                }
            }
        }
    }

    #[test]
    fn every_shipped_palette_needs_no_repair_at_any_temperature_or_emphasis() {
        for (label, palette) in palettes() {
            for hue in std::iter::once(None).chain(hues().map(Some)) {
                let palette = Palette { hue, ..palette };
                let mut emphases = vec![Emphasis::Inverted, Emphasis::You];
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
                        "{label}@{hue:?} {emphasis:?}: {}",
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
            for emphasis in [
                Emphasis::Inverted,
                Emphasis::You,
                Emphasis::Tinted { hue: 150.0 },
            ] {
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
    fn the_accent_colours_actions_and_selection_and_nothing_else() {
        let of = |emphasis| {
            Colors::of_palette(
                &Palette::dark(),
                emphasis,
                Palette::dark().stock_presence(),
                PresenceHue::Blue,
            )
        };
        let tinted = of(Emphasis::Tinted { hue: 30.0 });
        let plain = of(Emphasis::Inverted);
        let fill = Oklch::from_color(tinted.intent.accent);
        assert!(fill.c > 0.08, "the primary fill carries the accent");
        assert!((fill.h - 30.0).abs() < 6.0, "at its hue: {:.0}", fill.h);
        let lift = Oklch::from_color(tinted.intent.selection);
        assert!(lift.c > 0.02, "the selection lift is tinted");
        assert_eq!(rgba(tinted.surfaces.content), rgba(plain.surfaces.content));
        assert_eq!(rgba(tinted.text_primary), rgba(plain.text_primary));
        assert_eq!(rgba(tinted.intent.you), rgba(plain.intent.you));
        assert_eq!(
            rgba(tinted.intent.on_state),
            rgba(plain.intent.on_state),
            "the on state is green whatever the accent"
        );
        let plain_fill = Oklch::from_color(plain.intent.accent);
        assert!(plain_fill.c < 0.01, "inverted emphasis has no hue");
        let stock = Colors::dark();
        assert_eq!(stock.emphasis, Emphasis::You);
        let you = Oklch::from_color(stock.intent.accent);
        let blue = PresenceHue::Blue.degrees();
        let d = (you.h - blue).rem_euclid(360.0);
        assert!(d.min(360.0 - d) <= 12.0, "accent hue {:.0}", you.h);
        let on = Oklch::from_color(stock.intent.on_state);
        assert!(
            (on.h - ON_STATE_HUE).abs() < 8.0 && on.c > 0.1,
            "on is green"
        );
    }

    #[test]
    fn primary_labels_on_dark_and_light_are_white_and_read() {
        for palette in [Palette::dark(), Palette::light()] {
            for you in PresenceHue::ALL {
                let c = Colors::as_authored(&palette, Emphasis::You, palette.stock_presence(), you);
                assert_eq!(rgba(c.intent.on_accent), [255, 255, 255, 255]);
                for fill in [
                    c.intent.accent,
                    c.intent.accent_hover,
                    c.intent.accent_pressed,
                ] {
                    let r = contrast_ratio(c.intent.on_accent, fill);
                    assert!(r >= 4.5, "{you:?}: label {r:.2}:1");
                }
                let mark = contrast_ratio(c.intent.on_state_mark, c.intent.on_state);
                assert!(mark >= 3.0, "the knob on the on state is {mark:.2}:1");
            }
        }
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
        assert_eq!(nearest_presence_hue(orange), Some(PresenceHue::Orange));
        assert_eq!(nearest_presence_hue(Color::from_rgb8(128, 128, 128)), None);
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
            assert_eq!(warm.palette.hue, Some(40.0));
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
            let grey = Colors::stock(&Palette {
                hue: None,
                chroma: 0.0,
                ..Palette::dark()
            });
            let warm = grey.with_neutral_hue(40.0);
            assert_eq!(rgba(warm.surfaces.content), rgba(grey.surfaces.content));
            assert_eq!(warm.palette.hue, grey.palette.hue);
        }
        // phase: a_workspace_moves_the_stock_temperature_to_its_hue
        {
            let light = Colors::light();
            let warm = light.with_neutral_hue(40.0);
            let h = Oklch::from_color(warm.surfaces.base);
            assert!(h.c > 0.003, "a known hue keeps the temperature");
            assert!((h.h - 40.0).abs() <= 25.0, "base hue {:.0}", h.h);
            assert_ne!(rgba(warm.surfaces.base), rgba(light.surfaces.base));
        }
        // phase: your_colour_is_the_accent_focus_caret_and_selection
        {
            let mut c = Colors::light();
            c.set_you(PresenceHue::Teal);
            assert_eq!(c.you, PresenceHue::Teal);
            assert_eq!(rgba(c.intent.you), rgba(c.presence.hue(PresenceHue::Teal)));
            assert_eq!(rgba(c.border_focus), rgba(c.intent.accent_ink));
            let teal = PresenceHue::Teal.degrees();
            for (name, colour) in [
                ("accent", c.intent.accent),
                ("accent ink", c.intent.accent_ink),
            ] {
                let o = Oklch::from_color(colour);
                let d = (o.h - teal).rem_euclid(360.0);
                assert!(d.min(360.0 - d) <= 12.0, "{name} is at hue {:.0}", o.h);
            }
            assert_eq!(c.intent.selection_tint.a(), 82);
            let tint = c.intent.selection_tint;
            assert_eq!(
                [tint.r(), tint.g(), tint.b()],
                [
                    c.intent.accent_ink.r(),
                    c.intent.accent_ink.g(),
                    c.intent.accent_ink.b()
                ]
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
                ("accent", c.intent.accent, c.intent.on_accent),
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

    #[test]
    fn a_selected_status_wash_keeps_its_hue_and_its_ink_reads() {
        use crate::widget::Tone;
        for (label, palette) in palettes() {
            let colors = Colors::stock(&palette);
            let ground = colors.surfaces.content;
            for tone in [Tone::Success, Tone::Danger, Tone::Warning, Tone::Info] {
                let rest = colors.wash_on(ground, tone, WashStrength::Rest);
                let selected = colors.wash_on(ground, tone, WashStrength::Selected);
                let lift = colors.lift_on(ground, 1.0);
                // Selection stays in the status hue instead of falling to the
                // neutral lift.
                let chroma = |c: Color| Oklch::from_color(c).c;
                assert!(
                    chroma(selected.fill) > chroma(lift) + 0.01,
                    "{label}/{tone:?}: selected wash lost its hue",
                );
                assert_ne!(rgba(rest.fill), rgba(selected.fill), "{label}/{tone:?}");
                for wash in [rest, selected] {
                    assert!(
                        contrast_ratio(wash.ink, wash.fill) >= 4.5,
                        "{label}/{tone:?}"
                    );
                    assert!(
                        contrast_ratio(wash.ink_dim, wash.fill) >= 4.5,
                        "{label}/{tone:?}"
                    );
                    assert!(
                        contrast_ratio(wash.mark, wash.fill) >= 3.0,
                        "{label}/{tone:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_media_guide_is_translucent_media_ink() {
        let media = MediaColors::new(0.0);
        assert!(media.guide.components[3] < 1.0);
        assert!(media.guide.components[3] > 0.3);
        assert_eq!(rgba(media.guide.with_alpha(1.0)), rgba(media.ink));
    }
}
