//! Shortcut configuration, the shortcut guide's focus and Print Screen arbitration.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Capture a chord for the shortcut editor.  Validation is shared with
    /// Settings and persisted loading, so capture cannot create an effective
    /// table the compositor would later reject or repair.
    pub(super) fn capture_shortcut_chord(
        &mut self,
        action: settings_store::hardware::ShortcutAction,
        logical_key: &Key,
        modifiers: winit::event::Modifiers,
    ) {
        let Some(chord) = shortcut_chord_from_event(logical_key, modifiers) else {
            return;
        };
        if matches!(logical_key, Key::Character(_)) && chord.modifiers.is_empty() {
            self.shortcut_rebind_message = Some(t("guide-rebind-needs-modifier"));
            return;
        }
        let mut config = self.shortcut_config.clone();
        if let Err(error) = config.set(action, chord) {
            self.shortcut_rebind_message = Some(match error {
                settings_store::hardware::ShortcutConfigError::Conflict(other) => t_args(
                    "guide-rebind-in-use",
                    &[("action", shortcut_action_label(other).into())],
                ),
                settings_store::hardware::ShortcutConfigError::ReservedSystemKey => {
                    t("guide-rebind-reserved")
                }
                settings_store::hardware::ShortcutConfigError::Fixed(id) => t_args(
                    "guide-rebind-in-use",
                    &[(
                        "action",
                        settings_store::keyboard_shortcuts::fixed_shortcut(id)
                            .map_or_else(|| id.to_string(), |fixed| t(fixed.label_key))
                            .into(),
                    )],
                ),
                settings_store::hardware::ShortcutConfigError::InvalidChord => {
                    t("guide-rebind-unsupported")
                }
                settings_store::hardware::ShortcutConfigError::TakenByAction => {
                    t("guide-rebind-taken-by-action")
                }
            });
            return;
        }
        match if self.desktop_controls.is_explicit() {
            config
                .validate()
                .map_err(|error| std::io::Error::other(format!("{error:?}")))
        } else {
            config.save()
        } {
            Ok(()) => {
                self.shortcut_config = config;
                self.refresh_nested_shortcuts();
                self.shortcut_rebind = None;
                self.shortcut_rebind_message = None;
            }
            Err(_) => {
                self.shortcut_rebind_message = Some(t("guide-rebind-save-failed"));
            }
        }
    }

    /// Replace the effective shortcut table after Settings has accepted a
    /// change.  The compositor's foreign-client pre-routing reads this
    /// snapshot, so dispatch and client ownership cannot disagree.
    ///
    /// Settings storage has no change-notification transport today: an
    /// external writer must make this validated handoff after saving, or its
    /// file change takes effect at the next compositor session.  The shell's
    /// own shortcut guide does the handoff synchronously after its save.
    pub fn set_shortcut_config(
        &mut self,
        config: settings_store::hardware::ShortcutConfig,
    ) -> Result<(), settings_store::hardware::ShortcutConfigError> {
        config.validate()?;
        self.shortcut_config = settings_store::hardware::ShortcutConfig::normalise(config);
        self.refresh_nested_shortcuts();
        Ok(())
    }

    /// The effective shortcut table. Pre-routing checks that run for every
    /// input event (the session-lock chord) read this snapshot instead of
    /// settings storage.
    pub(crate) fn shortcut_config(&self) -> &settings_store::hardware::ShortcutConfig {
        self.nested_shortcuts
            .as_ref()
            .unwrap_or(&self.shortcut_config)
    }

    /// Replace the cached desktop-control preferences after settings
    /// storage changed. A host constructed with explicit preferences keeps
    /// them: it does not follow the settings file.
    pub fn set_desktop_controls_config(
        &mut self,
        config: settings_store::hardware::DesktopControlsConfig,
    ) {
        if let DesktopControlsSource::Persisted(cached) = &mut self.desktop_controls {
            *cached = config;
        }
    }

    /// Resolve a press against the effective user-edited shortcut table.
    ///
    /// The compositor asks before offering a press to a focused Wayland
    /// client, and the shell dispatcher uses the returned action.  This is
    /// deliberately one resolver over the same cached table, including any
    /// custom binding accepted in Settings.
    pub(crate) fn configured_shortcut_action(
        &self,
        logical_key: &Key,
        modifiers: winit::event::Modifiers,
    ) -> Option<settings_store::hardware::ShortcutAction> {
        // The windowed defaults add usable entry points, rather than removing
        // the usual chords when a parent does forward them (or input is injected).
        self.nested_shortcuts
            .as_ref()
            .and_then(|config| configured_shortcut_action(config, logical_key, modifiers))
            .or_else(|| configured_shortcut_action(&self.shortcut_config, logical_key, modifiers))
    }

    pub(super) fn reset_shortcut_pair(
        &mut self,
        first: settings_store::hardware::ShortcutAction,
        second: settings_store::hardware::ShortcutAction,
    ) {
        let defaults = settings_store::hardware::ShortcutConfig::default();
        let mut candidate = self.shortcut_config.clone();
        candidate.bindings.remove(&first);
        candidate.bindings.remove(&second);
        let restore = [first, second].into_iter().try_for_each(|action| {
            defaults
                .bindings
                .get(&action)
                .cloned()
                .ok_or(settings_store::hardware::ShortcutConfigError::InvalidChord)
                .and_then(|chord| candidate.set(action, chord))
        });
        if let Err(error) = restore {
            self.shortcut_rebind_message = Some(match error {
                settings_store::hardware::ShortcutConfigError::Conflict(other) => t_args(
                    "guide-reset-in-use",
                    &[("action", shortcut_action_label(other).into())],
                ),
                settings_store::hardware::ShortcutConfigError::TakenByAction => {
                    t("guide-reset-taken-by-action")
                }
                settings_store::hardware::ShortcutConfigError::ReservedSystemKey
                | settings_store::hardware::ShortcutConfigError::InvalidChord
                | settings_store::hardware::ShortcutConfigError::Fixed(_) => {
                    t("guide-reset-invalid")
                }
            });
            return;
        }
        match if self.desktop_controls.is_explicit() {
            candidate
                .validate()
                .map_err(|error| std::io::Error::other(format!("{error:?}")))
        } else {
            candidate.save()
        } {
            Ok(()) => {
                self.shortcut_config = candidate;
                self.refresh_nested_shortcuts();
                self.shortcut_rebind = None;
                self.shortcut_rebind_message = None;
            }
            Err(_) => {
                self.shortcut_rebind_message = Some(t("guide-reset-save-failed"));
            }
        }
    }

    #[allow(
        clippy::unused_self,
        reason = "kept as a host helper alongside the focus state it describes"
    )]
    pub(super) fn shortcut_guide_controls(&self) -> Vec<ShortcutGuideFocus> {
        let config = &self.shortcut_config;
        let defaults = settings_store::hardware::ShortcutConfig::default();
        let mut controls = Vec::with_capacity(22);
        for (_, first, second) in SHORTCUT_GUIDE_ROWS {
            controls.push(ShortcutGuideFocus::Binding(first));
            controls.push(ShortcutGuideFocus::Binding(second));
            if [first, second]
                .into_iter()
                .any(|action| config.bindings.get(&action) != defaults.bindings.get(&action))
            {
                controls.push(ShortcutGuideFocus::Reset(first, second));
            }
        }
        controls.push(ShortcutGuideFocus::Done);
        controls
    }

    pub(super) fn move_shortcut_guide_focus(&mut self, backwards: bool) {
        // Focus walks the editable pairs at the top of the body; bring them
        // back into view rather than focusing something scrolled away.
        self.shortcut_guide_scroll = 0.0;
        let controls = self.shortcut_guide_controls();
        let current = self
            .shortcut_guide_focus
            .and_then(|focus| controls.iter().position(|candidate| *candidate == focus));
        let next = match (current, backwards) {
            (Some(0) | None, true) => controls.len().saturating_sub(1),
            (Some(index), true) => index.saturating_sub(1),
            (Some(index), false) => index
                .saturating_add(1)
                .checked_rem(controls.len())
                .unwrap_or_default(),
            (None, false) => 0,
        };
        self.shortcut_guide_focus = controls.get(next).copied();
    }

    /// The role and revoke sheets are genuinely modal within Workspace
    /// Settings: arrows move the visible choice, Enter (or Space) chooses it,
    /// and Escape abandons it. All other keys are consumed so a shortcut or
    /// focused document cannot change behind a permission decision.
    pub(super) fn handle_member_permission_sheet_keyboard(
        &mut self,
        logical_key: &Key,
        handled: &mut bool,
    ) -> bool {
        if !self.workspace_settings_overlay.member_sheet_is_open() {
            return false;
        }
        let key = match logical_key {
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowLeft) => Some(MemberSheetKey::Previous),
            Key::Named(NamedKey::ArrowDown | NamedKey::ArrowRight | NamedKey::Tab) => {
                Some(MemberSheetKey::Next)
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => Some(MemberSheetKey::Activate),
            Key::Named(NamedKey::Escape) => Some(MemberSheetKey::Cancel),
            _ => None,
        };
        if let Some(key) = key {
            let outcome = self.workspace_settings_overlay.handle_member_sheet_key(key);
            if let Some(outcome) = outcome {
                let is_owner = self
                    .chrome
                    .as_ref()
                    .is_some_and(|chrome| is_active_workspace_owner(&chrome.borrow()));
                self.apply_workspace_settings_outcome(outcome, is_owner);
            } else if let Some(chrome) = self.chrome.as_ref() {
                chrome.borrow_mut().bump();
            }
        }
        *handled = true;
        true
    }

    /// The agent access view in Workspace Settings › Agents takes the
    /// keyboard while it is shown: Tab and Shift+Tab walk its controls, Left
    /// and Right change the focused answer, Enter or Space presses, and
    /// Escape goes back to the roster. Other keys keep their usual routes.
    pub(super) fn handle_agent_access_keyboard(
        &mut self,
        logical_key: &Key,
        shift_down: bool,
        handled: &mut bool,
    ) -> bool {
        let on_agents = self.chrome.as_ref().is_some_and(|chrome| {
            let state = chrome.borrow();
            state.overlays.ws_settings && state.ws_settings_tab == WsTab::Agents
        });
        if !on_agents || self.workspace_settings_overlay.reviewing_agent().is_none() {
            return false;
        }
        let key = match logical_key {
            Key::Named(NamedKey::Tab) if shift_down => AgentAccessKey::Previous,
            Key::Named(NamedKey::Tab | NamedKey::ArrowDown) => AgentAccessKey::Next,
            Key::Named(NamedKey::ArrowUp) => AgentAccessKey::Previous,
            Key::Named(NamedKey::ArrowLeft) => AgentAccessKey::Left,
            Key::Named(NamedKey::ArrowRight) => AgentAccessKey::Right,
            Key::Named(NamedKey::Enter | NamedKey::Space) => AgentAccessKey::Activate,
            Key::Named(NamedKey::Escape) => AgentAccessKey::Back,
            _ => return false,
        };
        if let Some(outcome) = self.workspace_settings_overlay.handle_agent_access_key(key) {
            let is_owner = self
                .chrome
                .as_ref()
                .is_some_and(|chrome| is_active_workspace_owner(&chrome.borrow()));
            self.apply_workspace_settings_outcome(outcome, is_owner);
        }
        self.bump_chrome();
        *handled = true;
        true
    }

    /// Decide the global screenshot shortcut before any modal gets a chance
    /// to consume it.  This is intentionally a single policy point shared by
    /// the early modal gate and ordinary keyboard routing: otherwise Print
    /// Screen can behave differently merely because a dialog happened to be
    /// first in the event-dispatch chain.
    pub(super) fn arbitrate_print_screen(&self) -> PrintScreenArbitration {
        if self.screenshot_markup.is_some() || self.capture_studio.is_open() {
            return PrintScreenArbitration::Blocked(
                "Finish the current capture before taking another screenshot.",
            );
        }
        PrintScreenArbitration::OpenCapture
    }

    /// Dismiss presentation-only chrome as one state transition before the
    /// capture selector opens. Transactional surfaces stay mounted below it:
    /// Capture Studio must be able to photograph a broken setup page, portal,
    /// permission prompt, or credential error and then return to it intact.
    pub(super) fn dismiss_transient_chrome_for_capture(&mut self) {
        self.palette_visible = false;
        self.palette.close();
        if self.shortcut_rebind.is_none() {
            self.shortcut_rebind_message = None;
        }
        self.shortcut_guide_focus = None;
        self.strip_navigator.close();
        let Some(chrome) = self.chrome.as_ref() else {
            return;
        };
        let mut state = chrome.borrow_mut();
        state.set_strip_switcher_open(false);
        state.overlays.shortcut_help = false;
        state.overlays.desktop_tour = false;
        state.overlays.overview = false;
        if state.quick_settings.wifi_password.is_none() {
            state.overlays.quick_settings = false;
        }
        if state.editing_workspace_name.is_none()
            && !self.workspace_settings_overlay.member_sheet_is_open()
            && state.pending_grants.is_empty()
        {
            state.overlays.ws_settings = false;
            state.overlays.collaborators = false;
            state.overlays.room_activity = false;
        }
        state.bump();
    }

    /// Apply the Print Screen policy. Existing capture work is the only flow
    /// that blocks a new capture; ordinary dialogs remain mounted below the
    /// temporary Capture Studio input owner.
    ///
    /// Shift+Print photographs the whole screen and Alt+Print the focused
    /// window at once, with no chooser: saved to the screenshots folder,
    /// copied, and announced by the same receipt a chooser capture leaves.
    /// Alt+Print with no window to photograph opens the chooser instead of
    /// doing nothing.
    pub(super) fn apply_print_screen_priority(
        &mut self,
        modifiers: winit::keyboard::ModifiersState,
    ) {
        match self.arbitrate_print_screen() {
            PrintScreenArbitration::OpenCapture => {
                self.dismiss_transient_chrome_for_capture();
                let instant = match (modifiers.shift_key(), modifiers.alt_key()) {
                    (true, false) => Some(instant_screen_capture(self.get_rect())),
                    (false, true) => self.focused_capture_target().map(instant_window_capture),
                    _ => None,
                };
                if let Some(intent) = instant {
                    self.instant_captures.push(intent);
                    self.bump_chrome();
                } else {
                    let _ = self.open_capture_studio(ui::widget::CaptureMode::Screenshot);
                }
            }
            PrintScreenArbitration::Blocked(reason) => {
                tracing::debug!(reason, "Print Screen deferred by modal workflow");
            }
        }
    }

    /// Returns `true` only for a Print Screen press.  It runs before modal
    /// event gates; [`Self::apply_print_screen_priority`] is also used by the
    /// normal keyboard dispatcher so the policy cannot drift.
    pub(super) fn handle_print_screen_priority(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::keyboard::ModifiersState,
        handled: &mut bool,
    ) -> bool {
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return false;
        };
        if key.state != ElementState::Pressed
            || !matches!(key.logical_key, Key::Named(NamedKey::PrintScreen))
        {
            return false;
        }
        if !key.repeat {
            self.apply_print_screen_priority(modifiers);
        }
        *handled = true;
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ShortcutGuideFocus {
    Binding(settings_store::hardware::ShortcutAction),
    Reset(
        settings_store::hardware::ShortcutAction,
        settings_store::hardware::ShortcutAction,
    ),
    Done,
}

