//! Deterministic note content written through the real application's Save effect.

use std::path::Path;

use serde::Serialize;
use tilo_actions::Action;
use tilo_actions::OperationKind;

#[derive(Debug, Serialize)]
pub struct NativeFixtureManifest {
    pub schema_version: u32,
    pub lines: usize,
    pub editor_id: &'static str,
    pub body_hash: String,
    pub body_bytes: usize,
    pub prefixed_x_hash: String,
    pub prefixed_x_bytes: usize,
}

/// Seed a fresh isolated directory with 1,000 ASCII lines for native diagnostics.
///
/// This refuses every existing path, including an empty directory or symlink.
/// Body hashes are deterministic BLAKE3; Automerge serialization need not be.
///
/// # Errors
/// Returns directory creation, production storage, document, or Save errors.
pub fn seed_native_fixture(directory: &Path) -> anyhow::Result<NativeFixtureManifest> {
    std::fs::create_dir(directory)?;
    let body = (0..1000)
        .map(|line| {
            format!("Line {line:04}: A deterministic note for native scrolling and editing.\n")
        })
        .collect::<String>();
    let prefixed = format!("x{body}");
    let manifest = NativeFixtureManifest {
        schema_version: 1,
        lines: 1000,
        editor_id: "note-editor",
        body_hash: iroh_blobs::Hash::new(body.as_bytes()).to_hex(),
        body_bytes: body.len(),
        prefixed_x_hash: iroh_blobs::Hash::new(prefixed.as_bytes()).to_hex(),
        prefixed_x_bytes: prefixed.len(),
    };
    let (mut state, mut root) = crate::open(directory)?;
    crate::actions::EditNote { text: body }.apply(&mut state.doc)?;
    let saved = root
        .invoke_resource_operation(
            OperationKind::Effect,
            crate::SAVE,
            b"null",
            &state.doc,
            &state.root,
        )
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(saved, "production Save operation was not handled");
    Ok(manifest)
}
