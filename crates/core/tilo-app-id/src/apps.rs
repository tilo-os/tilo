//! The first-party apps: the `so.tilo.*` id, one constant each.
//!
//! Current names only: nothing here reads or aliases a retired id. Used to
//! name an app in an action, a grant or a record, to tell a first-party app
//! from a foreign one, and to find the package a desktop built-in label names.

/// The first-party apps, one row each: the constant that names an app and its
/// id.
macro_rules! first_party_apps {
    ($($name:ident => $id:literal),* $(,)?) => {
        $(pub const $name: &str = $id;)*

        /// Every first-party package: one per app manifest.
        pub const FIRST_PARTY_PACKAGES: &[&str] = &[$($name),*];
    };
}

mod app_ids;

pub use app_ids::*;

/// The app an agent's own work acts under: what an agent's turns, tools and programs name as
/// their caller when they ask a model, call an action or write a record. No package declares it;
/// an agent is a member, not an app it installs.
pub const AGENT: &str = "so.tilo.agent";

/// The package namespace every first-party app is registered under.
pub const FIRST_PARTY_PREFIX: &str = "so.tilo";

/// The label a desktop built-in shows for `package` when it is not the
/// package's name in `PascalCase`.
const BUILTIN_LABELS: &[(&str, &str)] = &[
    (FILE_EXPLORER, "Files"),
    (CODE_EDITOR, "Editor"),
    (WORD_PROCESSOR, "Word"),
];

/// Whether `label` is `kebab` spelled in `PascalCase`: `pdf-viewer` ↔
/// `PdfViewer`. Exact — `Pdfviewer` and `pdfViewer` do not match.
fn is_pascal_case_of(label: &str, kebab: &str) -> bool {
    let mut label = label.chars();
    for word in kebab.split('-') {
        let mut chars = word.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        if label.next() != Some(first.to_ascii_uppercase()) {
            return false;
        }
        for c in chars {
            if label.next() != Some(c) {
                return false;
            }
        }
    }
    label.next().is_none()
}

/// The first-party app a desktop built-in label names, if one exists:
/// `Notes` → `so.tilo.notes`, `Files` → `so.tilo.file-explorer`, `Editor` →
/// `so.tilo.code-editor`. `None` for a label no app answers to (test
/// fixtures).
#[must_use]
pub fn app_id_for_builtin_label(label: &str) -> Option<&'static str> {
    FIRST_PARTY_PACKAGES.iter().copied().find(|package| {
        BUILTIN_LABELS
            .iter()
            .find(|(named, _)| named == package)
            .map_or_else(
                || {
                    package
                        .strip_prefix(FIRST_PARTY_PREFIX)
                        .and_then(|rest| rest.strip_prefix('.'))
                        .is_some_and(|name| is_pascal_case_of(label, name))
                },
                |(_, builtin)| *builtin == label,
            )
    })
}

/// The label a desktop built-in shows for the first-party app `app_id`: the inverse of
/// [`app_id_for_builtin_label`] (`so.tilo.file-explorer` → `Files`, `so.tilo.activity` →
/// `Activity`). `None` for an app that is not first-party.
#[must_use]
pub fn builtin_label_for_app_id(app_id: &str) -> Option<String> {
    let package = FIRST_PARTY_PACKAGES
        .iter()
        .copied()
        .find(|package| *package == app_id)?;
    if let Some((_, label)) = BUILTIN_LABELS.iter().find(|(named, _)| *named == package) {
        return Some((*label).to_owned());
    }
    let name = package
        .strip_prefix(FIRST_PARTY_PREFIX)?
        .strip_prefix('.')?;
    Some(
        name.split('-')
            .map(|word| {
                let mut chars = word.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_ascii_uppercase().to_string() + chars.as_str()
                })
            })
            .collect(),
    )
}

/// Whether `app_id` names a first-party app — any `so.tilo.*` package — as
/// opposed to a foreign one.
#[must_use]
pub fn is_first_party_app_id(app_id: &str) -> bool {
    app_id
        .strip_prefix(FIRST_PARTY_PREFIX)
        .is_some_and(|rest| rest.starts_with('.') && rest.len() > 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_first_party_app_is_named_by_the_label_that_finds_it() {
        assert_eq!(
            builtin_label_for_app_id(FILE_EXPLORER).as_deref(),
            Some("Files")
        );
        assert_eq!(builtin_label_for_app_id("org.gimp.GIMP"), None);
        for package in FIRST_PARTY_PACKAGES {
            let label = builtin_label_for_app_id(package).expect("a first-party app has a label");
            assert_eq!(app_id_for_builtin_label(&label), Some(*package), "{label}");
        }
    }

    #[test]
    fn every_app_names_itself_once() {
        let mut packages = FIRST_PARTY_PACKAGES.to_vec();
        packages.sort_unstable();
        packages.dedup();
        assert_eq!(packages.len(), FIRST_PARTY_PACKAGES.len());
        assert_eq!(SETTINGS, "so.tilo.settings");
    }

    #[test]
    fn builtin_labels_name_their_packages() {
        assert_eq!(app_id_for_builtin_label("Notes"), Some("so.tilo.notes"));
        assert_eq!(
            app_id_for_builtin_label("PdfViewer"),
            Some("so.tilo.pdf-viewer")
        );
        assert_eq!(
            app_id_for_builtin_label("Files"),
            Some("so.tilo.file-explorer")
        );
        assert_eq!(
            app_id_for_builtin_label("Editor"),
            Some("so.tilo.code-editor")
        );
        assert_eq!(app_id_for_builtin_label("FileExplorer"), None);
        assert_eq!(app_id_for_builtin_label("Scratch"), None);
        assert_eq!(app_id_for_builtin_label("notes"), None);
    }

    #[test]
    fn only_the_tilo_namespace_is_first_party() {
        assert!(is_first_party_app_id("so.tilo.chat"));
        assert!(is_first_party_app_id("so.tilo.desktop"));
        assert!(!is_first_party_app_id("so.tilonet.App"));
        assert!(!is_first_party_app_id("so.tilo."));
    }
}
