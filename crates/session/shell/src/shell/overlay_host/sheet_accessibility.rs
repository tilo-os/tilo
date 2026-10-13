//! Shell sheets, prompts and live status in the published semantic tree.
//!
//! The modal sheets painted over every overlay — the portal and polkit
//! sheet, the network-secrets sheet, the file chooser and the invite/join
//! sheet — and the Quick Settings Wi-Fi password prompt each publish a
//! dialog with roles, names, focus and state, and are operated through
//! action targets they name themselves. Secrets are published as password
//! text without their value.
//!
//! Sheet nodes follow the per-overlay children of the host's semantic root,
//! so the ordinal paths those overlays route by are unchanged.

use ui::a11y::{AccessNode, AccessState, Action, Politeness, Role};

use super::{ChromeNotifications, ShellOverlayHost, t};
use crate::shell::quick_settings::WifiPasswordPrompt;

const WIFI_PASSWORD_PREFIX: &str = "wifi-password:";
const FILE_PICKER_PREFIX: &str = "file-picker:";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WifiPasswordPart {
    Back,
    Field,
    Cancel,
    Connect,
}

impl WifiPasswordPart {
    const fn slug(self) -> &'static str {
        match self {
            Self::Back => "back",
            Self::Field => "field",
            Self::Cancel => "cancel",
            Self::Connect => "connect",
        }
    }

    fn from_slug(slug: &str) -> Option<Self> {
        [Self::Back, Self::Field, Self::Cancel, Self::Connect]
            .into_iter()
            .find(|part| part.slug() == slug)
    }
}

/// Named by the prompt as well as the part, so an action aimed at one
/// prompt can never type into or submit another, even for the same network.
fn wifi_password_target(prompt: u64, part: WifiPasswordPart) -> String {
    format!("{WIFI_PASSWORD_PREFIX}{}#{prompt}", part.slug())
}

/// Give every node of the chooser's own semantic tree an action target
/// carrying its key path, which is what the chooser resolves actions by.
fn route_file_picker(node: &mut AccessNode, parent: &[u64]) {
    let mut path = parent.to_vec();
    if let Some(key) = node.key {
        path.push(key);
    }
    if !path.is_empty() {
        let encoded = path
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join("/");
        let target = format!("{FILE_PICKER_PREFIX}{encoded}");
        node.semantic_key = Some(target.clone());
        node.action_target = Some(target);
    }
    for child in &mut node.children {
        route_file_picker(child, &path);
    }
}

fn clear_focus(node: &mut AccessNode) {
    node.state.focused = false;
    for child in &mut node.children {
        clear_focus(child);
    }
}

impl ShellOverlayHost {
    /// Whether a sheet that takes every key is up, so assistive focus
    /// belongs inside it rather than on whatever the desktop last focused.
    pub fn modal_sheet_owns_keyboard(&self) -> bool {
        self.portal_dialogs.view().is_some()
            || self.file_picker_overlay.widget().is_some()
            || self.peer_sync_overlay.visible()
            || self.wifi_password_prompt_up()
    }

    fn wifi_password_prompt_up(&self) -> bool {
        self.overlay_flags().quick_settings
            && self.chrome.as_ref().is_some_and(|chrome| {
                let state = chrome.borrow();
                state.quick_settings.wifi_password.is_some()
                    && matches!(
                        state.quick_settings.current_level(),
                        crate::shell::quick_settings::QsLevel::Wifi
                            | crate::shell::quick_settings::QsLevel::WifiDetails(_)
                    )
            })
    }

    /// The modal sheets over every overlay, bottom to top as painted.
    /// While one is up, only it claims focus: the overlays under it keep
    /// their nodes but give up their focused marks.
    pub(super) fn append_modal_sheet_accessibility(&self, root: &mut AccessNode) {
        let mut sheets = Vec::new();
        sheets.extend(self.peer_sync_overlay.accessibility());
        if let Some(mut picker) = self
            .file_picker_overlay
            .widget()
            .and_then(ui::widget::Widget::accessibility)
        {
            route_file_picker(&mut picker, &[]);
            sheets.push(picker);
        }
        sheets.extend(self.portal_dialogs.accessibility(self.rect));
        if sheets.is_empty() {
            return;
        }
        for child in &mut root.children {
            clear_focus(child);
        }
        // Only the topmost sheet has the keyboard.
        let top = sheets.len().saturating_sub(1);
        for sheet in sheets.iter_mut().take(top) {
            clear_focus(sheet);
        }
        root.children.extend(sheets);
    }

