//! Which of this device's windows other devices must not relaunch.
//!
//! A Tilo app is a record plus logic that runs wherever the tab is shown, so
//! a room carries its window as an ordinary tab and every member opens its
//! own instance. A legacy (X11 / Wayland) app is not: its state lives in its
//! process here. Such a window is published as a
//! [`TileContent::HostedWindow`] naming this device, so other devices show
//! it as running here rather than starting a second, unrelated copy.

use std::collections::HashMap;

use super::{DesktopTree, Strip, TabId, TileContent, WorkspaceTree};

/// This device as the host of its legacy windows: its device key and name.
/// Set at session start; absent (tests, offscreen renderer) every window is
/// published as is.
#[derive(Clone, Debug)]
pub struct WindowHosting {
    /// Hex device key (iroh endpoint id) of this device.
    pub device: String,
    /// This device's name, as its certificate labels it.
    pub device_label: String,
}

impl WindowHosting {
    /// `strips` as other devices are to see them: each window this device
    /// runs (live, or still being restored) that was not launched as a Tilo
    /// package, according to `launches`, becomes a
    /// [`TileContent::HostedWindow`] on this device; everything else is
    /// unchanged. What a window calls itself never makes it a Tilo app.
    pub fn published(&self, strips: &[Strip], launches: &HashMap<TabId, String>) -> Vec<Strip> {
        let mut strips = strips.to_vec();
        for tab in strips
            .iter_mut()
            .flat_map(|strip| &mut strip.window_groups)
            .flat_map(|group| &mut group.tabs)
        {
            let app_id = match &tab.tile {
                TileContent::Foreign { app_id, .. } | TileContent::LocalApp { app_id, .. }
                    if launches.get(&tab.id) != Some(app_id) =>
                {
                    app_id.clone()
                }
                _ => continue,
            };
            tab.tile = TileContent::HostedWindow {
                app_id,
                label: tab.title.clone(),
                host: self.device.clone(),
                host_label: self.device_label.clone(),
            };
        }
        strips
    }
}

impl WorkspaceTree {
    /// Record that `tab`'s window was launched as Tilo package `package`.
    /// Only the compositor's adoption of a toplevel whose client the
    /// launcher authenticated as that confined package calls this.
    pub(crate) fn remember_package_launch(&mut self, tab: TabId, package: String) {
        if self.package_launches.get(&tab) != Some(&package) {
            self.package_launches.insert(tab, package);
            self.queue_save();
        }
    }

    /// Forget launches whose tab is gone or now shows another app.
    pub(crate) fn prune_package_launches(&mut self) {
        Self::prune_package_launches_of(&self.desktop, &mut self.package_launches);
    }

