use std::fmt::Write as _;
use std::path::Path;

use serde::Serialize;
use ui::prelude::{HeadlessApp, Invoke};

use scratchpad::{Note, Scratchpad};

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

/// Seed a fresh isolated `--data-dir` profile with 1,000 ASCII lines for
/// native diagnostics. The note is written by the body's own field edit,
/// through the same runtime and storage the app opens.
///
/// This refuses every existing path, including an empty directory or symlink.
/// Body hashes are deterministic BLAKE3; document serialization need not be.
pub fn seed_native_fixture(directory: &Path) -> anyhow::Result<NativeFixtureManifest> {
    let (parent, name) = tilo_files::file_at(directory)?;
    match tilo_files::blocking::metadata(&parent, &name) {
        Err(tilo_files::FsError::NotFound) => tilo_files::blocking::create_dir_all(&parent, &name)?,
        Ok(_) => anyhow::bail!("{} already exists", directory.display()),
        Err(error) => return Err(error.into()),
    }
    let body = (0..1000).fold(String::new(), |mut body, line| {
        let _ = writeln!(
            body,
            "Line {line:04}: A deterministic note for native scrolling and editing."
        );
        body
    });
    let prefixed = format!("x{body}");
    let manifest = NativeFixtureManifest {
        schema_version: 1,
        lines: 1000,
        editor_id: "note-editor",
        body_hash: blake3::hash(body.as_bytes()).to_hex().to_string(),
        body_bytes: body.len(),
        prefixed_x_hash: blake3::hash(prefixed.as_bytes()).to_hex().to_string(),
        prefixed_x_bytes: prefixed.len(),
    };
    let mut app = HeadlessApp::<Scratchpad>::open_data_dir(directory)?;
    app.invoke(Invoke::edit(&Note::paths().body.replace(body))?)?;
    app.flush()?;
    Ok(manifest)
}
