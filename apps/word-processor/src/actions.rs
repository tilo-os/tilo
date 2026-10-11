//! The word processor's exported **actions** and the **resource tree** it
//! publishes to Ctrl+K.
//!
//! Every document-mutating operation is a [`RichTextAction`] from qdoc's
//! rich-text vocabulary (re-exported here with its builders). The same
//! [`EncodedAction`] is what toolbar/panel/menu affordances dispatch and what
//! Ctrl+K dispatches — one source of truth, one replay path. This module adds
//! what is the word processor's own: its application operations (open, save,
//! zoom, panels) and the resource tree it publishes.

/// Edits bound to a view's caret and selection, as the document's typed
/// declarations.
pub(crate) mod bound;
pub(crate) mod continuation;
/// Pure editor input primitives shared by keyboard, palette and agents.
pub mod editing;
/// Per-participant view state and its pure resource-context projection.
pub mod participant;
/// Per-participant view operations (caret, selection, Find) and their
/// published `word-processor.view-operation` specs.
pub mod view_ops;

use automerge::AutoCommit;
use std::collections::HashSet;

use crate::num;

use tilo_actions::{
    Action, ActionArgs, ActionParam, ActionRegistry, ActionSpec, ContextRequirement, EncodedAction,
    ExecutionLocus, OperationKind, ParamType, Resource, ResultSpec,
};
use ui::tilo_resource::Effect as _;

use crate::effects::WordProcessorEffect;

/// Registry `type_id` for every word-processor action.
pub use tilo_document::RICH_TEXT_ACTION_TYPE as WORD_PROCESSOR_ACTION_TYPE;
pub use tilo_document::action::*;
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
        .requires(ContextRequirement::Selection)
        .requiring(tilo_actions::PermissionRequirement::Comment),
        "add-comment.v1",
        domain,
    ));
}

/// What the word processor's framework app publishes for a view at `ctx` on
/// `doc`: the document, its declared actions, effects and children, with the
/// edits bound to the view's caret and selection beside them, and the view's
/// Find results. For a peer in a test that serves
/// a Writer document without a window.
#[cfg(any(test, feature = "test-support"))]
pub fn published_for(ctx: &WpResourceCtx, doc: &AutoCommit) -> Vec<Resource> {
    use ui::tilo_resource::Resource as _;
    let Ok(state) = crate::framework::Document::read(doc) else {
        return Vec::new();
    };
    let label = <crate::framework::WordProcessorApp as ui::framework::ResourceApp>::label(&state);
    let mut root = ui::tilo_resource::publish::<crate::framework::Document>(
        crate::framework::Document::KIND,
        &label,
        &state,
    );
    let view = live_presentation_resources(ctx, doc, None);
    for decl in bound::declarations(&view) {
        root = ui::tilo_resource::bind_declaration(root, &decl);
    }
    for spec in crate::effects::specs() {
        if !root.actions.iter().any(|existing| existing.id == spec.id) {
            root = root.with_action(spec);
        }
    }
    let mut published = vec![root];
    published.extend(
        search_rows(ctx, doc)
            .into_iter()
            .map(ui::framework::LiveRow::into_resource),
    );
    published
}

/// The edits a view's caret and selection offer — typing, marks, placing an
/// image, a table, a page break, replacing or commenting the selection — as
/// operations whose builders capture where they apply; the document
/// publishes them as declarations bound to that view
/// ([`bound::declarations`]).
pub(crate) fn live_presentation_resources(
    ctx: &WpResourceCtx,
    doc: &AutoCommit,
    body: Option<&tilo_document::BodyOrder>,
) -> Vec<Resource> {
    let mut scratch = Resource::new("word-processor.live", "word_processor.live", "Live");
    // Pass the kept body order so building erase/new-paragraph targets does not
    // list and sort every paragraph on each keystroke.
    add_selection_resources(&mut scratch, ctx, editing::large_selection(ctx, doc, body));
    add_comment_resource_action(&mut scratch, ctx);
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
    let mut caret = Resource::new("word-processor.caret", "word_processor.caret", "Caret");
    editing::add_operations(&mut caret, ctx, doc, body);
    add_document_edit_actions(&mut caret, ctx);
    add_caret_actions(&mut caret, ctx);
    add_body_append_actions(&mut caret, ctx, doc, body);
    // Hosted edits answer with the blocks they changed, same as the
    // standalone document root (see returning_changes).
    caret = returning_changes(caret);
    if !caret.actions.is_empty() {
        extras.push(caret);
    }
    extras
}

