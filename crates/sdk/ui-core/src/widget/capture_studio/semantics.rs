//! The studio as assistive technology and agents meet it: every control it
//! paints itself (the bar's outputs, shutter, targets and options, the
//! status bar's stop and save, a receipt's actions and the menus they open)
//! published as a named node and operated through the same paths as a
//! press on it.

use super::{
    BarItem, CaptureStudio, OUTPUTS, OptionChoice, PipLayout, ScreenshotSendTarget, TARGETS,
    output_name, receipt::ReceiptAction, studio_text, target_key,
};
use crate::a11y::{AccessNode, AccessState, Action, Politeness, Role};
use vello::kurbo::Rect;

/// Where a published control is operated from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    /// The setup bar, pressed as a click at the control.
    Bar,
    /// The status bar's stop or save.
    Status,
    /// A receipt's action or its Dismiss.
    Receipt,
    /// A row of the options menu.
    Option(usize),
    /// A row of a screenshot's send menu.
    Send(usize),
}

/// One control the studio paints: its stable target, where it is, how it
/// is pressed, and its node.
pub(super) struct StudioControl {
    target: String,
    rect: Rect,
    route: Route,
    node: AccessNode,
}

/// The stable name of a setup-bar item, as tests, agents and assistive
/// technology address it.
pub(super) fn bar_target(item: BarItem) -> Option<&'static str> {
    Some(match item {
        BarItem::Output(0) => "capture.output.screenshot",
        BarItem::Output(1) => "capture.output.recording",
        BarItem::Output(_) => "capture.output.pip",
        BarItem::Target(0) => "capture.target.selection",
        BarItem::Target(1) => "capture.target.window",
        BarItem::Target(2) => "capture.target.screen",
        BarItem::Target(_) => "capture.target.strip",
        BarItem::Shutter => "capture.shutter",
        BarItem::Options => "capture.options",
        BarItem::Time | BarItem::Save => return None,
    })
}

/// The stable name of a row of the options menu.
pub(super) fn option_target(choice: OptionChoice) -> String {
    match choice {
        OptionChoice::Delay(seconds) => format!("capture.option.delay-{seconds}"),
        OptionChoice::Pointer => "capture.option.pointer".into(),
        OptionChoice::SystemAudio => "capture.option.system-audio".into(),
        OptionChoice::Microphone => "capture.option.microphone".into(),
        OptionChoice::KeepRecent => "capture.option.keep-recent".into(),
        OptionChoice::Layout(layout) => format!(
            "capture.option.combine-{}",
            match layout {
                PipLayout::Auto => "arranged",
                PipLayout::Grid => "grid",
                PipLayout::Stack => "stacked",
            }
        ),
    }
}

/// The stable name of a row of the send menu.
pub(super) fn send_target(target: &ScreenshotSendTarget) -> String {
    match target {
        ScreenshotSendTarget::Workspace => "capture.send-to-workspace".into(),
        ScreenshotSendTarget::Device(id) => format!("capture.send-to-device.{id}"),
    }
}

fn button(name: String) -> AccessNode {
    AccessNode::new(Role::Button).with_name(name)
}

fn radio(name: String, checked: bool) -> AccessNode {
    AccessNode::new(Role::RadioButton)
        .with_name(name)
        .with_state(AccessState::new().checked(checked))
}

