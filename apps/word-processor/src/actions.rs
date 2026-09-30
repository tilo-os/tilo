//! The word processor's exported **actions** and the **resource tree** it
//! publishes to Ctrl+K.
//!
//! Every document-mutating operation is a [`RichTextAction`] from qdoc's
//! rich-text vocabulary (re-exported here with its builders). The same
//! [`EncodedAction`] is what toolbar/panel/menu affordances dispatch and what
//! Ctrl+K dispatches — one source of truth, one replay path. This module adds
//! what is the word processor's own: its application operations (open, save,
//! zoom, panels) and the resource tree it publishes.

pub(crate) mod continuation;
/// Pure editor input primitives shared by keyboard, palette and agents.
pub mod editing;
/// Per-participant view state and its pure resource-context projection.
pub mod participant;
/// Per-participant view operations (caret, selection, Find) and their
/// published `word-processor.view-operation` specs.
pub mod view_ops;

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::num;

use tilo_actions::{
    Action, ActionArgs, ActionContract, ActionParam, ActionRegistry, ActionSpec,
    ContextRequirement, EncodedAction, ExecutionLocus, ExecutionMode, OperationKind, ParamType,
    Resource, ResultSpec,
};

/// Registry `type_id` for every word-processor action.
pub use tilo_document::RICH_TEXT_ACTION_TYPE as WORD_PROCESSOR_ACTION_TYPE;
pub use tilo_document::action::*;
pub const WORD_PROCESSOR_OPERATION_TYPE: &str = "word-processor.operation";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WordProcessorOperation {
    ResolveFileChoice {
        generation: u64,
        use_selected: bool,
    },
    Open {
        path: Option<String>,
    },
    Save {
        path: Option<String>,
    },
    SaveAs {
        path: Option<String>,
    },
    Find,
    ZoomIn,
    ZoomOut,
    /// Return the page canvas to its explicit 100% scale. This matches the
    /// percentage chip in the visible zoom stepper and Ctrl+0.
    ZoomReset,
    ToggleSpellcheck,
    ToggleMarkup,
    ToggleStyles,
    ToggleRuler,
    ToggleOutline,
    ToggleHistory,
    /// Focus one exact body range. Used by bounded search-hit/comment resources
    /// instead of an index into the renderer's transient cache.
    NavigateTo {
        block: String,
        start: usize,
        end: usize,
    },
    /// Open history focused on a stable change-graph position.
    ShowRevision {
        index: usize,
    },
}

/// Decode an application-owned word-processor operation payload.
///
/// # Errors
///
/// Returns a message when a payload bearing
/// [`WORD_PROCESSOR_OPERATION_TYPE`] is not valid JSON for
/// [`WordProcessorOperation`].
pub fn decode_operation(
    type_id: &str,
    payload: &[u8],
) -> Result<Option<WordProcessorOperation>, String> {
    if type_id != WORD_PROCESSOR_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn operation(
    id: &str,
    label: &str,
    kind: OperationKind,
    value: &WordProcessorOperation,
) -> Option<ActionSpec> {
    let Ok(spec) = ActionSpec::operation(id, label, kind, WORD_PROCESSOR_OPERATION_TYPE, value)
    else {
        // A malformed operation must not become an invokable catalogue
        // entry. These variants contain only JSON-native values, but keep the
        // fallible serializer boundary fail-closed rather than panicking.
        return None;
    };
    match value {
        WordProcessorOperation::ResolveFileChoice { use_selected, .. } => Some(
            spec.with_contract(if *use_selected {
                ActionContract::default()
            } else {
                ActionContract::view()
            })
            .execution(ExecutionMode::Background { cancellable: false })
            .returns(ResultSpec::Job),
        ),
        WordProcessorOperation::Open { .. }
        | WordProcessorOperation::Save { .. }
        | WordProcessorOperation::SaveAs { .. } => Some(
            spec.destructive()
                .execution(ExecutionMode::Background { cancellable: false })
                .returns(ResultSpec::Job),
        ),
        _ => Some(spec.with_contract(ActionContract::view())),
    }
}

/// Register the word processor's action decoder for palette execution + remote
/// replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<RichTextAction>(WORD_PROCESSOR_ACTION_TYPE);
}

/// Encode any [`RichTextAction`] into the wire form the palette and the
/// on-screen affordances both dispatch. The single point that turns an action
/// into a replayable envelope.
pub fn encode(action: &RichTextAction) -> EncodedAction {
    // The action is a plain serde enum over owned data, so the JSON encode
    // cannot fail. Were it ever to, an empty payload fails decoding at the far
    // end as an explicit action error instead of aborting the process.
    EncodedAction::encode(WORD_PROCESSOR_ACTION_TYPE, action).unwrap_or_else(|_| EncodedAction {
        type_id: WORD_PROCESSOR_ACTION_TYPE.to_string(),
        payload: Vec::new(),
    })
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// The presentation context Ctrl+K needs to build the document's resource tree:
/// the caret block/offset the structural inserts anchor on, and the block ids
/// the block-level formatting targets (the selection, or the caret block).
/// Everything here derives from state the app owns; the resource children
/// (images / tables / sections) are enumerated from the shared document.
#[derive(Debug, Clone)]
pub struct WpResourceCtx {
    pub caret_block: String,
    pub caret_offset: usize,
    /// The blocks a block-level action targets — the selection, or the caret
    /// block when there is no selection.
    pub target_blocks: Vec<String>,
    /// Exact non-empty character spans selected at palette invocation time.
    /// This keeps a prompted hyperlink action resource-addressed and replayable
    /// instead of consulting mutable presentation state after the user enters
    /// a target.
    pub format_spans: Vec<Span>,
    /// The selection in document order when it is non-empty.  This semantic
    /// context, rather than a caller-provided block id, binds the palette's
    /// Replace selection operation to exactly what the user has selected.
    pub selection: Option<SelectionTarget>,
    pub region: RegionKind,
    /// Current Find results, capped before they enter the global resource tree.
    /// Each carries CRDT block identity + exact character range, never a
    /// renderer index that would drift under a collaborator's edit.
    pub search_hits: Vec<SearchTarget>,
    /// Where content another action produced (an image to place) is read from.
    pub content: tilo_content::ContentStore,
}

/// A bounded, precisely addressable Find result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchTarget {
    pub block: String,
    pub start: usize,
    pub end: usize,
}

/// Resource enumeration is intentionally bounded: a giant document can still
/// search completely, but Ctrl+K/global search gets the first useful page and
/// never allocates one resource object per occurrence forever.
pub const MAX_RESOURCE_SEARCH_HITS: usize = 64;
pub const MAX_RESOURCE_REVISIONS: usize = 64;
/// Sections and images listed around the caret (and sixteen times as many
/// table cells, so the nearest tables are whole).
pub const MAX_RESOURCE_OBJECTS: usize = 64;

fn immediate_spec(id: &str, label: &str, action: &RichTextAction) -> ActionSpec {
    // Built from `encode`, which is total, rather than the fallible
    // `ActionSpec::immediate` — the two produce the same spec.
    //
    // Through the constructor rather than a struct literal: `ActionSpec`
    // carries private `params`/`build` fields, so a literal cannot name
    // them and the shared defaults would be bypassed if it could.
    let spec =
        ActionSpec::immediate_encoded(id, label, action.locus(), encode(action)).reversible();
    if id.contains("delete") || id.contains("remove") {
        spec.destructive()
    } else {
        spec
    }
}

/// A short label for a structural block in the palette.
fn short(text: &str, fallback: &str) -> String {
    let t: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let t = t.trim();
    if t.is_empty() {
        fallback.to_string()
    } else if t.chars().count() > 48 {
        t.chars().take(45).collect::<String>() + "…"
    } else {
        t.to_string()
    }
}

/// Human label for a stable document range without retaining a separate search
/// text cache. The caller materializes once, then each bounded result borrows
/// from that snapshot.
fn range_label(
    blocks: &[tilo_doc::snapshot::BlockView],
    block: &str,
    start: usize,
    end: usize,
) -> Option<String> {
    let text: String = blocks
        .iter()
        .find(|entry| entry.id == block)?
        .text
        .chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect();
    Some(format!("Result: {}", short(&text, "Empty match")))
}

fn document_resource(doc: &AutoCommit) -> Resource {
    let state =
        <tilo_document::RichText as ui::tilo_resource::Resource>::read(doc).unwrap_or_else(|_| {
            let mut empty = AutoCommit::new();
            <tilo_document::RichText as ui::tilo_resource::Resource>::initialize(&mut empty)
                .expect("empty rich text initializes");
            <tilo_document::RichText as ui::tilo_resource::Resource>::read(&empty)
                .expect("empty rich text reads")
        });
    let mut root = ui::tilo_resource::publish::<tilo_document::RichText>(
        "word-processor.document",
        "Document",
        &state,
    );
    if !root
        .actions
        .iter()
        .any(|action| action.id.as_str() == "read-text")
    {
        root.actions.push(crate::semantic_read::action::spec());
    }
    root
}

fn add_file_operations(root: &mut Resource, file: Option<&continuation::FileContext>) {
    for (id, label, variant) in [
        ("open", "Open document", 0_u8),
        ("save", "Save document", 1_u8),
        ("save-as", "Save document as", 2_u8),
    ] {
        root.actions.push(continuation::file(
            ActionSpec::typed_operation(
                id,
                label,
                "path (omit to use the system file picker)",
                OperationKind::Effect,
                WORD_PROCESSOR_OPERATION_TYPE,
                vec![ActionParam::optional_owned(
                    "path",
                    ParamType::Text,
                    "Direct filesystem path; omit to open the system file picker",
                )],
                move |args: &ActionArgs| {
                    let path = args
                        .text("path")
                        .filter(|path| !path.trim().is_empty())
                        .map(str::to_string);
                    let operation = match variant {
                        0 => WordProcessorOperation::Open { path },
                        1 => WordProcessorOperation::Save { path },
                        _ => WordProcessorOperation::SaveAs { path },
                    };
                    EncodedAction::encode_operation(WORD_PROCESSOR_OPERATION_TYPE, &operation)
                        .unwrap_or_else(|_| EncodedAction {
                            type_id: WORD_PROCESSOR_OPERATION_TYPE.into(),
                            payload: Vec::new(),
                        })
                },
            )
            .destructive()
            .execution(ExecutionMode::Background { cancellable: false })
            .returns(ResultSpec::Job),
            variant,
            file,
        ));
    }
}

pub(crate) fn view_operations(include_history: bool) -> Vec<ActionSpec> {
    let mut specs = Vec::new();
    for (id, label, value) in [
        ("find", "Find in document", WordProcessorOperation::Find),
        ("zoom-in", "Zoom in", WordProcessorOperation::ZoomIn),
        ("zoom-out", "Zoom out", WordProcessorOperation::ZoomOut),
        (
            "zoom-reset",
            "Reset zoom to 100%",
            WordProcessorOperation::ZoomReset,
        ),
        (
            "spellcheck",
            "Toggle spellcheck",
            WordProcessorOperation::ToggleSpellcheck,
        ),
        (
            "markup",
            "Toggle review markup",
            WordProcessorOperation::ToggleMarkup,
        ),
        (
            "styles",
            "Toggle styles panel",
            WordProcessorOperation::ToggleStyles,
        ),
        ("ruler", "Toggle ruler", WordProcessorOperation::ToggleRuler),
        (
            "outline",
            "Toggle outline",
            WordProcessorOperation::ToggleOutline,
        ),
        (
            "history",
            "Toggle history",
            WordProcessorOperation::ToggleHistory,
        ),
    ] {
        if !include_history && id == "history" {
            continue;
        }
        if let Some(spec) = operation(id, label, OperationKind::View, &value) {
            specs.push(spec);
        }
    }
    specs
}

fn add_view_operations(root: &mut Resource) {
    root.actions.extend(view_operations(true));
}

fn add_caret_actions(root: &mut Resource, ctx: &WpResourceCtx) {
    root.actions.push(
        immediate_spec(
            "insert-table",
            "Insert table",
            &insert_table_action(&ctx.caret_block, 2, 2),
        )
        .requires(ContextRequirement::Caret),
    );
    if ctx.format_spans.is_empty() {
        return;
    }
    let spans = ctx.format_spans.clone();
    let region = ctx.region;
    root.actions.push(continuation::pure(
        ActionSpec::typed(
            "set-hyperlink",
            "Add or change hyperlink",
            "Web URL (https://… or mailto:…) or #bookmark",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required_owned(
                "target",
                ParamType::Text,
                "A safe external URL, mail address, or document-local #bookmark target",
            )],
            move |args: &ActionArgs| {
                let target = args.text("target").unwrap_or_default().trim();
                if !valid_hyperlink_target(target) {
                    return EncodedAction {
                        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                        payload: b"hyperlink target must use https:, mailto:, or #bookmark"
                            .to_vec(),
                    };
                }
                encode(&set_hyperlink_action(region, spans.clone(), target))
            },
        )
        .requires(ContextRequirement::Selection),
        "set-hyperlink.v1",
        continuation::spans(region, &ctx.format_spans),
    ));
}

