//! A theme bundle: a theme document and the painter modules it names
//! (`docs/architecture/theming.md`, "Theme bundles"). A bundle is a directory holding
//! `theme.json` and one `<set>.wasm` for each painter set the document names that is not
//! built in. A bundle is never trusted: each module is held to the wasm host's limits,
//! and the document is validated like any other.

use crate::part_paint::{self, builtin, wasm};
use crate::theme::Theme;

/// The document's file in a bundle directory.
pub const DOCUMENT: &str = "theme.json";

const ROLES: [&str; 4] = ["window", "control", "floating", "bar"];

const MAX_SET_NAME: usize = 64;

/// Why a bundle is not a theme.
#[derive(Debug)]
pub enum BundleError {
    Read(String),
    Document(String),
    SetName(String),
    Missing(String),
    Painter { set: String, refusal: wasm::Refusal },
}

impl std::fmt::Display for BundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(why) => write!(f, "cannot read the bundle: {why}"),
            Self::Document(why) => write!(f, "not a valid theme document: {why}"),
            Self::SetName(name) => write!(f, "{name:?} is not a painter set name a bundle may use"),
            Self::Missing(name) => write!(
                f,
                "the document names the painter set {name:?}, which the bundle does not carry"
            ),
            Self::Painter { set, refusal } => write!(f, "the painter set {set:?}: {refusal}"),
        }
    }
}

impl std::error::Error for BundleError {}

fn set_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_SET_NAME
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')
        })
        && !name.starts_with('.')
}

