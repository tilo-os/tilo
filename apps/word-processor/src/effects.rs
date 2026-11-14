//! What the word processor does that is not an edit of its document: reading and
//! writing files, what the window shows, and each participant's caret and selection.

use ui::tilo_resource::{self, Effect, Place};

use crate::actions::editing::{
    self, DeleteDirection, EditResult, EditingError, InlineMark, ParagraphStyle, PasteContent,
};
use crate::actions::participant::ParticipantView;

/// The word processor's effects. Block ids are the document's stable paragraph ids;
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "word-processor.effects")]
pub enum WordProcessorEffect {
    /// Share image
    /// The picture at the caret, else the document's only one, as content.
    #[effect(content, kind = "read")]
    ShareImage,
    /// Open document.
    #[effect(job, destructive)]
    OpenDocument {
        #[param("Absolute filesystem path; omit to choose one")]
        path: Option<ui::tilo_resource::AbsolutePath>,
    },
    /// Save document.
    #[effect(job, destructive)]
    Save {
        #[param("Absolute filesystem path; omit to save to the document's file")]
        path: Option<ui::tilo_resource::AbsolutePath>,
    },
    /// Save document as.
    #[effect(job, destructive)]
    SaveAs {
        #[param("Absolute filesystem path; omit to choose one")]
        path: Option<ui::tilo_resource::AbsolutePath>,
    },
    /// Read a file.
    /// What a document file says, as Markdown: Word, `OpenDocument`, RTF, HTML, EPUB,
    /// Markdown; the document stays as it is. A long file answers in parts: read on from `next`.
    #[effect(kind = "read", returns = crate::file_convert::ReadDocument, idempotent, audience = "agents")]
    ReadFile {
        #[param("Absolute path of the file")]
        file: ui::tilo_resource::AbsolutePath,
        #[param("The character to read from; the start when none")]
        from: Option<usize>,
    },
    /// Read a document kept as content.
    /// As `read-file`, for content the workspace holds; the format is read from its
    /// contents. A picture has no text: it answers with its type and size.
    /// it.
    #[effect(kind = "read", returns = crate::file_convert::ReadDocument, idempotent, audience = "agents")]
    ReadContent {
        #[param("The document's content address, `tilo.content:<hash>`")]
        content: String,
        #[param("Its file name, e.g. report.docx")]
        name: String,
        #[param("The character to read from; the start when none")]
        from: Option<usize>,
    },
    /// Import a document kept as content.
    /// Replaces the body with a Word file (or `OpenDocument`, HTML, RTF, Markdown) from
    /// content while the document is empty; a document someone wrote is never replaced.
    #[effect(returns, kind = "undoable", audience = "agents")]
    ImportDocxContent {
        #[param(content, "The document's content address, `tilo.content:<hash>`")]
        content: String,
        #[param("Its file name, e.g. report.docx; the format is read from the content")]
        name: Option<String>,
    },
    /// Export document as content.
    /// Keeps the document as a file in the content store, Word unless `format` names another.
    #[effect(content, kind = "read", idempotent)]
    ExportContent {
        #[param("docx (default), odt, md, html, rtf, txt or pdf")]
        format: Option<String>,
    },
    /// Convert a file.
    /// Writes a file again in another format beside it (PDF, Word, `OpenDocument`, Markdown,
    /// HTML, RTF, plain text) and answers with the new file's path.
    #[effect(returns)]
    ConvertFile {
        #[param("Absolute path of the file")]
        file: ui::tilo_resource::AbsolutePath,
        #[param(
            "The format to write",
            choices = crate::file_convert::CONVERSIONS.iter().copied()
        )]
        to: String,
    },
    /// Find in document.
    #[effect(view, idempotent)]
    Find,
    /// Zoom in.
    #[effect(view, idempotent)]
    ZoomIn,
    /// Zoom out.
    #[effect(view, idempotent)]
    ZoomOut,
    /// Reset zoom to 100%.
    #[effect(view, idempotent)]
    ZoomReset,
    /// Toggle spellcheck.
    #[effect(view)]
    Spellcheck,
    /// Toggle review markup.
    #[effect(view)]
    Markup,
    /// Toggle styles panel.
    #[effect(view)]
    Styles,
    /// Toggle ruler.
    #[effect(view)]
    Ruler,
    /// Toggle outline.
    #[effect(view)]
    Outline,
    /// Move caret.
    #[effect(view, participant, idempotent, audience = "agents")]
    MoveCaret {
        #[param(
            "point (default; needs block and offset), document-start, document-end or block-end",
            choices = ["point", "document-start", "document-end", "block-end"]
        )]
        to: Option<String>,
        #[param("Block id for a point")]
        block: Option<String>,
        #[param("Character offset for a point")]
        offset: Option<usize>,
    },
    /// Select range.
    #[effect(view, participant, idempotent, audience = "agents")]
    SelectRange {
        #[param("Block where the selection starts")]
        start_block: String,
        #[param("Character offset in start_block")]
        start: usize,
        #[param("Block where the selection ends")]
        end_block: String,
        #[param("Character offset in end_block")]
        end: usize,
    },
    /// Select paragraph.
    #[effect(view, participant, idempotent, audience = "agents")]
    SelectBlock {
        #[param("Block id to select")]
        block: String,
    },
    /// Select all.
    #[effect(view, participant, idempotent)]
    #[effect(label = "menu-select-all")]
    SelectAll,
    /// Find text.
    /// Case-insensitive; the results appear as search-hit resources.
    #[effect(view, participant, idempotent, audience = "agents")]
    FindText {
        #[param("Text to find (empty clears Find)")]
        query: String,
    },
    /// Type text.
    #[effect(edit, participant, audience = "agents")]
    TypeText {
        #[param(
            "Text typed at the caret, replacing the selection (empty only erases a selection)"
        )]
        text: String,
        #[param(
            "Where to act (a paragraph, a point or a range in it); your caret or selection when not given",
            place = "blocks"
        )]
        at: Option<Place>,
    },
    /// New paragraph.
    #[effect(edit, participant, audience = "agents")]
    NewParagraph,
    /// Erase text.
    #[effect(edit, participant, audience = "agents")]
    EraseText {
        #[param(
            "backward (Backspace, default) or forward (Delete)",
            choices = ["backward", "forward"]
        )]
        direction: Option<String>,
        #[param("Characters to erase (default 1, at most 256); a paragraph break counts as one")]
        count: Option<usize>,
    },
    /// Toggle formatting.
    /// Off where the selection covers all of it (super and sub also the caret paragraph).
    #[effect(edit, participant)]
    ToggleMark {
        #[param("The inline mark to toggle", choices = TOGGLED_MARKS)]
        mark: String,
        #[param(
            "Where to act (a paragraph, a point or a range in it); your caret or selection when not given",
            place = "blocks"
        )]
        at: Option<Place>,
    },
    /// Set text format.
    #[effect(edit, participant)]
    SetFormat {
        #[param("The inline mark", choices = FORMATS)]
        name: String,
        #[param("strong, em, underline, strike, super, sub: true sets it, false removes it")]
        on: Option<bool>,
        #[param("font: sans|serif|mono; size: points 6-200; color/highlight: #rrggbb")]
        value: Option<String>,
        #[param(
            "Where to act (a paragraph, a point or a range in it); your caret or selection when not given",
            place = "blocks"
        )]
        at: Option<Place>,
    },
    /// Clear text format.
    #[effect(edit, participant)]
    ClearFormat {
        #[param("The inline mark to remove", choices = FORMATS)]
        name: String,
        #[param(
            "Where to act (a paragraph, a point or a range in it); your caret or selection when not given",
            place = "blocks"
        )]
        at: Option<Place>,
    },
    /// Set paragraph style.
    #[effect(edit, participant, job, returns)]
    SetParagraphStyle {
        #[param(
            "Paragraph kind; bullet and numbered are list items",
            choices = ["paragraph", "heading", "list_item", "bullet", "numbered"]
        )]
        kind: String,
        #[param("Heading level 1-6")]
        level: Option<i64>,
        #[param("Numbered (true) or bulleted (false) list")]
        ordered: Option<bool>,
        #[param(
            "Where to act (a paragraph, a point or a range in it); your caret or selection when not given",
            place = "blocks"
        )]
        at: Option<Place>,
    },
    /// Apply style.
    /// `body`, `heading1` to `heading3`, `quote`, `list`, or a custom style's name.
    #[effect(edit, participant)]
    ApplyNamedStyle {
        #[param("Named style id")]
        style: String,
        #[param(
            "Where to act (a paragraph, a point or a range in it); your caret or selection when not given",
            place = "blocks"
        )]
        at: Option<Place>,
    },
    /// Paste.
    /// Text or Markdown (headings, lists, pipe tables).
    #[effect(edit, participant, job, returns, audience = "agents")]
    #[effect(label = "menu-paste")]
    Paste {
        #[param("Plain text; each line becomes a paragraph")]
        text: Option<String>,
        #[param("Markdown pasted with its formatting")]
        markdown: Option<String>,
        #[param(
            "Where to act (a paragraph, a point or a range in it); your caret or selection when not given",
            place = "blocks"
        )]
        at: Option<Place>,
    },
    /// Align paragraph.
    #[effect(edit, participant)]
    Align {
        #[param("How the caret paragraph lines up", choices = ["left", "center", "right", "justify"])]
        align: String,
        #[param(
            "Where to act (a paragraph, a point or a range in it); your caret or selection when not given",
            place = "blocks"
        )]
        at: Option<Place>,
    },
    /// Insert table here.
    #[effect(edit, participant)]
    InsertTableHere {
        #[param("Rows (default 2)")]
        rows: Option<usize>,
        #[param("Columns (default 2)")]
        columns: Option<usize>,
        #[param(
            "Where to act (a paragraph, a point or a range in it); your caret or selection when not given",
            place = "blocks"
        )]
        at: Option<Place>,
    },
    /// Insert page break.
    #[effect(edit, participant)]
    InsertPageBreak {
        #[param(
            "Where to act (a paragraph, a point or a range in it); your caret or selection when not given",
            place = "blocks"
        )]
        at: Option<Place>,
    },
    /// Add or change hyperlink.
    #[effect(edit, participant)]
    SetHyperlink {
        #[param("A web address (https://…), a mail address (mailto:…) or a #bookmark")]
        target: String,
    },
    /// Replace selection.
    #[effect(edit, participant, audience = "agents")]
    ReplaceSelection {
        #[param("Text that replaces the current selection (empty deletes it)")]
        text: String,
        #[param(
            "Where to act (a paragraph, a point or a range in it); your caret or selection when not given",
            place = "blocks"
        )]
        at: Option<Place>,
    },
    /// Comment on selection.
    #[effect(edit, participant)]
    CommentSelection {
        #[param("The comment")]
        text: tilo_resource::NonBlank,
        #[param(
            "Where to act (a paragraph, a point or a range in it); your caret or selection when not given",
            place = "blocks"
        )]
        at: Option<Place>,
    },
    /// Place image.
    /// Places at your caret, at `at`, or after the heading `after` names, the content
    /// another action produced. Answers with the blocks it changed.
    #[effect(edit, participant, job, returns, audience = "agents")]
    PlaceImage {
        #[param(
            content,
            "The image another action produced: its content address (`tilo.content:<hash>`) or hash"
        )]
        content: String,
        #[param(
            "Where to act (a paragraph, a point or a range in it); your caret or selection when not given",
            place = "blocks"
        )]
        at: Option<Place>,
        #[param(
            "The heading (or paragraph) the image goes after, by its text: an exact heading first, else the closest; not with at"
        )]
        after: Option<String>,
    },
    /// Go to.
    #[effect(view, row, participant, idempotent)]
    GoTo {
        #[param("Block id")]
        block: String,
        #[param("Character offset where the range starts")]
        start: usize,
        #[param("Character offset where the range ends")]
        end: usize,
    },
    /// Go to commented text.
    #[effect(view, row, participant, idempotent)]
    GoToAnchor {
        #[param("Block id")]
        block: String,
        #[param("Character offset where the comment's range starts")]
        start: usize,
        #[param("Character offset where the comment's range ends")]
        end: usize,
    },
}

