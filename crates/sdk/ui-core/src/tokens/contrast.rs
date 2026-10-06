//! The legibility half of theme validation.
//!
//! [`FLOORS`] is the one table of contrast promises a theme makes to every
//! app: which foreground role must clear which ratio on which ground. The
//! shipped presets are swept against it in tests, Theme Studio refuses to
//! save a theme with a [`ContrastFinding`], and
//! [`crate::theme::Theme::from_custom_spec`] repairs a file that reaches the
//! runtime anyway ([`repair`]) instead of dropping the person's theme. See
//! `docs/architecture/theming.md`.

use super::colors::{Colors, NON_TEXT_MIN_CONTRAST, PaletteSpec, mix8};
use crate::color::contrast_ratio;
use vello::peniko::Color;

/// WCAG 2.x AA for body text.
pub const BODY_TEXT_MIN_CONTRAST: f64 = 4.5;

/// A colour role a contrast floor is stated in. Grounds are surfaces;
/// everything else is a foreground drawn on them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    Base,
    Raised,
    Overlay,
    TextPrimary,
    TextDim,
    TextTertiary,
    Accent,
    Focus,
    Success,
    Warning,
    Danger,
    Info,
}

impl Role {
    /// The token's name as Theme Studio and the docs spell it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Base => "surface_base",
            Self::Raised => "surface_raised",
            Self::Overlay => "surface_overlay",
            Self::TextPrimary => "text_primary",
            Self::TextDim => "text_dim",
            Self::TextTertiary => "text_tertiary",
            Self::Accent => "accent",
            Self::Focus => "focus",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Danger => "danger",
            Self::Info => "info",
        }
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

/// Labels drawn *on* an accent or semantic fill are absent on purpose: they
/// are always derived (`Colors::on_accent`, `legible_fill`,
/// `tinted_chip_on`) and cannot be authored wrong.
pub const FLOORS: &[ContrastFloor] = &[
    floor(Role::TextPrimary, Role::Base, BODY_TEXT_MIN_CONTRAST),
    floor(Role::TextPrimary, Role::Raised, BODY_TEXT_MIN_CONTRAST),
    floor(Role::TextPrimary, Role::Overlay, BODY_TEXT_MIN_CONTRAST),
    // Captions and field labels are body text too.
    floor(Role::TextDim, Role::Base, BODY_TEXT_MIN_CONTRAST),
    floor(Role::TextDim, Role::Raised, BODY_TEXT_MIN_CONTRAST),
    floor(Role::TextDim, Role::Overlay, BODY_TEXT_MIN_CONTRAST),
    // The token's documented contract: large-text AA on the ground.
    floor(Role::TextTertiary, Role::Base, NON_TEXT_MIN_CONTRAST),
    // Toggle on, slider track, checkbox tick (WCAG 1.4.11).
    floor(Role::Accent, Role::Base, NON_TEXT_MIN_CONTRAST),
    floor(Role::Accent, Role::Raised, NON_TEXT_MIN_CONTRAST),
    // Focus indicator (WCAG 2.4.11).
    floor(Role::Focus, Role::Base, NON_TEXT_MIN_CONTRAST),
    // Status dots and icons.
    floor(Role::Success, Role::Base, NON_TEXT_MIN_CONTRAST),
    floor(Role::Warning, Role::Base, NON_TEXT_MIN_CONTRAST),
    floor(Role::Danger, Role::Base, NON_TEXT_MIN_CONTRAST),
    floor(Role::Info, Role::Base, NON_TEXT_MIN_CONTRAST),
];

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

/// Anything the floors can be measured on: the authored [`PaletteSpec`] and
/// the expanded [`Colors`] (which also covers presets assembled outside
/// `Colors::themed`, such as high contrast).
pub trait RoleColors {
    fn role(&self, role: Role) -> Color;
}

impl RoleColors for Colors {
    fn role(&self, role: Role) -> Color {
        use super::colors::ACCENT_PRIMARY_IDX;
        match role {
            Role::Base => self.surfaces.base,
            Role::Raised => self.surfaces.raised,
            Role::Overlay => self.surfaces.overlay,
            Role::TextPrimary => self.text_primary,
            Role::TextDim => self.text_dim,
            Role::TextTertiary => self.text_tertiary,
            Role::Accent => self.accent[ACCENT_PRIMARY_IDX],
            Role::Focus => self.border_focus,
            Role::Success => self.semantic.success,
            Role::Warning => self.semantic.warning,
            Role::Danger => self.semantic.danger,
            Role::Info => self.semantic.info,
        }
    }
}

impl RoleColors for PaletteSpec {
    fn role(&self, role: Role) -> Color {
        match role {
            Role::Base => self.base,
            Role::Raised => self.raised,
            Role::Overlay => self.popover,
            Role::TextPrimary => self.text,
            Role::TextDim => self.text_dim,
            Role::TextTertiary => self.text_tertiary,
            Role::Accent => self.accent,
            Role::Focus => self.focus,
            Role::Success => self.success,
            Role::Warning => self.warning,
            Role::Danger => self.danger,
            Role::Info => self.info,
        }
    }
}

fn palette_slot(p: &mut PaletteSpec, role: Role) -> &mut Color {
    match role {
        Role::Base => &mut p.base,
        Role::Raised => &mut p.raised,
        Role::Overlay => &mut p.popover,
        Role::TextPrimary => &mut p.text,
        Role::TextDim => &mut p.text_dim,
        Role::TextTertiary => &mut p.text_tertiary,
        Role::Accent => &mut p.accent,
        Role::Focus => &mut p.focus,
        Role::Success => &mut p.success,
        Role::Warning => &mut p.warning,
        Role::Danger => &mut p.danger,
        Role::Info => &mut p.info,
    }
}