/// `large`: the selection is over [`editing::LARGE_REMOVAL_CHARS`], and the
/// replacement refuses like the editing primitives do.
fn add_selection_resources(root: &mut Resource, ctx: &WpResourceCtx, large: bool) {
    let Some(selection) = &ctx.selection else {
        return;
    };
    let selection = selection.clone();
    let root_selection = selection.clone();
    let region = ctx.region;
    let domain = continuation::selection(region, &selection);
    root.actions.push(
        continuation::pure(
            ActionSpec::typed(
                "replace-selection",
                "Replace selection",
                "Replacement text for the selected document range",
                ExecutionLocus::ClientLocal,
                vec![ActionParam::required_owned(
                    "text",
                    ParamType::Text,
                    "Text that replaces the current selection (empty deletes it)",
                )],
                move |args: &ActionArgs| {
                    if large {
                        return EncodedAction {
                            type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                            payload: editing::LARGE_REFUSAL.as_bytes().to_vec(),
                        };
                    }
                    let Some(text) = args.text("text") else {
                        return EncodedAction {
                            type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                            payload: b"replacement text is required".to_vec(),
                        };
                    };
                    encode(&replace_selection_action(
                        region,
                        &root_selection.start_block,
                        root_selection.start,
                        &root_selection.end_block,
                        root_selection.end,
                        text,
                    ))
                },
            ),
            "replace-selection.v1",
            domain.clone(),
        )
        .requires(ContextRequirement::Selection),
    );
    root.children.push(
        Resource::new(
            format!(
                "word-processor.selection:{}:{}:{}:{}",
                selection.start_block, selection.start, selection.end_block, selection.end
            ),
            "word_processor.selection",
            "Current selection",
        )
        .with_action(
            continuation::pure(
                ActionSpec::typed(
                    "replace",
                    "Replace selection",
                    "Replacement text for this exact selection",
                    ExecutionLocus::ClientLocal,
                    vec![ActionParam::required_owned(
                        "text",
                        ParamType::Text,
                        "Replacement text",
                    )],
                    move |args: &ActionArgs| {
                        if large {
                            return EncodedAction {
                                type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                                payload: editing::LARGE_REFUSAL.as_bytes().to_vec(),
                            };
                        }
                        let Some(text) = args.text("text") else {
                            return EncodedAction {
                                type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                                payload: b"replacement text is required".to_vec(),
                            };
                        };
                        encode(&replace_selection_action(
                            region,
                            &selection.start_block,
                            selection.start,
                            &selection.end_block,
                            selection.end,
                            text,
                        ))
                    },
                ),
                "replace-selection.v1",
                domain,
            )
            .requires(ContextRequirement::Selection),
        ),
    );
}

/// Contract tag prefix declaring that a Text parameter may be delivered in
/// order across several invocations: `tilo.incremental-text:<param>:<next>`
/// says the first part goes through this action and every later part through
/// action `<next>` on the same resource, with `<param>` set to that part, and
/// that the parts applied in order equal one invocation with the whole text.
/// Any caller (a person's tool, a script, an agent) may rely on it; the
/// declaration is the app's, not the caller's.
pub const INCREMENTAL_TEXT_TAG: &str = "tilo.incremental-text:";

/// Contract tag marking an operation kept only for compatibility:
/// `tilo.deprecated:<replacement>` names the operation to use instead.
/// Callers choosing an operation (an agent planner) should skip it.
pub const DEPRECATED_TAG: &str = "tilo.deprecated:";

/// Root-level text writes that need no caret or selection, for anyone
/// writing into the document (a person from the palette, a script, an
/// agent). Their payload names no position; the end of the body is read at
/// apply time. `append-paragraph` is an ordinary document operation that,
/// like typing, leaves the invoker's caret at the end of the text it wrote,
/// so it declares [`INCREMENTAL_TEXT_TAG`] continued by `type-text`: the rest
/// of a long paragraph is typed at that caret. `continue-paragraph` is
/// deprecated in favour of `type-text` at a caret (`move-caret {to:
/// document-end}` first).
fn add_body_append_actions(
    root: &mut Resource,
    ctx: &WpResourceCtx,
    doc: &AutoCommit,
    body: Option<&tilo_document::BodyOrder>,
) {
    let appended =
        editing::AppendTarget::new(&participant::ParticipantView::from_ctx(ctx), doc, body);
    for (id, label, description, handler, append) in [
        (
            "append-paragraph",
            "Append paragraph",
            "Add a new paragraph containing the text at the end of the document",
            "append-paragraph.v1",
            true,
        ),
        (
            "continue-paragraph",
            "Continue last paragraph",
            "Deprecated: move-caret to document-end, then type-text. Appends the text to the end of the document's last paragraph",
            "continue-paragraph.v1",
            false,
        ),
    ] {
        let appended = appended.clone();
        let spec = ActionSpec::typed(
            id,
            label,
            "Text to write",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required_owned(
                "text",
                ParamType::Text,
                "The text to write (not empty)",
            )],
            move |args: &ActionArgs| {
                let text = args.text("text").unwrap_or_default();
                if text.is_empty() || text.chars().count() > MAX_ACTION_TEXT_CHARS {
                    return EncodedAction {
                        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                        payload: b"text must be non-empty and within Writer's limits".to_vec(),
                    };
                }
                if append {
                    crate::widget::note_participant_outcome(&appended.caret_after(text));
                    encode(&append_paragraph_action(text))
                } else {
                    encode(&continue_paragraph_action(text))
                }
            },
        )
        .with_description(description)
        .reversible();
        let mut spec = spec;
        spec.contract.tags.push(if append {
            format!("{INCREMENTAL_TEXT_TAG}text:{}", editing::op::TYPE_TEXT)
        } else {
            format!("{DEPRECATED_TAG}{}", editing::op::TYPE_TEXT)
        });
        root.actions.push(continuation::pure(
            spec,
            handler,
            vec![b"body-end".to_vec()],
        ));
    }
}

fn add_comment_resource_action(root: &mut Resource, ctx: &WpResourceCtx) {
    let Some(selection) = &ctx.selection else {
        return;
    };
    if ctx.region != RegionKind::Body
        || selection.start_block != selection.end_block
        || selection.start >= selection.end
    {
        return;
    }
    let target = selection.clone();
    let domain = continuation::selection(RegionKind::Body, &target);
    root.actions.push(continuation::pure(
        ActionSpec::typed(
            "add-comment",
            "Add comment",
            "Comment text and author for the selected text",
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required_owned("author", ParamType::Text, "Comment author"),
                ActionParam::required_owned("text", ParamType::Text, "Comment text"),
                ActionParam::optional_owned("date", ParamType::Text, "ISO-8601 comment date"),
            ],
            move |args: &ActionArgs| {
                let author = args.text("author").unwrap_or_default().trim();
                let text = args.text("text").unwrap_or_default();
                if author.is_empty() || text.chars().count() > MAX_ACTION_TEXT_CHARS {
                    return EncodedAction {
                        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                        payload: b"comment author is required and comment text is too long"
                            .to_vec(),
                    };
                }
                encode(&add_comment_action(
                    &target.start_block,
                    target.start,
                    target.end,
                    author,
                    args.text("date").unwrap_or_default(),
                    text,
                ))
            },
        )
        .requires(ContextRequirement::Selection),
        "add-comment.v1",
        domain,
    ));
}

/// Selection, comment, and find-hit rows that depend on this peer's caret.
/// Not a second `word-processor.document` root.
pub(crate) fn live_presentation_resources(ctx: &WpResourceCtx, doc: &AutoCommit) -> Vec<Resource> {
    let mut scratch = Resource::new("word-processor.live", "word_processor.live", "Live");
    add_selection_resources(&mut scratch, ctx, editing::large_selection(ctx, doc, None));
    add_comment_resource_action(&mut scratch, ctx);
    if !ctx.search_hits.is_empty()
        && let Ok(containers) = tilo_doc::model::open(doc, &automerge::ROOT)
    {
        let snapshot = tilo_doc::model::materialize(doc, &containers);
        add_search_resources(&mut scratch, ctx, &snapshot);
    }
    let mut extras = scratch.children;
    if let Some(selection) = extras
        .iter_mut()
        .find(|resource| resource.kind == "word_processor.selection")
    {
        selection.actions.extend(scratch.actions);
    } else if !scratch.actions.is_empty() {
        let mut selection = Resource::new(
            "word-processor.selection",
            "word_processor.selection",
            "Current selection",
        );
        selection.actions.extend(scratch.actions);
        extras.insert(0, selection);
    }
    extras
}

fn add_search_resources(
    root: &mut Resource,
    ctx: &WpResourceCtx,
    snapshot: &tilo_doc::snapshot::DocSnapshot,
) {
    if ctx.search_hits.is_empty() {
        return;
    }
    let mut search = Resource::new(
        "word-processor.search-results",
        "word_processor.search-results",
        "Search results",
    );
    let mut seen = HashSet::new();
    for (result_index, hit) in ctx
        .search_hits
        .iter()
        .filter(|hit| seen.insert((hit.block.as_str(), hit.start, hit.end)))
        .take(MAX_RESOURCE_SEARCH_HITS)
        .enumerate()
    {
        let label = range_label(&snapshot.blocks, &hit.block, hit.start, hit.end)
            .unwrap_or_else(|| format!("Result {}", result_index + 1));
        let view_operation = WordProcessorOperation::NavigateTo {
            block: hit.block.clone(),
            start: hit.start,
            end: hit.end,
        };
        if let Some(spec) = operation(
            "go-to",
            "Go to result",
            OperationKind::View,
            &view_operation,
        )
        .map(|spec| {
            spec.requires(ContextRequirement::FindHits)
                .participant_view()
        }) {
            search.children.push(
                Resource::new(
                    format!(
                        "word-processor.search-hit:{}:{}:{}",
                        hit.block, hit.start, hit.end
                    ),
                    "word_processor.search-hit",
                    label,
                )
                .with_action(spec),
            );
        }
    }
    root.children.push(search);
}

fn add_document_edit_actions(root: &mut Resource, ctx: &WpResourceCtx) {
    let image_anchor = ctx.caret_block.clone();
    let image_offset = ctx.caret_offset;
    let content = ctx.content.clone();
    root.actions.push(
        ActionSpec::prompted_content(
            "place-image",
            "Place image",
            "Image content produced by the preceding action",
            "content hash",
            ExecutionLocus::ClientLocal,
            move |hash| {
                let Some(bytes) = content.read(hash) else {
                    return EncodedAction {
                        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                        payload: format!("content `{hash}` is not available on this machine")
                            .into_bytes(),
                    };
                };
                encode(&insert_image_action(
                    &image_anchor,
                    image_offset,
                    ImageInput::of(hash.to_string(), &bytes),
                ))
            },
        )
        .with_description(
            "Place the single content result of the immediately preceding action at the caret",
        )
        .reversible()
        .requires(ContextRequirement::Caret),
    );
    root.actions.push(
        immediate_spec(
            "insert-page-break",
            "Insert page break",
            &insert_page_break_action(&ctx.caret_block, ctx.caret_offset),
        )
        .requires(ContextRequirement::Caret),
    );
    for (id, label, align) in [
        ("align-left", "Align left", "left"),
        ("align-center", "Align centre", "center"),
        ("align-right", "Align right", "right"),
        ("align-justify", "Justify", "justify"),
    ] {
        root.actions.push(
            immediate_spec(
                id,
                label,
                &set_align_action(ctx.target_blocks.clone(), align),
            )
            .requires(ContextRequirement::Caret),
        );
    }
}

fn comment_resource(comment: &tilo_doc::snapshot::CommentView) -> Resource {
    let text = comment
        .blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let mut resource = Resource::new(
        format!("word-processor.comment:{}", comment.id),
        "word_processor.comment",
        format!(
            "Comment by {}: {}",
            short(&comment.author, "Unknown author"),
            short(&text, "Empty comment")
        ),
    )
    .with_action(immediate_spec(
        "resolve",
        "Resolve comment",
        &RichTextAction::ResolveComment {
            id: comment.id.clone(),
        },
    ));
    if let Some((block, start, end)) = &comment.range
        && let Some(spec) = operation(
            "go-to-anchor",
            "Go to commented text",
            OperationKind::View,
            &WordProcessorOperation::NavigateTo {
                block: block.clone(),
                start: *start,
                end: *end,
            },
        )
        .map(ActionSpec::participant_view)
    {
        resource.actions.push(spec);
    }
    for reply in comment.replies.iter().take(MAX_RESOURCE_SEARCH_HITS) {
        let reply_text = reply
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        resource.children.push(Resource::new(
            format!("word-processor.comment-reply:{}:{}", comment.id, reply.id),
            "word_processor.comment-reply",
            format!(
                "Reply by {}: {}",
                short(&reply.author, "Unknown author"),
                short(&reply_text, "Empty reply")
            ),
        ));
    }
    let parent = comment.id.clone();
    let domain = vec![parent.as_bytes().to_vec()];
    resource.actions.push(continuation::pure(
        ActionSpec::typed(
            "reply",
            "Reply to comment",
            "Reply text and author, stored in the shared comment thread",
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required_owned("author", ParamType::Text, "Reply author"),
                ActionParam::required_owned("text", ParamType::Text, "Reply text"),
                ActionParam::optional_owned("date", ParamType::Text, "ISO-8601 reply date"),
            ],
            move |args: &ActionArgs| {
                let author = args.text("author").unwrap_or_default().trim();
                let text = args.text("text").unwrap_or_default();
                if author.is_empty() || text.chars().count() > MAX_ACTION_TEXT_CHARS {
                    return EncodedAction {
                        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                        payload: b"reply author is required and reply text is too long".to_vec(),
                    };
                }
                encode(&reply_to_comment_action(
                    &parent,
                    author,
                    args.text("date").unwrap_or_default(),
                    text,
                ))
            },
        ),
        "reply-to-comment.v1",
        domain,
    ));
    resource
}

