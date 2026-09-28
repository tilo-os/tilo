//! Bluetooth device list for quick settings.
//!
//! Rows act only on accessories already paired through the compositor-owned
//! `BlueZ` agent. Pairing remains a privileged conversation; reconnecting or
//! disconnecting an existing bond is safe to expose as a typed shell action.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};
use winit::keyboard::{Key, NamedKey};

use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace};

use crate::shell::i18n::t;

use crate::shell::quick_settings::BtDevice;
use crate::shell::quick_settings::actions::{QsActions, log_qs_result};
use tilo_desktop::drill_in::{
    CONTROL_INSET, DrillInList, GLYPH, LIST_TOP, STATUS_DOT, trailing_dot_center,
    trailing_status_glyph,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BluetoothOutcome {
    Back,
    Changed,
    ActionFailed { device: String, error: String },
    OpenDetails { address: String },
    None,
}

#[derive(Default)]
pub struct QuickSettingsBluetoothPane {
    list: DrillInList,
    keyboard_focus: Option<usize>,
}

impl QuickSettingsBluetoothPane {
    const BACK_FOCUS: usize = 0;
    const DEVICE_FOCUS_START: usize = 1;

    fn back_rect(panel: Rect) -> Rect {
        DrillInList::back_rect(panel)
    }

    pub fn reset_focus(&mut self) {
        self.keyboard_focus = None;
    }

    pub fn focus_accessibility_path(
        &mut self,
        path: &[usize],
        panel: Rect,
        devices: &[BtDevice],
    ) -> bool {
        let [0, index] = path else {
            return false;
        };
        if *index >= Self::DEVICE_FOCUS_START.saturating_add(devices.len()) {
            return false;
        }
        self.keyboard_focus = Some(*index);
        if let Some(device) = index.checked_sub(Self::DEVICE_FOCUS_START) {
            self.list
                .ensure_visible(panel, devices.len(), LIST_TOP, device);
        }
        true
    }

    pub fn semantic_rects(&self, panel: Rect, devices: &[BtDevice]) -> Vec<(String, Rect)> {
        let mut rects = vec![(
            "quick_settings_bluetooth_back".into(),
            Self::back_rect(panel),
        )];
        rects.extend(
            self.list
                .visible_rects(panel, devices.len(), LIST_TOP)
                .into_iter()
                .map(|(index, rect)| (format!("quick_settings_bluetooth_device[{index}]"), rect)),
        );
        rects
    }

    pub fn accessibility_rect_for_path(&self, panel: Rect, path: &[usize]) -> Option<Rect> {
        let [0, index] = path else {
            return None;
        };
        if *index == Self::BACK_FOCUS {
            Some(Self::back_rect(panel))
        } else {
            let device = index.checked_sub(Self::DEVICE_FOCUS_START)?;
            self.list
                .visible_rects(panel, self.list.row_count(), LIST_TOP)
                .into_iter()
                .find_map(|(index, rect)| (index == device).then_some(rect))
        }
    }

    pub fn accessibility_snapshot(&self, devices: &[BtDevice]) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};

        let mut children = vec![
            AccessNode::new(Role::Button)
                .with_name(t("qs-back"))
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.keyboard_focus == Some(Self::BACK_FOCUS)),
                ),
        ];
        children.extend(devices.iter().enumerate().map(|(index, device)| {
            AccessNode::new(Role::Button)
                .with_name(device.name.clone())
                .with_description(t(if device.connected {
                    "qs-bluetooth-device-connected-a11y"
                } else {
                    "qs-bluetooth-device-paired-a11y"
                }))
                .with_position(index.saturating_add(1), devices.len())
                .with_state(AccessState::new().focusable(true).focused(
                    self.keyboard_focus == Some(Self::DEVICE_FOCUS_START.saturating_add(index)),
                ))
        }));
        AccessNode::new(Role::Dialog)
            .with_name(t("qs-bluetooth"))
            .with_description(t(if devices.is_empty() {
                "qs-bluetooth-none-a11y"
            } else {
                "qs-bluetooth-choose-a11y"
            }))
            .with_state(AccessState::new().modal(true))
            .with_children(children)
    }

    pub fn focus_next(&mut self, panel: Rect, devices: &[BtDevice], backwards: bool) {
        let count = Self::DEVICE_FOCUS_START.saturating_add(devices.len());
        self.keyboard_focus = Some(match (self.keyboard_focus, backwards) {
            (Some(index), true) => index
                .checked_sub(1)
                .unwrap_or_else(|| count.saturating_sub(1)),
            (Some(index), false) => index
                .saturating_add(1)
                .checked_rem(count)
                .unwrap_or_default(),
            (None, true) => count.saturating_sub(1),
            (None, false) => 0,
        });
        if let Some(device) = self
            .keyboard_focus
            .and_then(|index| index.checked_sub(Self::DEVICE_FOCUS_START))
        {
            self.list
                .ensure_visible(panel, devices.len(), LIST_TOP, device);
        }
    }

    pub fn scroll_wheel(&mut self, panel: Rect, devices: &[BtDevice], rows: f64) -> bool {
        self.list.scroll_wheel(panel, devices.len(), LIST_TOP, rows)
    }

    pub fn activate_focused(
        &mut self,
        panel: Rect,
        devices: &mut [BtDevice],
        actions: &dyn QsActions,
    ) -> BluetoothOutcome {
        let Some(index) = self.keyboard_focus else {
            return BluetoothOutcome::None;
        };
        let point = if index == Self::BACK_FOCUS {
            Self::back_rect(panel).center()
        } else {
            self.list.layout(panel, devices.len());
            let device = index.saturating_sub(Self::DEVICE_FOCUS_START);
            self.list
                .ensure_visible(panel, devices.len(), LIST_TOP, device);
            let Some(row) = self.list.row_for(device) else {
                return BluetoothOutcome::None;
            };
            row.center()
        };
        self.handle_click(point, panel, devices, actions)
    }

    pub fn handle_key(
        &mut self,
        logical_key: &Key,
        backwards: bool,
        panel: Rect,
        devices: &mut [BtDevice],
        actions: &dyn QsActions,
    ) -> Option<BluetoothOutcome> {
        match logical_key {
            Key::Named(NamedKey::Tab) => {
                self.focus_next(panel, devices, backwards);
                Some(BluetoothOutcome::None)
            }
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowLeft) => {
                self.focus_next(panel, devices, true);
                Some(BluetoothOutcome::None)
            }
            Key::Named(NamedKey::ArrowDown | NamedKey::ArrowRight) => {
                self.focus_next(panel, devices, false);
                Some(BluetoothOutcome::None)
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                Some(self.activate_focused(panel, devices, actions))
            }
            _ => None,
        }
    }

    pub fn handle_click(
        &mut self,
        point: Point,
        panel: Rect,
        devices: &mut [BtDevice],
        actions: &dyn QsActions,
    ) -> BluetoothOutcome {
        self.list.layout(panel, devices.len());
        if self.list.back_contains(point) {
            BluetoothOutcome::Back
        } else if let Some(index) = self.list.row_at(point) {
            let Some(device) = devices.get_mut(index) else {
                return BluetoothOutcome::None;
            };
            let was_connected = device.connected;
            if was_connected {
                return BluetoothOutcome::OpenDetails {
                    address: device.address.clone(),
                };
            }
            let result = actions.connect_bluetooth(&device.address);
            if let Err(error) = result {
                log_qs_result("connect_bluetooth", Err(error.clone()));
                return BluetoothOutcome::ActionFailed {
                    device: device.name.clone(),
                    error,
                };
            }
            log_qs_result("connect_bluetooth", Ok(()));
            device.connected = true;
            BluetoothOutcome::Changed
        } else {
            BluetoothOutcome::None
        }
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        cursor: Option<Point>,
        devices: &[BtDevice],
    ) {
        self.list.layout(panel, devices.len());
        self.list.draw_header(
            scene,
            transform,
            text,
            theme,
            panel,
            cursor,
            &t("qs-bluetooth"),
        );
        if self.keyboard_focus == Some(Self::BACK_FOCUS) {
            Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::new(Self::back_rect(panel), "")
                    .ghost()
                    .icon(ui::icons::Icon::ChevronLeft)
                    .focused(true)
                    .accessible_name(&t("qs-back")),
            );
        }
        if devices.is_empty() {
            DrillInList::draw_empty(
                scene,
                transform,
                text,
                theme,
                panel,
                &t("qs-bluetooth-empty"),
            );
            return;
        }
        let colors = &theme.tokens.colors;
        for (index, row) in self.list.visible_indices().zip(self.list.rows().iter()) {
            let Some(device) = devices.get(index) else {
                continue;
            };
            let glyph = DrillInList::draw_discovery_row(
                &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
                *row,
                cursor,
                72.0,
                &device.name,
            );
            if self.keyboard_focus == Some(Self::DEVICE_FOCUS_START.saturating_add(index)) {
                Button::paint(
                    scene,
                    transform,
                    text,
                    theme,
                    &ButtonFace::new(*row, "")
                        .ghost()
                        .focused(true)
                        .accessible_name(&device.name),
                );
            }
            // Use the same recognisable leading glyph treatment as the root
            // card and Wi-Fi list. A neutral ring made Bluetooth look like a
            // placeholder rather than a real system destination.
            ui::icons::draw_icon(
                scene,
                transform,
                glyph,
                ui::icons::Icon::Bluetooth,
                colors.text_primary,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            // The battery glyph takes the trailing status slot, centred on
            // the row's centre line, one related step before the dot.
            let battery = trailing_status_glyph(*row, CONTROL_INSET, GLYPH);
            let battery_color = if device.battery > 50 {
                colors.semantic.success
            } else if device.battery > 20 {
                colors.text_primary
            } else {
                colors.semantic.danger
            };
            ui::icons::draw_icon(
                scene,
                transform,
                battery,
                ui::icons::Icon::Battery,
                battery_color,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            if device.connected {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    colors.accent[5],
                    None,
                    &Circle::new(trailing_dot_center(*row, CONTROL_INSET), STATUS_DOT / 2.0),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Actions {
        calls: RefCell<Vec<String>>,
        reject: bool,
    }

    impl QsActions for Actions {
        fn set_brightness(&self, _: f32) -> Result<(), String> {
            Ok(())
        }
        fn set_volume(&self, _: f32) -> Result<(), String> {
            Ok(())
        }
        fn set_wifi(&self, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn set_bluetooth(&self, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn connect_wifi(&self, _: &str, _: Option<&str>) -> Result<(), String> {
            Ok(())
        }
        fn set_power_profile(
            &self,
            _: crate::shell::quick_settings::actions::PowerProfile,
        ) -> Result<(), String> {
            Ok(())
        }
        fn connect_bluetooth(&self, address: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("connect:{address}"));
            if self.reject {
                Err("Bluetooth connection rejected".into())
            } else {
                Ok(())
            }
        }
        fn disconnect_bluetooth(&self, address: &str) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("disconnect:{address}"));
            Ok(())
        }
    }

    fn device() -> BtDevice {
        BtDevice {
            name: "Headphones".into(),
            address: "02:00:00:00:00:01".into(),
            battery: 73,
            connected: true,
        }
    }

    #[test]
    fn back_is_owned_and_paired_rows_toggle_the_exact_accessory() {
        let mut pane = QuickSettingsBluetoothPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut devices = vec![device()];
        let actions = Actions::default();
        let validated = crate::shell::search::providers::actions::ValidatedQsActions::new(&actions);
        pane.list.layout(panel, devices.len());
        assert_eq!(
            pane.handle_click(pane.list.back_center(), panel, &mut devices, &validated),
            BluetoothOutcome::Back
        );
        let row = pane.list.rows()[0].center();
        assert_eq!(
            pane.handle_click(row, panel, &mut devices, &validated),
            BluetoothOutcome::OpenDetails {
                address: "02:00:00:00:00:01".into()
            }
        );
        assert!(devices[0].connected);
        assert!(actions.calls.borrow().is_empty());
        devices[0].connected = false;
        assert_eq!(
            pane.handle_click(row, panel, &mut devices, &validated),
            BluetoothOutcome::Changed
        );
        assert!(devices[0].connected);
        assert_eq!(actions.calls.borrow()[0], "connect:02:00:00:00:00:01");
        validated.disconnect_bluetooth(&devices[0].address).unwrap();
        assert_eq!(actions.calls.borrow()[1], "disconnect:02:00:00:00:00:01");
    }
}
