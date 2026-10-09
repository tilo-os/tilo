//! What the word processor does that is not an edit of its document: reading
//! and writing files, what the window shows, and where each participant's
//! caret and selection are. Every one is published on the document, so a
//! button, the palette, an agent and a script reach the same handler (the
//! editor's `ResourceSurface::effect`).

use ui::tilo_resource::{self, Effect};

use crate::actions::editing::{
    self, DeleteDirection, EditResult, EditingError, InlineMark, ParagraphStyle, PasteContent,
};
use crate::actions::participant::ParticipantView;

/// The word processor's effect vocabulary. Block ids are the document's
/// stable paragraph ids; offsets count characters.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "word-processor.effects")]
pub enum WordProcessorEffect {
    /// Share image
    ///
    /// The picture at the caret, else the document's only picture, as
    /// content another app's operation takes (a photo editor opening it).
    #[effect(content, permission = "read")]
    ShareImage,
    /// Open document.
    ///
    /// Replaces the document with a file's content; Undo takes it back.
    #[effect(job, destructive, device_only)]
    Open {
        #[param("Direct filesystem path; omit to choose one")]
        path: Option<String>,
    },
    /// Save document.
    ///
    /// Writes the document to its file, or to the one given.
    #[effect(job, destructive, device_only)]
    Save {
        #[param("Direct filesystem path; omit to save to the document's file")]
        path: Option<String>,
    },
    /// Save document as.
    ///
    /// Writes the document to a file, in the format its name says.
    #[effect(job, destructive, device_only)]
    SaveAs {
        #[param("Direct filesystem path; omit to choose one")]
        path: Option<String>,
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
    ///
    /// Moves your caret (collapsing your selection) to a point, or the
    /// document start, document end or end of the current paragraph.
    #[effect(view, participant, idempotent)]
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
    ///
    /// Selects from (`start_block`, `start`) to (`end_block`, `end`); your caret
    /// ends at the end.
    #[effect(view, participant, idempotent)]
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
    ///
    /// Selects one whole paragraph (block).
    #[effect(view, participant, idempotent)]
    SelectBlock {
        #[param("Block id to select")]
        block: String,
    },
    /// Select all.
    ///
    /// Selects the whole document (or the header or footer you are editing).
    #[effect(view, participant, idempotent)]
    SelectAll,
    /// Find text.
    ///
    /// Finds text (case-insensitive) and selects the first match; the
    /// results appear as search-hit resources.
    #[effect(view, participant, idempotent)]
    FindText {
        #[param("Text to find (empty clears Find)")]
        query: String,
    },
    /// Type text.
    ///
    /// Types at your caret like the keyboard: it replaces your selection,
    /// and your caret moves past it. Repeat to keep typing.
    #[effect(edit, participant)]
    TypeText {
        #[param(
            "Text typed at the caret, replacing the selection (empty only erases a selection)"
        )]
        text: String,
    },
    /// New paragraph.
    ///
    /// Presses Enter at your caret: splits the paragraph (an empty list item
    /// becomes a paragraph).
    #[effect(edit, participant)]
    NewParagraph,
    /// Erase text.
    ///
    /// Backspace or Delete at your caret, or erases your selection.
    #[effect(edit, participant)]
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
    ///
    /// Flips bold, italic, underline or strike on your selection, off where
    /// it covers all of it (super and sub also the caret paragraph).
    #[effect(edit, participant)]
    ToggleMark {
        #[param("The inline mark to toggle", choices = TOGGLED_MARKS)]
        mark: String,
    },
    /// Set text format.
    ///
    /// Sets formatting on your selection whatever it has now: bold, italic,
    /// underline or strike on or off, or font, size, colour or highlight.
    #[effect(edit, participant)]
    SetFormat {
        #[param("The inline mark", choices = FORMATS)]
        name: String,
        #[param("strong, em, underline, strike, super, sub: true sets it, false removes it")]
        on: Option<bool>,
        #[param("font: sans|serif|mono; size: points 6-200; color/highlight: #rrggbb")]
        value: Option<String>,
    },
    /// Clear text format.
    ///
    /// Removes one inline format from your selection, or the caret paragraph.
    #[effect(edit, participant)]
    ClearFormat {
        #[param("The inline mark to remove", choices = FORMATS)]
        name: String,
    },
    /// Set paragraph style.
    ///
    /// Makes your selected paragraphs (or the caret paragraph) a heading, a
    /// list or a body paragraph.
    #[effect(edit, participant)]
    SetParagraphStyle {
        #[param("Paragraph kind", choices = ["paragraph", "heading", "list_item"])]
        kind: String,
        #[param("Heading level 1-6")]
        level: Option<i64>,
        #[param("Numbered (true) or bulleted (false) list")]
        ordered: Option<bool>,
    },
    /// Apply style.
    ///
    /// Applies a named style (body, heading1-3, quote, list, or a custom
    /// style) to your selected paragraphs, or the caret paragraph.
    #[effect(edit, participant)]
    ApplyNamedStyle {
        #[param("Named style id")]
        style: String,
    },
    /// Paste.
    ///
    /// Pastes text or Markdown (headings, lists, pipe tables) at your caret,
    /// replacing your selection.
    #[effect(edit, participant)]
    Paste {
        #[param("Plain text; each line becomes a paragraph")]
        text: Option<String>,
        #[param("Markdown pasted with its formatting")]
        markdown: Option<String>,
    },
    /// Align paragraph.
    #[effect(edit, participant)]
    Align {
        #[param("How the caret paragraph lines up", choices = ["left", "center", "right", "justify"])]
        align: String,
    },
    /// Insert table here.
    ///
    /// An empty table after the caret paragraph.
    #[effect(edit, participant)]
    InsertTableHere {
        #[param("Rows (default 2)")]
        rows: Option<usize>,
        #[param("Columns (default 2)")]
        columns: Option<usize>,
    },
    /// Insert page break.
    #[effect(edit, participant)]
    InsertPageBreak,
    /// Add or change hyperlink.
    ///
    /// Links your selected text.
    #[effect(edit, participant)]
    SetHyperlink {
        #[param("A web address (https://…), a mail address (mailto:…) or a #bookmark")]
        target: String,
    },
    /// Replace selection.
    ///
    /// Replaces your selection with the text (empty deletes it).
    #[effect(edit, participant)]
    ReplaceSelection {
        #[param("Text that replaces the current selection (empty deletes it)")]
        text: String,
    },
    /// Comment on selection.
    ///
    /// Adds a comment about your selected text.
    #[effect(edit, participant)]
    CommentSelection {
        #[param("The comment")]
        text: String,
    },
    /// Place image.
    ///
    /// Places an image another action produced at your caret.
    #[effect(edit, participant)]
    PlaceImage {
        #[param("The image's content hash")]
        content: String,
    },
    /// Go to.
    ///
    /// Selects one exact range: a search result, a table cell.
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