/// The Find results of the view `ctx` describes, as live rows beneath the
/// document: each goes to its range in the invoker's own view.
pub(crate) fn search_rows(ctx: &WpResourceCtx, doc: &AutoCommit) -> Vec<ui::framework::LiveRow> {
    if ctx.search_hits.is_empty() {
        return Vec::new();
    }
    let Ok(containers) = tilo_doc::model::open(doc, &automerge::ROOT) else {
        return Vec::new();
    };
    let hits: Vec<String> = ctx
        .search_hits
        .iter()
        .take(MAX_RESOURCE_SEARCH_HITS)
        .map(|hit| hit.block.clone())
        .collect();
    let blocks = tilo_doc::model::materialize_blocks(doc, &containers, &hits);
    let mut search = ui::framework::LiveRow::new(
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
        let label = range_label(&blocks, &hit.block, hit.start, hit.end)
            .unwrap_or_else(|| format!("Result {}", result_index + 1));
        search = search.row(
            ui::framework::LiveRow::new(
                format!(
                    "word-processor.search-hit:{}:{}:{}",
                    hit.block, hit.start, hit.end
                ),
                "word_processor.search-hit",
                label,
            )
            .operation(
                WordProcessorEffect::GoTo {
                    block: hit.block.clone(),
                    start: hit.start,
                    end: hit.end,
                }
                .bound()
                .requires(ContextRequirement::FindHits),
            ),
        );
    }
    vec![search]
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
    if let Some((block, start, end)) = &comment.range {
        resource.actions.push(
            WordProcessorEffect::GoToAnchor {
                block: block.clone(),
                start: *start,
                end: *end,
            }
            .bound(),
        );
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
        )
        .requiring(tilo_actions::PermissionRequirement::Comment),
        "reply-to-comment.v1",
        domain,
    ));
    resource
}

fn place_child(root: &mut Resource, child: Resource) {
    if let Some(existing) = root
        .children
        .iter_mut()
        .find(|existing| existing.id == child.id)
    {
        *existing = child;
    } else {
        root.children.push(child);
    }
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
    place_child(root, review);
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
        place_child(root, notes);
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
        place_child(root, bookmarks);
    }
}

