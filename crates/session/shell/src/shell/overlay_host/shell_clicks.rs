//! Shell pointer clicks outside the overlays: workspace navigation and search.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Outside-click close for a popover/modal, swallowing the click so
    /// content beneath doesn't react: strip ribbon/rail focus, the
    /// search's outside-dismiss, the power menu, then
    /// (each absorbing every click once open) room activity,
    /// collaborators, quick settings and workspace settings. Overview
    /// has its own dismissal path, so it is skipped here. Was over 700
    /// lines of one `handle_event`; the flag-gated per-overlay handlers
    /// always end in `*handled = true`, so once inside one this always
    /// returns.
    #[allow(
        clippy::too_many_lines,
        reason = "ordered modal hit testing is intentionally centralized"
    )]
    pub(super) fn handle_shell_mouse_click_event(
        &mut self,
        event: &WindowEvent,
        handled: &mut bool,
    ) -> bool {
        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
        else {
            return false;
        };
        let Some(p) = self.last_cursor else {
            return false;
        };
        if !self.shortcut_help_button_is_suppressed()
            && self.shortcut_help_button_rect().contains(p)
        {
            if let Some(chrome) = self.chrome.clone() {
                let mut st = chrome.borrow_mut();
                st.overlays.shortcut_help = true;
                st.bump();
            }
            *handled = true;
            return true;
        }
        if self.handle_workspace_navigation_click(p) {
            *handled = true;
            return true;
        }
        if self.palette_visible && self.handle_palette_pointer_click(p) {
            *handled = true;
            return true;
        }
        let flags = self.overlay_flags();
        if flags.capture && !self.capture_studio.is_open() {
            self.capture_studio.open();
        }
        if flags.collaborators {
            *handled = true;
            return true;
        }
        if flags.quick_settings {
            // The panel took what was over it; anything else closed it.
            *handled = true;
            return true;
        }
        if flags.ws_settings {
            if self.ws_settings_rect().contains(p) {
                self.handle_shell_ws_settings_click(p, handled);
                return true;
            }
            if let Some(chrome) = self.chrome.clone() {
                // Commit an in-progress field before the desktop takes focus,
                // but keep the contextual lens open beside the workspace.
                chrome.borrow_mut().commit_workspace_name();
            }
        }
        false
    }

    /// Whether `p` is on a window's edge grip. The strip ribbon and rail hit
    /// zones are wider than their painted segments and reach into the
    /// windows, so inside a window the grip has to win.
    pub(super) fn point_on_window_edge_grip(&self, p: Point) -> bool {
        self.group_row_ptrs.values().any(|&ptr| {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &GroupRow = unsafe { &*ptr };
            row.resize_grab_hit_at(p)
        })
    }

    pub(super) fn handle_workspace_navigation_click(&mut self, p: Point) -> bool {
        if !self.cursor_over_open_overlay(p) && self.follow_banner.stop_rect().contains(p) {
            self.set_followed_peer(None);
            return true;
        }
        // Following is a viewing mode, not a lock. The first deliberate
        // click back into the workspace hands control to the local user and
        // continues routing that very click to its intended target.
        if self.followed_peer.is_some()
            && !self.cursor_over_open_overlay(p)
            && !self.follow_banner.chip_rect().contains(p)
        {
            self.stop_following_for_local_input();
        }
        if self.followed_peer.is_none()
            && !self.cursor_over_open_overlay(p)
            && let Some((peer_id, _)) = self
                .follow_cursor_targets
                .iter()
                .find(|(_, target)| target.contains(p))
        {
            // A cursor is an embodied presence affordance: its halo is the
            // visible action, so a person can attach to work without opening
            // a roster or interpreting a text command.
            self.set_followed_peer(Some(peer_id.clone()));
            return true;
        }
        // Gated on no overlay owning the pointer, so a click
        // under an open panel goes to the panel.
        !self.cursor_over_open_overlay(p)
            && !self.point_on_window_edge_grip(p)
            && self.press_strip_ribbon_or_rail(p)
    }

    /// A press while search is open: inside the launcher's place it is the
    /// launcher's (which closes search for a press beside its card);
    /// anywhere else it closes search.
    pub(super) fn handle_palette_pointer_click(&mut self, p: Point) -> bool {
        if self.search_place().contains(p) {
            return false;
        }
        self.palette_visible = false;
        self.search.close();
        true
    }

    /// The one executor for session lifecycle actions, whichever door they
    /// came through: the power flyout's rows and the typed `session.*`
    /// actions both land here, so they share admission and failure reporting.
    pub(super) fn execute_session_action(&self, action: os_actions::SessionAction) {
        if let Err(reason) = self.session_action_policy.admit(action) {
            if let Some(chrome) = self.chrome.as_ref() {
                chrome
                    .borrow_mut()
                    .push_notification(t("host-session-action-blocked"), reason);
            }
            return;
        }
        match action {
            os_actions::SessionAction::Lock => {
                // Do not delegate this to logind: its lock-session request
                // cannot establish Tilo's lock scene or revoke our clients'
                // focus/input.  The native loop drains this edge through its
                // compositor-owned SessionManager instead.
                if let Some(chrome) = self.chrome.as_ref() {
                    chrome.borrow_mut().request_session_lock();
                } else {
                    tracing::error!(
                        target: "tilo::power",
                        "session Lock requested without compositor chrome state"
                    );
                }
            }
            os_actions::SessionAction::Suspend => {
                if let Some(bridge) = &self.power_bridge {
                    bridge.request_suspend();
                } else {
                    let _ = self.session_action_channel.0.send(SessionActionResult {
                        action,
                        result: Err("power service is unavailable".into()),
                    });
                }
            }
            _ => {
                // Logout is a tiny, private file write plus an in-process
                // edge. Do it on this input tick so the native loop observes
                // the edge before it parks. The other actions may block on
                // logind/GDM and therefore remain on workers.
                if action == os_actions::SessionAction::Logout {
                    if let Err(error) = os_actions::request_logout() {
                        let _ = self.session_action_channel.0.send(SessionActionResult {
                            action,
                            result: Err(error),
                        });
                    }
                    return;
                }
                let results = self.session_action_channel.0.clone();
                std::thread::spawn(move || {
                    let result = os_actions::perform_session_action(action);
                    let _ = results.send(SessionActionResult { action, result });
                });
            }
        }
    }
}