const TOGGLED_MARKS: [&str; 6] = ["strong", "em", "underline", "strike", "super", "sub"];
const FORMATS: [&str; 10] = [
    "strong",
    "em",
    "underline",
    "strike",
    "super",
    "sub",
    "font",
    "size",
    "color",
    "highlight",
];

impl WordProcessorEffect {
    pub(crate) fn view_operation(&self) -> Result<Option<ViewMove>, String> {
        use crate::actions::editing::CaretTarget;
        use crate::actions::view_ops::ViewOperation;
        Ok(Some(match self {
            Self::MoveCaret { to, block, offset } => {
                let to = match to.as_deref().unwrap_or("point") {
                    "point" => match (block, offset) {
                        (Some(block), Some(offset)) if !block.is_empty() => CaretTarget::Point {
                            block: block.clone(),
                            offset: *offset,
                        },
                        _ => return Err("a point needs block and offset".into()),
                    },
                    "document-start" => CaretTarget::DocumentStart,
                    "document-end" => CaretTarget::DocumentEnd,
                    "block-end" => CaretTarget::BlockEnd,
                    _ => return Err("unknown caret target".into()),
                };
                ViewMove::Operation(ViewOperation::MoveCaret { to })
            }
            Self::SelectRange {
                start_block,
                start,
                end_block,
                end,
            } => ViewMove::Operation(ViewOperation::SelectRange {
                range: crate::actions::SelectionTarget {
                    start_block: start_block.clone(),
                    start: *start,
                    end_block: end_block.clone(),
                    end: *end,
                },
            }),
            Self::SelectBlock { block } if !block.is_empty() => {
                ViewMove::Operation(ViewOperation::SelectBlock {
                    block: block.clone(),
                })
            }
            Self::SelectBlock { .. } => return Err("select-block needs a block".into()),
            Self::SelectAll => ViewMove::Operation(ViewOperation::SelectAll),
            Self::FindText { query } => {
                if query.len() > crate::actions::editing::MAX_FIND_QUERY_BYTES {
                    return Err("the Find query is too long".into());
                }
                ViewMove::Operation(ViewOperation::FindText {
                    query: query.clone(),
                })
            }
            Self::GoTo { block, start, end } | Self::GoToAnchor { block, start, end } => {
                ViewMove::NavigateTo {
                    block: block.clone(),
                    start: *start,
                    end: *end,
                }
            }
            _ => return Ok(None),
        }))
    }
}

