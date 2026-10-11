//! The shell's side of the shared workspace overview
//! ([`tilo_desktop::overview`]): who sits at this device, the icon theme's
//! artwork, and how workspace settings name what a share covers.

use tilo_desktop::overview::{OverviewHost, RoomCard};
use tilo_desktop::window_switcher::WindowSource;

use crate::shell::state::{Scope, TileContent, WorkspaceTree};
use crate::shell::window_previews::ShellWindows;

/// The overview as the person `local_user` at this device sees it.
pub struct ShellOverview<'a> {
    local_user: &'a str,
}

impl<'a> ShellOverview<'a> {
    pub fn for_user(local_user: &'a str) -> Self {
        Self { local_user }
    }
}

impl WindowSource for ShellOverview<'_> {
    fn app_kind(&self, tile: &TileContent) -> ui::widget::AppKind {
        ShellWindows.app_kind(tile)
    }

    fn app_icon(&self, tile: &TileContent) -> Option<ui::icons::AppImage> {
        ShellWindows.app_icon(tile)
    }
}

impl OverviewHost for ShellOverview<'_> {
    fn local_user(&self) -> &str {
        self.local_user
    }

    fn scope_label(&self, scope: &Scope) -> String {
        tilo_desktop::workspace_settings::scope_label(scope, &crate::shell::launcher::ShellApps)
    }

    fn node_label(&self, node: &tilo_action_policy::TreePath) -> String {
        tilo_desktop::workspace_settings::node_phrase(node, &crate::shell::launcher::ShellApps)
    }
}

/// Every room as the overview shows it to `local_user`, now.
pub fn room_cards(tree: &WorkspaceTree, local_user: &str) -> Vec<RoomCard> {
    tilo_desktop::overview::room_cards(
        &tree.desktop,
        &ShellOverview::for_user(local_user),
        tilo_clock::unix_now(),
    )
}
