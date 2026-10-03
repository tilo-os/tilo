//! The legibility half of the colour tokens.
//!
//! [`FLOORS`] is the one table of contrast promises a theme makes to every
//! app: which foreground role must clear which ratio on which ground.
//! [`audit`] measures a colour set against it, Theme Studio refuses to save
//! a document with findings, and [`repair`] steps any foreground that still
//! misses (a hand-edited file) out until it holds, so a theme that reaches
//! the screen always keeps every floor. A high-contrast palette holds every
//! text floor at WCAG AAA ([`HIGH_CONTRAST_TEXT_MIN_CONTRAST`]).

use super::colors::{Colors, NON_TEXT_MIN_CONTRAST, PRESENCE_LEN, mix8};
use crate::color::{Oklch, contrast_ratio, oklch};
use vello::peniko::Color;

/// WCAG 2.x AA for body text.
pub const BODY_TEXT_MIN_CONTRAST: f64 = 4.5;

/// WCAG 2.x AAA for body text: what every text floor becomes on a
/// high-contrast palette.
pub const HIGH_CONTRAST_TEXT_MIN_CONTRAST: f64 = 7.0;

/// A colour role a contrast floor is stated in. Grounds are surfaces and
/// the selection lift; everything else is a foreground drawn on them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    Base,
    Content,
    Sidebar,
    Inspector,
    Raised,
    Overlay,
    Selection,
    SelectionHover,
    TextPrimary,
    TextDim,
    TextTertiary,
    Inverted,
    InvertedHover,
    OnInverted,
    /// The person's own colour as their focus ring, caret and unread mark.
    Focus,
    /// Each of the twelve presence colours: a collaborator's caret, cursor
    /// and pill. A floor on it holds for every slot.
    Presence,
    Success,
    Warning,
    Danger,
    Info,
}

impl Role {
    /// The token's name as the docs spell it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Base => "surfaces.base",
            Self::Content => "surfaces.content",
            Self::Sidebar => "surfaces.sidebar",
            Self::Inspector => "surfaces.inspector",
            Self::Raised => "surfaces.raised",
            Self::Overlay => "surfaces.overlay",
            Self::Selection => "intent.selection",
            Self::SelectionHover => "intent.selection_hover",
            Self::TextPrimary => "text_primary",
            Self::TextDim => "text_dim",
            Self::TextTertiary => "text_tertiary",
            Self::Inverted => "intent.inverted",
            Self::InvertedHover => "intent.inverted_hover",
            Self::OnInverted => "intent.on_inverted",
            Self::Focus => "intent.you",
            Self::Presence => "presence",
            Self::Success => "semantic.success",
            Self::Warning => "semantic.warning",
            Self::Danger => "semantic.danger",
            Self::Info => "semantic.info",
        }
    }

    /// The colour this role names in `colors`. [`Role::Presence`] names its
    /// least legible slot against `ground`.
    #[must_use]
    pub fn of(self, colors: &Colors) -> Color {
        match self {
            Self::Base => colors.surfaces.base,
            Self::Content => colors.surfaces.content,
            Self::Sidebar => colors.surfaces.sidebar,
            Self::Inspector => colors.surfaces.inspector,
            Self::Raised => colors.surfaces.raised,
            Self::Overlay => colors.surfaces.overlay,
            Self::Selection => colors.intent.selection,
            Self::SelectionHover => colors.intent.selection_hover,
            Self::TextPrimary => colors.text_primary,
            Self::TextDim => colors.text_dim,
            Self::TextTertiary => colors.text_tertiary,
            Self::Inverted => colors.intent.inverted,
            Self::InvertedHover => colors.intent.inverted_hover,
            Self::OnInverted => colors.intent.on_inverted,
            Self::Focus => colors.intent.you,
            Self::Presence => colors.presence.get(0),
            Self::Success => colors.semantic.success,
            Self::Warning => colors.semantic.warning,
            Self::Danger => colors.semantic.danger,
            Self::Info => colors.semantic.info,
        }
    }

    /// Whether this foreground is read as text, so a high-contrast palette
    /// holds it to AAA.
    const fn is_text(self) -> bool {
        matches!(
            self,
            Self::TextPrimary | Self::TextDim | Self::TextTertiary | Self::OnInverted
        )
    }
}