fn add_region_resources(
    root: &mut Resource,
    id: &str,
    label: &str,
    region: RegionKind,
    blocks: &[tilo_doc::snapshot::BlockView],
) {
    if blocks.is_empty() {
        return;
    }
    let mut region_resource = Resource::new(
        format!("word-processor.{id}"),
        format!("word_processor.{id}"),
        label,
    );
    for block in blocks.iter().take(MAX_RESOURCE_OBJECTS) {
        let mut child = Resource::new(
            format!("word-processor.{id}:{}", block.id),
            format!("word_processor.{id}-block"),
            short(&block.text, label),
        );
        for (action_id, action_label, align) in [
            ("align-left", "Align left", "left"),
            ("align-center", "Align centre", "center"),
            ("align-right", "Align right", "right"),
        ] {
            child.actions.push(immediate_spec(
                action_id,
                action_label,
                &set_block_attr_action(
                    region,
                    vec![block.id.clone()],
                    "align",
                    AttrVal::Str(align.to_string()),
                ),
            ));
        }
        region_resource.children.push(child);
    }
    place_child(root, region_resource);
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
    place_child(root, styles);
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
    let block_id = block.id.clone();
    resource.actions.push(ActionSpec::typed(
        "set-size",
        "Set image size",
        "width and height in pixels",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required_owned("width", ParamType::Integer, "Width in px"),
            ActionParam::required_owned("height", ParamType::Integer, "Height in px"),
        ],
        move |args: &ActionArgs| {
            let width = args.integer("width").unwrap_or(0);
            let height = args.integer("height").unwrap_or(0);
            encode(&set_image_size_action(&block_id, width, height))
        },
    ));
    let flipped = |key: &str| matches!(block.attrs.get(key), Some(tilo_doc::AttrVal::Bool(true)));
    for (id, label, key) in [
        ("flip-horizontal", "Flip horizontally", "flip_h"),
        ("flip-vertical", "Flip vertically", "flip_v"),
    ] {
        resource.actions.push(immediate_spec(
            id,
            label,
            &set_block_attr_action(
                RegionKind::Body,
                vec![block.id.clone()],
                key,
                AttrVal::Bool(!flipped(key)),
            ),
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
        cell_resource.actions.push(
            WordProcessorEffect::GoTo {
                block: cell.id.clone(),
                start: 0,
                end: cell.text.chars().count(),
            }
            .bound(),
        );
        resource.children.push(cell_resource);
    }
    resource
}

/// Images, tables and headings already on the published document stay that
/// node. A listing may replace one when it carries at least as much (a full
/// table the eager page split). Anything the eager page omitted is added.
fn place_structural(root: &mut Resource, child: Resource) {
    if let Some(existing) = root
        .children
        .iter_mut()
        .find(|existing| existing.id == child.id)
    {
        if child.children.len() >= existing.children.len() {
            *existing = child;
        }
    } else {
        root.children.push(child);
    }
}

fn add_structural_resources(root: &mut Resource, snapshot: &tilo_doc::snapshot::DocSnapshot) {
    for block in snapshot.blocks.iter().filter(|block| block.kind == "image") {
        place_structural(root, image_resource(block));
    }
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
        place_structural(root, table_resource(table_id, &blocks));
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
        place_structural(root, section);
    }
}

/// A table of contents built from the document's headings. Page numbers stay
/// with the laid-out view; this door inserts the headings the document holds.
pub(crate) fn toc_action(state: &tilo_document::RichTextState) -> Option<ActionSpec> {
    let entries: Vec<TocTarget> = state
        .headings()
        .into_iter()
        .map(|heading| TocTarget {
            heading: heading.id,
            level: heading.level,
            text: heading.text,
            page: None,
        })
        .collect();
    if entries.is_empty() {
        return None;
    }
    Some(immediate_spec(
        "generate-toc",
        "Generate table of contents",
        &generate_toc_action(entries),
    ))
}

/// Body paragraphs the document resource reads whole when it is read
/// ([`crate::framework::Document`]), so a document operation can find one by
/// its text without a view. A longer document is read around its views only.
pub(crate) const MAX_FOUND_PARAGRAPHS: usize = MAX_RESOURCE_OBJECTS.saturating_mul(4);

/// The paragraph kinds `insert-image` places an image after.
const TEXT_BLOCKS: &[&str] = &["paragraph", "heading", "list_item"];

/// The body as `insert-image` finds its place in it: each text block's id
/// and text in order, and the last block's id and length.
struct ImagePlaces {
    paragraphs: Vec<(String, String)>,
    end: Option<(String, usize)>,
}

impl ImagePlaces {
    /// `None` for a body `state` did not read whole.
    fn of(state: &tilo_document::RichTextState) -> Option<Self> {
        let len = state.body().len();
        if len > MAX_FOUND_PARAGRAPHS || state.blocks(0..len).count() != len {
            return None;
        }
        let blocks: Vec<_> = state.blocks(0..len).collect();
        Some(Self {
            paragraphs: blocks
                .iter()
                .filter(|block| TEXT_BLOCKS.contains(&block.kind.as_str()))
                .map(|block| (block.id.clone(), block.text.clone()))
                .collect(),
            end: blocks
                .last()
                .map(|block| (block.id.clone(), block.text.chars().count())),
        })
    }

