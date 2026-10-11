//! What the word processor does that is not an edit of its document: reading
//! and writing files, what the window shows, and where each participant's
//! caret and selection are. Every one is published on the document, so a
//! button, the palette, an agent and a script reach the same handler (the
//! editor's `ResourceSurface::effect`).

use ui::tilo_resource::{self, Effect};

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
    /// Export document as content.
    ///
    /// Keeps the document in a format, its pictures inside it, in the content
    /// store, with no file to write: the caller reads the bytes by the
    /// content address it answers.
    #[effect(content, permission = "read")]
    ExportContent {
        #[param(
            "The format to export",
            choices = ["docx", "odt", "md", "html", "rtf", "txt"]
        )]
        format: String,
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
