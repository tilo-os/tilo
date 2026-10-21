//! Which of this device's windows a room may carry, and how.
//!
//! A Tilo app is a record plus logic that runs wherever the tab is shown, so
//! a room carries its window as an ordinary tab and every member opens its
//! own instance. A window from a host or unconfined program (a desktop entry
//! the launcher ran "UNCONFINED", an X11 or Wayland client started outside
//! the launcher) is not: its state lives in its process here, and it is the
//! person's own business.
//!
//! - In a room whose every member is this person (their own devices,
//!   continuation), such a window is published as a
//!   [`TileContent::HostedWindow`] naming this device, so their other
//!   devices show it as running here and view it from here.
//! - In a room with anyone else in it (another person, an agent, a bot) it
//!   is never published: not its app, not its title. Once someone joins a
//!   room of the person's alone, the next projection takes those windows
//!   out of it.

use std::collections::HashMap;

use super::{DesktopTree, Strip, Tab, TabId, TileContent, Workspace, WorkspaceTree};

/// This device as the host of its unconfined windows: its device key and
/// name. Set at session start; absent (tests, offscreen renderer) such a
/// window reaches a room of the person's alone as it is.
#[derive(Clone, Debug)]
pub struct WindowHosting {
    /// Hex device key (iroh endpoint id) of this device.
    pub device: String,
    /// This device's name, as its certificate labels it.
    pub device_label: String,
}

impl WindowHosting {
    /// `strips` as the person's other devices are to see them: each window
    /// of a host or unconfined program ([`is_unconfined`]) becomes a
    /// [`TileContent::HostedWindow`] on this device; everything else is
    /// unchanged.
    pub fn published(&self, strips: &[Strip], launches: &HashMap<TabId, String>) -> Vec<Strip> {
        let mut strips = strips.to_vec();
        for tab in strips
            .iter_mut()
            .flat_map(|strip| &mut strip.window_groups)
            .flat_map(|group| &mut group.tabs)
        {
            if !is_unconfined(tab, launches) {
                continue;
            }
            let (TileContent::Foreign { app_id, .. } | TileContent::LocalApp { app_id, .. }) =
                &tab.tile
            else {
                continue;
            };
            tab.tile = TileContent::HostedWindow {
                app_id: app_id.clone(),
                label: tab.title.clone(),
                host: self.device.clone(),
                host_label: self.device_label.clone(),
            };
        }
        strips
    }
}

/// Whether `tab` is a window of a host or unconfined program: a live or
/// restoring window whose app the launcher did not start as that confined
/// Tilo package, according to `launches`. What a window calls itself never
/// makes it a Tilo app.
pub(crate) fn is_unconfined(tab: &Tab, launches: &HashMap<TabId, String>) -> bool {
    match &tab.tile {
        TileContent::Foreign { app_id, .. } | TileContent::LocalApp { app_id, .. } => {
            launches.get(&tab.id) != Some(app_id)
        }
        _ => false,
    }
}

/// Whether every member of `workspace`'s room is one person: it has an
/// owner, no other member, and grants nothing to anyone else (another
/// person, an agent, a task, a bot, or someone on a device not theirs).
/// A room whose owner is not known yet is not.
pub(crate) fn is_one_persons(workspace: &Workspace) -> bool {
    use tilo_action_policy::Subject;
    let policy = &workspace.policy;
    let Some(owner) = policy.owner.as_deref() else {
        return false;
    };
    policy.members.is_empty()
        && policy.grants.iter().all(|grant| match &grant.subject {
            Subject::Person(person) => person == owner,
            Subject::Role(_) | Subject::Shortcut(_) | Subject::App(_) => true,
            Subject::Agent(_) | Subject::Task(_) | Subject::Bot(_) | Subject::ForeignDevice => {
                false
            }
        })
}

/// `strips` as a room may carry them: without any window of a host or
/// unconfined program ([`is_unconfined`]), and without the groups left empty
/// by that. This device's own tree is untouched.
pub(crate) fn shareable(strips: &[Strip], launches: &HashMap<TabId, String>) -> Vec<Strip> {
    let mut strips = strips.to_vec();
    for strip in &mut strips {
        for group in &mut strip.window_groups {
            group.tabs.retain(|tab| !is_unconfined(tab, launches));
            if !group.tabs.iter().any(|tab| tab.id == group.active_tab)
                && let Some(first) = group.tabs.first()
            {
                group.active_tab = first.id;
            }
        }
        strip.window_groups.retain(|group| !group.tabs.is_empty());
    }
    strips
}

impl WorkspaceTree {
    /// `workspace`'s strips as its room may carry them: in a room of this
    /// person's alone, every window, an unconfined one as hosted here
    /// ([`WindowHosting::published`]); in any other room, none of the
    /// unconfined ones ([`shareable`]).
    pub(crate) fn publishable_strips(&self, workspace: &Workspace) -> Vec<Strip> {
        if !is_one_persons(workspace) {
            return shareable(&workspace.strips, &self.package_launches);
        }
        self.window_hosting.as_ref().map_or_else(
            || workspace.strips.clone(),
            |hosting| hosting.published(&workspace.strips, &self.package_launches),
        )
    }

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
            focus_history: Vec::new(),
        });
        strip
    }

    // USER REQUIREMENT (2026-10-20): "Any window from a host or unconfined program (launcher "host application … UNCONFINED") is never published to any room, private or public."
    #[test]
    fn only_windows_launched_as_a_tilo_package_may_reach_a_room() {
        let native = TileContent::Foreign {
            app_id: "so.tilo.notes".into(),
            surface_id: 4,
        };
        let strips = vec![strip_of(vec![
            native.clone(),
            TileContent::Foreign {
                app_id: "Alacritty".into(),
                surface_id: 5,
            },
            TileContent::LocalApp {
                app_id: "xterm".into(),
                label: "xterm".into(),
                note: String::new(),
            },
            // A client started outside the launcher that calls itself a
            // Tilo package is still unconfined.
            TileContent::Foreign {
                app_id: "so.tilo.files".into(),
                surface_id: 6,
            },
        ])];
        let launches = HashMap::from([(10, "so.tilo.notes".to_string())]);
        let shared = shareable(&strips, &launches);
        let tiles: Vec<&TileContent> = shared
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .map(|tab| &tab.tile)
            .collect();
        assert_eq!(tiles, [&native], "only the Tilo app's window");
        // The device's own tree is untouched: the windows stay live here.
        assert_eq!(strips[0].window_groups[0].tabs.len(), 4);
        // A launch recorded for a different package does not vouch for the
        // window the tab now shows.
        let mismatched = HashMap::from([(10, "so.tilo.files".to_string())]);
        assert!(
            shareable(&strips, &mismatched)[0].window_groups.is_empty(),
            "a group left with no window goes"
        );
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
        let shared = shareable(strips, &restored.package_launches);
        let tile_of = |id| {
            shared
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
        assert_eq!(tile_of(impostor), None, "the impostor stays on this device");
    }
}
