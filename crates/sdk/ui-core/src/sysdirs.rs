//! Localized XDG user directories.
//!
//! Resolves the user's real, localized home + standard folders (via `tilo-paths`,
//! honouring `~/.config/user-dirs.dirs`) so a German desktop yields `~/Dokumente`,
//! not hard-coded English names. Shared by every "Places" list so they don't each
//! re-hardcode English strings. Key any persisted identity off [`UserDir`] *kind*,
//! not the localized label — the label doesn't survive a path-only round-trip.

use std::path::{Path, PathBuf};

/// The host's [`crate::host::HostPaths::user_home`]. The launcher sets it only
/// when the capability plan bound a path containing it — presence means "you
/// can read this", not merely "this is where it would be".
pub fn user_home_override() -> Option<PathBuf> {
    let path = crate::host::environment().paths.user_home.clone()?;
    // Fall through to `HOME` rather than open on a directory that doesn't exist.
    path.is_dir().then_some(path)
}

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
    /// Home and Desktop are excluded — callers place those explicitly.
    pub const CONTENT: [UserDir; 5] = [
        UserDir::Documents,
        UserDir::Downloads,
        UserDir::Pictures,
        UserDir::Music,
        UserDir::Videos,
    ];

    /// Used only when `user-dirs.dirs` doesn't define this directory; the
    /// resolved localized path is always preferred over this.
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

/// Resolve once at startup via [`UserDirs::from_system`]; tests inject an explicit
/// set so classification + labels are deterministic regardless of build-machine locale.
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
    pub fn from_system() -> Self {
        // A confined app's `HOME` is its own per-app directory with none of the
        // user's files, so prefer `TILO_USER_HOME` when the launcher bound it.
        if let Some(home) = user_home_override() {
            return Self::under(home);
        }
        Self {
            home: tilo_paths::home(),
            desktop: tilo_paths::desktop_dir(),
            documents: tilo_paths::document_dir(),
            downloads: tilo_paths::download_dir(),
            music: tilo_paths::audio_dir(),
            pictures: tilo_paths::picture_dir(),
            videos: tilo_paths::video_dir(),
        }
    }

    /// Used when `home` is not this process's `HOME`. Names are conventional
    /// English, not localized: `user-dirs.dirs` lives in the user's own config
    /// directory, which a confined app cannot read. A folder is listed only if
    /// it exists.
    fn under(home: PathBuf) -> Self {
        let sub = |name: &str| {
            let p = home.join(name);
            p.is_dir().then_some(p)
        };
        Self {
            desktop: sub("Desktop"),
            documents: sub("Documents"),
            downloads: sub("Downloads"),
            music: sub("Music"),
            pictures: sub("Pictures"),
            videos: sub("Videos"),
            home: Some(home),
        }
    }

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

    /// The resolved directory's own file name — `~/Dokumente` → `"Dokumente"`.
    pub fn label(&self, dir: UserDir) -> String {
        self.path(dir).and_then(|p| p.file_name()).map_or_else(
            || dir.fallback_name().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    /// Home wins over Desktop wins over the content dirs when paths coincide.
    pub fn classify(&self, path: &Path) -> Option<UserDir> {
        [
            UserDir::Home,
            UserDir::Desktop,
            UserDir::Documents,
            UserDir::Downloads,
            UserDir::Music,
            UserDir::Pictures,
            UserDir::Videos,
        ]
        .into_iter()
        .find(|&kind| self.path(kind) == Some(path))
    }

    /// `(kind, localized label, path)`, falling back to `home/<English>` so the
    /// list is never missing a standard entry. Home/Desktop are excluded — a
    /// caller adds those explicitly.
    pub fn content_places(&self, home: &Path) -> Vec<(UserDir, String, PathBuf)> {
        UserDir::CONTENT
            .iter()
            .map(|&kind| {
                let path = self
                    .path(kind)
                    .map_or_else(|| home.join(kind.fallback_name()), Path::to_path_buf);
                let label = path.file_name().map_or_else(
                    || kind.fallback_name().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                (kind, label, path)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn content_places_use_localized_names_paths_and_english_fallbacks() {
        {
            let dirs = german();
            let places = dirs.content_places(Path::new("/home/u"));
            let labels: Vec<&str> = places.iter().map(|(_, l, _)| l.as_str()).collect();
            assert_eq!(
                labels,
                ["Dokumente", "Downloads", "Bilder", "Musik", "Videos"]
            );
            let (kind, _, path) = &places[0];
            assert_eq!(*kind, UserDir::Documents);
            assert_eq!(path, &PathBuf::from("/home/u/Dokumente"));
        }

        {
            let places = UserDirs::default().content_places(Path::new("/home/u"));
            let labels: Vec<&str> = places.iter().map(|(_, l, _)| l.as_str()).collect();
            assert_eq!(
                labels,
                ["Documents", "Downloads", "Pictures", "Music", "Videos"]
            );
        }

        {
            let dirs = german();
            assert_eq!(
                dirs.classify(Path::new("/home/u/Bilder")),
                Some(UserDir::Pictures)
            );
            assert_eq!(dirs.classify(Path::new("/home/u")), Some(UserDir::Home));
            assert_eq!(
                dirs.classify(Path::new("/home/u/Musik")),
                Some(UserDir::Music)
            );
            assert_eq!(dirs.classify(Path::new("/data/projects")), None);
        }

        {
            let dirs = german();
            assert_eq!(dirs.label(UserDir::Documents), "Dokumente");
            assert_eq!(dirs.label(UserDir::Pictures), "Bilder");
            assert_eq!(UserDirs::default().label(UserDir::Music), "Music");
        }
    }
}