/// One promise: `foreground` clears `min_ratio` against `ground`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContrastFloor {
    pub foreground: Role,
    pub ground: Role,
    pub min_ratio: f64,
}

const fn floor(foreground: Role, ground: Role, min_ratio: f64) -> ContrastFloor {
    ContrastFloor {
        foreground,
        ground,
        min_ratio,
    }
}

const SURFACES: [Role; 6] = [
    Role::Base,
    Role::Content,
    Role::Sidebar,
    Role::Inspector,
    Role::Raised,
    Role::Overlay,
];

const fn on_every_surface(foreground: Role, min_ratio: f64) -> [ContrastFloor; SURFACES.len()] {
    [
        floor(foreground, SURFACES[0], min_ratio),
        floor(foreground, SURFACES[1], min_ratio),
        floor(foreground, SURFACES[2], min_ratio),
        floor(foreground, SURFACES[3], min_ratio),
        floor(foreground, SURFACES[4], min_ratio),
        floor(foreground, SURFACES[5], min_ratio),
    ]
}

const PRIMARY: [ContrastFloor; 6] = on_every_surface(Role::TextPrimary, BODY_TEXT_MIN_CONTRAST);
const SECONDARY: [ContrastFloor; 6] = on_every_surface(Role::TextDim, BODY_TEXT_MIN_CONTRAST);
const TERTIARY: [ContrastFloor; 6] = on_every_surface(Role::TextTertiary, NON_TEXT_MIN_CONTRAST);

/// Every promise, in the order findings are reported.
///
/// - Primary and secondary ink are body text on every surface and on a
///   selected or hovered row; tertiary ink is large-text quiet on every
///   surface and body text on content, where it is defined.
/// - A primary action's label reads on its fill at rest and under the
///   pointer, and the fill (an on switch, a filled track) stands off the
///   window (WCAG 1.4.11).
/// - A selected row is a visible step off the window.
/// - Your colour is a focus indicator (WCAG 2.4.11) on every surface of a
///   window and on floating chrome; every presence colour reads as a caret
///   or pill on the window, its fields and floating chrome.
/// - Status is ink on a neutral control and a dot on the desktop.
pub const FLOORS: &[ContrastFloor] = &[
    PRIMARY[0],
    PRIMARY[1],
    PRIMARY[2],
    PRIMARY[3],
    PRIMARY[4],
    PRIMARY[5],
    SECONDARY[0],
    SECONDARY[1],
    SECONDARY[2],
    SECONDARY[3],
    SECONDARY[4],
    SECONDARY[5],
    TERTIARY[0],
    TERTIARY[1],
    TERTIARY[2],
    TERTIARY[3],
    TERTIARY[4],
    TERTIARY[5],
    floor(Role::TextTertiary, Role::Content, BODY_TEXT_MIN_CONTRAST),
    floor(Role::TextPrimary, Role::Selection, BODY_TEXT_MIN_CONTRAST),
    floor(Role::TextDim, Role::Selection, BODY_TEXT_MIN_CONTRAST),
    floor(
        Role::TextPrimary,
        Role::SelectionHover,
        BODY_TEXT_MIN_CONTRAST,
    ),
    floor(Role::TextDim, Role::SelectionHover, BODY_TEXT_MIN_CONTRAST),
    floor(Role::Selection, Role::Content, SELECTION_STEP),
    floor(Role::OnInverted, Role::Inverted, BODY_TEXT_MIN_CONTRAST),
    floor(
        Role::OnInverted,
        Role::InvertedHover,
        BODY_TEXT_MIN_CONTRAST,
    ),
    floor(Role::Inverted, Role::Content, NON_TEXT_MIN_CONTRAST),
    floor(Role::Focus, Role::Content, NON_TEXT_MIN_CONTRAST),
    floor(Role::Focus, Role::Sidebar, NON_TEXT_MIN_CONTRAST),
    floor(Role::Focus, Role::Inspector, NON_TEXT_MIN_CONTRAST),
    floor(Role::Focus, Role::Raised, NON_TEXT_MIN_CONTRAST),
    floor(Role::Focus, Role::Overlay, NON_TEXT_MIN_CONTRAST),
    floor(Role::Presence, Role::Content, NON_TEXT_MIN_CONTRAST),
    floor(Role::Presence, Role::Raised, NON_TEXT_MIN_CONTRAST),
    floor(Role::Presence, Role::Overlay, NON_TEXT_MIN_CONTRAST),
    floor(Role::Success, Role::Content, BODY_TEXT_MIN_CONTRAST),
    floor(Role::Warning, Role::Content, BODY_TEXT_MIN_CONTRAST),
    floor(Role::Danger, Role::Content, BODY_TEXT_MIN_CONTRAST),
    floor(Role::Info, Role::Content, BODY_TEXT_MIN_CONTRAST),
    floor(Role::Success, Role::Base, NON_TEXT_MIN_CONTRAST),
    floor(Role::Warning, Role::Base, NON_TEXT_MIN_CONTRAST),
    floor(Role::Danger, Role::Base, NON_TEXT_MIN_CONTRAST),
    floor(Role::Info, Role::Base, NON_TEXT_MIN_CONTRAST),
];

