//! Inventory spill storage allocated below an acknowledged preparation operation.
#[cfg(target_os = "linux")]
mod native;
#[cfg(target_os = "linux")]
pub use native::review;

/// Hosts without native spill admission cannot mint inventory working storage.
#[cfg(not(target_os = "linux"))]
pub fn review(
    operation: &std::path::Path,
    preparation: std::sync::Arc<dyn crate::migration_review::InventoryScratchCustody>,
    current: crate::prepared_identity::DraftLock,
) -> std::io::Result<std::sync::Arc<crate::migration_review::InventoryScratch>> {
    let _ = (operation, preparation, current);
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Inventory working storage is unavailable on this host",
    ))
}
