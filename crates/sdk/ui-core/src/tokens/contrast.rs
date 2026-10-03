//! The legibility half of the colour tokens.
//!
//! [`FLOORS`] is the one table of contrast promises a theme makes to every
//! app: which foreground role must clear which ratio on which ground. Every
//! material is swept against it at every temperature in tests, so a
//! workspace hue can never cost an app its legibility. High contrast holds
//! every text floor at WCAG AAA ([`HIGH_CONTRAST_TEXT_MIN_CONTRAST`]).

use super::colors::{Colors, Material, NON_TEXT_MIN_CONTRAST};
use crate::color::contrast_ratio;
use vello::peniko::Color;

/// WCAG 2.x AA for body text.
pub const BODY_TEXT_MIN_CONTRAST: f64 = 4.5;

/// WCAG 2.x AAA for body text: what every text floor becomes on the
/// high-contrast material.
pub const HIGH_CONTRAST_TEXT_MIN_CONTRAST: f64 = 7.0;

/// A colour role a contrast floor is stated in. Grounds are surfaces;
/// everything else is a foreground drawn on them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    Base,
    Content,
    Sidebar,
    Inspector,
    Raised,
    Overlay,
    Selection,
    TextPrimary,
    TextDim,
    TextTertiary,
    Inverted,
    OnInverted,
    /// The person's own colour as their focus ring.
    Focus,
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
            Self::TextPrimary => "text_primary",
            Self::TextDim => "text_dim",
            Self::TextTertiary => "text_tertiary",
            Self::Inverted => "intent.inverted",
            Self::OnInverted => "intent.on_inverted",
            Self::Focus => "intent.you",
            Self::Success => "semantic.success",
            Self::Warning => "semantic.warning",
            Self::Danger => "semantic.danger",
            Self::Info => "semantic.info",
        }
    }

    /// The colour this role names in `colors`.
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
            Self::TextPrimary => colors.text_primary,
            Self::TextDim => colors.text_dim,
            Self::TextTertiary => colors.text_tertiary,
            Self::Inverted => colors.intent.inverted,
            Self::OnInverted => colors.intent.on_inverted,
            Self::Focus => colors.intent.you,
            Self::Success => colors.semantic.success,
            Self::Warning => colors.semantic.warning,
            Self::Danger => colors.semantic.danger,
            Self::Info => colors.semantic.info,
        }
    }

    /// Whether this foreground is read as text, so the high-contrast
    /// material holds it to AAA.
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

/// Every promise, in the order findings are reported. Primary and secondary
/// ink are body text on every surface; tertiary ink is large-text quiet on
/// every surface and body text on content, where it is defined.
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
    // A selected row hosts the row's own ink.
    floor(Role::TextPrimary, Role::Selection, BODY_TEXT_MIN_CONTRAST),
    floor(Role::TextDim, Role::Selection, BODY_TEXT_MIN_CONTRAST),
    // A primary action: its label on its fill, its fill on the window.
    floor(Role::OnInverted, Role::Inverted, BODY_TEXT_MIN_CONTRAST),
    floor(Role::Inverted, Role::Content, NON_TEXT_MIN_CONTRAST),
    // Focus indicator (WCAG 2.4.11), on everything a focusable control
    // sits on.
    floor(Role::Focus, Role::Content, NON_TEXT_MIN_CONTRAST),
    floor(Role::Focus, Role::Raised, NON_TEXT_MIN_CONTRAST),
    floor(Role::Focus, Role::Overlay, NON_TEXT_MIN_CONTRAST),
    // Status is ink on a neutral control and a dot on the desktop.
    floor(Role::Success, Role::Content, BODY_TEXT_MIN_CONTRAST),
    floor(Role::Warning, Role::Content, BODY_TEXT_MIN_CONTRAST),
    floor(Role::Danger, Role::Content, BODY_TEXT_MIN_CONTRAST),
    floor(Role::Info, Role::Content, BODY_TEXT_MIN_CONTRAST),
    floor(Role::Success, Role::Base, NON_TEXT_MIN_CONTRAST),
    floor(Role::Warning, Role::Base, NON_TEXT_MIN_CONTRAST),
    floor(Role::Danger, Role::Base, NON_TEXT_MIN_CONTRAST),
    floor(Role::Info, Role::Base, NON_TEXT_MIN_CONTRAST),
];

impl ContrastFloor {
    /// The ratio this floor asks of `material`: text floors rise to AAA on
    /// high contrast.
    #[must_use]
    pub fn min_ratio_on(&self, material: Material) -> f64 {
        if material == Material::HighContrast && self.foreground.is_text() {
            self.min_ratio.max(HIGH_CONTRAST_TEXT_MIN_CONTRAST)
        } else {
            self.min_ratio
        }
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
            let min_ratio = f.min_ratio_on(colors.material);
            let ratio = contrast_ratio(f.foreground.of(colors), f.ground.of(colors));
            (ratio < min_ratio).then_some(ContrastFinding {
                foreground: f.foreground,
                ground: f.ground,
                ratio,
                min_ratio,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::colors::DEFAULT_NEUTRAL_HUE;

    #[test]
    fn every_material_keeps_every_floor_at_every_temperature() {
        for material in [Material::Dark, Material::Light, Material::HighContrast] {
            for step in 0..36_u16 {
                let hue = f32::from(step) * 10.0;
                let mut colors = Colors::of_material(material, hue);
                let findings = audit(&colors);
                assert!(
                    findings.is_empty(),
                    "{material:?}@{hue}: {}",
                    findings
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                // Whoever sits here, their colour still reads as a focus ring.
                for slot in 0..crate::tokens::colors::PRESENCE_LEN {
                    let identity = colors.presence.get(slot);
                    colors.set_you(identity);
                    let focus: Vec<_> = audit(&colors)
                        .into_iter()
                        .filter(|f| f.foreground == Role::Focus)
                        .collect();
                    assert!(focus.is_empty(), "{material:?} slot {slot}: {focus:?}");
                }
            }
        }
    }

    #[test]
    fn a_failing_palette_names_the_pair_and_the_shortfall() {
        let mut murky = Colors::of_material(Material::Dark, DEFAULT_NEUTRAL_HUE);
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
}
