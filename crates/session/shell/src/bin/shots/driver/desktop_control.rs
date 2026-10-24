//! A terminal tab controlling the desktop: the person asked, the tab's
//! "Controls your desktop" chip, and the chip's menu.

use super::ShellDriver;
use shell::shell::desktop_control::{DesktopControlAsk, Holder, TabGrant};

/// The tab the scenes stand for: this process, which lives as long as the
/// capture does.
fn this_tab() -> Holder {
    let pid = std::process::id();
    let start = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            let rest = stat.rsplit_once(") ")?.1.to_owned();
            rest.split_whitespace().nth(19)?.parse().ok()
        })
        .unwrap_or_default();
    Holder { pid, start }
}

impl ShellDriver {
    /// A Terminal tab titled as a person's agent tab is, in front of the
    /// active workspace's first window: its id and its workspace's room.
    fn terminal_tab_in_front(&mut self) -> Option<(u64, String)> {
        use shell::shell::state::{AppKind, Tab, TileContent};
        // Far above the bootstrap range, so it never collides.
        const TERMINAL_TAB: u64 = 9201;
        let mut found = None;
        self.with_chrome(|st| {
            let mut tree = st.tree_mut();
            let Some(workspace) = tree.desktop.active_workspace_mut() else {
                return;
            };
            let room = workspace.room().to_string();
            let Some(group) = workspace
                .strips
                .first_mut()
                .and_then(|strip| strip.window_groups.first_mut())
            else {
                return;
            };
            if !group.tabs.iter().any(|tab| tab.id == TERMINAL_TAB) {
                group.tabs.push(Tab {
                    id: TERMINAL_TAB,
                    title: "claude".into(),
                    tile: TileContent::AppPlaceholder {
                        kind: AppKind::Terminal,
                        label: "Terminal".into(),
                    },
                    app_title: None,
                });
            }
            group.active_tab = TERMINAL_TAB;
            found = Some((TERMINAL_TAB, room));
            drop(tree);
            st.bump();
        });
        self.step_frames(4);
        found
    }

    /// "Let this terminal control your desktop?", asked in trusted chrome
    /// when a program typed in a Terminal tab first reaches beyond its
    /// workspace.
    pub(super) fn scene_desktop_control_ask(&mut self) {
        let (tab, room) = self.terminal_tab_in_front().unwrap_or_default();
        let (reply, answer) = tokio::sync::oneshot::channel();
        // Held: a prompt whose requester went away is withdrawn.
        self.held.push(Box::new(answer));
        self.with_chrome(|st| {
            st.pending_grants.push_back(
                shell::shell::chrome_state::PendingPrompt::desktop_control(
                    DesktopControlAsk {
                        package: "so.tilo.terminal".into(),
                        app_label: "Terminal".into(),
                        room,
                        action: "Show the overview".into(),
                        sensitive: false,
                        holder: this_tab(),
                        tabs: vec![tab],
                    },
                    reply,
                ),
            );
            st.bump();
        });
        self.host_mut().pump_permission_prompt();
        self.step_frames(8);
    }

    /// The Terminal tab the person let control the desktop, for this tab:
    /// its bar says so.
    pub(super) fn scene_desktop_control_chip(&mut self) {
        let Some((tab, room)) = self.terminal_tab_in_front() else {
            return;
        };
        self.with_chrome(|st| {
            st.desktop_control.allow_tab(TabGrant {
                holder: this_tab(),
                package: "so.tilo.terminal".into(),
                room,
                tabs: vec![tab],
                label: "claude".into(),
            });
            st.bump();
        });
        // The person is typing in the tab, not pointing at its bar; the bar
        // settles its new tab before the picture is taken.
        self.move_cursor(vello::kurbo::Point::new(800.0, 600.0));
        self.step_frames(120);
        let _ = self.paint_settled();
    }

    /// The chip's menu: what the tab may do, for how long, and Stop.
    pub(super) fn scene_desktop_control_menu(&mut self) {
        self.scene_desktop_control_chip();
        let _ = self.paint();
        self.host_mut().open_desktop_control_menu();
        self.step_frames(8);
    }
}
