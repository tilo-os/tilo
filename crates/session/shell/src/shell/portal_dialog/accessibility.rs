//! The compositor-owned sheet as assistive technology sees and operates it.
//!
//! Every part of the painted sheet is published with the geometry the
//! controller hands the painter ([`PortalDialogController::layout`]), so a
//! screen reader's cursor lands where the eye does. Secrets are published as
//! password text: role, name and focus, never the value or its length.
//!
//! Each actionable node carries an action target naming the sheet's handle,
//! so an action an assistive technology aimed at an answered sheet can never
//! answer the next one in the queue.

use ui::a11y::{AccessNode, AccessState, Action, Politeness, Role};
use ui::widget::{TextInput, Widget};
use vello::kurbo::Rect;

use super::{DialogView, PendingDialog, PortalDialogController, PortalDialogQueue};
use crate::shell::i18n::{t, t_args};

const TARGET_PREFIX: &str = "portal-sheet:";

/// One operable part of the sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    /// A choice row: an app, an answer, an identity, a USB device or a
    /// shortcut binding.
    Choice(usize),
    /// The authentication sheet's secret.
    Secret,
    /// A network-secrets field.
    Field(usize),
    Remember,
    /// The launcher title, or a shortcut step's answer.
    Name,
    Icon(usize),
    Cancel,
    Confirm,
}

impl Part {
    fn encode(self) -> String {
        match self {
            Self::Choice(index) => format!("choice/{index}"),
            Self::Secret => "secret".into(),
            Self::Field(index) => format!("field/{index}"),
            Self::Remember => "remember".into(),
            Self::Name => "name".into(),
            Self::Icon(index) => format!("icon/{index}"),
            Self::Cancel => "cancel".into(),
            Self::Confirm => "confirm".into(),
        }
    }

    fn decode(part: &str) -> Option<Self> {
        let indexed = |prefix: &str| part.strip_prefix(prefix)?.parse::<usize>().ok();
        Some(match part {
            "secret" => Self::Secret,
            "remember" => Self::Remember,
            "name" => Self::Name,
            "cancel" => Self::Cancel,
            "confirm" => Self::Confirm,
            _ => {
                if let Some(index) = indexed("choice/") {
                    Self::Choice(index)
                } else if let Some(index) = indexed("field/") {
                    Self::Field(index)
                } else {
                    Self::Icon(indexed("icon/")?)
                }
            }
        })
    }
}

fn target(handle: &str, part: Part) -> String {
    format!("{TARGET_PREFIX}{}#{handle}", part.encode())
}

/// Whether `target` was published by a portal sheet, current or answered.
pub fn is_sheet_target(target: &str) -> bool {
    target.starts_with(TARGET_PREFIX)
}

fn focusable(focused: bool) -> AccessState {
    AccessState::new().focusable(true).focused(focused)
}

/// A sheet's text field as the field itself publishes it — a secret one as
/// password text without its value, even while shown on screen — named,
/// placed and focused as the sheet draws it.
fn field_node(
    input: &TextInput,
    name: String,
    target: String,
    bounds: Option<Rect>,
    state: AccessState,
) -> AccessNode {
    let mut node = Widget::accessibility(input).unwrap_or_else(|| AccessNode::new(Role::TextInput));
    let protected = node.state.protected;
    if protected {
        node.value = None;
        node.text_selection = None;
    }
    node.state = AccessState { protected, ..state };
    node = node.with_name(name).with_action_target(target);
    if let Some(bounds) = bounds {
        node = node.with_bounds(bounds);
    }
    node
}

/// Replace a field's whole value, as an assistive technology's SetValue
/// asks; control characters never enter a single-line field.
fn set_field(input: &mut TextInput, value: &str) {
    let typed: String = value.chars().filter(|ch| !ch.is_control()).collect();
    input.set_text(&typed);
    input.set_cursor_pos(typed.len());
}

