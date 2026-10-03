//! Per-participant view operations: `move-caret`, `select-range`,
//! `select-block`, `select-all` and `find-text`.
//!
//! They change only the invoking participant's [`ParticipantView`] (caret,
//! selection, Find), never the document, so they are published as
//! [`OperationKind::View`] operations of their own type
//! [`WORD_PROCESSOR_VIEW_OPERATION_TYPE`]. The payload carries the *request*,
//! not a precomputed view: whoever holds the invoker's view (the widget for
//! its own window, the headless worker for an agent) applies it with
//! [`apply`] against the current document, which runs the same
//! [`super::editing`] function the keyboard/mouse path uses.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use tilo_actions::{
    ActionArgs, ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, Resource,
};

use super::editing::{self, CaretTarget, EditResult, EditingError, MAX_FIND_QUERY_BYTES, op};
use super::participant::{ParticipantView, ViewPoint};
use super::{RegionKind, SelectionTarget, WpResourceCtx, continuation};

/// Registry/operation `type_id` of every Writer view operation. Separate from
/// `word-processor.operation` so a host that does not yet keep participant
/// views refuses them instead of misreading them.
pub const WORD_PROCESSOR_VIEW_OPERATION_TYPE: &str = "word-processor.view-operation";

/// One view change requested by a participant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "op")]
pub enum ViewOperation {
    MoveCaret { to: CaretTarget },
    SelectRange { range: SelectionTarget },
    SelectBlock { block: String },
    SelectAll,
    FindText { query: String },
}

/// Decode a view-operation payload; `Ok(None)` for any other type.
///
/// # Errors
///
/// Returns a message when a payload bearing
/// [`WORD_PROCESSOR_VIEW_OPERATION_TYPE`] is not a valid [`ViewOperation`].
pub fn decode_view_operation(
    type_id: &str,
    payload: &[u8],
) -> Result<Option<ViewOperation>, String> {
    if type_id != WORD_PROCESSOR_VIEW_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

/// Apply one view operation to the invoker's view. Pure; returns no
/// envelopes, only the next view.
///
/// # Errors
///
/// Returns an [`EditingError`] when the view has no caret in
/// `doc` or the target is stale or out of range for this document.
pub fn apply(view: &ParticipantView, doc: &AutoCommit, operation: &ViewOperation) -> EditResult {
    match operation {
        ViewOperation::MoveCaret { to } => editing::move_caret(view, doc, to),
        ViewOperation::SelectRange { range } => editing::select_range(view, doc, range),
        ViewOperation::SelectBlock { block } => editing::select_block(view, doc, block),
        ViewOperation::SelectAll => editing::select_all(view, doc),
        ViewOperation::FindText { query } => editing::find(view, doc, query),
    }
}

/// The existing `search-hit#go-to` (`WordProcessorOperation::NavigateTo`)
/// for a participant view: select one exact body range, as the widget's
/// `navigate_to_range` does for its own window.
///
/// # Errors
///
/// Returns an [`EditingError`] when the view has no caret in
/// `doc` or the target is stale or out of range for this document.
pub fn navigate_to(
    view: &ParticipantView,
    doc: &AutoCommit,
    block: &str,
    start: usize,
    end: usize,
) -> EditResult {
    if view.region != RegionKind::Body {
        return Err(EditingError::Invalid(
            "leave header or footer editing before navigating body results".into(),
        ));
    }
    if start > end {
        return Err(EditingError::Invalid(
            "the requested document range is stale".into(),
        ));
    }
    let (_, mut next) = editing::select_range(
        view,
        doc,
        &SelectionTarget {
            start_block: block.to_owned(),
            start,
            end_block: block.to_owned(),
            end,
        },
    )?;
    if start == end {
        next.anchor = None;
        next.caret = ViewPoint::new(block, start);
    }
    Ok((Vec::new(), next))
}

fn encoded(operation: &ViewOperation) -> EncodedAction {
    EncodedAction::encode_operation(WORD_PROCESSOR_VIEW_OPERATION_TYPE, operation).unwrap_or_else(
        |_| EncodedAction {
            type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
            payload: b"view operation could not be encoded".to_vec(),
        },
    )
}

fn invalid(reason: &str) -> EncodedAction {
    EncodedAction {
        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
        payload: reason.as_bytes().to_vec(),
    }
}

fn offset(args: &ActionArgs, name: &str) -> Option<usize> {
    args.integer(name)
        .and_then(|value| usize::try_from(value).ok())
}

fn view_spec(
    id: &str,
    label: &str,
    description: &str,
    params: Vec<ActionParam>,
    build: impl Fn(&ActionArgs) -> EncodedAction + Send + Sync + 'static,
) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        label,
        "",
        OperationKind::View,
        WORD_PROCESSOR_VIEW_OPERATION_TYPE,
        params,
        build,
    )
    .with_description(description)
    .participant_view()
}

