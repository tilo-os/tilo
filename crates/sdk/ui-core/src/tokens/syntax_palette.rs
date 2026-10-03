//! The one colour scheme every syntax-highlighting surface paints with: the
//! code editor widget, block-editor code fences, highlighted text inputs and
//! any app that renders source (a code editor, a diff, a snippet preview).
//!
//! Like [`super::TerminalPalette`] it is derived from the active
//! [`Colors`], never hand-authored per theme, so a theme swap recolours every
//! code surface coherently. Two properties hold on every preset:
//!
//! - **Polarity-aware accent steps.** Keywords and type names are the primary
//!   accent moved a quarter and a half of the way toward the body text
//!   colour — *away from the ground*: lighter on a dark theme, darker on a
//!   light one. Fixed accent-ramp stops cannot promise that: the ramp is
//!   eased around a pinned primary, so its "lighter" neighbours are darker
//!   than the accent on some presets and wash out on paper on others.
//! - **A contrast floor.** Each role is walked toward the body text colour
//!   until it clears WCAG AA against both code grounds (`surfaces.base` and
//!   `surfaces.raised`); a preset whose body text sits at maximum contrast
//!   (high contrast) asks for AAA instead.

use super::colors::{ACCENT_PRIMARY_IDX, Colors, mix8};
use crate::color::contrast_ratio;
use crate::widget::code_editor::TokenKind;
use vello::peniko::Color;

/// WCAG AA for body text; the floor every syntax role clears.
const AA: f64 = 4.5;
/// WCAG AAA; the floor on a preset that already runs its text at AAA.
const AAA: f64 = 7.0;
/// Body text at or above this ratio marks a maximum-contrast preset, whose
/// syntax colours then clear [`AAA`] too.
const MAX_CONTRAST_TEXT: f64 = 19.0;

/// Theme-derived colour for every syntax role. Read it from
/// `theme.tokens.syntax` and resolve a token with [`SyntaxPalette::color`].
#[derive(Clone, Debug)]
pub struct SyntaxPalette {
    pub keyword: Color,
    /// Type names (`String`, `u32`, a capitalised identifier).
    pub type_name: Color,
    pub function: Color,
    /// String and character literals.
    pub string: Color,
    /// Numbers and named constants (`true`, `None`).
    pub number: Color,
    pub comment: Color,
    /// Operators and punctuation.
    pub operator: Color,
    /// Attributes, macros and preprocessor lines.
    pub attribute: Color,
    /// Markup headings; also the role snippet placeholders borrow.
    pub markup: Color,
    /// Raw / inline-code markup runs.
    pub raw: Color,
    pub math: Color,
    /// Identifiers, variables and uncategorised text.
    pub text: Color,
}

impl SyntaxPalette {
    /// Derive the palette from a theme's colour tokens (see the module docs
    /// for the two rules it guarantees).
    #[must_use]
    pub fn from_colors(c: &Colors) -> Self {
        let base = c.surfaces.base;
        let raised = c.surfaces.raised;
        let accent = c.accent[ACCENT_PRIMARY_IDX];
        // The accent moved toward the body text, i.e. away from the ground,
        // so each step gains contrast on either polarity.
        let step = |toward_text: f32| mix8(accent, c.text_primary, toward_text);
        let floor = if contrast_ratio(c.text_primary, base) >= MAX_CONTRAST_TEXT {
            AAA
        } else {
            AA
        };
        let legible = |color: Color| legible_on(color, c.text_primary, [base, raised], floor);
        Self {
            keyword: legible(step(0.25)),
            type_name: legible(step(0.5)),
            function: legible(c.semantic.info),
            string: legible(c.semantic.success),
            number: legible(c.semantic.warning),
            comment: legible(c.text_dim),
            operator: legible(c.neutral[7]),
            attribute: legible(c.semantic.danger),
            markup: legible(accent),
            raw: legible(step(0.5)),
            math: legible(step(0.25)),
            text: c.text_primary,
        }
    }

    /// The colour a token of `kind` paints in.
    #[must_use]
    pub fn color(&self, kind: TokenKind) -> Color {
        match kind {
            TokenKind::Keyword => self.keyword,
            TokenKind::Type => self.type_name,
            TokenKind::Function => self.function,
            TokenKind::StringLit | TokenKind::CharLit => self.string,
            TokenKind::Number | TokenKind::Constant => self.number,
            TokenKind::Comment => self.comment,
            TokenKind::Operator | TokenKind::Punctuation => self.operator,
            TokenKind::Attribute | TokenKind::Preprocessor => self.attribute,
            TokenKind::Heading => self.markup,
            TokenKind::Raw => self.raw,
            TokenKind::Math => self.math,
            TokenKind::Variable | TokenKind::Emphasis | TokenKind::Strong | TokenKind::Text => {
                self.text
            }
        }
    }

