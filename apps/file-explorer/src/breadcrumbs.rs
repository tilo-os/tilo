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
/// Labels are plain text: the trail is drawn by the shared breadcrumb, so
/// nothing here depends on a font glyph.
pub fn breadcrumb_components(
    cwd: &Path,
    home: Option<&Path>,
    workspace_roots: &[PathBuf],
) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if crate::recent::is_location(cwd) {
        out.push((crate::i18n::t("files-place-recent"), cwd.to_path_buf()));
        return out;
    }
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
        out.push((crate::i18n::t("files-place-home"), home.to_path_buf()));
        let mut current = home.to_path_buf();
        for comp in rel.components() {
            if let std::path::Component::Normal(name) = comp {
                current.push(name);
                out.push((name.to_string_lossy().into_owned(), current.clone()));
            }
        }
        return out;
    }
    let mut current = filesystem_root(cwd);
    out.push((current.display().to_string(), current.clone()));
    for comp in cwd.components() {
        if let std::path::Component::Normal(name) = comp {
            current.push(name);
            out.push((name.to_string_lossy().into_owned(), current.clone()));
        }
    }
    out
}

/// The drive or share root on Windows; `/` for unprefixed paths.
pub fn filesystem_root(path: &Path) -> PathBuf {
    if let Some(std::path::Component::Prefix(prefix)) = path.components().next() {
        let mut root = PathBuf::from(prefix.as_os_str());
        root.push(std::path::MAIN_SEPARATOR_STR);
        root
    } else {
        PathBuf::from("/")
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn breadcrumbs_stay_on_the_selected_drive_or_share() {
        for (directory, root) in [
            (r"D:\projects\notes", r"D:\"),
            (r"\\server\share\projects\notes", r"\\server\share\"),
        ] {
            let trail = breadcrumb_components(Path::new(directory), None, &[]);
            assert_eq!(trail.first().unwrap().1, PathBuf::from(root));
            assert_eq!(trail.last().unwrap().1, PathBuf::from(directory));
            assert!(trail.iter().all(|(_, path)| path.starts_with(root)));
        }
    }
}
