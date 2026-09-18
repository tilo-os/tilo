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
use crate::shell::quick_settings_drill_in::{
    CONTROL_INSET, DrillInList, DrillInOutcome, GLYPH, STATUS_DOT, trailing_dot_center,
    trailing_status_glyph,
};

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