impl WordProcessorEffect {
    /// The edit this effect makes at `view`'s caret, `None` for the others. Images are
    /// read from `content`; a selection too large to replace in one step is refused.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per edit effect, each validating its own arguments"
    )]
    pub(crate) fn edit(
        &self,
        view: &ParticipantView,
        doc: &automerge::AutoCommit,
        content: &tilo_content::ContentStore,
    ) -> Option<EditResult> {
        let large = || {
            if editing::large_selection(view, doc) {
                Err(EditingError::Invalid(editing::LARGE_REFUSAL.into()))
            } else {
                Ok(())
            }
        };
        let within_limits = |text: &str| {
            if text.len() > crate::actions::MAX_ACTION_TEXT_BYTES {
                Err(EditingError::Invalid("text is too long".into()))
            } else {
                Ok(())
            }
        };
        Some(match self {
            Self::TypeText { text, .. } => large().and_then(|()| within_limits(text)).and_then(|()| {
                if text.is_empty() && view.selection(doc).is_none() {
                    return Err(EditingError::Invalid(
                        "text must be non-empty without a selection".into(),
                    ));
                }
                editing::type_text(view, doc, text)
            }),
            Self::NewParagraph => {
                if view.selection(doc).is_some() {
                    Err(EditingError::Invalid(
                        "collapse the selection first (type-text replaces it)".into(),
                    ))
                } else {
                    editing::new_paragraph(view, doc)
                }
            }
            Self::EraseText { direction, count } => large().and_then(|()| {
                let direction = match direction.as_deref() {
                    None | Some("backward") => DeleteDirection::Backward,
                    Some("forward") => DeleteDirection::Forward,
                    Some(_) => {
                        return Err(EditingError::Invalid(
                            "direction is backward or forward".into(),
                        ));
                    }
                };
                let count = count.unwrap_or(1);
                if count == 0 || count > editing::MAX_ERASE_COUNT {
                    return Err(EditingError::Invalid(format!(
                        "count is 1 to {}",
                        editing::MAX_ERASE_COUNT
                    )));
                }
                editing::delete(view, doc, direction, count)
            }),
            Self::ToggleMark { mark, .. } => match InlineMark::from_name(mark) {
                Some(mark) => editing::toggle_mark(view, doc, mark).and_then(|outcome| {
                    if outcome.0.is_empty() {
                        Err(EditingError::NeedsSelection)
                    } else {
                        Ok(outcome)
                    }
                }),
                None => Err(EditingError::Invalid("unknown mark".into())),
            },
            Self::SetFormat { name, on, value, .. } => {
                editing::set_format(view, doc, name, *on, value.as_deref())
            }
            Self::ClearFormat { name, .. } => editing::clear_format(view, doc, name),
            Self::SetParagraphStyle {
                kind,
                level,
                ordered, ..
            } => {
                // A list as the reader sees it: a bullet or numbered item is that kind of list.
                let (kind, ordered) = match kind.as_str() {
                    "bullet" => ("list_item", Some(false)),
                    "numbered" => ("list_item", Some(true)),
                    other => (other, *ordered),
                };
                if !matches!(kind, "paragraph" | "heading" | "list_item") {
                    Err(EditingError::Invalid(
                        "kind is paragraph, heading or list_item".into(),
                    ))
                } else if level.is_some_and(|level| !(1..=6).contains(&level)) {
                    Err(EditingError::Invalid("level is 1-6".into()))
                } else {
                    editing::set_paragraph_style(
                        view,
                        doc,
                        &ParagraphStyle {
                            kind: kind.to_owned(),
                            level: *level,
                            ordered,
                        },
                    )
                }
            }
            Self::ApplyNamedStyle { style, .. } => {
                let safe = !style.is_empty()
                    && style.len() <= 128
                    && style
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
                if safe {
                    editing::apply_named_style(view, doc, style)
                } else {
                    Err(EditingError::Invalid("style is a style id".into()))
                }
            }
            Self::Paste { text, markdown, .. } => large().and_then(|()| {
                let pasted = match (text, markdown) {
                    (Some(text), None) => PasteContent::Text(text.clone()),
                    (None, Some(markdown)) => PasteContent::Markdown(markdown.clone()),
                    _ => {
                        return Err(EditingError::Invalid(
                            "give exactly one of text or markdown".into(),
                        ));
                    }
                };
                let empty = matches!(&pasted, PasteContent::Text(t) | PasteContent::Markdown(t) if t.is_empty());
                if empty && view.selection(doc).is_none() {
                    return Err(EditingError::Invalid("nothing to paste".into()));
                }
                editing::paste(view, doc, &pasted)
            }),
            Self::Align { align, .. } => editing::align(view, doc, align),
            Self::InsertTableHere { rows, columns, .. } => {
                editing::insert_table_here(view, doc, rows.unwrap_or(2), columns.unwrap_or(2))
            }
            Self::InsertPageBreak { .. } => editing::insert_page_break(view, doc),
            Self::SetHyperlink { target } => editing::set_hyperlink(view, doc, target),
            Self::ReplaceSelection { text, .. } => large()
                .and_then(|()| within_limits(text))
                .and_then(|()| editing::replace_selection(view, doc, text)),
            Self::PlaceImage { content: hash, .. } => editing::place_image(view, doc, content, hash),
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ViewMove {
    Operation(crate::actions::view_ops::ViewOperation),
    NavigateTo {
        block: String,
        start: usize,
        end: usize,
    },
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn specs() -> Vec<tilo_actions::ActionSpec> {
    <WordProcessorEffect as tilo_resource::Effect>::specs()
}

#[cfg(test)]
pub(crate) fn decode(encoded: &tilo_actions::EncodedAction) -> Option<WordProcessorEffect> {
    #[derive(serde::Deserialize)]
    struct Payload {
        effect: String,
        #[serde(default)]
        args: serde_json::Map<String, serde_json::Value>,
    }
    if encoded.type_id != <WordProcessorEffect as tilo_resource::Effect>::OPERATION {
        return None;
    }
    let payload: Payload = serde_json::from_slice(&encoded.payload).ok()?;
    <WordProcessorEffect as tilo_resource::Effect>::decode(&payload.effect, &payload.args).ok()
}
