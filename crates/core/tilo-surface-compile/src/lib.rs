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

/// `value` as canonical JSON: the one encoding every surface is committed, addressed, hashed and
/// sent in.
#[must_use]
pub fn canonical(value: &Value) -> String {
    tilo_canonical_json::to_string(value)
}

/// The compiled form of the surface `toml` writes.
///
/// # Errors
/// `toml` is not a surface file.
pub fn compile(toml: &str) -> Result<String, String> {
    let value: Value = toml::from_str(toml).map_err(|error| error.to_string())?;
    Ok(canonical(&value))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A surface as an author writes it.
    const AUTHORED: &str = r#"
[[resources]]
kind = "so.tilo.test.note"
label = "Note"
version = 1
fields = [
  { path = "title", type = "text", stored = "value" },
  { path = "items", type = { collection = "item" }, stored = "collection" },
]

[[resources.verbs]]
id = "add"
kind = "undoable"
params = [
  { name = "title", required = true, type = "text" },
]
"#;

    /// Its compiled form: one address, the same on every target and every build.
    const COMPILED: &str = r#"{"resources":[{"fields":[{"path":"title","stored":"value","type":"text"},{"path":"items","stored":"collection","type":{"collection":"item"}}],"kind":"so.tilo.test.note","label":"Note","verbs":[{"id":"add","kind":"undoable","params":[{"name":"title","required":true,"type":"text"}]}],"version":1}]}"#;

    /// The BLAKE3 of [`COMPILED`]: what a change of this surface's app names.
    const ADDRESS: &str = "c6100ca8877209905e1a31149043ee2279972c3a05e66347853670b8734f677a";

    #[test]
    fn compiling_gives_the_one_canonical_address() {
        let compiled = compile(AUTHORED).expect("the authored surface compiles");
        assert_eq!(compiled, COMPILED);
        assert!(is_canonical(&compiled));
        assert_eq!(compile(AUTHORED).expect("it compiles"), compiled);
        assert_eq!(
            blake3::hash(compiled.as_bytes()).to_hex().to_string(),
            ADDRESS,
            "the same surface hashes to the same bytes wherever it is compiled"
        );
    }

    #[test]
    fn an_authored_form_is_not_canonical_and_a_pretty_json_is_not_either() {
        assert!(!is_canonical(AUTHORED));
        assert!(!is_canonical("{\n  \"resources\": []\n}"));
        assert!(is_canonical("{\"resources\":[]}"));
    }

    #[test]
    fn the_same_surface_has_one_address_however_its_keys_were_written() {
        let one = compile("[[resources]]\nkind = \"k\"\nlabel = \"L\"\nversion = 1\n").unwrap();
        let other = compile("[[resources]]\nversion = 1\nlabel = \"L\"\nkind = \"k\"\n").unwrap();
        assert_eq!(one, other);
    }
}
