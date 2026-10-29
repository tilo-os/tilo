//! Between the document's paragraphs and the editor's blocks.
//!
//! tilo-doc names paragraph kinds and inline marks the way its codecs do
//! (`heading` with a `level`, `list_item` with `ordered`/`checked`, `strong`,
//! `em`), and counts offsets in characters. The block editor has its own
//! kinds and marks and counts bytes. Everything the editor shows passes
//! through [`content_of`]; everything it writes back goes through
//! [`KindSpec`] and [`mark_runs`].

use std::collections::BTreeMap;

use tilo_doc::snapshot::BlockView;
use tilo_document::action::{AttrVal as ActionAttr, MarkVal};
use ui::widget::BlockContent;
use ui::widget::BlockKind;
use ui::widget::marks::Mark;

pub(crate) use ui::widget::rich_doc::{
    CALLOUT, PAGE_LINK_PREFIX, TABLE_SOURCE, bool_attr, int_attr,
};
pub use ui::widget::rich_doc::{RESOURCE_LINK_SCHEME, content_of, editor_mark, is_resource_link};

/// The character offset of byte `byte` in `text`.
pub(crate) fn char_of_byte(text: &str, byte: usize) -> usize {
    text.char_indices().take_while(|(at, _)| *at < byte).count()
}

/// Number of characters in `text`.
pub(crate) fn chars(text: &str) -> usize {
    text.chars().count()
}

/// The tilo-doc mark an editor mark is stored as: its name and value, for an
/// edit that sets it ([`RichTextAction::SetMark`](tilo_document::RichTextAction::SetMark)).
pub fn document_mark(mark: &Mark) -> (&'static str, MarkVal) {
    match mark {
        Mark::Bold => ("strong", MarkVal::Bool(true)),
        Mark::Italic => ("em", MarkVal::Bool(true)),
        Mark::Underline => ("underline", MarkVal::Bool(true)),
        Mark::Strikethrough => ("strike", MarkVal::Bool(true)),
        Mark::Code => ("fontFamily", MarkVal::Str("mono".to_owned())),
        Mark::Link(page) => ("link", MarkVal::Str(format!("{PAGE_LINK_PREFIX}{page}"))),
        Mark::Transclusion(page) => ("transclusion", MarkVal::Str(page.to_string())),
        Mark::ExternalLink(target) => ("link", MarkVal::Str(target.clone())),
        Mark::ResourceLink(href) => ("link", MarkVal::Str(href.clone())),
        Mark::Mention(id) => ("mention", MarkVal::Str(id.clone())),
    }
}

/// The marks the editor can show, by name. A document mark the editor has no
/// form for (a colour, a comment anchor, a serif family) is neither shown nor
/// ever written by the editor.
pub(crate) const EDITOR_MARKS: [&str; 8] = [
    "strong",
    "em",
    "underline",
    "strike",
    "fontFamily",
    "link",
    "transclusion",
    "mention",
];

/// A paragraph as the paste vocabulary carries it: kind, text, marks and
/// attributes, so an action can write it elsewhere (a comment reply, a
/// message) as it was written.
pub fn clip_block(view: &BlockView) -> tilo_document::ClipBlock {
    tilo_document::ClipBlock::from_view(view)
}

/// What a paragraph an action carries shows as in the editor.
pub fn content_of_clip(clip: &tilo_document::ClipBlock) -> BlockContent {
    use tilo_document::ClipAttr;
    let mut attrs: BTreeMap<String, tilo_doc::AttrVal> = clip
        .attrs
        .iter()
        .map(|(key, value)| {
            let value = match value {
                ClipAttr::Bool(value) => tilo_doc::AttrVal::Bool(*value),
                ClipAttr::Int(value) => tilo_doc::AttrVal::Int(*value),
                ClipAttr::Str(value) => tilo_doc::AttrVal::Str(value.clone()),
            };
            (key.clone(), value)
        })
        .collect();
    if let Some(level) = clip.level {
        attrs.insert("level".into(), tilo_doc::AttrVal::Int(level));
    }
    if let Some(ordered) = clip.ordered {
        attrs.insert("ordered".into(), tilo_doc::AttrVal::Bool(ordered));
    }
    if let Some(align) = &clip.align {
        attrs.insert("align".into(), tilo_doc::AttrVal::Str(align.clone()));
    }
    content_of(&BlockView {
        id: String::new(),
        kind: clip.kind.clone(),
        text: clip.text.clone(),
        marks: clip.marks.clone(),
        attrs,
        children: Vec::new(),
    })
}

