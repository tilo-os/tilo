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

fn named_sets(document: &serde_json::Value) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for role in ROLES {
        let name = document
            .pointer(&format!("/material/{role}/painter"))
            .and_then(serde_json::Value::as_str);
        if let Some(name) = name
            && builtin::builtin(name).is_none()
            && !names.iter().any(|known| known == name)
        {
            names.push(name.to_owned());
        }
    }
    names
}

/// The theme of the bundle whose document is `document`, taking each painter module it
/// names from `module` (by set name). The bundle is not a theme ([`BundleError`]); sets
/// loaded before the failure stay registered.
pub fn load(
    document: &str,
    mut module: impl FnMut(&str) -> Result<Option<Vec<u8>>, BundleError>,
) -> Result<Theme, BundleError> {
    let value: serde_json::Value =
        serde_json::from_str(document).map_err(|e| BundleError::Document(e.to_string()))?;
    for name in named_sets(&value) {
        if !set_name_ok(&name) {
            return Err(BundleError::SetName(name));
        }
        let bytes = module(&name)?.ok_or_else(|| BundleError::Missing(name.clone()))?;
        let set = wasm::painter_set(&name, &bytes).map_err(|refusal| BundleError::Painter {
            set: name.clone(),
            refusal,
        })?;
        part_paint::register_painter_set(set);
    }
    let document: crate::theme::ThemeDocument =
        serde_json::from_value(value).map_err(|e| BundleError::Document(e.to_string()))?;
    document
        .validate()
        .map_err(|why| BundleError::Document(why.to_owned()))?;
    Theme::from_document(&document).ok_or_else(|| BundleError::Document("refused".to_owned()))
}

#[cfg(test)]
mod tests;
