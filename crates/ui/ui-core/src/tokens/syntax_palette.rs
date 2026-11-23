//! The one colour scheme every syntax-highlighting surface paints with: the code editor
//! widget, block-editor code fences, highlighted text inputs and any app that renders
//! source (a code editor, a diff, a snippet preview). Like [`super::TerminalPalette`] it
//! is derived from the active [`Colors`], never hand-authored per theme, so a theme swap
//! recolours every code surface coherently.

use super::colors::{Colors, mix8};
use crate::color::contrast_ratio;
use crate::widget::code_editor::TokenKind;
use vello::peniko::Color;

const AA: f64 = 4.5;
const AAA: f64 = 7.0;
/// Body text at or above this ratio marks a maximum-contrast preset, whose syntax colours
/// then clear [`AAA`] too.
const MAX_CONTRAST_TEXT: f64 = 19.0;
/// The presence slot keywords and markup start from: the blue at 265°.
const KEYWORD_SLOT: usize = 8;
/// The presence slot attributes and macros take: the magenta at 325°.
const ATTRIBUTE_SLOT: usize = 10;
/// The presence slot markup tags take: the cyan at 205°.
const TAG_SLOT: usize = 6;

/// Theme-derived colour for every syntax role.
#[derive(Clone, Debug, PartialEq)]
pub struct SyntaxPalette {
    pub keyword: Color,
    pub type_name: Color,
    pub function: Color,
    pub string: Color,
    pub number: Color,
    pub comment: Color,
    pub operator: Color,
    pub attribute: Color,
    pub tag: Color,
    pub markup: Color,
    pub raw: Color,
    pub math: Color,
    pub text: Color,
}

impl SyntaxPalette {
    /// Derive the palette from a theme's colour tokens (see the module docs for the two
    /// rules it guarantees).
    #[must_use]
    pub fn from_colors(c: &Colors) -> Self {
        let base = c.surfaces.base;
        let raised = c.surfaces.raised;
        let blue = c.presence.get(KEYWORD_SLOT);
        let step = |toward_text: f32| mix8(blue, c.text_primary, toward_text);
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
            attribute: legible(mix8(c.presence.get(ATTRIBUTE_SLOT), c.text_primary, 0.25)),
            tag: legible(mix8(c.presence.get(TAG_SLOT), c.text_primary, 0.25)),
            markup: legible(blue),
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
            TokenKind::Tag => self.tag,
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

    const ROLES: [TokenKind; 13] = [
        TokenKind::Tag,
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
            let blue =
                |theme: &Theme| relative_luminance(theme.tokens.colors.presence.get(KEYWORD_SLOT));
            assert!(luminance(&dark) > blue(&dark), "dark keywords lighten");
            assert!(luminance(&light) < blue(&light), "light keywords darken");
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
        // phase: attributes_and_tags_are_not_the_error_hue
        {
            for name in Theme::ALL {
                let theme = Theme::named(name).unwrap_or_else(|| panic!("named({name}) resolves"));
                let syntax = &theme.tokens.syntax;
                let danger = rgb(theme.tokens.colors.semantic.danger);
                assert_ne!(
                    rgb(syntax.attribute),
                    danger,
                    "{name}: attribute is not an error"
                );
                assert_ne!(rgb(syntax.tag), danger, "{name}: tag is not an error");
                assert_ne!(
                    rgb(syntax.tag),
                    rgb(syntax.attribute),
                    "{name}: tag and attribute differ"
                );
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
