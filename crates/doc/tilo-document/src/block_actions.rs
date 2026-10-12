//! The document actions an agent or script edits rich text by, and what they
//! answer.
//!
//! Every app that hosts a rich-text document offers these on it, because they
//! are the [`RichText`] resource's own declarations: read the body as
//! Markdown with stable block ids ([`read_markdown_under`]), insert blocks of
//! any kind, edit a block by id, move and delete blocks, and replace the body
//! from Markdown as one edit that keeps the blocks it did not change.
//!
//! | action | arguments | edits |
//! | --- | --- | --- |
//! | `insert-block` | `kind`, `text`, `after`/`position`, `level`, `indent`, `done`, `lang` | one block |
//! | `insert-markdown` | `markdown`, `after`/`position` | the blocks it says |
//! | `set-block-text` | `block`, `text`, `markdown` | a block's text |
//! | `set-block-kind` | `block`, `kind`, `level`, `indent`, `done`, `lang` | a block's kind, or (no kind) a checklist item's state |
//! | `indent-block` | `block`, `levels` (negative outdents) | a list item |
//! | `move-block` | `block`, `after`/`position` | a block's place |
//! | `delete-blocks` | `blocks` | blocks |
//! | `replace-body` | `markdown` | the whole body, kept block by block |
//!
//! Each answers the ids it touched ([`changes_under`]), so its caller needs no
//! second read.

use std::collections::BTreeSet;

use automerge::{AutoCommit, ObjId, PatchAction, Prop, Value};
use serde_json::json;
use tilo_actions::{ActionArgs, ActionParam, ParamType};
use tilo_doc::model::{Containers, Region};
use tilo_resource::ActionDecl;

use crate::RichText;
use crate::action::RichTextAction;
use crate::block_edit::{BlockKind, BlockShape, NewBlock, Placement};
use crate::block_markdown::{render, short_ids};

/// The ids of the declared actions that answer what they changed.
pub const ANSWERING: [&str; 8] = [
    "insert-block",
    "insert-markdown",
    "set-block-text",
    "set-block-kind",
    "indent-block",
    "move-block",
    "delete-blocks",
    "replace-body",
];

/// The most ids an answer lists; the rest are counted.
const MAX_LISTED: usize = 50;

fn text_param(name: &'static str, description: &'static str) -> ActionParam {
    ActionParam::required_owned(name, ParamType::Text, description)
}

fn optional(name: &'static str, ty: ParamType, description: &'static str) -> ActionParam {
    ActionParam::optional_owned(name, ty, description)
}

fn block_param() -> ActionParam {
    text_param(
        "block",
        "A block id, as the read shows it after ^ (a unique prefix of it is enough)",
    )
}

fn placement_params() -> [ActionParam; 2] {
    [
        optional(
            "after",
            ParamType::Text,
            "Place directly after this block id; leave out to use `position`",
        ),
        optional(
            "position",
            ParamType::Text,
            "`start` or `end` of the page, when `after` is left out (default end)",
        )
        .with_choices(["start", "end"]),
    ]
}

fn shape_params() -> [ActionParam; 4] {
    [
        optional(
            "level",
            ParamType::Integer,
            "A heading's level, 1 to 6 (default 1)",
        ),
        optional(
            "indent",
            ParamType::Integer,
            "A list item's depth, 0 to 5 (default 0)",
        ),
        optional(
            "done",
            ParamType::Boolean,
            "Whether a checklist item is ticked (default not)",
        ),
        optional("lang", ParamType::Text, "A code block's language"),
    ]
}

fn kind_param(required: bool) -> ActionParam {
    let description = "paragraph, heading, bullet, numbered, checklist, quote or code";
    let param = if required {
        text_param("kind", description)
    } else {
        optional("kind", ParamType::Text, description)
    };
    param.with_choices(BlockKind::NAMES)
}