/// Every floor `colors` misses, in [`FLOORS`] order. Empty means the palette
/// keeps every promise an app relies on. Alpha is ignored: every role in the
/// table is opaque.
#[must_use]
pub fn audit(colors: &impl RoleColors) -> Vec<ContrastFinding> {
    FLOORS
        .iter()
        .filter_map(|f| {
            let ratio = contrast_ratio(colors.role(f.foreground), colors.role(f.ground));
            (ratio < f.min_ratio).then_some(ContrastFinding {
                foreground: f.foreground,
                ground: f.ground,
                ratio,
                min_ratio: f.min_ratio,
            })
        })
        .collect()
}

/// Steps each failing foreground toward whichever extreme contrasts with its
/// hardest ground, just far enough to clear every floor it has. Grounds and
/// passing foregrounds are untouched, so a theme that already keeps its
/// promises comes back identical.
pub fn repair(palette: &mut PaletteSpec) {
    let mut foregrounds: Vec<Role> = FLOORS.iter().map(|f| f.foreground).collect();
    foregrounds.dedup();
    for fg in foregrounds {
        let floors: Vec<(Color, f64)> = FLOORS
            .iter()
            .filter(|f| f.foreground == fg)
            .map(|f| (palette.role(f.ground), f.min_ratio))
            .collect();
        let tone = palette.role(fg);
        *palette_slot(palette, fg) = legible_on_all(tone, &floors);
    }
}

fn legible_on_all(tone: Color, floors: &[(Color, f64)]) -> Color {
    // Worst margin over the floors: negative while any of them is missed.
    let margin = |c: Color| {
        floors
            .iter()
            .map(|(ground, min)| contrast_ratio(c, *ground) - min)
            .fold(f64::INFINITY, f64::min)
    };
    if floors.is_empty() || margin(tone) >= 0.0 {
        return tone;
    }
    let Some(hardest) = floors
        .iter()
        .map(|(ground, min)| (*ground, contrast_ratio(tone, *ground) - min))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(ground, _)| ground)
    else {
        return tone;
    };
    let target = if contrast_ratio(Color::BLACK, hardest) >= contrast_ratio(Color::WHITE, hardest) {
        Color::BLACK
    } else {
        Color::WHITE
    };
    // Sixty-fourths: close to the authored hue, and a 0.4 % step can't skip
    // far past the floor.
    for step in 1..=64_u8 {
        let candidate = mix8(tone, target, f32::from(step) / 64.0);
        if margin(candidate) >= 0.0 {
            return candidate;
        }
    }
    target
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color_compat::ColorCompat;

    fn grey_on_grey() -> PaletteSpec {
        let g = |v: u8| Color::from_rgb8(v, v, v);
        PaletteSpec {
            dark: true,
            base: g(0x30),
            raised: g(0x38),
            popover: g(0x28),
            text: g(0x70),
            text_dim: g(0x58),
            text_tertiary: g(0x40),
            border: g(0x44),
            focus: g(0x48),
            accent: Color::from_rgb8(0x30, 0x40, 0x60),
            accent_hover: Color::from_rgb8(0x28, 0x38, 0x58),
            success: Color::from_rgb8(0x20, 0x50, 0x30),
            warning: Color::from_rgb8(0x50, 0x48, 0x20),
            danger: Color::from_rgb8(0x58, 0x20, 0x20),
            info: Color::from_rgb8(0x20, 0x40, 0x58),
            window_card: None,
        }
    }

    #[test]
    fn repair_clears_every_floor_and_leaves_grounds_and_passing_roles_alone() {
        let mut p = grey_on_grey();
        let before = audit(&p);
        assert!(
            before.iter().any(|f| f.foreground == Role::TextPrimary),
            "the fixture must start illegible: {before:?}"
        );
        repair(&mut p);
        assert_eq!(audit(&p), Vec::new(), "repair must clear every floor");
        let fixture = grey_on_grey();
        assert_eq!(p.base, fixture.base, "grounds are never moved");
        assert_eq!(p.raised, fixture.raised);
        assert_eq!(p.popover, fixture.popover);
        assert_eq!(p.border, fixture.border, "border has no floor");

        // A palette that already passes comes back bit-identical.
        let mut again = p.clone();
        repair(&mut again);
        for role in FLOORS.iter().flat_map(|f| [f.foreground, f.ground]) {
            assert_eq!(again.role(role), p.role(role), "{}", role.name());
        }
    }

    #[test]
    fn repair_moves_a_foreground_only_as_far_as_its_floor_demands() {
        let mut p = grey_on_grey();
        repair(&mut p);
        // One sixty-fourth back toward the authored tone must miss again —
        // otherwise repair overshot and threw away more of the palette than
        // legibility required.
        let fixture = grey_on_grey();
        let r = contrast_ratio(p.text, p.raised);
        assert!(r >= BODY_TEXT_MIN_CONTRAST, "{r:.2}");
        assert!(
            r < BODY_TEXT_MIN_CONTRAST + 0.5,
            "text moved to {r:.2}:1 on raised; it only needed 4.5 (authored {:?})",
            [fixture.text.r(), fixture.text.g(), fixture.text.b()],
        );
    }

    #[test]
    fn findings_name_the_pair_and_the_shortfall() {
        let finding = ContrastFinding {
            foreground: Role::TextDim,
            ground: Role::Raised,
            ratio: 4.11,
            min_ratio: 4.5,
        };
        assert_eq!(
            finding.to_string(),
            "text_dim on surface_raised is 4.11:1, needs 4.5:1"
        );
    }
}
