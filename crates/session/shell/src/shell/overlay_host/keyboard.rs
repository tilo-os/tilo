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
        if self.handle_collaborators_keyboard(logical_key, shift_down, handled) {
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
                self.latch_overview_transition_from(ui::tokens::MotionSource::Keyboard);
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
            && self
                .shortcut_action_outcome_as(action, repeat, ui::tokens::MotionSource::Keyboard)
                .is_some()
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
                    let changed = self.shortcut_action_outcome_direct(action);
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
            let started = if invocation.tab == QUICK_SETTINGS_WINDOW {
                self.invoke_quick_settings_action(&invocation.address, &invocation.arguments)
            } else {
                match self.desktop_surface_mut() {
                    Some(surface) => surface.invoke_device_local_action(
                        invocation.tab,
                        &invocation.address,
                        &invocation.arguments,
                    ),
                    None => Err(tilo_actions::DoorError::Unknown(invocation.address.clone())),
                }
            };
            *invocation.answer.borrow_mut() = Some(receipt(started));
        }
        if ran {
            crate::shell::search::invocation_wake::waker().wake_by_ref();
        }
        // Quick Settings is this device's own, whether or not it is open:
        // its actions are always there for a script or an agent here.
        let panel = self
            .quick_settings_device_local_resources()
            .map(|resources| (QUICK_SETTINGS_WINDOW, resources));
        let windows: Vec<DeviceLocalWindow> = self
            .desktop_surface_mut()
            .map(tilo_desktop::composition::DesktopSurface::device_local_windows)
            .unwrap_or_default()
            .into_iter()
            .chain(panel)
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
        self.shortcut_action_outcome_as(action, false, ui::tokens::MotionSource::Action)
            .is_some()
    }

    /// Run `action` once, as its chord does: whether it changed anything,
    /// or `None` when it is not a desktop shortcut the host runs. A desktop
    /// chord (a picker, a focus by number) counts as a change once handled.
    ///
    /// The shortcut catalogue says which action does what each shortcut does
    /// ([`settings_store::hardware::ShortcutAction::action`]), so a key runs
    /// the published action — the same one the palette's row, an agent and a
    /// screen reader invoke. What the shell does not publish a row for yet
    /// is still run here, so no chord loses its behaviour to a missing row.
    ///
    /// A key that acts once, like a move to the next window, acts on the
    /// press and not again on every repeat of a held key.
    fn shortcut_action_outcome_as(
        &mut self,
        action: settings_store::hardware::ShortcutAction,
        repeat: bool,
        source: ui::tokens::MotionSource,
    ) -> Option<bool> {
        if !Self::is_navigation_shortcut(action) {
            return self
                .handle_desktop_chord(action, repeat, source)
                .then_some(true);
        }
        if repeat {
            return Some(false);
        }
        self.with_motion(true, |host| host.run_shortcut_as_action(action))
    }

    /// Run `action` the way the desktop itself does, without going through
    /// the action that publishes it: the chord's own code path, which is
    /// what an action's request finally asks for here.
    fn shortcut_action_outcome_direct(
        &mut self,
        action: settings_store::hardware::ShortcutAction,
    ) -> Option<bool> {
        self.navigation_shortcut_outcome(action, false, ui::tokens::MotionSource::Action)
            .or_else(|| {
                self.handle_desktop_chord(action, false, ui::tokens::MotionSource::Action)
                    .then_some(true)
            })
    }

    /// The action `action` publishes, run; `None` when nothing publishes it.
    fn run_shortcut_as_action(
        &mut self,
        action: settings_store::hardware::ShortcutAction,
    ) -> Option<bool> {
        let (provider, id) = action.action()?;
        let invoked_by =
            settings_store::InvokingAction::action(format!("shortcut.{}", action.id()));
        if let Some(outcome) = self.run_published_action(provider, id, &invoked_by) {
            return Some(outcome);
        }
        // The action door answered nothing: run the chord's own code rather
        // than lose the key's behaviour to a row nobody published.
        if Self::is_navigation_shortcut(action) {
            return self.shortcut_action_outcome_direct(action);
        }
        self.run_desktop_action(action)
    }

    /// `outcome` with `run` started first when `moving`, so a key that moves
    /// something animates like the chord it is.
    fn with_motion<T>(&mut self, moving: bool, run: impl FnOnce(&mut Self) -> T) -> T {
        if moving {
            self.begin_motion(ui::tokens::MotionSource::Action);
        }
        run(self)
    }

    /// The desktop's own actions, as the palette's row for each runs them
    /// ([`Self::run_desktop_chord_chrome`] holds the one implementation).
    fn run_desktop_action(
        &mut self,
        action: settings_store::hardware::ShortcutAction,
    ) -> Option<bool> {
        use settings_store::hardware::ShortcutAction as A;
        match action {
            A::TabMenu => Some(self.open_focused_tab_context_menu()),
            A::AskAgent => Some(self.press_ask_agent_chord(false)),
            A::SaySomething => {
                self.open_cursor_chat_on_request();
                Some(true)
            }
            A::ReportProblem if !self.feedback_configured() => Some(false),
            _ => {
                let chrome = self.chrome.clone()?;
                self.run_desktop_chord_chrome(&chrome, action);
                Some(true)
            }
        }
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
}
