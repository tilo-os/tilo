//! Shell sheets, prompts and live status in the published semantic tree.
//!
//! The modal sheets painted over every overlay — the portal and polkit
//! sheet, the network-secrets sheet, the file chooser and the invite/join
//! sheet — each publish a dialog with roles, names, focus and state, and
//! are operated through action targets they name themselves, as the Quick
//! Settings panel's nodes are. Secrets are published as password text
//! without their value.
//!
//! Sheet nodes follow the per-overlay children of the host's semantic root,
//! so the ordinal paths those overlays route by are unchanged.

use ui::a11y::{AccessNode, Action, Politeness, Role};

use super::{ShellOverlayHost, fullscreen_hint, t};

const QUICK_SETTINGS_PREFIX: &str = "quick-settings:";
const FILE_PICKER_PREFIX: &str = "file-picker:";

/// Give every node of the chooser's own semantic tree an action target
/// carrying its key path, which is what the chooser resolves actions by.
fn route_file_picker(node: &mut AccessNode, parent: &[u64]) {
    route_keyed(node, parent, FILE_PICKER_PREFIX);
}

/// Give every node of the Quick Settings panel's tree an action target
/// carrying its key path, which is what the panel resolves actions by: a
/// screen reader sets a slider or turns a switch through the panel's own
/// controls. A control the panel names keeps its name, so it is reached by
/// it as it is in the panel's own window.
pub(super) fn route_quick_settings(node: &mut AccessNode) {
    route_keyed_unnamed(node, &[], QUICK_SETTINGS_PREFIX);
}

fn route_keyed_unnamed(node: &mut AccessNode, parent: &[u64], prefix: &str) {
    let mut path = parent.to_vec();
    if let Some(key) = node.key {
        path.push(key);
    }
    let named = node
        .action_target
        .as_deref()
        .is_some_and(|target| !target.starts_with("tilo.key-action:"));
    if !named && !path.is_empty() {
        let encoded = path
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join("/");
        let target = format!("{prefix}{encoded}");
        node.semantic_key = Some(target.clone());
        node.action_target = Some(target);
    }
    for child in &mut node.children {
        route_keyed_unnamed(child, &path, prefix);
    }
}

fn route_keyed(node: &mut AccessNode, parent: &[u64], prefix: &str) {
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
        let target = format!("{prefix}{encoded}");
        node.semantic_key = Some(target.clone());
        node.action_target = Some(target);
    }
    for child in &mut node.children {
        route_keyed(child, &path, prefix);
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

    /// The modal sheets over every overlay, bottom to top as painted.
    /// While one is up, only it claims focus: the overlays under it keep
    /// their nodes but give up their focused marks.
    pub(super) fn append_modal_sheet_accessibility(&self, root: &mut AccessNode) {
        let mut sheets = Vec::new();
        let mut waiting = None;
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
        if self.chrome.is_some()
            && let Some(node) = self.banner_accessibility()
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
        if self
            .fullscreen_hint_accessibility(ui::time::now())
            .is_some()
        {
            nodes.push(fullscreen_hint::hint_node());
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
        if self.quick_settings_publishes(target) {
            let app = self.quick_settings.app_mut()?;
            return Some(ui::widget::Widget::perform_semantic_action(
                app.as_mut(),
                target,
                action,
                state,
                path,
            ));
        }
        let encoded = target.strip_prefix(QUICK_SETTINGS_PREFIX)?;
        let Ok(keys) = encoded
            .split('/')
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
        else {
            return Some(false);
        };
        let Some(app) = self.quick_settings.app_mut() else {
            return Some(false);
        };
        Some(match action {
            Action::ScrollIntoView => true,
            _ => ui::widget::Widget::perform_action(app.as_mut(), action, &keys, state, path),
        })
    }
}
