//! Explicit, retry-safe import of a quiescent legacy workspace.
//!
//! The caller must stop legacy writers first. Comparing two complete reads
//! detects observed changes, but cannot make independent file writers transactional.
//! Source files are retained. Only the canonical owner publishes content and receipt.
#![allow(
    clippy::result_large_err,
    reason = "the adopted import API retains the exact checkout, source key and strong owner in its durability error"
)]
use crate::{
    doc::NotesDoc,
    markdown,
    model::{PageId, Workspace},
    notebook, storage,
};
use automerge::{
    AutoCommit, LoadOptions, ObjType, OnPartialLoad, ROOT, ReadDoc, Value,
    transaction::Transactable,
};
use std::{collections::BTreeMap, path::Path};
use ui::{
    record::{
        RecordCheckout, RecordId, RecordKind, RecordOpenError, RecordSaveStatus, RecordStore,
    },
    widget::{BlockContent, marks::Mark},
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LegacySourceKey([u8; 32]);
impl LegacySourceKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}
pub struct LegacySnapshot {
    workspace: Workspace,
    pages: Vec<ImportedPage>,
    digest: [u8; 32],
    warnings: Vec<String>,
}
pub struct ImportedPage {
    id: PageId,
    blocks: Vec<BlockContent>,
}
#[derive(Debug)]
pub struct ImportOutcome {
    pub checkout: RecordCheckout,
    pub already_imported: bool,
    pub source_key: String,
    pub initial_selected: Option<PageId>,
    pub warnings: Vec<String>,
}
pub enum ImportError {
    Source(String),
    Conflict(RecordCheckout),
    Record(RecordOpenError),
    Durability {
        message: String,
        source_key: LegacySourceKey,
        checkout: RecordCheckout,
        records: RecordStore,
    },
}
impl std::fmt::Debug for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source(s) => f.debug_tuple("Source").field(s).finish(),
            Self::Conflict(c) => f.debug_tuple("Conflict").field(c).finish(),
            Self::Record(e) => f.debug_tuple("Record").field(e).finish(),
            Self::Durability {
                message,
                source_key,
                checkout,
                ..
            } => f
                .debug_struct("Durability")
                .field("message", message)
                .field("source_key", source_key)
                .field("checkout", checkout)
                .finish_non_exhaustive(),
        }
    }
}
impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source(message) => write!(f, "legacy source: {message}"),
            Self::Conflict(checkout) => write!(
                f,
                "import destination {} has a different or missing receipt",
                checkout.id.as_str()
            ),
            Self::Record(error) => std::fmt::Display::fmt(error, f),
            Self::Durability {
                message, checkout, ..
            } => write!(
                f,
                "import destination {} is not durable: {message}",
                checkout.id.as_str()
            ),
        }
    }
}
impl std::error::Error for ImportError {}

fn hex(bytes: &[u8; 32]) -> String {
    blake3::Hash::from(*bytes).to_hex().to_string()
}