    /// Where the image goes: after the first paragraph containing `after`,
    /// else at the end of the body.
    fn place(&self, after: Option<&str>) -> Result<(String, usize), String> {
        match after {
            Some(after) => self
                .paragraphs
                .iter()
                .find(|(_, text)| text.contains(after))
                .map(|(id, text)| (id.clone(), text.chars().count()))
                .ok_or_else(|| format!("no paragraph contains the text `{after}`")),
            None => self
                .end
                .clone()
                .ok_or_else(|| "the document has no body to insert into".to_string()),
        }
    }
}

/// Insert an image another action produced (its content hash, carried as
/// the prompted content) after a paragraph named by its text, or at the end
/// of the document: a document operation on the record, so a participant
/// without a view or caret (an agent) places a picture too. The paragraphs
/// are captured as `state` holds them; a document longer than
/// [`MAX_FOUND_PARAGRAPHS`] that was not read whole refuses, and a view
/// places at its caret with `place-image`.
pub(crate) fn insert_image_spec(state: &tilo_document::RichTextState) -> ActionSpec {
    let places = ImagePlaces::of(state);
    let refused = |reason: String| EncodedAction {
        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
        payload: reason.into_bytes(),
    };
    let mut spec = ActionSpec::typed(
        "insert-image",
        "Insert image",
        "Image content produced by the preceding action",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required_owned(
                tilo_actions::PROMPTED_PARAM,
                ParamType::Text,
                "Image content produced by the preceding action",
            ),
            ActionParam::optional_owned(
                "after",
                ParamType::Text,
                "Text of the paragraph to insert the image after (the first paragraph containing it); omit to insert at the end of the document",
            ),
        ],
        move |args: &ActionArgs| {
            let value = args.text(tilo_actions::PROMPTED_PARAM).unwrap_or_default();
            let hash = tilo_actions::content_hash(value).unwrap_or(value);
            let Some(bytes) = ui::tilo_resource::content::current()
                .and_then(|content| content.read(hash))
            else {
                return refused(format!("content `{hash}` is not available on this machine"));
            };
            let Some(places) = &places else {
                return refused(format!(
                    "the document is longer than {MAX_FOUND_PARAGRAPHS} paragraphs; place the image at a caret with place-image"
                ));
            };
            let image = ImageInput::of(hash.to_string(), &bytes);
            if !image.is_picture() {
                return refused(format!(
                    "content `{hash}` is {}, not a picture: insert the image an export produced",
                    image.mime
                ));
            }
            let after = args.text("after").filter(|after| !after.trim().is_empty());
            match places.place(after) {
                Ok((block, at)) => encode(&insert_image_action(&block, at, image)),
                Err(reason) => refused(reason),
            }
        },
    )
    .with_description(
        "Insert the image the preceding action produced: after the first paragraph containing the text `after`, or at the end of the document without it",
    )
    .requiring(tilo_actions::PermissionRequirement::Write)
    .reversible();
    // The image is carried from the step before, as `place-image` takes it.
    if let tilo_actions::ActionInput::Prompted { collects, .. } = &mut spec.input {
        *collects = tilo_actions::Collects::Content;
    }
    spec
}

