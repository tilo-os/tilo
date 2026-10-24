//! Portal dialogs, the file picker and the permission prompt.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Attach the portal's queues so the per-frame pumps have something
    /// to drain. Called by the compositor once, at startup, after
    /// `portal::serve` succeeds.
    pub fn set_portal_queues(
        &mut self,
        prompts: crate::portal::PromptQueueHandle,
        pickers: crate::portal::FilePickerQueue,
    ) {
        self.portal_prompts = Some(prompts);
        self.file_picker_overlay.set_queue(pickers);
    }

    /// The live file-chooser dialog, if one is up. For the frame
    /// builder's widget walk and for tests.
    #[cfg(test)]
    pub(crate) fn file_picker_actions_for_test(
        &self,
    ) -> crate::shell::file_picker_overlay::actions::PickerActions {
        self.file_picker_overlay.actions()
    }

    /// What the open file chooser holds back from the windows: the
    /// keyboard while its sheet's window is focused, the pointer over that
    /// window and the sheet.
    pub(crate) fn file_picker_capture(&self) -> crate::shell::input_policy::InputCapture {
        self.file_picker_overlay.capture(self.file_picker_place())
    }

    pub fn file_picker(&self) -> Option<&ui::widget::FilePicker> {
        self.file_picker_overlay.widget()
    }

    /// A portal sheet (permission, secrets, device choice) is on screen.
    pub fn portal_dialog_open(&self) -> bool {
        self.portal_dialogs.view().is_some()
    }

    pub fn file_picker_mut(&mut self) -> Option<&mut ui::widget::FilePicker> {
        self.file_picker_overlay.widget_mut()
    }

    /// Mounts queued file-chooser dialogs and resolves whichever one the
    /// user just finished. Returns whether anything changed, so the caller
    /// can repaint.
    pub fn pump_file_picker(&mut self) -> bool {
        self.file_picker_overlay.pump(self.chrome.as_ref())
    }

    /// The queue the compositor's sheets come from, for a harness that
    /// raises one the way a request from polkit or the network daemon does.
    pub fn portal_dialog_queue(&self) -> crate::shell::portal_dialog::PortalDialogQueue {
        self.portal_dialogs.queue().clone()
    }

    /// Latch the next portal-owned sheet. Its async reply remains held by the
    /// controller until an explicit confirm or cancel event.
    pub fn pump_portal_dialogs(&mut self) -> bool {
        self.portal_dialogs.pump()
    }

    /// The compositor-owned sheet on screen, for `state-dump`; masked
    /// values appear only as their length.
    pub fn portal_dialog_state_json(&self) -> serde_json::Value {
        self.portal_dialogs.state_json()
    }

    /// Moves buffered portal prompts onto the chrome's queue for
    /// `pump_permission_prompt`. Senders travel with them rather than being
    /// dropped — dropping one would answer deny before the user was asked.
    pub fn drain_portal_prompts(&mut self) -> bool {
        let Some(prompts) = self.portal_prompts.as_ref() else {
            return false;
        };
        let drained = prompts.drain();
        if drained.is_empty() {
            return false;
        }
        let Some(chrome_handle) = self.chrome.clone() else {
            // No chrome to render a prompt — answer deny now rather than
            // hold senders that would never be fulfilled.
            for pending in drained {
                let _ = pending.reply.send(crate::portal::PromptOutcome::Deny);
            }
            return false;
        };
        let mut st = chrome_handle.borrow_mut();
        for pending in drained {
            st.pending_grants
                .push_back(crate::shell::chrome_state::PendingPrompt::with_reply(
                    pending.caller.app_id.clone(),
                    pending.capability,
                    pending.reply,
                ));
        }
        st.bump();
        true
    }

    /// Test-only: runs the per-frame permission-prompt pump outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_permission_prompt_for_tests(&mut self) {
        self.pump_permission_prompt();
    }

    /// Geometry `draw_desktop_extras` gives the live permission prompt.
    /// Where the permission prompt goes: across the top of the window of
    /// the app that asks (the one showing it, else any holding it), where the
    /// person is looking when it asks, at least
    /// [`PROMPT_MIN_WIDTH`] wide and kept on that window's display; when the
    /// app has no window on screen, across the top of the display in use.
    pub(super) fn permission_prompt_surface(&self) -> Rect {
        let inset = f64::from(self.chrome_top_y);
        let asking = self.permission_prompt_app.as_deref().and_then(|app| {
            let group = {
                let st = self.chrome.as_ref()?.borrow();
                let tree = st.tree();
                crate::shell::placement::group_showing(tree.desktop.active_workspace()?, app)?
            };
            self.group_hit_candidates()
                .into_iter()
                .find(|(_, id, _)| *id == group)
                .map(|(_, _, rect)| rect.intersect(self.rect))
                .filter(|rect| rect.area() > 0.0)
        });
        let Some(window) = asking else {
            let area = self.overlay_area();
            let bar_top = area.y0 + inset;
            return Rect::new(
                area.x0 + inset,
                bar_top,
                area.x1 - inset,
                bar_top + PROMPT_HEIGHT,
            );
        };
        let outputs = self.layer_outputs();
        let display = self.layer_geometry(&outputs).display_for(Some(window));
        let room = (display.width() - 2.0 * inset).max(1.0);
        let width = (window.width() - 2.0 * inset)
            .max(PROMPT_MIN_WIDTH)
            .min(room);
        let x0 = (window.center().x - width / 2.0)
            .min(display.x1 - inset - width)
            .max(display.x0 + inset);
        let y0 = (window.y0 + inset)
            .max(display.y0 + inset)
            .min(display.y1 - PROMPT_HEIGHT);
        Rect::new(x0, y0, x0 + width, y0 + PROMPT_HEIGHT)
    }

    /// Seal bands for trusted surfaces that are on screen. Empty when none
    /// are up. Only [`crate::shell::frame::FrameBuilder::widget_registry`]
    /// publishes these names; the widget walk cannot.
    pub(crate) fn trusted_frame_targets(
        &self,
        theme: &ui::theme::Theme,
    ) -> Vec<(&'static str, Rect)> {
        let mut targets = Vec::new();
        let thickness = f64::from(theme.tokens.spacing.s1);
        if self.permission_prompt.is_some() {
            let surface = self.permission_prompt_surface();
            targets.push((
                crate::shell::trusted_ui::PERMISSION_PROMPT_TARGET,
                crate::shell::trusted_ui::seal_band(
                    surface,
                    thickness,
                    f64::from(theme.tokens.radii.control),
                ),
            ));
        }
        if self.layers.dictation.is_consent() {
            let outputs = self.layer_outputs();
            let surface = self
                .layers
                .dictation
                .surface_rect(self.layer_geometry(&outputs));
            targets.push((
                crate::shell::trusted_ui::DICTATION_PROMPT_TARGET,
                crate::shell::trusted_ui::seal_band(
                    surface,
                    thickness,
                    f64::from(theme.tokens.radii.control),
                ),
            ));
        }
        if let Some(layout) = self.portal_dialogs.layout(self.overlay_area()) {
            let radius = ui::chrome::ChromeRole::Panel.radius(theme, layout.sheet.height());
            targets.push((
                crate::shell::trusted_ui::PORTAL_DIALOG_TARGET,
                crate::shell::trusted_ui::seal_band(layout.sheet, thickness, radius),
            ));
        }
        targets
    }

    /// Named hit targets for the visible prompt’s existing one-shot consent choices.
    #[cfg(any(test, debug_assertions))]
    pub fn permission_prompt_choice_rects_for_tests(&self) -> Vec<(&'static str, Rect)> {
        self.permission_prompt
            .as_ref()
            .map_or_else(Vec::new, |prompt| {
                vec![
                    ("permission_prompt_allow_once", prompt.allow_rect()),
                    ("permission_prompt_deny", prompt.deny_rect()),
                ]
            })
    }

    /// Test-only accessor for the held [`PermissionPrompt`].
    #[cfg(any(test, debug_assertions))]
    pub fn permission_prompt_mut_for_tests(&mut self) -> Option<&mut PermissionPrompt> {
        self.permission_prompt.as_mut()
    }
}

