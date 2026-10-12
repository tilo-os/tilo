//! The **Favourites / Places** model: the user-curated list of shortcut
//! folders shown at the top of the sidebar. The XDG home folder is a distinct,
//! specially-styled "Home" entry; the rest are ordinary folders the user can
//! add, remove and reorder. The list itself is the Files document's
//! ([`crate::resource::FileExplorer::places`]), stored as [`crate::places`]
//! names them.
//!
//! The standard entries follow the user's **real, localized XDG user
//! directories** (`~/.config/user-dirs.dirs`) — so a German desktop shows
//! *Dokumente / Bilder / Musik*, not hard-coded English names — resolved via
//! [`UserDirs`]. Each favourite carries a [`FavKind`] so its evocative glyph
//! survives the localized label and a persistence round-trip (the icon is keyed
//! off the kind, not a fragile label string-match).
//!
//! Rendering + drag-to-reorder live in [`crate::places_sidebar`]; this module
//! owns only the data, as pure functions over an explicit home + [`UserDirs`]
//! (so it is testable without depending on the machine's actual locale).

use std::path::{Path, PathBuf};
use ui::icons::Icon;
use ui::sysdirs::UserDir;

/// The user's resolved, **localized** XDG user directories — shared with every
/// other "Places" surface (file pickers) so nobody re-hardcodes English folder
/// names. Resolved once at startup via [`UserDirs::from_system`]; tests inject
/// an explicit set so the classification + labels are deterministic.
pub use ui::sysdirs::UserDirs;

/// Which standard place a favourite represents. Drives the evocative glyph
/// independently of the (possibly localized) label, and survives the
/// path-only persistence format because it is re-derived from the path on
/// load.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FavKind {
    Home,
    Documents,
    Downloads,
    Pictures,
    Music,
    Videos,
    /// Any other folder (a user-added favourite).
    Folder,
}

impl FavKind {
    /// The vector glyph for this kind.
    pub fn icon(self) -> Icon {
        match self {
            FavKind::Home => Icon::Home,
            FavKind::Documents => Icon::FileText,
            FavKind::Downloads => Icon::CloudDownload,
            FavKind::Pictures => Icon::Image,
            FavKind::Music => Icon::Music,
            FavKind::Videos => Icon::Film,
            FavKind::Folder => Icon::Folder,
        }
    }

    /// Map a shared [`UserDir`] kind to this app's favourite kind. Desktop (and
    /// anything without an evocative glyph here) reads as a plain folder.
    fn from_user_dir(dir: UserDir) -> FavKind {
        match dir {
            UserDir::Home => FavKind::Home,
            UserDir::Documents => FavKind::Documents,
            UserDir::Downloads => FavKind::Downloads,
            UserDir::Pictures => FavKind::Pictures,
            UserDir::Music => FavKind::Music,
            UserDir::Videos => FavKind::Videos,
            UserDir::Desktop => FavKind::Folder,
        }
    }
}

/// Classify `path` against the live [`UserDirs`] (else `Folder`, or `Home`
/// when it is exactly the home directory).
fn kind_of(dirs: &UserDirs, path: &Path, home: &Path) -> FavKind {
    if path == home {
        return FavKind::Home;
    }
    match dirs.classify(path) {
        Some(dir) => FavKind::from_user_dir(dir),
        None => FavKind::Folder,
    }
}

/// One favourite shortcut: a label, the folder it points at, whether it is the
/// special XDG home entry (rendered with a house glyph and never removed), and
/// its [`FavKind`] (the icon key).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Favorite {
    pub label: String,
    pub path: PathBuf,
    pub is_home: bool,
    pub kind: FavKind,
}

impl Favorite {
    /// The vector glyph for this favourite (keyed off its [`FavKind`], so a
    /// localized label like "Dokumente" still shows the documents glyph).
    pub fn icon(&self) -> Icon {
        self.kind.icon()
    }
}

/// Derive a display label, home-ness, and kind for `path`. The label is the
/// folder's own (localized) name; `Home` is special-cased.
fn describe(path: &Path, home: &Path, dirs: &UserDirs) -> (String, bool, FavKind) {
    let kind = kind_of(dirs, path, home);
    if kind == FavKind::Home {
        return (crate::i18n::t("files-place-home"), true, kind);
    }
    let label = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    (label, false, kind)
}

/// The out-of-the-box favourites: Home plus the standard XDG user folders,
/// using the user's real localized directories (`tilo-paths`). When a specific XDG
/// dir isn't defined, fall back gracefully to the conventional English
/// subfolder of `home` so the sidebar is never missing a standard place.
pub fn defaults(home: &Path, dirs: &UserDirs) -> Vec<Favorite> {
    let mut favs = vec![Favorite {
        label: crate::i18n::t("files-place-home"),
        path: home.to_path_buf(),
        is_home: true,
        kind: FavKind::Home,
    }];
    // The standard content places, resolved to the user's real localized
    // folder names via the shared helper (a German desktop yields
    // Dokumente / Bilder / Musik).
    for (dir, label, path) in dirs.content_places(home) {
        favs.push(Favorite {
            label,
            path,
            is_home: false,
            kind: FavKind::from_user_dir(dir),
        });
    }
    favs
}

