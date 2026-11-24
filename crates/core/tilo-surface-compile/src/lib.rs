//! Compiles an app's authored surface — the `surface.toml` an author writes at build time — into
//! the one compiled form the system commits, addresses, hashes and sends: `surface.json`.
//!
//! The compiled form is [`tilo_canonical_json`]: every object's keys sorted and no insignificant
//! whitespace, so the same surface is always the same bytes and always hashes to the same
//! address, whichever build wrote it and on whichever target.
//!
//! Only build tools read `surface.toml`. A runtime reads the compiled form
//! ([`collab_types::surface::SurfaceFile`]), and refuses anything else: a surface sent in the
//! authored form has no address the system would have given it.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// The file an author writes: TOML, beside the app's manifest.
pub const SOURCE_FILE: &str = "surface.toml";

/// The compiled form: canonical JSON, the one form that is committed, addressed, hashed and sent.
pub const FILE: &str = "surface.json";

/// The compiled form of the surface `toml` writes.
///
/// # Errors
/// `toml` is not a surface file.
pub fn compile(toml: &str) -> Result<String, String> {
    let value: Value = toml::from_str(toml).map_err(|error| error.to_string())?;
    Ok(tilo_canonical_json::to_string(&value))
}

/// The compiled form of `resources` — a surface an app's code publishes — with `reads` the
/// resource kinds of other apps it reads.
#[must_use]
pub fn compile_value(resources: &[Value], reads: &[String]) -> String {
    let mut surface = serde_json::Map::new();
    surface.insert("resources".into(), Value::Array(resources.to_vec()));
    if !reads.is_empty() {
        surface.insert(
            "reads".into(),
            Value::Array(reads.iter().cloned().map(Value::String).collect()),
        );
    }
    tilo_canonical_json::to_string(&Value::Object(surface))
}

/// Whether `json` is the canonical compiled form of itself: re-encoding it gives the same bytes.
#[must_use]
pub fn is_canonical(json: &str) -> bool {
    serde_json::from_str::<Value>(json)
        .is_ok_and(|value| tilo_canonical_json::to_string(&value) == json)
}

/// The compiled form of the `surface.toml` at `path`.
///
/// # Errors
/// The file cannot be read, or is not a surface file.
pub fn compile_file(path: &Path) -> Result<String, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    compile(&text).map_err(|error| format!("{}: {error}", path.display()))
}

/// Compile `app_dir`'s [`SOURCE_FILE`] into its [`FILE`], when it has one, and answer the compiled
/// file's path. `None` for an app with no authored surface.
///
/// # Errors
/// The authored surface cannot be read or is not one, or the compiled file cannot be written.
pub fn write_compiled(app_dir: &Path) -> Result<Option<PathBuf>, String> {
    let source = app_dir.join(SOURCE_FILE);
    if !source.is_file() {
        return Ok(None);
    }
    let compiled = compile_file(&source)?;
    let path = app_dir.join(FILE);
    std::fs::write(&path, compiled).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(Some(path))
}
