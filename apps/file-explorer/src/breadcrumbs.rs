use std::path::{Path, PathBuf};

/// Returns an ordered list of `(label, navigation_target)` pairs for the
/// breadcrumb trail. The first entry is always the trail *root*: `"Home"`
/// when `cwd` is inside `home`, otherwise `"/"` (the filesystem root).
/// Subsequent entries are the path components below that root.
///
/// A workspace outranks home, and both outrank the filesystem: the trail
/// should name the most specific place the reader is actually in. For a
/// workspace that is the whole point of it - the folder belongs to the
/// room rather than to an account - and a reader shown the mount path
/// instead has been told the opposite, that this is scratch space on
/// somebody's machine. Where the driver put the mount is not the subject.
///
/// Labels are plain text — the leading root is drawn with a themed vector
/// icon by [`crate::breadcrumb_bar::BreadcrumbBar`], so nothing here depends
/// on a font glyph (the old `"󰋜 ~"` prefix rendered as tofu with no Nerd
/// Font loaded).
pub fn breadcrumb_components(
    cwd: &Path,
    home: Option<&Path>,
    workspace_roots: &[PathBuf],
) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if let Some(root) = crate::model::workspace_root_for(cwd, workspace_roots) {
        out.push((crate::model::workspace_label(root), root.to_path_buf()));
        let mut current = root.to_path_buf();
        // The remainder is empty when `cwd` is the root itself, which is
        // the ordinary case and needs no special handling: the trail is
        // then just the workspace, named.
        if let Ok(rel) = cwd.strip_prefix(root) {
            for comp in rel.components() {
                if let std::path::Component::Normal(name) = comp {
                    current.push(name);
                    out.push((name.to_string_lossy().into_owned(), current.clone()));
                }
            }
        }
        return out;
    }
    if let Some(home) = home
        && let Ok(rel) = cwd.strip_prefix(home)
    {
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