/// Images, tables, headings, notes, review, paragraph styles, and the page
/// header and footer the document resource publishes as its own children.
/// Structural objects are bounded in document order; a caret listing adds
/// only what this page omitted.
pub(crate) fn structural_children(state: &tilo_document::RichTextState) -> Vec<Resource> {
    let mut blocks = Vec::new();
    for (kind, limit) in [
        ("heading", MAX_RESOURCE_OBJECTS),
        ("image", MAX_RESOURCE_OBJECTS),
        ("cell", MAX_RESOURCE_OBJECTS.saturating_mul(16)),
    ] {
        let mut of_kind = state.featured(kind);
        of_kind.truncate(limit);
        blocks.extend(of_kind.into_iter().map(|view| (*view).clone()));
    }
    let snapshot = tilo_doc::snapshot::DocSnapshot {
        blocks,
        notes: state.notes().as_ref().clone(),
        comments: state.comments().as_ref().clone(),
        changes: state.changes().as_ref().clone(),
        styles: state.styles().as_ref().clone(),
        ..tilo_doc::snapshot::DocSnapshot::default()
    };
    let mut root = Resource::new("word-processor.document", "tilo.rich-text", "Document");
    add_structural_resources(&mut root, &snapshot);
    add_review_resources(&mut root, &snapshot);
    add_metadata_resources(&mut root, &snapshot);
    add_style_resources(&mut root, &snapshot);
    add_region_resources(
        &mut root,
        "header",
        "Header",
        RegionKind::Header,
        state.header().as_slice(),
    );
    add_region_resources(
        &mut root,
        "footer",
        "Footer",
        RegionKind::Footer,
        state.footer().as_slice(),
    );
    // Same result contracts the standalone tree declares: append-row /
    // append-column answer with the cells they added.
    for table in root
        .children
        .iter_mut()
        .filter(|child| child.kind == "word_processor.table")
    {
        declare_changes(table, TABLE_RETURNING_CHANGES);
    }
    root.children
}

/// Stable id standing for every view-derived `word-processor.selection:…`
/// resource in the catalogue schema.
pub const SCHEMA_SELECTION_RESOURCE: &str = "word-processor.selection";
/// Stable id standing for every `word-processor.search-hit:…` resource in
/// the catalogue schema.
pub const SCHEMA_SEARCH_HIT_RESOURCE: &str = "word-processor.search-hit";

/// The edits a caller composes a document with answer with the blocks they
/// changed ([`crate::semantic_read::changes`]), so it sees the result without
/// reading the document again. Only these: every declared result grows the
/// catalogue each open document publishes to the broker, whose reply is
/// capped.
const RETURNING_CHANGES: &[&str] = &[
    "paste",
    "set-paragraph-style",
    "apply-named-style",
    "insert-table",
];

/// A table's edits that answer with its new cells, whose ids the caller
/// fills them through.
const TABLE_RETURNING_CHANGES: &[&str] = &["append-row", "append-column"];

fn returning_changes(mut root: Resource) -> Resource {
    declare_changes(&mut root, RETURNING_CHANGES);
    for table in root
        .children
        .iter_mut()
        .filter(|child| child.kind == "word_processor.table")
    {
        declare_changes(table, TABLE_RETURNING_CHANGES);
    }
    root
}

