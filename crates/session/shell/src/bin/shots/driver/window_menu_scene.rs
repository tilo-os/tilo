//! The window menu, opened the way a keyboard opens it: the window-menu
//! shortcut, at the focused window's tab.
use winit::keyboard::{Key, ModifiersState, NamedKey};

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
