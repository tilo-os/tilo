//! The block editor's document as what it is, for a host that renders the
//! window's composition with elements of its own ([`crate::composition`]):
//! each block's kind, text and inline marks, and the caret while one is
//! edited.

use super::{
    BlockEditor, BlockKind, LIST_INDENT_STEP_PX, Mark, numbered_ordinals, prefix_gutter_for,
};

impl BlockEditor {
    /// Where its text stands, as its own layout puts it: a paragraph at the
    /// editor's text inset (the edge a title above lines up with), a list
    /// item and a quote past their gutters, a nested item a step further.
    pub(super) fn composed_edges(&self, theme: &crate::Theme) -> crate::composition::DocEdges {
        let scale = self.text_scale;
        crate::composition::DocEdges {
            text: self.text_inset(theme),
            list: prefix_gutter_for(&BlockKind::Bullet { indent: 0 }) * scale,
            indent: f64::from(LIST_INDENT_STEP_PX) * scale,
            quote: prefix_gutter_for(&BlockKind::Quote) * scale,
        }
    }

    pub(super) fn composed_blocks(&self) -> Vec<crate::composition::DocBlock> {
        use crate::composition::{DocBlock, InlineMark};
        let ordinals = numbered_ordinals(&self.blocks);
        self.blocks
            .iter()
            .enumerate()
            .map(|(index, block)| {
                let (kind, level, done, tag) = match &block.kind {
                    BlockKind::Paragraph => ("paragraph", block.nesting, false, None),
                    BlockKind::Quote => ("quote", block.nesting, false, None),
                    BlockKind::Heading(level) => ("heading", *level, false, None),
                    BlockKind::Bullet { indent } => ("bullet", *indent, false, None),
                    BlockKind::Numbered { indent } => ("numbered", *indent, false, None),
                    BlockKind::Checklist { indent, done } => ("check", *indent, *done, None),
                    BlockKind::CodeBlock { lang } => ("code", 0, false, lang.clone()),
                    BlockKind::Table { .. } => ("table", 0, false, None),
                    BlockKind::Divider => ("divider", 0, false, None),
                    BlockKind::Callout(callout) => {
                        ("callout", 0, false, Some(callout.name().to_owned()))
                    }
                };
                let style = crate::tokens::typography::stock().style(block.kind.text_role());
                let marks = block
                    .line
                    .marks()
                    .iter()
                    .map(|span| {
                        let (mark, href) = match &span.mark {
                            Mark::Bold => ("bold", None),
                            Mark::Italic => ("italic", None),
                            Mark::Code => ("code", None),
                            Mark::Underline => ("underline", None),
                            Mark::Strikethrough => ("strike", None),
                            Mark::ExternalLink(uri) | Mark::ResourceLink(uri) => {
                                ("link", Some(uri.clone()))
                            }
                            Mark::Link(_) | Mark::Embed(_) | Mark::Transclusion(_) => {
                                ("link", None)
                            }
                            Mark::Mention(_) => ("mention", None),
                        };
                        InlineMark {
                            start: span.range.start,
                            end: span.range.end,
                            mark,
                            href,
                        }
                    })
                    .collect();
                DocBlock {
                    id: block.id.clone(),
                    kind,
                    level,
                    done,
                    ordinal: matches!(block.kind, BlockKind::Numbered { .. })
                        .then(|| ordinals.get(index).copied())
                        .flatten(),
                    tag,
                    soft_break: block.soft_break,
                    text: block.text().to_owned(),
                    marks,
                    selection: (self.focused == Some(index)).then(|| {
                        block.line.selection_range().map_or_else(
                            || [block.line.cursor_pos(); 2],
                            |(start, end)| [start, end],
                        )
                    }),
                    editable: block.kind.is_editable(),
                    // The gap the editor stacks it at, its role's leading and weight, and the
                    // document's size for its kind: what the canvas sets it in.
                    gap: self
                        .stack_boxes
                        .get(index)
                        .map_or(0.0, |stack| stack.margin_top),
                    size: block.kind.font_size() * self.font_scale,
                    line_height: style.line_height,
                    weight: match style.weight {
                        crate::text_render::TextWeight::Regular => 400,
                        crate::text_render::TextWeight::Medium => 500,
                        crate::text_render::TextWeight::Semibold => 600,
                        crate::text_render::TextWeight::Bold => 700,
                    },
                }
            })
            .collect()
    }
}