/// The marks a toggle flips.
const TOGGLED_MARKS: [&str; 6] = ["strong", "em", "underline", "strike", "super", "sub"];
/// Every inline format by name: the toggled marks and the valued ones.
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
    /// The view operation this effect is, for the effects that move a
    /// participant's caret, selection or Find.
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
    /// The edit this effect makes at `view`'s caret in `doc`, for the
    /// effects that edit the document (`None` for the others). Images are
    /// read from `content`. Built by the same primitives the keyboard uses;
    /// a selection too large to replace in one step is refused.
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
            if text.chars().count() > crate::actions::MAX_ACTION_TEXT_CHARS {
                Err(EditingError::Invalid("text is too long".into()))
            } else {
                Ok(())
            }
        };
        Some(match self {
            Self::TypeText { text } => large().and_then(|()| within_limits(text)).and_then(|()| {
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
            Self::ToggleMark { mark } => match InlineMark::from_name(mark) {
                Some(mark) => editing::toggle_mark(view, doc, mark).and_then(|outcome| {
                    if outcome.0.is_empty() {
                        Err(EditingError::NeedsSelection)
                    } else {
                        Ok(outcome)
                    }
                }),
                None => Err(EditingError::Invalid("unknown mark".into())),
            },
            Self::SetFormat { name, on, value } => {
                editing::set_format(view, doc, name, *on, value.as_deref())
            }
            Self::ClearFormat { name } => editing::clear_format(view, doc, name),
            Self::SetParagraphStyle {
                kind,
                level,
                ordered,
            } => {
                if !matches!(kind.as_str(), "paragraph" | "heading" | "list_item") {
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
                            kind: kind.clone(),
                            level: *level,
                            ordered: *ordered,
                        },
                    )
                }
            }
            Self::ApplyNamedStyle { style } => {
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
            Self::Paste { text, markdown } => large().and_then(|()| {
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
            Self::Align { align } => editing::align(view, doc, align),
            Self::InsertTableHere { rows, columns } => {
                editing::insert_table_here(view, doc, rows.unwrap_or(2), columns.unwrap_or(2))
            }
            Self::InsertPageBreak => editing::insert_page_break(view, doc),
            Self::SetHyperlink { target } => editing::set_hyperlink(view, doc, target),
            Self::ReplaceSelection { text } => large()
                .and_then(|()| within_limits(text))
                .and_then(|()| editing::replace_selection(view, doc, text)),
            Self::PlaceImage { content: hash } => editing::place_image(view, doc, content, hash),
            _ => return None,
        })
    }
}

/// Where a view effect moves its invoker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ViewMove {
    Operation(crate::actions::view_ops::ViewOperation),
    NavigateTo {
        block: String,
        start: usize,
        end: usize,
    },
}

/// The effects as the document publishes them, by id, for a peer that
/// serves a Writer document the way its framework app does.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn specs() -> Vec<tilo_actions::ActionSpec> {
    <WordProcessorEffect as tilo_resource::Effect>::specs()
}

/// The effect an invocation `encoded` names, as its handler receives it.
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
