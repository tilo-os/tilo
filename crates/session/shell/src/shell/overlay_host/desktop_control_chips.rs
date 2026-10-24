//! While a program running in an app may control the desktop beyond that
//! app's workspace ([`crate::shell::desktop_control`]), the bar of each tab
//! the grant applies to says so: a "Controls your desktop" chip
//! ([`ui::widget::TabBar::desktop_control_chip`]). One model: the chip shows
//! exactly where the grant applies. A "this tab" grant shows on the tab it
//! was asked from and nowhere else; an "always" grant, which is the app's in
//! its workspace, shows on every tab of that app there. Pressing the chip
//! opens what applies to that tab and for how long, and lets the person stop
//! it, there and then.

use std::collections::HashSet;

use ui::widget::GroupRow;
use vello::kurbo::Rect;

use super::ShellOverlayHost;
use super::tab_menu::TabMenuTarget;
use crate::shell::desktop_control::{DesktopControlGrants, always_allowed, revoke_always};
use crate::shell::i18n::{t, t_args};
use crate::shell::state::{AppKind, Tab, TabId, TileContent, WorkspaceTree};

/// The tabs whose bar shows the chip, as of one tree and grants generation.
#[derive(Default)]
pub(crate) struct DesktopControlMarks {
    generation: Option<(u64, u64)>,
    tabs: HashSet<TabId>,
}

/// A grant that applies to one tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Granted {
    /// For as long as the tab that asked stays open.
    UntilTabCloses,
    /// Always, for the tab's app in its workspace.
    Always,
}

/// The app a tab shows, as a grant names it: the package it was launched
/// as, or Terminal for a Terminal tab the desktop shows itself.
fn tab_package(tree: &WorkspaceTree, tab: &Tab) -> Option<String> {
    if let Some(package) = tree.package_launches.get(&tab.id) {
        return Some(package.clone());
    }
    matches!(
        tab.tile,
        TileContent::AppPlaceholder {
            kind: AppKind::Terminal,
            ..
        }
    )
    .then(|| ush::host::TERMINAL_APP_ID.to_owned())
}

/// `tab`, with its app and its workspace's room.
fn find(tree: &WorkspaceTree, tab: TabId) -> Option<(&Tab, String)> {
    tree.desktop.workspaces.iter().find_map(|workspace| {
        workspace
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find(|candidate| candidate.id == tab)
            .map(|found| (found, workspace.room().to_string()))
    })
}

/// The grants that apply to `tab` in the workspace of `room`.
fn granted_on(
    tree: &WorkspaceTree,
    grants: &DesktopControlGrants,
    tab: &Tab,
    room: &str,
) -> Vec<Granted> {
    let mut granted = Vec::new();
    if grants.tabs().any(|grant| grant.tabs.contains(&tab.id)) {
        granted.push(Granted::UntilTabCloses);
    }
    if tab_package(tree, tab).is_some_and(|package| always_allowed(tree, room, &package)) {
        granted.push(Granted::Always);
    }
    granted
}

/// The grants that apply to the tab `tab`, with its app.
pub(crate) fn granted(
    tree: &WorkspaceTree,
    grants: &DesktopControlGrants,
    tab: TabId,
) -> Option<(String, Vec<Granted>)> {
    let (found, room) = find(tree, tab)?;
    let granted = granted_on(tree, grants, found, &room);
    let package = tab_package(tree, found)?;
    (!granted.is_empty()).then_some((package, granted))
}

/// The tabs of `tree` whose bar shows the chip: each tab a grant applies to.
pub(crate) fn desktop_control_tabs(
    tree: &WorkspaceTree,
    grants: &DesktopControlGrants,
) -> HashSet<TabId> {
    let mut marked = HashSet::new();
    for workspace in &tree.desktop.workspaces {
        let room = workspace.room().to_string();
        let tabs = workspace
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs);
        for tab in tabs {
            if !granted_on(tree, grants, tab, &room).is_empty() {
                marked.insert(tab.id);
            }
        }
    }
    marked
}

/// What the chip's menu says: what the app may do, and for how long each
/// grant that applies lasts.
pub(crate) fn captions(package: &str, granted: &[Granted]) -> Vec<String> {
    let app = crate::shell::launcher::display_name_for_app_id_without_waiting(package)
        .unwrap_or_else(|| package.to_owned());
    std::iter::once(t_args("desktop-control-allows", &[("app", app.into())]))
        .chain(granted.iter().map(|how| {
            t(match how {
                Granted::UntilTabCloses => "desktop-control-until-tab-closes",
                Granted::Always => "desktop-control-always-here",
            })
        }))
        .collect()
}

