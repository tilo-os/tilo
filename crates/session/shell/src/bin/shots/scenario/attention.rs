//! Windows an agent opens aside: the attention mark on their tab, strip and
//! workspace, and looking at one clears it.

use shell::shell::chrome_state::ChromeState;
use shell::shell::state::WorkspaceTree;
use vello::kurbo::Point;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::super::driver::ShellDriver;
use super::{Action, Scenario};

/// The person goes to the window of client surface `surface`, as choosing it
/// in search does.
pub(super) fn focus_surface(driver: &mut ShellDriver, surface: u64) {
    driver.with_chrome(|st: &mut ChromeState| {
        let address = {
            let tree = st.tree();
            tree.desktop.workspaces.iter().find_map(|workspace| {
                workspace.strips.iter().find_map(|strip| {
                    strip.window_groups.iter().find_map(|group| {
                        group.tabs.iter().find_map(|tab| {
                            matches!(
                                tab.tile,
                                shell::shell::state::TileContent::Foreign { surface_id, .. }
                                    if surface_id == surface
                            )
                            .then_some((
                                workspace.id,
                                strip.id,
                                group.id,
                                tab.id,
                            ))
                        })
                    })
                })
            })
        };
        if let Some((workspace, strip, group, tab)) = address {
            st.select_tab(workspace, strip, group, tab);
        }
    });
}

/// Place the next window of `app_id` beside the first window of strip
/// `strip` of the active workspace, as the broker places an open nobody
/// pressed for.
pub(super) fn place_aside(app_id: &str, tree: &WorkspaceTree, strip: usize) {
    if let Some(group) = tree
        .desktop
        .active_workspace()
        .and_then(|workspace| workspace.strips.get(strip))
        .and_then(|strip| strip.window_groups.first())
    {
        shell::shell::launcher::set_pending_placement(
            app_id,
            shell::compositor::tab_binding::Placement::Beside(group.id),
        );
    }
}

/// Two windows open aside, one beside the person's window and one on another
/// strip; the switcher and the workspace menu carry the dot; looking clears it.
pub(super) fn attention_mark() -> Scenario {
    Scenario {
        name: "attention_mark",
        description: "Windows an agent opens aside never take the person's view: one \
         beside the window they are in carries the attention keyline on its tab, one on \
         the other strip puts the attention dot on that strip in the ribbon and the \
         switcher, and the workspace menu dots the workspace holding them. Looking at a \
         window clears its mark.",
        actions: vec![
            Action::SeedStripsWithPeople,
            Action::FocusStrip(0),
            Action::OpenWindow {
                app_id: "org.example.writer",
                surface_id: 6101,
            },
            Action::Wait(20),
            Action::OpenWindowAside {
                app_id: "org.example.report",
                surface_id: 6102,
                strip: 0,
            },
            Action::OpenWindowAside {
                app_id: "org.example.chart",
                surface_id: 6103,
                strip: 2,
            },
            Action::Wait(20),
            Action::MoveCursor(Point::new(800.0, 500.0)),
            Action::WaitWall(1700),
            Action::Snapshot("00-tab-mark-and-ribbon-dot"),
            Action::Modifiers(ModifiersState::SUPER),
            Action::Key(Key::Character("s".into())),
            Action::Modifiers(ModifiersState::empty()),
            Action::Wait(10),
            Action::Snapshot("01-switcher-dot"),
            Action::Key(Key::Named(NamedKey::Escape)),
            Action::Wait(4),
            Action::ClickNamed("topbar_workspace_pill"),
            Action::Wait(6),
            Action::Snapshot("02-workspace-menu-dot"),
            Action::Key(Key::Named(NamedKey::Escape)),
            Action::Wait(4),
            Action::FocusSurface(6102),
            Action::Wait(24),
            Action::Snapshot("03-looked-at-clears-the-mark"),
        ],
    }
}
