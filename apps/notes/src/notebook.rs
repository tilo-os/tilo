//! Versioned canonical notebook schema. Validation and projection never repair a document.
//!
//! Page links retain their non-nil UUID after a target is deleted. Tree and tag
//! endpoints must exist; a surviving text link need not have a surviving page.
use crate::{
    doc::NotesDoc,
    model::{PageId, PageMeta, TagAssignment, TagMeta, Workspace, WorkspaceData},
};
use automerge::{
    AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable,
};
use std::collections::{HashMap, HashSet};

pub const NOTES_SCHEMA_VERSION: u32 = 2;
const OWNED_FIELDS: [&str; 6] = [
    "notes_schema_version",
    "pages",
    "tags",
    "tag_assignments",
    "assets",
    "notes_import_v1",
];
/// Leaves headroom below the collaboration transport's 3 MiB document-frame
/// cap for Automerge/action encoding overhead.
pub const MAX_ASSET_BYTES: usize = 1024 * 1024;

/// Initialize only a new notebook; existing Notes data is never reset or stamped.
/// # Errors
/// Refuses preexisting Notes fields or an Automerge mutation failure.
pub fn initialize_empty(doc: &mut AutoCommit) -> Result<(), String> {
    for key in OWNED_FIELDS {
        if !doc
            .get_all(ROOT, key)
            .map_err(|e| e.to_string())?
            .is_empty()
        {
            return Err("notebook already contains Notes data; migration required".into());
        }
    }
    for key in ["pages", "tags", "tag_assignments", "assets"] {
        doc.put_object(ROOT, key, ObjType::Map)
            .map_err(|e| e.to_string())?;
    }
    doc.put(
        ROOT,
        "notes_schema_version",
        u64::from(NOTES_SCHEMA_VERSION),
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Validate schema, complete model and native formatting without writing anything.
/// # Errors
/// Returns a migration-required or malformed-notebook diagnostic.
pub fn validate(doc: &AutoCommit) -> Result<(), String> {
    validated_workspace(doc, true).map(|_| ())
}

/// Project the current document, retaining a local selection only while it exists.
/// # Errors
/// Returns the same schema/structure errors as [`validate`].
pub fn project(doc: &AutoCommit, selected: Option<PageId>) -> Result<Workspace, String> {
    project_impl(doc, selected, true)
}

/// Per-frame projection after the component has validated the current asset
/// revision separately. Image payloads can be large, so ordinary text edits
/// must not hash every unchanged byte again.
pub(crate) fn project_without_asset_rehash(
    doc: &AutoCommit,
    selected: Option<PageId>,
) -> Result<Workspace, String> {
    project_impl(doc, selected, false)
}

fn project_impl(
    doc: &AutoCommit,
    selected: Option<PageId>,
    validate_image_bytes: bool,
) -> Result<Workspace, String> {
    let mut workspace = validated_workspace(doc, validate_image_bytes)?;
    workspace.data.selected =
        selected.filter(|id| workspace.data.pages.iter().any(|page| page.id == *id));
    Ok(workspace)
}

pub(crate) fn required<'a>(
    doc: &'a AutoCommit,
    object: &ObjId,
    key: &str,
) -> Result<(Value<'a>, ObjId), String> {
    let mut values = doc.get_all(object, key).map_err(|e| e.to_string())?;
    if values.len() != 1 {
        return Err(format!("{key} must have exactly one value"));
    }
    values.pop().ok_or_else(|| format!("missing {key}"))
}
fn object(doc: &AutoCommit, parent: &ObjId, key: &str, kind: ObjType) -> Result<ObjId, String> {
    let (value, id) = required(doc, parent, key)?;
    if value != Value::Object(kind) {
        return Err(format!("{key} must be {kind:?}"));
    }
    Ok(id)
}
pub(crate) fn string(doc: &AutoCommit, parent: &ObjId, key: &str) -> Result<String, String> {
    match required(doc, parent, key)?.0 {
        Value::Scalar(value) => match value.as_ref() {
            ScalarValue::Str(s) => Ok(s.to_string()),
            _ => Err(format!("{key} must be a string")),
        },
        Value::Object(_) => Err(format!("{key} must be a string")),
    }
}
fn text(doc: &AutoCommit, parent: &ObjId, key: &str) -> Result<String, String> {
    doc.text(object(doc, parent, key, ObjType::Text)?)
        .map_err(|e| e.to_string())
}
fn integer(doc: &AutoCommit, parent: &ObjId, key: &str) -> Result<u64, String> {
    let (value, _) = required(doc, parent, key)?;
    match value {
        Value::Scalar(s) => match s.as_ref() {
            ScalarValue::Uint(n) => Ok(*n),
            ScalarValue::Int(n) => u64::try_from(*n).map_err(|_| format!("{key} is negative")),
            _ => Err(format!("{key} must be an integer")),
        },
        Value::Object(_) => Err(format!("{key} must be an integer")),
    }
}

fn schema_version(doc: &AutoCommit) -> Result<u64, String> {
    let version = |value: &Value<'_>| match value {
        Value::Scalar(value) => match value.as_ref() {
            ScalarValue::Uint(version) => Some(*version),
            ScalarValue::Int(version) => u64::try_from(*version).ok(),
            _ => None,
        },
        Value::Object(_) => None,
    };
    let values = doc
        .get_all(ROOT, "notes_schema_version")
        .map_err(|e| e.to_string())?;
    let first = values
        .first()
        .and_then(|(value, _)| version(value))
        .ok_or("notebook schema version is missing or malformed")?;
    // Two replicas can independently perform the same additive upgrade. Equal
    // concurrent version stamps describe one schema, not a corrupt notebook.
    if values
        .iter()
        .any(|(value, _)| version(value) != Some(first))
    {
        return Err("notebook schema versions conflict".into());
    }
    Ok(first)
}
fn uuid(value: &str) -> Result<uuid::Uuid, String> {
    value
        .parse::<uuid::Uuid>()
        .ok()
        .filter(|id| !id.is_nil())
        .ok_or_else(|| format!("invalid non-nil UUID {value}"))
}
fn keyed_uuid(value: &str) -> Result<uuid::Uuid, String> {
    let id = uuid(value)?;
    if id.to_string() != value {
        return Err(format!("noncanonical UUID map key {value}"));
    }
    Ok(id)
}

