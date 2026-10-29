//! Between tilo-doc's paragraphs and the block editor's blocks, in the direction the editor
//! reads them.
//!
//! tilo-doc names paragraph kinds and inline marks the way its codecs do (`heading` with a
//! `level`, `list_item` with `ordered`/`checked`, `strong`, `em`), and counts offsets in
//! characters. The block editor has its own kinds and marks and counts bytes. Everything the
//! editor shows passes through [`content_of`]; what it writes back is the rich-text SDK's
//! (`tilo_rich_text`).

use tilo_doc::document::MarkValue;
use tilo_doc::snapshot::BlockView;

use super::BlockContent;
use super::marks::{Mark, MarkSpan};
use super::{BlockKind, CalloutKind};

/// The deepest list indent the editor draws.
pub const MAX_INDENT: i64 = 5;

/// The byte offset of character `chars` in `text`.
pub fn byte_of_char(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map_or(text.len(), |(byte, _)| byte)
}

/// The integer attribute `key` of a paragraph.
pub fn int_attr(view: &BlockView, key: &str) -> Option<i64> {
    view.int_attr(key)
}

/// The boolean attribute `key` of a paragraph.
pub fn bool_attr(view: &BlockView, key: &str) -> Option<bool> {
    match view.attrs.get(key) {
        Some(tilo_doc::AttrVal::Bool(value)) => Some(*value),
        _ => None,
    }
}

fn str_attr<'a>(view: &'a BlockView, key: &str) -> Option<&'a str> {
    view.str_attr(key)
}

fn indent_of(view: &BlockView) -> u8 {
    u8::try_from(int_attr(view, "level").unwrap_or(0).clamp(0, MAX_INDENT)).unwrap_or(0)
}

/// The editor kind a paragraph shows as.
pub fn kind_of(view: &BlockView) -> BlockKind {
    match view.kind.as_str() {
        "heading" => BlockKind::Heading(
            u8::try_from(int_attr(view, "level").unwrap_or(1).clamp(1, 3)).unwrap_or(1),
        ),
        "quote" | "blockquote" => str_attr(view, CALLOUT)
            .and_then(CalloutKind::from_name)
            .map_or(BlockKind::Quote, BlockKind::Callout),
        "list_item" => {
            let indent = indent_of(view);
            match bool_attr(view, "checked") {
                Some(done) => BlockKind::Checklist { indent, done },
                None if bool_attr(view, "ordered") == Some(true) => BlockKind::Numbered { indent },
                None => BlockKind::Bullet { indent },
            }
        }
        "divider" => BlockKind::Divider,
        _ if bool_attr(view, "code") == Some(true) => BlockKind::CodeBlock {
            lang: str_attr(view, "lang").map(str::to_owned),
        },
        _ => match bool_attr(view, TABLE_SOURCE) {
            Some(visual) => BlockKind::Table { visual },
            None => BlockKind::Paragraph,
        },
    }
}

/// A table the editor keeps as its Markdown source is a paragraph of that
/// source with this attribute, `true` while it shows as a grid.
pub const TABLE_SOURCE: &str = "table_source";

/// The attribute that makes a quote a callout: the alert's name (`warning`).
pub const CALLOUT: &str = "alert";

/// An internal link to another page is a `link` to this fragment prefix and
/// the page's id, which the document's link validation accepts.
pub const PAGE_LINK_PREFIX: &str = "#page-";

/// The scheme a resource link is written with in a `link` mark's target, in
/// the one form every document codec already round-trips: `tilo://open#…`,
/// the native link `tilo_reference::link::Link::to_native` makes. A link
/// mark whose target starts with this scheme names a resource, so it is
/// shown as a [`Mark::ResourceLink`] rather than an external URL.
pub const RESOURCE_LINK_SCHEME: &str = "tilo://open#";

