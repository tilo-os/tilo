//! Exact retained inputs for multiline preparation. Text is compared by value:
//! local edits, CRDT reconciliation and callers without a mutation witness all
//! take the same conservative path. No display text/spans are built on a hit.
use super::{Arc, ColorCompat, TextAlign, TextInput, TextStyle, Theme};

type SyntaxColors = [[u8; 3]; 12];

pub(super) struct Preparation {
    text: Arc<str>,
    preedit: String,
    composition_caret: Option<(usize, Option<(usize, usize)>)>,
    style: TextStyle,
    width: f32,
    align: TextAlign,
    syntax: Option<SyntaxColors>,
}

impl Preparation {
    pub(super) fn capture(
        input: &TextInput,
        style: &TextStyle,
        width: f32,
        theme: Option<&Theme>,
    ) -> Option<Self> {
        // Do not retain another plaintext copy of a secret, even while revealed.
        // A custom hyphenator can mutate behind its Arc without a frozen witness.
        if input.secret
            || matches!(
                style.hyphenation,
                crate::text_render::Hyphenation::Custom(_)
            )
        {
            return None;
        }
        Some(Self {
            text: input
                .hl_text
                .as_ref()
                .filter(|text| text.as_ref() == input.text)
                .cloned()
                .unwrap_or_else(|| Arc::from(input.text.as_str())),
            preedit: input.preedit.clone(),
            composition_caret: Self::composition_caret(input),
            style: style.clone(),
            width,
            align: input.align,
            syntax: theme.map(Self::syntax_colors),
        })
    }

    pub(super) fn matches_inputs(&self, input: &TextInput, style: &TextStyle, width: f32) -> bool {
        !input.secret
            && !matches!(
                style.hyphenation,
                crate::text_render::Hyphenation::Custom(_)
            )
            && self.text.as_ref() == input.text
            && self.preedit == input.preedit
            && self.composition_caret == Self::composition_caret(input)
            && self.style == *style
            // Exact finite bit identity is conservative for wrapping. Unknown
            // non-finite widths always use the original construction path.
            && width.is_finite()
            && self.width.to_bits() == width.to_bits()
            && self.align == input.align
    }

    pub(super) fn matches(
        &self,
        input: &TextInput,
        style: &TextStyle,
        width: f32,
        theme: &Theme,
    ) -> bool {
        self.matches_inputs(input, style, width) && self.syntax == Some(Self::syntax_colors(theme))
    }

    fn composition_caret(input: &TextInput) -> Option<(usize, Option<(usize, usize)>)> {
        (!input.preedit.is_empty()).then_some((input.cursor_pos, input.preedit_cursor))
    }

    fn syntax_colors(theme: &Theme) -> SyntaxColors {
        let p = &theme.tokens.syntax;
        [
            p.keyword,
            p.type_name,
            p.function,
            p.string,
            p.number,
            p.comment,
            p.operator,
            p.attribute,
            p.markup,
            p.raw,
            p.math,
            p.text,
        ]
        .map(|color| [color.r(), color.g(), color.b()])
    }
}
