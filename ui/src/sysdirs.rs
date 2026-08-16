//! Localized XDG user directories.
//!
//! Resolves the user's real, *localized* home + standard folders from the
//! environment (honouring `~/.config/user-dirs.dirs` via the `dirs` crate), so
//! a German desktop yields `~/Dokumente`, `~/Bilder`, `~/Musik` — never
//! hard-coded English names.
//!
//! This is shared so every surface that shows a "Places" list — the file
//! manager's Favourites sidebar, a file picker's shortcuts — derives the same
//! real names from **one** place, instead of each re-hardcoding English
//! strings (which was the bug: a sidebar listing English *Documents* beside
//! the machine's own *Dokumente*). Key the icon and any persisted identity off
//! the [`UserDir`] *kind*, not the (localized) label, so the evocative glyph
//! survives both localization and a path-only persistence round-trip.

use std::path::{Path, PathBuf};

/// One of the standard XDG user directories, independent of its localized
/// name. An app keys an icon / label / persisted identity off the *kind* so it
/// survives localization (a German *Bilder* is still [`UserDir::Pictures`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UserDir {
    Home,
    Desktop,
    Documents,
    Downloads,
    Music,
    Pictures,
    Videos,
}

impl UserDir {
    /// The standard content dirs (everything but Home/Desktop) in the
    /// conventional sidebar order. Home and Desktop are excluded because
    /// callers place them explicitly (Home is usually specially styled).
    pub const CONTENT: [UserDir; 5] = [
        UserDir::Documents,
        UserDir::Downloads,
        UserDir::Pictures,
        UserDir::Music,
        UserDir::Videos,
    ];

    /// The conventional English fallback folder name, used only when the
    /// platform / `user-dirs.dirs` doesn't define this directory (so a "Places"
    /// list is never missing a standard entry). The *resolved* localized path
    /// is always preferred over this.
    pub fn fallback_name(self) -> &'static str {
        match self {
            UserDir::Home => "Home",
            UserDir::Desktop => "Desktop",
            UserDir::Documents => "Documents",
            UserDir::Downloads => "Downloads",
            UserDir::Music => "Music",
            UserDir::Pictures => "Pictures",
            UserDir::Videos => "Videos",
        }
    }
}

/// The user's resolved, localized XDG user directories. Each is `None` when the
/// platform / `user-dirs.dirs` doesn't define it. Resolve once at startup via
/// [`UserDirs::from_system`]; tests inject an explicit set so classification +
/// labels are deterministic regardless of the build machine's locale.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserDirs {
    pub home: Option<PathBuf>,
    pub desktop: Option<PathBuf>,
    pub documents: Option<PathBuf>,
    pub downloads: Option<PathBuf>,
    pub music: Option<PathBuf>,
    pub pictures: Option<PathBuf>,
    pub videos: Option<PathBuf>,
}

impl UserDirs {
    /// Read the real, localized XDG user directories from the environment. The
    /// `dirs` crate honours `~/.config/user-dirs.dirs`, so on a German desktop
    /// `documents` resolves to `~/Dokumente`, `pictures` to `~/Bilder`, etc.
    pub fn from_system() -> Self {
        Self {
            home: dirs::home_dir(),
            desktop: dirs::desktop_dir(),
            documents: dirs::document_dir(),
            downloads: dirs::download_dir(),
            music: dirs::audio_dir(),
            pictures: dirs::picture_dir(),
            videos: dirs::video_dir(),
        }
    }

    /// The resolved path for `dir`, if the environment defines it.
    pub fn path(&self, dir: UserDir) -> Option<&Path> {
        let p = match dir {
            UserDir::Home => &self.home,
            UserDir::Desktop => &self.desktop,
            UserDir::Documents => &self.documents,
            UserDir::Downloads => &self.downloads,
            UserDir::Music => &self.music,
            UserDir::Pictures => &self.pictures,
            UserDir::Videos => &self.videos,
        };
        p.as_deref()
    }