/// How far a selected row must stand off the window: no hue does the work,
/// so the lightness step has to be visible on its own.
pub const SELECTION_STEP: f64 = 1.15;

impl ContrastFloor {
    /// The ratio this floor asks of `colors`: text floors rise to AAA on a
    /// high-contrast palette.
    #[must_use]
    pub fn min_ratio_on(&self, colors: &Colors) -> f64 {
        if colors.palette.high_contrast && self.foreground.is_text() {
            self.min_ratio.max(HIGH_CONTRAST_TEXT_MIN_CONTRAST)
        } else {
            self.min_ratio
        }
    }

    /// The foreground as measured: for [`Role::Presence`], the slot that
    /// reads worst on this ground.
    fn measured(&self, colors: &Colors) -> (Color, f64) {
        let ground = self.ground.of(colors);
        if self.foreground == Role::Presence {
            return (0..PRESENCE_LEN)
                .map(|slot| {
                    let c = colors.presence.get(slot);
                    (c, contrast_ratio(c, ground))
                })
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap_or((Color::BLACK, 0.0));
        }
        let fg = self.foreground.of(colors);
        (fg, contrast_ratio(fg, ground))
    }
}

/// A floor a palette misses, and by how much.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContrastFinding {
    pub foreground: Role,
    pub ground: Role,
    pub ratio: f64,
    pub min_ratio: f64,
}

impl std::fmt::Display for ContrastFinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} on {} is {:.2}:1, needs {:.1}:1",
            self.foreground.name(),
            self.ground.name(),
            self.ratio,
            self.min_ratio
        )
    }
}

/// Every floor `colors` misses, in [`FLOORS`] order. Empty means the palette
/// keeps every promise an app relies on. Alpha is ignored: every role in the
/// table is opaque.
#[must_use]
pub fn audit(colors: &Colors) -> Vec<ContrastFinding> {
    FLOORS
        .iter()
        .filter_map(|f| {
            let min_ratio = f.min_ratio_on(colors);
            let (_, ratio) = f.measured(colors);
            (ratio < min_ratio).then_some(ContrastFinding {
                foreground: f.foreground,
                ground: f.ground,
                ratio,
                min_ratio,
            })
        })
        .collect()
}

