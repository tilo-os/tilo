use std::path::{Path, PathBuf};

/// Returns an ordered list of `(label, navigation_target)` pairs for the
/// breadcrumb trail. The first entry is always the trail *root*: `"Home"`
/// when `cwd` is inside `home`, otherwise `"/"` (the filesystem root).
/// Subsequent entries are the path components below that root.
///
/// Labels are plain text — the leading root is drawn with a themed vector
/// icon by [`crate::breadcrumb_bar::BreadcrumbBar`], so nothing here depends
/// on a font glyph (the old `"󰋜 ~"` prefix rendered as tofu with no Nerd
/// Font loaded).
pub fn breadcrumb_components(cwd: &Path, home: Option<&Path>) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if let Some(home) = home {
        if let Ok(rel) = cwd.strip_prefix(home) {
            out.push(("Home".into(), home.to_path_buf()));
            let mut current = home.to_path_buf();
            for comp in rel.components() {
                if let std::path::Component::Normal(name) = comp {
                    current.push(name);
                    out.push((name.to_string_lossy().into_owned(), current.clone()));
                }
            }
            return out;
        }
    }
    out.push(("/".into(), PathBuf::from("/")));
    let mut current = PathBuf::from("/");
    for comp in cwd.components() {
        if let std::path::Component::Normal(name) = comp {
            current.push(name);
            out.push((name.to_string_lossy().into_owned(), current.clone()));
        }
    }
    out
}
