//! The first-party packages, by their `so.tilo.*` ids.
//!
//! Current names only: nothing here reads or aliases a retired id. Used to
//! tell a first-party app from a foreign one, and to find the package a
//! desktop built-in label names.

/// The package namespace every first-party app is registered under.
pub const FIRST_PARTY_PREFIX: &str = "so.tilo";

include!("first_party_catalogue.rs");

/// The label a desktop built-in shows for `package` when it is not the
/// package's name in `PascalCase`.
const BUILTIN_LABELS: &[(&str, &str)] = &[("so.tilo.file-explorer", "Files")];

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

/// The first-party package a desktop built-in label names, if one exists:
/// `Notes` → `so.tilo.notes`, `Files` → `so.tilo.file-explorer`. `None` for
/// a label no package answers to (the shell's own `Editor`, test fixtures).
#[must_use]
pub fn package_id_for_builtin_label(label: &str) -> Option<&'static str> {
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
    fn builtin_labels_name_their_packages() {
        assert_eq!(package_id_for_builtin_label("Notes"), Some("so.tilo.notes"));
        assert_eq!(
            package_id_for_builtin_label("PdfViewer"),
            Some("so.tilo.pdf-viewer")
        );
        assert_eq!(
            package_id_for_builtin_label("Files"),
            Some("so.tilo.file-explorer")
        );
        assert_eq!(package_id_for_builtin_label("FileExplorer"), None);
        assert_eq!(package_id_for_builtin_label("Editor"), None);
        assert_eq!(package_id_for_builtin_label("notes"), None);
    }

    #[test]
    fn only_the_tilo_namespace_is_first_party() {
        assert!(is_first_party_app_id("so.tilo.chat"));
        assert!(is_first_party_app_id("so.tilo.desktop"));
        assert!(!is_first_party_app_id("so.tilonet.App"));
        assert!(!is_first_party_app_id("so.tilo."));
    }
}
