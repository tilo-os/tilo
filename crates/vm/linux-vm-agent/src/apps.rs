//! Installed apps: the guest's Flatpak exports.

use std::path::{Path, PathBuf};

use linux_vm_protocol::InstalledApp;

#[must_use]
pub fn export_dirs(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".local/share/flatpak/exports/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
    ]
}

#[must_use]
pub fn installed(dirs: &[PathBuf]) -> Vec<InstalledApp> {
    let mut apps: Vec<InstalledApp> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        files.sort();
        for path in files {
            let Some(id) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".desktop"))
            else {
                continue;
            };
            if apps.iter().any(|app| app.id == id) {
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Some(app) = parse(&contents, id, &path) {
                apps.push(app);
            }
        }
    }
    apps.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    apps
}

fn parse(contents: &str, id: &str, path: &Path) -> Option<InstalledApp> {
    let entry = desktop_entry::entry::parse(contents, id, path, &[])?;
    if entry.entry_type != desktop_entry::EntryType::Application
        || entry.no_display
        || entry.hidden
        || entry.exec.is_none()
    {
        return None;
    }
    Some(InstalledApp {
        id: id.to_string(),
        name: entry.name,
        window_class: Some(entry.startup_wm_class.unwrap_or_else(|| id.to_string())),
        comment: entry.comment,
        terminal: entry.terminal,
    })
}
