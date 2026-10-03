//! Exact Photos authored roots and frozen, content-addressed History presentation.
use crate::{
    doc::PhotosDoc,
    model::{Edits, OpenImage, PhotoLibrary, Rect01},
};
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use std::{collections::BTreeMap, rc::Rc};
use tilo_content::ContentStore;
use ui::history_mode::restore::{HistoryRestoreRoot, HistoryRestoreSchema, RestoreValueType};
use ui::history_mode::{
    HistoricalAssetRequest, HistoricalAssetResponse, HistoricalAssetSource, HistoryDefinition,
    HistoryPresentation, HistoryPreview, HistoryScopeDefinition, PreviewInput,
};

pub static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "so.tilo.photos",
    scopes: &[HistoryScopeDefinition {
        key: "photos",
        title: "Photograph and photo library",
        presentation: HistoryPresentation::DomainWithAssets {
            create: preview,
            assets: || {
                Rc::new(PhotoAssets {
                    content: ContentStore::for_app(),
                })
            },
        },
        content_schema: Some(&SCHEMA),
    }],
};
pub static SCHEMA: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "photos.content",
    version: 1,
    roots: &[
        HistoryRestoreRoot {
            key: "image",
            required: false,
            expected_type: RestoreValueType::Scalar(ui::history_mode::restore::ScalarType::String),
        },
        HistoryRestoreRoot {
            key: "photos_edits",
            required: false,
            expected_type: RestoreValueType::Object(ObjType::Map),
        },
        HistoryRestoreRoot {
            key: "photos_library",
            required: false,
            expected_type: RestoreValueType::Object(ObjType::Map),
        },
    ],
    selectors: &[],
    preserved_roots: &[],
    preserved: &[],
    validate,
    validate_result: validate,
};
const MAX_RECORDS: usize = 10_000;
fn values<'a>(
    doc: &'a AutoCommit,
    obj: &ObjId,
    key: impl Into<automerge::Prop>,
) -> Result<Vec<(Value<'a>, ObjId)>, String> {
    doc.get_all(obj, key).map_err(|e| e.to_string())
}
fn json<T: serde::de::DeserializeOwned>(v: &Value<'_>) -> Result<T, String> {
    let text = v.as_str().ok_or("Recorded Photos JSON must be a string")?;
    if text.len() > 16 * 1024 * 1024 {
        return Err("Recorded Photos JSON exceeds safe size".into());
    }
    serde_json::from_str(text).map_err(|e| format!("Malformed recorded Photos JSON: {e}"))
}
fn text(v: &Value<'_>) -> Result<(), String> {
    v.as_str()
        .filter(|s| s.len() <= 65_536)
        .map(|_| ())
        .ok_or_else(|| "Invalid recorded text".into())
}
fn map(v: &Value<'_>) -> Result<(), String> {
    if matches!(v, Value::Object(ObjType::Map)) {
        Ok(())
    } else {
        Err("Recorded Photos map required".into())
    }
}
fn image(open: &OpenImage) -> Result<(), String> {
    if open.name.len() > 65_536
        || open.path.len() > 65_536
        || u64::from(open.width).saturating_mul(u64::from(open.height))
            > crate::pixels::MAX_DECODED_PIXELS
    {
        return Err("Recorded photograph exceeds domain limits".into());
    }
    if open.content.as_ref().is_some_and(|h| {
        h.len() != 64
            || !h
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    }) {
        return Err("Recorded content ID must be a BLAKE3 hex digest".into());
    }
    Ok(())
}
fn edits(e: Edits) -> Result<(), String> {
    if e.crop.is_valid_selection() && e.rotate_quarters <= 3 {
        Ok(())
    } else {
        Err("Invalid recorded crop or quarter turns".into())
    }
}
fn library(lib: &PhotoLibrary) -> Result<(), String> {
    if lib.photos.len().saturating_add(lib.albums.len()) > MAX_RECORDS {
        return Err("Recorded library exceeds safe size".into());
    }
    let mut ids = std::collections::HashSet::new();
    let mut metadata_bytes = 0usize;
    let mut references = 0usize;
    for p in &lib.photos {
        if p.id.is_empty() || !ids.insert(&p.id) {
            return Err("Invalid or duplicate photo ID".into());
        }
        image(&p.image)?;
        for label in [&p.id, &p.image.name, &p.image.path]
            .into_iter()
            .chain(p.folder.iter())
            .chain(p.captured_at.iter())
            .chain(p.people.iter())
            .chain(p.tags.iter())
        {
            metadata_bytes = metadata_bytes.saturating_add(label.len());
        }
        references = references
            .saturating_add(p.people.len())
            .saturating_add(p.tags.len());
    }
    ids.clear();
    for a in &lib.albums {
        if a.id.is_empty() || !ids.insert(&a.id) || a.photo_ids.len() > MAX_RECORDS {
            return Err("Invalid album identity or membership".into());
        }
        references = references.saturating_add(a.photo_ids.len());
        for label in [&a.id, &a.name].into_iter().chain(a.photo_ids.iter()) {
            metadata_bytes = metadata_bytes.saturating_add(label.len());
        }
    }
    if references > 100_000 || metadata_bytes > 16 * 1024 * 1024 {
        return Err("Recorded library metadata exceeds preview limits".into());
    }
    Ok(())
}
fn list(doc: &AutoCommit, obj: &ObjId, key: &str) -> Result<(), String> {
    for (v, id) in values(doc, obj, key)? {
        string_list(doc, &v, &id)?;
    }
    Ok(())
}
fn string_list(doc: &AutoCommit, value: &Value<'_>, id: &ObjId) -> Result<(), String> {
    if !matches!(value, Value::Object(ObjType::List)) || doc.length(id) > MAX_RECORDS {
        return Err("Invalid recorded string list".into());
    }
    for i in 0..doc.length(id) {
        for (v, _) in values(doc, id, i)? {
            text(&v)?;
        }
    }
    Ok(())
}
/// Absolute album memberships are JSON arrays. Validate every conflict, not
/// only the reader's chosen set.
fn album_membership(doc: &AutoCommit, row: &ObjId) -> Result<(), String> {
    for (value, _) in values(doc, row, "photo_ids")? {
        let members = json::<Vec<String>>(&value)?;
        if members.len() > MAX_RECORDS || members.iter().any(|id| id.len() > 65_536) {
            return Err("Invalid recorded album membership".into());
        }
    }
    Ok(())
}
/// Labels are per-label boolean membership maps. Inspect every membership
/// conflict, including false removal tombstones.
fn labels(doc: &AutoCommit, value: &Value<'_>, row: &ObjId) -> Result<(), String> {
    if matches!(value, Value::Object(ObjType::Map)) {
        if doc.length(row) > MAX_RECORDS {
            return Err("Too many metadata labels".into());
        }
        let mut bytes = 0usize;
        for label in doc.keys(row) {
            if label.is_empty() || label.len() > 65_536 {
                return Err("Invalid recorded metadata label".into());
            }
            bytes = bytes.saturating_add(label.len());
            if bytes > 16 * 1024 * 1024 {
                return Err("Recorded metadata labels exceed safe size".into());
            }
            for (membership, _) in values(doc, row, label)? {
                if !matches!(membership, Value::Scalar(s) if matches!(s.as_ref(), automerge::ScalarValue::Boolean(_)))
                {
                    return Err("Invalid recorded metadata membership".into());
                }
            }
        }
    } else {
        return Err("Recorded metadata labels must be a membership map".into());
    }
    Ok(())
}
/// Validate all consumed conflict branches; the shared restore engine separately
/// refuses selected conflicts it cannot restore exactly. No tolerant fallback here.
#[allow(
    clippy::too_many_lines,
    reason = "validate the complete authored schema together"
)]
pub(crate) fn validate(doc: &AutoCommit) -> Result<(), String> {
    for (v, _) in values(doc, &ROOT, "image")? {
        if let Some(open) = json::<Option<OpenImage>>(&v)? {
            image(&open)?;
        }
    }
    for (v, root) in values(doc, &ROOT, "photos_edits")? {
        map(&v)?;
        for key in ["crop", "rotate_quarters"] {
            let vs = values(doc, &root, key)?;
            if vs.is_empty() {
                return Err(format!("Missing recorded {key}"));
            }
            for (v, _) in vs {
                if key == "crop" {
                    edits(Edits {
                        crop: json::<Rect01>(&v)?,
                        rotate_quarters: 0,
                    })?;
                } else {
                    edits(Edits {
                        rotate_quarters: json::<u8>(&v)?,
                        ..Edits::default()
                    })?;
                }
            }
        }
    }
    for (v, root) in values(doc, &ROOT, "photos_library")? {
        map(&v)?;
        for required in ["photo_core", "albums"] {
            if values(doc, &root, required)?.is_empty() {
                return Err(format!("Missing recorded {required}"));
            }
        }
        for key in [
            "albums",
            "photo_core",
            "photo_favorite",
            "photo_folder",
            "photo_people",
            "photo_tags",
            "photo_captured_at",
            "removed_photos",
            "removed_albums",
        ] {
            for (v, rows) in values(doc, &root, key)? {
                map(&v)?;
                if doc.length(&rows) > MAX_RECORDS {
                    return Err("Too many recorded Photos records".into());
                }
                for id in doc.keys(&rows) {
                    if id.is_empty() || id.len() > 65_536 {
                        return Err("Invalid recorded stable ID".into());
                    }
                    for (v, row) in values(doc, &rows, id)? {
                        match key {
                            "photo_core" => image(&json(&v)?)?,
                            "photo_favorite" => {
                                json::<bool>(&v)?;
                            }
                            "photo_folder" | "photo_captured_at" => {
                                json::<Option<String>>(&v)?;
                            }
                            "photo_people" | "photo_tags" => labels(doc, &v, &row)?,
                            "removed_photos" | "removed_albums" => {
                                if !matches!(v, Value::Scalar(s) if matches!(s.as_ref(), automerge::ScalarValue::Boolean(_)))
                                {
                                    return Err("Invalid tombstone".into());
                                }
                            }
                            _ => {
                                map(&v)?;
                                if values(doc, &row, "name")?.is_empty() {
                                    return Err("Missing recorded name".into());
                                }
                                for (v, _) in values(doc, &row, "name")? {
                                    text(&v)?;
                                }
                                album_membership(doc, &row)?;
                            }
                        }
                    }
                }
            }
        }
        list(doc, &root, "photo_order")?;
        list(doc, &root, "album_order")?;
    }
    library(&PhotosDoc::library(doc))
}
/// Content addresses are BLAKE3. Keep authored bare IDs untouched.
pub(crate) fn asset_id(open: &OpenImage) -> Option<String> {
    open.content.as_ref().map(|h| format!("asset:blake3:{h}"))
}
/// Historical originals come from the app's own content store: the same
/// directory the running app imports into, resolved the same way.
struct PhotoAssets {
    content: ContentStore,
}
impl HistoricalAssetSource for PhotoAssets {
    fn request(&self, request: HistoricalAssetRequest, response: HistoricalAssetResponse) {
        let result = (|| {
            let doc = AutoCommit::load(request.snapshot.bytes()).map_err(|e| e.to_string())?;
            validate(&doc)?;
            let lib = PhotosDoc::library(&doc);
            let mut result = BTreeMap::new();
            let mut total = 0usize;
            for open in PhotosDoc::image(&doc)
                .iter()
                .chain(lib.photos.iter().map(|p| &p.image))
            {
                let (Some(id), Some(hash)) = (asset_id(open), &open.content) else {
                    continue;
                };
                if result.contains_key(&id) {
                    continue;
                }
                // Selected-source custody; local immutable content lookup has no
                // transport request and never resolves an ambient historical path.
                if let Some(bytes) = self.content.read(hash) {
                    total = total.saturating_add(bytes.len());
                    if total > 128 * 1024 * 1024 {
                        return Err("Recorded Photos assets exceed byte budget".into());
                    }
                    if bytes.len() <= crate::pixels::MAX_ENCODED_BYTES {
                        result.insert(id, bytes.into());
                    }
                }
            }
            Ok(result)
        })();
        response.complete(result);
    }
}
fn preview(input: PreviewInput) -> Result<Box<dyn HistoryPreview>, String> {
    let doc = AutoCommit::load(input.snapshot.bytes()).map_err(|e| e.to_string())?;
    validate(&doc)?;
    Ok(Box::new(crate::presentation::PhotoPresentation::new(
        PhotosDoc::image(&doc),
        PhotosDoc::edits(&doc),
        PhotosDoc::library(&doc),
        input.assets,
        input.initial_view,
    )))
}