fn add_review_resources(root: &mut Resource, snapshot: &tilo_doc::snapshot::DocSnapshot) {
    if snapshot.comments.is_empty() && snapshot.changes.is_empty() {
        return;
    }
    let mut review = Resource::new("word-processor.review", "word_processor.review", "Review");
    if !snapshot.changes.is_empty() {
        // Bulk review rewrites everyone's tracked changes at once: confirmed
        // for people, refused for unattended callers.
        review.actions.push(
            immediate_spec(
                "accept-all",
                "Accept all changes",
                &RichTextAction::AcceptChanges { id: None },
            )
            .privileged(),
        );
        review.actions.push(
            immediate_spec(
                "reject-all",
                "Reject all changes",
                &RichTextAction::RejectChanges { id: None },
            )
            .privileged(),
        );
    }
    review
        .children
        .extend(snapshot.comments.iter().map(comment_resource));
    for change in &snapshot.changes {
        let kind = match change.kind {
            tilo_doc::snapshot::ChangeKind::Ins => "Insertion",
            tilo_doc::snapshot::ChangeKind::Del => "Deletion",
        };
        review.children.push(
            Resource::new(
                format!("word-processor.change:{}", change.id),
                "word_processor.change",
                format!("{kind} by {}", short(&change.author, "Unknown author")),
            )
            .with_action(immediate_spec(
                "accept",
                "Accept change",
                &RichTextAction::AcceptChanges {
                    id: Some(change.id.clone()),
                },
            ))
            .with_action(immediate_spec(
                "reject",
                "Reject change",
                &RichTextAction::RejectChanges {
                    id: Some(change.id.clone()),
                },
            )),
        );
    }
    root.children.push(review);
}

fn add_revision_resources(root: &mut Resource, doc: &AutoCommit) {
    let mut history_doc = doc.clone();
    // Revision resources need labels and hashes, not reconstructed operation
    // payloads for every change in the document's history.
    let changes = history_doc.get_changes_meta(&[]);
    if changes.is_empty() {
        return;
    }
    let mut history = Resource::new(
        "word-processor.revisions",
        "word_processor.revisions",
        "Revision history",
    );
    let start = changes.len().saturating_sub(MAX_RESOURCE_REVISIONS);
    for (index, change) in changes.iter().enumerate().skip(start) {
        let label = change.message.as_deref().map_or_else(
            || format!("Revision {}", index + 1),
            |message| short(message, "Revision"),
        );
        if let Some(spec) = operation(
            "show",
            "Show revision",
            OperationKind::View,
            &WordProcessorOperation::ShowRevision { index },
        ) {
            history.children.push(
                Resource::new(
                    format!("word-processor.revision:{}", change.hash),
                    "word_processor.revision",
                    label,
                )
                .with_action(spec),
            );
        }
    }
    root.children.push(history);
}

fn add_metadata_resources(root: &mut Resource, snapshot: &tilo_doc::snapshot::DocSnapshot) {
    if !snapshot.notes.is_empty() {
        let mut notes = Resource::new("word-processor.notes", "word_processor.notes", "Notes");
        for note in &snapshot.notes {
            let text = note
                .blocks
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            notes.children.push(Resource::new(
                format!("word-processor.note:{}", note.id),
                "word_processor.note",
                format!("{}: {}", note.kind, short(&text, "Empty note")),
            ));
        }
        root.children.push(notes);
    }
    if !snapshot.bookmarks.is_empty() {
        let mut bookmarks = Resource::new(
            "word-processor.bookmarks",
            "word_processor.bookmarks",
            "Bookmarks",
        );
        for bookmark in &snapshot.bookmarks {
            bookmarks.children.push(Resource::new(
                format!("word-processor.bookmark:{}", bookmark.name),
                "word_processor.bookmark",
                bookmark.name.clone(),
            ));
        }
        root.children.push(bookmarks);
    }
}

fn add_style_resources(root: &mut Resource, snapshot: &tilo_doc::snapshot::DocSnapshot) {
    if snapshot.styles.is_empty() {
        return;
    }
    let mut styles = Resource::new(
        "word-processor.styles",
        "word_processor.styles",
        "Paragraph styles",
    );
    for (style, properties) in &snapshot.styles {
        let bold = matches!(
            properties.get(tilo_doc::schema::STYLE_BOLD),
            Some(tilo_doc::AttrVal::Bool(true))
        );
        let mut child = Resource::new(
            format!("word-processor.style:{style}"),
            "word_processor.style",
            style.clone(),
        )
        .with_action(immediate_spec(
            "toggle-bold",
            if bold { "Remove bold" } else { "Make bold" },
            &set_style_property_action(style, tilo_doc::schema::STYLE_BOLD, AttrVal::Bool(!bold)),
        ));
        if !crate::styles::StyleSheet::is_builtin(style) {
            let instances = snapshot
                .blocks
                .iter()
                .filter(|block| {
                    matches!(block.attrs.get("style"), Some(tilo_doc::AttrVal::Str(id)) if id == style)
                })
                .map(|block| block.id.clone())
                .collect();
            child.actions.push(immediate_spec(
                "delete",
                "Delete style",
                &delete_style_action(style, instances),
            ));
        }
        styles.children.push(child);
    }
    root.children.push(styles);
}

fn image_resource(block: &tilo_doc::snapshot::BlockView) -> Resource {
    let edge = |key: &str| match block.attrs.get(key) {
        Some(tilo_doc::AttrVal::Int(value)) => (*value).clamp(0, 9_500),
        _ => 0,
    };
    let left = edge("crop_left");
    let top = edge("crop_top");
    let right = edge("crop_right");
    let bottom = edge("crop_bottom");
    let can_crop_further =
        left.saturating_add(right) <= 8_500 && top.saturating_add(bottom) <= 8_500;
    let mut resource = Resource::new(
        format!("word-processor.image:{}", block.id),
        "word_processor.image",
        "Image",
    )
    .with_action(immediate_spec(
        "delete",
        "Delete image",
        &remove_blocks_action(vec![block.id.clone()]),
    ));
    if can_crop_further {
        resource.actions.push(immediate_spec(
            "crop-inset",
            "Crop image by 5%",
            &set_image_crop_action(&block.id, left + 250, top + 250, right + 250, bottom + 250),
        ));
    }
    if left != 0 || top != 0 || right != 0 || bottom != 0 {
        resource.actions.push(immediate_spec(
            "reset-crop",
            "Reset crop",
            &set_image_crop_action(&block.id, 0, 0, 0, 0),
        ));
    }
    for (id, label, anchor, wrap) in [
        ("free-position", "Place freely on page", "page", "none"),
        ("flow-left", "Wrap text on right", "paragraph", "left"),
        ("flow-right", "Wrap text on left", "paragraph", "right"),
    ] {
        resource.actions.push(immediate_spec(
            id,
            label,
            &set_image_placement_action(&block.id, anchor, wrap, 0, 0, 0),
        ));
    }
    resource
}

fn table_resource(table_id: &str, table_blocks: &[&tilo_doc::snapshot::BlockView]) -> Resource {
    let coordinate = |block: &tilo_doc::snapshot::BlockView, key: &str| match block.attrs.get(key) {
        Some(tilo_doc::AttrVal::Int(value)) if *value >= 0 => num::usize_of_i64(*value),
        _ => 0,
    };
    let rows = table_blocks
        .iter()
        .map(|block| coordinate(block, "row"))
        .max()
        .map_or(0, |value| value.saturating_add(1));
    let columns = table_blocks
        .iter()
        .map(|block| coordinate(block, "col"))
        .max()
        .map_or(0, |value| value.saturating_add(1));
    let mut resource = Resource::new(
        format!("word-processor.table:{table_id}"),
        "word_processor.table",
        "Table",
    )
    .with_action(immediate_spec(
        "append-row",
        "Add row",
        &edit_table_action(table_id, TableEdit::InsertRow { at: rows }),
    ))
    .with_action(immediate_spec(
        "append-column",
        "Add column",
        &edit_table_action(table_id, TableEdit::InsertColumn { at: columns }),
    ))
    .with_action(immediate_spec(
        "delete",
        "Delete table",
        &edit_table_action(table_id, TableEdit::Delete),
    ));
    for cell in table_blocks {
        let row = coordinate(cell, "row");
        let column = coordinate(cell, "col");
        let mut cell_resource = Resource::new(
            format!("word-processor.cell:{}", cell.id),
            "word_processor.table-cell",
            format!(
                "Cell {}, {}",
                row.saturating_add(1),
                column.saturating_add(1)
            ),
        )
        .with_action(immediate_spec(
            "delete-row",
            "Delete row",
            &edit_table_action(table_id, TableEdit::DeleteRow { row }),
        ))
        .with_action(immediate_spec(
            "delete-column",
            "Delete column",
            &edit_table_action(table_id, TableEdit::DeleteColumn { column }),
        ));
        if let Some(spec) = operation(
            "go-to",
            "Go to cell",
            OperationKind::View,
            &WordProcessorOperation::NavigateTo {
                block: cell.id.clone(),
                start: 0,
                end: cell.text.chars().count(),
            },
        )
        .map(ActionSpec::participant_view)
        {
            cell_resource.actions.push(spec);
        }
        resource.children.push(cell_resource);
    }
    resource
}

fn add_structural_resources(root: &mut Resource, snapshot: &tilo_doc::snapshot::DocSnapshot) {
    root.children.extend(
        snapshot
            .blocks
            .iter()
            .filter(|block| block.kind == "image")
            .map(image_resource),
    );
    let mut table_ids = Vec::new();
    for block in snapshot.blocks.iter().filter(|block| block.kind == "cell") {
        if let Some(tilo_doc::AttrVal::Str(table_id)) = block.attrs.get("table")
            && !table_ids.contains(table_id)
        {
            table_ids.push(table_id.clone());
        }
    }
    for table_id in &table_ids {
        let blocks = snapshot
            .blocks
            .iter()
            .filter(|block| {
                block.kind == "cell"
                    && matches!(block.attrs.get("table"), Some(tilo_doc::AttrVal::Str(id)) if id == table_id)
            })
            .collect::<Vec<_>>();
        root.children.push(table_resource(table_id, &blocks));
    }
    for block in snapshot
        .blocks
        .iter()
        .filter(|block| block.kind == "heading")
    {
        let mut section = Resource::new(
            format!("word-processor.section:{}", block.id),
            "word_processor.section",
            short(&block.text, "Heading"),
        );
        for (id, label, align) in [
            ("align-left", "Align left", "left"),
            ("align-center", "Align centre", "center"),
            ("align-right", "Align right", "right"),
        ] {
            section.actions.push(immediate_spec(
                id,
                label,
                &set_align_action(vec![block.id.clone()], align),
            ));
        }
        root.children.push(section);
    }
}

/// Stable id standing for every view-derived `word-processor.selection:…`
/// resource in the catalogue schema.
pub const SCHEMA_SELECTION_RESOURCE: &str = "word-processor.selection";
/// Stable id standing for every `word-processor.search-hit:…` resource in
/// the catalogue schema.
pub const SCHEMA_SEARCH_HIT_RESOURCE: &str = "word-processor.search-hit";

