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

/// The notebook a launch opens: in a room, the room's default record, which
/// every device's first Notes window shares; standalone, the personal
/// notebook it always opened.
pub fn launch_notebook(workspace_scoped: bool) -> RecordCheckout {
    if workspace_scoped {
        RecordCheckout::new(
            RecordId::room_default(),
            crate::definition().record_kind.clone(),
        )
    } else {
        default_notebook()
    }
}

/// Prepare Notes from the trusted native launcher environment.
/// # Errors
/// Refuses invalid host identity, authority, storage or canonical records
/// through the same recovery-preserving paths as `prepare_default`.
pub fn prepare_native(
    runner_inputs: ComponentRunnerInputs,
) -> Result<PreparedComponentInputs, LaunchError> {
    let mut native = NativeProcessHost::open(crate::definition())?;
    let instance_id = native.instance_id();
    let checkout = launch_notebook(native.workspace_scoped());
    prepare_notebook(
        native.host_mut(),
        instance_id,
        checkout,
        None,
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
    prepare_notebook(
        host,
        instance_id,
        default_notebook(),
        legacy_root,
        runner_inputs,
    )
}

/// [`prepare_default`] for `checkout`, the notebook this launch opens.
#[allow(
    clippy::result_large_err,
    reason = "launch setup must preserve the recovery-bearing ComponentSetup error variant"
)]
fn prepare_notebook(
    host: &mut ApplicationHost,
    instance_id: u64,
    checkout: RecordCheckout,
    legacy_root: Option<&Path>,
    runner_inputs: ComponentRunnerInputs,
) -> Result<PreparedComponentInputs, LaunchError> {
    // A window carried in from another workspace shows the notebook it
    // brought, which the host opens by the instance's saved presentation.
    if host
        .saved_record(crate::definition(), instance_id)?
        .is_some_and(|named| named != checkout)
    {
        return host.prepare_default(
            crate::definition(),
            instance_id,
            checkout,
            None,
            runner_inputs,
        );
    }
    prepare_default_notebook(host, instance_id, checkout, legacy_root, runner_inputs)
}

/// [`prepare_default`] for the room's own default notebook.
#[allow(
    clippy::result_large_err,
    reason = "launch setup must preserve the recovery-bearing ComponentSetup error variant"
)]
fn prepare_default_notebook(
    host: &mut ApplicationHost,
    instance_id: u64,
    checkout: RecordCheckout,
    legacy_root: Option<&Path>,
    runner_inputs: ComponentRunnerInputs,
) -> Result<PreparedComponentInputs, LaunchError> {
    let saved = host.saved_view_for(crate::definition(), instance_id, &checkout)?;
    let records = host.records_for(crate::definition())?;
    let mut selected = None;
    match records.try_open_record(&checkout) {
        // A retired notebook moved to another workspace: the host starts its
        // successor here, and a moved notebook is never re-imported.
        Ok(_) | Err(RecordOpenError::Retired) => {}
        Err(RecordOpenError::MissingRecord) => {
            if saved.is_some() {
                return Err(LaunchError::Record(RecordOpenError::MissingRecord));
            }
            if let Some(root) = legacy_root {
                let metadata_path = crate::storage::workspace_file_path(root);
                let has_source = match crate::files::metadata(&metadata_path) {
                    Ok(_) => true,
                    Err(tilo_files::FsError::NotFound) => false,
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
                                record: Box::new(checkout),
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