impl CaptureStudio {
    /// Every control shown now, in reading order.
    pub(super) fn studio_controls(&self) -> Vec<StudioControl> {
        let mut out = Vec::new();
        let mut push = |target: &str, rect: Rect, route: Route, node: AccessNode| {
            // A menu row is offered before its first paint has measured
            // it; anything else is offered only where it is painted.
            let menu_row = matches!(route, Route::Option(_) | Route::Send(_));
            if menu_row || (rect.width() > 0.0 && rect.height() > 0.0) {
                out.push(StudioControl {
                    target: target.to_owned(),
                    rect,
                    route,
                    node,
                });
            }
        };
        if self.screenshot_receipt.is_some() {
            let layout = self.screenshot_receipt_layout();
            for (action, rect) in &layout.actions {
                let target = match action {
                    ReceiptAction::Copy => "capture.copy-screenshot",
                    ReceiptAction::Edit => "capture.edit-screenshot",
                    ReceiptAction::Open => "capture.open-screenshot",
                    ReceiptAction::Reveal => "capture.show-screenshot",
                    ReceiptAction::Send => "capture.send-screenshot",
                    ReceiptAction::Play => continue,
                };
                let mut node = button(action.label());
                if *action == ReceiptAction::Send {
                    node = node.with_state(AccessState::new().expanded(self.send_menu.is_some()));
                }
                push(target, *rect, Route::Receipt, node);
            }
            push(
                "capture.dismiss-screenshot",
                layout.dismiss,
                Route::Receipt,
                button(studio_text("widget-capture-dismiss")),
            );
            if let Some(menu) = self.send_menu.as_ref().filter(|menu| menu.is_open()) {
                for (index, item) in menu.items().iter().enumerate() {
                    let Some(label) = item.label() else {
                        continue;
                    };
                    let rect = menu.item_rect(index).unwrap_or(Rect::ZERO);
                    // The row saying no device is online has nothing to
                    // send to; it is read, never chosen.
                    let target = self
                        .send_menu_targets
                        .get(index)
                        .and_then(Option::as_ref)
                        .map_or_else(|| "capture.send-no-device".to_owned(), send_target);
                    push(
                        &target,
                        rect,
                        Route::Send(index),
                        AccessNode::new(Role::MenuItem)
                            .with_name(label.to_owned())
                            .with_state(AccessState::new().disabled(!item.is_enabled())),
                    );
                }
            }
        } else if self.recording_finished_at.is_some() {
            let layout = self.recording_receipt_layout();
            for (action, rect) in &layout.actions {
                let target = match action {
                    ReceiptAction::Play => "capture.play-recording",
                    _ => "capture.show-recording",
                };
                push(target, *rect, Route::Receipt, button(action.label()));
            }
            push(
                "capture.dismiss-recording",
                layout.dismiss,
                Route::Receipt,
                button(studio_text("widget-capture-dismiss")),
            );
        }
        if self.shows_status() {
            for (item, rect) in self.bar_items() {
                match item {
                    BarItem::Shutter => push(
                        if self.replay_active {
                            "capture.stop-replay"
                        } else {
                            "capture.stop"
                        },
                        rect,
                        Route::Status,
                        button(self.shutter_name())
                            .with_state(AccessState::new().disabled(!self.shutter_ready())),
                    ),
                    BarItem::Save => push(
                        "capture.save-replay",
                        rect,
                        Route::Status,
                        button(studio_text("widget-replay-save")).with_state(
                            AccessState::new().disabled(self.replay_buffered_seconds == 0),
                        ),
                    ),
                    _ => {}
                }
            }
        }
        if self.choosing() {
            for (item, rect) in self.bar_items() {
                let Some(target) = bar_target(item) else {
                    continue;
                };
                let node = match item {
                    BarItem::Output(index) => {
                        let Some(mode) = OUTPUTS.get(index).copied() else {
                            continue;
                        };
                        radio(output_name(mode), self.mode == mode)
                    }
                    BarItem::Target(index) => {
                        let Some(source) = TARGETS.get(index).copied() else {
                            continue;
                        };
                        radio(studio_text(target_key(source)), self.source == source)
                    }
                    BarItem::Shutter => button(self.shutter_name())
                        .with_state(AccessState::new().disabled(!self.shutter_ready())),
                    BarItem::Options => button(studio_text("widget-capture-options"))
                        .with_state(AccessState::new().expanded(self.options_menu.is_some())),
                    BarItem::Time | BarItem::Save => continue,
                };
                push(target, rect, Route::Bar, node);
            }
            if let Some(menu) = self.options_menu.as_ref().filter(|menu| menu.is_open()) {
                for (index, item) in menu.items().iter().enumerate() {
                    // A section's caption names the rows under it; it is
                    // not a choice.
                    let Some(choice) = self.options_choices.get(index).copied().flatten() else {
                        continue;
                    };
                    let Some(label) = item.label() else {
                        continue;
                    };
                    let rect = menu.item_rect(index).unwrap_or(Rect::ZERO);
                    let mut state = AccessState::new().disabled(!item.is_enabled());
                    if let Some(checked) = item.checked() {
                        state = state.checked(checked);
                    }
                    push(
                        &option_target(choice),
                        rect,
                        Route::Option(index),
                        AccessNode::new(Role::MenuItem)
                            .with_name(label.to_owned())
                            .with_state(state),
                    );
                }
            }
        }
        out
    }

