//! Which of this device's windows other devices must not relaunch.
//!
//! A Tilo app is a record plus logic that runs wherever the tab is shown, so
//! a room carries its window as an ordinary tab and every member opens its
//! own instance. A legacy (X11 / Wayland) app is not: its state lives in its
//! process here. Such a window is published as a
//! [`TileContent::HostedWindow`] naming this device, so other devices show
//! it as running here rather than starting a second, unrelated copy.

use std::sync::Arc;

use super::{Strip, TileContent};

/// This device as the host of its legacy windows: its device key and name,
/// and how to tell a legacy window from a Tilo app's. Set at session start;
/// absent (tests, offscreen renderer) every window is published as is.
#[derive(Clone)]
pub struct WindowHosting {
    /// Hex device key (iroh endpoint id) of this device.
    pub device: String,
    /// This device's name, as its certificate labels it.
    pub device_label: String,
    /// Whether a window announcing this app id belongs to a legacy app
    /// rather than an installed Tilo package.
    pub is_legacy: Arc<dyn Fn(&str) -> bool + Send + Sync>,
}

impl std::fmt::Debug for WindowHosting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowHosting")
            .field("device", &self.device)
            .field("device_label", &self.device_label)
            .finish_non_exhaustive()
    }
}

impl WindowHosting {
    /// `strips` as other devices are to see them: each legacy window this
    /// device runs (live, or still being restored) becomes a
    /// [`TileContent::HostedWindow`] on this device; everything else is
    /// unchanged.
    pub fn published(&self, strips: &[Strip]) -> Vec<Strip> {
        let mut strips = strips.to_vec();
        for tab in strips
            .iter_mut()
            .flat_map(|strip| &mut strip.window_groups)
            .flat_map(|group| &mut group.tabs)
        {
            let app_id = match &tab.tile {
                TileContent::Foreign { app_id, .. } | TileContent::LocalApp { app_id, .. }
                    if (self.is_legacy)(app_id) =>
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

    #[test]
    fn only_legacy_windows_are_published_as_hosted_here() {
        let hosting = WindowHosting {
            device: "cd".repeat(32),
            device_label: "Laptop".into(),
            is_legacy: Arc::new(|app| !app.starts_with("so.tilo.")),
        };
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
        ])];
        let published = hosting.published(&strips);
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
        // The device's own tree is untouched: the window stays live here.
        assert!(matches!(
            strips[0].window_groups[0].tabs[1].tile,
            TileContent::Foreign { .. }
        ));
    }
}
