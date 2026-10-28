//! Searching for a Linux app that already has a window open: the app is
//! still there to start another one, beside its window.
use super::super::ShellDriver;

impl ShellDriver {
    /// The Terminal tab is an Alacritty window in `~/src`; the search is
    /// for Alacritty. The app row comes from this machine's own Linux apps.
    pub(in crate::driver) fn scene_palette_open_app(&mut self) {
        use shell::shell::state::TileContent;
        self.with_chrome(|chrome| {
            let mut tree = chrome.tree_mut();
            for group in tree
                .desktop
                .workspaces
                .iter_mut()
                .flat_map(|workspace| &mut workspace.strips)
                .flat_map(|strip| &mut strip.window_groups)
            {
                for tab in group.tabs.iter_mut().filter(|tab| tab.title == "Terminal") {
                    tab.title = "~/src".into();
                    tab.tile = TileContent::Foreign {
                        app_id: "Alacritty".into(),
                        surface_id: tab.id,
                    };
                }
            }
        });
        self.relayout();
        self.scene_seeded_query("alacritty");
    }
}
