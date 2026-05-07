//! Adopt foreign Wayland clients into the active workspace's tab tree.
//!
//! Phase 6 (Task 6.1 + 6.2) of
//! `docs/superpowers/plans/2026-05-02-real-wayland-compositor.md`.
//!
//! When a client commits its first `xdg_toplevel`, the compositor needs to
//! splice a new tab into the workspace tree so the chrome paints a tab pill
//! and the frame loop knows which surface to blit into the tile rect. The
//! reverse path — destroyed surface → tab close — also lives here.
//!
//! The two public entry points,
//! [`adopt_toplevel`] and [`close_tab_for_destroyed_surface`], take a
//! `surface_id: u64` (the wayland-server `WlSurface::id().protocol_id()`
//! widened) rather than an `&WlSurface`. That's a deliberate testability
//! refactor: smithay's `WlSurface` cannot be constructed without a running
//! `Display` + client, so unit tests would otherwise need a full server
//! runtime. The compositor's `XdgShellHandler` impl performs the
//! `WlSurface → u64` conversion at the call-site (see
//! `compositor::state::State::new_toplevel`).

use crate::shell::state::{
    Tab, TabId, TileContent, WindowGroup, WindowWidth, WorkspaceTree, next_id,
};

/// Adopt a freshly-committed wayland toplevel into the active workspace's
/// tab tree.
///
/// `surface_id` is the wayland-server protocol id of the `wl_surface`
/// (widened to `u64`). `app_id` may be `None` if the client hasn't called
/// `set_app_id` yet — in that case a synthetic placeholder app-id of the
/// form `anon-<surface_id>` is recorded; a future Task 6.2 step rewrites it
/// when the client eventually sets one.
///
/// Returns the freshly-minted tab id. If the active workspace exists but
/// has no strips, the tab can't be parked anywhere and the function
/// returns the synthesized id without mutating the tree (the compositor
/// can drop the surface). If the strip exists but has no window-group,
/// one is synthesized — mirrors `apply_workspace_drop`'s empty-strip
/// path.
///
/// Phase 6 doesn't yet wire the `surface_id → WlSurface` reverse map; the
/// compositor state holds that map alongside the tree (Phase 4
/// follow-up will use it to actually blit foreign buffers into the tile
/// rect).
pub fn adopt_toplevel(
    tree: &mut WorkspaceTree,
    surface_id: u64,
    app_id: Option<String>,
) -> TabId {
    let resolved_id = app_id.unwrap_or_else(|| format!("anon-{}", surface_id));
    let new_tab = Tab {
        id: next_id(),
        title: tab_label_for_foreign(&resolved_id),
        tile: TileContent::Foreign {
            app_id: resolved_id,
            surface_id,
        },
    };
    let tab_id = new_tab.id;
    if let Some(ws) = tree.active_workspace_mut() {
        if let Some(strip) = ws.strips.first_mut() {
            if let Some(group) = strip.window_groups.first_mut() {
                group.tabs.push(new_tab);
                group.active_tab = tab_id;
            } else {
                // Empty strip: synthesize a window-group so the new tab
                // has a parent. Mirrors apply_workspace_drop's empty-strip
                // path (`shell/workspace_drop.rs`).
                strip.window_groups.push(WindowGroup {
                    id: next_id(),
                    tabs: vec![new_tab],
                    active_tab: tab_id,
                    width: WindowWidth::Normal,
                    maximized: false,
                });
            }
        }
        // No strips at all: the tab is dropped on the floor (the active
        // workspace can't host it). The synthesized tab_id still flows
        // back so the caller can correlate the surface with a no-op
        // adoption result.
    }
    tab_id
}