    /// Drop the launches in `launches` whose tab is gone from `desktop` or
    /// now shows another app.
    pub(crate) fn prune_package_launches_of(
        desktop: &DesktopTree,
        launches: &mut HashMap<TabId, String>,
    ) {
        let live: HashMap<TabId, &str> = desktop
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .filter_map(|tab| match &tab.tile {
                TileContent::Foreign { app_id, .. } | TileContent::LocalApp { app_id, .. } => {
                    Some((tab.id, app_id.as_str()))
                }
                _ => None,
            })
            .collect();
        launches.retain(|tab, package| live.get(tab) == Some(&package.as_str()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{Tab, WindowGroup, WindowWidth};

    fn strip_of(tiles: Vec<TileContent>) -> Strip {
        let mut strip = Strip::new(1.into(), "Main");
        strip.window_groups.push(WindowGroup {
            id: 2,
            tabs: (10_u64..)
                .zip(tiles)
                .map(|(id, tile)| Tab {
                    id,
                    title: format!("Window {}", id.saturating_sub(10)),
                    tile,
                })
                .collect(),
            active_tab: 10,
            width: WindowWidth::Normal,
            maximized: false,
        });
        strip
    }

    fn hosting() -> WindowHosting {
        WindowHosting {
            device: "cd".repeat(32),
            device_label: "Laptop".into(),
        }
    }

    #[test]
    fn only_windows_launched_as_a_tilo_package_are_published_as_apps() {
        let native = TileContent::Foreign {
            app_id: "so.tilo.notes".into(),
            surface_id: 4,
        };
        let strips = vec![strip_of(vec![
            native.clone(),
            TileContent::Foreign {
                app_id: "org.gimp.GIMP".into(),
                surface_id: 5,
            },
            TileContent::LocalApp {
                app_id: "xterm".into(),
                label: "xterm".into(),
                note: String::new(),
            },
            // A client started outside the launcher that calls itself a
            // Tilo package is still a legacy window.
            TileContent::Foreign {
                app_id: "so.tilo.files".into(),
                surface_id: 6,
            },
        ])];
        let launches = HashMap::from([(10, "so.tilo.notes".to_string())]);
        let published = hosting().published(&strips, &launches);
        let tiles: Vec<&TileContent> = published[0].window_groups[0]
            .tabs
            .iter()
            .map(|tab| &tab.tile)
            .collect();
        assert_eq!(tiles[0], &native, "a Tilo app opens on every device");
        assert_eq!(
            tiles[1],
            &TileContent::HostedWindow {
                app_id: "org.gimp.GIMP".into(),
                label: "Window 1".into(),
                host: "cd".repeat(32),
                host_label: "Laptop".into(),
            }
        );
        assert!(matches!(
            tiles[2],
            TileContent::HostedWindow { app_id, .. } if app_id == "xterm"
        ));
        assert!(
            matches!(
                tiles[3],
                TileContent::HostedWindow { app_id, .. } if app_id == "so.tilo.files"
            ),
            "a package name alone never makes a window a Tilo app"
        );
        // The device's own tree is untouched: the window stays live here.
        assert!(matches!(
            strips[0].window_groups[0].tabs[1].tile,
            TileContent::Foreign { .. }
        ));
        // A launch recorded for a different package does not vouch for the
        // window the tab now shows.
        let mismatched = HashMap::from([(10, "so.tilo.files".to_string())]);
        assert!(matches!(
            hosting().published(&strips, &mismatched)[0].window_groups[0].tabs[0].tile,
            TileContent::HostedWindow { .. }
        ));
    }

    #[test]
    fn a_package_launched_window_is_still_a_tilo_app_after_a_restart() {
        let dir = tempfile::tempdir().expect("state dir");
        let path = dir.path().join("state.json");
        let mut tree = WorkspaceTree::bootstrap_default();
        let workspace = tree.desktop.active;
        let tab = crate::shell::tab_binding::adopt_toplevel(
            &mut tree,
            (7_u64 << 32) | 1,
            Some("so.tilo.notes".into()),
        );
        let impostor = crate::shell::tab_binding::adopt_toplevel(
            &mut tree,
            (8_u64 << 32) | 1,
            Some("so.tilo.files".into()),
        );
        tree.remember_package_launch(tab, "so.tilo.notes".into());
        crate::shell::persist::save_tree_to(&tree, &path).expect("saved");
        let restored = crate::shell::persist::load_tree_from(&path).expect("restored");
        assert_eq!(
            restored.package_launches.get(&tab).map(String::as_str),
            Some("so.tilo.notes")
        );
        let strips = &restored
            .desktop
            .workspaces
            .iter()
            .find(|w| w.id == workspace)
            .expect("workspace")
            .strips;
        let published = hosting().published(strips, &restored.package_launches);
        let tile_of = |id| {
            published
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .find(|candidate| candidate.id == id)
                .map(|candidate| candidate.tile.clone())
        };
        assert!(
            matches!(tile_of(tab), Some(TileContent::LocalApp { app_id, .. }) if app_id == "so.tilo.notes"),
            "the restored window still opens on other devices"
        );
        assert!(matches!(
            tile_of(impostor),
            Some(TileContent::HostedWindow { .. })
        ));
    }
}