/// The editor mark a tilo-doc mark shows as, when it has one.
pub fn editor_mark(name: &str, value: &MarkValue) -> Option<Mark> {
    match (name, value) {
        (_, MarkValue::Bool(false)) => None,
        ("strong", _) => Some(Mark::Bold),
        ("em", _) => Some(Mark::Italic),
        ("underline", _) => Some(Mark::Underline),
        ("strike", _) => Some(Mark::Strikethrough),
        ("fontFamily", MarkValue::Str(family)) if family == "mono" => Some(Mark::Code),
        ("link", MarkValue::Str(target)) => Some(
            target
                .strip_prefix(PAGE_LINK_PREFIX)
                .and_then(|id| id.parse().ok())
                .map_or_else(
                    || {
                        if is_resource_link(target) {
                            Mark::ResourceLink(target.clone())
                        } else {
                            Mark::ExternalLink(target.clone())
                        }
                    },
                    Mark::Link,
                ),
        ),
        ("transclusion", MarkValue::Str(page)) => page.parse().ok().map(Mark::Transclusion),
        ("mention", MarkValue::Str(id)) if !id.is_empty() => Some(Mark::Mention(id.clone())),
        _ => None,
    }
}

/// Whether a `link` target names a resource of this desktop
/// ([`RESOURCE_LINK_SCHEME`]) rather than a web address.
pub fn is_resource_link(target: &str) -> bool {
    target
        .strip_prefix(RESOURCE_LINK_SCHEME)
        .is_some_and(|route| !route.is_empty())
}

/// What a paragraph shows as in the editor.
pub fn content_of(view: &BlockView) -> BlockContent {
    let text = view.text.clone();
    let mut marks: Vec<MarkSpan> = view
        .marks
        .iter()
        .filter(|span| span.start < span.end)
        .filter_map(|span| {
            let mark = editor_mark(&span.name, &span.value)?;
            let start = byte_of_char(&text, span.start);
            let end = byte_of_char(&text, span.end);
            (start < end).then(|| MarkSpan::new(start..end, mark))
        })
        .collect();
    marks.sort_by_key(|span| (span.range.start, span.range.end));
    let kind = kind_of(view);
    let nesting = if kind.supports_indent() {
        0
    } else {
        u8::try_from(int_attr(view, "nesting").unwrap_or(0).clamp(0, MAX_INDENT)).unwrap_or(0)
    };
    let code_group = matches!(kind, BlockKind::CodeBlock { .. })
        .then(|| str_attr(view, "code_group").map(str::to_owned))
        .flatten();
    let authors = tilo_doc::document::author_runs(&view.marks)
        .into_iter()
        .map(|(start, end, author)| (byte_of_char(&text, start)..byte_of_char(&text, end), author))
        .filter(|(range, _)| range.start < range.end)
        .collect();
    BlockContent {
        id: view.id.clone(),
        kind,
        text,
        marks,
        nesting,
        code_group,
        authors,
    }
}

/// The mark a reference's run carries (`tilo_reference::embed::MARK`).
pub const REFERENCE_MARK: &str = tilo_reference::embed::MARK;

/// A paragraph a component is given, as tilo-doc reads it.
#[must_use]
pub fn view_of(block: &tilo_ui_graph::rich::Block, id: String) -> BlockView {
    use tilo_ui_graph::rich::Attr;
    let mark_value = |value: &Attr| match value {
        Attr::Bool(flag) => MarkValue::Bool(*flag),
        Attr::Int(number) => MarkValue::Int(*number),
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a mark's number is a small count"
        )]
        Attr::Float(number) => MarkValue::Int(*number as i64),
        Attr::Text(text) => MarkValue::Str(text.clone()),
    };
    BlockView {
        id,
        kind: block.kind.clone(),
        text: block.text.clone(),
        marks: block
            .marks
            .iter()
            .map(|mark| tilo_doc::document::MarkSpan {
                start: mark.start,
                end: mark.end,
                name: mark.name.clone(),
                value: mark_value(&mark.value),
            })
            .collect(),
        attrs: block
            .attrs
            .iter()
            .map(|(key, value)| {
                let value = match value {
                    Attr::Bool(flag) => tilo_doc::AttrVal::Bool(*flag),
                    Attr::Int(number) => tilo_doc::AttrVal::Int(*number),
                    Attr::Float(number) => tilo_doc::AttrVal::Float(*number),
                    Attr::Text(text) => tilo_doc::AttrVal::Str(text.clone()),
                };
                (key.clone(), value)
            })
            .collect(),
        children: Vec::new(),
    }
}