/// Publish the view operations on `root` (`word-processor.document`). They
/// are the same for every participant and tagged
/// [`participant_view`](ActionSpec::participant_view): each changes only the
/// invoker's own view, so a remote participant may run them; the evidence domain is the
/// participant's current view because `move-caret {to: block-end}` resolves
/// against it.
#[expect(
    clippy::too_many_lines,
    reason = "the published view-operation table: one independent spec per operation"
)]
pub(super) fn add_operations(root: &mut Resource, ctx: &WpResourceCtx) {
    let caret = ViewPoint::new(&ctx.caret_block, ctx.caret_offset);
    let selection = ctx.selection.as_ref().map(|selection| {
        (
            ViewPoint::new(&selection.start_block, selection.start),
            ViewPoint::new(&selection.end_block, selection.end),
        )
    });
    let domain = continuation::view(
        ctx.region,
        &caret,
        selection.as_ref().map(|(start, end)| (start, end)),
    );
    let text = |name: &'static str, description: &'static str| {
        ActionParam::required_owned(name, ParamType::Text, description)
    };
    let index = |name: &'static str, description: &'static str| {
        ActionParam::required_owned(name, ParamType::Integer, description)
            .with_schema(serde_json::json!({"minimum": 0}))
    };
    let specs = [
        (
            view_spec(
                op::MOVE_CARET,
                "Move caret",
                "Move your caret (collapsing your selection) to a point, or the document start, document end or end of the current paragraph",
                vec![
                    ActionParam::optional_owned(
                        "to",
                        ParamType::Text,
                        "point (default; needs block and offset), document-start, document-end or block-end",
                    )
                    .with_choices(["point", "document-start", "document-end", "block-end"]),
                    ActionParam::optional_owned("block", ParamType::Text, "Block id for a point"),
                    ActionParam::optional_owned("offset", ParamType::Integer, "Character offset for a point")
                        .with_schema(serde_json::json!({"minimum": 0})),
                ],
                |args| {
                    let to = match args.text("to").unwrap_or("point") {
                        "point" => match (args.text("block"), offset(args, "offset")) {
                            (Some(block), Some(offset)) if !block.is_empty() => {
                                CaretTarget::Point {
                                    block: block.to_owned(),
                                    offset,
                                }
                            }
                            _ => return invalid("a point needs block and offset"),
                        },
                        "document-start" => CaretTarget::DocumentStart,
                        "document-end" => CaretTarget::DocumentEnd,
                        "block-end" => CaretTarget::BlockEnd,
                        _ => return invalid("unknown caret target"),
                    };
                    encoded(&ViewOperation::MoveCaret { to })
                },
            ),
            "move-caret.v1",
        ),
        (
            view_spec(
                op::SELECT_RANGE,
                "Select range",
                "Select from (start_block, start) to (end_block, end); your caret ends at the end",
                vec![
                    text("start_block", "Block where the selection starts"),
                    index("start", "Character offset in start_block"),
                    text("end_block", "Block where the selection ends"),
                    index("end", "Character offset in end_block"),
                ],
                |args| {
                    let (Some(start_block), Some(start), Some(end_block), Some(end)) = (
                        args.text("start_block"),
                        offset(args, "start"),
                        args.text("end_block"),
                        offset(args, "end"),
                    ) else {
                        return invalid("select-range needs start_block, start, end_block, end");
                    };
                    encoded(&ViewOperation::SelectRange {
                        range: SelectionTarget {
                            start_block: start_block.to_owned(),
                            start,
                            end_block: end_block.to_owned(),
                            end,
                        },
                    })
                },
            ),
            "select-range.v1",
        ),
        (
            view_spec(
                op::SELECT_BLOCK,
                "Select paragraph",
                "Select one whole paragraph (block)",
                vec![text("block", "Block id to select")],
                |args| match args.text("block") {
                    Some(block) if !block.is_empty() => encoded(&ViewOperation::SelectBlock {
                        block: block.to_owned(),
                    }),
                    _ => invalid("select-block needs a block"),
                },
            ),
            "select-block.v1",
        ),
        (
            view_spec(
                op::SELECT_ALL,
                "Select all",
                "Select the whole document (or header/footer you are editing)",
                Vec::new(),
                |_| encoded(&ViewOperation::SelectAll),
            ),
            "select-all.v1",
        ),
        (
            view_spec(
                op::FIND,
                "Find text",
                "Find text (case-insensitive) and select the first match; results appear as search-hit resources",
                vec![
                    text("query", "Text to find (empty clears Find)")
                        .with_schema(serde_json::json!({"maxLength": MAX_FIND_QUERY_BYTES})),
                ],
                |args| {
                    let query = args.text("query").unwrap_or_default();
                    if query.len() > MAX_FIND_QUERY_BYTES {
                        return invalid("find query is too long");
                    }
                    encoded(&ViewOperation::FindText {
                        query: query.to_owned(),
                    })
                },
            ),
            "find-text.v1",
        ),
    ];
    for (spec, handler) in specs {
        root.actions
            .push(continuation::pure(spec, handler, domain.clone()));
    }
}