impl ShellOverlayHost {
    /// Every window bar marks the windows whose app may control the
    /// desktop. Runs every frame: the marks are read again only once the
    /// tree or the grants changed. Whether a bar changed.
    pub(super) fn sync_desktop_control_chips(&mut self) -> bool {
        let Some(chrome) = self.chrome.as_ref() else {
            return false;
        };
        {
            let chrome = chrome.borrow();
            let generation = (
                chrome.tree_handle().generation(),
                chrome.desktop_control.generation(),
            );
            if self.desktop_control.generation != Some(generation) {
                self.desktop_control.tabs =
                    desktop_control_tabs(&chrome.tree(), &chrome.desktop_control);
                self.desktop_control.generation = Some(generation);
            }
        }
        let marked = &self.desktop_control.tabs;
        let mut changed = false;
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for group in &mut row.groups {
                let ids: Vec<u64> = group.tabs().iter().map(|tab| tab.id).collect();
                for id in ids {
                    changed |= group.set_tab_desktop_control(id, marked.contains(&id));
                }
            }
        }
        changed
    }

    /// Drains every bar's press on its chip and opens the chip's menu for
    /// the last one. Whether a menu opened.
    pub(super) fn harvest_desktop_control_presses(&mut self) -> bool {
        let mut pressed: Option<(TabMenuTarget, Rect)> = None;
        for (&strip, &ptr) in &self.group_row_ptrs {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for group in &mut row.groups {
                if let Some((tab, chip)) = group.take_desktop_control_press() {
                    let target = TabMenuTarget {
                        strip,
                        group: group.id,
                        tab,
                    };
                    pressed = Some((target, chip));
                }
            }
        }
        let Some((target, chip)) = pressed else {
            return false;
        };
        let Some(chrome) = self.chrome.as_ref() else {
            return false;
        };
        let captions = {
            let chrome = chrome.borrow();
            let Some((package, granted)) =
                granted(&chrome.tree(), &chrome.desktop_control, target.tab)
            else {
                return false;
            };
            captions(&package, &granted)
        };
        let display = self.surface_area(Some(chip.center()));
        self.layers
            .tab_menu
            .open_desktop_control(target, chip, captions, display);
        true
    }

    /// Open the chip's menu on the first bar that shows the chip, as a
    /// press on it does: for captures and tests. Whether it opened.
    pub fn open_desktop_control_menu(&mut self) -> bool {
        let pressed = self.group_row_ptrs.values().any(|&ptr| {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            row.groups
                .iter_mut()
                .any(ui::widget::WindowGroup::activate_desktop_control)
        });
        pressed && self.harvest_desktop_control_presses()
    }

    /// Stop what lets `tab` control the desktop: the "this tab" grant shown
    /// on it, and its app's "always" grant in its workspace. Whether the tab
    /// was found: afterwards no grant applies to it either way.
    pub(super) fn stop_desktop_control(&self, tab: TabId) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let mut chrome = chrome.borrow_mut();
        let Some((package, room)) = find(&chrome.tree(), tab)
            .map(|(found, room)| (tab_package(&chrome.tree(), found), room))
        else {
            return false;
        };
        chrome.desktop_control.revoke_on_tab(tab);
        if let Some(package) = package {
            let tree = chrome.tree_handle();
            revoke_always(&mut tree.write(), &room, &package);
        }
        chrome.bump();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::desktop_control::{Holder, TabGrant, allow_always};

    fn me() -> Holder {
        let pid = std::process::id();
        let (_, start) = crate::shell::host_runtime::proc_stat(pid).expect("own stat");
        Holder { pid, start }
    }

    fn terminal_tab(id: TabId, title: &str) -> Tab {
        Tab {
            id,
            title: title.into(),
            tile: TileContent::AppPlaceholder {
                kind: AppKind::Terminal,
                label: "Terminal".into(),
            },
            app_title: None,
        }
    }

    /// The chip shows exactly where a grant applies: a "this tab" grant on
    /// the tab it was asked from alone, an "always" grant on every Terminal
    /// tab of its workspace, and nothing on another app's window.
    #[test]
    fn the_chip_shows_exactly_where_a_grant_applies() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let (asked, other, notes) = {
            let group = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
            let asked = group.tabs[0].id;
            let other = asked.wrapping_add(1_000);
            let notes = asked.wrapping_add(2_000);
            group.tabs[0] = terminal_tab(asked, "claude");
            group.tabs.push(terminal_tab(other, "build"));
            group.tabs.push(Tab {
                id: notes,
                title: "Groceries".into(),
                tile: TileContent::Foreign {
                    app_id: "so.tilo.notes".into(),
                    surface_id: 2,
                },
                app_title: None,
            });
            (asked, other, notes)
        };
        tree.package_launches.insert(notes, "so.tilo.notes".into());
        let room = tree.desktop.workspaces[0].room().to_string();
        let mut grants = DesktopControlGrants::default();
        assert!(desktop_control_tabs(&tree, &grants).is_empty());

        grants.allow_tab(TabGrant {
            holder: me(),
            package: "so.tilo.terminal".into(),
            room: room.clone(),
            tabs: vec![asked],
            label: "claude".into(),
        });
        assert_eq!(
            desktop_control_tabs(&tree, &grants),
            HashSet::from([asked]),
            "the tab that asked, alone"
        );
        assert_eq!(
            granted(&tree, &grants, asked),
            Some(("so.tilo.terminal".into(), vec![Granted::UntilTabCloses]))
        );
        assert_eq!(granted(&tree, &grants, other), None);

        assert!(allow_always(&mut tree, &room, "so.tilo.terminal"));
        let terminals: HashSet<TabId> = tree.desktop.workspaces[0]
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .filter(|tab| {
                matches!(
                    tab.tile,
                    TileContent::AppPlaceholder {
                        kind: AppKind::Terminal,
                        ..
                    }
                )
            })
            .map(|tab| tab.id)
            .collect();
        assert!(terminals.contains(&asked) && terminals.contains(&other));
        assert!(!terminals.contains(&notes));
        assert_eq!(
            desktop_control_tabs(&tree, &grants),
            terminals,
            "always: every Terminal tab of the workspace, and no other window"
        );
        assert_eq!(
            granted(&tree, &grants, other),
            Some(("so.tilo.terminal".into(), vec![Granted::Always]))
        );
        let said = captions(
            "so.tilo.terminal",
            &[Granted::UntilTabCloses, Granted::Always],
        )
        .join(" ");
        assert!(said.contains("Until the tab that asked closes"), "{said}");
        assert!(said.contains("Always in this workspace"), "{said}");

        assert!(grants.revoke_on_tab(asked));
        assert!(revoke_always(&mut tree, &room, "so.tilo.terminal"));
        assert!(desktop_control_tabs(&tree, &grants).is_empty());
    }
}