    pub fn dark() -> Self {
        Self::from_colors(&Colors::dark())
    }

    pub fn light() -> Self {
        Self::from_colors(&Colors::light())
    }
}

/// Walk `color` toward `ink` in tenths until it clears `floor` against every
/// ground; `ink` (the body text colour, legible by construction) is the
/// fallback.
fn legible_on(color: Color, ink: Color, grounds: [Color; 2], floor: f64) -> Color {
    let worst = |candidate: Color| {
        grounds
            .iter()
            .map(|ground| contrast_ratio(candidate, *ground))
            .fold(f64::INFINITY, f64::min)
    };
    if worst(color) >= floor {
        return color;
    }
    (1..=10_u8)
        .map(|tenth| mix8(color, ink, f32::from(tenth) / 10.0))
        .find(|candidate| worst(*candidate) >= floor)
        .unwrap_or(ink)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::relative_luminance;
    use crate::theme::Theme;

    const ROLES: [TokenKind; 12] = [
        TokenKind::Keyword,
        TokenKind::Type,
        TokenKind::Function,
        TokenKind::StringLit,
        TokenKind::Number,
        TokenKind::Comment,
        TokenKind::Operator,
        TokenKind::Attribute,
        TokenKind::Heading,
        TokenKind::Raw,
        TokenKind::Math,
        TokenKind::Text,
    ];

    fn rgb(c: Color) -> [u8; 3] {
        crate::color::to_rgb8(c)
    }

    #[test]
    fn syntax_roles_are_legible_distinct_and_track_the_theme() {
        // phase: every_role_is_legible_on_every_preset
        {
            for name in Theme::ALL {
                let theme = Theme::named(name).unwrap_or_else(|| panic!("named({name}) resolves"));
                let colors = &theme.tokens.colors;
                let floor = if contrast_ratio(colors.text_primary, colors.surfaces.base)
                    >= MAX_CONTRAST_TEXT
                {
                    AAA
                } else {
                    AA
                };
                for kind in ROLES {
                    let color = theme.tokens.syntax.color(kind);
                    for ground in [colors.surfaces.base, colors.surfaces.raised] {
                        let ratio = contrast_ratio(color, ground);
                        assert!(
                            ratio >= floor - 0.05,
                            "{name}: {kind:?} measures {ratio:.2}:1 on {:?}",
                            rgb(ground)
                        );
                    }
                }
            }
        }
        // phase: high_contrast_syntax_clears_aaa
        {
            let theme = Theme::high_contrast();
            for kind in ROLES {
                let ratio = contrast_ratio(
                    theme.tokens.syntax.color(kind),
                    theme.tokens.colors.surfaces.base,
                );
                assert!(ratio >= AAA, "{kind:?} measures {ratio:.2}:1");
            }
        }
        // phase: keywords_step_away_from_the_ground
        {
            let dark = Theme::dark();
            let light = Theme::light();
            let luminance = |theme: &Theme| relative_luminance(theme.tokens.syntax.keyword);
            let accent =
                |theme: &Theme| relative_luminance(theme.tokens.colors.accent[ACCENT_PRIMARY_IDX]);
            assert!(luminance(&dark) > accent(&dark), "dark keywords lighten");
            assert!(luminance(&light) < accent(&light), "light keywords darken");
        }
        // phase: roles_that_carry_meaning_stay_distinct
        {
            for name in Theme::ALL {
                let syntax = Theme::named(name)
                    .unwrap_or_else(|| panic!("named({name}) resolves"))
                    .tokens
                    .syntax;
                let distinct = [
                    syntax.keyword,
                    syntax.string,
                    syntax.comment,
                    syntax.number,
                    syntax.function,
                ]
                .map(rgb);
                for (i, a) in distinct.iter().enumerate() {
                    for b in distinct.iter().skip(i + 1) {
                        assert_ne!(a, b, "{name}: two syntax roles collapsed onto one colour");
                    }
                }
            }
        }
        // phase: the_palette_tracks_the_theme
        {
            assert_ne!(
                rgb(Theme::dark().tokens.syntax.keyword),
                rgb(Theme::light().tokens.syntax.keyword)
            );
        }
    }
}