fn placement_of(args: &ActionArgs) -> Result<Placement, String> {
    if let Some(after) = args
        .text("after")
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        return Ok(Placement::After(after.to_owned()));
    }
    match args.text("position").map(str::trim) {
        None | Some("" | "end") => Ok(Placement::End),
        Some("start") => Ok(Placement::Start),
        Some(other) => Err(format!("position is `start` or `end`, not `{other}`")),
    }
}

fn shape_of(args: &ActionArgs, default: Option<BlockKind>) -> Result<BlockShape, String> {
    let kind = match args
        .text("kind")
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
    {
        Some(name) => BlockKind::parse(name).ok_or_else(|| {
            format!(
                "kind is one of {}, not `{name}`",
                BlockKind::NAMES.join(", ")
            )
        })?,
        None => default.ok_or("name the kind of block")?,
    };
    Ok(BlockShape {
        kind,
        level: args.integer("level"),
        indent: args.integer("indent"),
        done: args.boolean("done"),
        lang: args.text("lang").map(str::to_owned),
    })
}

fn block_of(args: &ActionArgs) -> Result<String, String> {
    args.text("block")
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "name the block".to_owned())
}

/// Indent (`levels` positive, default 1) or outdent (negative) a list item.
fn shifted(args: &ActionArgs) -> Result<RichTextAction, String> {
    let levels = args.integer("levels").unwrap_or(1);
    let max = crate::block_markdown::MAX_INDENT;
    if levels == 0 || !(-max..=max).contains(&levels) {
        return Err(format!(
            "levels is 1 to {max} to indent or -1 to -{max} to outdent, not {levels}"
        ));
    }
    Ok(RichTextAction::ShiftIndent {
        block: block_of(args)?,
        by: levels,
    })
}

