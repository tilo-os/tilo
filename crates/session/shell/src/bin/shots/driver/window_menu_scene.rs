//! The window menu, opened the way a keyboard opens it: the window-menu
//! shortcut, at the focused window's tab.
use winit::keyboard::{Key, ModifiersState, NamedKey};

use ui::Widget as _;

use super::ShellDriver;

impl ShellDriver {
    pub(super) fn scene_window_menu(&mut self) {
        // The menu opens at the focused tab's pill, which a paint lays out.
        let _ = self.paint_settled();
        self.set_modifiers(ModifiersState::SUPER);
        self.press_key(&Key::Named(NamedKey::F10));
        self.set_modifiers(ModifiersState::empty());
        assert!(
            self.host().tab_context_menu_is_open(),
            "window-menu scene: the shortcut opened no window menu"
        );
        // Past the menu's entrance.
        self.step_frames(20);
    }
}

impl ShellDriver {
    /// A terminal window from outside Tilo being named in place: the window
    /// menu's Rename tab, reached by the keyboard, opens the name field in
    /// the tab's own pill holding its title, selected.
    pub(super) fn scene_tab_rename(&mut self) {
        const SURFACE: u64 = 0x7e57;
        self.with_chrome(|st| {
            let mut tree = st.tree_mut();
            let tab = shell::shell::tab_binding::adopt_toplevel(
                &mut tree,
                SURFACE,
                Some("Alacritty".into()),
            );
            let _ = shell::shell::tab_binding::rename_tab_for_title(
                &mut tree,
                SURFACE,
                "~/src/tilo: cargo build",
            );
            let placed = tree.desktop.active_workspace().and_then(|ws| {
                ws.strips.iter().find_map(|strip| {
                    strip
                        .window_groups
                        .iter()
                        .find(|group| group.tabs.iter().any(|t| t.id == tab))
                        .map(|group| (strip.id, group.id))
                })
            });
            if let Some((strip, group)) = placed {
                tree.desktop.set_active_group(strip, group);
            }
            drop(tree);
            st.bump();
        });
        // The new window arrives and settles before its menu opens at it.
        let _ = self.paint_settled();
        std::thread::sleep(std::time::Duration::from_millis(600));
        let _ = self.paint_settled();
        self.set_modifiers(ModifiersState::SUPER);
        self.press_key(&Key::Named(NamedKey::F10));
        self.set_modifiers(ModifiersState::empty());
        assert!(
            self.host().tab_context_menu_is_open(),
            "tab-rename scene: the shortcut opened no window menu"
        );
        let armed = |driver: &Self| {
            driver
                .host()
                .accessibility()
                .and_then(|root| {
                    root.children
                        .into_iter()
                        .find(|node| node.role == ui::a11y::Role::Menu)
                })
                .and_then(|menu| {
                    menu.children
                        .into_iter()
                        .find(|node| node.state.focused)
                        .and_then(|node| node.name)
                })
        };
        for _ in 0..24 {
            if armed(self).as_deref() == Some("Rename tab") {
                break;
            }
            self.press_key(&Key::Named(NamedKey::ArrowDown));
        }
        assert_eq!(
            armed(self).as_deref(),
            Some("Rename tab"),
            "tab-rename scene: the window menu offers Rename tab"
        );
        self.press_key(&Key::Named(NamedKey::Enter));
        assert!(
            self.host().tab_renaming(),
            "tab-rename scene: Rename tab opened no name field"
        );
        self.step_frames(20);
    }
}
