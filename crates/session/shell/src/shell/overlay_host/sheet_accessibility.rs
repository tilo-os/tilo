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

/// Give every keyed node of the Wi-Fi prompt an action target carrying its
/// key path and the prompt it belongs to, so an action aimed at one prompt
/// can never type into or submit another, even for the same network.
fn route_wifi_prompt(children: &mut [AccessNode], prompt: u64, path: &mut Vec<u64>) {
    for child in children {
        let Some(key) = child.key else {
            continue;
        };
        path.push(key);
        let encoded = path
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join("/");
        let target = format!("{WIFI_PASSWORD_PREFIX}{encoded}#{prompt}");
        child.semantic_key = Some(target.clone());
        child.action_target = Some(target);
        route_wifi_prompt(&mut child.children, prompt, path);
        path.pop();
    }
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
            || self.file_picker_owns_keyboard()
            || self.peer_sync_overlay.visible()
            || self.directory_sheet.is_open()
            || self.wifi_password_prompt_up()
    }

    /// Whether the file chooser takes the keys: a sheet does while its
    /// window is focused, a chooser no window asked for always does.
    fn file_picker_owns_keyboard(&self) -> bool {
        use crate::shell::file_picker_overlay::PickerPlace;
        self.file_picker_overlay.widget().is_some()
            && match self.file_picker_place() {
                PickerPlace::Window { focused, .. } => focused,
                PickerPlace::Display(_) => true,
                PickerPlace::Hidden => false,
            }
    }

    pub(super) fn wifi_password_prompt_up(&self) -> bool {
        self.overlay_flags().quick_settings
            && self.chrome.as_ref().is_some_and(|chrome| {
                let state = chrome.borrow();
                state.quick_settings.wifi_password.is_some()
                    && state.quick_settings.current_level()
                        == &crate::shell::quick_settings::QsLevel::Root
                    && state.quick_settings.section
                        == Some(crate::shell::quick_settings::QsSection::Wifi)
            })
    }

    /// The modal sheets over every overlay, bottom to top as painted.
    /// While one is up, only it claims focus: the overlays under it keep
    /// their nodes but give up their focused marks.
    pub(super) fn append_modal_sheet_accessibility(&self, root: &mut AccessNode) {
        let mut sheets = Vec::new();
        let mut waiting = None;
        sheets.extend(self.directory_sheet.accessibility());
        if self.file_picker_place().dialog_rect().is_some()
            && let Some(mut picker) = self
                .file_picker_overlay
                .widget()
                .and_then(ui::widget::Widget::accessibility)
        {
            route_file_picker(&mut picker, &[]);
            // A sheet on a window in the background is there to be found,
            // not where the keys go.
            if self.file_picker_owns_keyboard() {
                sheets.push(picker);
            } else {
                clear_focus(&mut picker);
                waiting = Some(picker);
            }
        }
        sheets.extend(self.portal_dialogs.accessibility(self.rect));
        root.children.extend(waiting);
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
    /// notification banner, the hardware-key readout, a passing
    /// acknowledgement and a held pad's mapping, each a live region.
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
        if let Some(ack) = self.current_acknowledgement(ui::time::now()) {
            nodes.push(ack.accessibility());
        }
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
    /// last failure as live text, and its buttons — the shared password
    /// prompt's own parts, in a dialog named by the network.
    pub(super) fn wifi_password_accessibility(&self, prompt: &WifiPasswordPrompt) -> AccessNode {
        let mut children = self.wifi_prompt.access_node().children;
        route_wifi_prompt(&mut children, prompt.id, &mut Vec::new());
        AccessNode::new(Role::Dialog)
            .with_name(prompt.ssid.clone())
            .with_description(t("qs-wifi-protected"))
            .with_bounds(self.qs_password_rect())
            .with_state(AccessState::new().modal(true).busy(prompt.connecting))
            .with_children(children)
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
        if crate::shell::portal_dialog::accessibility::is_sheet_target(target) {
            return Some(
                self.portal_dialogs
                    .perform_accessibility_action(target, action),
            );
        }
        if crate::shell::directory_sheet::is_accessibility_target(target) {
            let Some(outcome) = self
                .directory_sheet
                .perform_accessibility_action(target, action)
            else {
                return Some(false);
            };
            if let Some(request) = outcome.request {
                self.run_directory_request(request);
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
        let Some((path, id)) = rest.split_once('#') else {
            return false;
        };
        let Ok(path) = path
            .split('/')
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
        else {
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
            .is_some_and(|prompt| prompt.id.to_string() == id);
        if !current || !self.wifi_password_prompt_up() || !self.sync_wifi_password_input() {
            return false;
        }
        if matches!(action, Action::ScrollIntoView) {
            return true;
        }
        // The prompt's field is local: the scratch document is never
        // written and never leaves this call.
        let mut scratch = automerge::AutoCommit::new();
        let applied = self.wifi_prompt.perform_access_action(
            action,
            &path,
            &mut ui::widget::Edits::lend(&mut scratch),
            &automerge::ROOT,
        );
        if self.wifi_prompt.error().is_none() {
            let mut state = chrome.borrow_mut();
            if let Some(prompt) = state.quick_settings.wifi_password.as_mut() {
                // Typing clears a rejection message; it was about the old
                // value.
                prompt.error = None;
            }
            state.bump();
        }
        self.settle_wifi_prompt();
        applied
    }
}