/// Walk the persisted tree looking for a `TileContent::Foreign` tab whose
/// `surface_id` matches `destroyed_surface_id`; remove it from its group
/// when found and return `true`. Phase 6 (Task 6.2) of the compositor
/// plan: surface destroy → tab close.
///
/// Returns `false` when no matching tab exists (e.g. the surface was
/// never adopted, or the tab was already removed via another path).
/// The widget tree handles close animation; the data layer just records
/// absence.
pub fn close_tab_for_destroyed_surface(
    tree: &mut WorkspaceTree,
    destroyed_surface_id: u64,
) -> bool {
    for ws in tree.workspaces.iter_mut() {
        for strip in ws.strips.iter_mut() {
            for group in strip.window_groups.iter_mut() {
                if let Some(pos) = group.tabs.iter().position(|t| {
                    matches!(
                        &t.tile,
                        TileContent::Foreign { surface_id: s, .. }
                            if *s == destroyed_surface_id
                    )
                }) {
                    let removed = group.tabs.remove(pos);
                    // If the removed tab was the active one, fall back to
                    // the new last tab (or leave `active_tab` pointing at
                    // a stale id — the chrome's resolver tolerates that
                    // and picks the first existing tab on next frame).
                    if group.active_tab == removed.id {
                        group.active_tab = group
                            .tabs
                            .last()
                            .map(|t| t.id)
                            .unwrap_or(0);
                    }
                    return true;
                }
            }
        }
    }
    false
}

/// Truncate a wayland app-id down to a tab-bar friendly label. The
/// concept of "trailing component" mirrors what most apps actually want
/// to show: `org.example.MyApp` → `MyApp`.
fn tab_label_for_foreign(app_id: &str) -> String {
    let trimmed = app_id.split('.').next_back().unwrap_or(app_id);
    if trimmed.len() > 32 {
        trimmed[..32].to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::WorkspaceTree;

    #[test]
    fn adopt_toplevel_appends_a_foreign_tab_to_active_group() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let initial_count = tree.workspaces[0].strips[0].window_groups[0]
            .tabs
            .len();
        let tab_id =
            adopt_toplevel(&mut tree, 4242, Some("org.example.app".into()));
        let group = &tree.workspaces[0].strips[0].window_groups[0];
        assert_eq!(
            group.tabs.len(),
            initial_count + 1,
            "expected adoption to grow the tab list by one"
        );
        let last = group.tabs.last().unwrap();
        assert_eq!(last.id, tab_id, "returned tab_id matches the appended tab");
        assert_eq!(group.active_tab, tab_id, "newly-adopted tab gains focus");
        match &last.tile {
            TileContent::Foreign { app_id, surface_id } => {
                assert_eq!(app_id, "org.example.app");
                assert_eq!(*surface_id, 4242);
            }
            other => panic!("expected Foreign tile, got {:?}", other),
        }
    }

    #[test]
    fn close_tab_for_destroyed_surface_removes_matching_foreign_tab() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let _ = adopt_toplevel(&mut tree, 42, Some("io.example.thing".into()));
        let before = tree.workspaces[0].strips[0].window_groups[0].tabs.len();
        let removed = close_tab_for_destroyed_surface(&mut tree, 42);
        assert!(removed, "close should report it removed a tab");
        let after = tree.workspaces[0].strips[0].window_groups[0].tabs.len();
        assert_eq!(after, before - 1, "tab count must drop by exactly one");
        let still_there = tree.workspaces[0].strips[0].window_groups[0]
            .tabs
            .iter()
            .any(|t| {
                matches!(&t.tile, TileContent::Foreign { surface_id: 42, .. })
            });
        assert!(!still_there, "the foreign tab with surface_id=42 must be gone");
    }

    #[test]
    fn anonymous_app_id_falls_back_to_surface_synthetic() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let _ = adopt_toplevel(&mut tree, 7, None);
        let tab = tree.workspaces[0].strips[0].window_groups[0]
            .tabs
            .last()
            .unwrap();
        match &tab.tile {
            TileContent::Foreign { app_id, surface_id } => {
                assert_eq!(*surface_id, 7);
                assert_eq!(
                    app_id, "anon-7",
                    "anonymous toplevels get a synthetic anon-<surface_id> placeholder"
                );
            }
            other => panic!("expected Foreign tile, got {:?}", other),
        }
    }
}
