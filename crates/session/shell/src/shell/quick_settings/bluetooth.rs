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

use crate::shell::quick_settings::BtDevice;
use crate::shell::quick_settings::actions::{QsActions, log_qs_result};
use crate::shell::quick_settings::drill_in::DrillInList;

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
        Rect::new(
            panel.x0 + 8.0,
            panel.y0 + 8.0,
            panel.x0 + 40.0,
            panel.y0 + 40.0,
        )
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
            self.list.ensure_visible(panel, devices.len(), 56.0, device);
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
                .visible_rects(panel, devices.len(), 56.0)
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
                .visible_rects(panel, self.list.row_count(), 56.0)
                .into_iter()
                .find_map(|(index, rect)| (index == device).then_some(rect))
        }
    }

    pub fn accessibility_snapshot(&self, devices: &[BtDevice]) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};

        let mut children = vec![
            AccessNode::new(Role::Button)
                .with_name("Back")
                .with_state(AccessState::new().focusable(true)),
        ];
        children.extend(devices.iter().enumerate().map(|(index, device)| {
            AccessNode::new(Role::Button)
                .with_name(device.name.clone())
                .with_description(if device.connected {
                    "Connected; open device actions"
                } else {
                    "Paired; activate to connect"
                })
                .with_position(index.saturating_add(1), devices.len())
                .with_state(AccessState::new().focusable(true))
        }));
        AccessNode::new(Role::Dialog)
            .with_name("Bluetooth")
            .with_description(if devices.is_empty() {
                "No paired Bluetooth devices found"
            } else {
                "Choose a paired Bluetooth device"
            })
            .with_state(AccessState::new().modal(true))
            .with_children(children)
    }

    pub fn focus_next(&mut self, panel: Rect, devices: &[BtDevice], backwards: bool) {
        let count = Self::DEVICE_FOCUS_START.saturating_add(devices.len());
        self.keyboard_focus = Some(match (self.keyboard_focus, backwards) {
            (Some(index), true) => index.checked_sub(1).unwrap_or(count.saturating_sub(1)),
            (Some(index), false) => index.saturating_add(1) % count,
            (None, true) => count.saturating_sub(1),
            (None, false) => 0,
        });
        if let Some(device) = self
            .keyboard_focus
            .and_then(|index| index.checked_sub(Self::DEVICE_FOCUS_START))
        {
            self.list.ensure_visible(panel, devices.len(), 56.0, device);
        }
    }

    pub fn scroll_wheel(&mut self, panel: Rect, devices: &[BtDevice], rows: f64) -> bool {
        self.list.scroll_wheel(panel, devices.len(), 56.0, rows)
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
            self.list.ensure_visible(panel, devices.len(), 56.0, device);
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

    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, matching the sibling \
                  wifi/vpn drill-in panes this is called alongside"
    )]
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
        self.list
            .draw_header(scene, transform, text, theme, panel, cursor, "Bluetooth");
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
                    .accessible_name("Back"),
            );
        }
        if devices.is_empty() {
            DrillInList::draw_empty(scene, transform, text, theme, panel, "No devices found");
            return;
        }
        let colors = &theme.tokens.colors;
        for (index, row) in self.list.visible_indices().zip(self.list.rows().iter()) {
            let Some(device) = devices.get(index) else {
                continue;
            };
            let glyph = DrillInList::draw_discovery_row(
                scene,
                transform,
                text,
                theme,
                *row,
                index,
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
            let battery = Rect::new(
                row.x1 - 64.0,
                row.y0 + (row.height() - 18.0) / 2.0,
                row.x1 - 42.0,
                row.y0 + f64::midpoint(row.height(), 18.0),
            );
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
                    &Circle::new(Point::new(row.x1 - 14.0, row.center().y), 5.0),
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
        pane.list.layout(panel, devices.len());
        assert_eq!(
            pane.handle_click(pane.list.back_center(), panel, &mut devices, &actions),
            BluetoothOutcome::Back
        );
        let row = pane.list.rows()[0].center();
        assert_eq!(
            pane.handle_click(row, panel, &mut devices, &actions),
            BluetoothOutcome::OpenDetails {
                address: "02:00:00:00:00:01".into()
            }
        );
        assert!(devices[0].connected);
        assert!(actions.calls.borrow().is_empty());
        devices[0].connected = false;
        assert_eq!(
            pane.handle_click(row, panel, &mut devices, &actions),
            BluetoothOutcome::Changed
        );
        assert!(devices[0].connected);
        assert_eq!(actions.calls.borrow()[0], "connect:02:00:00:00:00:01");
    }

    #[test]
    fn failed_connect_is_reported_without_publishing_false_device_state() {
        let mut pane = QuickSettingsBluetoothPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut devices = vec![device()];
        devices[0].connected = false;
        let actions = Actions {
            reject: true,
            ..Default::default()
        };
        pane.list.layout(panel, devices.len());

        assert_eq!(
            pane.handle_click(pane.list.rows()[0].center(), panel, &mut devices, &actions,),
            BluetoothOutcome::ActionFailed {
                device: "Headphones".into(),
                error: "Bluetooth connection rejected".into(),
            }
        );
        assert!(!devices[0].connected);
        assert_eq!(
            actions.calls.borrow().as_slice(),
            ["connect:02:00:00:00:00:01"]
        );
    }

    #[test]
    fn keyboard_wraps_to_a_device_and_activates_the_same_row() {
        let mut pane = QuickSettingsBluetoothPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut devices = vec![device()];
        devices[0].connected = false;
        let actions = Actions::default();

        assert_eq!(
            pane.handle_key(
                &Key::Named(NamedKey::Tab),
                true,
                panel,
                &mut devices,
                &actions,
            ),
            Some(BluetoothOutcome::None)
        );
        assert_eq!(
            pane.handle_key(
                &Key::Named(NamedKey::Enter),
                false,
                panel,
                &mut devices,
                &actions,
            ),
            Some(BluetoothOutcome::Changed)
        );
        assert!(devices[0].connected);
        assert_eq!(
            actions.calls.borrow().as_slice(),
            ["connect:02:00:00:00:00:01"]
        );

        pane.reset_focus();
        let _ = pane.handle_key(
            &Key::Named(NamedKey::ArrowDown),
            false,
            panel,
            &mut devices,
            &actions,
        );
        assert_eq!(
            pane.handle_key(
                &Key::Named(NamedKey::Space),
                false,
                panel,
                &mut devices,
                &actions,
            ),
            Some(BluetoothOutcome::Back)
        );
    }

    #[test]
    fn semantics_match_back_then_visible_devices() {
        let pane = QuickSettingsBluetoothPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let devices = vec![device()];
        let names = pane
            .semantic_rects(panel, &devices)
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "quick_settings_bluetooth_back",
                "quick_settings_bluetooth_device[0]",
            ]
        );
        let snapshot = pane.accessibility_snapshot(&devices);
        assert_eq!(snapshot.name.as_deref(), Some("Bluetooth"));
        assert_eq!(snapshot.children.len(), names.len());
        assert_eq!(snapshot.children[1].name.as_deref(), Some("Headphones"));
        assert_eq!(
            snapshot.children[1].description.as_deref(),
            Some("Connected; open device actions")
        );
    }

    #[test]
    fn compact_keyboard_focus_scrolls_the_last_device_into_view() {
        let mut pane = QuickSettingsBluetoothPane::default();
        let panel = Rect::new(600.0, 40.0, 948.0, 240.0);
        let mut devices = (0..8)
            .map(|index| BtDevice {
                name: format!("Device {index}"),
                address: format!("02:00:00:00:00:{index:02X}"),
                battery: 70,
                connected: false,
            })
            .collect::<Vec<_>>();
        let actions = Actions::default();

        let _ = pane.handle_key(
            &Key::Named(NamedKey::Tab),
            true,
            panel,
            &mut devices,
            &actions,
        );
        assert_eq!(
            pane.keyboard_focus,
            Some(QuickSettingsBluetoothPane::DEVICE_FOCUS_START + 7)
        );
        assert_eq!(pane.list.visible_indices(), 6..8);
        let names = pane
            .semantic_rects(panel, &devices)
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"quick_settings_bluetooth_device[7]".to_string()));
        assert!(!names.contains(&"quick_settings_bluetooth_device[0]".to_string()));
    }
}
