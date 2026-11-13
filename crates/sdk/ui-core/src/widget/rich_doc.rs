//! Between tilo-doc's paragraphs and the block editor's blocks, both ways.

use tilo_doc::document::MarkValue;
use tilo_doc::snapshot::BlockView;

use super::BlockContent;
use super::marks::{Mark, MarkSpan};
use super::{BlockKind, CalloutKind};

pub const MAX_INDENT: i64 = 5;

/// The byte offset of character `chars` in `text`.
pub fn byte_of_char(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map_or(text.len(), |(byte, _)| byte)
}

/// The character offset of byte `byte` in `text`: [`byte_of_char`]'s inverse.
pub fn char_of_byte(text: &str, byte: usize) -> usize {
    text.char_indices().take_while(|(at, _)| *at < byte).count()
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

pub fn kind_of(view: &BlockView) -> BlockKind {
    match view.kind.as_str() {
        "heading" => BlockKind::Heading(
            u8::try_from(int_attr(view, "level").unwrap_or(1).clamp(1, 6)).unwrap_or(1),
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

/// The scheme a resource link is written with in a `link` mark's target, in the one form every
/// document codec already round-trips: `tilo://open#…`, the native link
/// `tilo_reference::link::Link::to_native` makes.
pub const RESOURCE_LINK_SCHEME: &str = "tilo://open#";

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
        // A live projection: of a host's own page when it names one by id, and of
        // whatever a record's reference names (`tilo_reference::embed`) otherwise.
        ("transclusion", MarkValue::Str(what)) => what.parse().ok().map_or_else(
            || (!what.is_empty()).then(|| Mark::Embed(what.clone())),
            |page| Some(Mark::Transclusion(page)),
        ),
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

pub const REFERENCE_MARK: &str = tilo_reference::embed::MARK;

/// The stored mark of a paragraph projecting a reference (`Mark::Transclusion`,
/// `Mark::Embed`): what the paragraph is, rather than how its text is drawn.
pub const EMBED_MARK: &str = tilo_doc::schema::MARK_PROJECTION;

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

/// The stored name and value the editor mark `mark` is written as: [`editor_mark`]'s inverse.
fn stored_mark(mark: &Mark) -> Option<(&'static str, MarkValue)> {
    Some(match mark {
        Mark::Bold => ("strong", MarkValue::Bool(true)),
        Mark::Italic => ("em", MarkValue::Bool(true)),
        Mark::Underline => ("underline", MarkValue::Bool(true)),
        Mark::Strikethrough => ("strike", MarkValue::Bool(true)),
        Mark::Code => ("fontFamily", MarkValue::Str("mono".to_owned())),
        Mark::Link(page) => ("link", MarkValue::Str(format!("{PAGE_LINK_PREFIX}{page}"))),
        Mark::Transclusion(page) => ("transclusion", MarkValue::Str(page.to_string())),
        Mark::Embed(reference) => ("transclusion", MarkValue::Str(reference.clone())),
        Mark::ExternalLink(target) | Mark::ResourceLink(target) => {
            ("link", MarkValue::Str(target.clone()))
        }
        Mark::Mention(id) if !id.is_empty() => ("mention", MarkValue::Str(id.clone())),
        Mark::Mention(_) => return None,
    })
}

/// The paragraph the editor block `content` is stored as: [`content_of`]'s inverse, in
/// tilo-doc's names, a list's depth in `level`, and offsets counted in characters.
#[must_use]
pub fn view_of_content(content: &BlockContent) -> BlockView {
    let mut attrs: std::collections::BTreeMap<String, tilo_doc::AttrVal> = Default::default();
    let mut level = |depth: u8| {
        attrs.insert("level".to_owned(), tilo_doc::AttrVal::Int(i64::from(depth)));
    };
    let kind = match &content.kind {
        BlockKind::Paragraph => "paragraph",
        BlockKind::Quote => "quote",
        BlockKind::Callout(callout) => {
            attrs.insert(
                CALLOUT.to_owned(),
                tilo_doc::AttrVal::Str(callout.name().to_owned()),
            );
            "quote"
        }
        BlockKind::Heading(depth) => {
            level((*depth).clamp(1, 6));
            "heading"
        }
        BlockKind::Bullet { indent } => {
            level(*indent);
            attrs.insert("ordered".to_owned(), tilo_doc::AttrVal::Bool(false));
            "list_item"
        }
        BlockKind::Numbered { indent } => {
            level(*indent);
            attrs.insert("ordered".to_owned(), tilo_doc::AttrVal::Bool(true));
            "list_item"
        }
        BlockKind::Checklist { indent, done } => {
            level(*indent);
            attrs.insert("ordered".to_owned(), tilo_doc::AttrVal::Bool(false));
            attrs.insert("checked".to_owned(), tilo_doc::AttrVal::Bool(*done));
            "list_item"
        }
        BlockKind::Divider => "divider",
        BlockKind::CodeBlock { lang } => {
            attrs.insert("code".to_owned(), tilo_doc::AttrVal::Bool(true));
            if let Some(lang) = lang {
                attrs.insert("lang".to_owned(), tilo_doc::AttrVal::Str(lang.clone()));
            }
            if let Some(group) = &content.code_group {
                attrs.insert(
                    "code_group".to_owned(),
                    tilo_doc::AttrVal::Str(group.clone()),
                );
            }
            "paragraph"
        }
        BlockKind::Table { visual } => {
            attrs.insert(TABLE_SOURCE.to_owned(), tilo_doc::AttrVal::Bool(*visual));
            "paragraph"
        }
    };
    if content.nesting > 0 && !content.kind.supports_indent() {
        attrs.insert(
            "nesting".to_owned(),
            tilo_doc::AttrVal::Int(i64::from(content.nesting)),
        );
    }
    let marks = content
        .marks
        .iter()
        .filter_map(|span| {
            let (name, value) = stored_mark(&span.mark)?;
            let start = char_of_byte(&content.text, span.range.start);
            let end = char_of_byte(&content.text, span.range.end);
            (start < end).then(|| tilo_doc::document::MarkSpan {
                start,
                end,
                name: name.to_owned(),
                value,
            })
        })
        .collect();
    BlockView {
        id: content.id.clone(),
        kind: kind.to_owned(),
        text: content.text.clone(),
        marks,
        attrs,
        children: Vec::new(),
    }
}

/// The paragraph a component is given for `view`: [`view_of`]'s inverse, its marks and
/// attributes in the one form a window reads.
#[must_use]
pub fn block_of_view(view: &BlockView) -> tilo_ui_graph::rich::Block {
    use tilo_ui_graph::rich::Attr;
    let value = |value: &MarkValue| match value {
        MarkValue::Bool(flag) => Attr::Bool(*flag),
        MarkValue::Int(number) => Attr::Int(*number),
        MarkValue::Str(text) => Attr::Text(text.clone()),
    };
    tilo_ui_graph::rich::Block {
        kind: view.kind.clone(),
        text: view.text.clone(),
        marks: view
            .marks
            .iter()
            .map(|mark| tilo_ui_graph::rich::Mark {
                start: mark.start,
                end: mark.end,
                name: mark.name.clone(),
                value: value(&mark.value),
            })
            .collect(),
        attrs: view
            .attrs
            .iter()
            .map(|(key, value)| {
                let value = match value {
                    tilo_doc::AttrVal::Bool(flag) => Attr::Bool(*flag),
                    tilo_doc::AttrVal::Int(number) => Attr::Int(*number),
                    tilo_doc::AttrVal::Float(number) => Attr::Float(*number),
                    tilo_doc::AttrVal::Str(text) => Attr::Text(text.clone()),
                };
                (key.clone(), value)
            })
            .collect(),
    }
}

/// The blocks of `rich` as the editor that edits it holds them: each paragraph as its writer
/// wrote it, every reference as its run (never as what a reader reads for it), and a block id
/// that stands for its place, so the editor keeps a block's widget from one presentation to the
/// next.
#[must_use]
pub fn blocks_as_written(rich: &tilo_ui_graph::rich::RichText) -> Vec<BlockContent> {
    rich.blocks
        .iter()
        .enumerate()
        .map(|(index, block)| content_of(&view_of(block, format!("p{index}"))))
        .collect()
}

/// `rich` with the paragraphs `blocks` are: everything an editor's document writes back
/// ([`blocks_as_written`]'s inverse).
#[must_use]
pub fn rich_of_blocks(
    blocks: &[BlockContent],
    rich: &tilo_ui_graph::rich::RichText,
) -> tilo_ui_graph::rich::RichText {
    // Each of `rich`'s blocks until an editor block reads as it does: an editor's block ids
    // stand for its widget, not for a paragraph of the document, so what a block holds now is
    // the only name the two share.
    let mut taken = vec![false; rich.blocks.len()];
    let mut written = Vec::with_capacity(blocks.len());
    for content in blocks {
        let kept = (0..rich.blocks.len())
            .find(|index| {
                let block = &rich.blocks[*index];
                !taken[*index]
                    && block.text == content.text
                    && content_of(&view_of(block, content.id.clone())) == *content
            })
            .map(|index| {
                taken[index] = true;
                rich.blocks[index].clone()
            });
        written.push(kept.unwrap_or_else(|| block_of_view(&view_of_content(content))));
    }
    tilo_ui_graph::rich::RichText {
        blocks: written,
        embeds: rich.embeds.clone(),
    }
}

/// What a reference reads as for the person reading, by how the text says it reads
/// ([`tilo_ui_graph::rich::InlineView`]): a person as `@` and what the workspace calls them, a
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
        InlineView::Unreadable => unreadable_phrase(),
        InlineView::Person { .. } | InlineView::Resource { .. } | InlineView::Elsewhere => {
            run.to_owned()
        }
    }
}

/// How a reference the reader cannot read reads: as being there, without anything of it.
#[must_use]
pub fn unreadable_phrase() -> String {
    crate::i18n::I18nManager::lookup("rich-text-reference-unreadable", None)
}

pub const TIME_MARK: &str = tilo_doc::inline_time::MARK;

pub use tilo_doc::inline_time::Show as TimeShow;

/// A moment written into rich text (`tilo_doc::inline_time`), read: every reader sees it as
/// [`time_label`] reads it for them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InlineTime {
    pub at: chrono::DateTime<chrono::Utc>,
    pub show: TimeShow,
}

impl InlineTime {
    #[must_use]
    pub fn of_mark(value: &str) -> Option<Self> {
        let written = tilo_doc::inline_time::InlineTime::of_mark(value)?;
        let at = chrono::DateTime::parse_from_rfc3339(&written.at)
            .ok()?
            .with_timezone(&chrono::Utc);
        Some(Self {
            at,
            show: written.show,
        })
    }

    #[must_use]
    pub fn mark_value(&self) -> String {
        tilo_doc::inline_time::InlineTime {
            at: self.at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            show: self.show,
        }
        .mark_value()
    }

    /// How long until what it reads as changes, at `now`: a countdown each
    /// second, a relative time each minute, an absolute one never.
    #[must_use]
    pub fn changes_in(&self, now: chrono::DateTime<chrono::Utc>) -> Option<std::time::Duration> {
        let ahead = self.at.signed_duration_since(now).num_seconds();
        match self.show {
            TimeShow::Absolute => None,
            TimeShow::Countdown if ahead > 0 && ahead < 86_400 => {
                Some(std::time::Duration::from_secs(1))
            }
            TimeShow::Countdown | TimeShow::Relative => Some(std::time::Duration::from_secs(60)),
        }
    }
}

/// What `time` reads as for a person at `now`, in `zone` and `formats`.
#[must_use]
pub fn time_label<Tz: chrono::TimeZone>(
    time: &InlineTime,
    now: chrono::DateTime<chrono::Utc>,
    zone: &Tz,
    formats: crate::format::Formats,
) -> String {
    let ahead = time.at.signed_duration_since(now).num_seconds();
    match time.show {
        TimeShow::Absolute => formats.timestamp(time.at.with_timezone(zone).naive_local()),
        TimeShow::Countdown if ahead > 0 && ahead < 86_400 => {
            crate::format::timecode(u64::try_from(ahead).unwrap_or_default())
        }
        TimeShow::Relative | TimeShow::Countdown if ahead > 0 => {
            crate::format::relative_ahead(ahead)
        }
        TimeShow::Relative | TimeShow::Countdown => crate::format::relative(-ahead),
    }
}

/// Where, at `now`, a person reads rich text: their zone and formats.
#[derive(Clone, Copy, Debug)]
pub struct Reading<Tz: chrono::TimeZone> {
    pub now: chrono::DateTime<chrono::Utc>,
    pub zone: Tz,
    pub formats: crate::format::Formats,
}

#[must_use]
pub fn reading_here() -> Reading<chrono::Local> {
    Reading {
        now: crate::clock::local_now().with_timezone(&chrono::Utc),
        zone: chrono::Local,
        formats: crate::format::Formats::current(),
    }
}

/// The blocks rich text shows as for the person reading here, now ([`blocks_at`]).
#[must_use]
pub fn blocks_of(rich: &tilo_ui_graph::rich::RichText) -> Vec<BlockContent> {
    blocks_at(rich, &reading_here())
}

/// The blocks rich text shows as for `reading`: each reference as the text says it reads for
/// the person ([`reference_label`]), each time as it reads for them ([`time_label`]);
/// paragraph ids are their places.
#[must_use]
pub fn blocks_at<Tz: chrono::TimeZone>(
    rich: &tilo_ui_graph::rich::RichText,
    reading: &Reading<Tz>,
) -> Vec<BlockContent> {
    rich.blocks
        .iter()
        .enumerate()
        .map(|(index, block)| content_of(&read_view(rich, index, block, reading)))
        .collect()
}

/// The text rich text says to the person reading here, now, a line per paragraph: each
/// reference and time as [`blocks_at`] shows it, never a run as written (a reference's run
/// keeps the title its resource had then, which the reader may not be allowed to read).
#[must_use]
pub fn plain_text(rich: &tilo_ui_graph::rich::RichText) -> String {
    let reading = reading_here();
    rich.blocks
        .iter()
        .enumerate()
        .map(|(index, block)| read_view(rich, index, block, &reading).text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The paragraph `block` (the `index`th of `rich`) as it reads for `reading`.
fn read_view<Tz: chrono::TimeZone>(
    rich: &tilo_ui_graph::rich::RichText,
    index: usize,
    block: &tilo_ui_graph::rich::Block,
    reading: &Reading<Tz>,
) -> BlockView {
    let mut view = view_of(block, format!("p{index}"));
    relabel(&mut view, |name, value, run| match name {
        REFERENCE_MARK | EMBED_MARK => rich.embed(value).map(|embed| reference_label(embed, run)),
        TIME_MARK => InlineTime::of_mark(value)
            .map(|time| time_label(&time, reading.now, &reading.zone, reading.formats)),
        _ => None,
    });
    view
}

/// How soon, at `now`, any time in `rich` reads differently.
#[must_use]
pub fn changes_in(
    rich: &tilo_ui_graph::rich::RichText,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<std::time::Duration> {
    rich.blocks
        .iter()
        .flat_map(|block| &block.marks)
        .filter(|mark| mark.name == TIME_MARK)
        .filter_map(|mark| match &mark.value {
            tilo_ui_graph::rich::Attr::Text(value) => InlineTime::of_mark(value),
            _ => None,
        })
        .filter_map(|time| time.changes_in(now))
        .min()
}

/// Rewrite each run of `view` that `label` reads otherwise (given the mark's name, its value
/// and the run), shifting the marks after it.
fn relabel(view: &mut BlockView, label: impl Fn(&str, &str, &str) -> Option<String>) {
    let mut references: Vec<(usize, usize, String)> = view
        .marks
        .iter()
        .filter(|mark| mark.start < mark.end)
        .filter_map(|mark| match &mark.value {
            MarkValue::Str(value) => {
                let run: String = view
                    .text
                    .chars()
                    .skip(mark.start)
                    .take(mark.end.saturating_sub(mark.start))
                    .collect();
                label(&mark.name, value, &run).map(|label| (mark.start, mark.end, label))
            }
            MarkValue::Bool(_) | MarkValue::Int(_) => None,
        })
        .collect();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TimeStyle;
    use crate::format::{DateStyle, Formats, RegionFormats};
    use tilo_ui_graph::rich::{Attr, Block, Mark as RichMark, RichText};

    fn at(text: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(text)
            .expect("a moment")
            .with_timezone(&chrono::Utc)
    }

    /// `Standup ⟨time⟩, release ⟨time⟩, launch ⟨time⟩.` with the three ways a time is shown.
    fn three_times() -> RichText {
        let mark = |start: usize, end: usize, time: InlineTime| RichMark {
            start,
            end,
            name: TIME_MARK.into(),
            value: Attr::Text(time.mark_value()),
        };
        let standup = InlineTime {
            at: at("2026-10-29T23:30:00Z"),
            show: TimeShow::Absolute,
        };
        let release = InlineTime {
            at: at("2026-10-29T12:00:00Z"),
            show: TimeShow::Relative,
        };
        let launch = InlineTime {
            at: at("2026-10-29T11:04:59Z"),
            show: TimeShow::Countdown,
        };
        let text = "Standup 23:30 UTC, release noon, launch 11:05.";
        RichText {
            blocks: vec![Block {
                kind: "paragraph".into(),
                text: text.into(),
                marks: vec![
                    mark(8, 17, standup),
                    mark(27, 31, release),
                    mark(40, 45, launch),
                ],
                ..Block::default()
            }],
            embeds: Vec::new(),
        }
    }

    fn reading<Tz: chrono::TimeZone>(zone: Tz) -> Reading<Tz> {
        Reading {
            now: at("2026-10-29T10:00:00Z"),
            zone,
            formats: Formats::with(RegionFormats {
                date: DateStyle::Iso,
                time: TimeStyle::H24,
                ..RegionFormats::default()
            }),
        }
    }

    #[test]
    fn a_time_reads_for_each_reader_in_their_zone_and_ticks_toward_it() {
        let rich = three_times();
        assert_eq!(
            InlineTime::of_mark(
                &InlineTime {
                    at: at("2026-10-29T12:00:00Z"),
                    show: TimeShow::Relative
                }
                .mark_value()
            ),
            Some(InlineTime {
                at: at("2026-10-29T12:00:00Z"),
                show: TimeShow::Relative
            })
        );
        let utc = blocks_at(&rich, &reading(chrono::Utc));
        let tokyo = blocks_at(
            &rich,
            &reading(chrono::FixedOffset::east_opt(9 * 3600).expect("a zone")),
        );
        let (utc, tokyo) = (utc[0].text.as_str(), tokyo[0].text.as_str());
        assert!(utc.contains("2026-10-29") && utc.contains("23:30"), "{utc}");
        assert!(
            tokyo.contains("2026-10-30") && tokyo.contains("08:30"),
            "the next morning in Tokyo: {tokyo}"
        );
        assert!(utc.contains("release in 2 hr,"), "{utc}");
        assert!(utc.contains("launch 1:04:59."), "a countdown: {utc}");
        assert!(
            !utc.contains("noon"),
            "nobody reads the writer's words for it"
        );

        let mut later = reading(chrono::Utc);
        later.now = at("2026-10-29T11:08:00Z");
        assert!(
            blocks_at(&rich, &later)[0]
                .text
                .contains("launch 3 min ago."),
            "{}",
            blocks_at(&rich, &later)[0].text
        );
        assert_eq!(
            changes_in(&rich, at("2026-10-29T10:00:00Z")),
            Some(std::time::Duration::from_secs(1)),
            "a running countdown changes every second"
        );
        assert_eq!(
            changes_in(&rich, at("2026-10-29T12:30:00Z")),
            Some(std::time::Duration::from_secs(60))
        );
    }

    #[test]
    fn a_time_reads_in_the_readers_language() {
        use crate::i18n::I18nManager;
        let rich = three_times();
        I18nManager::init("en-US");
        I18nManager::add_resource("de-DE", "window = Fenster".to_owned());
        I18nManager::register_widget_messages();
        I18nManager::set_locale("de-DE");
        let german = blocks_at(&rich, &reading(chrono::Utc))[0].text.clone();
        I18nManager::set_locale("en-US");
        assert!(german.contains("in 2 Std."), "{german}");
    }

    #[test]
    fn a_document_round_trips_through_the_editor() {
        let attrs = |pairs: &[(&str, Attr)]| {
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone()))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let written = RichText {
            blocks: vec![
                Block {
                    kind: "heading".into(),
                    text: "Title".into(),
                    attrs: attrs(&[("level", Attr::Int(3))]),
                    ..Block::default()
                },
                Block {
                    kind: "list_item".into(),
                    text: "one".into(),
                    ..Block::default()
                },
                Block {
                    kind: "list_item".into(),
                    text: "two".into(),
                    attrs: attrs(&[("level", Attr::Int(2)), ("checked", Attr::Bool(true))]),
                    ..Block::default()
                },
                Block {
                    kind: "quote".into(),
                    text: "careful".into(),
                    attrs: attrs(&[(CALLOUT, Attr::Text("warning".into()))]),
                    ..Block::default()
                },
                Block {
                    kind: "paragraph".into(),
                    text: "fn main() {}".into(),
                    attrs: attrs(&[
                        ("code", Attr::Bool(true)),
                        ("lang", Attr::Text("rust".into())),
                    ]),
                    ..Block::default()
                },
                Block {
                    kind: "divider".into(),
                    ..Block::default()
                },
                Block {
                    kind: "paragraph".into(),
                    text: "Héllo wörld".into(),
                    marks: vec![
                        RichMark {
                            start: 0,
                            end: 5,
                            name: "strong".into(),
                            value: Attr::Bool(true),
                        },
                        RichMark {
                            start: 6,
                            end: 11,
                            name: "em".into(),
                            value: Attr::Bool(true),
                        },
                    ],
                    ..Block::default()
                },
            ],
            embeds: Vec::new(),
        };
        let blocks = blocks_as_written(&written);
        assert_eq!(
            blocks[0].kind,
            BlockKind::Heading(3),
            "the stored form as the editor holds it"
        );
        assert_eq!(
            blocks[2].kind,
            BlockKind::Checklist {
                indent: 2,
                done: true
            }
        );
        assert_eq!(blocks[3].kind, BlockKind::Callout(CalloutKind::Warning));
        assert_eq!(
            blocks[4].kind,
            BlockKind::CodeBlock {
                lang: Some("rust".into())
            }
        );
        assert_eq!(
            blocks[6].marks,
            vec![
                MarkSpan::new(0..6, Mark::Bold),
                MarkSpan::new(7..13, Mark::Italic),
            ],
            "offsets count characters, the editor counts bytes"
        );

        let back = rich_of_blocks(&blocks, &written);
        assert_eq!(blocks_as_written(&back), blocks);
        let stored = view_of_content(&blocks[6]);
        let spans: Vec<(usize, usize, &str)> = stored
            .marks
            .iter()
            .map(|mark| (mark.start, mark.end, mark.name.as_str()))
            .collect();
        assert_eq!(
            spans,
            [(0, 5, "strong"), (6, 11, "em")],
            "the stored marks are the ones written, counted in characters again"
        );
        assert_eq!(stored.text, "Héllo wörld");
        assert_eq!(view_of_content(&blocks[2]).kind, "list_item");
        assert_eq!(
            view_of_content(&blocks[2]).attrs.get("checked"),
            Some(&tilo_doc::AttrVal::Bool(true))
        );
        assert_eq!(view_of_content(&blocks[0]).kind, "heading");
        assert_eq!(
            view_of_content(&blocks[0]).attrs.get("level"),
            Some(&tilo_doc::AttrVal::Int(3))
        );
        assert_eq!(view_of_content(&blocks[5]).kind, "divider");
        assert_eq!(block_of_view(&stored).text, "Héllo wörld");

        // A mark the editor cannot draw is the document's, not the editor's: a paragraph the
        // person did not touch keeps it, and one they changed writes what the editor shows.
        let mut timed = written.clone();
        timed.blocks[6].marks.push(RichMark {
            start: 0,
            end: 5,
            name: "time".into(),
            value: Attr::Text("2026-11-02T09:00:00Z".into()),
        });
        let untouched = blocks_as_written(&timed);
        let back = rich_of_blocks(&untouched, &timed);
        assert_eq!(
            back.blocks[6], timed.blocks[6],
            "the paragraph is kept as it was written"
        );
        let mut edited = untouched.clone();
        edited[6].text = "Héllo again".into();
        let back = rich_of_blocks(&edited, &timed);
        assert_eq!(back.blocks[6].text, "Héllo again");
        assert!(
            back.blocks[6].marks.iter().all(|mark| mark.name != "time"),
            "what the editor shows is what a changed paragraph is written as"
        );
        let mut renamed = untouched.clone();
        renamed[6].id = "split-3".into();
        renamed.insert(6, BlockContent::paragraph("made room for it", ""));
        let back = rich_of_blocks(&renamed, &timed);
        assert_eq!(
            back.blocks[7], timed.blocks[6],
            "the paragraph still reads as it was written, its mark with it"
        );
    }

    #[test]
    fn plain_text_reads_each_reference_as_its_reader_does() {
        use tilo_ui_graph::rich::{Embed, InlineView};
        const TASK: &str =
            r#"{"app":"so.tilo.lang.board","id":"tl:so.tilo.lang.board:d/f:t1","kind":""}"#;
        let read_as = |view: InlineView| RichText {
            blocks: vec![Block {
                kind: "paragraph".into(),
                text: "Notes in Acquisition plan".into(),
                marks: vec![RichMark {
                    start: 9,
                    end: 25,
                    name: REFERENCE_MARK.into(),
                    value: Attr::Text(TASK.into()),
                }],
                attrs: std::collections::BTreeMap::new(),
            }],
            embeds: vec![Embed {
                reference: TASK.into(),
                view,
                preview: None,
            }],
        };
        let holder = read_as(InlineView::Resource {
            title: "Merger timeline".into(),
            icon: None,
            detail: None,
        });
        assert_eq!(plain_text(&holder), "Notes in Merger timeline");
        let other = plain_text(&read_as(InlineView::Unreadable));
        assert_eq!(other, "Notes in Not shared with you");
    }
}