fn validated_workspace(doc: &AutoCommit, validate_image_bytes: bool) -> Result<Workspace, String> {
    if !matches!(schema_version(doc).ok(), Some(1 | 2)) {
        return Err("notebook schema version is missing or unsupported; migration required".into());
    }
    let pages = object(doc, &ROOT, "pages", ObjType::Map)?;
    let tags = object(doc, &ROOT, "tags", ObjType::Map)?;
    let assignments = object(doc, &ROOT, "tag_assignments", ObjType::Map)?;
    if validate_image_bytes {
        validate_assets(doc)?;
    } else {
        validate_asset_shape(doc)?;
    }
    let mut data = WorkspaceData::default();
    for key in doc.keys(&pages) {
        let id = keyed_uuid(&key)?;
        let page = object(doc, &pages, &key, ObjType::Map)?;
        let parent = string(doc, &page, "parent")?;
        let parent = if parent.is_empty() {
            None
        } else {
            Some(uuid(&parent)?)
        };
        let order = u32::try_from(integer(doc, &page, "order")?)
            .map_err(|_| format!("page {id} order exceeds u32"))?;
        data.pages.push(PageMeta {
            id,
            title: text(doc, &page, "title")?,
            parent,
            order,
        });
        validate_blocks(doc, &page).map_err(|e| format!("page {id}: {e}"))?;
    }
    for key in doc.keys(&tags) {
        let id = keyed_uuid(&key)?;
        let entry = object(doc, &tags, &key, ObjType::Map)?;
        data.tags.push(TagMeta {
            id,
            name: text(doc, &entry, "name")?,
        });
    }
    for key in doc.keys(&assignments) {
        let (page, tag) = key
            .split_once(':')
            .ok_or_else(|| format!("invalid tag assignment {key}"))?;
        let page = keyed_uuid(page)?;
        let tag = keyed_uuid(tag)?;
        if !matches!(required(doc,&assignments,&key)?.0,Value::Scalar(s) if s.as_ref()==&ScalarValue::Boolean(true))
        {
            return Err(format!("tag assignment {key} must be true"));
        }
        data.tag_assignments.push(TagAssignment { page, tag });
    }
    let workspace = Workspace { data };
    workspace.check_invariants()?;
    validate_receipt(doc)?;
    // The established NotesDoc readers determine display order after strict
    // validation has ruled out their permissive malformed-data fallbacks.
    Ok(Workspace {
        data: WorkspaceData {
            pages: NotesDoc::list_pages(doc),
            tags: NotesDoc::list_tags(doc),
            tag_assignments: workspace.data.tag_assignments,
            selected: None,
        },
    })
}

