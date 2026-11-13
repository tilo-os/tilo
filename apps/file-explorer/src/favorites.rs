//! Favourites: the folders pinned down the left, named from the person's own
//! XDG directories.

use std::path::{Path, PathBuf};
use ui::icons::Icon;
use ui::sysdirs::UserDir;

pub use ui::sysdirs::UserDirs;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FavKind {
    Home,
    Documents,
    Downloads,
    Pictures,
    Music,
    Videos,
    Folder,
}

impl FavKind {
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

fn kind_of(dirs: &UserDirs, path: &Path, home: &Path) -> FavKind {
    if path == home {
        return FavKind::Home;
    }
    match dirs.classify(path) {
        Some(dir) => FavKind::from_user_dir(dir),
        None => FavKind::Folder,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Favorite {
    pub label: String,
    pub path: PathBuf,
    pub is_home: bool,
    pub kind: FavKind,
}

impl Favorite {
    pub fn icon(&self) -> Icon {
        self.kind.icon()
    }
}

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

pub fn defaults(home: &Path, dirs: &UserDirs) -> Vec<Favorite> {
    let mut favs = vec![Favorite {
        label: crate::i18n::t("files-place-home"),
        path: home.to_path_buf(),
        is_home: true,
        kind: FavKind::Home,
    }];
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

pub fn favorite(path: PathBuf, home: &Path, dirs: &UserDirs) -> Favorite {
    let (label, is_home, kind) = describe(&path, home, dirs);
    Favorite {
        label,
        path,
        is_home,
        kind,
    }
}

pub fn reorder(favs: &mut Vec<Favorite>, from: usize, to: usize) {
    if from >= favs.len() || to >= favs.len() || from == to {
        return;
    }
    let item = favs.remove(from);
    favs.insert(to, item);
}

pub fn move_for(
    favs: &[Favorite],
    source: &Path,
    target: &Path,
    before: bool,
) -> Option<(usize, usize)> {
    let index_of = |path: &Path| favs.iter().position(|favourite| favourite.path == path);
    let from = index_of(source)?;
    let target = index_of(target)?;
    // The target index is read before the dragged row leaves its slot, so a
    // move down lands one place earlier than the slot named.
    let slot = if before { target } else { target + 1 };
    let to = if from < slot { slot - 1 } else { slot };
    (from != to).then_some((from, to))
}

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

/// Remove the favourite pointing at `path`; Home is pinned and stays.
pub fn remove(favs: &mut Vec<Favorite>, path: &Path) -> bool {
    let before = favs.len();
    favs.retain(|f| f.is_home || f.path != path);
    favs.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn favourites_default_localise_edit_and_persist() {
        {
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

        {
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
            let docs = favs.iter().find(|f| f.kind == FavKind::Documents).unwrap();
            assert_eq!(docs.path, PathBuf::from("/home/u/Dokumente"));
            assert_eq!(docs.icon(), Icon::FileText);
            let pics = favs.iter().find(|f| f.kind == FavKind::Pictures).unwrap();
            assert_eq!(pics.path, PathBuf::from("/home/u/Bilder"));
            assert_eq!(pics.icon(), Icon::Image);
        }

        {
            let home = Path::new("/home/u");
            let dirs = german_dirs();
            let place = crate::places::place_of(&home.join("Dokumente"), Some(home), &dirs);
            let path = crate::places::path_of(&place, Some(home), &dirs).unwrap();
            let docs = favorite(path, home, &dirs);
            assert_eq!(docs.kind, FavKind::Documents);
            assert_eq!(docs.label, "Dokumente");
            assert_eq!(docs.icon(), Icon::FileText);
        }

        {
            let home = Path::new("/home/u");
            let dirs = UserDirs::default();
            let mut favs = defaults(home, &dirs);
            assert!(!add(&mut favs, home.join("Documents"), home, &dirs));
            assert!(add(&mut favs, PathBuf::from("/data/projects"), home, &dirs));
            assert_eq!(favs.last().unwrap().label, "projects");
            assert_eq!(favs.last().unwrap().kind, FavKind::Folder);
            assert!(remove(&mut favs, Path::new("/data/projects")));
            assert!(favs.iter().all(|f| f.path != Path::new("/data/projects")));
            assert!(!remove(&mut favs, home));
            assert!(favs[0].is_home);
        }

        {
            let home = Path::new("/home/u");
            let dirs = UserDirs::default();
            let mut favs = defaults(home, &dirs);
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
