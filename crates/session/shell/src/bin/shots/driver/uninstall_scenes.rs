//! "Uninstall \<name\>?" for each kind of app, over the desktop as the shell
//! hosts it. The apps are made up: a capture never reads the machine's.
use std::path::PathBuf;

use shell::shell::app_list::{AppListItem, AppSource, FlatpakInstallation};

use super::ShellDriver;

impl ShellDriver {
    /// The sheet for the app kind `scene` names.
    pub(super) fn scene_app_uninstall(&mut self, scene: &str) {
        // This run's private home, so the copies' folders read as the
        // person's own.
        let home = tilo_paths::home().unwrap_or_else(|| PathBuf::from("/home/you"));
        let (id, name, source) = match scene {
            "app-uninstall-flatpak" => (
                "org.gnome.Chess",
                "Chess",
                AppSource::Flatpak {
                    app_id: "org.gnome.Chess".into(),
                    installation: FlatpakInstallation::Person,
                },
            ),
            "app-uninstall-downloaded" => (
                "tilo-downloaded-krita",
                "Krita",
                AppSource::Downloaded {
                    program: home.join("Applications/krita-5.2.AppImage"),
                    entry: home.join(".local/share/applications/tilo-downloaded/tilo-downloaded-krita.desktop"),
                    appimage: true,
                    locations: Vec::new(),
                },
            ),
            "app-uninstall-downloaded-copies" => (
                "tilo-downloaded-tilo-imager",
                "Tilo Imager",
                AppSource::Downloaded {
                    program: home.join("Downloads/Tilo-Imager-x86_64.AppImage"),
                    entry: home.join(".local/share/applications/tilo-downloaded/tilo-downloaded-tilo-imager.desktop"),
                    appimage: true,
                    locations: vec![home.join("Downloads/tilo-imager-linux-x86_64/tilo-imager")],
                },
            ),
            "app-uninstall-system" => ("org.gnome.TextEditor", "Text Editor", AppSource::System),
            "app-uninstall-system-tilo" => {
                ("so.tilo.notes", "Notes", AppSource::Tilo { added: false })
            }
            "app-uninstall-device-flatpak" => (
                "org.gnome.Maps",
                "Maps",
                AppSource::Flatpak {
                    app_id: "org.gnome.Maps".into(),
                    installation: FlatpakInstallation::Device {
                        administrator: true,
                    },
                },
            ),
            _ => ("so.tilo.notes", "Notes", AppSource::Tilo { added: true }),
        };
        let _ = self.paint_settled();
        self.host_mut().ask_uninstall(AppListItem {
            id: id.into(),
            name: name.into(),
            icon: None,
            source,
            size: None,
        });
        // Past the sheet's entrance.
        self.step_frames(20);
    }
}
