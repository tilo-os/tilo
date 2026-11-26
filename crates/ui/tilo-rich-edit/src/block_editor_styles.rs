//! A document's named styles as the editor draws them: the table the document keeps beside its
//! paragraphs ([`tilo_ui_graph::rich::RichText::styles`]), each style's look by the name a
//! paragraph's `style` names. A paragraph set in a style takes its look where the paragraph does
//! not say otherwise, so editing a style changes every paragraph set in it at once. The table is
//! the document's; the editor reads it and never writes it.

use std::collections::BTreeMap;

use tilo_ui_graph::rich::Attr;

use super::{Block, BlockEditor, ParagraphStyle};
use crate::rich_text_line::RunLook;

/// One named style's look: how its paragraphs stand and how their words are set.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StyleLook {
    pub paragraph: ParagraphStyle,
    pub run: RunLook,
}

impl StyleLook {
    /// The look a style's stored parts say (`font`, `size`, `bold`, `italic`, `color`, `align`,
    /// `space_before`, `space_after`, `indent`, `line_spacing`); a part it does not set is left
    /// to the paragraph's kind.
    #[must_use]
    pub fn of(parts: &BTreeMap<String, Attr>) -> Self {
        let text = |key: &str| match parts.get(key) {
            Some(Attr::Text(text)) => Some(text.as_str()),
            _ => None,
        };
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_precision_loss,
            reason = "a style's points are small"
        )]
        let number = |key: &str| match parts.get(key) {
            Some(Attr::Float(n)) if n.is_finite() => Some(*n as f32),
            Some(Attr::Int(n)) => Some(*n as f32),
            Some(Attr::Text(text)) => text.trim().parse::<f32>().ok(),
            _ => None,
        };
        let flag = |key: &str| match parts.get(key) {
            Some(Attr::Bool(on)) => Some(*on),
            Some(Attr::Text(text)) => text.parse::<bool>().ok(),
            _ => None,
        };
        Self {
            paragraph: ParagraphStyle {
                align: text("align").and_then(crate::rich_doc::align_of),
                space_before: number("space_before"),
                space_after: number("space_after"),
                indent: number("indent"),
                first_line_indent: None,
                line_spacing: number("line_spacing"),
                style: None,
            },
            run: RunLook {
                family: text("font").and_then(crate::rich_doc::family_of),
                size: number("size"),
                bold: flag("bold"),
                italic: flag("italic"),
                color: text("color").and_then(crate::rich_doc::rgb_of),
            },
        }
    }
}

impl ParagraphStyle {
    /// This paragraph's own look, the style's filling what it leaves unset.
    #[must_use]
    pub fn over(&self, named: &Self) -> Self {
        Self {
            align: self.align.or(named.align),
            space_before: self.space_before.or(named.space_before),
            space_after: self.space_after.or(named.space_after),
            indent: self.indent.or(named.indent),
            first_line_indent: self.first_line_indent.or(named.first_line_indent),
            line_spacing: self.line_spacing.or(named.line_spacing),
            style: self.style.clone(),
        }
    }
}

impl BlockEditor {
    /// Take the document's style table: `true` when a look changed, and the paragraphs set in
    /// the styles that changed are laid out again.
    pub(crate) fn set_named_styles(
        &mut self,
        styles: &BTreeMap<String, BTreeMap<String, Attr>>,
    ) -> bool {
        let looks: BTreeMap<String, StyleLook> = styles
            .iter()
            .map(|(name, parts)| (name.clone(), StyleLook::of(parts)))
            .collect();
        if looks == self.named_styles {
            return false;
        }
        self.named_styles = looks;
        for block in &mut self.blocks {
            let run = block
                .style
                .style
                .as_ref()
                .and_then(|name| self.named_styles.get(name))
                .map(|look| look.run);
            block.line.set_run_look(run);
        }
        self.structure_dirty = true;
        self.dirty = true;
        self.restack();
        true
    }

    /// The look of the style a document calls `name`, where its table sets one.
    #[must_use]
    pub fn named_style(&self, name: &str) -> Option<&StyleLook> {
        self.named_styles.get(name)
    }

    /// How `block` stands: its own look, the style it is set in filling the rest.
    pub(crate) fn paragraph_look(&self, block: &Block) -> ParagraphStyle {
        match block
            .style
            .style
            .as_ref()
            .and_then(|name| self.named_styles.get(name))
        {
            Some(look) => block.style.over(&look.paragraph),
            None => block.style.clone(),
        }
    }

    /// The run look of the style `block` is set in, for its line.
    pub(crate) fn run_look_of(&self, block: &Block) -> Option<RunLook> {
        block
            .style
            .style
            .as_ref()
            .and_then(|name| self.named_styles.get(name))
            .map(|look| look.run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CursorPlacement;
    use tilo_ui_base::{richtext::FontFamily, richtext::TextAlign};

    /// A paragraph set in a style the document keeps takes that style's look where it says
    /// nothing itself, and follows the style when its look changes; its own look still wins.
    #[test]
    fn a_paragraph_takes_the_look_of_the_style_it_is_set_in() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("aside"), Block::paragraph("plain")]);
        editor.focus_block(0, CursorPlacement::End);
        assert!(editor.format_asked(Some("style:Aside:1".into())));
        assert_eq!(editor.blocks[0].style.style.as_deref(), Some("Aside"));

        let look = BTreeMap::from([(
            "Aside".to_owned(),
            BTreeMap::from([
                ("font".to_owned(), Attr::Text("serif".into())),
                ("size".to_owned(), Attr::Float(14.0)),
                ("italic".to_owned(), Attr::Bool(true)),
                ("align".to_owned(), Attr::Text("center".into())),
                ("space_after".to_owned(), Attr::Float(18.0)),
            ]),
        )]);
        assert!(editor.set_named_styles(&look));
        assert!(
            !editor.set_named_styles(&look),
            "the same table is no change"
        );

        let set_in = editor.paragraph_look(&editor.blocks[0]);
        assert_eq!(set_in.align, Some(TextAlign::Center));
        assert_eq!(set_in.space_after, Some(18.0));
        let run = editor.blocks[0]
            .line
            .run_look()
            .expect("the words take the style's look");
        assert_eq!(run.family, Some(FontFamily::Serif));
        assert_eq!(run.size, Some(14.0));
        assert_eq!(run.italic, Some(true));
        assert_eq!(editor.paragraph_look(&editor.blocks[1]).align, None);
        assert_eq!(
            editor.blocks[1].line.run_look(),
            None,
            "a paragraph in no style is its own"
        );

        // The paragraph's own alignment wins over its style's.
        assert!(editor.format_asked(Some("align:right:2".into())));
        assert_eq!(
            editor.paragraph_look(&editor.blocks[0]).align,
            Some(TextAlign::Right)
        );

        // A style taken out of the table leaves its paragraphs their kind's look.
        assert!(editor.set_named_styles(&BTreeMap::new()));
        assert_eq!(editor.blocks[0].line.run_look(), None);
    }
}