/// Writer's participant-independent catalogue schema
/// ([`ResourceProvider::catalogue_schema`]): the tree every participant can
/// reach, with no participant's caret, selection or Find hits in it.
///
/// - Root operations are all present, whatever any view holds; context
///   preconditions are declared as [`ContextRequirement`] flags, and each
///   operation's contract, params and input shape equal those in every
///   participant's tree.
/// - Selection- and Find-derived resources appear once under stable ids
///   ([`SCHEMA_SELECTION_RESOURCE`], [`SCHEMA_SEARCH_HIT_RESOURCE`]), never
///   as `selection:<range>` / `search-hit:<range>`.
/// - Document-derived resources (images, tables, sections, review,
///   revisions, styles) are included: they do not depend on any view.
pub fn catalogue_schema(doc: &AutoCommit) -> Vec<Resource> {
    let block = tilo_doc::model::open(doc, &automerge::ROOT)
        .ok()
        .and_then(|containers| {
            tilo_doc::model::blocks_in_order(doc, &containers)
                .first()
                .cloned()
        })
        .unwrap_or_else(|| "word-processor.schema".to_owned());
    // A synthetic view that switches on every view-dependent builder; its
    // positions never reach the schema (ids are replaced below, and the
    // schema is declaration, never invoked).
    let ctx = WpResourceCtx {
        caret_block: block.clone(),
        caret_offset: 0,
        target_blocks: vec![block.clone()],
        format_spans: vec![Span {
            block: block.clone(),
            start: 0,
            end: 1,
        }],
        selection: Some(SelectionTarget {
            start_block: block.clone(),
            start: 0,
            end_block: block.clone(),
            end: 1,
        }),
        region: RegionKind::Body,
        search_hits: vec![SearchTarget {
            block,
            start: 0,
            end: 1,
        }],
        content: tilo_content::ContentStore::memory(),
    };
    let mut resources = provide_resources_with_file(&ctx, doc, None);
    resources.iter_mut().for_each(declarations_only);
    for root in &mut resources {
        for child in &mut root.children {
            if child.kind == "word_processor.selection" {
                child.id = tilo_actions::ResourceId(SCHEMA_SELECTION_RESOURCE.to_owned());
                child.label = "Selection".into();
            }
            if child.kind == "word_processor.search-results" {
                child.children.truncate(1);
                for hit in &mut child.children {
                    hit.id = tilo_actions::ResourceId(SCHEMA_SEARCH_HIT_RESOURCE.to_owned());
                    hit.label = "Search result".into();
                }
            }
        }
    }
    resources
}

const DECLARATION_ONLY: &[u8] =
    b"catalogue schema is a declaration only: resolve this operation in the invoker's own tree";

/// Replace every builder in a schema tree by a refusal, keeping each
/// operation's id, label, description, kind, locus, operation type,
/// contract, params and input shape. The schema is built on a synthetic view;
/// resolving an invocation against it must never encode a positional
/// envelope from that view. Fixed-encoding and no-argument operations become
/// deferred ones declaring their registered type (a fixed sentinel would fail
/// surface validation); argument-taking ones keep their prompt.
fn declarations_only(resource: &mut Resource) {
    for spec in &mut resource.actions {
        *spec = declaration_only(spec);
    }
    resource.children.iter_mut().for_each(declarations_only);
}

fn declaration_only(spec: &ActionSpec) -> ActionSpec {
    let refuse = || EncodedAction {
        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
        payload: DECLARATION_ONLY.to_vec(),
    };
    let document = spec.kind == OperationKind::Document;
    let declared_type = |fallback: &str| {
        spec.operation_type
            .clone()
            .unwrap_or_else(|| fallback.to_owned())
    };
    let mut out = match &spec.input {
        tilo_actions::ActionInput::Immediate(encoded) => {
            let type_id = declared_type(&encoded.type_id);
            if document {
                ActionSpec::deferred(&spec.id, &spec.label, spec.locus, type_id, refuse)
            } else {
                ActionSpec::deferred_operation(&spec.id, &spec.label, spec.kind, type_id, refuse)
            }
        }
        tilo_actions::ActionInput::Deferred { type_id, .. } => {
            let type_id = declared_type(type_id);
            if document {
                ActionSpec::deferred(&spec.id, &spec.label, spec.locus, type_id, refuse)
            } else {
                ActionSpec::deferred_operation(&spec.id, &spec.label, spec.kind, type_id, refuse)
            }
        }
        tilo_actions::ActionInput::Prompted {
            prompt,
            placeholder,
            collects: tilo_actions::Collects::Content,
            ..
        } => ActionSpec::prompted_content(
            &spec.id,
            &spec.label,
            prompt,
            placeholder,
            spec.locus,
            move |_| refuse(),
        ),
        tilo_actions::ActionInput::Prompted { placeholder, .. } if document => ActionSpec::typed(
            &spec.id,
            &spec.label,
            placeholder,
            spec.locus,
            spec.params().to_vec(),
            move |_| refuse(),
        ),
        tilo_actions::ActionInput::Prompted { placeholder, .. } => ActionSpec::typed_operation(
            &spec.id,
            &spec.label,
            placeholder,
            spec.kind,
            declared_type(WORD_PROCESSOR_OPERATION_TYPE),
            spec.params().to_vec(),
            move |_| refuse(),
        ),
    };
    out.description.clone_from(&spec.description);
    out.with_contract(spec.contract.clone())
}

/// Build the resource tree the word processor currently has open.
pub fn provide_resources(ctx: &WpResourceCtx, doc: &AutoCommit) -> Vec<Resource> {
    provide_resources_with_file(ctx, doc, None)
}

pub(crate) fn provide_resources_with_file(
    ctx: &WpResourceCtx,
    doc: &AutoCommit,
    file: Option<&continuation::FileContext>,
) -> Vec<Resource> {
    let Ok(containers) = tilo_doc::model::open(doc, &automerge::ROOT) else {
        return vec![resource_root(ctx, doc, file, None)];
    };
    let snapshot = tilo_doc::model::materialize(doc, &containers);
    provide_resources_from(ctx, doc, file, None, &snapshot, true)
}

/// The operations every tree has, before any listing is read from the
/// document. `body` is the kept order of `doc`'s body, when there is one.
fn resource_root(
    ctx: &WpResourceCtx,
    doc: &AutoCommit,
    file: Option<&continuation::FileContext>,
    body: Option<&tilo_document::BodyOrder>,
) -> Resource {
    let mut root = document_resource(doc);
    add_file_operations(&mut root, file);
    add_view_operations(&mut root);
    add_caret_actions(&mut root, ctx);
    add_selection_resources(&mut root, ctx, editing::large_selection(ctx, doc, body));
    add_body_append_actions(&mut root, ctx, doc, body);
    add_comment_resource_action(&mut root, ctx);
    root
}

/// The resource tree over `listed`: a snapshot holding at least the blocks,
/// review, notes, styles and properties the tree lists. `body`, the kept
/// order of `doc`'s body, lets the operations read only the blocks around
/// the caret.
fn provide_resources_from(
    ctx: &WpResourceCtx,
    doc: &AutoCommit,
    file: Option<&continuation::FileContext>,
    body: Option<&tilo_document::BodyOrder>,
    listed: &tilo_doc::snapshot::DocSnapshot,
    revisions: bool,
) -> Vec<Resource> {
    let mut root = resource_root(ctx, doc, file, body);
    if !revisions {
        // A hosted document's History is the framework's surface; the
        // editor's own version strip is not offered beside it.
        root.actions.retain(|action| action.id != "history");
    }
    editing::add_operations(&mut root, ctx, doc, body);
    view_ops::add_operations(&mut root, ctx);
    add_search_resources(&mut root, ctx, listed);
    add_document_edit_actions(&mut root, ctx);
    add_review_resources(&mut root, listed);
    if revisions {
        add_revision_resources(&mut root, doc);
    }
    add_metadata_resources(&mut root, listed);
    add_style_resources(&mut root, listed);
    add_structural_resources(&mut root, listed);
    vec![root]
}

/// The resource tree from the incrementally kept document state: its
/// objects (headings, images, tables), review, notes, styles and properties,
/// and the paragraphs Find is pointing at — never the document's prose, so
/// listing it costs as much as what it lists however long the document is.
/// A hosted document's revisions are the framework's History, not listed
/// here.
pub(crate) fn provide_resources_listed(
    ctx: &WpResourceCtx,
    doc: &AutoCommit,
    file: Option<&continuation::FileContext>,
    state: &tilo_document::RichTextState,
    revisions: bool,
) -> Vec<Resource> {
    let c = state.containers();
    let body = state.body();
    // Nearest the caret: a book-length document lists the sections, images
    // and tables around where its user is, not all of them.
    let caret = body.position(&ctx.caret_block).unwrap_or(0);
    let mut blocks: Vec<(usize, tilo_doc::snapshot::BlockView)> = Vec::new();
    for (kind, limit) in [
        ("heading", MAX_RESOURCE_OBJECTS),
        ("image", MAX_RESOURCE_OBJECTS),
        ("cell", MAX_RESOURCE_OBJECTS.saturating_mul(16)),
    ] {
        blocks.extend(
            state
                .featured_near(kind, caret, limit)
                .into_iter()
                .filter_map(|view| Some((body.position(&view.id)?, (*view).clone()))),
        );
    }
    let hits: Vec<String> = ctx
        .search_hits
        .iter()
        .take(MAX_RESOURCE_SEARCH_HITS)
        .map(|hit| hit.block.clone())
        .filter(|id| !blocks.iter().any(|(_, view)| view.id == *id))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    blocks.extend(
        tilo_doc::model::materialize_blocks(doc, c, &hits)
            .into_iter()
            .filter_map(|view| Some((body.position(&view.id)?, view))),
    );
    blocks.sort_by_key(|(position, _)| *position);
    let listed = tilo_doc::snapshot::DocSnapshot {
        blocks: blocks.into_iter().map(|(_, view)| view).collect(),
        styles: tilo_doc::model::all_styles(doc, c),
        properties: tilo_doc::model::doc_props(doc, c),
        notes: state.notes().as_ref().clone(),
        comments: state.comments().as_ref().clone(),
        changes: state.changes().as_ref().clone(),
        ..tilo_doc::snapshot::DocSnapshot::default()
    };
    provide_resources_from(ctx, doc, file, Some(&**body), &listed, revisions)
}

pub(crate) fn file_choice_resource(generation: u64) -> Result<Resource, String> {
    let mut resource = Resource::new(
        format!("word-processor.file-choice:{generation}"),
        "word_processor.file_choice",
        crate::i18n::t("file-choice-title"),
    );
    for (id, label, use_selected) in [
        ("keep-workspace", "file-choice-keep", false),
        ("use-selected", "file-choice-use", true),
    ] {
        resource.actions.push(
            operation(
                id,
                &crate::i18n::t(label),
                OperationKind::Effect,
                &WordProcessorOperation::ResolveFileChoice {
                    generation,
                    use_selected,
                },
            )
            .ok_or("file choice operation could not be encoded")?,
        );
    }
    Ok(resource)
}