    /// Announcements that are not part of any overlay: the newest
    /// notification banner, the hardware-key readout and a held pad's
    /// mapping, each a live region.
    pub(super) fn live_status_accessibility(&self) -> Vec<AccessNode> {
        let mut nodes = Vec::new();
        if let Some(chrome) = self.chrome.as_ref()
            && let Some(node) = self
                .notification_stack
                .accessibility(&ChromeNotifications::new(chrome))
        {
            nodes.push(node);
        }
        let readout = self.chrome.as_ref().and_then(|chrome| {
            chrome
                .borrow()
                .osd
                .filter(|osd| osd.is_live(std::time::Instant::now()))
                .map(|osd| osd.caption())
        });
        nodes.push(
            AccessNode::new(Role::StaticText)
                .with_name(t("a11y-status-readout"))
                .with_live(Politeness::Polite)
                .with_value(readout.unwrap_or_default()),
        );
        let pad_map = self.chrome.as_ref().and_then(|chrome| {
            chrome
                .borrow()
                .pad_map
                .as_ref()
                .map(|map| (map.name.clone(), map.narration()))
        });
        if let Some((name, narration)) = pad_map {
            nodes.push(
                AccessNode::new(Role::StaticText)
                    .with_name(name)
                    .with_live(Politeness::Polite)
                    .with_value(narration),
            );
        }
        nodes
    }

