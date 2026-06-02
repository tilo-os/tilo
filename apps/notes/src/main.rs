//! Thin binary entry point. Phase 1 bootstrap stub — the real widget
//! tree lands in the follow-up commit. This stub just confirms the
//! storage layer can seed a workspace.

use std::path::PathBuf;

use notes::storage;

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let root = storage::default_root()
        .unwrap_or_else(|| PathBuf::from("./notes"));
    storage::ensure_layout(&root)?;
    let workspace = storage::load_workspace(&root)?.unwrap_or_default();
    eprintln!(
        "notes: workspace root = {} ({} pages)",
        root.display(),
        workspace.pages().len()
    );
    Ok(())
}
