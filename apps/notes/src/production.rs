//! Standalone notebook preparation, keeping the production runner's authority and transport.
use std::path::Path;
use ui::{
    record::{RecordCheckout, RecordStore},
    widget::TileHost,
};
pub struct PreparedNotebook {
    pub root: TileHost,
    pub records: RecordStore,
    pub checkout: RecordCheckout,
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
    let key = crate::legacy_import::source_key(root).map_err(|e| e.to_string())?;
    let checkout = crate::legacy_import::destination(key);
    crate::files::create_dir_all(state_root).map_err(|e| e.to_string())?;
    let state_root = tilo_files::canonicalize(state_root).map_err(|e| e.to_string())?;
    let marker_path = root.join(".notes-canonical-v1");
    let custody = serde_json::json!({
        "version": 1, "state_root": state_root, "workspace": workspace_id,
        "package": crate::definition().app_id, "record": checkout.id.as_str(),
    });
    let retired = match crate::files::read(&marker_path) {
        Ok(bytes) => {
            let saved: serde_json::Value = serde_json::from_slice(&bytes)
                .map_err(|e| format!("invalid canonical custody marker: {e}"))?;
            if saved != custody {
                return Err("this source is retired into a different canonical storage/workspace; open its original custody".into());
            }
            true
        }
        Err(tilo_files::FsError::NotFound) => false,
        Err(error) => return Err(error.to_string()),
    };
    let records = ui::record::RecordOwners::new(state_root)
        .open(crate::definition().app_id, workspace_id)
        .map_err(|e| e.to_string())?;
    match records.try_open_record(&checkout) {
        Ok(_) => {
            records
                .read(&checkout.id, crate::notebook::validate)
                .ok_or("notebook unavailable")??;
        }
        Err(ui::record::RecordOpenError::MissingRecord) => {
            if retired {
                return Err("canonical notebook is missing; retained legacy files are not a recovery of its later history".into());
            }
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
    records.try_flush()?;
    // Publish retirement only after acknowledged canonical durability. A crash
    // before this point retries the same destination without importing again.
    if !retired {
        crate::files::write(
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