/// Map the prompt widget's choice onto the portal's outcome vocabulary.
/// Two enums for one decision because they belong to different layers;
/// keeping the mapping in one place stops them drifting.
pub(crate) fn prompt_choice_to_outcome(choice: PromptChoice) -> crate::portal::PromptOutcome {
    use crate::portal::PromptOutcome;
    match choice {
        PromptChoice::Allow => PromptOutcome::AllowOnce,
        PromptChoice::Session => PromptOutcome::AllowSession,
        PromptChoice::Always => PromptOutcome::AllowAlways,
        PromptChoice::Deny => PromptOutcome::Deny,
    }
}

/// Human-readable label for the inline permission prompt from a
/// [`Capability`] payload.
/// What a pending prompt asks, as the person reads it.
pub(crate) fn prompt_label(request: &crate::shell::chrome_state::PromptRequest) -> String {
    match request {
        crate::shell::chrome_state::PromptRequest::Capability(capability) => {
            capability_label(capability)
        }
        crate::shell::chrome_state::PromptRequest::AgentAdmission(admission) => admission.label(),
        crate::shell::chrome_state::PromptRequest::DesktopControl(ask) => ask.label(),
    }
}

pub(crate) fn capability_label(cap: &Capability) -> String {
    match cap {
        Capability::Filesystem { path, mode, .. } => {
            let path = path.display().to_string();
            match mode {
                crate::shell::state::FsMode::Read => {
                    t_args("capability-filesystem-read", &[("path", path.into())])
                }
                crate::shell::state::FsMode::ReadWrite => {
                    t_args("capability-filesystem-read-write", &[("path", path.into())])
                }
            }
        }
        Capability::Camera { .. } => t("capability-camera"),
        Capability::Microphone { .. } => t("capability-microphone"),
        Capability::Screencast { .. } => t("capability-screencast"),
        Capability::RemoteDesktop { .. } => t("capability-remote-desktop"),
        Capability::Print { .. } => t("capability-print"),
        Capability::HostShell { .. } => t("capability-host-shell"),
        Capability::DesktopControl { .. } => t("capability-desktop-control"),
        Capability::SystemInfo { .. } => t("capability-system-info"),
        Capability::Processes { .. } => t("capability-processes"),
        Capability::ProcessCommands { .. } => t("capability-process-commands"),
        Capability::IoUring { .. } => t("capability-io-uring"),
        Capability::Network { hosts, .. } => match hosts {
            Some(h) if !h.is_empty() => t_args(
                "capability-network-hosts",
                &[("hosts", h.join(", ").into())],
            ),
            _ => t("capability-network"),
        },
    }
}