impl PortalDialogQueue {
    /// PAM's prompt and the last attempt's notice, for the live sheet.
    fn authentication_prompt(&self, handle: &str) -> Option<(String, Option<String>)> {
        let inner = self.inner.lock().ok()?;
        match inner.pending.get(handle)? {
            PendingDialog::Authentication { request, .. } => Some((
                request
                    .prompt
                    .trim()
                    .trim_end_matches(':')
                    .trim()
                    .to_owned(),
                request.notice.clone().filter(|notice| !notice.is_empty()),
            )),
            _ => None,
        }
    }
}

impl PortalDialogController {
    /// The sheet on screen as one modal dialog, laid out in `bounds` like
    /// the painted one. `None` while no sheet is up.
    #[allow(
        clippy::too_many_lines,
        reason = "one projection covers every sheet shape so the published parts cannot drift from the painted ones"
    )]
    pub fn accessibility(&self, bounds: Rect) -> Option<AccessNode> {
        let view = self.view()?;
        let layout = self.layout(bounds)?;
        let handle = view.handle().to_owned();
        let target = |part| target(&handle, part);
        let mut children = vec![AccessNode::new(Role::StaticText).with_name(view.app_name())];

        // Typing goes to a text field whenever one is up; the highlighted
        // row only has the keyboard on a sheet without one.
        let text_focus = self.is_text_entry() || self.launcher_name_focused();
        let icon_focus = view
            .launcher_editor()
            .is_some_and(|editor| editor.editable_icon && !self.launcher_name_focused());
        let row_focus = !text_focus && !icon_focus;

        let choices = view.choices();
        let total = choices.len();
        for (index, choice) in choices.iter().enumerate() {
            let selected = index == self.selected;
            let mut node = AccessNode::new(Role::RadioButton)
                .with_name(choice.clone())
                .with_action_target(target(Part::Choice(index)))
                .with_position(index.saturating_add(1), total)
                .with_state(AccessState {
                    checked: Some(selected),
                    selected,
                    ..focusable(row_focus && selected)
                });
            if let Some(rect) = layout.choices.get(index) {
                node = node.with_bounds(*rect);
            }
            children.push(node);
        }

        let usb_start = self.visible_selection_start();
        let devices = view.usb_devices();
        for (index, device) in devices.iter().enumerate() {
            let access_key = if device.writable {
                "portal-usb-row-read-write"
            } else {
                "portal-usb-row-read-only"
            };
            let mut node = AccessNode::new(Role::Checkbox)
                .with_name(t_args(
                    access_key,
                    &[("device", device.label.as_str().into())],
                ))
                .with_action_target(target(Part::Choice(index)))
                .with_position(index.saturating_add(1), devices.len())
                .with_state(AccessState {
                    checked: Some(device.selected),
                    ..focusable(index == self.selected)
                });
            if let Some(rect) = index
                .checked_sub(usb_start)
                .and_then(|visible| layout.choices.get(visible))
            {
                node = node.with_bounds(*rect);
            }
            children.push(node);
        }

        let rows = view.shortcut_rows();
        for (index, row) in rows.iter().enumerate() {
            let selected = index == self.selected;
            let name = if row.description.is_empty() {
                row.id.clone()
            } else {
                row.description.clone()
            };
            let accelerator = row.accelerator.as_deref().map_or_else(
                || t("portal-shortcut-not-set"),
                crate::portal::global_shortcuts::display_accelerator,
            );
            let mut node = AccessNode::new(Role::ListItem)
                .with_name(name)
                .with_value(accelerator)
                .with_action_target(target(Part::Choice(index)))
                .with_position(index.saturating_add(1), rows.len())
                .with_state(AccessState {
                    selected,
                    ..focusable(selected)
                });
            if let Some(status) = &row.status {
                node = node.with_description(status.clone());
            }
            if let Some(rect) = layout.choices.get(index) {
                node = node.with_bounds(*rect);
            }
            children.push(node);
        }

        if let Some(editor) = view.launcher_editor() {
            if let Some(rect) = layout.launcher_name {
                let focused = focusable(self.launcher_name_focused());
                children.push(if matches!(view, DialogView::ShortcutStep { .. }) {
                    field_node(
                        &self.step_field,
                        t("portal-a11y-answer"),
                        target(Part::Name),
                        Some(rect),
                        AccessState {
                            required: matches!(
                                view,
                                DialogView::ShortcutStep {
                                    field_review: true,
                                    ..
                                }
                            ),
                            invalid: self.entry_error.is_some(),
                            ..focused
                        },
                    )
                } else {
                    let end = self.launcher_name.len();
                    AccessNode::new(Role::TextInput)
                        .with_name(t("portal-a11y-name"))
                        .with_value(self.launcher_name.clone())
                        .with_text_selection(end, end)
                        .with_action_target(target(Part::Name))
                        .with_bounds(rect)
                        .with_state(focused)
                });
            }
            let icons = editor.icon_choices.len();
            for (index, label) in editor.icon_choices.iter().enumerate() {
                let selected = index == self.launcher_icon;
                let mut node = AccessNode::new(Role::RadioButton)
                    .with_name(label.clone())
                    .with_action_target(target(Part::Icon(index)))
                    .with_position(index.saturating_add(1), icons)
                    .with_state(AccessState {
                        checked: Some(selected),
                        ..focusable(icon_focus && selected)
                    });
                if let Some(rect) = layout.launcher_icons.get(index) {
                    node = node.with_bounds(*rect);
                }
                children.push(node);
            }
        }

        if matches!(view, DialogView::Authentication { .. }) {
            let (prompt, notice) = self
                .queue
                .authentication_prompt(&handle)
                .unwrap_or_default();
            let name = if prompt.is_empty() {
                t("portal-network-password")
            } else {
                prompt
            };
            children.push(field_node(
                &self.secret,
                name,
                target(Part::Secret),
                layout.secret,
                AccessState {
                    required: true,
                    ..focusable(true)
                },
            ));
            if let Some(notice) = notice {
                children.push(
                    AccessNode::new(Role::StaticText)
                        .with_name(notice)
                        .with_live(Politeness::Assertive),
                );
            }
        }

        if matches!(
            view,
            DialogView::ShortcutStep {
                field_review: true,
                ..
            }
        ) {
            if let Some(error) = self.entry_error() {
                children.push(
                    AccessNode::new(Role::StaticText)
                        .with_name(error.to_owned())
                        .with_live(Politeness::Assertive),
                );
            }
        }
        if let DialogView::NetworkSecrets { fields, .. } = &view {
            for (index, (field, input)) in fields.iter().zip(&self.entries).enumerate() {
                let invalid = self.entry_error.is_some() && index == self.entry_focus;
                children.push(field_node(
                    input,
                    field.label.clone(),
                    target(Part::Field(index)),
                    layout.secret_fields.get(index).copied(),
                    AccessState {
                        required: field.required,
                        invalid,
                        ..focusable(index == self.entry_focus)
                    },
                ));
            }
            if let Some(error) = self.entry_error() {
                children.push(
                    AccessNode::new(Role::StaticText)
                        .with_name(error.to_owned())
                        .with_live(Politeness::Assertive),
                );
            }
        }

        if let (Some(rect), Some((remember, focused)), Some(label)) = (
            layout.remember,
            self.remember_display(),
            self.remember_label(),
        ) {
            children.push(
                AccessNode::new(Role::Checkbox)
                    .with_name(label)
                    .with_action_target(target(Part::Remember))
                    .with_bounds(rect)
                    .with_state(AccessState {
                        checked: Some(remember),
                        ..focusable(focused)
                    }),
            );
        }

        // With nothing to choose or type, Enter confirms: the reader's
        // cursor belongs on the button that answers.
        let confirm_focus = row_focus && view.selectable_count() == 0;
        children.push(
            AccessNode::new(Role::Button)
                .with_name(view.cancel_label())
                .with_action_target(target(Part::Cancel))
                .with_bounds(layout.cancel)
                .with_state(focusable(false)),
        );
        children.push(
            AccessNode::new(Role::Button)
                .with_name(view.confirm_label())
                .with_action_target(target(Part::Confirm))
                .with_bounds(layout.confirm)
                .with_state(focusable(confirm_focus)),
        );

        Some(
            AccessNode::new(Role::Dialog)
                .with_name(view.title())
                .with_description(view.detail())
                .with_bounds(layout.sheet)
                .with_state(AccessState::new().modal(true))
                .with_children(children),
        )
    }

    /// Operate the sheet from an assistive technology. `true` when `target`
    /// named a part of the sheet that is up now and the action applied to
    /// it; a target from an answered sheet changes nothing.
    pub fn perform_accessibility_action(&mut self, target: &str, action: &Action) -> bool {
        let Some((part, handle)) = target
            .strip_prefix(TARGET_PREFIX)
            .and_then(|rest| rest.split_once('#'))
        else {
            return false;
        };
        let Some(part) = Part::decode(part) else {
            return false;
        };
        let Some(view) = self.view().filter(|view| view.handle() == handle) else {
            return false;
        };
        match (part, action) {
            (_, Action::ScrollIntoView) => true,
            (Part::Cancel, Action::Activate) => self.cancel(),
            (Part::Confirm, Action::Activate) => self.confirm(),
            (Part::Cancel | Part::Confirm, Action::Focus) => true,
            (Part::Choice(index), Action::Activate | Action::Focus | Action::ToggleCheck) => {
                if index >= view.selectable_count() {
                    return false;
                }
                self.selected = index;
                if matches!(view, DialogView::UsbDevices { .. }) && !matches!(action, Action::Focus)
                {
                    return self.toggle_selected_usb_device();
                }
                true
            }
            (Part::Icon(index), Action::Activate | Action::Focus) => {
                let Some(editor) = view.launcher_editor() else {
                    return false;
                };
                if !editor.editable_icon || index >= editor.icon_choices.len() {
                    return false;
                }
                self.launcher_icon = index;
                self.launcher_field = 1;
                true
            }
            (Part::Name, Action::Focus | Action::Activate) => self.focus_name(&view),
            (Part::Name, Action::SetValue(value)) => {
                if !self.focus_name(&view) {
                    return false;
                }
                if matches!(view, DialogView::ShortcutStep { .. }) {
                    set_field(&mut self.step_field, value);
                    self.entry_error = None;
                } else {
                    self.launcher_name = value
                        .chars()
                        .filter(|ch| !ch.is_control())
                        .take(255)
                        .collect();
                }
                true
            }
            (Part::Secret, Action::Focus | Action::Activate) => self.is_authentication(),
            (Part::Secret, Action::SetValue(value)) => {
                if !self.is_authentication() {
                    return false;
                }
                set_field(&mut self.secret, value);
                true
            }
            (Part::Field(index), Action::Focus | Action::Activate) => {
                if index >= self.entries.len() {
                    return false;
                }
                self.entry_focus = index;
                true
            }
            (Part::Field(index), Action::SetValue(value)) => {
                let Some(entry) = self.entries.get_mut(index) else {
                    return false;
                };
                set_field(entry, value);
                self.entry_focus = index;
                self.entry_error = None;
                true
            }
            (Part::Remember, Action::Focus) => {
                if self.remember_display().is_none() {
                    return false;
                }
                self.entry_focus = self.entries.len();
                true
            }
            (Part::Remember, Action::Activate | Action::ToggleCheck) => {
                if self.remember_display().is_none() {
                    return false;
                }
                self.entry_focus = self.entries.len();
                self.remember = !self.remember;
                true
            }
            _ => false,
        }
    }

    fn focus_name(&mut self, view: &DialogView) -> bool {
        if !view
            .launcher_editor()
            .is_some_and(|editor| editor.editable_name)
        {
            return false;
        }
        self.launcher_field = 0;
        true
    }
}

#[cfg(test)]
#[path = "accessibility_tests.rs"]
mod tests;
