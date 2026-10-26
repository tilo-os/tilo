//! The shell's keyboard dispatch: navigation shortcuts, dictation and banner keys.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    #[allow(
        clippy::items_after_statements,
        clippy::too_many_lines,
        reason = "central keyboard dispatcher keeps shortcut precedence visible in one ordered match"
    )]
    pub(super) fn handle_shell_keyboard_event(
        &mut self,
        logical_key: &Key,
        modifiers: winit::event::Modifiers,
        repeat: bool,
        state: &AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) -> bool {
        let mstate = modifiers.state();
        let super_down = mstate.super_key();
        let shift_down = mstate.shift_key();
        // Workspace naming is a text-field modal.  It must arbitrate before
        // every shell chord: otherwise Super+W/K/T can close a tab or open a
        // palette while the field is visibly active.
        let rename_submit = matches!(logical_key, Key::Named(NamedKey::Enter))
            && !self.palette_visible
            && self
                .chrome
                .as_ref()
                .is_some_and(|chrome| chrome.borrow().editing_workspace_name.is_some());
        if self.handle_workspace_rename_key(logical_key, modifiers, handled) {
            self.workspace_rename_submit_held |= rename_submit;
            return true;
        }
        // The strip switcher, opened for the keyboard or naming a strip, takes
        // every key but the Super+S that closes it, ahead of the shell's
        // chords for the same reason as the workspace name above.
        let closes_switcher =
            super_down && matches!(logical_key, Key::Character(c) if c.eq_ignore_ascii_case("s"));
        if !self.palette_visible && self.strip_navigator.takes_keys() && !closes_switcher {
            self.strip_switcher_key(logical_key, mstate);
            *handled = true;
            return true;
        }
        // A strip's name being typed in the overview takes every key, for the
        // same reason.
        if !self.palette_visible && self.overview_naming.is_some() {
            self.overview_naming_key(logical_key, mstate);
            *handled = true;
            return true;
        }
        // The usual host event path handles this ahead of modal routing.
        // Keep the same arbitration here for direct widget dispatch and any
        // future backend that enters through this keyboard-only helper.
        if matches!(logical_key, Key::Named(NamedKey::PrintScreen)) {
            if !repeat {
                self.apply_print_screen_priority(modifiers.state());
            }
            *handled = true;
            return true;
        }
        if self.handle_member_permission_sheet_keyboard(logical_key, handled) {
            return true;
        }
        if self.handle_agent_access_keyboard(logical_key, shift_down, handled) {
            return true;
        }
        if self.overlay_flags().shortcut_help
            && let Some(action) = self.shortcut_rebind
        {
            if matches!(logical_key, Key::Named(NamedKey::Escape)) {
                self.shortcut_rebind = None;
                self.shortcut_rebind_message = None;
            } else {
                self.capture_shortcut_chord(action, logical_key, modifiers);
            }
            self.bump_chrome();
            *handled = true;
            return true;
        }
        if self.handle_collaborators_keyboard(logical_key, shift_down, handled) {
            return true;
        }
        if self.handle_quick_settings_keyboard(
            logical_key,
            shift_down,
            mstate.control_key(),
            handled,
        ) {
            return true;
        }
        let configured_action = self.configured_shortcut_action(logical_key, modifiers);
        if self.handle_banner_keyboard(logical_key, configured_action, shift_down, repeat, handled)
        {
            return true;
        }
        let shortcut_pressed = |action| configured_action == Some(action);
        // F1 is the fixed, hardware-neutral desktop help key. The configured
        // shortcut is a second route to the exact same guide.
        let help_chord = matches!(logical_key, Key::Named(NamedKey::F1))
            || shortcut_pressed(settings_store::hardware::ShortcutAction::ShortcutHelp);
        if help_chord {
            if !repeat {
                if let Some(chrome) = self.chrome.clone() {
                    let mut st = chrome.borrow_mut();
                    st.overlays.shortcut_help = !st.overlays.shortcut_help;
                    st.bump();
                }
                self.shortcut_rebind = None;
                self.shortcut_rebind_message = None;
                self.shortcut_guide_focus =
                    self.overlay_flags()
                        .shortcut_help
                        .then_some(ShortcutGuideFocus::Binding(
                            settings_store::hardware::ShortcutAction::FocusPreviousWindow,
                        ));
            }
            *handled = true;
            return true;
        }
        // Quick Settings is modal. Its own drill-in/root handler above gets
        // first refusal, F1 remains the documented shortcut-guide escape
        // hatch, and Escape follows the normal dismissal/back ladder. Every
        // other key is consumed before it can reach a hidden app or shell
        // action (including configured shortcuts and Super chords below).
        if self.overlay_flags().quick_settings {
            if matches!(logical_key, Key::Named(NamedKey::Escape)) {
                if self.overlay_flags().shortcut_help {
                    if let Some(chrome) = self.chrome.as_ref() {
                        let mut st = chrome.borrow_mut();
                        st.overlays.shortcut_help = false;
                        st.bump();
                    }
                    self.shortcut_rebind = None;
                    self.shortcut_rebind_message = None;
                    self.shortcut_guide_focus = None;
                    *handled = true;
                    return true;
                }
                if self.handle_shell_escape_chord(handled) {
                    return true;
                }
            }
            *handled = true;
            return true;
        }
        // The guide is modal: unfamiliar keys must not act on a window
        // hidden behind it. Escape and F1 are its explicit exits.
        if self.overlay_flags().shortcut_help {
            match logical_key {
                Key::Named(NamedKey::Tab) => {
                    self.move_shortcut_guide_focus(shift_down);
                }
                Key::Named(NamedKey::Enter | NamedKey::Space) => match self.shortcut_guide_focus {
                    Some(ShortcutGuideFocus::Binding(action)) => {
                        self.shortcut_rebind = Some(action);
                        self.shortcut_rebind_message = None;
                    }
                    Some(ShortcutGuideFocus::Reset(first, second)) => {
                        self.reset_shortcut_pair(first, second);
                        self.shortcut_guide_focus = Some(ShortcutGuideFocus::Binding(first));
                    }
                    Some(ShortcutGuideFocus::Done) => {
                        if let Some(chrome) = self.chrome.clone() {
                            let mut st = chrome.borrow_mut();
                            st.overlays.shortcut_help = false;
                            st.bump();
                        }
                        self.shortcut_guide_focus = None;
                    }
                    None => self.move_shortcut_guide_focus(false),
                },
                Key::Named(NamedKey::Escape) => {
                    if let Some(chrome) = self.chrome.clone() {
                        let mut st = chrome.borrow_mut();
                        st.overlays.shortcut_help = false;
                        st.bump();
                        self.shortcut_rebind = None;
                        self.shortcut_rebind_message = None;
                        self.shortcut_guide_focus = None;
                    }
                }
                _ => {}
            }
            self.bump_chrome();
            *handled = true;
            return true;
        }
        use settings_store::hardware::ShortcutAction;
        if shortcut_pressed(ShortcutAction::Search) {
            crate::compositor::key_latency::note_key_handler(
                crate::compositor::key_latency::KeyHandler::Shortcut(ShortcutAction::Search),
            );
            if !repeat {
                self.toggle_global_search();
                self.refresh_search_results(state, path);
            }
            *handled = true;
            return true;
        }
        if shortcut_pressed(ShortcutAction::WorkspaceOverview) {
            crate::compositor::key_latency::note_key_handler(
                crate::compositor::key_latency::KeyHandler::Shortcut(
                    ShortcutAction::WorkspaceOverview,
                ),
            );
            if !repeat && let Some(chrome) = self.chrome.clone() {
                let mut st = chrome.borrow_mut();
                st.overlays.overview = !st.overlays.overview;
                st.bump();
                drop(st);
                self.latch_keyboard_overview();
            }
            *handled = true;
            return true;
        }
        // Super+/: say something beside the pointer, whatever has the
        // keyboard; a bare "/" opens it only where nothing takes text.
        if shortcut_pressed(ShortcutAction::SaySomething) {
            crate::compositor::key_latency::note_key_handler(
                crate::compositor::key_latency::KeyHandler::Shortcut(ShortcutAction::SaySomething),
            );
            if !repeat {
                self.open_cursor_chat_on_request();
            }
            *handled = true;
            return true;
        }
        // Super+R: point at what is wrong, where reports have somewhere to go.
        if shortcut_pressed(ShortcutAction::ReportProblem) && self.feedback_configured() {
            crate::compositor::key_latency::note_key_handler(
                crate::compositor::key_latency::KeyHandler::Shortcut(ShortcutAction::ReportProblem),
            );
            if !repeat {
                self.open_feedback_picker();
            }
            *handled = true;
            return true;
        }
        if shortcut_pressed(ShortcutAction::QuickSettings) {
            crate::compositor::key_latency::note_key_handler(
                crate::compositor::key_latency::KeyHandler::Shortcut(ShortcutAction::QuickSettings),
            );
            if !repeat && let Some(chrome) = self.chrome.clone() {
                chrome.borrow_mut().toggle_quick_settings();
            }
            *handled = true;
            return true;
        }
        if let Some(action) = configured_action {
            crate::compositor::key_latency::note_key_handler(
                crate::compositor::key_latency::KeyHandler::Shortcut(action),
            );
        }
        if let Some(action) = configured_action
            && self.handle_desktop_chord(action, repeat, ui::tokens::MotionSource::Keyboard)
        {
            *handled = true;
            return true;
        }
        // Chords bound to published actions, a saved shortcut's keyboard
        // trigger among them. Saving refuses every fixed chord below, so
        // order only matters for a chord nothing else claims.
        if self.handle_action_chord(logical_key, modifiers, repeat) {
            *handled = true;
            return true;
        }
        // Alt+Tab is the conventional window ring and remains a stable
        // fallback even when spatial navigation is rebound in Settings.
        // This also keeps first-run discoverability consistent with other
        // Linux desktops.
        if let Some(scope) =
            tilo_desktop::window_switcher::SwitcherScope::for_chord(logical_key, mstate)
        {
            if !repeat {
                let _ = self.begin_window_switcher(scope, shift_down);
            }
            *handled = true;
            return true;
        }
        // Alt+F4 is the stable Linux close gesture. Tilo owns the frame and
        // its durable tab/group state, so forwarding this to the client would
        // let toolkits disagree about whether the window actually closed.
        if mstate.alt_key()
            && !mstate.control_key()
            && !super_down
            && !shift_down
            && matches!(logical_key, Key::Named(NamedKey::F4))
        {
            if !repeat {
                let _ = self.close_focused_tab();
            }
            *handled = true;
            return true;
        }
        // Super+Page Up/Down switch workspace whatever the editable
        // workspace bindings say: they are fixed chords, clamped by the
        // same workspace primitive as those bindings.
        if super_down
            && !mstate.control_key()
            && !mstate.alt_key()
            && !shift_down
            && matches!(
                logical_key,
                Key::Named(NamedKey::PageUp | NamedKey::PageDown)
            )
        {
            let direction = if matches!(logical_key, Key::Named(NamedKey::PageUp)) {
                -1
            } else {
                1
            };
            if let Some(chrome) = self.chrome.clone()
                && chrome.borrow_mut().switch_workspace(direction)
            {
                // Both directions. Page Down's following frames were already
                // cheap; Page Up fell through to a full upload of the desktop
                // just restored.
                self.start_workspace_switch(ui::tokens::MotionSource::Keyboard);
            }
            *handled = true;
            return true;
        }
        // Super+L is the conventional immediate lock gesture and is already
        // reserved by client-input routing and advertised by the shortcut
        // guide. Publish the same compositor-owned edge as the power menu;
        // never delegate to logind or let a focused client consume it.
        if super_down
            && !mstate.control_key()
            && !mstate.alt_key()
            && !shift_down
            && let Key::Character(key) = logical_key
            && key.eq_ignore_ascii_case("l")
        {
            if !repeat && let Some(chrome) = self.chrome.as_ref() {
                chrome.borrow_mut().request_session_lock();
            }
            *handled = true;
            return true;
        }
        // Optional Vim navigation is intentionally a full Super+Ctrl layer,
        // never a bare/Ctrl-only text chord. This keeps H/J/K/L available to
        // terminals and editors and matches the promise in Settings.
        if self.desktop_controls.config().vim_mode
            && super_down
            && mstate.control_key()
            && !mstate.alt_key()
            && let Key::Character(key) = logical_key
        {
            let action = match (key.to_ascii_lowercase().as_str(), shift_down) {
                ("h", false) => Some(ShortcutAction::FocusPreviousWindow),
                ("l", false) => Some(ShortcutAction::FocusNextWindow),
                ("k", false) => Some(ShortcutAction::PreviousStrip),
                ("j", false) => Some(ShortcutAction::NextStrip),
                ("h", true) => Some(ShortcutAction::MoveGroupLeft),
                ("l", true) => Some(ShortcutAction::MoveGroupRight),
                ("k", true) => Some(ShortcutAction::MoveGroupToPreviousStrip),
                ("j", true) => Some(ShortcutAction::MoveGroupToNextStrip),
                _ => None,
            };
            if let Some(action) = action {
                self.begin_motion(ui::tokens::MotionSource::Keyboard);
                match action {
                    ShortcutAction::FocusPreviousWindow => {
                        let _ = self.step_focus_along_strip(-1);
                    }
                    ShortcutAction::FocusNextWindow => {
                        let _ = self.step_focus_along_strip(1);
                    }
                    ShortcutAction::PreviousStrip | ShortcutAction::NextStrip => {
                        let direction = if action == ShortcutAction::PreviousStrip {
                            -1
                        } else {
                            1
                        };
                        if let Some(chrome) = self.chrome.clone() {
                            let _ = chrome.borrow_mut().switch_strip(direction);
                        }
                    }
                    ShortcutAction::MoveGroupLeft => {
                        let _ = self.reorder_focused_group(-1);
                    }
                    ShortcutAction::MoveGroupRight => {
                        let _ = self.reorder_focused_group(1);
                    }
                    ShortcutAction::MoveGroupToPreviousStrip => {
                        let _ = self.move_focused_group_to_strip(-1);
                    }
                    ShortcutAction::MoveGroupToNextStrip => {
                        let _ = self.move_focused_group_to_strip(1);
                    }
                    _ => {}
                }
                *handled = true;
                return true;
            }
        }
        let navigation = [
            ShortcutAction::PreviousTab,
            ShortcutAction::NextTab,
            ShortcutAction::ResizeNarrower,
            ShortcutAction::ResizeWider,
            ShortcutAction::MoveGroupLeft,
            ShortcutAction::MoveGroupRight,
            ShortcutAction::MoveGroupToPreviousStrip,
            ShortcutAction::MoveGroupToNextStrip,
            ShortcutAction::MoveGroupToPreviousDisplay,
            ShortcutAction::MoveGroupToNextDisplay,
            ShortcutAction::FocusPreviousWindow,
            ShortcutAction::FocusNextWindow,
            ShortcutAction::PreviousStrip,
            ShortcutAction::NextStrip,
            ShortcutAction::PreviousWorkspace,
            ShortcutAction::NextWorkspace,
            ShortcutAction::ToggleMaximize,
            ShortcutAction::MaximizeWindow,
            ShortcutAction::RestoreWindow,
            ShortcutAction::DetachTab,
            ShortcutAction::AttachTabLeft,
            ShortcutAction::AttachTabRight,
            ShortcutAction::ReopenClosedTab,
        ];
        if let Some(action) = navigation
            .into_iter()
            .find(|action| shortcut_pressed(*action))
        {
            crate::compositor::key_latency::note_key_handler(
                crate::compositor::key_latency::KeyHandler::Shortcut(action),
            );
            let _ = self.navigation_shortcut_outcome(
                action,
                repeat,
                ui::tokens::MotionSource::Keyboard,
            );
            *handled = true;
            return true;
        }
        if self.handle_overview_strip_keys(logical_key, mstate, handled) {
            return true;
        }
        if self.handle_overview_keyboard_nav(logical_key, shift_down, handled) {
            return true;
        }
        if super_down
            && self.handle_shell_mod_chords(
                logical_key,
                super_down,
                shift_down,
                repeat,
                state,
                path,
                handled,
            )
        {
            return true;
        }
        if matches!(logical_key, Key::Named(NamedKey::Escape))
            && self.handle_shell_escape_chord(handled)
        {
            return true;
        }
        false
    }

    /// Run one editable navigation or window shortcut: whether it changed
    /// anything, or `None` when `action` is not one of them. A held chord's
    /// repeat changes nothing for an action that acts once.
    #[allow(
        clippy::too_many_lines,
        reason = "one dispatch over every navigation shortcut"
    )]
    pub(super) fn navigation_shortcut_outcome(
        &mut self,
        action: settings_store::hardware::ShortcutAction,
        repeat: bool,
        source: ui::tokens::MotionSource,
    ) -> Option<bool> {
        use settings_store::hardware::ShortcutAction;
        if Self::is_navigation_shortcut(action) {
            self.begin_motion(source);
        }
        let changed = match action {
            ShortcutAction::PreviousTab => self.cycle_focused_tab(-1),
            ShortcutAction::NextTab => self.cycle_focused_tab(1),
            ShortcutAction::ToggleMaximize => !repeat && self.toggle_focused_group_maximized(),
            ShortcutAction::MaximizeWindow | ShortcutAction::RestoreWindow => {
                !repeat
                    && self.set_focused_group_maximized(action == ShortcutAction::MaximizeWindow)
            }
            ShortcutAction::DetachTab => !repeat && self.detach_focused_tab(),
            ShortcutAction::AttachTabLeft => !repeat && self.attach_focused_tab(-1),
            ShortcutAction::AttachTabRight => !repeat && self.attach_focused_tab(1),
            ShortcutAction::ReopenClosedTab => {
                !repeat
                    && self
                        .chrome
                        .clone()
                        .is_some_and(|chrome| chrome.borrow_mut().reopen_closed_tab().is_some())
            }
            ShortcutAction::ResizeNarrower => self.resize_focused_group(-50),
            ShortcutAction::ResizeWider => self.resize_focused_group(50),
            ShortcutAction::MoveGroupLeft => self.reorder_focused_group(-1),
            ShortcutAction::MoveGroupRight => self.reorder_focused_group(1),
            ShortcutAction::MoveGroupToPreviousStrip => self.move_focused_group_to_strip(-1),
            ShortcutAction::MoveGroupToNextStrip => self.move_focused_group_to_strip(1),
            ShortcutAction::MoveGroupToPreviousDisplay => {
                !repeat && self.move_focused_group_to_display(-1)
            }
            ShortcutAction::MoveGroupToNextDisplay => {
                !repeat && self.move_focused_group_to_display(1)
            }
            ShortcutAction::FocusPreviousWindow => self.step_focus_along_strip(-1),
            ShortcutAction::FocusNextWindow => self.step_focus_along_strip(1),
            ShortcutAction::PreviousStrip | ShortcutAction::NextStrip => {
                let direction = if action == ShortcutAction::PreviousStrip {
                    -1
                } else {
                    1
                };
                self.chrome
                    .clone()
                    .is_some_and(|chrome| chrome.borrow_mut().switch_strip(direction))
            }
            ShortcutAction::PreviousWorkspace | ShortcutAction::NextWorkspace => {
                let direction = if action == ShortcutAction::PreviousWorkspace {
                    -1
                } else {
                    1
                };
                let switched = self
                    .chrome
                    .clone()
                    .is_some_and(|chrome| chrome.borrow_mut().switch_workspace(direction));
                if switched {
                    self.start_workspace_switch(source);
                }
                switched
            }
            _ => return None,
        };
        Some(changed)
    }

    /// Intercept only local input controls without withdrawing text-input focus.
    pub(crate) fn dictation_owns_key(&self, key: &winit::keyboard::Key) -> bool {
        self.layers.dictation.owns_key(key)
    }

    /// Local microphone controls cannot be operated through a shared seat.
    /// This does not withdraw the original editable field's local IME focus.
    pub(crate) fn dictation_mutes_remote_input(&self) -> bool {
        self.layers.dictation.is_open()
    }

    pub(crate) fn dictation_pointer_at(&self, point: (f64, f64)) -> bool {
        let outputs = self.layer_outputs();
        self.layers
            .dictation
            .contains(self.layer_geometry(&outputs), Point::new(point.0, point.1))
    }

    pub(crate) fn dictation_input_available(&self) -> bool {
        self.onboarding.is_none()
            && self.permission_prompt.is_none()
            && self.portal_dialogs.layout(self.overlay_area()).is_none()
            && !self.display_confirmation_visible()
            && !self.window_switcher_active()
    }

    pub(crate) fn dictation_surface_mut(&mut self) -> &mut dictation_layer::DictationSurface {
        &mut self.layers.dictation
    }

    /// Public fixture for the existing offscreen shots driver. No session,
    /// microphone, model, permission, or field is created by this showcase.
    /// # Errors
    /// Requires the debug environment gate and a known fixture phase.
    #[cfg(debug_assertions)]
    pub fn seed_dictation_for_shots(&mut self, phase: &str) -> Result<(), &'static str> {
        use crate::compositor::dictation_status::DictationStatus;
        if std::env::var_os("TILO_UI_DEBUG").is_none() {
            return Err("dictation shots require TILO_UI_DEBUG and a debug build");
        }
        match phase {
            "dictation-consent" => self.layers.dictation.consent(),
            "dictation-listening" | "dictation-finishing" => {
                self.layers
                    .dictation
                    .status(if phase == "dictation-listening" {
                        DictationStatus::Listening
                    } else {
                        DictationStatus::Finishing
                    });
                self.layers.dictation.preview(Some(
                    "A public offscreen example: ideas can arrive through speech, a keyboard, or another assistive input. 日本語 · العربية · Français",
                ));
            }
            "dictation-model-required" => {
                self.layers.dictation.status(DictationStatus::Unavailable(
                    crate::compositor::dictation_status::RuntimeError::Worker(
                        agent_provider_config::speech::SpeechError::ModelUnavailable,
                    ),
                ));
            }
            _ => return Err("unknown dictation shots phase"),
        }
        Ok(())
    }

    /// Run every window operation the action door queued on the chrome
    /// ([`ChromeState::request_shortcut_action`]), in order. The shell
    /// runs this each update, before splicing tree changes into the rows.
    ///
    /// [`ChromeState::request_shortcut_action`]: crate::shell::chrome_state::ChromeState::request_shortcut_action
    pub fn run_requested_shortcut_actions(&mut self) {
        let cancel = self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.borrow_mut().take_tab_drag_cancel_request());
        if cancel {
            self.cancel_tab_drags();
        }

        let gather = self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.borrow_mut().take_axis_gather_request());
        if gather {
            self.request_axis_gather();
        }
        let requested = self
            .chrome
            .as_ref()
            .map(|chrome| chrome.borrow_mut().take_window_requests())
            .unwrap_or_default();
        let mut answered = false;
        for (request, outcome) in requested {
            let changed = match request {
                crate::shell::chrome_state::WindowRequest::Shortcut(action) => {
                    let changed = self.shortcut_action_outcome(action);
                    if changed.is_none() {
                        tracing::warn!(?action, "requested shortcut action has no desktop handler");
                    }
                    changed.unwrap_or(false)
                }
                crate::shell::chrome_state::WindowRequest::MoveGroupToStrip { group, strip } => {
                    self.begin_motion(ui::tokens::MotionSource::Action);
                    self.move_group_to_strip(group, strip)
                }
            };
            if let Some(outcome) = outcome {
                outcome.set(Some(changed));
                answered = true;
            }
        }
        // A caller waiting on an outcome hears it now, not on whatever
        // wakes the loop next.
        if answered {
            crate::shell::search::invocation_wake::waker().wake_by_ref();
        }
    }

    /// Serve private windows' actions on this device
    /// ([`crate::shell::device_local_actions`]): run what the catalogue
    /// queued through each window's door, then publish what every private
    /// window offers now.
    pub(crate) fn run_device_local_actions(&mut self) {
        use crate::shell::device_local_actions::{DeviceLocalWindow, receipt};
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let queued = std::mem::take(&mut chrome.borrow_mut().device_local_invocations);
        let ran = !queued.is_empty();
        for invocation in queued {
            let started = match self.desktop_surface_mut() {
                Some(surface) => surface.invoke_device_local_action(
                    invocation.tab,
                    &invocation.address,
                    &invocation.arguments,
                ),
                None => Err(tilo_actions::DoorError::Unknown(invocation.address.clone())),
            };
            *invocation.answer.borrow_mut() = Some(receipt(started));
        }
        if ran {
            crate::shell::search::invocation_wake::waker().wake_by_ref();
        }
        let windows: Vec<DeviceLocalWindow> = self
            .desktop_surface_mut()
            .map(tilo_desktop::composition::DesktopSurface::device_local_windows)
            .unwrap_or_default()
            .into_iter()
            .map(|(tab, resources)| DeviceLocalWindow {
                tab,
                actions: tilo_actions::door::survey(&resources),
            })
            .collect();
        let mut state = chrome.borrow_mut();
        if state.device_local_windows != windows {
            state.device_local_windows = windows;
            state.device_local_revision = state.device_local_revision.wrapping_add(1);
            // Read once more after the tree settles: a window opened this
            // frame mounts after this pass.
            state.bump();
        }
    }

    /// Run `action` once, as its chord does; for the action door, which
    /// has no key event. `false` when it is not a desktop shortcut the
    /// host runs.
    #[cfg(test)]
    pub(crate) fn run_shortcut_action(
        &mut self,
        action: settings_store::hardware::ShortcutAction,
    ) -> bool {
        self.shortcut_action_outcome(action).is_some()
    }

    /// Run `action` once, as its chord does: whether it changed anything,
    /// or `None` when it is not a desktop shortcut the host runs. A desktop
    /// chord (a picker, a focus by number) counts as a change once handled.
    fn shortcut_action_outcome(
        &mut self,
        action: settings_store::hardware::ShortcutAction,
    ) -> Option<bool> {
        let source = ui::tokens::MotionSource::Action;
        self.navigation_shortcut_outcome(action, false, source)
            .or_else(|| {
                self.handle_desktop_chord(action, false, source)
                    .then_some(true)
            })
    }

    /// Whether [`Self::navigation_shortcut_outcome`] handles `action`.
    pub(super) fn is_navigation_shortcut(action: settings_store::hardware::ShortcutAction) -> bool {
        use settings_store::hardware::ShortcutAction as A;
        matches!(
            action,
            A::PreviousTab
                | A::NextTab
                | A::ToggleMaximize
                | A::MaximizeWindow
                | A::RestoreWindow
                | A::DetachTab
                | A::AttachTabLeft
                | A::AttachTabRight
                | A::ReopenClosedTab
                | A::ResizeNarrower
                | A::ResizeWider
                | A::MoveGroupLeft
                | A::MoveGroupRight
                | A::MoveGroupToPreviousStrip
                | A::MoveGroupToNextStrip
                | A::MoveGroupToPreviousDisplay
                | A::MoveGroupToNextDisplay
                | A::FocusPreviousWindow
                | A::FocusNextWindow
                | A::PreviousStrip
                | A::NextStrip
                | A::PreviousWorkspace
                | A::NextWorkspace
        )
    }

    /// Tell the desktop what starts the change about to be made, before
    /// making it, so the springs and fades it starts follow that input.
    pub(super) fn begin_motion(&mut self, source: ui::tokens::MotionSource) {
        if let Some(surface) = self.desktop_surface_mut() {
            surface.begin_transition(source);
        }
    }

    /// The conventional desktop chords (Super+1…9 for strips, Ctrl+Super+1…9
    /// for windows, Super+E, Ctrl+Alt+T,
    /// Super+V, Super+A, Super+F, Super+Alt+8, Ctrl+Alt+Delete by
    /// default). Returns `false` for any other action. A held chord acts
    /// once: every action here is an edge, not a step.
    /// Keyboard focus in the banner stack. The Notifications chord moves
    /// focus into the newest banner when one is on screen (pressed again,
    /// it opens the centre as before); the stack then takes the arrows,
    /// Tab, Return, Space and Escape, and any other key gives focus back to
    /// the window. Returns whether the key was taken.
    pub(super) fn handle_banner_keyboard(
        &mut self,
        key: &Key,
        action: Option<settings_store::hardware::ShortcutAction>,
        shift: bool,
        repeat: bool,
        handled: &mut bool,
    ) -> bool {
        use settings_store::hardware::ShortcutAction;
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let host = ChromeNotifications::new(&chrome);
        let chord = action == Some(ShortcutAction::Notifications);
        if !self.notification_stack.has_keyboard_focus() {
            if chord && !repeat && self.notification_stack.focus_newest(&host) {
                chrome.borrow_mut().bump();
                *handled = true;
                return true;
            }
            return false;
        }
        if !chord && self.notification_stack.handle_key(&host, key, shift) {
            chrome.borrow_mut().bump();
            *handled = true;
            return true;
        }
        if self.notification_stack.release_keyboard(&host) {
            chrome.borrow_mut().bump();
        }
        false
    }

    /// Whether the banner stack holds keyboard focus and acts on `key`, so
    /// the window behind it must not get it.
    pub fn banner_owns_key(&self, key: &Key) -> bool {
        self.notification_stack.owns_key(key)
    }

    /// Whether keyboard focus is in the banner stack.
    pub fn banner_has_keyboard(&self) -> bool {
        self.notification_stack.has_keyboard_focus()
    }

    /// Give keyboard focus back from the banner stack to the window: a key
    /// the stack does not act on went there, or a press landed elsewhere.
    pub fn release_banner_keyboard(&mut self) {
        if let Some(chrome) = self.chrome.clone()
            && self
                .notification_stack
                .release_keyboard(&ChromeNotifications::new(&chrome))
        {
            chrome.borrow_mut().bump();
        }
    }
}