/// Derive an opaque identity from the canonicalized explicit source directory.
/// No ambient profile discovery or writes occur.
/// # Errors
/// Returns an error if the directory cannot be resolved.
pub fn source_key(root: &Path) -> Result<LegacySourceKey, ImportError> {
    let canonical = root.canonicalize().map_err(|e| source_error(root, e))?;
    if !canonical.is_dir() {
        return Err(source_error(root, "source is not a directory"));
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo-notes-legacy-source-v1\0");
    hash.update(canonical.as_os_str().as_encoded_bytes());
    Ok(LegacySourceKey::from_bytes(*hash.finalize().as_bytes()))
}

/// The fixed destination for a source identity, independent of source content.
/// The full 256-bit key is encoded in two bounded, alphabetic derived roles
/// over a fixed typed anchor. A bare hex digest is not a valid record ID.
pub fn destination(key: LegacySourceKey) -> RecordCheckout {
    let digest = hex(&key.0);
    let (first, second) = digest.split_at(32);
    let tail = RecordId::derived(&format!("key-{second}"), &RecordId::for_tab(0));
    let source = RecordId::derived(&format!("key-{first}"), &tail);
    RecordCheckout::new(
        RecordId::derived("notes-legacy-v1", &source),
        RecordKind::new("notes.notebook"),
    )
}

/// Strictly read, re-read and compile a complete explicit legacy source.
/// Legacy writers must already be quiescent; two reads cannot prevent future races.
/// # Errors
/// Returns a local path/page diagnostic for missing, malformed or changed inputs.
pub fn read_snapshot(root: &Path, key: LegacySourceKey) -> Result<LegacySnapshot, ImportError> {
    snapshot_with_hook(root, key, &mut |_| Ok(()))
}

/// Import one complete notebook and acknowledge its destination writer.
/// Existing matching receipts are retried without reading any source file.
/// # Errors
/// Source failures leave the source unchanged. Durability errors retain the
/// exact checkout, source identity and strong shared owner for edit-free retry.
pub fn import_legacy(
    records: &RecordStore,
    root: &Path,
    key: LegacySourceKey,
) -> Result<ImportOutcome, ImportError> {
    import_with_hook(records, root, key, |_| Ok(()))
}

/// Import into an explicit host-selected notebook (for example its stable
/// default notebook). A ready destination still requires the matching receipt;
/// this never overwrites an existing user's notebook. Legacy sources are retained.
/// # Errors
/// Rejects foreign record kinds, conflicts, invalid sources and durability failures.
pub fn import_legacy_into(
    records: &RecordStore,
    root: &Path,
    key: LegacySourceKey,
    checkout: RecordCheckout,
) -> Result<ImportOutcome, ImportError> {
    import_into_with_hook(records, root, key, checkout, |_| Ok(()))
}

fn source_error(path: &Path, error: impl std::fmt::Display) -> ImportError {
    ImportError::Source(format!("{}: {error}", path.display()))
}
fn read(path: &Path) -> Result<Vec<u8>, ImportError> {
    crate::files::read(path).map_err(|e| source_error(path, e))
}
fn source_workspace(path: &Path, bytes: &[u8]) -> Result<Workspace, ImportError> {
    if bytes.is_empty() {
        return Err(source_error(path, "empty Automerge workspace"));
    }
    let doc = AutoCommit::load_with_options(
        bytes,
        LoadOptions::new().on_partial_load(OnPartialLoad::Error),
    )
    .map_err(|e| source_error(path, e))?;
    if let Some(field) = doc.keys(ROOT).find(|field| field != "workspace_json") {
        return Err(source_error(
            path,
            format!("unsupported legacy root field {field}"),
        ));
    }
    let (Value::Object(ObjType::Text), text) =
        notebook::required(&doc, &ROOT, "workspace_json").map_err(|e| source_error(path, e))?
    else {
        return Err(source_error(path, "workspace_json must be Text"));
    };
    let json = doc.text(text).map_err(|e| source_error(path, e))?;
    let workspace = Workspace {
        data: serde_json::from_str(&json).map_err(|e| source_error(path, e))?,
    };
    validate_legacy_json_fields(&json).map_err(|e| source_error(path, e))?;
    workspace
        .check_invariants()
        .map_err(|e| source_error(path, e))?;
    Ok(workspace)
}

// Keep the permissive legacy codec unchanged. Migration must explicitly refuse
// future fields rather than report success after silently dropping their content.
fn validate_legacy_json_fields(json: &str) -> Result<(), String> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    reject_unknown_fields(
        &value,
        &["pages", "selected", "tags", "tag_assignments"],
        "workspace",
    )?;
    let collections: [(&str, &[&str]); 3] = [
        ("pages", &["id", "title", "parent", "order"]),
        ("tags", &["id", "name"]),
        ("tag_assignments", &["page", "tag"]),
    ];
    for (collection, fields) in collections {
        if let Some(entries) = value.get(collection) {
            let entries = entries
                .as_array()
                .ok_or_else(|| format!("{collection} must be an array"))?;
            for (index, entry) in entries.iter().enumerate() {
                reject_unknown_fields(entry, fields, &format!("{collection}[{index}]"))?;
            }
        }
    }
    Ok(())
}
fn reject_unknown_fields(
    value: &serde_json::Value,
    fields: &[&str],
    location: &str,
) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{location} must be an object"))?;
    for field in object.keys() {
        if !fields.contains(&field.as_str()) {
            return Err(format!("unsupported legacy field {location}.{field}"));
        }
    }
    Ok(())
}