/// What a reference reads as for the person reading, by how the text says it reads
/// ([`tilo_ui_graph::rich::InlineView`]): a person as `@` and what the room calls them, a
/// resource by its title now (after what its type shows beside it, a task's number), one kept
/// elsewhere as its run, and one the reader cannot read as only that something is there.
#[must_use]
pub fn reference_label(view: &tilo_ui_graph::rich::InlineView, run: &str) -> String {
    use tilo_ui_graph::rich::InlineView;
    match view {
        InlineView::Person { name, .. } if !name.is_empty() => format!("@{name}"),
        InlineView::Resource { title, detail, .. } if !title.is_empty() => match detail {
            Some(detail) if !detail.is_empty() => format!("{detail} {title}"),
            _ => title.clone(),
        },
        InlineView::Unreadable => {
            crate::i18n::I18nManager::lookup("rich-text-reference-unreadable", None)
        }
        InlineView::Person { .. } | InlineView::Resource { .. } | InlineView::Elsewhere => {
            run.to_owned()
        }
    }
}

/// The blocks rich text shows as, each reference reading as the text says it reads for the
/// person reading ([`reference_label`]); paragraph ids are their places.
#[must_use]
pub fn blocks_of(rich: &tilo_ui_graph::rich::RichText) -> Vec<BlockContent> {
    rich.blocks
        .iter()
        .enumerate()
        .map(|(index, block)| {
            let mut view = view_of(block, format!("p{index}"));
            if !rich.embeds.is_empty() {
                relabel(&mut view, rich);
            }
            content_of(&view)
        })
        .collect()
}

/// Rewrite each reference's run of `view` as it reads, shifting the marks after it.
fn relabel(view: &mut BlockView, rich: &tilo_ui_graph::rich::RichText) {
    let mut references: Vec<(usize, usize, String)> = view
        .marks
        .iter()
        .filter(|mark| mark.name == REFERENCE_MARK && mark.start < mark.end)
        .filter_map(|mark| match &mark.value {
            MarkValue::Str(value) => rich
                .embed(value)
                .map(|embed| (mark.start, mark.end, embed.clone())),
            MarkValue::Bool(_) | MarkValue::Int(_) => None,
        })
        .map(|(start, end, embed)| {
            let run: String = view
                .text
                .chars()
                .skip(start)
                .take(end.saturating_sub(start))
                .collect();
            (start, end, reference_label(&embed, &run))
        })
        .collect();
    // From the last, so the offsets of those before stay as they are.
    references.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    for (start, end, label) in references {
        let chars: Vec<char> = view.text.chars().collect();
        if end > chars.len() {
            continue;
        }
        let new_len = label.chars().count();
        let old_len = end.saturating_sub(start);
        let mut text: String = chars.iter().take(start).collect();
        text.push_str(&label);
        text.extend(chars.iter().skip(end));
        view.text = text;
        let shift = |at: usize| {
            if at >= end {
                at.saturating_add(new_len).saturating_sub(old_len)
            } else if at > start {
                start.saturating_add(new_len.min(at.saturating_sub(start)))
            } else {
                at
            }
        };
        for mark in &mut view.marks {
            if mark.start == start && mark.end == end {
                mark.end = start.saturating_add(new_len);
            } else {
                mark.start = shift(mark.start);
                mark.end = shift(mark.end);
            }
        }
    }
}