/// The keyboard guide's navigation rows: a label and the previous/next pair
/// it edits. The guide paints these and Tab walks these, so a binding is
/// focusable exactly when it is drawn.
pub(super) const SHORTCUT_GUIDE_ROWS: [(
    &str,
    settings_store::hardware::ShortcutAction,
    settings_store::hardware::ShortcutAction,
); 9] = {
    use settings_store::hardware::ShortcutAction as A;
    [
        (
            "guide-row-windows",
            A::FocusPreviousWindow,
            A::FocusNextWindow,
        ),
        ("guide-row-tabs", A::PreviousTab, A::NextTab),
        ("guide-row-move-tab", A::AttachTabLeft, A::AttachTabRight),
        ("guide-row-maximize", A::MaximizeWindow, A::RestoreWindow),
        ("guide-row-strips", A::PreviousStrip, A::NextStrip),
        (
            "guide-row-workspaces",
            A::PreviousWorkspace,
            A::NextWorkspace,
        ),
        ("guide-row-move-group", A::MoveGroupLeft, A::MoveGroupRight),
        ("guide-row-resize", A::ResizeNarrower, A::ResizeWider),
        (
            "guide-row-move-group-strip",
            A::MoveGroupToPreviousStrip,
            A::MoveGroupToNextStrip,
        ),
    ]
};

/// The result of asking whether the global Print Screen shortcut may change
/// the desktop's modal stack. Capture is a temporary input owner: it may sit
/// above an OOBE, portal, consent, or file-selection dialog, but it must not
/// mutate that dialog's state. Escaping capture returns to the exact workflow
/// the person was inspecting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PrintScreenArbitration {
    /// Close only presentation-only chrome and enter Capture Studio.
    OpenCapture,
    /// Do not stack a second capture over an in-progress capture/markup flow.
    Blocked(&'static str),
}

/// Where the desktop-control preferences come from. The persisted variant
/// caches the last snapshot read from settings storage; the compositor
/// refreshes it at its settings reload point, so key dispatch and paint
/// never read the file.
pub(super) enum DesktopControlsSource {
    Persisted(settings_store::hardware::DesktopControlsConfig),
    Explicit(settings_store::hardware::DesktopControlsConfig),
}

impl DesktopControlsSource {
    pub(super) fn config(&self) -> settings_store::hardware::DesktopControlsConfig {
        match self {
            Self::Persisted(config) | Self::Explicit(config) => *config,
        }
    }

    pub(super) fn is_explicit(&self) -> bool {
        matches!(self, Self::Explicit(_))
    }
}
