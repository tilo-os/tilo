//! Bluetooth discovery list for quick settings.
//!
//! Device rows are intentionally informational: the hardware dispatcher only
//! exposes radio power today, not pair/connect. The pane owns exact painted
//! geometry and hit-testing so adding a typed device action later is local.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};

use ui::text_render::TextRenderer;
use ui::theme::Theme;

use crate::shell::quick_settings::BtDevice;
use crate::shell::quick_settings_drill_in::{DrillInList, DrillInOutcome};

#[derive(Default)]
pub struct QuickSettingsBluetoothPane {
    list: DrillInList,
}

impl QuickSettingsBluetoothPane {
    pub fn handle_click(
        &mut self,
        point: Point,
        panel: Rect,
        devices: &[BtDevice],
    ) -> DrillInOutcome {
        self.list.layout(panel, devices.len());
        if self.list.back_contains(point) {
            DrillInOutcome::Back
        } else {
            // Rows have no fake action: `os_actions::Dispatcher` has no
            // pair/connect contract yet. They remain absorbed by the pane.
            DrillInOutcome::None
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
            // Retain the neutral ring until the icon library grows Bluetooth.
            scene.stroke(
                &vello::kurbo::Stroke::new(1.5),
                transform,
                colors.text_primary,
                None,
                &Circle::new(glyph.center(), 7.0),
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

    fn device() -> BtDevice {
        BtDevice {
            name: "Headphones".into(),
            battery: 73,
            connected: true,
        }
    }

    #[test]
    fn back_is_owned_but_an_informational_row_has_no_invented_action() {
        let mut pane = QuickSettingsBluetoothPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let devices = vec![device()];
        pane.list.layout(panel, devices.len());
        assert_eq!(
            pane.handle_click(pane.list.back_center(), panel, &devices),
            DrillInOutcome::Back
        );
        assert_eq!(
            pane.handle_click(pane.list.rows()[0].center(), panel, &devices),
            DrillInOutcome::None
        );
    }
}
