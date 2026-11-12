//! The block editor's document as what it is, for a host that renders the
//! window's composition with elements of its own ([`crate::composition`]):
//! each block's kind, text and inline marks, and the caret while one is
//! edited.

use super::{BlockEditor, BlockKind, Mark, numbered_ordinals};

impl BlockEditor {
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
                }
            })
            .collect()
    }
}