/// A paragraph the editor shows, as an action carries it: written the way
/// the editor stores it, then read back.
pub fn clip_of_content(content: &BlockContent) -> tilo_document::ClipBlock {
    use ui::automerge::{ObjType, ROOT, transaction::Transactable};
    let mut doc = ui::automerge::AutoCommit::new();
    let clips = doc
        .put_object(ROOT, "clip", ObjType::Map)
        .ok()
        .filter(|root| {
            tilo_document::RichText::initialize_under(&mut doc, root).is_ok()
                && crate::fill_under(&mut doc, root, std::slice::from_ref(content)).is_ok()
        })
        .and_then(|root| tilo_doc::model::open(&doc, &root).ok())
        .map(|containers| {
            tilo_doc::model::materialize_region(&doc, &containers, tilo_doc::model::Region::Body)
                .iter()
                .map(clip_block)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    clips
        .into_iter()
        .next()
        .unwrap_or_else(|| tilo_document::ClipBlock {
            kind: "paragraph".into(),
            text: content.text.clone(),
            marks: Vec::new(),
            level: None,
            ordered: None,
            align: None,
            attrs: BTreeMap::new(),
        })
}

/// How an editor block is stored: the paragraph's kind with its `level` and
/// `ordered`, and the attributes whose absence is their default.
#[derive(Debug, PartialEq)]
pub(crate) struct KindSpec {
    pub kind: &'static str,
    pub level: Option<i64>,
    pub ordered: Option<bool>,
    /// `(key, value)`; `None` removes the attribute.
    pub attrs: Vec<(&'static str, Option<ActionAttr>)>,
}

impl KindSpec {
    pub(crate) fn of(content: &BlockContent) -> Self {
        let (kind, level, ordered) = match &content.kind {
            BlockKind::Heading(level) => ("heading", Some(i64::from(*level)), None),
            BlockKind::Quote | BlockKind::Callout(_) => ("quote", None, None),
            BlockKind::Bullet { indent } | BlockKind::Checklist { indent, .. } => {
                ("list_item", Some(i64::from(*indent)), Some(false))
            }
            BlockKind::Numbered { indent } => ("list_item", Some(i64::from(*indent)), Some(true)),
            BlockKind::Divider => ("divider", None, None),
            BlockKind::Paragraph | BlockKind::CodeBlock { .. } | BlockKind::Table { .. } => {
                ("paragraph", None, None)
            }
        };
        let checked = match content.kind {
            BlockKind::Checklist { done, .. } => Some(ActionAttr::Bool(done)),
            _ => None,
        };
        let table = match content.kind {
            BlockKind::Table { visual } => Some(ActionAttr::Bool(visual)),
            _ => None,
        };
        let callout = match content.kind {
            BlockKind::Callout(kind) => Some(ActionAttr::Str(kind.name().to_owned())),
            _ => None,
        };
        let (code, lang, group) = match &content.kind {
            BlockKind::CodeBlock { lang } => (
                Some(ActionAttr::Bool(true)),
                lang.clone().map(ActionAttr::Str),
                content.code_group.clone().map(ActionAttr::Str),
            ),
            _ => (None, None, None),
        };
        let nesting = (content.nesting > 0 && !content.kind.supports_indent())
            .then(|| ActionAttr::Int(i64::from(content.nesting)));
        Self {
            kind,
            level,
            ordered,
            attrs: vec![
                ("checked", checked),
                ("code", code),
                ("lang", lang),
                ("code_group", group),
                ("nesting", nesting),
                (TABLE_SOURCE, table),
                (CALLOUT, callout),
            ],
        }
    }

    /// Whether `view` is stored with this kind, level and order.
    pub(crate) fn same_style(&self, view: &BlockView) -> bool {
        let level = int_attr(view, "level");
        let ordered = bool_attr(view, "ordered");
        view.kind == self.kind
            && self.level.is_none_or(|want| level.unwrap_or(0) == want)
            && self
                .ordered
                .is_none_or(|want| ordered.unwrap_or(false) == want)
    }

    /// The attributes `view` holds otherwise than this spec wants.
    pub(crate) fn attr_changes(&self, view: &BlockView) -> Vec<(&'static str, Option<ActionAttr>)> {
        self.attrs
            .iter()
            .filter(|(key, want)| {
                let have = view.attrs.get(*key).map(|value| match value {
                    tilo_doc::AttrVal::Bool(value) => ActionAttr::Bool(*value),
                    tilo_doc::AttrVal::Int(value) => ActionAttr::Int(*value),
                    tilo_doc::AttrVal::Float(value) => ActionAttr::Float(*value),
                    tilo_doc::AttrVal::Str(value) => ActionAttr::Str(value.clone()),
                });
                have != *want
            })
            .cloned()
            .collect()
    }
}

/// Per mark name, the value over each character, for the names the editor
/// shows.
pub(crate) type MarkRuns = BTreeMap<&'static str, Vec<Option<MarkVal>>>;

fn empty_runs(len: usize) -> MarkRuns {
    EDITOR_MARKS
        .iter()
        .map(|name| (*name, vec![None; len]))
        .collect()
}

fn cover(runs: &mut MarkRuns, name: &str, value: &MarkVal, start: usize, end: usize) {
    if let Some(chars) = runs.get_mut(name) {
        for slot in chars.iter_mut().take(end).skip(start) {
            *slot = Some(value.clone());
        }
    }
}

/// The marks an editor block carries, per character.
pub(crate) fn editor_runs(content: &BlockContent) -> MarkRuns {
    let len = chars(&content.text);
    let mut runs = empty_runs(len);
    for span in &content.marks {
        let (name, value) = document_mark(&span.mark);
        let start = char_of_byte(&content.text, span.range.start);
        let end = char_of_byte(&content.text, span.range.end);
        cover(&mut runs, name, &value, start, end);
    }
    runs
}

/// The marks a stored paragraph carries that the editor could show, per
/// character. A value the editor has no form for is left out, so the editor
/// never clears what it cannot show.
pub(crate) fn document_runs(view: &BlockView) -> MarkRuns {
    let len = chars(&view.text);
    let mut runs = empty_runs(len);
    for span in &view.marks {
        let Some(mark) = editor_mark(&span.name, &span.value) else {
            continue;
        };
        let (name, value) = document_mark(&mark);
        if name != span.name {
            continue;
        }
        cover(&mut runs, name, &value, span.start, span.end);
    }
    runs
}

/// One mark to write: its name, its value (`None` to clear) and the
/// character ranges it covers.
pub(crate) type MarkEdit = (&'static str, Option<MarkVal>, Vec<(usize, usize)>);

/// Where `want` differs from `have`: per mark name and value, the character
/// ranges to write.
pub(crate) fn mark_runs(want: &MarkRuns, have: &MarkRuns) -> Vec<MarkEdit> {
    let mut out: Vec<MarkEdit> = Vec::new();
    for (name, wanted) in want {
        let Some(had) = have.get(name) else {
            continue;
        };
        let mut at = 0;
        while at < wanted.len() {
            let target = wanted.get(at).cloned().flatten();
            if had.get(at).cloned().flatten() == target {
                at = at.saturating_add(1);
                continue;
            }
            let start = at;
            while at < wanted.len()
                && wanted.get(at).cloned().flatten() == target
                && had.get(at).cloned().flatten() != target
            {
                at = at.saturating_add(1);
            }
            match out
                .iter_mut()
                .find(|(n, value, _)| n == name && *value == target)
            {
                Some((_, _, ranges)) => ranges.push((start, at)),
                None => out.push((name, target, vec![(start, at)])),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_doc::document::MarkSpan as DocMark;
    use tilo_doc::document::MarkValue;
    use ui::widget::CalloutKind;
    use ui::widget::marks::MarkSpan;

    fn view(kind: &str, text: &str, attrs: &[(&str, tilo_doc::AttrVal)]) -> BlockView {
        BlockView {
            id: "b".into(),
            kind: kind.into(),
            text: text.into(),
            marks: Vec::new(),
            attrs: attrs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone()))
                .collect(),
            children: Vec::new(),
        }
    }

    #[test]
    fn kinds_round_trip_through_their_stored_form() {
        let cases = [
            view("paragraph", "", &[]),
            view("heading", "", &[("level", tilo_doc::AttrVal::Int(2))]),
            view("quote", "", &[]),
            view(
                "quote",
                "",
                &[(CALLOUT, tilo_doc::AttrVal::Str("warning".into()))],
            ),
            view(
                "list_item",
                "",
                &[
                    ("ordered", tilo_doc::AttrVal::Bool(true)),
                    ("level", tilo_doc::AttrVal::Int(1)),
                ],
            ),
            view(
                "list_item",
                "",
                &[
                    ("ordered", tilo_doc::AttrVal::Bool(false)),
                    ("level", tilo_doc::AttrVal::Int(0)),
                    ("checked", tilo_doc::AttrVal::Bool(true)),
                ],
            ),
            view(
                "paragraph",
                "",
                &[
                    ("code", tilo_doc::AttrVal::Bool(true)),
                    ("lang", tilo_doc::AttrVal::Str("rust".into())),
                ],
            ),
        ];
        assert_eq!(
            content_of(&cases[3]).kind,
            BlockKind::Callout(CalloutKind::Warning),
            "a quote with an alert is that callout"
        );
        let plain = KindSpec::of(&content_of(&cases[2]));
        assert!(plain.same_style(&cases[3]));
        assert_eq!(plain.attr_changes(&cases[3]), [(CALLOUT, None)]);
        for case in cases {
            let content = content_of(&case);
            let spec = KindSpec::of(&content);
            assert!(spec.same_style(&case), "{case:?} as {content:?}");
            assert!(
                spec.attr_changes(&case).is_empty(),
                "{case:?}: {:?}",
                spec.attr_changes(&case)
            );
        }
    }

    #[test]
    fn marks_convert_between_characters_and_bytes() {
        let mut paragraph = view("paragraph", "héllo wörld", &[]);
        paragraph.marks.push(DocMark {
            start: 6,
            end: 11,
            name: "strong".into(),
            value: MarkValue::Bool(true),
        });
        paragraph.marks.push(DocMark {
            start: 0,
            end: 5,
            name: "color".into(),
            value: MarkValue::Str("#ff0000".into()),
        });
        let content = content_of(&paragraph);
        assert_eq!(
            content.marks,
            [MarkSpan::new(7..13, Mark::Bold)],
            "a colour has no editor form"
        );
        assert_eq!(editor_runs(&content), document_runs(&paragraph));
    }

    #[test]
    fn differences_are_runs_to_set_or_clear() {
        let mut want = empty_runs(6);
        let mut have = empty_runs(6);
        cover(&mut want, "strong", &MarkVal::Bool(true), 0, 3);
        cover(&mut have, "strong", &MarkVal::Bool(true), 2, 6);
        let runs = mark_runs(&want, &have);
        assert_eq!(
            runs,
            [
                ("strong", Some(MarkVal::Bool(true)), vec![(0, 2)]),
                ("strong", None, vec![(3, 6)]),
            ]
        );
    }

    #[test]
    fn a_serif_family_the_editor_cannot_show_is_left_alone() {
        let mut paragraph = view("paragraph", "abc", &[]);
        paragraph.marks.push(DocMark {
            start: 0,
            end: 3,
            name: "fontFamily".into(),
            value: MarkValue::Str("serif".into()),
        });
        let content = content_of(&paragraph);
        assert!(mark_runs(&editor_runs(&content), &document_runs(&paragraph)).is_empty());
    }

    #[test]
    fn a_link_to_a_resource_reads_as_one_and_stays_a_link() {
        let href = "tilo://open#/me/so.tilo.calendar/-/calendar.event:e-1";
        assert_eq!(
            editor_mark("link", &MarkValue::Str(href.into())),
            Some(Mark::ResourceLink(href.into())),
            "the editor shows the resource an address names"
        );
        assert_eq!(
            document_mark(&Mark::ResourceLink(href.into())),
            ("link", MarkVal::Str(href.into())),
            "the document keeps the link, which a reader without this editor opens"
        );
        assert_eq!(
            editor_mark("link", &MarkValue::Str("https://example.test".into())),
            Some(Mark::ExternalLink("https://example.test".into())),
            "a web address stays a web link"
        );
        assert!(is_resource_link(href));
        assert!(!is_resource_link("https://example.test"));
        assert!(
            !is_resource_link(RESOURCE_LINK_SCHEME),
            "a bare scheme names nothing"
        );
    }
}