fn declare_changes(resource: &mut Resource, ids: &[&str]) {
    for action in &mut resource.actions {
        if ids.contains(&action.id.as_str())
            && action.kind == OperationKind::Document
            && action.contract.result == ResultSpec::None
        {
            action.contract.result = ResultSpec::Json {
                schema: serde_json::json!({"type": ["string", "null"]}),
            };
        }
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
        ActionRegistry::with(register)
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
            let specs = crate::effects::specs();
            let spec = specs
                .iter()
                .find(|spec| spec.id == "zoom-reset")
                .expect("the visible zoom reset has a resource action");
            let encoded = spec_encoding(spec, None);
            assert_eq!(
                crate::effects::decode(&encoded),
                Some(WordProcessorEffect::ZoomReset),
                "the resource action decodes to the one canonical reset effect"
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
            let resources = published_for(&ctx, &state.doc);
            assert_eq!(
                resources
                    .iter()
                    .filter(|resource| resource.id.as_str() == "tilo.rich-text")
                    .count(),
                1,
                "one document root"
            );
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
            let published = <crate::framework::Document as ui::tilo_resource::Resource>::children(
                &<crate::framework::Document as ui::tilo_resource::Resource>::read(&state.doc)
                    .expect("the seeded document reads"),
            );
            assert!(
                published.iter().any(|child| child.id == image.id),
                "the image is a child of the document resource"
            );
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
            let resources = published_for(&ctx, &state.doc);
            for id in ["open", "save", "save-as"] {
                let action = resources[0]
                    .actions
                    .iter()
                    .find(|action| action.id == id)
                    .expect("file action");
                assert_eq!(action.contract.result, ResultSpec::Job);
                assert!(matches!(
                    action.contract.execution,
                    tilo_actions::ExecutionMode::Background { .. }
                ));
                assert!(action.contract.validate_declaration().is_empty());
            }
        }

        // Phase: resource_results_are_bounded_and_precisely_addressable.
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
            let block = ids[0].clone();
            let resources = published_for(&overflowing_resource_ctx(&block), &state.doc);
            assert!(
                resources[0]
                    .actions
                    .iter()
                    .any(|action| action.id == "replace-selection"),
                "the selection's edit is bound to the document"
            );
            let results = resources
                .iter()
                .find(|resource| resource.id.as_str() == "word-processor.search-results")
                .expect("bounded search result group");
            assert_eq!(results.children.len(), MAX_RESOURCE_SEARCH_HITS);
            assert!(results.children.iter().all(|hit| {
                hit.actions.iter().any(|action| action.id == "go-to")
                    && hit.id.as_str().contains(&block)
            }));
            assert_eq!(
                (state.doc.base_heads(), state.doc.pending_ops()),
                pending,
                "resource discovery must not commit the owner"
            );
        }
    }

    /// A comment thread is a child of the document resource, and the open tree
    /// lists that review once.
    #[test]
    fn review_is_a_child_of_the_document_resource() {
        let (mut state, ids) = seeded("Hello\n");
        dispatch(
            &mut state.doc,
            &RichTextAction::AddComment {
                block: ids[0].clone(),
                start: 0,
                end: 5,
                author: "Ada".into(),
                date: "2026-01-01".into(),
                text: "Note this".into(),
            },
        );
        let read = <crate::framework::Document as ui::tilo_resource::Resource>::read(&state.doc)
            .expect("the commented document reads");
        let published =
            <crate::framework::Document as ui::tilo_resource::Resource>::children(&read);
        let review = published
            .iter()
            .find(|child| child.kind == "word_processor.review")
            .expect("review is published with the document");
        assert!(
            review
                .children
                .iter()
                .any(|child| child.kind == "word_processor.comment"),
            "the comment is on the published review"
        );
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
        let tree = published_for(&ctx, &state.doc);
        assert_eq!(
            tree[0]
                .children
                .iter()
                .filter(|child| child.kind == "word_processor.review")
                .count(),
            1,
            "the open tree does not publish a second review"
        );
    }

    /// A named style is a child of the document resource, and the open tree
    /// lists that style sheet once.
    #[test]
    fn a_named_style_is_a_child_of_the_document_resource() {
        let (mut state, ids) = seeded("Styled paragraph\n");
        dispatch(
            &mut state.doc,
            &set_style_property_action(
                "custom1",
                tilo_doc::schema::STYLE_NAME,
                AttrVal::Str("Callout".into()),
            ),
        );
        dispatch(
            &mut state.doc,
            &set_block_attr_action(
                RegionKind::Body,
                vec![ids[0].clone()],
                "style",
                AttrVal::Str("custom1".into()),
            ),
        );
        let read = <crate::framework::Document as ui::tilo_resource::Resource>::read(&state.doc)
            .expect("the styled document reads");
        let published =
            <crate::framework::Document as ui::tilo_resource::Resource>::children(&read);
        let sheet = published
            .iter()
            .find(|child| child.kind == "word_processor.styles")
            .expect("styles are published with the document");
        let style = sheet
            .children
            .iter()
            .find(|child| child.id.as_str() == "word-processor.style:custom1")
            .expect("the named style is a published child");
        assert!(
            style
                .actions
                .iter()
                .any(|action| action.id == "toggle-bold"),
            "the published style can change its bold field"
        );
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
        let tree = published_for(&ctx, &state.doc);
        assert_eq!(
            tree[0]
                .children
                .iter()
                .filter(|child| child.kind == "word_processor.styles")
                .count(),
            1,
            "the open tree does not publish a second style sheet"
        );
    }

    /// A page header is a child of the document resource, and the open tree
    /// lists that header once.
    #[test]
    fn a_header_is_a_child_of_the_document_resource() {
        let (mut state, _) = seeded("Body\n");
        dispatch(
            &mut state.doc,
            &RichTextAction::EnsureRegionBlock {
                region: RegionKind::Header,
            },
        );
        let read = <crate::framework::Document as ui::tilo_resource::Resource>::read(&state.doc)
            .expect("the document with a header reads");
        let block = read
            .header()
            .first()
            .expect("the header has a paragraph")
            .id
            .clone();
        dispatch(
            &mut state.doc,
            &RichTextAction::SpliceText {
                region: RegionKind::Header,
                block: block.clone(),
                at: 0,
                del: 0,
                ins: "Running head".into(),
            },
        );
        let read = <crate::framework::Document as ui::tilo_resource::Resource>::read(&state.doc)
            .expect("the headed document reads");
        let published =
            <crate::framework::Document as ui::tilo_resource::Resource>::children(&read);
        let header = published
            .iter()
            .find(|child| child.kind == "word_processor.header")
            .expect("the header is published with the document");
        let paragraph = header
            .children
            .iter()
            .find(|child| child.id.as_str() == format!("word-processor.header:{block}"))
            .expect("the header paragraph is a published child");
        assert!(
            paragraph
                .actions
                .iter()
                .any(|action| action.id == "align-center"),
            "the published header paragraph can be aligned"
        );
        let ctx = WpResourceCtx {
            caret_block: block,
            caret_offset: 0,
            target_blocks: Vec::new(),
            format_spans: Vec::new(),
            selection: None,
            region: RegionKind::Header,
            search_hits: Vec::new(),
            content: tilo_content::ContentStore::memory(),
        };
        let tree = published_for(&ctx, &state.doc);
        assert_eq!(
            tree[0]
                .children
                .iter()
                .filter(|child| child.kind == "word_processor.header")
                .count(),
            1,
            "the open tree does not publish a second header"
        );
    }

    /// Resizing an image is a published action on that image, not only a panel click.
    #[test]
    fn image_size_is_a_published_document_action() {
        let (mut state, ids) = seeded("Body\n");
        dispatch(
            &mut state.doc,
            &insert_image_action(&ids[0], 0, test_image(Some(20), Some(10))),
        );
        let read = <crate::framework::Document as ui::tilo_resource::Resource>::read(&state.doc)
            .expect("the document with an image reads");
        let image = <crate::framework::Document as ui::tilo_resource::Resource>::children(&read)
            .into_iter()
            .find(|child| child.kind == "word_processor.image")
            .expect("the image is published with the document");
        let spec = image
            .actions
            .iter()
            .find(|action| action.id == "set-size")
            .expect("set size is published on the image");
        let encoded = spec
            .build_args([
                ("width".into(), tilo_actions::ArgValue::Integer(300)),
                ("height".into(), tilo_actions::ArgValue::Integer(180)),
            ])
            .expect("width and height bind");
        let action: RichTextAction = serde_json::from_slice(&encoded.payload).expect("decodes");
        tilo_actions::Action::apply(&action, &mut state.doc).expect("size applies");
        let blocks = body_blocks(&state.doc);
        let image = blocks
            .iter()
            .find(|block| block.kind == "image")
            .expect("image remains");
        assert_eq!(image.attrs.get("width"), Some(&tilo_doc::AttrVal::Int(300)));
        assert_eq!(
            image.attrs.get("height"),
            Some(&tilo_doc::AttrVal::Int(180))
        );
    }

    /// Flipping an image is a published action on that image, not only a panel click.
    #[test]
    fn image_flip_is_a_published_document_action() {
        let (mut state, ids) = seeded("Body\n");
        dispatch(
            &mut state.doc,
            &insert_image_action(&ids[0], 0, test_image(Some(20), Some(10))),
        );
        let read = <crate::framework::Document as ui::tilo_resource::Resource>::read(&state.doc)
            .expect("the document with an image reads");
        let image = <crate::framework::Document as ui::tilo_resource::Resource>::children(&read)
            .into_iter()
            .find(|child| child.kind == "word_processor.image")
            .expect("the image is published with the document");
        let spec = image
            .actions
            .iter()
            .find(|action| action.id == "flip-horizontal")
            .expect("flip is published on the image");
        let encoded = spec_encoding(spec, None);
        let action: RichTextAction =
            serde_json::from_slice(&encoded.payload).expect("the flip action decodes");
        tilo_actions::Action::apply(&action, &mut state.doc).expect("flip applies");
        let blocks = body_blocks(&state.doc);
        let image = blocks
            .iter()
            .find(|block| block.kind == "image")
            .expect("image remains");
        assert_eq!(
            image.attrs.get("flip_h"),
            Some(&tilo_doc::AttrVal::Bool(true))
        );
    }

    /// The table of contents is a document action built from the headings the
    /// document already holds.
    #[test]
    fn a_table_of_contents_is_a_published_document_action() {
        let (mut state, _) = seeded("# Section\n\nBody\n");
        let state_read =
            <crate::framework::Document as ui::tilo_resource::Resource>::read(&state.doc).unwrap();
        let root = ui::tilo_resource::publish::<crate::framework::Document>(
            "tilo.rich-text",
            "Document",
            &state_read,
        );
        let spec = root
            .actions
            .iter()
            .find(|action| action.id == "generate-toc")
            .expect("generate table of contents is published");
        let encoded = spec_encoding(spec, None);
        let action: RichTextAction =
            serde_json::from_slice(&encoded.payload).expect("the toc action decodes");
        tilo_actions::Action::apply(&action, &mut state.doc).expect("toc applies");
        let blocks = body_blocks(&state.doc);
        assert!(
            blocks.iter().any(|block| {
                matches!(block.attrs.get("toc"), Some(tilo_doc::AttrVal::Bool(true)))
                    && block.text.contains("Section")
            }),
            "the published action inserts a contents line for the heading"
        );
    }

    /// A `ui::State` holding a canonical document seeded from `markdown`, plus the
    /// ordered body block ids.
    fn seeded(markdown: &str) -> (ui::State, Vec<String>) {
        let mut state = crate::initialized_state();
        tilo_actions::Action::apply(&replace_body_action(markdown), &mut state.doc).unwrap();
        let c = tilo_doc::model::open(&state.doc, &automerge::ROOT).unwrap();
        let ids = tilo_doc::model::blocks_in_order(&state.doc, &c);
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
        let extras = live_presentation_resources(&ctx, &doc, None);
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
        resources: |doc| published_for(&wp_ctx(doc), doc),
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
            let root = published_for(&ctx, &state.doc).remove(0);
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
            let root = published_for(&ctx, &state.doc).remove(0);
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
            let resources = published_for(&ctx, &state.doc);
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
            let resources = published_for(&ctx, &a.doc);
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
        published_for(&ctx, &state.doc).remove(0)
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
        assert_eq!(root.id.0, "tilo.rich-text");
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
        assert!(ops.contains(&"tilo.rich-text#append-paragraph".to_owned()));
        assert!(ops.contains(&"tilo.rich-text#continue-paragraph".to_owned()));
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
