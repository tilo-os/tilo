//! Bluetooth device list for quick settings.
//!
//! Rows act only on accessories already paired through the compositor-owned
//! `BlueZ` agent. Pairing remains a privileged conversation; reconnecting or
//! disconnecting an existing bond is safe to expose as a typed shell action.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};

use ui::text_render::TextRenderer;
use ui::theme::Theme;

use crate::shell::quick_settings::BtDevice;
use crate::shell::quick_settings::actions::{QsActions, log_qs_result};
use crate::shell::quick_settings::drill_in::DrillInList;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BluetoothOutcome {
    Back,
    Changed,
    OpenDetails { address: String },
    None,
}

#[derive(Default)]
pub struct QuickSettingsBluetoothPane {
    list: DrillInList,
}

impl QuickSettingsBluetoothPane {
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
        } else if let Some((index, _)) = self
            .list
            .rows()
            .iter()
            .enumerate()
            .find(|(_, row)| row.contains(point))
        {
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
            if result.is_ok() {
                device.connected = true;
            }
            log_qs_result("connect_bluetooth", result);
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
        if devices.is_empty() {
            DrillInList::draw_empty(scene, transform, text, theme, panel, "No devices found");
            return;
        }
        let colors = &theme.tokens.colors;
        for (index, (device, row)) in devices.iter().zip(self.list.rows()).enumerate() {
            let glyph = DrillInList::draw_discovery_row(
                scene,
                transform,
                text,
                theme,
                *row,
                index,
                cursor,
                &device.name,
            );
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
    struct Actions(RefCell<Vec<String>>);

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
            self.0.borrow_mut().push(format!("connect:{address}"));
            Ok(())
        }
        fn disconnect_bluetooth(&self, address: &str) -> Result<(), String> {
            self.0.borrow_mut().push(format!("disconnect:{address}"));
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
        assert!(actions.0.borrow().is_empty());
        devices[0].connected = false;
        assert_eq!(
            pane.handle_click(row, panel, &mut devices, &actions),
            BluetoothOutcome::Changed
        );
        assert!(devices[0].connected);
        assert_eq!(actions.0.borrow()[0], "connect:02:00:00:00:00:01");
    }
}
