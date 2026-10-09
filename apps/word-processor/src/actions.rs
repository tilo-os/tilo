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

use tilo_actions::{ActionRegistry, ContextRequirement};
use ui::tilo_resource::Effect as _;

use crate::effects::WordProcessorEffect;

/// Registry `type_id` for every word-processor action.
pub use tilo_document::RICH_TEXT_ACTION_TYPE as WORD_PROCESSOR_ACTION_TYPE;
pub use tilo_document::action::*;
/// Register the word processor's action decoder for palette execution + remote
/// replay.
pub fn register(registry: &mut ActionRegistry) {
    <crate::model::Document as ui::tilo_resource::Resource>::register(registry);
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

/// What the word processor's framework app publishes for a view at `ctx` on
/// `doc`: the document, its verbs, field edits, effects and children, and
/// the view's Find results. For a peer in a test that serves a Writer
/// document without a window.
#[cfg(any(test, feature = "test-support"))]
pub fn published_for(ctx: &WpResourceCtx, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
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

/// The Find results of the view `ctx` describes, as live rows beneath the
/// document: each goes to its range in the invoker's own view.
pub(crate) fn search_rows(ctx: &WpResourceCtx, doc: &AutoCommit) -> Vec<ui::framework::LiveRow> {
    if ctx.search_hits.is_empty() {
        return Vec::new();
    }
    let Ok(containers) = crate::model::open(doc) else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use tilo_actions::LocalTransport;
    use tilo_actions::{ActionError, EncodedAction};
    use tilo_automerge::ApplyIncoming;

    /// A body edit as the document's action on the wire.
    fn encode(op: &RichTextAction) -> EncodedAction {
        crate::model::encoded(op)
    }

    /// The body edit an encoded document action carries.
    fn decode(encoded: &EncodedAction) -> RichTextAction {
        crate::model::decoded(encoded).expect("a body edit")
    }

    /// Apply a body edit locally through the executor, returning its envelope.
    fn apply_local(
        doc: &mut AutoCommit,
        op: &RichTextAction,
        encoded: EncodedAction,
    ) -> Result<tilo_actions::ActionEnvelope, ActionError> {
        tilo_actions::apply_local(doc, &crate::model::action(op), encoded)
    }

    /// Body edits applied straight to a document, as the door applies them.
    trait ApplyBody {
        fn apply(&self, doc: &mut AutoCommit) -> Result<(), ActionError>;
    }

    impl ApplyBody for RichTextAction {
        fn apply(&self, doc: &mut AutoCommit) -> Result<(), ActionError> {
            crate::model::apply(doc, self)
        }
    }

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
                &crate::model::open(&state.doc).unwrap(),
            );
            let last = after_img.last().cloned().unwrap();
            dispatch(&mut state.doc, &insert_table_action(&last, 2, 2));

            let resources = published_for(&caret_at(&para), &state.doc);
            assert_eq!(
                resources
                    .iter()
                    .filter(|resource| resource.id.as_str() == DOCUMENT)
                    .count(),
                1,
                "one document root"
            );
            let root = &resources[0];
            let image = derived(root, "image").expect("an image resource");
            let table = derived(root, "table").expect("a table resource");
            assert!(
                image
                    .actions
                    .iter()
                    .any(|action| action.id == "delete-image")
            );
            for id in ["add-row", "add-column", "delete-table"] {
                assert!(table.actions.iter().any(|action| action.id == id), "{id}");
            }
            assert_eq!(table.children.len(), 4, "every stable cell is navigable");
            assert!(table.children.iter().all(|cell| {
                cell.actions.iter().any(|action| action.id == "go-to")
                    && cell.actions.iter().any(|action| action.id == "delete-row")
                    && cell
                        .actions
                        .iter()
                        .any(|action| action.id == "delete-column")
            }));
            let published = <crate::framework::Document as ui::tilo_resource::Resource>::children(
                &<crate::framework::Document as ui::tilo_resource::Resource>::read(&state.doc)
                    .expect("the seeded document reads"),
            );
            assert!(
                published.iter().any(|child| child.id == image.id),
                "the image is a child of the document resource"
            );
            // Adding a row is bound to its table: invoked on the table, it adds one.
            invoke(
                &mut state.doc,
                &resources,
                &table.id.0,
                "add-row",
                &serde_json::Map::new(),
            );
            let cells = body_blocks(&state.doc)
                .into_iter()
                .filter(|block| block.kind == "cell")
                .count();
            assert_eq!(cells, 6, "a third row of two cells");
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
            for id in ["open-document", "save", "save-as"] {
                let action = resources[0]
                    .actions
                    .iter()
                    .find(|action| action.id == id)
                    .expect("file action");
                assert_eq!(action.contract.result, tilo_actions::ResultSpec::Job);
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
                "the selection's edit is published on the document"
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

    /// The document's own resource id and kind.
    const DOCUMENT: &str = "word-processor.document";

    /// The document verb `operation` as its action value.
    fn verb(
        operation: crate::model::DocumentAction,
    ) -> <crate::model::Document as ui::tilo_resource::Resource>::Action {
        ui::tilo_resource::Stamped {
            operation,
            now_ms: 0,
        }
    }

    /// Apply `action` as `author` through the executor, returning its
    /// envelope.
    fn apply_verb_by(
        doc: &mut AutoCommit,
        action: &<crate::model::Document as ui::tilo_resource::Resource>::Action,
        author: &str,
    ) -> tilo_actions::ActionEnvelope {
        let encoded = ui::tilo_resource::encode::<crate::model::Document>(action).unwrap();
        let context = tilo_actions::ActionContext::for_app("org.tilo.Writer", "Writer")
            .with_invoker(tilo_actions::ActionInvoker::actor(author));
        tilo_actions::apply_local_with_context(doc, action, encoded, None, context).unwrap()
    }

    /// The document's comment threads, each with where its text is now.
    fn threads(doc: &AutoCommit) -> Vec<tilo_doc::snapshot::CommentView> {
        <crate::model::Document as ui::tilo_resource::Resource>::read(doc)
            .unwrap()
            .comment_views(doc)
    }

    /// The document's accept-change verb, as its action value.
    fn accept_change(
        change: &str,
    ) -> <crate::model::Document as ui::tilo_resource::Resource>::Action {
        ui::tilo_resource::Stamped {
            operation: crate::model::DocumentAction::AcceptChange(
                crate::model::document::AcceptChange {
                    change: change.to_owned(),
                },
            ),
            now_ms: 0,
        }
    }

    /// A collapsed caret at the start of `block`.
    fn caret_at(block: &str) -> WpResourceCtx {
        WpResourceCtx {
            caret_block: block.to_owned(),
            caret_offset: 0,
            target_blocks: vec![block.to_owned()],
            format_spans: Vec::new(),
            selection: None,
            region: RegionKind::Body,
            search_hits: Vec::new(),
            content: tilo_content::ContentStore::memory(),
        }
    }

    /// The object of `kind` (`table`, `image`, `change`, `comment`) the
    /// document publishes beneath it.
    fn derived<'a>(
        root: &'a tilo_actions::Resource,
        kind: &str,
    ) -> Option<&'a tilo_actions::Resource> {
        let prefix = format!("{DOCUMENT}/derived/{kind}:");
        root.children
            .iter()
            .find(|child| child.id.as_str().starts_with(&prefix))
    }

    /// Invoke `action` on the published resource `resource` through the door.
    fn invoke(
        doc: &mut AutoCommit,
        resources: &[tilo_actions::Resource],
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) {
        // As a person who confirmed it: deleting asks first.
        tilo_actions::door::invoke_json_confirmed(
            doc,
            resources,
            &tilo_actions::door::ActionAddress::new(resource, action),
            arguments,
            tilo_actions::door::InvocationProvenance::caller(b"agent"),
            &registry(),
            &mut LocalTransport::new(),
        )
        .unwrap_or_else(|error| panic!("{resource}#{action}: {error:?}"));
    }

    /// A tracked change is published with the document, and accepting it
    /// there keeps it as text.
    #[test]
    fn review_is_a_child_of_the_document_resource() {
        let (mut state, ids) = seeded("Hello\n");
        dispatch(
            &mut state.doc,
            &RichTextAction::TrackInsert {
                block: ids[0].clone(),
                at: 5,
                text: " there".into(),
                author: "Ada".into(),
                date: "2026-01-01".into(),
            },
        );
        let resources = published_for(&caret_at(&ids[0]), &state.doc);
        let change = derived(&resources[0], "change").expect("the change is published");
        assert!(
            change
                .actions
                .iter()
                .any(|action| action.id == "reject-change")
        );
        assert_eq!(
            resources[0]
                .children
                .iter()
                .filter(|child| child.id == change.id)
                .count(),
            1,
            "the open tree publishes the change once"
        );
        let change = change.id.0.clone();
        invoke(
            &mut state.doc,
            &resources,
            &change,
            "accept-change",
            &serde_json::Map::new(),
        );
        let snap =
            tilo_doc::model::materialize(&state.doc, &crate::model::open(&state.doc).unwrap());
        assert!(snap.changes.is_empty(), "the accepted change left review");
        assert_eq!(snap.blocks[0].text, "Hello there");
    }

    /// A named style is changed by an edit of the body: one property at a
    /// time, so collaborators' edits of different properties merge.
    #[test]
    fn a_named_style_is_edited_through_the_body() {
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
        let resources = published_for(&caret_at(&ids[0]), &state.doc);
        let edit = crate::model::edit(&set_style_property_action(
            "custom1",
            tilo_doc::schema::STYLE_BOLD,
            AttrVal::Bool(true),
        ));
        let mut arguments = serde_json::Map::new();
        arguments.insert(
            ui::tilo_resource::EDIT_PARAM.to_owned(),
            serde_json::to_value(&edit).unwrap(),
        );
        invoke(
            &mut state.doc,
            &resources,
            DOCUMENT,
            ui::tilo_resource::FIELD_EDIT,
            &arguments,
        );
        let c = crate::model::open(&state.doc).unwrap();
        assert_eq!(
            tilo_doc::model::style_props(&state.doc, &c, "custom1")
                .get(tilo_doc::schema::STYLE_BOLD),
            Some(&tilo_doc::AttrVal::Bool(true))
        );
    }

    /// The page header is part of the body field: the document reads it,
    /// and a header paragraph is aligned by an edit of the body.
    #[test]
    fn a_header_is_part_of_the_document() {
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
            .text()
            .unwrap()
            .header()
            .first()
            .expect("the header has a paragraph")
            .id
            .clone();
        dispatch(
            &mut state.doc,
            &set_block_attr_action(
                RegionKind::Header,
                vec![block],
                "align",
                AttrVal::Str("center".into()),
            ),
        );
        let read = <crate::framework::Document as ui::tilo_resource::Resource>::read(&state.doc)
            .expect("the headed document reads");
        let header = read.text().unwrap().header();
        assert_eq!(
            header[0].attrs.get("align"),
            Some(&tilo_doc::AttrVal::Str("center".into()))
        );
    }

    /// Resizing an image is an edit of the body any caller submits through
    /// the document's field edit, not only a panel click.
    #[test]
    fn image_size_is_a_published_document_action() {
        let (mut state, ids) = seeded("Body\n");
        dispatch(
            &mut state.doc,
            &insert_image_action(&ids[0], 0, test_image(Some(20), Some(10))),
        );
        let image = body_blocks(&state.doc)
            .into_iter()
            .find(|block| block.kind == "image")
            .expect("the image is in the body")
            .id;
        let resources = published_for(&caret_at(&ids[0]), &state.doc);
        let mut arguments = serde_json::Map::new();
        arguments.insert(
            ui::tilo_resource::EDIT_PARAM.to_owned(),
            serde_json::to_value(crate::model::edit(&set_image_size_action(&image, 300, 180)))
                .unwrap(),
        );
        invoke(
            &mut state.doc,
            &resources,
            DOCUMENT,
            ui::tilo_resource::FIELD_EDIT,
            &arguments,
        );
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

    /// Flipping an image is an edit of the body, and deleting it is a verb
    /// on the image itself.
    #[test]
    fn image_flip_is_a_published_document_action() {
        let (mut state, ids) = seeded("Body\n");
        dispatch(
            &mut state.doc,
            &insert_image_action(&ids[0], 0, test_image(Some(20), Some(10))),
        );
        let id = body_blocks(&state.doc)
            .into_iter()
            .find(|block| block.kind == "image")
            .unwrap()
            .id;
        dispatch(
            &mut state.doc,
            &set_block_attr_action(RegionKind::Body, vec![id], "flip_h", AttrVal::Bool(true)),
        );
        let blocks = body_blocks(&state.doc);
        let image = blocks
            .iter()
            .find(|block| block.kind == "image")
            .expect("image remains");
        assert_eq!(
            image.attrs.get("flip_h"),
            Some(&tilo_doc::AttrVal::Bool(true))
        );
        let resources = published_for(&caret_at(&ids[0]), &state.doc);
        let image = derived(&resources[0], "image")
            .expect("the image is published")
            .id
            .0
            .clone();
        invoke(
            &mut state.doc,
            &resources,
            &image,
            "delete-image",
            &serde_json::Map::new(),
        );
        assert!(
            body_blocks(&state.doc)
                .iter()
                .all(|block| block.kind != "image")
        );
    }

    /// The table of contents is a verb of the document built from the
    /// headings it already holds.
    #[test]
    fn a_table_of_contents_is_a_published_document_action() {
        let (mut state, ids) = seeded("# Section\n\nBody\n");
        let resources = published_for(&caret_at(&ids[1]), &state.doc);
        assert!(
            resources[0]
                .actions
                .iter()
                .any(|action| action.id == "generate-toc"),
            "generate table of contents is published"
        );
        invoke(
            &mut state.doc,
            &resources,
            DOCUMENT,
            "generate-toc",
            &serde_json::Map::new(),
        );
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
        replace_body_action(markdown).apply(&mut state.doc).unwrap();
        let c = crate::model::open(&state.doc).unwrap();
        let ids = tilo_doc::model::blocks_in_order(&state.doc, &c);
        (state, ids)
    }

    fn body_blocks(doc: &AutoCommit) -> Vec<tilo_doc::snapshot::BlockView> {
        let c = crate::model::open(doc).unwrap();
        tilo_doc::model::materialize(doc, &c).blocks
    }

    fn text_of(doc: &AutoCommit, block: &str) -> String {
        let c = crate::model::open(doc).unwrap();
        tilo_doc::model::block_text(doc, &c, block).unwrap_or_default()
    }

    /// Dispatch an action through the Tilo executor with a discarding transport
    /// (the local, single-peer path the affordances take).
    fn dispatch(doc: &mut AutoCommit, action: &RichTextAction) {
        let encoded = encode(action);
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(doc, &crate::model::action(action), encoded, &mut tx).unwrap();
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
            let c = crate::model::open(&state.doc).unwrap();
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
        let c = crate::model::open(&state.doc).unwrap();
        let mut id_gen = tilo_doc::IdGen::new(state.doc.get_actor().to_hex_string());
        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut id_gen,
            };
            dm.track_insert(&ids[2], 6, " three", "Jonas", "2026-09-05");
        }
        crate::model::comment_by(
            &mut state.doc,
            "Maya",
            &ids[1],
            (0, 5),
            "Please clarify this.",
        );
        state.doc.commit();
        state.doc
    }

    /// The `n`th body block's stable id.
    fn nth_block(doc: &AutoCommit, n: usize) -> String {
        let c = crate::model::open(doc).unwrap();
        tilo_doc::model::blocks_in_order(doc, &c)
            .into_iter()
            .nth(n)
            .expect("seeded block")
    }

    /// Edits at the caret and the selection are effects published on the
    /// document itself, never a second document resource.
    #[test]
    fn edits_at_the_selection_are_published_on_the_document() {
        let doc = wp_seed();
        let resources = published_for(&wp_ctx(&doc), &doc);
        assert!(
            resources
                .iter()
                .filter(|resource| resource.id.as_str() == DOCUMENT)
                .count()
                == 1,
            "the document is published once"
        );
        for id in [
            "type-text",
            "replace-selection",
            "set-hyperlink",
            "comment-selection",
            "toggle-mark",
            "insert-table-here",
        ] {
            assert!(
                resources[0].actions.iter().any(|action| action.id == id),
                "{id} is on the document"
            );
        }
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
            "Insert table here",
            "Insert page break",
            "Align paragraph",
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
            let view = participant::ParticipantView {
                caret: participant::ViewPoint::new(&block, 4),
                anchor: Some(participant::ViewPoint::new(&block, 0)),
                ..participant::ParticipantView::default()
            };
            let (replace, _) = editing::replace_selection(&view, &state.doc, "guide").unwrap();
            assert_eq!(
                encode(&replace[0]),
                encode(&replace_selection_action(
                    RegionKind::Body,
                    &block,
                    0,
                    &block,
                    4,
                    "guide",
                )),
                "replacing the selection is the canonical selection action"
            );
            let (link, _) =
                editing::set_hyperlink(&view, &state.doc, " https://example.com/guide ").unwrap();
            link[0].apply(&mut state.doc).unwrap();
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
            let view = participant::ParticipantView {
                caret: participant::ViewPoint::new(&block, 4),
                anchor: Some(participant::ViewPoint::new(&block, 0)),
                ..participant::ParticipantView::default()
            };
            assert!(
                matches!(
                    editing::set_hyperlink(&view, &state.doc, "javascript:alert(1)"),
                    Err(editing::EditingError::Invalid(_))
                ),
                "the effect reports invalid input before dispatch"
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
        // Placing consumes bytes an earlier action actually put in the
        // content store. The caller supplies neither image metadata nor the
        // bytes; the app resolves both while building its own document edit.
        {
            let (mut state, ids) = seeded("Intro\n");
            let content = tilo_content::ContentStore::memory();
            let bytes = b"content supplied by a preceding action";
            let hash = content.put(bytes).expect("test fixture fits the store");
            let view = participant::ParticipantView::at(
                participant::ViewPoint::new(&ids[0], 0),
                RegionKind::Body,
            );
            let resources = published_for(&caret_at(&ids[0]), &state.doc);
            assert!(
                resources[0]
                    .actions
                    .iter()
                    .any(|action| action.id == "place-image"),
                "the document publishes placing an image"
            );
            assert!(
                editing::place_image(&view, &state.doc, &content, "not-in-the-store").is_err(),
                "content not on this machine is refused"
            );
            let (place, _) = editing::place_image(&view, &state.doc, &content, &hash).unwrap();
            dispatch(&mut state.doc, &place[0]);
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
                let envelope =
                    apply_local(&mut a.doc, action, encode(action)).expect("apply object action");
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
            let c = crate::model::open(&a.doc).unwrap();
            let mut id_gen = tilo_doc::IdGen::new(a.doc.get_actor().to_hex_string());
            let change = {
                let mut dm = tilo_doc::DocMut {
                    doc: &mut a.doc,
                    c: &c,
                    ids: &mut id_gen,
                };
                dm.track_insert(&block, 20, " now", "Jonas", "2026-09-05")
            };
            let comment = crate::model::comment_by(
                &mut a.doc,
                "Maya",
                &block,
                (0, 6),
                "Please clarify this.",
            );
            a.doc.commit();
            let bytes = a.save();
            let mut b = ui::State::load(&bytes).unwrap();

            let resources = published_for(&caret_at(&block), &a.doc);
            let thread = format!("{DOCUMENT}/comments/{comment}");
            let comment_resource = resources[0]
                .children
                .iter()
                .find(|child| child.id.as_str() == thread)
                .expect("the comment thread is a resource");
            for id in ["reply-to-comment", "resolve-comment"] {
                assert!(
                    comment_resource
                        .actions
                        .iter()
                        .any(|action| action.id == id),
                    "{id} is offered on the thread"
                );
            }
            let change_resource =
                derived(&resources[0], "change").expect("tracked change is a resource");
            assert!(change_resource.id.as_str().ends_with(&change));
            let accept = accept_change(&change);
            let encoded = ui::tilo_resource::encode::<crate::model::Document>(&accept).unwrap();
            let envelope = tilo_actions::apply_local(&mut a.doc, &accept, encoded).unwrap();
            tilo_actions::replay(&mut b.doc, &envelope, &registry()).unwrap();
            assert_eq!(a.doc.get_heads(), b.doc.get_heads());
            let snap = tilo_doc::model::materialize(&a.doc, &crate::model::open(&a.doc).unwrap());
            assert!(snap.changes.is_empty(), "accepted change left review state");
            assert_eq!(threads(&a.doc).len(), 1, "unrelated comment remains");
        }

        // Phase: durable_comment_reply_action_replays_and_resolve_cleans_the_thread.
        {
            let (mut a, ids) = seeded("Review this paragraph\n");
            let block = ids[0].clone();
            let comment =
                crate::model::comment_by(&mut a.doc, "Maya", &block, (0, 6), "Please clarify");
            a.doc.commit();
            let bytes = a.save();
            let mut b = ui::State::load(&bytes).unwrap();

            let reply = verb(crate::model::DocumentAction::ReplyToComment(
                crate::model::document::ReplyToComment {
                    comment: ui::tilo_resource::Id::from_raw(comment.clone()),
                    text: "Will do.".into(),
                    body: None,
                },
            ));
            let envelope = apply_verb_by(&mut a.doc, &reply, "Jonas");
            tilo_actions::replay(&mut b.doc, &envelope, &registry()).unwrap();
            assert_eq!(
                a.doc.get_heads(),
                b.doc.get_heads(),
                "reply replays exactly"
            );
            let comments = threads(&a.doc);
            assert_eq!(comments[0].replies.len(), 1);
            assert_eq!(comments[0].replies[0].author, "Jonas");
            assert_eq!(comments[0].replies[0].blocks[0].text, "Will do.");

            let resolve = verb(crate::model::DocumentAction::ResolveComment(
                crate::model::document::ResolveComment {
                    comment: ui::tilo_resource::Id::from_raw(comment),
                },
            ));
            let envelope = apply_verb_by(&mut a.doc, &resolve, "Maya");
            tilo_actions::replay(&mut b.doc, &envelope, &registry()).unwrap();
            assert_eq!(
                a.doc.get_heads(),
                b.doc.get_heads(),
                "resolution replays exactly"
            );
            assert!(threads(&a.doc).is_empty());
        }

        // Phase: resolved_comment_tombstone_wins_over_divergent_reply.
        {
            let (mut a, ids) = seeded("Review this paragraph\n");
            let block = ids[0].clone();
            let add = verb(crate::model::DocumentAction::AddComment(
                crate::model::document::AddComment {
                    block,
                    start: 0,
                    end: 6,
                    text: "Please clarify".into(),
                },
            ));
            apply_verb_by(&mut a.doc, &add, "Maya");
            let comment = threads(&a.doc)[0].id.clone();
            let bytes = a.save();
            let mut b = ui::State::load(&bytes).unwrap();
            b.doc.set_actor(automerge::ActorId::random());

            let resolve = verb(crate::model::DocumentAction::ResolveComment(
                crate::model::document::ResolveComment {
                    comment: ui::tilo_resource::Id::from_raw(comment.clone()),
                },
            ));
            let reply = verb(crate::model::DocumentAction::ReplyToComment(
                crate::model::document::ReplyToComment {
                    comment: ui::tilo_resource::Id::from_raw(comment),
                    text: "Will do".into(),
                    body: None,
                },
            ));
            apply_verb_by(&mut a.doc, &resolve, "Maya");
            apply_verb_by(&mut b.doc, &reply, "Jonas");
            a.doc.merge_incoming(&mut b.doc).unwrap();
            assert!(
                threads(&a.doc).is_empty(),
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
            let envelope = apply_local(&mut a.doc, &action, encoded).unwrap();
            tilo_actions::replay(&mut b.doc, &envelope, &registry()).unwrap();
            assert_eq!(a.doc.get_heads(), b.doc.get_heads());
            let c = crate::model::open(&a.doc).unwrap();
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
            let decoded_split = decode(&encoded_split);
            apply_local(&mut a.doc, &decoded_split, encoded_split).unwrap();

            let splice = splice_text_action(RegionKind::Body, &block, 0, 0, "Shared ");
            let encoded_splice = encode(&splice);
            let decoded_splice = decode(&encoded_splice);
            apply_local(&mut b.doc, &decoded_splice, encoded_splice).unwrap();

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
            apply_local(&mut a.doc, &bold, encode(&bold)).unwrap();
            apply_local(&mut b.doc, &size, encode(&size)).unwrap();
            a.doc.merge_incoming(&mut b.doc).unwrap();
            let c = crate::model::open(&a.doc).unwrap();
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
            apply_local(&mut deleted.doc, &remove, encode(&remove)).unwrap();
            apply_local(&mut divergent.doc, &rename, encode(&rename)).unwrap();
            deleted.doc.merge_incoming(&mut divergent.doc).unwrap();
            let c = crate::model::open(&deleted.doc).unwrap();
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
                let decoded = decode(&encoded);
                let envelope = apply_local(&mut origin.doc, &decoded, encoded).unwrap();
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
                let decoded = decode(&encoded);
                let envelope = apply_local(&mut origin.doc, &decoded, encoded).unwrap();
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
            apply_local(&mut deleted.doc, &remove, encode(&remove)).unwrap();
            apply_local(&mut divergent.doc, &late_edit, encode(&late_edit)).unwrap();

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

    fn root_for(state: &ui::State, block: &str) -> tilo_actions::Resource {
        published_for(&caret_at(block), &state.doc).remove(0)
    }

    #[test]
    fn appending_a_paragraph_is_a_root_level_document_verb() {
        let (mut state, ids) = seeded("Intro\n");
        let root = root_for(&state, &ids[0]);
        assert_eq!(root.id.0, DOCUMENT);
        let spec = root
            .actions
            .iter()
            .find(|action| action.id == "append-paragraph")
            .expect("append-paragraph is on the root resource");
        assert_eq!(spec.params().len(), 1);
        assert_eq!(spec.params()[0].name, "text");
        assert!(spec.params()[0].required);
        assert_eq!(spec.kind, tilo_actions::OperationKind::Document);
        assert_eq!(spec.locus, tilo_actions::ExecutionLocus::ClientLocal);
        assert!(matches!(
            spec.contract.confirmation,
            tilo_actions::ConfirmationPolicy::None
        ));
        assert!(
            spec.contract.validate_declaration().is_empty(),
            "{:?}",
            spec.contract.validate_declaration()
        );
        // Empty text is refused, and nothing is written.
        let before = state.doc.save();
        let mut arguments = serde_json::Map::new();
        arguments.insert("text".into(), "".into());
        assert!(
            tilo_actions::door::invoke_json(
                &mut state.doc,
                std::slice::from_ref(&root),
                &tilo_actions::door::ActionAddress::new(DOCUMENT, "append-paragraph"),
                &arguments,
                tilo_actions::door::InvocationProvenance::caller(b"agent"),
                &registry(),
                &mut LocalTransport::new(),
            )
            .is_err()
        );
        assert_eq!(state.doc.save(), before);
    }

    /// The host offers every Document operation of its workers as one grant
    /// set, and withdraws the offer above 64. A fresh document's catalogue
    /// stays well inside that bound.
    #[test]
    fn a_fresh_document_publishes_few_grantable_document_operations() {
        fn document_ops(resource: &tilo_actions::Resource, out: &mut Vec<String>) {
            for action in &resource.actions {
                if action.kind == tilo_actions::OperationKind::Document {
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
        assert!(ops.contains(&format!("{DOCUMENT}#append-paragraph")));
        assert!(ops.contains(&format!("{DOCUMENT}#field-edit")));
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
        let resources = vec![root_for(&state, &ids[0])];
        let mut arguments = serde_json::Map::new();
        arguments.insert("text".into(), "The ocean ".into());
        invoke(
            &mut state.doc,
            &resources,
            DOCUMENT,
            "append-paragraph",
            &arguments,
        );
        for text in ["covers most ", "of the ", "planet."] {
            dispatch(&mut state.doc, &continue_paragraph_action(text));
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
