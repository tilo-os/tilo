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
use std::collections::HashMap;

/// 3: page bodies are rich text (`body`).
pub const NOTES_SCHEMA_VERSION: u32 = 3;
const OWNED_FIELDS: [&str; 6] = [
    "notes_schema_version",
    "pages",
    "tags",
    "tag_assignments",
    "assets",
    "notes_import_v1",
];
/// Largest image a note accepts. The notebook records only a reference and
/// the bytes live in the device content store, but the insert action still
/// carries the bytes once, so this stays well inside one action payload.
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
    if schema_version(doc).ok() != Some(u64::from(NOTES_SCHEMA_VERSION)) {
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
        validate_body(doc, &page).map_err(|e| format!("page {id}: {e}"))?;
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

/// One image asset as the notebook stores it.
///
/// Notebooks written before images moved into the content store carry the
/// bytes inline; they stay readable, and
/// [`NotesAction::MoveAssetsToContentStore`](crate::actions::NotesAction::MoveAssetsToContentStore)
/// replaces them with references. A reference is a small map
/// `{len, mime}` under the content digest; the bytes are in the app's
/// content store ([`content`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredAsset {
    Inline(Vec<u8>),
    Reference { len: u64, mime: String },
}

const REFERENCE_MIMES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];

fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.get(..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

fn stored_asset(doc: &AutoCommit, assets: &ObjId, key: &str) -> Result<StoredAsset, String> {
    // The key is the content digest, so every value under it names the same
    // bytes. Two devices adding or moving one image concurrently leave
    // conflicting values; each must be well formed, and a reference wins over
    // inline bytes because it is what every device converges on.
    let values = doc.get_all(assets, key).map_err(|e| e.to_string())?;
    if values.is_empty() {
        return Err(format!("missing {key}"));
    }
    let mut chosen = None;
    for (value, id) in values {
        let asset = match value {
            Value::Scalar(value) => match value.as_ref() {
                ScalarValue::Bytes(bytes) => StoredAsset::Inline(bytes.clone()),
                _ => return Err(format!("asset {key} must be bytes or a reference")),
            },
            Value::Object(ObjType::Map) => {
                let len = integer(doc, &id, "len")?;
                let mime = string(doc, &id, "mime")?;
                if len == 0
                    || len > MAX_ASSET_BYTES as u64
                    || !REFERENCE_MIMES.contains(&mime.as_str())
                {
                    return Err(format!("asset {key} has an invalid reference"));
                }
                StoredAsset::Reference { len, mime }
            }
            Value::Object(_) => return Err(format!("asset {key} must be bytes or a reference")),
        };
        chosen = match (chosen, asset) {
            (Some(StoredAsset::Reference { len, mime }), _)
            | (_, StoredAsset::Reference { len, mime }) => {
                Some(StoredAsset::Reference { len, mime })
            }
            (_, inline) => Some(inline),
        };
    }
    chosen.ok_or_else(|| format!("missing {key}"))
}

fn validate_assets(doc: &AutoCommit) -> Result<(), String> {
    let Some(assets) = assets_map(doc)? else {
        return Ok(());
    };
    for key in doc.keys(&assets) {
        let Some(digest) = asset_digest(&key) else {
            return Err(format!("invalid asset digest {key}"));
        };
        if let StoredAsset::Inline(bytes) = stored_asset(doc, &assets, &key)? {
            validate_asset_payload(digest, &bytes)
                .map_err(|error| format!("asset {key}: {error}"))?;
        }
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
        stored_asset(doc, &assets, &key)?;
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

/// Keep the bytes of one validated image in the content store and record
/// its reference in the notebook. Only the reference reaches collaborators'
/// replicas; their devices fetch the bytes from the content store.
///
/// A process whose content store is not shared (one started outside the
/// launcher) keeps the bytes inline instead, as notebooks did before: a
/// reference to bytes only this device holds would dangle for collaborators.
/// [`move_inline_assets`] moves them once a shared content store is available.
///
/// # Errors
/// Returns an error when validation fails, the content store refuses the
/// bytes, or the collaborative asset map cannot be updated.
pub fn put_asset(doc: &mut AutoCommit, digest: &str, bytes: &[u8]) -> Result<(), String> {
    put_asset_in(content(), doc, digest, bytes)
}

/// The content store this process keeps notebook images in: the device
/// content store when the launcher handed the process one. Notebook actions
/// write images while they apply, where no app handle reaches them, so the
/// store is the process's own, created on first use.
pub fn content() -> &'static tilo_content::ContentStore {
    static CONTENT: std::sync::OnceLock<tilo_content::ContentStore> = std::sync::OnceLock::new();
    CONTENT.get_or_init(|| {
        // A host without native files (a browser page) has no local store for
        // notebook images; it keeps them in memory, as tests do.
        let native = matches!(
            tilo_files::availability(tilo_files::Feature::NativePaths),
            tilo_files::Availability::Available
        );
        if native && !cfg!(test) {
            tilo_content::ContentStore::for_app()
        } else {
            tilo_content::ContentStore::memory()
        }
    })
}

/// [`put_asset`] against an explicit store.
///
/// # Errors
/// As [`put_asset`].
pub fn put_asset_in(
    store: &tilo_content::ContentStore,
    doc: &mut AutoCommit,
    digest: &str,
    bytes: &[u8],
) -> Result<(), String> {
    validate_asset_payload(digest, bytes)?;
    let digest = digest.strip_prefix("asset:").unwrap_or(digest);
    let assets = match assets_map(doc)? {
        Some(assets) => assets,
        None => doc
            .put_object(ROOT, "assets", ObjType::Map)
            .map_err(|e| e.to_string())?,
    };
    let existing = if doc
        .get_all(&assets, digest)
        .map_err(|e| e.to_string())?
        .is_empty()
    {
        None
    } else {
        Some(stored_asset(doc, &assets, digest)?)
    };
    match existing {
        Some(StoredAsset::Inline(existing)) if existing == bytes => Ok(()),
        Some(StoredAsset::Reference { len, .. }) if len == bytes.len() as u64 => {
            // Keep this device able to serve the bytes the reference names.
            store_asset_bytes(store, digest, bytes)
        }
        Some(_) => Err("image asset digest already names different content".into()),
        None if store.shared() => {
            store_asset_bytes(store, digest, bytes)?;
            write_reference(doc, &assets, digest, bytes)
        }
        None => doc
            .put(&assets, digest, ScalarValue::Bytes(bytes.to_vec()))
            .map_err(|e| e.to_string()),
    }
}

fn store_asset_bytes(
    store: &tilo_content::ContentStore,
    digest: &str,
    bytes: &[u8],
) -> Result<(), String> {
    match store.put(bytes) {
        Ok(stored) if stored == digest => Ok(()),
        Ok(_) => Err("image asset bytes do not match their digest".into()),
        Err(error) => Err(format!("the content store refused the image: {error}")),
    }
}

fn write_reference(
    doc: &mut AutoCommit,
    assets: &ObjId,
    digest: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let mime = image_mime(bytes).ok_or("image asset must be PNG, JPEG, or WebP")?;
    let reference = doc
        .put_object(assets, digest, ObjType::Map)
        .map_err(|e| e.to_string())?;
    doc.put(&reference, "len", bytes.len() as u64)
        .map_err(|e| e.to_string())?;
    doc.put(&reference, "mime", mime).map_err(|e| e.to_string())
}

/// Digests of images the notebook still carries inline.
///
/// # Errors
/// Returns an error when the asset map is malformed.
pub fn inline_asset_digests(doc: &AutoCommit) -> Result<Vec<String>, String> {
    let Some(assets) = assets_map(doc)? else {
        return Ok(Vec::new());
    };
    let mut digests = Vec::new();
    for key in doc.keys(&assets) {
        if asset_digest(&key).is_some()
            && matches!(stored_asset(doc, &assets, &key)?, StoredAsset::Inline(_))
        {
            digests.push(key);
        }
    }
    digests.sort();
    Ok(digests)
}

/// Move inline images named by `digests` into the content store and replace
/// them with references. Idempotent: references and unknown digests are left
/// alone, so replaying the move, or moving concurrently on two devices,
/// converges on the same references.
///
/// # Errors
/// Returns an error when an inline payload is invalid or the content store
/// refuses it; nothing is rewritten in that case.
pub fn move_inline_assets(doc: &mut AutoCommit, digests: &[String]) -> Result<usize, String> {
    move_inline_assets_in(content(), doc, digests)
}

/// [`move_inline_assets`] against an explicit store.
///
/// # Errors
/// As [`move_inline_assets`], and when `store` is local to this process.
pub fn move_inline_assets_in(
    store: &tilo_content::ContentStore,
    doc: &mut AutoCommit,
    digests: &[String],
) -> Result<usize, String> {
    if !store.shared() {
        return Err("images stay inline until a content store is available".into());
    }
    let Some(assets) = assets_map(doc)? else {
        return Ok(0);
    };
    let mut moves = Vec::new();
    for digest in digests {
        let Some(digest) = asset_digest(digest) else {
            return Err(format!("invalid asset digest {digest}"));
        };
        if doc
            .get_all(&assets, digest)
            .map_err(|e| e.to_string())?
            .is_empty()
        {
            continue;
        }
        if let StoredAsset::Inline(bytes) = stored_asset(doc, &assets, digest)? {
            validate_asset_payload(digest, &bytes)?;
            store_asset_bytes(store, digest, &bytes)?;
            moves.push((digest.to_owned(), bytes));
        }
    }
    for (digest, bytes) in &moves {
        write_reference(doc, &assets, digest, bytes)?;
    }
    Ok(moves.len())
}

/// Snapshot the image assets whose bytes are readable now, using their
/// canonical `asset:` keys. A referenced image a collaborator added may still
/// be on its way from their device; it is absent until it arrives.
///
/// # Errors
/// Returns an error when the notebook asset schema or any payload is invalid.
pub fn asset_snapshot(doc: &AutoCommit) -> Result<HashMap<String, Vec<u8>>, String> {
    asset_snapshot_with(doc, |hash| content().read(hash))
}

/// [`asset_snapshot`] reading referenced bytes through `read`.
///
/// # Errors
/// As [`asset_snapshot`].
pub fn asset_snapshot_with(
    doc: &AutoCommit,
    read: impl Fn(&str) -> Option<Vec<u8>>,
) -> Result<HashMap<String, Vec<u8>>, String> {
    validate_assets(doc)?;
    let Some(assets) = assets_map(doc)? else {
        return Ok(HashMap::new());
    };
    let mut snapshot = HashMap::new();
    for digest in doc.keys(&assets) {
        let bytes = match stored_asset(doc, &assets, &digest)? {
            StoredAsset::Inline(bytes) => Some(bytes),
            StoredAsset::Reference { .. } => {
                read(&digest).filter(|bytes| tilo_content::verify(&digest, bytes))
            }
        };
        if let Some(bytes) = bytes {
            snapshot.insert(format!("asset:{digest}"), bytes);
        }
    }
    Ok(snapshot)
}

/// Whether every asset the notebook references is in `snapshot`.
///
/// # Errors
/// Returns an error when the asset map is malformed.
pub(crate) fn assets_complete(
    doc: &AutoCommit,
    snapshot: &HashMap<String, Vec<u8>>,
) -> Result<bool, String> {
    let Some(assets) = assets_map(doc)? else {
        return Ok(true);
    };
    Ok(doc
        .keys(&assets)
        .all(|digest| snapshot.contains_key(&format!("asset:{digest}"))))
}

/// A page's body: rich text.
fn validate_body(doc: &AutoCommit, page: &ObjId) -> Result<(), String> {
    let body = object(doc, page, "body", ObjType::Map)?;
    tilo_doc::model::open(doc, &body)
        .map(|_| ())
        .map_err(|error| format!("body is not rich text: {error}"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{InlineMark, InlineMarkSpan, NotesAction};

    fn notebook_with(blocks: &[ui::widget::BlockContent]) -> (AutoCommit, PageId) {
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
        NotesDoc::fill_page_body(&mut doc, page, blocks).unwrap();
        (doc, page)
    }

    fn linked_notebook(mark: InlineMark) -> (AutoCommit, PageId) {
        let mut read = ui::widget::BlockContent::paragraph("", "Read this");
        read.marks = InlineMarkSpan {
            start: 0,
            end: 9,
            mark,
        }
        .to_ui()
        .into_iter()
        .collect();
        notebook_with(&[read])
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn canonical_notebook_reopens_quotes_tables_links_and_embeds_losslessly() {
        eprintln!("phase quote_and_table_blocks_survive_canonical_reopen");
        {
            use ui::widget::{BlockContent, BlockKind};
            let kinds = [
                BlockKind::Quote,
                BlockKind::Table { visual: false },
                BlockKind::Table { visual: true },
            ];
            let blocks = kinds
                .iter()
                .map(|kind| BlockContent {
                    kind: kind.clone(),
                    ..BlockContent::paragraph("", "| a |")
                })
                .collect::<Vec<_>>();
            let (mut doc, page) = notebook_with(&blocks);
            let reopened = AutoCommit::load(&doc.save()).unwrap();
            project(&reopened, Some(page)).unwrap();
            validate(&reopened).unwrap();
            assert_eq!(
                NotesDoc::page_blocks(&reopened, page)
                    .into_iter()
                    .map(|block| block.kind)
                    .collect::<Vec<_>>(),
                kinds
            );
            for malformed in ["table:2", "table:true", "quote:1"] {
                assert!(BlockKind::from_stored(malformed).is_none());
            }
        }

        eprintln!("phase external_links_and_embeds_reopen_and_duplicate_without_losing_marks");
        {
            for mark in [
                InlineMark::ExternalLink {
                    url: "https://example.com/research?q=notes".into(),
                },
                InlineMark::Transclusion {
                    page: uuid::Uuid::new_v4().to_string(),
                },
            ] {
                let (mut doc, page) = linked_notebook(mark.clone());
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
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn notebook_images_are_content_addressed_references_that_survive_reopen_and_undo() {
        eprintln!("phase content_addressed_image_assets_survive_reopen_and_reject_bad_payloads");
        {
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
                tilo_document::RichTextAction::AppendParagraph {
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
                edit: tilo_document::RichTextAction::AppendParagraph {
                    text: "![Bad](asset:bad)".into(),
                },
            };
            let mut rejected = reopened;
            assert!(invalid.apply(&mut rejected).is_err());
            assert_eq!(NotesDoc::page_block_snapshots(&rejected, page), before);
        }

        eprintln!(
            "phase image_source_edits_persist_through_actions_and_undo_but_view_modes_do_not"
        );
        {
            use ui::Widget;
            let original = "![Scan](assets/original.png)";
            let edited = "![Updated scan](assets/revised.png)";
            let (doc, page) = notebook_with(&[ui::widget::BlockContent::paragraph("", original)]);
            let mut state = ui::State::new();
            state.doc = doc;
            state.doc.commit();
            let mut panel = crate::MainPanel::new();
            panel
                .bind_canonical_page(&state.doc, page, "Reference")
                .unwrap();
            panel.body_editor_mut().sync(&mut state.doc, &ROOT);
            let id = panel.editor().blocks()[0].id.clone();
            let initial_heads = state.doc.get_heads();
            assert!(panel.editor_mut().perform_semantic_action(
                &format!("notes.image-edit:{id}"),
                &ui::a11y::Action::Activate,
                &mut state.doc,
                &ROOT
            ));
            panel.body_editor_mut().sync(&mut state.doc, &ROOT);
            assert_eq!(
                state.doc.get_heads(),
                initial_heads,
                "a view mode writes nothing"
            );
            assert!(panel.editor_mut().perform_semantic_action(
                &format!("notes.image-source:{id}"),
                &ui::a11y::Action::SetValue(edited.into()),
                &mut state.doc,
                &ROOT
            ));
            panel.body_editor_mut().sync(&mut state.doc, &ROOT);
            let before = ui::undo::take_committed_action(panel.body_editor_mut())
                .expect("a source edit is a Notes action");
            state.record_committed_action(before, None, None);
            let edited_heads = state.doc.get_heads();
            assert!(panel.editor_mut().perform_semantic_action(
                &format!("notes.image-done:{id}"),
                &ui::a11y::Action::Activate,
                &mut state.doc,
                &ROOT
            ));
            panel.body_editor_mut().sync(&mut state.doc, &ROOT);
            assert_eq!(state.doc.get_heads(), edited_heads);
            let reopened = AutoCommit::load(&state.doc.save()).unwrap();
            validate(&reopened).unwrap();
            assert_eq!(NotesDoc::page_block_snapshots(&reopened, page)[0].1, edited);
            state.undo(None, None).unwrap();
            assert_eq!(
                NotesDoc::page_block_snapshots(&state.doc, page)[0].1,
                original
            );
        }

        eprintln!("phase new_images_are_references_only_when_a_content_store_holds_the_bytes");
        {
            let bytes = png(5);
            let digest = blake3::hash(&bytes).to_hex().to_string();

            let mut shared_doc = AutoCommit::new();
            initialize_empty(&mut shared_doc).unwrap();
            let store = device_store(false);
            put_asset_in(&store, &mut shared_doc, &digest, &bytes).unwrap();
            assert!(matches!(
                stored(&shared_doc, &digest),
                StoredAsset::Reference { .. }
            ));
            assert_eq!(store.read(&digest), Some(bytes.clone()));
            // Re-adding the same image is a no-op; other bytes under the digest are not.
            put_asset_in(&store, &mut shared_doc, &digest, &bytes).unwrap();

            let mut local_doc = AutoCommit::new();
            initialize_empty(&mut local_doc).unwrap();
            let local = tilo_content::ContentStore::memory();
            put_asset_in(&local, &mut local_doc, &digest, &bytes).unwrap();
            assert_eq!(stored(&local_doc, &digest), StoredAsset::Inline(bytes));
            validate(&local_doc).unwrap();
        }
    }

    use tilo_automerge::ApplyIncoming as _;

    /// A content backend standing in for the device store behind the
    /// launcher's capability: shared beyond this process, optionally refusing.
    #[derive(Default)]
    struct DeviceStore {
        local: tilo_content::MemoryBackend,
        refuse: bool,
    }

    impl tilo_content::ContentBackend for DeviceStore {
        fn put(
            &self,
            reader: &mut dyn std::io::Read,
            mime: &str,
        ) -> Result<tilo_content::Stored, tilo_content::ContentError> {
            if self.refuse {
                return Err(tilo_content::ContentError::Io("full".into()));
            }
            self.local.put(reader, mime)
        }
        fn open(
            &self,
            hash: &str,
        ) -> Result<Option<tilo_content::ContentReader>, tilo_content::ContentError> {
            self.local.open(hash)
        }
        fn has(&self, hash: &str) -> bool {
            self.local.has(hash)
        }
        fn shared(&self) -> bool {
            true
        }
    }

    fn device_store(refuse: bool) -> tilo_content::ContentStore {
        tilo_content::ContentStore::new(DeviceStore {
            refuse,
            ..DeviceStore::default()
        })
    }

    fn png(seed: u8) -> Vec<u8> {
        let mut bytes = vec![137, 80, 78, 71, 13, 10, 26, 10];
        bytes.extend(std::iter::repeat_n(seed, 2048));
        bytes
    }

    /// A notebook as it was written before images moved to the content
    /// store: bytes inline under their digest, referenced from a block.
    fn inline_notebook(images: &[Vec<u8>]) -> (AutoCommit, Vec<String>) {
        let mut doc = AutoCommit::new();
        initialize_empty(&mut doc).unwrap();
        let assets = assets_map(&doc).unwrap().unwrap();
        let digests = images
            .iter()
            .map(|bytes| {
                let digest = blake3::hash(bytes).to_hex().to_string();
                doc.put(&assets, &digest, ScalarValue::Bytes(bytes.clone()))
                    .unwrap();
                digest
            })
            .collect::<Vec<_>>();
        doc.commit();
        (doc, digests)
    }

    fn stored(doc: &AutoCommit, digest: &str) -> StoredAsset {
        stored_asset(doc, &assets_map(doc).unwrap().unwrap(), digest).unwrap()
    }

    #[test]
    fn inline_images_move_to_the_content_store_convergently_or_stay_inline() {
        eprintln!(
            "phase inline_images_move_to_the_content_store_and_the_notebook_keeps_references"
        );
        {
            let images = [png(1), png(2)];
            let (mut doc, digests) = inline_notebook(&images);
            validate(&doc).expect("an old notebook with inline images stays loadable");
            assert_eq!(inline_asset_digests(&doc).unwrap(), {
                let mut sorted = digests.clone();
                sorted.sort();
                sorted
            });

            let store = device_store(false);
            assert_eq!(
                move_inline_assets_in(&store, &mut doc, &digests).unwrap(),
                2
            );
            doc.commit();
            for (digest, bytes) in digests.iter().zip(&images) {
                assert_eq!(
                    stored(&doc, digest),
                    StoredAsset::Reference {
                        len: bytes.len() as u64,
                        mime: "image/png".into(),
                    }
                );
                assert_eq!(store.read(digest).as_ref(), Some(bytes));
            }
            assert!(inline_asset_digests(&doc).unwrap().is_empty());

            // Round trip: the saved notebook reopens with references and every
            // image readable through the store.
            let reopened = AutoCommit::load(&doc.save()).unwrap();
            validate(&reopened).unwrap();
            let snapshot = asset_snapshot_with(&reopened, |hash| store.read(hash)).unwrap();
            for (digest, bytes) in digests.iter().zip(&images) {
                assert_eq!(snapshot.get(&format!("asset:{digest}")), Some(bytes));
            }
            assert!(assets_complete(&reopened, &snapshot).unwrap());
            // Before a collaborator's bytes arrive, the image is simply absent.
            let waiting = asset_snapshot_with(&reopened, |_| None).unwrap();
            assert!(waiting.is_empty());
            assert!(!assets_complete(&reopened, &waiting).unwrap());

            // Idempotent: a second move changes nothing.
            let mut again = reopened;
            let heads = again.get_heads();
            assert_eq!(
                move_inline_assets_in(&store, &mut again, &digests).unwrap(),
                0
            );
            again.commit();
            assert_eq!(again.get_heads(), heads);
        }

        eprintln!("phase two_devices_moving_the_same_images_converge");
        {
            let images = [png(3)];
            let (mut doc, digests) = inline_notebook(&images);
            let mut a = doc.fork();
            let mut b = doc.fork();
            let store = device_store(false);
            move_inline_assets_in(&store, &mut a, &digests).unwrap();
            move_inline_assets_in(&store, &mut b, &digests).unwrap();
            a.merge_incoming(&mut b).unwrap();
            validate(&a).unwrap();
            assert!(matches!(
                stored(&a, &digests[0]),
                StoredAsset::Reference { .. }
            ));
        }

        eprintln!("phase a_refused_move_leaves_the_images_inline");
        {
            let images = [png(4)];
            let (mut doc, digests) = inline_notebook(&images);
            let refusing = device_store(true);
            assert!(move_inline_assets_in(&refusing, &mut doc, &digests).is_err());
            assert_eq!(
                stored(&doc, &digests[0]),
                StoredAsset::Inline(images[0].clone())
            );
            // A process-local store never receives the only copy.
            let local = tilo_content::ContentStore::memory();
            assert!(move_inline_assets_in(&local, &mut doc, &digests).is_err());
            assert_eq!(
                stored(&doc, &digests[0]),
                StoredAsset::Inline(images[0].clone())
            );
        }
    }
}
