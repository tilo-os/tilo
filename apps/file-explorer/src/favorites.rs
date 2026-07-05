//! The **Favourites / Places** model: the user-curated list of shortcut
//! folders shown at the top of the sidebar. The XDG home folder is a distinct,
//! specially-styled "Home" entry; the rest are ordinary folders the user can
//! add, remove and reorder. The list is persisted (one path per line) so it
//! survives across sessions.
//!
//! Rendering + drag-to-reorder live in [`crate::places_sidebar`]; this module
//! owns only the data and its disk format, as pure functions over an explicit
//! directory (so it is testable without touching the real config dir).

use std::path::{Path, PathBuf};
use ui::icons::Icon;

/// One favourite shortcut: a label, the folder it points at, and whether it is
/// the special XDG home entry (rendered with a house glyph and never removed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Favorite {
    pub label: String,
    pub path: PathBuf,
    pub is_home: bool,
}

impl Favorite {
    /// The vector glyph for this favourite. Home gets the house; the standard
    /// XDG folders get their evocative glyph; anything else is a plain folder.
    pub fn icon(&self) -> Icon {
        if self.is_home {
            return Icon::Home;
        }
        match self.label.as_str() {
            "Documents" => Icon::FileText,
            "Downloads" => Icon::CloudDownload,
            "Pictures" => Icon::Image,
            "Music" => Icon::Music,
            "Videos" => Icon::Film,
            _ => Icon::Folder,
        }
    }
}

/// Derive a display label + home-ness for `path` relative to `home`.
fn describe(path: &Path, home: &Path) -> (String, bool) {
    if path == home {
        return ("Home".to_string(), true);
    }
    let label = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    (label, false)
}

/// The out-of-the-box favourites: Home plus the standard XDG user folders.
pub fn defaults(home: &Path) -> Vec<Favorite> {
    let sub = |label: &str, dir: &str| Favorite {
        label: label.to_string(),
        path: home.join(dir),
        is_home: false,
    };
    vec![
        Favorite { label: "Home".to_string(), path: home.to_path_buf(), is_home: true },
        sub("Documents", "Documents"),
        sub("Downloads", "Downloads"),
        sub("Pictures", "Pictures"),
        sub("Music", "Music"),
        sub("Videos", "Videos"),
    ]
}

/// Load the favourites from `dir/favorites` (one path per line). Falls back to
/// [`defaults`] when the file is absent or empty.
pub fn load(dir: &Path, home: &Path) -> Vec<Favorite> {
    match std::fs::read_to_string(dir.join("favorites")) {
        Ok(body) => {
            let favs: Vec<Favorite> = body
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(|l| {
                    let p = PathBuf::from(l);
                    let (label, is_home) = describe(&p, home);
                    Favorite { label, path: p, is_home }
                })
                .collect();
            if favs.is_empty() { defaults(home) } else { favs }
        }
        Err(_) => defaults(home),
    }
}

/// Persist `favs` to `dir/favorites` (one path per line). Errors are swallowed
/// — a failed save must never break the UI.
pub fn save(dir: &Path, favs: &[Favorite]) {
    let _ = std::fs::create_dir_all(dir);
    let body = favs
        .iter()
        .map(|f| f.path.display().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let _ = std::fs::write(dir.join("favorites"), body);
}

/// Move the favourite at `from` to index `to`, sliding the rest along
/// (`Vec::remove` + `insert` semantics — the same move a drag commits).
pub fn reorder(favs: &mut Vec<Favorite>, from: usize, to: usize) {
    if from >= favs.len() || to >= favs.len() || from == to {
        return;
    }
    let item = favs.remove(from);
    favs.insert(to, item);
}

/// Append `path` as a favourite unless it is already present. Returns whether
/// it was added.
pub fn add(favs: &mut Vec<Favorite>, path: PathBuf, home: &Path) -> bool {
    if favs.iter().any(|f| f.path == path) {
        return false;
    }
    let (label, is_home) = describe(&path, home);
    favs.push(Favorite { label, path, is_home });
    true
}

/// Remove the favourite pointing at `path`. The Home entry is pinned and never
/// removed. Returns whether anything was removed.
pub fn remove(favs: &mut Vec<Favorite>, path: &Path) -> bool {
    let before = favs.len();
    favs.retain(|f| f.is_home || f.path != path);
    favs.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_start_with_home_then_xdg_dirs() {
        let favs = defaults(Path::new("/home/u"));
        assert!(favs[0].is_home);
        assert_eq!(favs[0].label, "Home");
        let labels: Vec<&str> = favs.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(labels, ["Home", "Documents", "Downloads", "Pictures", "Music", "Videos"]);
    }

    #[test]
    fn add_dedups_and_remove_pins_home() {
        let home = Path::new("/home/u");
        let mut favs = defaults(home);
        // Adding an existing path is a no-op.
        assert!(!add(&mut favs, home.join("Documents"), home));
        // A fresh folder is appended with its file-name label.
        assert!(add(&mut favs, PathBuf::from("/data/projects"), home));
        assert_eq!(favs.last().unwrap().label, "projects");
        // Remove drops it again.
        assert!(remove(&mut favs, Path::new("/data/projects")));
        assert!(favs.iter().all(|f| f.path != Path::new("/data/projects")));
        // Home is pinned: a remove request for it is ignored.
        assert!(!remove(&mut favs, home));
        assert!(favs[0].is_home);
    }

    #[test]
    fn reorder_moves_an_entry_and_persists_the_new_order() {
        let home = Path::new("/home/u");
        let d = tempfile::tempdir().unwrap();
        let mut favs = defaults(home);
        // Move "Videos" (index 5) to the top-after-home position (index 1).
        reorder(&mut favs, 5, 1);
        let labels: Vec<&str> = favs.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(labels, ["Home", "Videos", "Documents", "Downloads", "Pictures", "Music"]);
        save(d.path(), &favs);

        // Reloading yields the reordered list, not the defaults.
        let reloaded = load(d.path(), home);
        let reloaded_labels: Vec<&str> = reloaded.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(reloaded_labels, ["Home", "Videos", "Documents", "Downloads", "Pictures", "Music"]);
        assert!(reloaded[0].is_home, "home is recognised on reload");
    }

    #[test]
    fn load_defaults_when_no_file() {
        let d = tempfile::tempdir().unwrap();
        let favs = load(d.path(), Path::new("/home/u"));
        assert_eq!(favs, defaults(Path::new("/home/u")));
    }
}