/// Step every foreground that misses a floor out, in OKLCH lightness at its
/// own hue and chroma, until it holds. Grounds never move: the ladder is the
/// theme's, and a foreground that cannot reach its floor on one ground
/// without losing another (an ink tier on a ladder that straddles it) ends
/// at the extreme that serves the hardest ground. Returns what was missed
/// before the repair.
pub fn repair(colors: &mut Colors) -> Vec<ContrastFinding> {
    let before = audit(colors);
    if before.is_empty() {
        return before;
    }
    for _ in 0..4 {
        let findings = audit(colors);
        if findings.is_empty() {
            break;
        }
        for finding in findings {
            repair_one(colors, finding);
        }
    }
    colors.border_focus = colors.intent.you;
    colors.intent.selection_tint = crate::paint::with_alpha(colors.intent.you, 82);
    before
}

/// Every floor naming `foreground`, with the ratio each asks on `colors`.
fn grounds_of(colors: &Colors, foreground: Role) -> Vec<(Color, f64)> {
    FLOORS
        .iter()
        .filter(|f| f.foreground == foreground)
        .map(|f| (f.ground.of(colors), f.min_ratio_on(colors)))
        .collect()
}

/// `colour` moved in lightness, keeping hue and chroma, until it clears
/// every `(ground, ratio)` pair, or as far as it can go toward the extreme
/// that serves the hardest ground.
fn lightness_until_legible(colour: Color, grounds: &[(Color, f64)]) -> Color {
    let holds = |c: Color| grounds.iter().all(|(g, r)| contrast_ratio(c, *g) >= *r);
    if holds(colour) {
        return colour;
    }
    let Some(hardest) = grounds
        .iter()
        .min_by(|a, b| {
            (contrast_ratio(colour, a.0) / a.1).total_cmp(&(contrast_ratio(colour, b.0) / b.1))
        })
        .map(|(g, _)| *g)
    else {
        return colour;
    };
    let source = Oklch::from_color(colour);
    let ground_l = Oklch::from_color(hardest).l;
    let target = if ground_l < 0.5 { 1.0 } else { 0.0 };
    let mut best = colour;
    for step in 1..=40_u8 {
        let l = source.l + (target - source.l) * f32::from(step) / 40.0;
        let candidate = oklch(l, source.c, source.h);
        best = candidate;
        if holds(candidate) {
            return candidate;
        }
    }
    best
}

fn repair_one(colors: &mut Colors, finding: ContrastFinding) {
    if finding.foreground == Role::Presence {
        repair_presence(colors);
        return;
    }
    // A ground that misses its floor against the window (the selection
    // step) moves itself; everything else is a foreground on fixed grounds.
    if finding.foreground == Role::Selection {
        let content = colors.surfaces.content;
        let step = if colors.palette.dark { 0.02 } else { -0.02 };
        let mut lift = Oklch::from_color(colors.intent.selection);
        for _ in 0..10 {
            if contrast_ratio(oklch(lift.l, lift.c, lift.h), content) >= SELECTION_STEP {
                break;
            }
            lift.l = (lift.l + step).clamp(0.0, 1.0);
        }
        colors.intent.selection = oklch(lift.l, lift.c, lift.h);
        colors.intent.selection_hover = mix8(content, colors.intent.selection, 0.5);
        return;
    }
    let grounds = grounds_of(colors, finding.foreground);
    let slot = match finding.foreground {
        Role::TextPrimary => &mut colors.text_primary,
        Role::TextDim => &mut colors.text_dim,
        Role::TextTertiary => &mut colors.text_tertiary,
        Role::Inverted => &mut colors.intent.inverted,
        Role::InvertedHover => &mut colors.intent.inverted_hover,
        Role::OnInverted => &mut colors.intent.on_inverted,
        Role::Focus => &mut colors.intent.you,
        Role::Success => &mut colors.semantic.success,
        Role::Warning => &mut colors.semantic.warning,
        Role::Danger => &mut colors.semantic.danger,
        Role::Info => &mut colors.semantic.info,
        _ => return,
    };
    *slot = lightness_until_legible(*slot, &grounds);
}