fn asset_digest(value: &str) -> Option<&str> {
    let digest = value.strip_prefix("asset:").unwrap_or(value);
    (digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some(digest)
}

fn assets_map(doc: &AutoCommit) -> Result<Option<ObjId>, String> {
    let values = doc.get_all(ROOT, "assets").map_err(|e| e.to_string())?;
    if values.is_empty() {
        return Ok(None);
    }
    let [(Value::Object(ObjType::Map), id)] = values.as_slice() else {
        return Err("assets must be one Map".into());
    };
    Ok(Some(id.clone()))
}

fn validate_assets(doc: &AutoCommit) -> Result<(), String> {
    let Some(assets) = assets_map(doc)? else {
        return Ok(());
    };
    for key in doc.keys(&assets) {
        let Some(digest) = asset_digest(&key) else {
            return Err(format!("invalid asset digest {key}"));
        };
        let (value, _) = required(doc, &assets, &key)?;
        let Value::Scalar(value) = value else {
            return Err(format!("asset {key} must be bytes"));
        };
        let ScalarValue::Bytes(bytes) = value.as_ref() else {
            return Err(format!("asset {key} must be bytes"));
        };
        validate_asset_payload(digest, bytes).map_err(|error| format!("asset {key}: {error}"))?;
    }
    Ok(())
}

fn validate_asset_shape(doc: &AutoCommit) -> Result<(), String> {
    let Some(assets) = assets_map(doc)? else {
        return Ok(());
    };
    for key in doc.keys(&assets) {
        if asset_digest(&key).is_none() {
            return Err(format!("invalid asset digest {key}"));
        }
        let (value, _) = required(doc, &assets, &key)?;
        if !matches!(value, Value::Scalar(value) if matches!(value.as_ref(), ScalarValue::Bytes(_)))
        {
            return Err(format!("asset {key} must be bytes"));
        }
    }
    Ok(())
}

/// Cheap identity of every asset value. Automerge operation ids change when a
/// payload is replaced, including same-sized corrupt replacements.
pub(crate) fn asset_manifest(doc: &AutoCommit) -> Result<Vec<(String, String)>, String> {
    let Some(assets) = assets_map(doc)? else {
        return Ok(Vec::new());
    };
    let mut manifest = doc
        .map_range(&assets, ..)
        .map(|item| {
            let id = format!("{:?}", item.id());
            (item.key.into_owned(), id)
        })
        .collect::<Vec<_>>();
    manifest.sort();
    Ok(manifest)
}

/// Validate a content-addressed image payload before it enters the notebook.
///
/// # Errors
/// Returns an error for an invalid digest, unsupported image bytes, size, or hash mismatch.
pub fn validate_asset_payload(digest: &str, bytes: &[u8]) -> Result<(), String> {
    let Some(digest) = asset_digest(digest) else {
        return Err("image asset digest is invalid".into());
    };
    if bytes.is_empty() || bytes.len() > MAX_ASSET_BYTES {
        return Err("image asset must be between 1 byte and 1 MiB".into());
    }
    if !(bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10])
        || bytes.starts_with(&[0xff, 0xd8, 0xff])
        || (bytes.get(..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WEBP")))
    {
        return Err("image asset must be PNG, JPEG, or WebP".into());
    }
    if blake3::hash(bytes).to_hex().as_str() != digest {
        return Err("image asset bytes do not match their digest".into());
    }
    Ok(())
}

/// Store one validated content-addressed image payload.
///
/// # Errors
/// Returns an error when validation fails or the collaborative asset map cannot be updated.
pub fn put_asset(doc: &mut AutoCommit, digest: &str, bytes: &[u8]) -> Result<(), String> {
    validate_asset_payload(digest, bytes)?;
    let assets = match assets_map(doc)? {
        Some(assets) => assets,
        None => doc
            .put_object(ROOT, "assets", ObjType::Map)
            .map_err(|e| e.to_string())?,
    };
    let digest = digest.strip_prefix("asset:").unwrap_or(digest);
    if let Some((Value::Scalar(existing), _)) =
        doc.get(&assets, digest).map_err(|e| e.to_string())?
    {
        return match existing.as_ref() {
            ScalarValue::Bytes(existing) if existing.as_slice() == bytes => Ok(()),
            _ => Err("image asset digest already names different content".into()),
        };
    }
    doc.put(&assets, digest, ScalarValue::Bytes(bytes.to_vec()))
        .map_err(|e| e.to_string())
}

/// Snapshot all validated image assets using their canonical `asset:` keys.
///
/// # Errors
/// Returns an error when the notebook asset schema or any payload is invalid.
pub fn asset_snapshot(doc: &AutoCommit) -> Result<HashMap<String, Vec<u8>>, String> {
    validate_assets(doc)?;
    let Some(assets) = assets_map(doc)? else {
        return Ok(HashMap::new());
    };
    let mut snapshot = HashMap::new();
    for digest in doc.keys(&assets) {
        let Some((Value::Scalar(value), _)) =
            doc.get(&assets, &digest).map_err(|e| e.to_string())?
        else {
            continue;
        };
        if let ScalarValue::Bytes(bytes) = value.as_ref() {
            snapshot.insert(format!("asset:{digest}"), bytes.clone());
        }
    }
    Ok(snapshot)
}

fn validate_blocks(doc: &AutoCommit, page: &ObjId) -> Result<(), String> {
    let blocks = object(doc, page, "blocks", ObjType::List)?;
    let mut ids = HashSet::new();
    let mut positions = HashSet::new();
    for index in 0..doc.length(&blocks) {
        let values = doc.get_all(&blocks, index).map_err(|e| e.to_string())?;
        let [(Value::Object(ObjType::Map), block)] = values.as_slice() else {
            return Err(format!("block {index} must be one Map"));
        };
        let id = uuid(&string(doc, block, "id")?)?;
        if !ids.insert(id) {
            return Err(format!("duplicate block {id}"));
        }
        if schema_version(doc)? >= 2 {
            let rank = crate::block_order::stored(doc, block)
                .ok_or_else(|| format!("block {id} requires order metadata"))?;
            if !crate::block_order::valid(&rank) || !positions.insert(rank) {
                return Err(format!("block {id} has invalid order metadata"));
            }
        }
        if !crate::doc::valid_kind(&string(doc, block, "kind")?) {
            return Err(format!("block {id} has invalid kind"));
        }
        if !doc
            .get_all(block, "nesting")
            .map_err(|e| e.to_string())?
            .is_empty()
        {
            let depth = integer(doc, block, "nesting")?;
            let kind = string(doc, block, "kind")?;
            if depth > u64::from(crate::markdown::MAX_LIST_INDENT)
                || kind.starts_with("bullet:")
                || kind.starts_with("numbered:")
                || kind.starts_with("checklist:")
            {
                return Err(format!("block {id} has invalid nesting"));
            }
        }
        if !doc
            .get_all(block, "code_group")
            .map_err(|e| e.to_string())?
            .is_empty()
        {
            uuid(&string(doc, block, "code_group")?)?;
            if !string(doc, block, "kind")?.starts_with("code:") {
                return Err(format!("block {id} has a code group but is not code"));
            }
        }
        let text_obj = object(doc, block, "text", ObjType::Text)?;
        let body = doc.text(&text_obj).map_err(|e| e.to_string())?;
        if integer(doc, block, "notes_marks_version")? != 1 {
            return Err(format!("block {id} requires native mark migration"));
        }
        for mark in doc.marks(&text_obj).map_err(|e| e.to_string())? {
            let Some(name) = mark.name.strip_prefix("notes.inline.") else {
                continue;
            };
            if mark.start >= mark.end || mark.end > body.chars().count() {
                return Err(format!("block {id} has invalid mark range"));
            }
            match name {
                "bold" | "italic" | "code" | "underline" | "strikethrough"
                    if mark.value == ScalarValue::Boolean(true) => {}
                name if name.starts_with("link:") || name.starts_with("transclusion:") => {
                    let target = name
                        .strip_prefix("link:")
                        .or_else(|| name.strip_prefix("transclusion:"))
                        .ok_or_else(|| "invalid link name".to_string())?;
                    uuid(target)?;
                    if mark.value != ScalarValue::Str(target.into()) {
                        return Err(format!("block {id} has inconsistent link target"));
                    }
                }
                "external-link" => {
                    let ScalarValue::Str(url) = &mark.value else {
                        return Err(format!("block {id} has invalid external link target"));
                    };
                    if !crate::markdown::safe_external_url(url) {
                        return Err(format!("block {id} has invalid external link target"));
                    }
                }
                _ => return Err(format!("block {id} has invalid native mark {name}")),
            }
        }
    }
    Ok(())
}

fn validate_receipt(doc: &AutoCommit) -> Result<(), String> {
    if doc
        .get_all(ROOT, "notes_import_v1")
        .map_err(|e| e.to_string())?
        .is_empty()
    {
        return Ok(());
    }
    let receipt = object(doc, &ROOT, "notes_import_v1", ObjType::Map)?;
    let mut keys = doc.keys(&receipt).collect::<Vec<_>>();
    keys.sort();
    if keys
        != [
            "destination_id",
            "format",
            "snapshot_digest",
            "source_key",
            "version",
        ]
    {
        return Err("import receipt has unsupported fields".into());
    }
    let key = string(doc, &receipt, "source_key")?;
    let digest = string(doc, &receipt, "snapshot_digest")?;
    let destination = string(doc, &receipt, "destination_id")?;
    let historical_destination = destination == format!("derived:notes-legacy-v1@{key}");
    if key.len() != 64
        || !key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || digest.len() != 64
        || !digest.bytes().all(|b| b.is_ascii_hexdigit())
        || (historical_destination
            && !digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    {
        return Err("import receipt has invalid digest or source key".into());
    }
    if string(doc, &receipt, "format")? != "workspace-json-markdown-v1"
        || integer(doc, &receipt, "version")? != 1
        || (!historical_destination && ui::record::RecordId::parse(&destination).is_none())
    {
        return Err("unsupported import receipt".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{InlineMark, InlineMarkSpan, NotesAction};
    use automerge::marks::{ExpandMark, Mark};

    fn linked_notebook(mark: InlineMark) -> (AutoCommit, PageId, ObjId) {
        let mut doc = AutoCommit::new();
        initialize_empty(&mut doc).unwrap();
        let page = uuid::Uuid::new_v4();
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta {
                id: page,
                title: "Reference".into(),
                parent: None,
                order: 0,
            },
        );
        let list = NotesDoc::page_blocks_list(&doc, page).unwrap();
        let text = NotesDoc::push_block(&mut doc, &list, "paragraph", "Read this");
        let id = NotesDoc::page_block_snapshots(&doc, page)[0].0.clone();
        NotesDoc::set_block_marks(
            &mut doc,
            page,
            &id,
            &[InlineMarkSpan {
                start: 0,
                end: 9,
                mark,
            }],
        )
        .unwrap();
        (doc, page, text)
    }

    #[test]
    fn quote_and_table_edits_survive_canonical_reopen() {
        let (mut doc, page, _) = linked_notebook(InlineMark::Bold);
        let first = NotesDoc::page_block_snapshots(&doc, page)[0].0.clone();
        for kind in ["quote", "table:0", "table:1"] {
            NotesDoc::apply_block_ops(
                &mut doc,
                page,
                &[ui::widget::BlockEditOp::SetKind {
                    block_id: first.clone(),
                    kind: kind.into(),
                }],
            )
            .unwrap();
            let mut reopened = AutoCommit::load(&doc.save()).unwrap();
            project(&reopened, Some(page)).unwrap();
            let list = NotesDoc::page_blocks_list(&reopened, page).unwrap();
            assert_eq!(NotesDoc::block_kind(&reopened, &list, 0), kind);
            NotesDoc::apply_block_ops(
                &mut reopened,
                page,
                &[ui::widget::BlockEditOp::Insert {
                    block_id: uuid::Uuid::new_v4().to_string(),
                    after: Some(first.clone()),
                    kind: kind.into(),
                    text: "new block".into(),
                }],
            )
            .unwrap();
            validate(&reopened).unwrap();
            assert_eq!(NotesDoc::block_kind(&reopened, &list, 1), kind);
        }
        for malformed in ["table:2", "table:true", "quote:1"] {
            assert!(!crate::doc::valid_kind(malformed));
        }
    }

    #[test]
    fn content_addressed_image_assets_survive_reopen_and_reject_bad_payloads() {
        use tilo_actions::Action;
        let mut doc = AutoCommit::new();
        initialize_empty(&mut doc).unwrap();
        let page = uuid::Uuid::new_v4();
        crate::actions::new_page_action(page, None, 0, "Images")
            .apply(&mut doc)
            .unwrap();
        let bytes = vec![137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 0];
        let action = crate::actions::add_image_asset_action(
            page,
            bytes.clone(),
            ui::widget::BlockEditOp::Insert {
                block_id: uuid::Uuid::new_v4().to_string(),
                after: None,
                kind: "paragraph".into(),
                text: format!("![Scan](asset:{})", blake3::hash(&bytes).to_hex()),
            },
        );
        action.apply(&mut doc).unwrap();
        validate(&doc).unwrap();
        let source = format!("asset:{}", blake3::hash(&bytes).to_hex());
        assert_eq!(asset_snapshot(&doc).unwrap().get(&source), Some(&bytes));
        let reopened = AutoCommit::load(&doc.save()).unwrap();
        validate(&reopened).unwrap();
        assert_eq!(
            asset_snapshot(&reopened).unwrap().get(&source),
            Some(&bytes)
        );

        let before = NotesDoc::page_block_snapshots(&reopened, page);
        let invalid = crate::actions::NotesAction::AddImageAsset {
            page: page.to_string(),
            digest: "0".repeat(64),
            bytes: b"not an image".to_vec(),
            op: ui::widget::BlockEditOp::Insert {
                block_id: uuid::Uuid::new_v4().to_string(),
                after: None,
                kind: "paragraph".into(),
                text: "![Bad](asset:bad)".into(),
            },
        };
        let mut rejected = reopened;
        assert!(invalid.apply(&mut rejected).is_err());
        assert_eq!(NotesDoc::page_block_snapshots(&rejected, page), before);
    }

    #[test]
    fn image_source_edits_persist_through_actions_and_undo_but_view_modes_do_not() {
        use ui::Widget;
        let original = "![Scan](assets/original.png)";
        let edited = "![Updated scan](assets/revised.png)";
        let (mut doc, page, _) = linked_notebook(InlineMark::Bold);
        let id = NotesDoc::page_block_snapshots(&doc, page)[0].0.clone();
        NotesDoc::apply_block_ops(
            &mut doc,
            page,
            &[ui::widget::BlockEditOp::TextSplice {
                block_id: id.clone(),
                start: 0,
                end: 9,
                text: original.into(),
            }],
        )
        .unwrap();
        NotesDoc::set_block_marks(&mut doc, page, &id, &[]).unwrap();
        doc.commit();
        let mut state = ui::State::new();
        state.doc = doc;
        let list = NotesDoc::page_blocks_list(&state.doc, page).unwrap();
        let mut editor = ui::widget::BlockEditor::new();
        editor.set_action_interception(true);
        editor.bind_existing_blocks(&state.doc, list).unwrap();
        editor.sync(&mut state.doc, &ROOT);
        let initial_heads = state.doc.get_heads();
        assert!(editor.perform_semantic_action(
            &format!("notes.image-edit:{id}"),
            &ui::a11y::Action::Activate,
            &mut state.doc,
            &ROOT
        ));
        assert!(editor.take_pending_semantic_ops().is_empty());
        assert_eq!(state.doc.get_heads(), initial_heads);
        assert!(editor.perform_semantic_action(
            &format!("notes.image-source:{id}"),
            &ui::a11y::Action::SetValue(edited.into()),
            &mut state.doc,
            &ROOT
        ));
        let ops = editor.take_pending_semantic_ops();
        assert!(
            !ops.is_empty(),
            "source edits must use canonical semantic block operations"
        );
        let action = crate::actions::apply_block_ops_action(page, ops);
        tilo_actions::apply_local(&mut state.doc, &action, crate::actions::encode(&action))
            .unwrap();
        state.record_committed_action(initial_heads, None, None);
        assert!(editor.perform_semantic_action(
            &format!("notes.image-done:{id}"),
            &ui::a11y::Action::Activate,
            &mut state.doc,
            &ROOT
        ));
        assert!(editor.take_pending_semantic_ops().is_empty());
        let reopened = AutoCommit::load(&state.doc.save()).unwrap();
        validate(&reopened).unwrap();
        assert_eq!(NotesDoc::page_block_snapshots(&reopened, page)[0].1, edited);
        state.undo(None, None).unwrap();
        assert_eq!(
            NotesDoc::page_block_snapshots(&state.doc, page)[0].1,
            original
        );
    }

    #[test]
    fn external_links_and_embeds_reopen_and_duplicate_without_losing_marks() {
        for mark in [
            InlineMark::ExternalLink {
                url: "https://example.com/research?q=notes".into(),
            },
            InlineMark::Transclusion {
                page: uuid::Uuid::new_v4().to_string(),
            },
        ] {
            let (mut doc, page, _) = linked_notebook(mark.clone());
            let mut reopened = AutoCommit::load(&doc.save()).unwrap();
            let workspace = project(&reopened, Some(page)).unwrap();
            assert_eq!(workspace.selected(), Some(page));
            assert_eq!(
                NotesDoc::page_block_marks(&reopened, page)[0].1[0].mark,
                mark
            );
            let NotesAction::DuplicateSubtree { pages, .. } =
                crate::actions::duplicate_subtree_action(&reopened, page).unwrap()
            else {
                panic!("duplicate action expected");
            };
            NotesDoc::duplicate_subtree(&mut reopened, page, &pages).unwrap();
            let workspace = project(&reopened, Some(page)).unwrap();
            let copy = workspace
                .pages()
                .iter()
                .find(|note| note.id != page)
                .unwrap();
            assert_eq!(
                NotesDoc::page_block_marks(&reopened, copy.id)[0].1[0].mark,
                mark
            );
        }
    }

    #[test]
    fn external_link_validation_refuses_unsafe_urls_and_non_string_values() {
        for value in [
            ScalarValue::Str("javascript:alert(1)".into()),
            ScalarValue::Boolean(true),
        ] {
            let (mut doc, _, text) = linked_notebook(InlineMark::Bold);
            doc.mark(
                &text,
                Mark::new("notes.inline.external-link".into(), value, 0, 9),
                ExpandMark::After,
            )
            .unwrap();
            assert!(
                validate(&doc)
                    .unwrap_err()
                    .contains("invalid external link target")
            );
        }
    }

    #[test]
    fn embed_validation_refuses_a_mismatched_target() {
        let (mut doc, _, text) = linked_notebook(InlineMark::Bold);
        let target = uuid::Uuid::new_v4();
        doc.mark(
            &text,
            Mark::new(
                format!("notes.inline.transclusion:{target}"),
                ScalarValue::Str(uuid::Uuid::new_v4().to_string().into()),
                0,
                9,
            ),
            ExpandMark::After,
        )
        .unwrap();
        assert!(
            validate(&doc)
                .unwrap_err()
                .contains("inconsistent link target")
        );
    }
}
