use std::path::{Path, PathBuf};

/// Returns an ordered list of `(label, navigation_target)` pairs that mirror
/// the Iced app's `render_breadcrumbs` logic: if `cwd` is inside `home`, the
/// first label is `"󰋜 ~"` and subsequent labels are the components below
/// `home`. Otherwise the first label is `"󰋜"` for `/` and subsequent labels
/// are components below `/`.
pub fn breadcrumb_components(cwd: &Path, home: Option<&Path>) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if let Some(home) = home {
        if let Ok(rel) = cwd.strip_prefix(home) {
            out.push(("󰋜 ~".into(), home.to_path_buf()));
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
    out.push(("󰋜".into(), PathBuf::from("/")));
    let mut current = PathBuf::from("/");
    for comp in cwd.components() {
        if let std::path::Component::Normal(name) = comp {
            current.push(name);
            out.push((name.to_string_lossy().into_owned(), current.clone()));
        }
    }
    out
}