fn named_sets_in(document: &str) -> Result<Vec<String>, BundleError> {
    let value: serde_json::Value =
        serde_json::from_str(document).map_err(|e| BundleError::Document(e.to_string()))?;
    let mut names: Vec<String> = Vec::new();
    for role in ROLES {
        let name = value
            .pointer(&format!("/material/{role}/painter"))
            .and_then(serde_json::Value::as_str);
        if let Some(name) = name
            && builtin::builtin(name).is_none()
            && !names.iter().any(|known| known == name)
        {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

/// The theme of the bundle whose document is `document`, taking each painter module it
/// names from `module` (by set name). The bundle is not a theme ([`BundleError`]); sets
/// loaded before the failure stay registered.
pub fn load(
    document: &str,
    module: impl FnMut(&str) -> Result<Option<Vec<u8>>, BundleError>,
) -> Result<Theme, BundleError> {
    load_with(document, module, |_, _| None)
}

/// The theme of the bundle whose document is `document`, taking each painter module it names
/// from `module` (by set name) and the machine code the device's daemon compiled for that module
/// from `compiled`, which is handed the module's bytes: a set the daemon compiled starts on that
/// code, and one it did not on the interpreter. A changed module is a new module, so a theme
/// changed in conversation starts on its new compiled entry without a restart.
pub fn load_with(
    document: &str,
    mut module: impl FnMut(&str) -> Result<Option<Vec<u8>>, BundleError>,
    mut compiled: impl FnMut(&str, &[u8]) -> Option<Vec<u8>>,
) -> Result<Theme, BundleError> {
    for name in named_sets_in(document)? {
        if !set_name_ok(&name) {
            return Err(BundleError::SetName(name));
        }
        let bytes = module(&name)?.ok_or_else(|| BundleError::Missing(name.clone()))?;
        let entry = compiled(&name, &bytes);
        let set = wasm::painter_set_with(&name, &bytes, entry.as_deref()).map_err(|refusal| {
            BundleError::Painter {
                set: name.clone(),
                refusal,
            }
        })?;
        part_paint::register_painter_set(set);
    }
    let value: serde_json::Value =
        serde_json::from_str(document).map_err(|e| BundleError::Document(e.to_string()))?;
    let document: crate::theme::ThemeDocument =
        serde_json::from_value(value).map_err(|e| BundleError::Document(e.to_string()))?;
    document
        .validate()
        .map_err(|why| BundleError::Document(why.to_owned()))?;
    Theme::from_document(&document).ok_or_else(|| BundleError::Document("refused".to_owned()))
}

/// The theme of the bundle in `dir`: its [`DOCUMENT`] and a `<set>.wasm` beside it for each set
/// the document names that is not built in. The machine code the device's daemon compiled for a
/// set is read from `compiled`, the store the launcher grants a process read-only, so applying
/// the theme draws on the device's compiled code; a set the daemon has not compiled, and every
/// browser, draws on the interpreter.
///
/// # Errors
///
/// A bundle file that does not read, or as [`load_with`].
#[cfg(not(target_arch = "wasm32"))]
pub fn load_from(
    dir: &std::path::Path,
    compiled: Option<&std::path::Path>,
) -> Result<Theme, BundleError> {
    let document = read_document(dir)?;
    load_with(
        &document,
        |set| {
            let path = dir.join(format!("{set}.wasm"));
            match std::fs::read(&path) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(BundleError::Read(format!("{}: {error}", path.display()))),
            }
        },
        |_, bytes| {
            let compiled = compiled?;
            tilo_theme_painter_wasm::compiled_in(compiled, bytes)
        },
    )
}

/// The bundle's document in `dir`: its [`DOCUMENT`], read and validated like any other theme
/// document, without loading the painter sets. A device that applies the bundle loads it with
/// [`load_from`] to register the sets the document names; this reads what the theme is.
///
/// # Errors
///
/// A document that does not read, or one that is not a theme this build admits.
#[cfg(not(target_arch = "wasm32"))]
pub fn document(dir: &std::path::Path) -> Result<crate::theme::ThemeDocument, BundleError> {
    let text = read_document(dir)?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| BundleError::Document(e.to_string()))?;
    let document: crate::theme::ThemeDocument =
        serde_json::from_value(value).map_err(|e| BundleError::Document(e.to_string()))?;
    document
        .validate()
        .map_err(|why| BundleError::Document(why.to_owned()))?;
    Ok(document)
}

#[cfg(not(target_arch = "wasm32"))]
fn read_document(dir: &std::path::Path) -> Result<String, BundleError> {
    std::fs::read_to_string(dir.join(DOCUMENT))
        .map_err(|error| BundleError::Read(format!("{}: {error}", dir.join(DOCUMENT).display())))
}

/// Stage the bundle at `source` into `dest`, the directory a sandbox mounts read-only for the
/// device's apps: its document and one module for each painter set it names that is not built in.
/// Each file is replaced atomically — hard-linked where the filesystem links the two, copied
/// otherwise — and anything else `dest` held is removed, so a process reading it finds a whole
/// bundle of the theme the session applied, and nothing of another package.
///
/// # Errors
/// A file cannot be read, written or replaced.
#[cfg(not(target_arch = "wasm32"))]
pub fn stage(source: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    let text = std::fs::read_to_string(source.join(DOCUMENT))?;
    let sets = named_sets_in(&text).map_err(|error| std::io::Error::other(error.to_string()))?;
    let mut wanted = vec![DOCUMENT.to_owned()];
    wanted.extend(sets.iter().map(|name| format!("{name}.wasm")));
    std::fs::create_dir_all(dest)?;
    for name in &wanted {
        place(&source.join(name), &dest.join(name))?;
    }
    for entry in std::fs::read_dir(dest)? {
        let entry = entry?;
        let name = entry.file_name();
        let ours = name
            .to_str()
            .is_some_and(|name| wanted.iter().any(|wanted| wanted == name));
        if ours {
            continue;
        }
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Place `source` at `dest` atomically: linked where the filesystem links the two, copied
/// otherwise, written beside `dest` and renamed over it.
#[cfg(not(target_arch = "wasm32"))]
fn place(source: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    let name = dest
        .file_name()
        .ok_or_else(|| std::io::Error::other("a bundle file has no name"))?;
    let mut staged = std::ffi::OsString::from(".");
    staged.push(name);
    staged.push(".new");
    let staged = dest.with_file_name(staged);
    let _ = std::fs::remove_file(&staged);
    if std::fs::hard_link(source, &staged).is_err() {
        std::fs::copy(source, &staged)?;
    }
    std::fs::rename(&staged, dest)
}

#[cfg(test)]
mod tests;