/// The favourite for folder `path`: its (localized) label, and whether it is
/// Home or a standard folder, for its glyph.
pub fn favorite(path: PathBuf, home: &Path, dirs: &UserDirs) -> Favorite {
    let (label, is_home, kind) = describe(&path, home, dirs);
    Favorite {
        label,
        path,
        is_home,
        kind,
    }
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
pub fn add(favs: &mut Vec<Favorite>, path: PathBuf, home: &Path, dirs: &UserDirs) -> bool {
    if favs.iter().any(|f| f.path == path) {
        return false;
    }
    let (label, is_home, kind) = describe(&path, home, dirs);
    favs.push(Favorite {
        label,
        path,
        is_home,
        kind,
    });
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

    /// A German-localized set of XDG user dirs under `/home/u`.
    fn german_dirs() -> UserDirs {
        UserDirs {
            home: Some(PathBuf::from("/home/u")),
            documents: Some(PathBuf::from("/home/u/Dokumente")),
            downloads: Some(PathBuf::from("/home/u/Downloads")),
            pictures: Some(PathBuf::from("/home/u/Bilder")),
            music: Some(PathBuf::from("/home/u/Musik")),
            videos: Some(PathBuf::from("/home/u/Videos")),
            ..Default::default()
        }
    }

    /// Favourite defaults fall back to English or localised XDG dirs, keep their kind through persistence, dedup, pin home and persist reorders.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn favourites_default_localise_edit_and_persist() {
        // --- phase: defaults_fall_back_to_english_when_no_xdg_dirs ---
        {
            // With no resolved XDG dirs, the standard set uses the conventional
            // English subfolders of home, so the sidebar is never empty.
            let favs = defaults(Path::new("/home/u"), &UserDirs::default());
            assert!(favs[0].is_home);
            assert_eq!(favs[0].label, "Home");
            let labels: Vec<&str> = favs.iter().map(|f| f.label.as_str()).collect();
            assert_eq!(
                labels,
                [
                    "Home",
                    "Documents",
                    "Downloads",
                    "Pictures",
                    "Music",
                    "Videos"
                ]
            );
        }

        // --- phase: defaults_use_localized_xdg_names_and_paths ---
        {
            // The real bug: a German desktop must show its localized folder names,
            // pointing at the actual localized directories — not hard-coded
            // English ones.
            let home = Path::new("/home/u");
            let favs = defaults(home, &german_dirs());
            let labels: Vec<&str> = favs.iter().map(|f| f.label.as_str()).collect();
            assert_eq!(
                labels,
                [
                    "Home",
                    "Dokumente",
                    "Downloads",
                    "Bilder",
                    "Musik",
                    "Videos"
                ]
            );
            // The Documents favourite points at the localized directory.
            let docs = favs.iter().find(|f| f.kind == FavKind::Documents).unwrap();
            assert_eq!(docs.path, PathBuf::from("/home/u/Dokumente"));
            // And its evocative glyph survives the localized label.
            assert_eq!(docs.icon(), Icon::FileText);
            let pics = favs.iter().find(|f| f.kind == FavKind::Pictures).unwrap();
            assert_eq!(pics.path, PathBuf::from("/home/u/Bilder"));
            assert_eq!(pics.icon(), Icon::Image);
        }

        // --- phase: a_stored_place_resolves_to_a_localized_favourite_with_its_kind ---
        {
            // The document stores places by name; the kind (and thus the
            // glyph) is derived from the folder this device resolves.
            let home = Path::new("/home/u");
            let dirs = german_dirs();
            let place = crate::places::place_of(&home.join("Dokumente"), Some(home), &dirs);
            let path = crate::places::path_of(&place, Some(home), &dirs).unwrap();
            let docs = favorite(path, home, &dirs);
            assert_eq!(docs.kind, FavKind::Documents);
            assert_eq!(docs.label, "Dokumente");
            assert_eq!(docs.icon(), Icon::FileText);
        }

        // --- phase: add_dedups_and_remove_pins_home ---
        {
            let home = Path::new("/home/u");
            let dirs = UserDirs::default();
            let mut favs = defaults(home, &dirs);
            // Adding an existing path is a no-op.
            assert!(!add(&mut favs, home.join("Documents"), home, &dirs));
            // A fresh folder is appended with its file-name label.
            assert!(add(&mut favs, PathBuf::from("/data/projects"), home, &dirs));
            assert_eq!(favs.last().unwrap().label, "projects");
            assert_eq!(favs.last().unwrap().kind, FavKind::Folder);
            // Remove drops it again.
            assert!(remove(&mut favs, Path::new("/data/projects")));
            assert!(favs.iter().all(|f| f.path != Path::new("/data/projects")));
            // Home is pinned: a remove request for it is ignored.
            assert!(!remove(&mut favs, home));
            assert!(favs[0].is_home);
        }

        // --- phase: reorder_moves_an_entry ---
        {
            let home = Path::new("/home/u");
            let dirs = UserDirs::default();
            let mut favs = defaults(home, &dirs);
            // Move "Videos" (index 5) to the top-after-home position (index 1).
            reorder(&mut favs, 5, 1);
            let labels: Vec<&str> = favs.iter().map(|f| f.label.as_str()).collect();
            assert_eq!(
                labels,
                [
                    "Home",
                    "Videos",
                    "Documents",
                    "Downloads",
                    "Pictures",
                    "Music"
                ]
            );
        }
    }
}