/// The declared document actions, each answering the ids it touched.
#[expect(
    clippy::too_many_lines,
    reason = "the published action table: one independent declaration per action"
)]
pub(crate) fn declarations() -> Vec<ActionDecl<RichText>> {
    let mut insert = vec![kind_param(false)];
    insert.push(optional(
        "text",
        ParamType::Text,
        "The block's text, plain (no Markdown); empty for a blank block",
    ));
    insert.extend(placement_params());
    insert.extend(shape_params());

    let mut set_kind = vec![block_param(), kind_param(false)];
    set_kind.extend(shape_params());

    let mut insert_markdown = vec![text_param(
        "markdown",
        "Markdown, a block per line: `# heading`, `- bullet`, `1. numbered`, `- [ ] todo`, `- [x] done`, `> quote`, a fenced code block, `---`; inline **bold**, _italic_, ~~strike~~, `code`, [link](url)",
    )];
    insert_markdown.extend(placement_params());

    let mut move_params = vec![block_param()];
    move_params.extend(placement_params());

    vec![
        ActionDecl::new("insert-block", "Insert block", insert, |args, _| {
            let shape = shape_of(args, Some(BlockKind::Paragraph))?;
            Ok(RichTextAction::InsertBlocks {
                at: placement_of(args)?,
                blocks: vec![NewBlock {
                    shape,
                    text: args.text("text").unwrap_or_default().to_owned(),
                }],
            })
        })
        .describe("Insert one block of any kind (paragraph, heading, bullet, numbered, checklist, quote, code) after a block id, or at the start or end of the page. Answers the new block's id.")
        .answering(),
        ActionDecl::new(
            "insert-markdown",
            "Insert Markdown",
            insert_markdown,
            |args, _| {
                Ok(RichTextAction::InsertMarkdown {
                    at: placement_of(args)?,
                    markdown: args
                        .text("markdown")
                        .ok_or("give the markdown")?
                        .to_owned(),
                })
            },
        )
        .describe("Insert Markdown as blocks after a block id, or at the start or end of the page. Answers the new blocks' ids.")
        .answering(),
        ActionDecl::new(
            "set-block-text",
            "Set block text",
            vec![
                block_param(),
                text_param("text", "The block's new text"),
                optional(
                    "markdown",
                    ParamType::Boolean,
                    "Read `text` as inline Markdown (**bold**, _italic_, `code`, [link](url)); default plain text, where the block's marks stay as they are",
                ),
            ],
            |args, _| {
                Ok(RichTextAction::SetBlockText {
                    block: block_of(args)?,
                    text: args.text("text").ok_or("give the text")?.to_owned(),
                    markdown: args.boolean("markdown").unwrap_or(false),
                })
            },
        )
        .describe("Replace one block's text, changing only the characters that differ. Answers the block's id.")
        .answering(),
        ActionDecl::new("set-block-kind", "Set block kind", set_kind, |args, _| {
            let named = args
                .text("kind")
                .is_some_and(|kind| !kind.trim().is_empty());
            if named {
                Ok(RichTextAction::SetBlockShape {
                    block: block_of(args)?,
                    shape: shape_of(args, None)?,
                })
            } else {
                // Without a kind, `done` alone ticks, unticks or toggles an item.
                Ok(RichTextAction::SetBlockDone {
                    block: block_of(args)?,
                    done: args.boolean("done"),
                })
            }
        })
        .describe("Turn a block into a paragraph, heading, bullet, numbered or checklist item, quote or code line, keeping its text; or, with no kind, tick, untick (`done`) or toggle (leave `done` out) a list item, which becomes a checklist item. Answers the block's id.")
        .answering(),
        ActionDecl::new(
            "indent-block",
            "Indent or outdent item",
            vec![
                block_param(),
                optional(
                    "levels",
                    ParamType::Integer,
                    "How many levels to indent (default 1); negative outdents",
                ),
            ],
            |args, _| shifted(args),
        )
        .describe("Indent a list item, or outdent it with a negative `levels`. Answers the block's id.")
        .answering(),
        ActionDecl::new("move-block", "Move block", move_params, |args, _| {
            Ok(RichTextAction::MoveBlock {
                block: block_of(args)?,
                at: placement_of(args)?,
            })
        })
        .describe("Move a block to after another block id, or to the start or end of the page. Answers the block's id.")
        .answering(),
        ActionDecl::new(
            "delete-blocks",
            "Delete blocks",
            vec![ActionParam::required_owned(
                "blocks",
                ParamType::Array,
                "The block ids to delete, as the read shows them after ^",
            )],
            |args, _| {
                let blocks: Vec<String> = args
                    .array("blocks")
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|id| id.as_str().map(str::to_owned))
                    .collect();
                if blocks.is_empty() {
                    return Err("name the blocks to delete".to_owned());
                }
                Ok(RichTextAction::DeleteBlocks { blocks })
            },
        )
        .describe("Delete blocks. Answers the ids removed.")
        .answering(),
        ActionDecl::new(
            "replace-body",
            "Replace body from Markdown",
            vec![text_param(
                "markdown",
                "The whole new body as Markdown, as the read shows it. Keep a line's ^id to keep its block; a line without one is a new block",
            )],
            |args, _| {
                Ok(RichTextAction::SyncBody {
                    markdown: args.text("markdown").ok_or("give the markdown")?.to_owned(),
                })
            },
        )
        .describe("Replace the page body from Markdown as one undoable edit. Blocks whose line is unchanged, or whose ^id is kept, stay the same blocks (so their ids, history and collaborators' carets hold); only what differs is edited, inserted or removed. Answers the ids it touched.")
        .answering(),
    ]
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// The body of the rich text under `root` as Markdown with block ids.
///
/// # Errors
/// Returns why `root` holds no rich text.
pub fn read_markdown_under(doc: &AutoCommit, root: &ObjId) -> Result<String, String> {
    let c = tilo_doc::model::open(doc, root).map_err(|error| error.to_string())?;
    let blocks = tilo_doc::model::materialize_region(doc, &c, Region::Body);
    let shown = short_ids(blocks.iter().map(|block| block.id.as_str()));
    Ok(render(&blocks, &shown, true))
}

