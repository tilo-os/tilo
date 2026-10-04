//! Installed apps: the guest's Flatpak exports.

use std::path::{Path, PathBuf};

use legacy_agent_protocol::InstalledApp;

/// Where Flatpak exports desktop entries, system-wide and for `home`.
#[must_use]
pub fn export_dirs(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".local/share/flatpak/exports/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
    ]
}

/// Every launchable app in `dirs`, first directory winning on a shared id.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatpak_exports_become_installed_apps() {
        let user = tempfile::tempdir().unwrap();
        let system = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("org.gnome.Calculator.desktop"),
            "[Desktop Entry]\nType=Application\nName=Calculator\nExec=flatpak run org.gnome.Calculator\n",
        )
        .unwrap();
        std::fs::write(
            system.path().join("org.gnome.Calculator.desktop"),
            "[Desktop Entry]\nType=Application\nName=Shadowed\nExec=x\n",
        )
        .unwrap();
        std::fs::write(
            system.path().join("com.visualstudio.code.desktop"),
            "[Desktop Entry]\nType=Application\nName=Visual Studio Code\nExec=code\nStartupWMClass=Code\n",
        )
        .unwrap();
        std::fs::write(
            system.path().join("hidden.desktop"),
            "[Desktop Entry]\nType=Application\nName=Hidden\nExec=x\nNoDisplay=true\n",
        )
        .unwrap();
        let apps = installed(&[user.path().to_path_buf(), system.path().to_path_buf()]);
        let names: Vec<(&str, &str, Option<&str>)> = apps
            .iter()
            .map(|app| {
                (
                    app.id.as_str(),
                    app.name.as_str(),
                    app.window_class.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                (
                    "org.gnome.Calculator",
                    "Calculator",
                    Some("org.gnome.Calculator")
                ),
                ("com.visualstudio.code", "Visual Studio Code", Some("Code")),
            ]
        );
    }
}
