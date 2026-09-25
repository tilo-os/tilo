//! Standalone notebook preparation, keeping the production runner's authority and transport.
use std::path::{Path, PathBuf};
use ui::{
    record::{RecordCheckout, RecordStore},
    widget::TileHost,
};
pub struct PreparedNotebook {
    pub root: TileHost,
    pub records: RecordStore,
    pub checkout: RecordCheckout,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct CanonicalCustodyV1 {
    version: u32,
    state_root: PathBuf,
    workspace: String,
    package: String,
    record: String,
}

enum SavedCustodyProfile {
    Current(RecordCheckout),
    Historical(ui::record::LocalCanonicalCompatibility),
}
/// Prepare one explicitly scoped persistent notebook.
/// # Errors
/// Refuses active legacy writers, unacknowledged legacy migration, or storage errors.
pub fn prepare_notebook(
    root: &Path,
    state_root: &Path,
    workspace_id: &str,
    stopped_legacy: bool,
) -> Result<PreparedNotebook, String> {
    prepare_notebook_with_source(root, state_root, workspace_id, stopped_legacy, None)
}

/// Prepare a canonical notebook and queue an optional explicit Markdown import.
/// # Errors
/// Refuses invalid source input and the same custody/storage errors as [`prepare_notebook`].
pub fn prepare_notebook_with_source(
    root: &Path,
    state_root: &Path,
    workspace_id: &str,
    stopped_legacy: bool,
    markdown_source: Option<&str>,
) -> Result<PreparedNotebook, String> {
    let lock = crate::storage::lock_workspace(root)?;
    let marker_path = root.join(".notes-canonical-v1");
    let saved = match std::fs::read(&marker_path) {
        Ok(bytes) => Some(
            serde_json::from_slice::<CanonicalCustodyV1>(&bytes)
                .map_err(|e| format!("invalid canonical custody marker: {e}"))?,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    let (records, checkout, custody) = if let Some(saved) = saved {
        if saved.version != 1 || saved.workspace != workspace_id {
            return Err("this source is retired into a different canonical storage/workspace; open its original custody".into());
        }
        let requested_state = state_root.canonicalize().map_err(|e| {
            format!(
                "canonical custody state root is unavailable; reopen its original state root: {e}"
            )
        })?;
        if saved.state_root != requested_state {
            return Err("this source is retired into a different canonical storage/workspace; open its original custody".into());
        }
        let profile = match saved.package.as_str() {
            "dev.tilo.notes" => {
                let key = crate::legacy_import::source_key(root).map_err(|e| e.to_string())?;
                let checkout = crate::legacy_import::destination(key);
                if saved.record != checkout.id.as_str() {
                    return Err("this source is retired into a different canonical storage/workspace; open its original custody".into());
                }
                SavedCustodyProfile::Current(checkout)
            }
            "dev.qos.notes" => {
                let key = crate::legacy_import::qos_source_key(root).map_err(|e| e.to_string())?;
                let source_key = crate::legacy_import::source_key_hex(key);
                let record = crate::legacy_import::qos_destination(key);
                if saved.record != record {
                    return Err("this source is retired into a different canonical storage/workspace; open its original custody".into());
                }
                let evidence = ui::record::LocalCanonicalCompatibility::qos_notes_v1(
                    workspace_id,
                    &source_key,
                    &record,
                )
                .ok_or("invalid historical canonical custody marker")?;
                SavedCustodyProfile::Historical(evidence)
            }
            _ => {
                return Err("canonical custody marker has an unsupported package".into());
            }
        };
        let mut owners = ui::record::RecordOwners::new(requested_state);
        let records = owners
            .open(&saved.package, workspace_id)
            .map_err(|e| e.to_string())?;
        let checkout = match profile {
            SavedCustodyProfile::Current(checkout) => {
                records
                    .try_open_record(&checkout)
                    .map_err(|e| e.to_string())?;
                checkout
            }
            SavedCustodyProfile::Historical(evidence) => {
                records
                    .try_open_local_compatibility(&evidence)
                    .map_err(|e| e.to_string())?;
                evidence.checkout().clone()
            }
        };
        (records, checkout, None)
    } else {
        let key = crate::legacy_import::source_key(root).map_err(|e| e.to_string())?;
        let checkout = crate::legacy_import::destination(key);
        std::fs::create_dir_all(state_root).map_err(|e| e.to_string())?;
        let state_root = state_root.canonicalize().map_err(|e| e.to_string())?;
        let custody = CanonicalCustodyV1 {
            version: 1,
            state_root: state_root.clone(),
            workspace: workspace_id.to_owned(),
            package: crate::definition().app_id.to_owned(),
            record: checkout.id.as_str().to_owned(),
        };
        let records = ui::record::RecordOwners::new(state_root)
            .open(crate::definition().app_id, workspace_id)
            .map_err(|e| e.to_string())?;
        match records.try_open_record(&checkout) {
            Ok(_) => {}
            Err(ui::record::RecordOpenError::MissingRecord) => {
                if crate::storage::workspace_file_path(root).exists() {
                    if !stopped_legacy {
                        return Err("This notebook uses legacy Markdown storage. Close every older Notes process and external editor, then relaunch with --import-legacy-stopped. Source files will be retained; unavailable historical page bodies cannot be reconstructed.".into());
                    }
                    crate::legacy_import::import_legacy(&records, root, key)
                        .map_err(|e| e.to_string())?;
                } else {
                    // A retired source must never create an empty alternate notebook.
                    if root.join(".notes-canonical-v1").exists() {
                        return Err("canonical notebook is missing in this storage/workspace; use its original notebook directory and workspace".into());
                    }
                    records
                        .reserve_record(&checkout)
                        .map_err(|e| e.to_string())?
                        .initialize(crate::notebook::initialize_empty)
                        .map_err(|e| e.to_string())?;
                    records.try_flush()?;
                }
            }
            Err(error) => return Err(error.to_string()),
        }
        (records, checkout, Some(custody))
    };
    records
        .read(&checkout.id, crate::notebook::validate)
        .ok_or("notebook unavailable")??;
    records.try_flush()?;
    // Publish retirement only after acknowledged canonical durability. A crash
    // before this point retries the same destination without importing again.
    if let Some(custody) = custody {
        atomic_file::write(
            &marker_path,
            &serde_json::to_vec(&custody).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    let author = ui::collaboration::local_user_id();
    let mut app = records
        .read(&checkout.id, |doc| {
            crate::NotesApp::from_document(doc, &author)
        })
        .ok_or("notebook unavailable")??;
    app.configure_workspace_assets(root)?;
    if let Some(source) = markdown_source {
        app.queue_launch_markdown(source)?;
    }
    app.workspace_lock = Some(lock);
    let mut tile = TileHost::new(ui::widget::TileContent::App(Box::new(app)));
    tile.bind_record(records.clone(), checkout.clone());
    tile.register_document_history(&crate::framework_history::HISTORY)
        .map_err(|e| e.to_string())?;
    Ok(PreparedNotebook {
        root: tile,
        records,
        checkout,
    })
}
