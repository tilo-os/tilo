//! Notes preparation for a host-owned default notebook. No app-local writer,
//! profile discovery, permissions grant, or live fixture seeding happens here.
use std::path::Path;
use ui::application::{
    ApplicationHost, ComponentRunnerInputs, NativeProcessHost, PreparedComponentInputs,
};
use ui::launch::LaunchError;
use ui::record::{RecordCheckout, RecordId, RecordOpenError};

static NATIVE_LAUNCH_TARGET: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Retain the parsed positional launch target until the component factory has
/// a bound app instance. The native process hosts one Notes component.
pub fn set_native_launch_target(target: Option<String>) {
    if let Ok(mut pending) = NATIVE_LAUNCH_TARGET.lock() {
        *pending = target;
    }
}

pub(crate) fn take_native_launch_target() -> Option<String> {
    NATIVE_LAUNCH_TARGET.lock().ok()?.take()
}

pub fn default_notebook() -> RecordCheckout {
    RecordCheckout::new(
        RecordId::derived("default-notebook", &RecordId::for_tab(0)),
        crate::definition().record_kind.clone(),
    )
}

/// Prepare Notes from the trusted native launcher environment. Legacy files
/// are considered only for a standalone personal launch; a workspace must
/// never inherit a private notebook merely because it was opened first.
/// # Errors
/// Refuses invalid host identity, authority, storage, canonical records, or
/// legacy input through the same recovery-preserving paths as `prepare_default`.
pub fn prepare_native(
    runner_inputs: ComponentRunnerInputs,
) -> Result<PreparedComponentInputs, LaunchError> {
    let mut native = NativeProcessHost::open(crate::definition())?;
    let instance_id = native.instance_id();
    let legacy_root = if native.workspace_scoped() {
        None
    } else {
        crate::storage::default_root().map_err(LaunchError::Operation)?
    };
    prepare_default(
        native.host_mut(),
        instance_id,
        legacy_root.as_deref(),
        runner_inputs,
    )
}

/// Prepare the real component, importing a quiescent legacy workspace only
/// when the canonical notebook does not exist. Moving/deleting the legacy
/// directory after migration cannot change which notebook opens.
/// # Errors
/// Refuses corrupt canonical data, contested writers, invalid legacy sources,
/// durability failures and malformed local presentation without overwriting them.
#[allow(
    clippy::result_large_err,
    reason = "launch setup must preserve the recovery-bearing ComponentSetup error variant"
)]
pub fn prepare_default(
    host: &mut ApplicationHost,
    instance_id: u64,
    legacy_root: Option<&Path>,
    runner_inputs: ComponentRunnerInputs,
) -> Result<PreparedComponentInputs, LaunchError> {
    let checkout = default_notebook();
    let saved = host.saved_view_for(crate::definition(), instance_id, &checkout)?;
    let records = host.records_for(crate::definition())?;
    let mut selected = None;
    match records.try_open_record(&checkout) {
        Ok(_) => {}
        Err(RecordOpenError::MissingRecord) => {
            if saved.is_some() {
                return Err(LaunchError::Record(RecordOpenError::MissingRecord));
            }
            if let Some(root) = legacy_root {
                let metadata_path = crate::storage::workspace_file_path(root);
                let has_source = match std::fs::metadata(&metadata_path) {
                    Ok(_) => true,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                    Err(error) => {
                        return Err(LaunchError::Operation(format!(
                            "{}: {error}",
                            metadata_path.display()
                        )));
                    }
                };
                if has_source {
                    let imported = crate::legacy_import::source_key(root)
                        .and_then(|key| {
                            crate::legacy_import::import_legacy_into(
                                &records,
                                root,
                                key,
                                checkout.clone(),
                            )
                        })
                        .map_err(|error| match error {
                            crate::legacy_import::ImportError::Durability {
                                message,
                                checkout,
                                records,
                                ..
                            } => LaunchError::ComponentSetup {
                                source: Box::new(LaunchError::Operation(message)),
                                record: checkout,
                                recovery: ui::launch::LaunchRecovery::records(records),
                            },
                            other => LaunchError::Operation(other.to_string()),
                        })?;
                    selected = imported.initial_selected;
                }
            }
        }
        Err(error) => return Err(LaunchError::Record(error)),
    }
    let initial_view = if let Some(page) = selected {
        let expanded_pages = records
            .read(&checkout.id, |doc| {
                let workspace = crate::notebook::project(doc, Some(page))?;
                let mut ancestors = Vec::new();
                let mut current = page;
                while let Some(parent) = workspace
                    .pages()
                    .iter()
                    .find(|entry| entry.id == current)
                    .and_then(|entry| entry.parent)
                {
                    ancestors.push(parent);
                    current = parent;
                }
                Ok::<_, String>(ancestors)
            })
            .ok_or(LaunchError::Record(RecordOpenError::MissingRecord))?
            .map_err(LaunchError::Operation)?;
        Some(
            crate::local_view::NotesLocalViewV1 {
                selected_page: Some(page),
                active_tag: None,
                drawer_open: false,
                expanded_pages,
                editor_scroll: 0.0,
                focus: crate::local_view::NotesFocus::None,
                anchor: None,
                caret: None,
            }
            .encode()
            .map_err(LaunchError::Restore)?,
        )
    } else {
        None
    };
    host.prepare_default(
        crate::definition(),
        instance_id,
        checkout,
        initial_view,
        runner_inputs,
    )
}
