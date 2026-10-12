//! A bookmarked place as the Files document stores it.
//!
//! Bookmarks sync to every device the person uses, and each device has its
//! own home and its own (localized) user folders: Documents on one machine is
//! `~/Dokumente` on another. So a place is stored in terms every device can
//! resolve — a standard user folder by name (`xdg:documents`), a folder in the
//! home by its path inside it (`~/Projects/tilo`) — and only a folder outside
//! the home by its absolute path.

use std::path::{Path, PathBuf};

use ui::sysdirs::UserDir;

use crate::favorites::UserDirs;

const STANDARD_PREFIX: &str = "xdg:";
const HOME_PREFIX: &str = "~";

/// The places a new Files document starts with: Home and the standard user
/// folders, in the sidebar's order.
pub const DEFAULT_PLACES: [&str; 6] = [
    "xdg:home",
    "xdg:documents",
    "xdg:downloads",
    "xdg:pictures",
    "xdg:music",
    "xdg:videos",
];

fn standard_name(dir: UserDir) -> &'static str {
    match dir {
        UserDir::Home => "home",
        UserDir::Desktop => "desktop",
        UserDir::Documents => "documents",
        UserDir::Downloads => "downloads",
        UserDir::Music => "music",
        UserDir::Pictures => "pictures",
        UserDir::Videos => "videos",
    }
}

fn standard_dir(name: &str) -> Option<UserDir> {
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
    .find(|dir| standard_name(*dir) == name)
}

/// Where standard folder `dir` is on this device: its configured folder,
/// else the conventional one in `home`.
fn standard_path(dir: UserDir, home: &Path, dirs: &UserDirs) -> PathBuf {
    if dir == UserDir::Home {
        return home.to_path_buf();
    }
    dirs.path(dir)
        .map_or_else(|| home.join(dir.fallback_name()), Path::to_path_buf)
}

/// The stored form of `path` on this device, whose home is `home`.
pub fn place_of(path: &Path, home: Option<&Path>, dirs: &UserDirs) -> String {
    if let Some(home) = home {
        let standard = [
            UserDir::Home,
            UserDir::Desktop,
            UserDir::Documents,
            UserDir::Downloads,
            UserDir::Music,
            UserDir::Pictures,
            UserDir::Videos,
        ]
        .into_iter()
        .find(|dir| standard_path(*dir, home, dirs) == path);
        if let Some(dir) = standard {
            return format!("{STANDARD_PREFIX}{}", standard_name(dir));
        }
        if let Ok(inside) = path.strip_prefix(home) {
            return format!("{HOME_PREFIX}/{}", inside.display());
        }
    }
    path.display().to_string()
}

/// The folder `place` names on this device; `None` for a place in a home
/// this device does not have, or one stored in a form it cannot read.
pub fn path_of(place: &str, home: Option<&Path>, dirs: &UserDirs) -> Option<PathBuf> {
    if let Some(name) = place.strip_prefix(STANDARD_PREFIX) {
        return Some(standard_path(standard_dir(name)?, home?, dirs));
    }
    if let Some(inside) = place.strip_prefix(HOME_PREFIX) {
        let inside = inside.trim_start_matches('/');
        return Some(if inside.is_empty() {
            home?.to_path_buf()
        } else {
            home?.join(inside)
        });
    }
    let path = PathBuf::from(place);
    path.is_absolute().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn german() -> UserDirs {
        UserDirs {
            home: Some(PathBuf::from("/home/ute")),
            documents: Some(PathBuf::from("/home/ute/Dokumente")),
            ..UserDirs::default()
        }
    }

    /// A place stored on one device names the same folder on another: the
    /// standard folders by name, home folders by their path in the home.
    #[test]
    fn places_resolve_on_each_device_in_its_own_terms() {
        let alex = UserDirs::default();
        let alex_home = Path::new("/home/alex");
        let documents = place_of(&alex_home.join("Documents"), Some(alex_home), &alex);
        assert_eq!(documents, "xdg:documents");
        let project = place_of(&alex_home.join("Projects/tilo"), Some(alex_home), &alex);
        assert_eq!(project, "~/Projects/tilo");
        assert_eq!(place_of(alex_home, Some(alex_home), &alex), "xdg:home");
        assert_eq!(
            place_of(Path::new("/mnt/data"), Some(alex_home), &alex),
            "/mnt/data"
        );

        let ute = german();
        let ute_home = Path::new("/home/ute");
        assert_eq!(
            path_of(&documents, Some(ute_home), &ute),
            Some(PathBuf::from("/home/ute/Dokumente"))
        );
        assert_eq!(
            path_of(&project, Some(ute_home), &ute),
            Some(PathBuf::from("/home/ute/Projects/tilo"))
        );
        assert_eq!(
            path_of("/mnt/data", Some(ute_home), &ute),
            Some(PathBuf::from("/mnt/data"))
        );
        assert_eq!(path_of("xdg:documents", None, &ute), None);
        assert_eq!(path_of("xdg:attic", Some(ute_home), &ute), None);
        assert_eq!(path_of("relative/path", Some(ute_home), &ute), None);
    }
}
