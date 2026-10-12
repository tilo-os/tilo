//! Exact retained inputs for multiline preparation. Text is compared by value:
//! local edits, CRDT reconciliation and callers without a mutation witness all
//! take the same conservative path. No display text/spans are built on a hit.
use super::{Arc, TextAlign, TextInput, TextStyle};
use crate::tokens::SyntaxPalette;

pub(super) struct Preparation {
    text: Arc<str>,
    preedit: String,
    composition_caret: Option<(usize, Option<(usize, usize)>)>,
    style: TextStyle,
    width: f32,
    align: TextAlign,
    /// The palette highlighted runs were coloured from. `None` for a layout
    /// shaped without one, which no palette-keyed lookup accepts.
    syntax: Option<SyntaxPalette>,
}

impl Preparation {
    pub(super) fn capture(
        input: &TextInput,
        style: &TextStyle,
        width: f32,
        syntax: Option<&SyntaxPalette>,
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
            preedit: input.core.preedit().to_owned(),
            composition_caret: Self::composition_caret(input),
            style: style.clone(),
            width,
            align: input.align,
            syntax: syntax.cloned(),
        })
    }

    /// The wrap width the paragraph was prepared for.
    pub(super) fn width(&self) -> f32 {
        self.width
    }

    pub(super) fn matches_inputs(&self, input: &TextInput, style: &TextStyle, width: f32) -> bool {
        !input.secret
            && !matches!(
                style.hyphenation,
                crate::text_render::Hyphenation::Custom(_)
            )
            && self.text.as_ref() == input.text
            && self.preedit == input.core.preedit()
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
        syntax: &SyntaxPalette,
    ) -> bool {
        self.matches_inputs(input, style, width) && self.syntax.as_ref() == Some(syntax)
    }

    fn composition_caret(input: &TextInput) -> Option<(usize, Option<(usize, usize)>)> {
        input
            .core
            .has_preedit()
            .then_some((input.core.caret(), input.core.preedit_cursor()))
    }
}