/// What a read of the rich text under `root` answers: its Markdown with ids,
/// or, naming `block`, that block's line.
///
/// # Errors
/// Returns why `root` holds no rich text, or `block` no block.
pub fn read_under(
    doc: &AutoCommit,
    root: &ObjId,
    block: Option<&str>,
) -> Result<serde_json::Value, String> {
    let c = tilo_doc::model::open(doc, root).map_err(|error| error.to_string())?;
    let blocks = tilo_doc::model::materialize_region(doc, &c, Region::Body);
    let shown = short_ids(blocks.iter().map(|block| block.id.as_str()));
    let Some(token) = block else {
        return Ok(json!({
            "markdown": render(&blocks, &shown, true),
            "blocks": blocks.len(),
        }));
    };
    let id = crate::block_markdown::resolve(blocks.iter().map(|block| block.id.as_str()), token)?;
    let one: Vec<_> = blocks
        .iter()
        .filter(|block| block.id == id)
        .cloned()
        .collect();
    Ok(json!({ "markdown": render(&one, &shown, true), "blocks": 1 }))
}

/// The blocks of the rich text under `root` whose text holds `text`, as
/// `{block, text}` with each block's id as the read shows it.
///
/// # Errors
/// Returns why `root` holds no rich text.
pub fn find_under(
    doc: &AutoCommit,
    root: &ObjId,
    text: &str,
    case_sensitive: bool,
) -> Result<serde_json::Value, String> {
    let c = tilo_doc::model::open(doc, root).map_err(|error| error.to_string())?;
    let blocks = tilo_doc::model::materialize_region(doc, &c, Region::Body);
    let shown = short_ids(blocks.iter().map(|block| block.id.as_str()));
    let needle = if case_sensitive {
        text.to_owned()
    } else {
        text.to_lowercase()
    };
    let hits: Vec<_> = blocks
        .iter()
        .filter(|block| {
            !needle.is_empty()
                && if case_sensitive {
                    block.text.contains(&needle)
                } else {
                    block.text.to_lowercase().contains(&needle)
                }
        })
        .collect();
    let listed: Vec<serde_json::Value> = hits
        .iter()
        .take(MAX_LISTED)
        .map(|block| {
            let id = shown.get(block.id.as_str()).cloned().unwrap_or_default();
            json!({ "block": id, "text": excerpt(&block.text) })
        })
        .collect();
    let mut answer = json!({ "matches": listed });
    if hits.len() > MAX_LISTED
        && let Some(map) = answer.as_object_mut()
    {
        map.insert("more".into(), json!(hits.len().saturating_sub(MAX_LISTED)));
    }
    Ok(answer)
}