// Inherent resource publication (was ResourceProvider)
impl crate::widget::WordProcessorWidget {
    pub fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        if let Some(resource) = self.file_choice_resource() {
            return vec![resource];
        }
        let Some(ctx) = self.wp_resource_ctx(doc) else {
            return Vec::new();
        };
        let file = self.file_continuation_context();
        // The host lists resources every frame; read them from the kept
        // state, not the document.
        self.listed_resources(&ctx, doc, &file)
            .unwrap_or_else(|| provide_resources_with_file(&ctx, doc, Some(&file)))
    }

    pub fn publish_catalogue_schema(&self, doc: &AutoCommit) -> Vec<Resource> {
        catalogue_schema(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use tilo_actions::ActionError;
    use tilo_actions::LocalTransport;
    use tilo_automerge::ApplyIncoming;
    use tilo_doc::DocMut;

    /// A four-byte stand-in image, described as an [`ImageInput`].
    fn test_image(width: Option<i64>, height: Option<i64>) -> ImageInput {
        ImageInput {
            content: tilo_content::hash_bytes(&[1, 2, 3, 4]),
            mime: String::new(),
            len: 4,
            width,
            height,
        }
    }
    use ui::testing::action_contract::spec_encoding;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    /// The view catalogue exposes zoom reset as a typed operation; the resource catalogue exposes the
    /// structural document contract; file operations are typed background jobs; resource results and
    /// revisions are bounded and precisely addressable.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn the_resource_catalogue_exposes_typed_bounded_document_operations() {
        // Phase: view_catalogue_exposes_zoom_reset_as_a_typed_operation.
        {
            let mut root = document_resource(&AutoCommit::new());
            add_view_operations(&mut root);
            let spec = root
                .actions
                .iter()
                .find(|spec| spec.id == "zoom-reset")
                .expect("the visible zoom reset has a resource action");
            let encoded = spec_encoding(spec, None);
            assert_eq!(encoded.type_id, WORD_PROCESSOR_OPERATION_TYPE);
            assert_eq!(
                decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
                Some(WordProcessorOperation::ZoomReset),
                "the resource action decodes to the one canonical reset operation"
            );
        }

        // Phase: resource_catalogue_exposes_structural_document_contract.
        // The catalogue publishes each structural document object as a stable,
        // directly actionable resource.
        {
            let (mut state, ids) = seeded("# Section\n\nBody\n");
            let para = ids[1].clone();
            dispatch(
                &mut state.doc,
                &insert_image_action(&para, 0, test_image(Some(20), Some(10))),
            );
            let after_img = tilo_doc::model::blocks_in_order(
                &state.doc,
                &tilo_doc::model::open(&state.doc, &automerge::ROOT).unwrap(),
            );
            let last = after_img.last().cloned().unwrap();
            dispatch(&mut state.doc, &insert_table_action(&last, 2, 2));

            let ctx = WpResourceCtx {
                caret_block: para.clone(),
                caret_offset: 0,
                target_blocks: vec![para],
                format_spans: Vec::new(),
                selection: None,
                region: RegionKind::Body,
                search_hits: Vec::new(),
                content: tilo_content::ContentStore::memory(),
            };
            let resources = provide_resources(&ctx, &state.doc);
            assert_eq!(resources.len(), 1, "one document root");
            let root = &resources[0];
            let ids_of = |kind: &str| root.children.iter().filter(|r| r.kind == kind).count();
            assert_eq!(ids_of("word_processor.image"), 1, "an image resource");
            assert_eq!(ids_of("word_processor.table"), 1, "a table resource");
            let table = root
                .children
                .iter()
                .find(|resource| resource.kind == "word_processor.table")
                .unwrap();
            assert_eq!(table.children.len(), 4, "every stable cell is navigable");
            assert!(table.children.iter().all(|cell| {
                cell.kind == "word_processor.table-cell"
                    && cell.actions.iter().any(|action| action.id == "go-to")
                    && cell.actions.iter().any(|action| action.id == "delete-row")
                    && cell
                        .actions
                        .iter()
                        .any(|action| action.id == "delete-column")
            }));
            assert!(
                ids_of("word_processor.section") >= 1,
                "a heading section resource"
            );
            let image = root
                .children
                .iter()
                .find(|resource| resource.kind == "word_processor.image")
                .unwrap();
            assert!(image.actions.iter().any(|action| action.id == "delete"));
            assert!(image.actions.iter().any(|action| action.id == "crop-inset"));
            assert!(table.actions.iter().any(|action| action.id == "delete"));
            assert!(table.actions.iter().any(|action| action.id == "append-row"));
        }

        // Phase: file_operations_are_typed_background_jobs.
        {
            let (state, ids) = seeded("Intro\n");
            let ctx = WpResourceCtx {
                caret_block: ids[0].clone(),
                caret_offset: 0,
                target_blocks: vec![ids[0].clone()],
                format_spans: Vec::new(),
                selection: None,
                region: RegionKind::Body,
                search_hits: Vec::new(),
                content: tilo_content::ContentStore::memory(),
            };
            let resources = provide_resources(&ctx, &state.doc);
            for id in ["open", "save", "save-as"] {
                let action = resources[0]
                    .actions
                    .iter()
                    .find(|action| action.id == id)
                    .expect("file action");
                assert_eq!(action.contract.result, ResultSpec::Job);
                assert!(matches!(
                    action.contract.execution,
                    ExecutionMode::Background { .. }
                ));
                assert!(action.contract.validate_declaration().is_empty());
            }
        }

        // Phase: resource_results_and_revisions_are_bounded_and_precisely_addressable.
        {
            let (mut state, ids) =
                seeded(&format!("{}\n", "x".repeat(MAX_RESOURCE_SEARCH_HITS + 16)));
            for index in 0..MAX_RESOURCE_REVISIONS + 8 {
                state
                    .doc
                    .put(automerge::ROOT, "revision-fixture", index as u64)
                    .unwrap();
                state.doc.commit_with(
                    automerge::transaction::CommitOptions::default()
                        .with_message(format!("Edit {index}")),
                );
            }
            let mut remote = state.doc.fork();
            remote.put(automerge::ROOT, "remote-fixture", true).unwrap();
            remote.commit();
            state
                .doc
                .put(automerge::ROOT, "local-fixture", true)
                .unwrap();
            state.doc.commit();
            state.doc.merge_incoming(&mut remote).unwrap();
            state
                .doc
                .put(automerge::ROOT, "unfinished-fixture", true)
                .unwrap();
            let pending = (state.doc.base_heads(), state.doc.pending_ops());
            let expected = state.doc.clone().get_changes(&[]);
            let block = ids[0].clone();
            let resources = provide_resources(&overflowing_resource_ctx(&block), &state.doc);
            let root = &resources[0];
            assert!(
                root.children
                    .iter()
                    .any(|resource| resource.kind == "word_processor.selection")
            );
            let results = root
                .children
                .iter()
                .find(|resource| resource.id.as_str() == "word-processor.search-results")
                .expect("bounded search result group");
            assert_eq!(results.children.len(), MAX_RESOURCE_SEARCH_HITS);
            assert!(results.children.iter().all(|hit| {
                hit.actions.iter().any(|action| action.id == "go-to")
                    && hit.id.as_str().contains(&block)
            }));
            let revisions = root
                .children
                .iter()
                .find(|resource| resource.id.as_str() == "word-processor.revisions")
                .expect("history resource group");
            assert_eq!(revisions.children.len(), MAX_RESOURCE_REVISIONS);
            let start = expected.len() - MAX_RESOURCE_REVISIONS;
            for (index, (revision, change)) in revisions
                .children
                .iter()
                .zip(&expected[start..])
                .enumerate()
            {
                let index = start + index;
                assert_eq!(
                    revision.id.as_str(),
                    format!("word-processor.revision:{}", change.hash())
                );
                let label = change.message().map_or_else(
                    || format!("Revision {}", index + 1),
                    |message| short(message, "Revision"),
                );
                assert_eq!(revision.label, label);
                let show = revision
                    .actions
                    .iter()
                    .find(|action| action.id == "show")
                    .unwrap();
                assert_eq!(
                    show.build_args(std::iter::empty::<(String, tilo_actions::ArgValue)>())
                        .unwrap(),
                    EncodedAction::encode_operation(
                        WORD_PROCESSOR_OPERATION_TYPE,
                        &WordProcessorOperation::ShowRevision { index }
                    )
                    .unwrap()
                );
            }
            assert_eq!(
                (state.doc.base_heads(), state.doc.pending_ops()),
                pending,
                "resource discovery must not commit the owner"
            );
            assert!(
                revisions
                    .children
                    .iter()
                    .all(|revision| revision.actions.iter().any(|action| action.id == "show"))
            );
        }
    }

    /// A `ui::State` holding a canonical document seeded from `markdown`, plus the
    /// ordered body block ids.
    fn seeded(markdown: &str) -> (ui::State, Vec<String>) {
        let mut state = ui::State::new();
        let mut ed = crate::EditorDoc::bind(&mut state.doc, &state.root);
        {
            use tilo_doc_codecs::FormatProvider;
            let mut dm = ed.edit(&mut state.doc);
            tilo_doc_codecs::MarkdownCodec
                .import(
                    markdown.as_bytes(),
                    &mut dm,
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
        }
        let ids = tilo_doc::model::blocks_in_order(&state.doc, &ed.containers);
        (state, ids)
    }

    fn body_blocks(doc: &AutoCommit) -> Vec<tilo_doc::snapshot::BlockView> {
        let c = tilo_doc::model::open(doc, &automerge::ROOT).unwrap();
        tilo_doc::model::materialize(doc, &c).blocks
    }

    fn text_of(doc: &AutoCommit, block: &str) -> String {
        let c = tilo_doc::model::open(doc, &automerge::ROOT).unwrap();
        tilo_doc::model::block_text(doc, &c, block).unwrap_or_default()
    }

    /// Dispatch an action through the Tilo executor with a discarding transport
    /// (the local, single-peer path the affordances take).
    fn dispatch(doc: &mut AutoCommit, action: &RichTextAction) {
        let encoded = encode(action);
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(doc, action, encoded, &mut tx).unwrap();
    }

    /// Splice validation matches the snapshot without scanning regions; a stale multi-target action is an
    /// atomic no-op; hostile table dimensions are rejected before any write; an unbound document is refused
    /// without a write.
    #[expect(
        clippy::too_many_lines,
        reason = "end-to-end validation scenario that seeds every region shape and checks each against the snapshot in order"
    )]
    #[test]
    fn invalid_or_stale_actions_are_refused_before_any_write() {
        // Phase: splice_validation_matches_snapshot_without_scanning_regions.
        {
            use automerge::{ObjType, ScalarValue};
            use tilo_doc::schema::{BLOCK_ORDER, BLOCK_TEXT, KEY_FOOTER, KEY_HEADER};
            let (mut state, ids) = seeded("Aé🦀e\u{301}\n");
            let c = tilo_doc::model::open(&state.doc, &state.root).unwrap();
            for region in [RegionKind::Header, RegionKind::Footer] {
                let action = RichTextAction::SpliceText {
                    region,
                    block: ids[0].clone(),
                    at: 0,
                    del: 0,
                    ins: "x".into(),
                };
                assert!(!action_is_valid(&action, &state.doc, &c), "absent region");
            }
            // Keep c from before lazy header/footer creation to exercise fallback lookup.
            let header = state
                .doc
                .put_object(&c.root, KEY_HEADER, ObjType::Map)
                .unwrap();
            let footer = state
                .doc
                .put_object(&c.root, KEY_FOOTER, ObjType::Map)
                .unwrap();
            for container in [&c.blocks, &header, &footer] {
                for id in [
                    "valid",
                    "missing_text",
                    "scalar_text",
                    "object_text",
                    "missing_order",
                    "bad_order",
                ] {
                    let obj = state.doc.put_object(container, id, ObjType::Map).unwrap();
                    if id != "missing_order" {
                        if id == "bad_order" {
                            state.doc.put(&obj, BLOCK_ORDER, "invalid").unwrap();
                        } else {
                            state
                                .doc
                                .put(&obj, BLOCK_ORDER, ScalarValue::Bytes(vec![]))
                                .unwrap();
                        }
                    }
                    if id == "scalar_text" {
                        state
                            .doc
                            .put(&obj, BLOCK_TEXT, "not a text object")
                            .unwrap();
                    } else if id == "object_text" {
                        state
                            .doc
                            .put_object(&obj, BLOCK_TEXT, ObjType::Map)
                            .unwrap();
                    } else if id != "missing_text" {
                        let text = state
                            .doc
                            .put_object(&obj, BLOCK_TEXT, ObjType::Text)
                            .unwrap();
                        state.doc.splice_text(&text, 0, 0, "Aé🦀e\u{301}").unwrap();
                    }
                }
                state
                    .doc
                    .put(container, "scalar_block", "not a block")
                    .unwrap();
                state
                    .doc
                    .put_object(container, "deleted", ObjType::Map)
                    .unwrap();
                state.doc.delete(container, "deleted").unwrap();
            }
            let snap = tilo_doc::model::materialize(&state.doc, &c);
            for region in [RegionKind::Body, RegionKind::Header, RegionKind::Footer] {
                let blocks = match region {
                    RegionKind::Body => &snap.blocks,
                    RegionKind::Header => &snap.header,
                    RegionKind::Footer => &snap.footer,
                };
                for id in [
                    ids[0].as_str(),
                    "valid",
                    "missing_text",
                    "scalar_text",
                    "object_text",
                    "missing_order",
                    "bad_order",
                    "scalar_block",
                    "deleted",
                ] {
                    for at in [0, 1, 5, 6, usize::MAX] {
                        for del in [0, 1, 5, usize::MAX] {
                            let action = RichTextAction::SpliceText {
                                region,
                                block: id.into(),
                                at,
                                del,
                                ins: "🦀".into(),
                            };
                            let expected = blocks.iter().find(|b| b.id == id).is_some_and(|b| {
                                let len = b.text.chars().count();
                                at <= len && del <= len.saturating_sub(at)
                            });
                            let before = tilo_doc::model::ordered_keys_scan_count();
                            assert_eq!(
                                action_is_valid(&action, &state.doc, &c),
                                expected,
                                "{action:?}"
                            );
                            assert_eq!(
                                tilo_doc::model::ordered_keys_scan_count(),
                                before,
                                "single-block validation must not scan regions"
                            );
                        }
                    }
                }
            }
            for (count, expected) in [
                (MAX_ACTION_TEXT_CHARS, true),
                (MAX_ACTION_TEXT_CHARS + 1, false),
            ] {
                let action = RichTextAction::SpliceText {
                    region: RegionKind::Body,
                    block: ids[0].clone(),
                    at: 0,
                    del: 0,
                    ins: "é".repeat(count),
                };
                assert_eq!(action_is_valid(&action, &state.doc, &c), expected);
            }
            state.doc.commit();
            let before = state.save();
            let action = RichTextAction::SpliceText {
                region: RegionKind::Body,
                block: ids[0].clone(),
                at: usize::MAX,
                del: 1,
                ins: "x".into(),
            };
            assert!(matches!(
                action.apply(&mut state.doc),
                Err(ActionError::Refused(_))
            ));
            assert_eq!(state.save(), before, "invalid splice must make no writes");
        }

        // Phase: stale_multi_target_action_is_an_atomic_noop.
        {
            let (mut state, ids) = seeded("first\nsecond\n");
            let before = state.doc.save();
            let action = set_block_attr_action(
                RegionKind::Body,
                vec![ids[0].clone(), "deleted-by-a-peer".to_string()],
                "align",
                AttrVal::Str("center".to_string()),
            );
            let result = action.apply(&mut state.doc);
            assert!(matches!(result, Err(ActionError::Refused(_))));
            assert_eq!(
                state.doc.save(),
                before,
                "one stale target must not leave the valid target partly changed"
            );
        }

        // Phase: hostile_table_dimensions_are_rejected_before_any_write.
        {
            let (mut state, ids) = seeded("anchor\n");
            let before = state.doc.save();
            let result =
                insert_table_action(&ids[0], MAX_TABLE_DIMENSION + 1, 1).apply(&mut state.doc);
            assert!(matches!(result, Err(ActionError::Refused(_))));
            assert_eq!(state.doc.save(), before);
        }

        // Phase: unbound_document_is_refused_without_a_write.
        {
            let mut doc = AutoCommit::new();
            let before = doc.save();
            let result = RichTextAction::ReplaceBody {
                markdown: "# should not initialize an arbitrary document".to_string(),
            }
            .apply(&mut doc);
            assert!(matches!(result, Err(ActionError::Refused(_))));
            assert_eq!(doc.save(), before);
        }
    }

    /// A heading, two paragraphs, a comment and a tracked insertion, built
    /// through the markdown import and the document model's review writes.
    fn wp_seed() -> AutoCommit {
        let (mut state, ids) = seeded("# Title\n\nHello world\n\nonetwo\n");
        let c = tilo_doc::model::open(&state.doc, &automerge::ROOT).unwrap();
        let mut id_gen = tilo_doc::IdGen::new(state.doc.get_actor().to_hex_string());
        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut id_gen,
            };
            dm.add_comment(&ids[1], 0, 5, "Maya", "Please clarify this.");
            dm.track_insert(&ids[2], 6, " three", "Jonas", "2026-09-05");
        }
        state.doc.commit();
        state.doc
    }

    /// The `n`th body block's stable id.
    fn nth_block(doc: &AutoCommit, n: usize) -> String {
        let c = tilo_doc::model::open(doc, &automerge::ROOT).unwrap();
        tilo_doc::model::blocks_in_order(doc, &c)
            .into_iter()
            .nth(n)
            .expect("seeded block")
    }

    #[test]
    fn live_presentation_keeps_selection_without_a_document_root() {
        let doc = wp_seed();
        let ctx = wp_ctx(&doc);
        let extras = live_presentation_resources(&ctx, &doc);
        assert!(
            extras
                .iter()
                .all(|resource| resource.id.as_str() != "word-processor.document"),
            "hosted extras must not republish the document root"
        );
        assert!(
            extras
                .iter()
                .any(|resource| resource.kind == "word_processor.selection"),
            "selection stays addressable: {:?}",
            extras
                .iter()
                .map(|resource| resource.kind.as_str())
                .collect::<Vec<_>>()
        );
    }

    /// "Hello" selected in the second block, as the widget reports it.
    fn wp_ctx(doc: &AutoCommit) -> WpResourceCtx {
        let block = nth_block(doc, 1);
        WpResourceCtx {
            caret_block: block.clone(),
            caret_offset: 5,
            target_blocks: vec![block.clone()],
            format_spans: vec![Span {
                block: block.clone(),
                start: 0,
                end: 5,
            }],
            selection: Some(SelectionTarget {
                start_block: block.clone(),
                start: 0,
                end_block: block,
                end: 5,
            }),
            region: RegionKind::Body,
            search_hits: Vec::new(),
            content: tilo_content::ContentStore::memory(),
        }
    }

    fn hello_span(doc: &AutoCommit) -> Vec<Span> {
        vec![Span {
            block: nth_block(doc, 1),
            start: 0,
            end: 5,
        }]
    }

    ui::action_contract_tests! {
        seed: wp_seed(),
        registry: registry(),
        resources: |doc| provide_resources(&wp_ctx(doc), doc),
        palette: [
            "Insert table",
            "Insert page break",
            "Align centre",
            "Replace selection",
            "Add or change hyperlink",
            "Accept change",
            "Accept all changes",
        ],
        replay: [
            splice_text => |doc| encode(&splice_text_action(
                RegionKind::Body,
                &nth_block(doc, 1),
                6,
                11,
                "there",
            )),
            set_mark => |doc| encode(&set_mark_action(
                RegionKind::Body,
                hello_span(doc),
                "strong",
                Some(MarkVal::Bool(true)),
                vec![],
            )),
            set_paragraph_style => |doc| encode(&set_paragraph_style_action(
                RegionKind::Body,
                vec![nth_block(doc, 1)],
                "heading",
                Some(2),
                None,
            )),
            insert_table => |doc| encode(&insert_table_action(&nth_block(doc, 0), 2, 3)),
            insert_page_break => |doc| encode(&insert_page_break_action(&nth_block(doc, 2), 3)),
            replace_body => |_| encode(&replace_body_action("# Fresh\n\nBrand new body\n")),
        ],
        undo: [
            set_paragraph_style => |doc| encode(&set_paragraph_style_action(
                RegionKind::Body,
                vec![nth_block(doc, 1)],
                "heading",
                Some(1),
                None,
            )),
            insert_table => |doc| encode(&insert_table_action(&nth_block(doc, 0), 2, 3)),
            splice_text => |doc| encode(&splice_text_action(
                RegionKind::Body,
                &nth_block(doc, 1),
                6,
                11,
                "there",
            )),
        ],
        encodes: [
            insert_table => ("tilo.rich-text", "insert-table", |doc, _| {
                encode(&insert_table_action(&nth_block(doc, 1), 2, 2))
            }),
            align_center => ("tilo.rich-text", "align-center", |doc, _| {
                encode(&set_align_action(vec![nth_block(doc, 1)], "center"))
            }),
            replace_selection => (
                "tilo.rich-text",
                "replace-selection",
                arg: "guide",
                |doc, _| {
                    let block = nth_block(doc, 1);
                    encode(&replace_selection_action(
                        RegionKind::Body,
                        &block,
                        0,
                        &block,
                        5,
                        "guide",
                    ))
                }
            ),
            set_hyperlink => (
                "tilo.rich-text",
                "set-hyperlink",
                arg: "https://example.com/guide",
                |doc, _| encode(&set_hyperlink_action(
                    RegionKind::Body,
                    hello_span(doc),
                    "https://example.com/guide",
                ))
            ),
        ],
    }

    /// Selected text exposes a typed hyperlink action that replays; targets are limited to safe authored
    /// schemes; validation is enforced by both resource and executor.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn hyperlinks_are_typed_safe_and_validated_on_both_sides() {
        // Phase: selected_text_exposes_a_typed_hyperlink_action_that_replays.
        {
            let (mut state, ids) = seeded("Read this\n");
            let block = ids[0].clone();
            let ctx = WpResourceCtx {
                format_spans: vec![Span {
                    block: block.clone(),
                    start: 0,
                    end: 4,
                }],
                selection: Some(SelectionTarget {
                    start_block: block.clone(),
                    start: 0,
                    end_block: block.clone(),
                    end: 4,
                }),
                region: RegionKind::Body,
                caret_block: block.clone(),
                caret_offset: 4,
                target_blocks: vec![block.clone()],
                search_hits: Vec::new(),
                content: tilo_content::ContentStore::memory(),
            };
            let root = provide_resources(&ctx, &state.doc).pop().unwrap();
            let replace = root
                .actions
                .iter()
                .find(|action| action.id == "replace-selection")
                .expect("selected text exposes canonical replacement");
            let replacement = replace
                .build_args([(
                    "text".to_string(),
                    tilo_actions::ArgValue::Text("guide".into()),
                )])
                .unwrap();
            assert_eq!(
                replacement,
                encode(&replace_selection_action(
                    RegionKind::Body,
                    &block,
                    0,
                    &block,
                    4,
                    "guide",
                )),
                "palette binding is byte-identical to the canonical selection action"
            );
            let link = root
                .actions
                .iter()
                .find(|action| action.id == "set-hyperlink")
                .expect("selected text exposes link authoring");
            assert_eq!(link.params().len(), 1);
            assert_eq!(link.params()[0].name, "target");
            let encoded = link
                .build_args([(
                    "target".to_string(),
                    tilo_actions::ArgValue::Text("https://example.com/guide".into()),
                )])
                .unwrap();
            let action: RichTextAction = serde_json::from_slice(&encoded.payload).unwrap();
            action.apply(&mut state.doc).unwrap();
            assert!(body_blocks(&state.doc)[0].marks.iter().any(|mark| {
                mark.name == "link"
                    && mark.start == 0
                    && mark.end == 4
                    && mark.value.as_str() == Some("https://example.com/guide")
            }));
        }

        // Phase: hyperlink_targets_are_limited_to_safe_authored_schemes.
        {
            for target in [
                "https://example.com/guide?q=writer#links",
                "mailto:writer@example.com",
                "#introduction",
                "#_heading_actor-42",
            ] {
                assert!(valid_hyperlink_target(target), "expected valid: {target}");
            }
            for target in [
                "",
                " https://example.com",
                "http://example.com",
                "javascript:alert(1)",
                "file:///etc/passwd",
                "https://user:secret@example.com",
                "mailto:not-an-address",
                "#",
                "#two words",
            ] {
                assert!(
                    !valid_hyperlink_target(target),
                    "expected refusal: {target}"
                );
            }
        }

        // Phase: hyperlink_validation_is_enforced_by_both_resource_and_executor.
        {
            let (mut state, ids) = seeded("Read this\n");
            let block = ids[0].clone();
            let span = Span {
                block: block.clone(),
                start: 0,
                end: 4,
            };
            let ctx = WpResourceCtx {
                format_spans: vec![span.clone()],
                selection: None,
                region: RegionKind::Body,
                caret_block: block,
                caret_offset: 4,
                target_blocks: vec![ids[0].clone()],
                search_hits: Vec::new(),
                content: tilo_content::ContentStore::memory(),
            };
            let root = provide_resources(&ctx, &state.doc).pop().unwrap();
            let link = root
                .actions
                .iter()
                .find(|action| action.id == "set-hyperlink")
                .expect("selected text exposes link authoring");
            let encoded = link
                .build_args([(
                    "target".to_string(),
                    tilo_actions::ArgValue::Text("javascript:alert(1)".into()),
                )])
                .unwrap();
            assert_eq!(
                encoded.type_id,
                tilo_actions::resource::INVALID_ARGUMENTS_TYPE,
                "the prompt reports invalid input before dispatch"
            );

            let action = set_hyperlink_action(RegionKind::Body, vec![span], "javascript:alert(1)");
            assert!(matches!(
                action.apply(&mut state.doc),
                Err(ActionError::Refused(_))
            ));
            assert!(
                body_blocks(&state.doc)[0]
                    .marks
                    .iter()
                    .all(|mark| mark.name != "link"),
                "a refused target leaves the document untouched"
            );
        }
    }

    /// Place-image consumes a published content hash; image object actions are atomic, replayable and
    /// refuse stale targets; review entities are resources whose decisions replay identically; comment
    /// replies replay and resolve cleans the thread, whose tombstone wins over a divergent reply.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn image_and_review_actions_replay_atomically() {
        // Phase: place_image_consumes_a_published_content_hash.
        // The hand-off operation consumes bytes an earlier action actually put
        // in the content store. The model supplies neither the hash nor image
        // metadata; the app resolves both while building its own document action.
        {
            let (mut state, ids) = seeded("Intro\n");
            let block = ids[0].clone();
            let ctx = WpResourceCtx {
                caret_block: block,
                caret_offset: 0,
                target_blocks: Vec::new(),
                format_spans: Vec::new(),
                selection: None,
                region: RegionKind::Body,
                search_hits: Vec::new(),
                content: tilo_content::ContentStore::memory(),
            };
            let bytes = b"content supplied by a preceding action";
            let hash = ctx.content.put(bytes).expect("test fixture fits the store");
            let resources = provide_resources(&ctx, &state.doc);
            let descriptor = tilo_actions::door::survey(&resources)
                .into_iter()
                .find(|descriptor| descriptor.address.action == "place-image")
                .expect("the document publishes its content hand-off action");
            assert!(descriptor.input.wants_content());

            let arguments = serde_json::Map::from_iter([(
                tilo_actions::PROMPTED_PARAM.to_string(),
                serde_json::Value::String(hash.clone()),
            )]);
            tilo_actions::door::invoke_json(
                &mut state.doc,
                &resources,
                &descriptor.address,
                &arguments,
                tilo_actions::door::InvocationProvenance::caller(b"agent"),
                &registry(),
                &mut LocalTransport::new(),
            )
            .expect("the app resolves and places available content");

            assert!(body_blocks(&state.doc).iter().any(|block| {
                block.kind == "image"
                    && block.attrs.get("blob") == Some(&tilo_doc::AttrVal::Str(hash.clone()))
            }));
        }

        // Phase: image_object_actions_are_atomic_replayable_and_refuse_stale_targets.
        {
            let (mut a, ids) = seeded("Object\n");
            let block = ids[0].clone();
            dispatch(
                &mut a.doc,
                &insert_image_action(&block, 0, test_image(Some(40), Some(30))),
            );
            let image = body_blocks(&a.doc)
                .into_iter()
                .find(|view| view.kind == "image")
                .expect("inserted image")
                .id;
            let bytes = a.save();
            let mut b = ui::State::load(&bytes).expect("fork document");
            let actions = [
                set_image_size_action(&image, 120, 90),
                set_image_crop_action(&image, 500, 250, 500, 250),
                set_image_placement_action(&image, "page", "none", 24, 36, 0),
            ];
            for action in &actions {
                let envelope = tilo_actions::apply_local(&mut a.doc, action, encode(action))
                    .expect("apply object action");
                tilo_actions::replay(&mut b.doc, &envelope, &registry())
                    .expect("replay object action");
            }
            assert_eq!(
                a.doc.get_heads(),
                b.doc.get_heads(),
                "object actions replay byte-identically"
            );
            let image_view = body_blocks(&a.doc)
                .into_iter()
                .find(|view| view.id == image)
                .expect("image remains");
            assert_eq!(
                image_view.attrs.get("crop_left"),
                Some(&tilo_doc::AttrVal::Int(500))
            );
            assert_eq!(
                image_view.attrs.get("offset_x"),
                Some(&tilo_doc::AttrVal::Int(24))
            );

            let before = a.save();
            let stale = set_image_crop_action("already-removed", 0, 0, 0, 0);
            assert!(matches!(
                stale.apply(&mut a.doc),
                Err(ActionError::Refused(_))
            ));
            assert_eq!(a.save(), before, "refusal has no partial mutation");
        }

        // Phase: review_entities_are_resources_and_decisions_replay_identically.
        {
            let (mut a, ids) = seeded("Review this sentence\n");
            let block = ids[0].clone();
            let c = tilo_doc::model::open(&a.doc, &automerge::ROOT).unwrap();
            let mut id_gen = tilo_doc::IdGen::new(a.doc.get_actor().to_hex_string());
            let (comment, change) = {
                let mut dm = tilo_doc::DocMut {
                    doc: &mut a.doc,
                    c: &c,
                    ids: &mut id_gen,
                };
                (
                    dm.add_comment(&block, 0, 6, "Maya", "Please clarify this."),
                    dm.track_insert(&block, 20, " now", "Jonas", "2026-09-05"),
                )
            };
            a.doc.commit();
            let bytes = a.save();
            let mut b = ui::State::load(&bytes).unwrap();

            let ctx = WpResourceCtx {
                caret_block: block,
                caret_offset: 0,
                target_blocks: Vec::new(),
                format_spans: Vec::new(),
                selection: None,
                region: RegionKind::Body,
                search_hits: Vec::new(),
                content: tilo_content::ContentStore::memory(),
            };
            let resources = provide_resources(&ctx, &a.doc);
            let review = resources[0]
                .children
                .iter()
                .find(|resource| resource.id.as_str() == "word-processor.review")
                .expect("review group is navigable");
            let comment_resource_id = format!("word-processor.comment:{comment}");
            let comment_resource = review
                .children
                .iter()
                .find(|resource| resource.id.as_str() == comment_resource_id.as_str())
                .expect("comment is a resource");
            assert!(
                comment_resource
                    .actions
                    .iter()
                    .any(|action| action.id == "resolve")
            );
            let change_resource_id = format!("word-processor.change:{change}");
            let change_resource = review
                .children
                .iter()
                .find(|resource| resource.id.as_str() == change_resource_id.as_str())
                .expect("tracked change is a resource");
            let accept = spec_encoding(
                change_resource
                    .actions
                    .iter()
                    .find(|action| action.id == "accept")
                    .unwrap(),
                None,
            );
            let action: RichTextAction = serde_json::from_slice(&accept.payload).unwrap();
            let envelope = tilo_actions::apply_local(&mut a.doc, &action, accept).unwrap();
            tilo_actions::replay(&mut b.doc, &envelope, &registry()).unwrap();
            assert_eq!(a.doc.get_heads(), b.doc.get_heads());
            let snap = tilo_doc::model::materialize(
                &a.doc,
                &tilo_doc::model::open(&a.doc, &automerge::ROOT).unwrap(),
            );
            assert!(snap.changes.is_empty(), "accepted change left review state");
            assert_eq!(snap.comments.len(), 1, "unrelated comment remains");
        }

        // Phase: durable_comment_reply_action_replays_and_resolve_cleans_the_thread.
        {
            let (mut a, ids) = seeded("Review this paragraph\n");
            let block = ids[0].clone();
            let containers = tilo_doc::model::open(&a.doc, &automerge::ROOT).unwrap();
            let comment = {
                let mut ids = tilo_doc::model::seeded_id_gen(&a.doc, &containers);
                let mut dm = DocMut {
                    doc: &mut a.doc,
                    c: &containers,
                    ids: &mut ids,
                };
                dm.add_comment(&block, 0, 6, "Maya", "Please clarify")
            };
            a.doc.commit();
            let bytes = a.save();
            let mut b = ui::State::load(&bytes).unwrap();

            let reply = reply_to_comment_action(&comment, "Jonas", "2026-09-07", "Will do.");
            let envelope = tilo_actions::apply_local(&mut a.doc, &reply, encode(&reply)).unwrap();
            tilo_actions::replay(&mut b.doc, &envelope, &registry()).unwrap();
            assert_eq!(
                a.doc.get_heads(),
                b.doc.get_heads(),
                "reply replays exactly"
            );
            let c = tilo_doc::model::open(&a.doc, &automerge::ROOT).unwrap();
            let comments = tilo_doc::model::materialize_comments(&a.doc, &c);
            assert_eq!(comments[0].replies.len(), 1);
            assert_eq!(comments[0].replies[0].author, "Jonas");
            assert_eq!(comments[0].replies[0].blocks[0].text, "Will do.");

            let resolve = RichTextAction::ResolveComment { id: comment };
            let envelope =
                tilo_actions::apply_local(&mut a.doc, &resolve, encode(&resolve)).unwrap();
            tilo_actions::replay(&mut b.doc, &envelope, &registry()).unwrap();
            assert_eq!(
                a.doc.get_heads(),
                b.doc.get_heads(),
                "resolution replays exactly"
            );
            let c = tilo_doc::model::open(&a.doc, &automerge::ROOT).unwrap();
            assert!(tilo_doc::model::materialize_comments(&a.doc, &c).is_empty());
        }

        // Phase: resolved_comment_tombstone_wins_over_divergent_reply.
        {
            let (mut a, ids) = seeded("Review this paragraph\n");
            let block = ids[0].clone();
            let add = add_comment_action(&block, 0, 6, "Maya", "2026-09-07", "Please clarify");
            dispatch(&mut a.doc, &add);
            let c = tilo_doc::model::open(&a.doc, &automerge::ROOT).unwrap();
            let comment = tilo_doc::model::comment_ids(&a.doc, &c)
                .into_iter()
                .next()
                .unwrap();
            let bytes = a.save();
            let mut b = ui::State::load(&bytes).unwrap();
            b.doc.set_actor(automerge::ActorId::random());

            let resolve = RichTextAction::ResolveComment {
                id: comment.clone(),
            };
            let reply = reply_to_comment_action(&comment, "Jonas", "2026-09-08", "Will do");
            tilo_actions::apply_local(&mut a.doc, &resolve, encode(&resolve)).unwrap();
            tilo_actions::apply_local(&mut b.doc, &reply, encode(&reply)).unwrap();
            a.doc.merge_incoming(&mut b.doc).unwrap();

            let c = tilo_doc::model::open(&a.doc, &automerge::ROOT).unwrap();
            assert!(tilo_doc::model::comment_is_tombstoned(&a.doc, &c, &comment));
            assert!(
                tilo_doc::model::materialize_comments(&a.doc, &c).is_empty(),
                "a reply from a divergent replica cannot resurrect a resolved thread"
            );
        }
    }

    /// Cross-block selection replacement is one replayable action; an encoded split and concurrent splice
    /// converge; style fields merge granularly with the tombstone winning; TOC fields, rich paste and table
    /// edits replay identically; a table tombstone wins over a divergent cell edit.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn edits_encode_replay_and_converge_on_divergent_replicas() {
        // Phase: cross_block_selection_replacement_is_one_replayable_action.
        {
            let (mut a, ids) = seeded("alpha\n\nbeta\n\ngamma\n");
            let bytes = a.save();
            let mut b = ui::State::load(&bytes).unwrap();
            let action = replace_selection_action(RegionKind::Body, &ids[0], 2, &ids[2], 3, "X");
            let encoded = encode(&action);
            let envelope = tilo_actions::apply_local(&mut a.doc, &action, encoded).unwrap();
            tilo_actions::replay(&mut b.doc, &envelope, &registry()).unwrap();
            assert_eq!(a.doc.get_heads(), b.doc.get_heads());
            let c = tilo_doc::model::open(&a.doc, &automerge::ROOT).unwrap();
            let blocks = tilo_doc::model::materialize(&a.doc, &c).blocks;
            assert_eq!(blocks.len(), 1);
            assert_eq!(blocks[0].text, "alXma");
        }

        // Phase: encoded_split_and_concurrent_splice_converge_on_divergent_replicas.
        {
            let (mut a, ids) = seeded("alpha beta\n");
            let bytes = a.save();
            let mut b = ui::State::load(&bytes).unwrap();
            b.doc.set_actor(automerge::ActorId::random());
            let block = ids[0].clone();

            let split = split_block_action(RegionKind::Body, &block, 5, "paragraph", None);
            let encoded_split = encode(&split);
            let decoded_split: RichTextAction =
                serde_json::from_slice(&encoded_split.payload).unwrap();
            tilo_actions::apply_local(&mut a.doc, &decoded_split, encoded_split).unwrap();

            let splice = splice_text_action(RegionKind::Body, &block, 0, 0, "Shared ");
            let encoded_splice = encode(&splice);
            let decoded_splice: RichTextAction =
                serde_json::from_slice(&encoded_splice.payload).unwrap();
            tilo_actions::apply_local(&mut b.doc, &decoded_splice, encoded_splice).unwrap();

            let mut merged_a = a.doc.clone();
            let mut b_for_a = b.doc.clone();
            merged_a.merge_incoming(&mut b_for_a).unwrap();
            let mut merged_b = b.doc.clone();
            let mut a_for_b = a.doc.clone();
            merged_b.merge_incoming(&mut a_for_b).unwrap();
            assert_eq!(
                body_blocks(&merged_a),
                body_blocks(&merged_b),
                "encoded actions converge regardless of merge direction"
            );
        }

        // Phase: style_fields_merge_granularly_and_style_tombstone_wins.
        {
            let (mut base, ids) = seeded("Styled paragraph\n");
            let create = set_style_property_action(
                "custom1",
                tilo_doc::schema::STYLE_NAME,
                AttrVal::Str("Callout".into()),
            );
            dispatch(&mut base.doc, &create);
            dispatch(
                &mut base.doc,
                &set_block_attr_action(
                    RegionKind::Body,
                    vec![ids[0].clone()],
                    "style",
                    AttrVal::Str("custom1".into()),
                ),
            );
            let bytes = base.save();
            let mut a = ui::State::load(&bytes).unwrap();
            let mut b = ui::State::load(&bytes).unwrap();
            b.doc.set_actor(automerge::ActorId::random());

            let bold = set_style_property_action(
                "custom1",
                tilo_doc::schema::STYLE_BOLD,
                AttrVal::Bool(true),
            );
            let size = set_style_property_action(
                "custom1",
                tilo_doc::schema::STYLE_SIZE,
                AttrVal::Float(28.0),
            );
            tilo_actions::apply_local(&mut a.doc, &bold, encode(&bold)).unwrap();
            tilo_actions::apply_local(&mut b.doc, &size, encode(&size)).unwrap();
            a.doc.merge_incoming(&mut b.doc).unwrap();
            let c = tilo_doc::model::open(&a.doc, &automerge::ROOT).unwrap();
            let properties = tilo_doc::model::style_props(&a.doc, &c, "custom1");
            assert_eq!(
                properties.get(tilo_doc::schema::STYLE_BOLD),
                Some(&tilo_doc::AttrVal::Bool(true))
            );
            assert_eq!(
                properties.get(tilo_doc::schema::STYLE_SIZE),
                Some(&tilo_doc::AttrVal::Float(28.0))
            );

            let bytes = a.save();
            let mut deleted = ui::State::load(&bytes).unwrap();
            let mut divergent = ui::State::load(&bytes).unwrap();
            divergent.doc.set_actor(automerge::ActorId::random());
            let remove = delete_style_action("custom1", vec![ids[0].clone()]);
            let rename = set_style_property_action(
                "custom1",
                tilo_doc::schema::STYLE_NAME,
                AttrVal::Str("Late rename".into()),
            );
            tilo_actions::apply_local(&mut deleted.doc, &remove, encode(&remove)).unwrap();
            tilo_actions::apply_local(&mut divergent.doc, &rename, encode(&rename)).unwrap();
            deleted.doc.merge_incoming(&mut divergent.doc).unwrap();
            let c = tilo_doc::model::open(&deleted.doc, &automerge::ROOT).unwrap();
            assert!(tilo_doc::model::style_is_tombstoned(
                &deleted.doc,
                &c,
                "custom1"
            ));
            assert!(!tilo_doc::model::style_ids(&deleted.doc, &c).contains(&"custom1".to_string()));
        }

        // Phase: toc_field_rich_paste_and_table_edits_encode_and_replay_identically.
        {
            let (mut origin, ids) = seeded("# Heading\n\nBody\n");
            let bytes = origin.save();
            let mut replay = ui::State::load(&bytes).unwrap();
            let heading = ids[0].clone();
            let body = ids[1].clone();
            let fragment = crate::word_processor::ClipBlock {
                kind: "heading".into(),
                text: "Pasted title".into(),
                marks: vec![tilo_doc::document::MarkSpan {
                    name: "strong".into(),
                    start: 0,
                    end: 6,
                    value: tilo_doc::document::MarkValue::Bool(true),
                }],
                level: Some(2),
                ordered: None,
                align: Some("center".into()),
                attrs: std::collections::BTreeMap::default(),
            };
            let actions = [
                generate_toc_action(vec![TocTarget {
                    heading,
                    level: 1,
                    text: "Heading".into(),
                    page: Some(1),
                }]),
                insert_page_number_action(RegionKind::Body, &body, 0),
                paste_fragment_action(
                    RegionKind::Body,
                    SelectionTarget {
                        start_block: body.clone(),
                        start: 1,
                        end_block: body.clone(),
                        end: 1,
                    },
                    vec![fragment],
                    false,
                ),
                insert_table_action(&body, 2, 2),
            ];
            for action in actions {
                let encoded = encode(&action);
                let decoded: RichTextAction = serde_json::from_slice(&encoded.payload).unwrap();
                let envelope =
                    tilo_actions::apply_local(&mut origin.doc, &decoded, encoded).unwrap();
                tilo_actions::replay(&mut replay.doc, &envelope, &registry()).unwrap();
            }
            let table = body_blocks(&origin.doc)
                .iter()
                .find_map(|block| match block.attrs.get("table") {
                    Some(tilo_doc::AttrVal::Str(id)) => Some(id.clone()),
                    _ => None,
                })
                .unwrap();
            for edit in [
                TableEdit::InsertRow { at: 1 },
                TableEdit::InsertColumn { at: 1 },
                TableEdit::DeleteRow { row: 0 },
                TableEdit::DeleteColumn { column: 0 },
            ] {
                let action = edit_table_action(&table, edit);
                let encoded = encode(&action);
                let decoded: RichTextAction = serde_json::from_slice(&encoded.payload).unwrap();
                let envelope =
                    tilo_actions::apply_local(&mut origin.doc, &decoded, encoded).unwrap();
                tilo_actions::replay(&mut replay.doc, &envelope, &registry()).unwrap();
            }
            assert_eq!(origin.doc.get_heads(), replay.doc.get_heads());
            assert_eq!(body_blocks(&origin.doc), body_blocks(&replay.doc));
        }

        // Phase: table_tombstone_wins_over_a_divergent_cell_edit.
        {
            let (mut base, ids) = seeded("Anchor\n");
            dispatch(&mut base.doc, &insert_table_action(&ids[0], 2, 2));
            let blocks = body_blocks(&base.doc);
            let cell = blocks.iter().find(|block| block.kind == "cell").unwrap();
            let table = match cell.attrs.get("table") {
                Some(tilo_doc::AttrVal::Str(id)) => id.clone(),
                other => panic!("cell has stable table id: {other:?}"),
            };
            let bytes = base.save();
            let mut deleted = ui::State::load(&bytes).unwrap();
            let mut divergent = ui::State::load(&bytes).unwrap();
            divergent.doc.set_actor(automerge::ActorId::random());

            let remove = edit_table_action(&table, TableEdit::Delete);
            let late_edit = splice_text_action(RegionKind::Body, &cell.id, 0, 0, "late");
            tilo_actions::apply_local(&mut deleted.doc, &remove, encode(&remove)).unwrap();
            tilo_actions::apply_local(&mut divergent.doc, &late_edit, encode(&late_edit)).unwrap();

            let mut merged_a = deleted.doc.clone();
            let mut divergent_for_a = divergent.doc.clone();
            merged_a.merge_incoming(&mut divergent_for_a).unwrap();
            let mut merged_b = divergent.doc.clone();
            let mut deleted_for_b = deleted.doc.clone();
            merged_b.merge_incoming(&mut deleted_for_b).unwrap();
            assert_eq!(body_blocks(&merged_a), body_blocks(&merged_b));
            assert!(body_blocks(&merged_a)
                .iter()
                .all(|block| !matches!(block.attrs.get("table"), Some(tilo_doc::AttrVal::Str(id)) if id == &table)));
        }
    }

    /// Typing, same-block replacement and formatting neither materialize nor
    /// scan the block order when validated and applied.
    #[test]
    fn keystroke_actions_do_not_scan_block_order() {
        let (mut state, ids) = seeded("alpha\n\nbeta\n");
        for action in [
            splice_text_action(RegionKind::Body, &ids[0], 1, 1, "x"),
            replace_selection_action(RegionKind::Body, &ids[1], 0, &ids[1], 2, "B"),
            set_mark_action(
                RegionKind::Body,
                vec![Span {
                    block: ids[0].clone(),
                    start: 0,
                    end: 2,
                }],
                "em",
                Some(MarkVal::Bool(true)),
                vec![],
            ),
        ] {
            let scans = tilo_doc::model::ordered_keys_scan_count();
            dispatch(&mut state.doc, &action);
            assert_eq!(
                tilo_doc::model::ordered_keys_scan_count(),
                scans,
                "{action:?}"
            );
        }
        assert_eq!(text_of(&state.doc, &ids[0]), "axlpha");
        assert_eq!(text_of(&state.doc, &ids[1]), "Bta");
    }

    /// A selection in `block` plus more search hits than discovery may list.
    fn overflowing_resource_ctx(block: &str) -> WpResourceCtx {
        WpResourceCtx {
            caret_block: block.to_owned(),
            caret_offset: 0,
            target_blocks: vec![block.to_owned()],
            format_spans: Vec::new(),
            selection: Some(SelectionTarget {
                start_block: block.to_owned(),
                start: 0,
                end_block: block.to_owned(),
                end: 4,
            }),
            region: RegionKind::Body,
            search_hits: (0..MAX_RESOURCE_SEARCH_HITS + 8)
                .map(|start| SearchTarget {
                    block: block.to_owned(),
                    start,
                    end: start + 1,
                })
                .collect(),
            content: tilo_content::ContentStore::memory(),
        }
    }

    fn root_for(state: &ui::State, block: &str) -> Resource {
        let ctx = WpResourceCtx {
            format_spans: Vec::new(),
            selection: None,
            region: RegionKind::Body,
            caret_block: block.to_string(),
            caret_offset: 0,
            target_blocks: vec![block.to_string()],
            search_hits: Vec::new(),
            content: tilo_content::ContentStore::memory(),
        };
        provide_resources(&ctx, &state.doc).pop().unwrap()
    }

    fn text_arg(text: &str) -> [(String, tilo_actions::ArgValue); 1] {
        [(
            "text".to_string(),
            tilo_actions::ArgValue::Text(text.into()),
        )]
    }

    #[test]
    fn body_append_operations_are_root_level_pure_document_writes() {
        let (state, ids) = seeded("Intro\n");
        let root = root_for(&state, &ids[0]);
        assert_eq!(root.id.0, "word-processor.document");
        for id in ["append-paragraph", "continue-paragraph"] {
            let spec = root
                .actions
                .iter()
                .find(|action| action.id == id)
                .unwrap_or_else(|| panic!("{id} is on the root resource"));
            assert_eq!(spec.params().len(), 1);
            assert_eq!(spec.params()[0].name, "text");
            assert!(spec.params()[0].required);
            assert_eq!(spec.kind, OperationKind::Document);
            assert_eq!(spec.locus, ExecutionLocus::ClientLocal);
            assert!(matches!(
                spec.contract.confirmation,
                tilo_actions::ConfirmationPolicy::None
            ));
            assert!(spec.contract.reversible);
            // append-paragraph streams: its first part appends, the rest is
            // typed at the caret it leaves. continue-paragraph is deprecated.
            let expected: &[&str] = if id == "continue-paragraph" {
                &["tilo.deprecated:type-text"]
            } else {
                &["tilo.incremental-text:text:type-text"]
            };
            assert_eq!(spec.contract.tags, expected, "{id}");
            assert!(
                spec.contract.validate_declaration().is_empty(),
                "{:?}",
                spec.contract.validate_declaration()
            );
            assert!(spec.has_continuation_identity());
            // Empty text is refused before anything is encoded.
            let refused = spec.build_args(text_arg(""));
            assert!(
                refused.is_err()
                    || refused
                        .as_ref()
                        .is_ok_and(|encoded| encoded.type_id
                            == tilo_actions::resource::INVALID_ARGUMENTS_TYPE),
                "{id}: {refused:?}"
            );
        }
    }

    /// The host offers every Document operation of its workers as one grant
    /// set, and withdraws the offer above 64. A fresh document's catalogue,
    /// with both paragraph operations, stays well inside that bound.
    #[test]
    fn a_fresh_document_publishes_few_grantable_document_operations() {
        fn document_ops(resource: &Resource, out: &mut Vec<String>) {
            for action in &resource.actions {
                if action.kind == OperationKind::Document {
                    out.push(format!("{}#{}", resource.id.0, action.id));
                }
            }
            for child in &resource.children {
                document_ops(child, out);
            }
        }
        let (state, ids) = seeded("");
        let block = ids.first().cloned().unwrap_or_default();
        let mut ops = Vec::new();
        document_ops(&root_for(&state, &block), &mut ops);
        assert!(ops.contains(&"word-processor.document#append-paragraph".to_owned()));
        assert!(ops.contains(&"word-processor.document#continue-paragraph".to_owned()));
        assert!(
            ops.len() <= 32,
            "{} document operations: {ops:?}",
            ops.len()
        );
    }

    #[test]
    fn append_then_repeated_continue_builds_one_paragraph_in_order() {
        let (mut state, ids) = seeded("Intro\n");
        // The catalogue is captured once, before any write; every invocation
        // resolves the end of the body when it is applied.
        let root = root_for(&state, &ids[0]);
        let op = |id: &str| {
            root.actions
                .iter()
                .find(|action| action.id == id)
                .unwrap()
                .clone()
        };
        let (append, cont) = (op("append-paragraph"), op("continue-paragraph"));
        for (spec, text) in [
            (&append, "The ocean "),
            (&cont, "covers most "),
            (&cont, "of the "),
            (&cont, "planet."),
        ] {
            let encoded = spec.build_args(text_arg(text)).unwrap();
            let action: RichTextAction = serde_json::from_slice(&encoded.payload).unwrap();
            dispatch(&mut state.doc, &action);
        }
        let blocks = body_blocks(&state.doc);
        assert_eq!(blocks.len(), 2, "one new paragraph after the intro");
        assert_eq!(blocks[0].text, "Intro");
        assert_eq!(blocks[1].text, "The ocean covers most of the planet.");
        assert_eq!(blocks[1].kind, "paragraph");
    }

    #[test]
    fn append_fills_an_empty_trailing_paragraph_and_empty_text_is_refused() {
        let (mut state, _) = seeded("");
        let before = body_blocks(&state.doc);
        dispatch(&mut state.doc, &append_paragraph_action("Hello"));
        dispatch(&mut state.doc, &continue_paragraph_action(", world"));
        let blocks = body_blocks(&state.doc);
        assert!(blocks.len() <= before.len().max(1));
        assert_eq!(blocks.last().unwrap().text, "Hello, world");
        for action in [append_paragraph_action(""), continue_paragraph_action("")] {
            assert!(
                matches!(action.apply(&mut state.doc), Err(ActionError::Refused(_))),
                "{action:?}"
            );
        }
        assert_eq!(body_blocks(&state.doc).last().unwrap().text, "Hello, world");
    }

    #[test]
    fn continue_counts_characters_like_splice_text() {
        let (mut state, _) = seeded("Aé🦀\n");
        dispatch(&mut state.doc, &continue_paragraph_action("z"));
        assert_eq!(body_blocks(&state.doc)[0].text, "Aé🦀z");
    }
}