    /// The localized display label for `dir`: the resolved directory's own
    /// (localized) file name — `~/Dokumente` → `"Dokumente"` — falling back to
    /// the conventional English name when the directory isn't resolved.
    pub fn label(&self, dir: UserDir) -> String {
        self.path(dir)
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.fallback_name().to_string())
    }

    /// Classify `path` as one of the standard user dirs (matching the resolved
    /// localized path), else `None`. Home wins over Desktop wins over the
    /// content dirs when they coincide.
    pub fn classify(&self, path: &Path) -> Option<UserDir> {
        for kind in [
            UserDir::Home,
            UserDir::Desktop,
            UserDir::Documents,
            UserDir::Downloads,
            UserDir::Music,
            UserDir::Pictures,
            UserDir::Videos,
        ] {
            if self.path(kind) == Some(path) {
                return Some(kind);
            }
        }
        None
    }

    /// The standard content places `(kind, localized label, path)` in the
    /// conventional order, resolving each against the localized directory (or
    /// the conventional `home/<English>` fallback so the list is never missing
    /// a standard entry). Home/Desktop are excluded — a caller adds those
    /// explicitly. This is the reusable list a "Places" sidebar renders.
    pub fn content_places(&self, home: &Path) -> Vec<(UserDir, String, PathBuf)> {
        UserDir::CONTENT
            .iter()
            .map(|&kind| {
                let path = self
                    .path(kind)
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| home.join(kind.fallback_name()));
                let label = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| kind.fallback_name().to_string());
                (kind, label, path)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A German-localized set of XDG user dirs under `/home/u`.
    fn german() -> UserDirs {
        UserDirs {
            home: Some(PathBuf::from("/home/u")),
            desktop: Some(PathBuf::from("/home/u/Schreibtisch")),
            documents: Some(PathBuf::from("/home/u/Dokumente")),
            downloads: Some(PathBuf::from("/home/u/Downloads")),
            music: Some(PathBuf::from("/home/u/Musik")),
            pictures: Some(PathBuf::from("/home/u/Bilder")),
            videos: Some(PathBuf::from("/home/u/Videos")),
        }
    }

    #[test]
    fn content_places_use_localized_names_and_paths() {
        let dirs = german();
        let places = dirs.content_places(Path::new("/home/u"));
        let labels: Vec<&str> = places.iter().map(|(_, l, _)| l.as_str()).collect();
        assert_eq!(labels, ["Dokumente", "Downloads", "Bilder", "Musik", "Videos"]);
        // The Documents place points at the localized directory, keyed by kind.
        let (kind, _, path) = &places[0];
        assert_eq!(*kind, UserDir::Documents);
        assert_eq!(path, &PathBuf::from("/home/u/Dokumente"));
    }

    #[test]
    fn content_places_fall_back_to_english_when_unresolved() {
        // No resolved dirs → the list is still complete, using conventional
        // English subfolders of home so a sidebar is never missing an entry.
        let places = UserDirs::default().content_places(Path::new("/home/u"));
        let labels: Vec<&str> = places.iter().map(|(_, l, _)| l.as_str()).collect();
        assert_eq!(labels, ["Documents", "Downloads", "Pictures", "Music", "Videos"]);
    }

    #[test]
    fn classify_matches_localized_paths_by_kind() {
        let dirs = german();
        assert_eq!(dirs.classify(Path::new("/home/u/Bilder")), Some(UserDir::Pictures));
        assert_eq!(dirs.classify(Path::new("/home/u")), Some(UserDir::Home));
        assert_eq!(dirs.classify(Path::new("/home/u/Musik")), Some(UserDir::Music));
        assert_eq!(dirs.classify(Path::new("/data/projects")), None);
    }

    #[test]
    fn label_is_the_localized_folder_name() {
        let dirs = german();
        assert_eq!(dirs.label(UserDir::Documents), "Dokumente");
        assert_eq!(dirs.label(UserDir::Pictures), "Bilder");
        // Unresolved dir → conventional English fallback.
        assert_eq!(UserDirs::default().label(UserDir::Music), "Music");
    }
}
