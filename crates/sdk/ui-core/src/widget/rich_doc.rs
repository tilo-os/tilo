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

/// The mark a time's run carries (`tilo_doc::inline_time`).
pub const TIME_MARK: &str = tilo_doc::inline_time::MARK;

/// How an inline time reads for the person reading.
pub use tilo_doc::inline_time::Show as TimeShow;

/// A moment written into rich text (`tilo_doc::inline_time`), read: every reader sees it as
/// [`time_label`] reads it for them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InlineTime {
    pub at: chrono::DateTime<chrono::Utc>,
    pub show: TimeShow,
}

impl InlineTime {
    /// The time a [`TIME_MARK`] mark's value names.
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

    /// The [`TIME_MARK`] mark's value naming it.
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

/// The person reading here, now.
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
        .map(|(index, block)| {
            let mut view = view_of(block, format!("p{index}"));
            relabel(&mut view, |name, value, run| match name {
                REFERENCE_MARK => rich.embed(value).map(|embed| reference_label(embed, run)),
                TIME_MARK => InlineTime::of_mark(value)
                    .map(|time| time_label(&time, reading.now, &reading.zone, reading.formats)),
                _ => None,
            });
            content_of(&view)
        })
        .collect()
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
        // Each run is the moment as its writer wrote it.
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

        // Past the moment, a countdown says how long ago it was; times tick on their own.
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
        I18nManager::register_widget_messages();
        I18nManager::set_locale("de-DE");
        let german = blocks_at(&rich, &reading(chrono::Utc))[0].text.clone();
        I18nManager::set_locale("en-US");
        assert!(german.contains("in 2 Std."), "{german}");
    }
}
