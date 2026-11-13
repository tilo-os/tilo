//! The places the picker's sidebar offers.

use std::path::{Path, PathBuf};

use crate::icons::Icon;

use super::Place;

/// Recent (when there is something to show), Home, the usual folders under it that are
/// `present` (read off the UI thread), then every other accessible root (the workspace
/// folders), each only if the caller may enter it.
pub(super) fn default_places(
    roots: &[PathBuf],
    home: Option<&Path>,
    recent: bool,
    present: &[PathBuf],
) -> Vec<Place> {
    let allowed = |p: &Path| roots.iter().any(|root| p.starts_with(root));
    let mut places: Vec<Place> = Vec::new();
    if recent {
        places.push(Place {
            key: Some("widget-filepicker-place-recent"),
            name: String::new(),
            path: PathBuf::new(),
            icon: Icon::History,
            recent: true,
        });
    }
    if let Some(home) = home.filter(|h| h.is_absolute()).map(Path::to_path_buf) {
        let well_known = [
            (
                None,
                "widget-filepicker-place-home",
                Icon::Home,
                home.clone(),
            ),
            (
                Some("Documents"),
                "widget-filepicker-place-documents",
                Icon::FileText,
                home.join("Documents"),
            ),
            (
                Some("Downloads"),
                "widget-filepicker-place-downloads",
                Icon::Download,
                home.join("Downloads"),
            ),
            (
                Some("Pictures"),
                "widget-filepicker-place-pictures",
                Icon::Image,
                home.join("Pictures"),
            ),
            (
                Some("Music"),
                "widget-filepicker-place-music",
                Icon::Music,
                home.join("Music"),
            ),
            (
                Some("Videos"),
                "widget-filepicker-place-videos",
                Icon::Video,
                home.join("Videos"),
            ),
        ];
        for (folder, key, icon, path) in well_known {
            if folder.is_some() && !present.contains(&path) {
                continue;
            }
            if allowed(&path) {
                places.push(Place {
                    key: Some(key),
                    name: String::new(),
                    path,
                    icon,
                    recent: false,
                });
            }
        }
    }
    for root in roots {
        if places.iter().any(|p| &p.path == root) {
            continue;
        }
        let name = root.file_name().map_or_else(
            || root.to_string_lossy().into_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        places.push(Place {
            key: None,
            name,
            path: root.clone(),
            icon: Icon::Folder,
            recent: false,
        });
    }
    places
}