    /// The Quick Settings prompt for a protected network's password: the
    /// network it joins, the password as password text, progress and the
    /// last failure as live text, and its buttons.
    pub(super) fn wifi_password_accessibility(&self, prompt: &WifiPasswordPrompt) -> AccessNode {
        let target = |part| wifi_password_target(prompt.id, part);
        let (cancel, connect) = self.qs_password_button_rects();
        let mut children = vec![
            AccessNode::new(Role::Button)
                .with_name(t("qs-back"))
                .with_action_target(target(WifiPasswordPart::Back))
                .with_bounds(self.qs_back_chevron_rect())
                .with_state(AccessState::new().focusable(true)),
            self.wifi_password_field_accessibility(prompt)
                .with_action_target(target(WifiPasswordPart::Field)),
        ];
        if let Some(error) = prompt.error.as_ref() {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(error.clone())
                    .with_live(Politeness::Assertive),
            );
        } else if prompt.connecting {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(t("qs-connecting"))
                    .with_live(Politeness::Polite),
            );
        }
        children.push(
            AccessNode::new(Role::Button)
                .with_name(t("portal-cancel"))
                .with_action_target(target(WifiPasswordPart::Cancel))
                .with_bounds(cancel)
                .with_state(AccessState::new().focusable(true)),
        );
        children.push(
            AccessNode::new(Role::Button)
                .with_name(if prompt.connecting {
                    t("qs-connecting")
                } else {
                    t("portal-connect")
                })
                .with_action_target(target(WifiPasswordPart::Connect))
                .with_bounds(connect)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .disabled(!prompt.can_submit(self.wifi_password_input.value())),
                ),
        );
        AccessNode::new(Role::Dialog)
            .with_name(prompt.ssid.clone())
            .with_description(t("qs-wifi-protected"))
            .with_bounds(self.quick_settings_rect())
            .with_state(AccessState::new().modal(true).busy(prompt.connecting))
            .with_children(children)
    }

    /// The passphrase field as the field itself publishes it — password
    /// text, never its value, whether or not it is shown on screen — placed
    /// where it is painted.
    fn wifi_password_field_accessibility(&self, prompt: &WifiPasswordPrompt) -> AccessNode {
        let mut field = ui::widget::Widget::accessibility(&self.wifi_password_input)
            .unwrap_or_else(|| AccessNode::new(Role::TextInput));
        field.value = None;
        field.text_selection = None;
        field.state = AccessState {
            protected: true,
            required: true,
            invalid: prompt.error.is_some(),
            read_only: prompt.connecting,
            busy: prompt.connecting,
            ..field.state.focusable(true).focused(true)
        };
        field
            .with_name(t("portal-network-password"))
            .with_bounds(self.qs_password_field_rect())
    }

    /// Route an action aimed at a sheet's node. `None` when the target is
    /// not a sheet's; `Some(handled)` otherwise, including for a target
    /// from a sheet that has since closed, which changes nothing.
    pub(super) fn perform_sheet_action(
        &mut self,
        target: &str,
        action: &Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ui::widget::ModelPath,
    ) -> Option<bool> {
        if target == "task-direction" {
            return Some(self.room_activity.perform_task_direction_action(action));
        }
        if crate::shell::portal_dialog::accessibility::is_sheet_target(target) {
            return Some(
                self.portal_dialogs
                    .perform_accessibility_action(target, action),
            );
        }
        if crate::shell::peer_sync_overlay::is_accessibility_target(target) {
            let Some(outcome) = self
                .peer_sync_overlay
                .perform_accessibility_action(target, action)
            else {
                return Some(false);
            };
            if let Some(request) = outcome.request {
                self.run_peer_sync_request(&request, outcome.copy_result);
            }
            return Some(outcome.consumed);
        }
        if let Some(encoded) = target.strip_prefix(FILE_PICKER_PREFIX) {
            let Ok(keys) = encoded
                .split('/')
                .map(str::parse::<u64>)
                .collect::<Result<Vec<_>, _>>()
            else {
                return Some(false);
            };
            let Some(picker) = self.file_picker_overlay.widget_mut() else {
                return Some(false);
            };
            return Some(match action {
                Action::ScrollIntoView => true,
                _ => ui::widget::Widget::perform_action(picker, action, &keys, state, path),
            });
        }
        let rest = target.strip_prefix(WIFI_PASSWORD_PREFIX)?;
        Some(self.perform_wifi_password_action(rest, action))
    }

    fn perform_wifi_password_action(&mut self, rest: &str, action: &Action) -> bool {
        let Some((part, id)) = rest.split_once('#') else {
            return false;
        };
        let Some(part) = WifiPasswordPart::from_slug(part) else {
            return false;
        };
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let current = chrome
            .borrow()
            .quick_settings
            .wifi_password
            .as_ref()
            .map(|prompt| (prompt.id.to_string() == id, prompt.connecting));
        let (Some((true, connecting)), true) = (current, self.wifi_password_prompt_up()) else {
            return false;
        };
        match (part, action) {
            (_, Action::ScrollIntoView | Action::Focus)
            | (WifiPasswordPart::Field, Action::Activate) => true,
            (WifiPasswordPart::Back | WifiPasswordPart::Cancel, Action::Activate) => {
                self.cancel_wifi_password();
                true
            }
            (WifiPasswordPart::Connect, Action::Activate) => {
                self.sync_wifi_password_input();
                let password = self.wifi_password_input.value();
                let ready = chrome
                    .borrow()
                    .quick_settings
                    .wifi_password
                    .as_ref()
                    .is_some_and(|prompt| prompt.can_submit(password));
                if ready {
                    self.submit_wifi_password();
                }
                ready
            }
            (WifiPasswordPart::Field, Action::SetValue(value)) if !connecting => {
                if !self.sync_wifi_password_input() {
                    return false;
                }
                let typed: String = value.chars().filter(|ch| !ch.is_control()).collect();
                self.wifi_password_input.set_text(&typed);
                self.wifi_password_input.set_cursor_pos(typed.len());
                let mut state = chrome.borrow_mut();
                if let Some(prompt) = state.quick_settings.wifi_password.as_mut() {
                    // Typing clears a rejection message; it was about the
                    // old value.
                    prompt.error = None;
                }
                state.bump();
                true
            }
            _ => false,
        }
    }
}
