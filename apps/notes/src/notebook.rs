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
use std::collections::HashSet;

pub const NOTES_SCHEMA_VERSION: u32 = 1;
const OWNED_FIELDS: [&str; 5] = [
    "notes_schema_version",
    "pages",
    "tags",
    "tag_assignments",
    "notes_import_v1",
];

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
    for key in ["pages", "tags", "tag_assignments"] {
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
    validated_workspace(doc).map(|_| ())
}

/// Project the current document, retaining a local selection only while it exists.
/// # Errors
/// Returns the same schema/structure errors as [`validate`].
pub fn project(doc: &AutoCommit, selected: Option<PageId>) -> Result<Workspace, String> {
    let mut workspace = validated_workspace(doc)?;
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

fn validated_workspace(doc: &AutoCommit) -> Result<Workspace, String> {
    if integer(doc, &ROOT, "notes_schema_version").ok() != Some(u64::from(NOTES_SCHEMA_VERSION)) {
        return Err("notebook schema version is missing or unsupported; migration required".into());
    }
    let pages = object(doc, &ROOT, "pages", ObjType::Map)?;
    let tags = object(doc, &ROOT, "tags", ObjType::Map)?;
    let assignments = object(doc, &ROOT, "tag_assignments", ObjType::Map)?;
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

fn validate_blocks(doc: &AutoCommit, page: &ObjId) -> Result<(), String> {
    let blocks = object(doc, page, "blocks", ObjType::List)?;
    let mut ids = HashSet::new();
    for index in 0..doc.length(&blocks) {
        let values = doc.get_all(&blocks, index).map_err(|e| e.to_string())?;
        let [(Value::Object(ObjType::Map), block)] = values.as_slice() else {
            return Err(format!("block {index} must be one Map"));
        };
        let id = uuid(&string(doc, block, "id")?)?;
        if !ids.insert(id) {
            return Err(format!("duplicate block {id}"));
        }
        if !crate::doc::valid_kind(&string(doc, block, "kind")?) {
            return Err(format!("block {id} has invalid kind"));
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
                name if name.starts_with("link:") => {
                    let target = name
                        .strip_prefix("link:")
                        .ok_or_else(|| "invalid link name".to_string())?;
                    uuid(target)?;
                    if mark.value != ScalarValue::Str(target.into()) {
                        return Err(format!("block {id} has inconsistent link target"));
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
    if key.len() != 64
        || !key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || digest.len() != 64
        || !digest.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("import receipt has invalid digest or source key".into());
    }
    if string(doc, &receipt, "format")? != "workspace-json-markdown-v1"
        || integer(doc, &receipt, "version")? != 1
        || ui::record::RecordId::parse(&string(doc, &receipt, "destination_id")?).is_none()
    {
        return Err("unsupported import receipt".into());
    }
    Ok(())
}
