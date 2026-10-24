//! Desktop and shell chords, Escape and the workspace rename field.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    pub(super) fn handle_desktop_chord(
        &mut self,
        action: settings_store::hardware::ShortcutAction,
        repeat: bool,
        source: ui::tokens::MotionSource,
    ) -> bool {
        use settings_store::hardware::ShortcutAction as A;
        if let Some(index) = action.focus_group_index() {
            if !repeat {
                self.begin_motion(source);
                let _ = self.focus_nth_group_in_strip(index);
            }
            return true;
        }
        if let Some(index) = action.focus_strip_index() {
            if !repeat && let Some(chrome) = self.chrome.clone() {
                let _ = chrome.borrow_mut().focus_strip_index(index);
            }
            return true;
        }
        if let Some(step) = magnifier_step(action) {
            // Zooming is a step, so a held zoom key keeps zooming; the toggle is an edge.
            if !(repeat && step == MagnifierStep::Toggle) {
                self.step_magnifier(step);
            }
            return true;
        }
        if matches!(
            action,
            A::DictationToggle | A::DictationFinish | A::DictationCancel
        ) {
            if !repeat {
                match action {
                    A::DictationToggle => self.layers.dictation.request_toggle(),
                    A::DictationFinish => self.layers.dictation.request_finish(),
                    A::DictationCancel => self.layers.dictation.request_cancel(),
                    _ => {}
                }
                self.bump_chrome();
            }
            return true;
        }
        if action == A::TabMenu {
            if !repeat {
                let _ = self.open_focused_tab_context_menu();
            }
            return true;
        }
        if action == A::EmojiPicker {
            if !repeat {
                self.toggle_emoji_picker();
            }
            return true;
        }
        if action == A::AskAgent {
            return self.press_ask_agent_chord(repeat);
        }
        if !matches!(
            action,
            A::OpenFiles
                | A::OpenTerminal
                | A::ClipboardHistory
                | A::Notifications
                | A::ToggleFullscreen
                | A::PowerMenu
                | A::NextInputSource
        ) {
            return false;
        }
        if repeat {
            return true;
        }
        if let Some(chrome) = self.chrome.clone() {
            self.run_chrome_chord(&chrome, action);
        }
        true
    }

    /// The desktop chords that act on the chrome: open Files or a terminal,
    /// the clipboard history, notifications, fullscreen, the next input
    /// source and the power menu.
    fn run_chrome_chord(
        &mut self,
        chrome: &Chrome,
        action: settings_store::hardware::ShortcutAction,
    ) {
        use crate::shell::quick_settings::QsLevel;
        use settings_store::hardware::ShortcutAction as A;
        match action {
            A::OpenFiles => (self.app_launcher)(chrome, FILES_PACKAGE.0, FILES_PACKAGE.1),
            A::OpenTerminal => {
                let _ = crate::shell::window_actions::open_terminal(
                    &mut chrome.borrow_mut(),
                    crate::shell::window_actions::Destination::Beside,
                );
            }
            A::ClipboardHistory => {
                let origin = crate::shell::chrome_state::PasteOrigin {
                    caret: self.text_caret,
                    window: self.focused_window_rect(),
                };
                chrome.borrow_mut().open_clipboard_history_to_paste(origin);
            }
            A::Notifications => chrome
                .borrow_mut()
                .open_quick_settings_level(QsLevel::Notifications),
            A::ToggleFullscreen => {
                // Super+M applies on the host. Super+F must too: the
                // compositor loop then writes `set_fullscreen(None)` every
                // tick (client xdg is overlay-only), so a later request
                // parked in `fullscreen_group` is gone before W07 dumps.
                if !self.toggle_focused_group_fullscreen() {
                    chrome.borrow_mut().request_fullscreen_toggle();
                }
            }
            A::NextInputSource => {
                if let Some((index, name)) = select_next_input_source(&self.keyboard_scope) {
                    let mut st = chrome.borrow_mut();
                    st.request_input_source(index);
                    st.show_osd(crate::shell::osd::Osd::input_source(&name));
                }
            }
            A::PowerMenu => {
                self.layers.power_menu.open();
                let mut st = chrome.borrow_mut();
                st.overlays.quick_settings = false;
                st.bump();
            }
            _ => {}
        }
    }

    /// Focus the `index`th window group of the active strip, on its active
    /// tab. `false` when the strip has fewer groups.
    pub(super) fn focus_nth_group_in_strip(&mut self, index: usize) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let target = (|| -> Option<(StripId, GroupId, u64)> {
            let st = chrome.borrow();
            let tree = st.tree();
            let strip = tree
                .desktop
                .active_strip
                .get(&tree.desktop.active)
                .copied()?;
            let group = tree
                .desktop
                .active_workspace()?
                .strips
                .iter()
                .find(|candidate| candidate.id == strip)?
                .window_groups
                .get(index)
                .map(|group| (group.id, group.active_tab));
            drop(tree);
            let (group, tab) = group?;
            Some((strip, group, tab))
        })();
        let Some((strip, group, tab)) = target else {
            return false;
        };
        self.focus_window_on_strip(strip, group, tab)
    }

    /// Immutable Super-key shell chords: projection/capture actions,
    /// workspace history, and window creation. Configurable actions are
    /// resolved before this function from `ShortcutConfig`.
    pub(super) fn handle_shell_mod_chords(
        &mut self,
        logical_key: &Key,
        super_down: bool,
        shift_down: bool,
        repeat: bool,
        state: &AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) -> bool {
        // Workspace history is an OS surface, not browser navigation history,
        // so keep Ctrl+H available to the focused app and reserve Super+H.
        if super_down
            && let Key::Character(s) = logical_key
            && s.eq_ignore_ascii_case("h")
            && let Some(chrome) = self.chrome.clone()
        {
            if repeat {
                *handled = true;
                return true;
            }
            if shift_down {
                self.room_activity
                    .set_view(crate::shell::room_activity::ActivityView::Explore);
            } else {
                self.room_activity
                    .set_view(crate::shell::room_activity::ActivityView::CatchUp);
            }
            let mut st = chrome.borrow_mut();
            st.open_room_activity(None);
            *handled = true;
            return true;
        }
        // Super+Shift+P is the fast path into live PiP selection. It must be
        // checked before familiar Win+P so the shifted chord cannot briefly
        // flash or toggle the projection sheet underneath the picker.
        if super_down
            && shift_down
            && let Key::Character(s) = logical_key
            && s.eq_ignore_ascii_case("p")
        {
            if !repeat {
                let _ = self.open_capture_studio(ui::widget::CaptureMode::Pip);
            }
            *handled = true;
            return true;
        }
        // Win+P — the display layout sheet. Super specifically, so an
        // application's Ctrl+P remains Print. Once open, the sheet owns the
        // chord itself: pressing it again moves to the next layout.
        if super_down
            && !shift_down
            && let Key::Character(s) = logical_key
            && s.eq_ignore_ascii_case("p")
            && let Some(chrome) = self.chrome.clone()
        {
            if !repeat {
                let mut st = chrome.borrow_mut();
                st.open_projection();
                st.bump();
            }
            *handled = true;
            return true;
        }
        if super_down
            && shift_down
            && let Key::Character(s) = logical_key
            && s.eq_ignore_ascii_case("r")
        {
            if !repeat && self.capture_studio.is_replay_active() {
                // A warming buffer deliberately accepts no speculative save;
                // the chord is still shell-owned and the pill keeps saying
                // Buffering until a decodable tail exists.
                let _ = self.capture_studio.save_recent_replay();
            } else if !repeat && !self.capture_studio.is_recording_active() {
                let _ = self.open_capture_studio(ui::widget::CaptureMode::Recording);
            }
            *handled = true;
            return true;
        }
        if super_down
            && shift_down
            && let Key::Character(s) = logical_key
            && s.eq_ignore_ascii_case("s")
        {
            if !repeat {
                let _ = self.open_capture_studio(ui::widget::CaptureMode::Screenshot);
            }
            *handled = true;
            return true;
        }
        // Window-creation chords are Super-only (grabbed wholesale,
        // taking nothing from a focused client), not the
        // Ctrl+Shift+letter chords browsers/terminals define.
        // Super+Return is the recovery path when the last shell is
        // gone, so it must not depend on search.
        if super_down
            && matches!(logical_key, Key::Named(NamedKey::Enter))
            && let Some(chrome) = self.chrome.clone()
        {
            if !repeat {
                let mut st = chrome.borrow_mut();
                crate::shell::window_actions::open_terminal(
                    &mut st,
                    crate::shell::window_actions::Destination::Beside,
                );
            }
            *handled = true;
            return true;
        }
        if super_down
            && let Key::Character(s) = logical_key
            && self.handle_shell_window_open_chords(
                s.as_str(),
                super_down,
                repeat,
                state,
                path,
                handled,
            )
        {
            return true;
        }
        false
    }

    /// Super+letter chords that open or close a window: `T` (a new tab of
    /// the focused app), `N` (new window), `W` (close focused tab), `S`
    /// (summon the workspace wheel to the pointer). Split out of `handle_shell_mod_chords`,
    /// which was otherwise the single largest chord block.
    pub(super) fn handle_shell_window_open_chords(
        &mut self,
        s: &str,
        super_down: bool,
        repeat: bool,
        state: &AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) -> bool {
        match s {
            // Super+T — a new tab of the app you are looking at, in every
            // app: the system's chord, so it works where the app keeps
            // Ctrl+T for itself (a browser, a terminal). With no window
            // yet, it opens the app picker instead.
            "t" | "T" => {
                if repeat {
                    *handled = true;
                    return true;
                }
                let Some(chrome) = self.chrome.clone() else {
                    return false;
                };
                let focused = crate::shell::window_actions::focused_group(&chrome.borrow());
                if focused.is_none() {
                    self.open_app_launcher(state, path);
                    *handled = true;
                    return true;
                }
                let mut st = chrome.borrow_mut();
                if let Err(err) = crate::shell::window_actions::duplicate_active_tab(&mut st) {
                    st.push_warning_notification(
                        t("host-tab-duplicate-failed"),
                        err.user_message(),
                    );
                }
                *handled = true;
                true
            }
            // Super+N — a new window, placed by the rule.
            "n" | "N" => {
                if !repeat {
                    self.open_app_launcher(state, path);
                }
                *handled = true;
                true
            }
            // Chord form of the pill's `✕`. Super, not
            // Ctrl+W — the latter is already bound in
            // most browsers/terminals/editors, and the
            // shell would swallow it ahead of the client.
            "w" | "W" => {
                if repeat || self.close_focused_tab() {
                    *handled = true;
                    true
                } else {
                    false
                }
            }
            // Opens the strip switcher for the keyboard, or closes it.
            // Super specifically, not `is_mod`: Ctrl+S is save in nearly
            // every app.
            "s" | "S" if super_down => {
                if !repeat {
                    self.toggle_strip_switcher_from_keyboard();
                }
                *handled = true;
                true
            }
            _ => false,
        }
    }

    /// The Escape dismissal ladder. Priority matches z-order: palette >
    /// overview > ws settings > quick settings.
    pub(super) fn finish_notification_reading(
        &mut self,
        st: &mut crate::shell::chrome_state::ChromeState,
    ) -> bool {
        if st.overlays.quick_settings
            && matches!(
                st.quick_settings.current_level(),
                crate::shell::quick_settings::QsLevel::Notifications
            )
            && self.notification_center.close_reader()
        {
            st.bump();
            true
        } else {
            false
        }
    }

    /// Escape in the open search: step back out of an action's steps,
    /// then leave a resource scope, then clear what was typed, and only on
    /// an empty field close, so a slip of the finger never costs the search.
    pub(super) fn palette_escape(&mut self) {
        if self.param_flow.is_some() || self.secondary_flow.is_some() {
            self.palette_step_back();
            return;
        }
        if let Some(chrome) = self.chrome.clone() {
            let mut state = chrome.borrow_mut();
            if state.search_resource_scope.take().is_some() {
                state.bump();
                drop(state);
                self.palette.set_query("");
                self.palette
                    .set_query_placeholder(t("host-search-placeholder"));
                self.last_search_query = "\0resource-scope-back\0".into();
                return;
            }
        }
        if !self.palette.query().is_empty() {
            self.palette.set_query("");
            self.palette_layout_dirty = true;
            return;
        }
        self.palette_visible = false;
    }

    pub(super) fn handle_shell_escape_chord(&mut self, handled: &mut bool) -> bool {
        if self.cancel_tab_drags() {
            *handled = true;
            return true;
        }
        if self.palette_visible {
            self.palette_escape();
            *handled = true;
            return true;
        }
        if self.dismiss_permission_prompt() {
            *handled = true;
            return true;
        }
        if self.layers.dismiss_topmost() {
            *handled = true;
            return true;
        }
        // Most transient thing on screen, so goes first.
        if self.strip_navigator.is_open() {
            self.strip_navigator.close();
            self.publish_strip_switcher_open();
            *handled = true;
            return true;
        }
        // A strip's name being typed in the overview: Escape keeps the old
        // name and leaves the overview open.
        if self.overview_naming.take().is_some() {
            self.bump_chrome();
            *handled = true;
            return true;
        }
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let mut st = chrome.borrow_mut();
        if st.editing_workspace_name.is_some() {
            st.editing_workspace_name = None;
            st.bump();
            *handled = true;
            return true;
        }
        if st.overlays.overview {
            st.overlays.overview = false;
            st.bump();
            drop(st);
            self.latch_keyboard_overview();
            *handled = true;
            return true;
        }
        if st.overlays.ws_settings {
            st.overlays.ws_settings = false;
            st.bump();
            *handled = true;
            return true;
        }
        if st.overlays.projection {
            st.close_projection();
            st.bump();
            *handled = true;
            return true;
        }
        if st.overlays.collaborators {
            st.overlays.collaborators = false;
            st.bump();
            *handled = true;
            return true;
        }
        if self.finish_notification_reading(&mut st) {
            *handled = true;
            return true;
        }
        if st.overlays.quick_settings {
            // A drill-in is a nested page, so Escape follows its visible Back
            // affordance before it dismisses the whole popover. This keeps
            // keyboard navigation consistent with pointer navigation and
            // prevents a single mistaken key from losing the user's place in
            // Quick Settings.
            // An open section closes before the panel does, as its
            // chevron would.
            if st.quick_settings.wifi_password.take().is_some() {
                st.bump();
            } else if !matches!(
                st.quick_settings.current_level(),
                crate::shell::quick_settings::QsLevel::Root
            ) || st.quick_settings.pasting()
            {
                // Clipboard history opened to paste has no page behind it.
                st.quick_settings_back();
            } else if self.qs_root.close_section(&mut st.quick_settings) {
                st.bump();
            } else {
                st.overlays.quick_settings = false;
                st.bump();
            }
            *handled = true;
            return true;
        }
        // Toasts are transient chrome rather than a modal, so they come
        // after every real overlay in the dismissal ladder. Escape still
        // needs to offer a keyboard path out for people who cannot reach the
        // small close affordance with a pointer.
        if let Some(id) = st
            .notifications()
            .last()
            .map(|notification| notification.id)
        {
            st.dismiss_notification(id);
            *handled = true;
            return true;
        }
        false
    }

    /// Finish the key sequence the rename field started, even after commit
    /// removes the draft. A fresh press belongs to normal contextual routing.
    pub(super) fn consume_workspace_rename_submit_edge(&mut self, event: &WindowEvent) -> bool {
        if matches!(event, WindowEvent::Focused(false)) {
            self.workspace_rename_submit_held = false;
        }
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return false;
        };
        if !matches!(key.logical_key, Key::Named(NamedKey::Enter)) {
            return false;
        }
        if self.workspace_rename_submit_held && (key.repeat || key.state == ElementState::Released)
        {
            if key.state == ElementState::Released {
                self.workspace_rename_submit_held = false;
            }
            return true;
        }
        if key.state == ElementState::Pressed {
            self.workspace_rename_submit_held = false;
        }
        false
    }

    /// The workspace-rename text field's own key handling: a buffer in
    /// flight consumes character/backspace/enter/navigation/clipboard
    /// keys before anything else gets a chance.
    pub(super) fn handle_workspace_rename_key(
        &self,
        logical_key: &Key,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) -> bool {
        // The command palette is the topmost modal surface. A rename buffer
        // may remain alive underneath it, but must not intercept the
        // palette's query text or navigation keys.
        if self.palette_visible {
            return false;
        }
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let is_editing = chrome.borrow().editing_workspace_name.is_some();
        if !is_editing {
            return false;
        }
        let mods_state = modifiers.state();
        let shift = mods_state.shift_key();
        let ctrl = mods_state.control_key() || mods_state.super_key();
        let mut st = chrome.borrow_mut();
        if matches!(logical_key, Key::Named(NamedKey::Escape)) {
            st.editing_workspace_name = None;
            st.bump();
            *handled = true;
            return true;
        }
        // Pulled out by value first so it isn't a separate
        // immutable borrow alongside `buf`.
        let sel = st.editing_workspace_name_selection;
        let Some(buf) = st.editing_workspace_name.as_mut() else {
            return true;
        };
        if matches!(logical_key, Key::Named(NamedKey::Enter)) {
            drop(st);
            chrome.borrow_mut().commit_workspace_name();
            *handled = true;
            return true;
        }
        let (consumed, needs_bump, sel) =
            Self::apply_workspace_rename_key(logical_key, ctrl, shift, buf, sel);
        if consumed {
            st.editing_workspace_name_selection = sel;
            if needs_bump {
                st.bump();
            }
            *handled = true;
            return true;
        }
        // A modal text field owns even keys it does not interpret. This is
        // what prevents a reserved/global chord from mutating the desktop.
        *handled = true;
        true
    }

    /// Keep a typed status within [`ui::MAX_STATUS_CHARS`].
    /// An input method's text for whichever chrome field holds the keyboard
    /// (the workspace name, the status), typed at its caret as keys are.
    /// `false` when no such field is open.
    pub(super) fn commit_into_chrome_field(&mut self, text: &str) -> bool {
        use tilo_desktop::text_entry::{Edit, commit};
        if self.directory_sheet.commit_text(text) {
            return true;
        }
        if !self.palette_visible
            && let Some(chrome) = self.chrome.clone()
        {
            let mut st = chrome.borrow_mut();
            let mut sel = st.editing_workspace_name_selection;
            if let Some(buf) = st.editing_workspace_name.as_mut() {
                if commit(text, buf, &mut sel) == Edit::Changed {
                    st.editing_workspace_name_selection = sel;
                    st.bump();
                }
                return true;
            }
        }
        if !self.palette_visible && self.strip_navigator.commit_text(text) {
            self.bump_chrome();
            return true;
        }
        if !self.palette_visible && self.overview_naming_commit_text(text) {
            self.bump_chrome();
            return true;
        }
        false
    }

    /// Every workspace-rename chord but `Enter`, applied to `buf`/`sel` in
    /// place through the chrome's shared field editing and returned as
    /// `(consumed, needs_bump, selection)`. `Enter` is special-cased by the
    /// caller, which needs to drop the borrow this only holds a `&mut` into.
    pub(super) fn apply_workspace_rename_key(
        logical_key: &Key,
        ctrl: bool,
        shift: bool,
        buf: &mut String,
        mut sel: ui::text_selection::Selection,
    ) -> (bool, bool, ui::text_selection::Selection) {
        use tilo_desktop::text_entry::{Edit, Mods, apply};
        let (consumed, needs_bump) = match apply(logical_key, Mods { shift, ctrl }, buf, &mut sel) {
            Edit::Changed => (true, true),
            Edit::ConsumedQuiet | Edit::Submit | Edit::Cancel => (true, false),
            Edit::Ignored => (false, false),
        };
        (consumed, needs_bump, sel)
    }
}