    /// The studio's node: a modal dialog while choosing, the receipt or
    /// status bar otherwise; `None` while nothing shows.
    pub(super) fn studio_accessibility(&self) -> Option<AccessNode> {
        if !self.open {
            return None;
        }
        let children: Vec<AccessNode> = self
            .studio_controls()
            .into_iter()
            .map(|control| {
                let node = control.node.with_action_target(control.target);
                if control.rect.area() > 0.0 {
                    node.with_bounds(control.rect)
                } else {
                    node
                }
            })
            .collect();
        let node = if self.choosing() {
            AccessNode::new(Role::Dialog)
                .with_name(studio_text("widget-capture-title"))
                .with_state(AccessState::new().modal(true))
        } else if let Some(receipt) = self.screenshot_receipt.as_ref() {
            // What became of the screenshot is said as it appears.
            AccessNode::new(Role::Group)
                .with_name(studio_text(receipt.kept.status_key()))
                .with_live(Politeness::Polite)
        } else if self.recording_finished_at.is_some() {
            AccessNode::new(Role::Group)
                .with_name(studio_text("widget-recording-saved"))
                .with_live(Politeness::Polite)
        } else if self.shows_status() {
            AccessNode::new(Role::Toolbar).with_name(output_name(self.mode))
        } else {
            return None;
        };
        Some(node.with_children(children))
    }

    pub(super) fn studio_control_rect(&self, path: &[usize]) -> Option<Rect> {
        let [index] = path else { return None };
        self.studio_controls()
            .get(*index)
            .map(|control| control.rect)
            .filter(|rect| rect.area() > 0.0)
    }

    /// Operate one published control the way a press on it would.
    pub(super) fn perform_studio_control(&mut self, target: &str, action: &Action) -> bool {
        let Some(control) = self
            .studio_controls()
            .into_iter()
            .find(|control| control.target == target)
        else {
            return false;
        };
        let state = control.node.state;
        if state.disabled {
            return false;
        }
        let applies = match action {
            Action::Activate => true,
            Action::ToggleCheck => state.checked.is_some(),
            Action::Expand => state.expanded == Some(false),
            Action::Collapse => state.expanded == Some(true),
            _ => false,
        };
        if !applies {
            return false;
        }
        let at = control.rect.center();
        match control.route {
            Route::Bar => {
                if state.expanded == Some(true) {
                    self.options_menu = None;
                } else {
                    self.click(at, false);
                }
            }
            Route::Status => self.press_status(at),
            // The receipt is on its way into a pin; it takes nothing more.
            Route::Receipt if self.screenshot_pin_pending => return false,
            Route::Receipt => {
                if state.expanded == Some(true) {
                    self.close_send_menu();
                } else {
                    self.press_receipt(at);
                }
            }
            Route::Option(index) => self.choose_option(index),
            Route::Send(index) => self.choose_send_target(index),
        }
        true
    }
}

#[cfg(test)]
#[path = "semantics_tests.rs"]
mod tests;