fn excerpt(text: &str) -> String {
    const CHARS: usize = 120;
    if text.chars().count() <= CHARS {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(CHARS).collect();
    cut.push('…');
    cut
}

// ---------------------------------------------------------------------------
// Answers
// ---------------------------------------------------------------------------

/// The blocks `changes`, an edit's patches over `doc`, touched, as
/// `{created, changed, removed}` ids in the form the read shows them (an empty
/// list is left out). `root` is where the rich text lives.
#[must_use]
pub fn changes_under(
    doc: &AutoCommit,
    root: &ObjId,
    changes: &[automerge::Patch],
) -> serde_json::Value {
    let Ok(c) = tilo_doc::model::open(doc, root) else {
        return json!({});
    };
    let (touched, created) = touched_blocks(&c, changes);
    let order = tilo_doc::model::blocks_in_order(doc, &c);
    let present: BTreeSet<&str> = order.iter().map(String::as_str).collect();
    let shown = short_ids(
        order
            .iter()
            .map(String::as_str)
            .chain(touched.iter().map(String::as_str)),
    );
    let name = |id: &str| shown.get(id).cloned().unwrap_or_else(|| id.to_owned());
    let mut answer = serde_json::Map::new();
    let in_body = |wanted_new: bool| -> Vec<String> {
        order
            .iter()
            .filter(|id| {
                touched.contains(id.as_str()) && created.contains(id.as_str()) == wanted_new
            })
            .map(|id| name(id))
            .collect()
    };
    for (key, ids) in [
        ("created", in_body(true)),
        ("changed", in_body(false)),
        (
            "removed",
            touched
                .iter()
                .filter(|id| !present.contains(id.as_str()))
                .map(|id| name(id))
                .collect(),
        ),
    ] {
        if ids.is_empty() {
            continue;
        }
        let more = ids.len().saturating_sub(MAX_LISTED);
        answer.insert(
            key.to_owned(),
            json!(ids.into_iter().take(MAX_LISTED).collect::<Vec<_>>()),
        );
        if more > 0 {
            answer.insert(format!("{key}_more"), json!(more));
        }
    }
    serde_json::Value::Object(answer)
}

/// The ids of the blocks `changes` touched, and those among them it created.
fn touched_blocks(
    c: &Containers,
    changes: &[automerge::Patch],
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut touched = BTreeSet::new();
    let mut created = BTreeSet::new();
    for patch in changes {
        let under = patch
            .path
            .iter()
            .find(|(object, _)| *object == c.blocks)
            .map(|(_, prop)| prop.clone());
        let direct = (patch.obj == c.blocks)
            .then(|| match &patch.action {
                PatchAction::PutMap { key, value, .. } => {
                    if matches!(value.0, Value::Object(_)) {
                        created.insert(key.clone());
                    }
                    Some(Prop::Map(key.clone()))
                }
                PatchAction::DeleteMap { key } => Some(Prop::Map(key.clone())),
                _ => None,
            })
            .flatten();
        // A tombstone key (`~deleted:<id>`) is the delete's own record.
        if let Some(Prop::Map(id)) = under.or(direct)
            && !id.starts_with("~deleted:")
        {
            touched.insert(id);
        }
    }
    (touched, created)
}

/// The answer of the declared action `action` after it applied to the rich
/// text under `root`, when it is one that answers.
#[must_use]
pub fn action_result_under(
    doc: &AutoCommit,
    root: &ObjId,
    action: &str,
    changes: &[automerge::Patch],
) -> Option<serde_json::Value> {
    ANSWERING
        .contains(&action)
        .then(|| changes_under(doc, root, changes))
}

/// The [`RichTextAction`] variants the declared actions above build.
const BLOCK_VARIANTS: [&str; 9] = [
    "InsertBlocks",
    "InsertMarkdown",
    "SetBlockText",
    "SetBlockShape",
    "SetBlockDone",
    "ShiftIndent",
    "MoveBlock",
    "DeleteBlocks",
    "SyncBody",
];

/// The types only those variants use.
const BLOCK_TYPES: [&str; 4] = ["BlockShape", "NewBlock", "Placement", "BlockKind"];

/// `schema`, the schema of [`RichTextAction`], without the variants that only
/// restate a declared action here: `apply` still decodes them, and each is
/// published once, with typed and described parameters, as the action that
/// builds it.
#[must_use]
pub(crate) fn without_declared_variants(schema: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match schema {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter(|(key, _)| !BLOCK_TYPES.contains(&key.as_str()))
                .map(|(key, value)| {
                    let value = match (key.as_str(), value) {
                        ("oneOf", Value::Array(alternatives)) => Value::Array(
                            alternatives
                                .iter()
                                .filter(|alternative| !names_block_variant(alternative))
                                .map(without_declared_variants)
                                .collect(),
                        ),
                        _ => without_declared_variants(value),
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        Value::Array(values) => {
            Value::Array(values.iter().map(without_declared_variants).collect())
        }
        other => other.clone(),
    }
}

fn names_block_variant(alternative: &serde_json::Value) -> bool {
    alternative
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|properties| {
            !properties.is_empty()
                && properties
                    .keys()
                    .all(|key| BLOCK_VARIANTS.contains(&key.as_str()))
        })
}