/// Presence colours share one lightness, so a slot that misses moves the
/// whole palette: every participant stays equally loud.
fn repair_presence(colors: &mut Colors) {
    let grounds = grounds_of(colors, Role::Presence);
    let holds = |p: &super::colors::PresencePalette| {
        grounds
            .iter()
            .all(|(g, r)| (0..PRESENCE_LEN).all(|slot| contrast_ratio(p.get(slot), *g) >= *r))
    };
    let toward = if colors.palette.dark { 0.02 } else { -0.02 };
    let mut palette = colors.presence;
    for _ in 0..20 {
        if holds(&palette) {
            break;
        }
        palette = super::colors::PresencePalette::new(
            (palette.lightness + toward).clamp(0.0, 1.0),
            palette.chroma,
        );
    }
    colors.presence = palette;
    colors.intent.you = palette.hue(colors.you);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::colors::PresenceHue;
    use crate::tokens::palette::{Emphasis, Palette};

    fn presets() -> [Palette; 7] {
        [
            Palette::dark(),
            Palette::dark_cool(),
            Palette::dark_graphite(),
            Palette::dark_soft(),
            Palette::light(),
            Palette::high_contrast_dark(),
            Palette::high_contrast_light(),
        ]
    }

    #[test]
    fn every_preset_keeps_every_floor_at_every_temperature_for_everyone() {
        for preset in presets() {
            for step in 0..36_u16 {
                let palette = Palette {
                    hue: f32::from(step) * 10.0,
                    ..preset
                };
                for you in PresenceHue::ALL {
                    let colors = Colors::as_authored(
                        &palette,
                        Emphasis::Inverted,
                        palette.stock_presence(),
                        you,
                    );
                    let findings = audit(&colors);
                    assert!(
                        findings.is_empty(),
                        "{palette:?} as {you:?}: {}",
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
    fn a_failing_palette_names_the_pair_and_the_shortfall() {
        let mut murky = Colors::dark();
        murky.text_dim = murky.surfaces.raised;
        let findings = audit(&murky);
        assert!(
            findings
                .iter()
                .any(|f| f.foreground == Role::TextDim && f.ground == Role::Raised)
        );
        let finding = ContrastFinding {
            foreground: Role::TextDim,
            ground: Role::Raised,
            ratio: 4.11,
            min_ratio: 4.5,
        };
        assert_eq!(
            finding.to_string(),
            "text_dim on surfaces.raised is 4.11:1, needs 4.5:1"
        );
    }

    #[test]
    fn repair_restores_every_floor_without_moving_a_ground() {
        // phase: murky_ink_and_a_dim_presence_palette
        {
            let mut palette = Palette::dark();
            palette.ink.secondary = 0.40;
            palette.ink.tertiary = 0.30;
            let presence = crate::tokens::palette::Tone::new(0.40, 0.14);
            let authored =
                Colors::as_authored(&palette, Emphasis::Inverted, presence, PresenceHue::Blue);
            assert!(!audit(&authored).is_empty());
            let mut repaired = authored.clone();
            let missed = repair(&mut repaired);
            assert!(!missed.is_empty(), "repair reports what it fixed");
            assert_eq!(audit(&repaired), Vec::new());
            assert_eq!(repaired.surfaces.content, authored.surfaces.content);
            assert_eq!(repaired.surfaces.overlay, authored.surfaces.overlay);
            assert_eq!(repaired.border_focus, repaired.intent.you);
            let lightness: Vec<f32> = (0..PRESENCE_LEN)
                .map(|s| (Oklch::from_color(repaired.presence.get(s)).l * 100.0).round())
                .collect();
            assert!(
                lightness.windows(2).all(|w| w.first() == w.last()),
                "presence moves as one: {lightness:?}"
            );
        }
        // phase: a_legible_set_is_left_alone
        {
            let mut colors = Colors::light();
            let before = colors.clone();
            assert!(repair(&mut colors).is_empty());
            assert_eq!(colors.text_dim, before.text_dim);
        }
    }
}
