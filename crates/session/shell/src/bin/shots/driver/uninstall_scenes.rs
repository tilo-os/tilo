//! "Uninstall <name>?" for each kind of app, over the desktop as the shell
//! hosts it. The apps are made up: a capture never reads the machine's.
use std::path::PathBuf;

use shell::shell::app_list::{AppListItem, AppSource};

use super::ShellDriver;

impl ShellDriver {
    /// The sheet for the app kind `scene` names.
    pub(super) fn scene_app_uninstall(&mut self, scene: &str) {
        let (id, name, source) = match scene {
            "app-uninstall-flatpak" => (
                "org.gnome.Chess",
                "Chess",
                AppSource::Flatpak {
                    app_id: "org.gnome.Chess".into(),
                    user: true,
                },
            ),
            "app-uninstall-downloaded" => (
                "tilo-downloaded-krita",
                "Krita",
                AppSource::Downloaded {
                    program: PathBuf::from("/home/you/Applications/krita-5.2.AppImage"),
                    entry: PathBuf::from(
                        "/home/you/.local/share/applications/tilo-downloaded/tilo-downloaded-krita.desktop",
                    ),
                },
            ),
            "app-uninstall-system" => ("org.gnome.TextEditor", "Text Editor", AppSource::System),
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
