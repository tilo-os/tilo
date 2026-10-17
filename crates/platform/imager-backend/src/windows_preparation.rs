//! Native recovery export for selected Windows data and sealed account suggestions.
use crate::windows_inventory::Inventory;
use std::path::Path;

/// Private sign-in metadata; never serialized into the public review receipt.
pub use account_custody::hints::AccountHint;

/// Copy the reviewed selection and bind sealed suggestions to a durable receipt.
///
/// # Errors
/// Refuses invalid destinations, incomplete copies, sealing or durability failures.
pub fn export(
    inventory: &Inventory,
    hints: &[account_custody::hints::AccountHint],
    password: &[u8],
    destination: &Path,
) -> Result<crate::windows_media::WindowsMedia, String> {
    use crate::migration::PinnedDirectory;
    use std::io::Write;
    let parent = PinnedDirectory::open_absolute(destination).map_err(|e| e.to_string())?;
    inventory.validate_export_destination(&parent)?;
    let name = format!("tilo-windows-{}", uuid::Uuid::new_v4());
    let output = parent
        .create_child(std::ffi::OsStr::new(&name))
        .map_err(|e| e.to_string())?;
    let binding = uuid::Uuid::new_v4().to_string();
    let sealed = account_custody::hints::seal_password(password, &binding, hints)
        .map_err(|e| e.to_string())?;
    let dir = cap_std::fs::Dir::from_std_file(output.file.try_clone().map_err(|e| e.to_string())?);
    let mut options = cap_std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = dir
        .open_with("account-hints.sealed", &options)
        .map_err(|e| e.to_string())?;
    file.write_all(sealed.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())?;
    let mut verified_bytes = 0_u64;
    let mut verified_files = 0_usize;
    for (index, root) in inventory.roots.iter().enumerate() {
        if !root.has_copy_selection() {
            continue;
        }
        let report = inventory.copy_selected(
            &root.source_id,
            &destination.join(&name).join(format!("source-{index}")),
            &std::sync::atomic::AtomicBool::new(false),
        )?;
        if !report.complete() {
            return Err("Selected data was not completely copied; verified output retained".into());
        }
        verified_bytes = verified_bytes.saturating_add(report.verified_bytes);
        verified_files = verified_files.saturating_add(report.verified_files.len());
    }
    // No source environment, account identity, source path or filename inventory
    // is written to the public receipt. It never authorizes replacement.
    let receipt = serde_json::to_vec(&serde_json::json!({"schema":1,"hint_binding":binding,
        "budget":inventory.budget(), "verified_bytes":verified_bytes, "verified_files":verified_files,
        "replacement_authorized":false}))
    .map_err(|e| e.to_string())?;
    let mut file = dir
        .open_with("review.json", &options)
        .map_err(|e| e.to_string())?;
    file.write_all(&receipt)
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())?;
    output.validate_destination().map_err(|e| e.to_string())?;
    crate::migration_durability::sync_directory(&output.file).map_err(|e| e.to_string())?;
    crate::migration_durability::sync_directory(&parent.file).map_err(|e| e.to_string())?;
    let prepared =
        account_custody::hints::media::Prepared::seal(password, &binding, hints, &receipt)
            .map_err(|e| e.to_string())?;
    crate::windows_media::WindowsMedia::new(prepared, destination.join(name).join("review.json"))
        .map_err(|e| e.to_string())
}