// Private per-call boundary callbacks let unit tests interrupt actual reads and
// initialization without globals, alternate stores, clocks, or public fake APIs.
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "page details are observed by private unit-test callbacks; production uses the no-op callback"
    )
)]
enum ImportStep<'a> {
    SnapshotRead,
    Reserved,
    PageInitialized(usize, &'a AutoCommit),
}
fn snapshot_with_hook(
    root: &Path,
    key: LegacySourceKey,
    hook: &mut impl FnMut(ImportStep<'_>) -> Result<(), String>,
) -> Result<LegacySnapshot, ImportError> {
    let metadata = storage::workspace_file_path(root);
    let metadata_bytes = read(&metadata)?;
    let workspace = source_workspace(&metadata, &metadata_bytes)?;
    let mut files = BTreeMap::new();
    files.insert(metadata, metadata_bytes);
    for page in &workspace.data.pages {
        let path = storage::page_file_path(root, page.id);
        files.insert(path.clone(), read(&path)?);
    }
    hook(ImportStep::SnapshotRead).map_err(ImportError::Source)?;
    for (path, first) in &files {
        if &read(path)? != first {
            return Err(source_error(
                path,
                "legacy snapshot changed during read; stop legacy writers and retry",
            ));
        }
    }
    let mut digest = blake3::Hasher::new();
    digest.update(b"tilo-notes-legacy-snapshot-v1\0");
    for (path, bytes) in &files {
        let relative = path.strip_prefix(root).map_err(|e| source_error(path, e))?;
        let name = relative.as_os_str().as_encoded_bytes();
        digest.update(&(name.len() as u64).to_le_bytes());
        digest.update(name);
        digest.update(&(bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    let mut titles: BTreeMap<&str, Vec<PageId>> = BTreeMap::new();
    for page in &workspace.data.pages {
        titles.entry(&page.title).or_default().push(page.id);
    }
    let mut pages = Vec::new();
    let mut warnings = Vec::new();
    for page in &workspace.data.pages {
        let path = storage::page_file_path(root, page.id);
        let bytes = files
            .get(&path)
            .ok_or_else(|| source_error(&path, "page missing from complete snapshot"))?;
        let body = std::str::from_utf8(bytes).map_err(|e| source_error(&path, e))?;
        pages.push(
            compile_page(page.id, body, key, &titles, &mut warnings)
                .map_err(|e| source_error(&path, e))?,
        );
    }
    warnings.sort();
    warnings.dedup();
    Ok(LegacySnapshot {
        workspace,
        pages,
        digest: *digest.finalize().as_bytes(),
        warnings,
    })
}

fn block_id(key: LegacySourceKey, page: PageId, index: usize) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo-notes-legacy-block-v1\0");
    hash.update(&key.0);
    hash.update(page.as_bytes());
    hash.update(&(index as u64).to_le_bytes());
    let digest = hash.finalize();
    let mut bytes = [0; 16];
    for (target, source) in bytes.iter_mut().zip(digest.as_bytes()) {
        *target = *source;
    }
    // UUID version 8 reserves a custom deterministic layout; retain RFC variant.
    for (index, byte) in bytes.iter_mut().enumerate() {
        if index == 6 {
            *byte = (*byte & 0x0f) | 0x80;
        } else if index == 8 {
            *byte = (*byte & 0x3f) | 0x80;
        }
    }
    uuid::Uuid::from_bytes(bytes).to_string()
}
fn compile_page(
    id: PageId,
    body: &str,
    key: LegacySourceKey,
    titles: &BTreeMap<&str, Vec<PageId>>,
    warnings: &mut Vec<String>,
) -> Result<ImportedPage, String> {
    let rows = markdown::expanded_rows(&markdown::parse(body));
    let mut blocks = Vec::new();
    let mut groups = BTreeMap::new();
    for (index, (kind, inline, fence, nesting)) in rows.into_iter().enumerate() {
        let mut spans = Vec::new();
        for mut span in inline.marks {
            let transclusion = matches!(span.mark, Mark::Transclusion(_));
            if let Mark::Link(target) | Mark::Transclusion(target) = &mut span.mark {
                let label = inline
                    .text
                    .get(span.range.clone())
                    .ok_or_else(|| "invalid Markdown link range".to_string())?;
                let prefix = if transclusion { "!" } else { "" };
                match titles.get(label).map(Vec::as_slice) {
                    Some([page]) => *target = *page,
                    Some(_) => return Err(format!("ambiguous page link {prefix}[[{label}]]")),
                    None => {
                        warnings.push(format!(
                            "unresolved page link {prefix}[[{label}]] on page {id}; visible label retained"
                        ));
                        continue;
                    }
                }
            }
            spans.push(span);
        }
        let code_group = fence.map(|fence| {
            groups
                .entry(fence)
                .or_insert_with(|| block_id(key, id, index))
                .clone()
        });
        blocks.push(BlockContent {
            kind,
            marks: spans,
            nesting,
            code_group,
            ..BlockContent::paragraph("", inline.text)
        });
    }
    Ok(ImportedPage { id, blocks })
}

fn matches_receipt(
    doc: &AutoCommit,
    checkout: &RecordCheckout,
    key: LegacySourceKey,
) -> Result<bool, String> {
    notebook::validate(doc)?;
    let Some((Value::Object(ObjType::Map), receipt)) = doc
        .get(ROOT, "notes_import_v1")
        .map_err(|e| e.to_string())?
    else {
        return Ok(false);
    };
    Ok(
        notebook::string(doc, &receipt, "source_key")? == hex(&key.0)
            && notebook::string(doc, &receipt, "destination_id")? == checkout.id.as_str(),
    )
}
fn acknowledge(
    records: &RecordStore,
    checkout: &RecordCheckout,
    key: LegacySourceKey,
) -> Result<(), ImportError> {
    // Flush is owner-wide. An unrelated writer failure cannot override Saved
    // for this destination; any unacknowledged destination retains its owner.
    let _aggregate = records.try_flush();
    let message = match records.save_status(&checkout.id) {
        Ok(RecordSaveStatus::Saved) => return Ok(()),
        Ok(RecordSaveStatus::Failed(message)) => message,
        Ok(RecordSaveStatus::Saving) => {
            "destination is still saving; durability is not acknowledged".into()
        }
        Ok(RecordSaveStatus::MemoryOnly) => {
            "destination is memory-only; durability is not acknowledged".into()
        }
        Err(error) => error.to_string(),
    };
    Err(ImportError::Durability {
        message,
        source_key: key,
        checkout: checkout.clone(),
        records: records.clone(),
    })
}
fn initialize(
    doc: &mut AutoCommit,
    snapshot: &LegacySnapshot,
    checkout: &RecordCheckout,
    key: LegacySourceKey,
    hook: &mut impl FnMut(ImportStep<'_>) -> Result<(), String>,
) -> Result<(), String> {
    notebook::initialize_empty(doc)?;
    for (index, page) in snapshot.workspace.data.pages.iter().enumerate() {
        NotesDoc::upsert_page_meta(doc, page);
        hook(ImportStep::PageInitialized(index, doc))?;
    }
    for tag in &snapshot.workspace.data.tags {
        NotesDoc::upsert_tag(doc, tag);
    }
    for edge in &snapshot.workspace.data.tag_assignments {
        NotesDoc::set_tag_assignment(doc, edge.page, edge.tag, true);
    }
    for page in &snapshot.pages {
        NotesDoc::fill_page_body(doc, page.id, &page.blocks)?;
    }
    let receipt = doc
        .put_object(ROOT, "notes_import_v1", ObjType::Map)
        .map_err(|e| e.to_string())?;
    for (field, value) in [
        ("source_key", hex(&key.0)),
        ("snapshot_digest", hex(&snapshot.digest)),
        ("destination_id", checkout.id.as_str().to_owned()),
        ("format", "workspace-json-markdown-v1".into()),
    ] {
        doc.put(&receipt, field, value).map_err(|e| e.to_string())?;
    }
    doc.put(&receipt, "version", 1_u64)
        .map_err(|e| e.to_string())?;
    notebook::validate(doc)
}
fn import_with_hook(
    records: &RecordStore,
    root: &Path,
    key: LegacySourceKey,
    hook: impl FnMut(ImportStep<'_>) -> Result<(), String>,
) -> Result<ImportOutcome, ImportError> {
    import_into_with_hook(records, root, key, destination(key), hook)
}

fn import_into_with_hook(
    records: &RecordStore,
    root: &Path,
    key: LegacySourceKey,
    checkout: RecordCheckout,
    mut hook: impl FnMut(ImportStep<'_>) -> Result<(), String>,
) -> Result<ImportOutcome, ImportError> {
    if checkout.kind != crate::definition().record_kind
        || RecordId::parse(checkout.id.as_str()).as_ref() != Some(&checkout.id)
    {
        return Err(ImportError::Record(RecordOpenError::IdentityMismatch));
    }
    match records.try_open_record(&checkout) {
        Ok(_) => {
            let matches = records
                .read(&checkout.id, |doc| matches_receipt(doc, &checkout, key))
                .ok_or(ImportError::Record(RecordOpenError::MissingRecord))?
                .map_err(|error| ImportError::Record(RecordOpenError::Initialization(error)))?;
            if !matches {
                return Err(ImportError::Conflict(checkout));
            }
            acknowledge(records, &checkout, key)?;
            return Ok(ImportOutcome {
                checkout,
                already_imported: true,
                source_key: hex(&key.0),
                initial_selected: None,
                warnings: Vec::new(),
            });
        }
        Err(RecordOpenError::MissingRecord) => {}
        Err(error) => return Err(ImportError::Record(error)),
    }
    let snapshot = snapshot_with_hook(root, key, &mut hook)?;
    let mut reservation = records
        .reserve_record(&checkout)
        .map_err(ImportError::Record)?;
    hook(ImportStep::Reserved)
        .map_err(|error| ImportError::Record(RecordOpenError::Initialization(error)))?;
    reservation
        .initialize(|doc| initialize(doc, &snapshot, &checkout, key, &mut hook))
        .map_err(ImportError::Record)?;
    acknowledge(records, &checkout, key)?;
    Ok(ImportOutcome {
        checkout,
        already_imported: false,
        source_key: hex(&key.0),
        initial_selected: snapshot.workspace.selected(),
        warnings: snapshot.warnings,
    })
}
#[cfg(test)]
#[path = "../tests/support/canonical.rs"]
#[allow(
    dead_code,
    reason = "shared unit/integration fixture helpers have different exercised subsets"
)]
mod fixtures;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{doc::NotesDoc, notebook};
    use automerge::AutoCommit;
    use fixtures::LegacyFixture;
    use ui::record::{RecordId, RecordKind, RecordSaveStatus};
    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn legacy_import_publishes_atomically_or_leaves_no_candidate_state() {
        eprintln!("phase changed_snapshot_is_rejected_at_second_read_without_publication");
        {
            for metadata in [false, true] {
                let f = LegacyFixture::rich();
                let checkout = destination(f.key);
                let mut expected = f.source_bytes();
                let relative = if metadata {
                    std::path::PathBuf::from("workspace.automerge")
                } else {
                    std::path::PathBuf::from(format!("pages/{}.md", f.child))
                };
                expected.insert(relative.clone(), b"changed while reading".to_vec());
                let result = import_with_hook(&f.records, &f.source, f.key, |step| {
                    if matches!(step, ImportStep::SnapshotRead) {
                        std::fs::write(f.source.join(&relative), b"changed while reading").unwrap();
                    }
                    Ok(())
                });
                assert!(
                    matches!(result,Err(ImportError::Source(ref s)) if s.contains("changed")),
                    "{result:?}"
                );
                assert!(!f.records.is_ready(&checkout.id));
                assert!(!f.document_path(&checkout).exists());
                assert_eq!(f.source_bytes(), expected);
            }
        }

        eprintln!("phase stop_before_reservation_leaves_no_canonical_document");
        {
            let f = LegacyFixture::rich();
            let before = f.source_bytes();
            let checkout = destination(f.key);
            let result = import_with_hook(&f.records, &f.source, f.key, |step| {
                if matches!(step, ImportStep::SnapshotRead) {
                    return Err("stop before reservation".into());
                }
                Ok(())
            });
            assert!(matches!(result,Err(ImportError::Source(ref s)) if s.contains("stop before")));
            assert!(!f.records.is_ready(&checkout.id));
            assert!(!f.document_path(&checkout).exists());
            assert_eq!(f.source_bytes(), before);
            assert!(
                !import_legacy(&f.records, &f.source, f.key)
                    .unwrap()
                    .already_imported
            );
        }

        eprintln!("phase initializer_failure_after_first_page_discards_all_candidate_state");
        {
            let f = LegacyFixture::rich();
            let before = f.source_bytes();
            let checkout = destination(f.key);
            let mut seen = false;
            let result = import_with_hook(&f.records, &f.source, f.key, |step| {
                if let ImportStep::PageInitialized(0, doc) = step {
                    assert_eq!(NotesDoc::list_pages(doc).len(), 1);
                    seen = true;
                    return Err("first page failure".into());
                }
                Ok(())
            });
            assert!(seen);
            assert!(
                matches!(result,Err(ImportError::Record(RecordOpenError::Initialization(ref s))) if s.contains("first page"))
            );
            assert!(!f.records.is_ready(&checkout.id));
            assert!(!f.document_path(&checkout).exists());
            assert_eq!(f.source_bytes(), before);
            let result = import_legacy(&f.records, &f.source, f.key).unwrap();
            f.records
                .read(&result.checkout.id, |d| {
                    notebook::validate(d).unwrap();
                    assert_eq!(NotesDoc::list_pages(d).len(), 3);
                })
                .unwrap();
        }

        eprintln!(
            "phase first_publication_failure_retains_destination_and_retries_without_new_edit"
        );
        {
            let f = LegacyFixture::rich();
            let before = f.source_bytes();
            let checkout = destination(f.key);
            let other = RecordCheckout::new(
                RecordId::derived("notes-fixture-unrelated", &RecordId::for_tab(0)),
                RecordKind::new("notes.notebook"),
            );
            f.records
                .reserve_record(&other)
                .unwrap()
                .initialize(notebook::initialize_empty)
                .unwrap();
            f.records.try_flush().unwrap();
            let path = f.document_path(&checkout);
            let error = import_with_hook(&f.records, &f.source, f.key, |step| {
                if matches!(step, ImportStep::Reserved) {
                    std::fs::create_dir(&path).unwrap();
                }
                Ok(())
            })
            .unwrap_err();
            let ImportError::Durability {
                message,
                source_key,
                checkout: retained,
                records,
            } = error
            else {
                panic!("expected retained durability error")
            };
            assert!(!message.is_empty());
            assert_eq!(source_key, f.key);
            assert_eq!(retained, checkout);
            assert!(records.is_ready(&checkout.id));
            assert_eq!(
                records.save_status(&other.id).unwrap(),
                RecordSaveStatus::Saved
            );
            assert!(
                matches!(records.save_status(&checkout.id).unwrap(),RecordSaveStatus::Failed(ref s) if s==&message)
            );
            let heads = records.with(&checkout.id, AutoCommit::get_heads).unwrap();
            assert_eq!(f.source_bytes(), before);
            std::fs::remove_dir(&path).unwrap();
            std::fs::rename(&f.source, f.temp.path().join("retained-source")).unwrap();
            let outcome = import_legacy(&records, &f.source, f.key).unwrap();
            assert!(outcome.already_imported);
            assert_eq!(outcome.initial_selected, None);
            assert_eq!(
                records.save_status(&checkout.id).unwrap(),
                RecordSaveStatus::Saved
            );
            assert_eq!(
                records.with(&checkout.id, AutoCommit::get_heads).unwrap(),
                heads
            );
        }

        eprintln!("phase second_entrant_at_reservation_cannot_initialize_another_writer");
        {
            let f = LegacyFixture::rich();
            let mut entrants = 0;
            let mut pages = 0;
            let result = import_with_hook(&f.records, &f.source, f.key, |step| {
                match step {
                    ImportStep::Reserved => {
                        entrants += 1;
                        assert!(matches!(
                            import_legacy(&f.records, &f.source, f.key),
                            Err(ImportError::Record(RecordOpenError::Busy))
                        ));
                    }
                    ImportStep::PageInitialized(_, _) => pages += 1,
                    ImportStep::SnapshotRead => {}
                }
                Ok(())
            })
            .unwrap();
            assert_eq!(entrants, 1);
            assert_eq!(pages, 3);
            assert!(!result.already_imported);
            assert!(
                import_legacy(&f.records, &f.source, f.key)
                    .unwrap()
                    .already_imported
            );
        }
    }
}
