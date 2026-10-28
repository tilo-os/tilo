//! Two half-width windows with focus on the right-hand one.
use super::ShellDriver;

impl ShellDriver {
    /// Two half-width windows, focus on the right-hand one — nothing about
    /// the layout alone says which takes the next keystroke.
    pub(super) fn scene_two_groups_focus(&mut self) {
        use shell::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};
        let half = WindowWidth::Fraction(500);
        let mut placed = None;
        self.with_chrome(|st| {
            let mut tree = st.tree_mut();
            let mut sibling = None;
            let mut strip_id = None;
            if let Some(strip) = tree
                .desktop
                .active_workspace_mut()
                .and_then(|ws| ws.strips.first_mut())
            {
                for g in &mut strip.window_groups {
                    g.width = half;
                }
                let gid = 9101u64;
                let tid = 9111u64;
                strip.window_groups.push(WindowGroup {
                    id: gid,
                    tabs: vec![Tab {
                        id: tid,
                        title: "Settings".to_string(),
                        tile: TileContent::Stub {
                            kind: shell::shell::state::AppKind::Settings,
                            label: "Settings".to_string(),
                        },
                        app_title: None,
                    }],
                    active_tab: tid,
                    width: half,
                    maximized: false,
                    focus_history: Vec::new(),
                });
                strip_id = Some(strip.id);
                sibling = Some(gid);
            }
            // Focus the newly-added right-hand window, so the
            // scene shows the signal moving off the default.
            if let (Some(sid), Some(gid)) = (strip_id, sibling) {
                tree.desktop.set_active_group(sid, gid);
                placed = Some(sid);
            }
            drop(tree);
            st.bump();
        });
        // Widths only reach the widget tree at construction, which
        // ran before this; halve the live groups too or the scene
        // renders one full-width window with its sibling off-edge.
        if let Some(sid) = placed
            && let Some(row) = self.host_mut().group_row_mut_for_tests(sid)
        {
            for g in &mut row.groups {
                g.set_width(half);
            }
        }
        // The pushed group latches its 420 ms arrival fade off the
        // wall clock the moment it's spliced in; paint once so it
        // becomes a widget, then wait the curve out before the
        // caller's `paint_settled` captures the resting state.
        let _ = self.paint_settled();
        std::thread::sleep(std::time::Duration::from_millis(600));
    }
}
