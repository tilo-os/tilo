//! Wi-Fi network-list slice of quick settings.
//!
//! Owns list geometry, painting, row hit-testing and join routing. The
//! password editor remains a separate, deliberately specialised overlay-host
//! slice until the shared text input can bind an ephemeral secret directly.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::Fill;

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, ButtonStyle};

use crate::shell::chrome_state::QuickSettingsState;
use crate::shell::qs_actions::{QsActions, log_qs_result};
use crate::shell::quick_settings::{WifiNetwork, WifiPasswordPrompt};

const HEADER_HEIGHT: f64 = 48.0;
const LIST_TOP: f64 = 56.0;
const ROW_HEIGHT: f64 = 56.0;
const ROW_INSET: f64 = 12.0;

#[allow(clippy::cast_precision_loss, reason = "Wi-Fi network counts are tiny")]
fn usize_to_f64_lossy(value: usize) -> f64 {
    value as f64
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WifiPaneOutcome {
    #[default]
    None,
    Back,
    Changed,
}

#[derive(Default)]
pub struct QuickSettingsWifiPane {
    back_rect: Rect,
    row_rects: Vec<Rect>,
}

impl QuickSettingsWifiPane {
    fn layout(&mut self, panel: Rect, row_count: usize) {
        self.back_rect = Rect::new(
            panel.x0 + 8.0,
            panel.y0 + 8.0,
            panel.x0 + 40.0,
            panel.y0 + 40.0,
        );
        self.row_rects.clear();
        self.row_rects.extend((0..row_count).map(|index| {
            let y = panel.y0 + LIST_TOP + usize_to_f64_lossy(index) * ROW_HEIGHT;
            Rect::new(
                panel.x0 + ROW_INSET,
                y,
                panel.x1 - ROW_INSET,
                y + ROW_HEIGHT - 4.0,
            )
        }));
    }

    pub fn handle_click(
        &mut self,
        point: Point,
        panel: Rect,
        state: &mut QuickSettingsState,
        actions: &dyn QsActions,
    ) -> WifiPaneOutcome {
        self.layout(panel, state.wifi_networks.len());
        if self.back_rect.contains(point) {
            return WifiPaneOutcome::Back;
        }
        let Some(index) = self.row_rects.iter().position(|row| row.contains(point)) else {
            return WifiPaneOutcome::None;
        };
        let Some(network) = state.wifi_networks.get(index) else {
            return WifiPaneOutcome::None;
        };
        let ssid = network.ssid.clone();
        if network.secure && !network.connected {
            state.wifi_password = Some(WifiPasswordPrompt::new(ssid));
            WifiPaneOutcome::Changed
        } else {
            log_qs_result("connect_wifi", actions.connect_wifi(&ssid, None));
            WifiPaneOutcome::None
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
        networks: &[WifiNetwork],
    ) {
        self.layout(panel, networks.len());
        let colors = &theme.tokens.colors;
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(self.back_rect, "")
                .style(ButtonStyle::Ghost)
                .icon(ui::icons::Icon::ChevronLeft)
                .hovered(cursor.is_some_and(|point| self.back_rect.contains(point)))
                .accessible_name("Back"),
        );
        let title = "Wi-Fi";
        let (title_width, title_height) =
            text.measure(title, theme.tokens.typography.scale.sm, None);
        text.draw_text_weighted(
            scene,
            title,
            theme.tokens.typography.scale.sm,
            colors.text_primary,
            transform.pre_translate(
                (
                    panel.center().x - f64::from(title_width) / 2.0,
                    panel.y0 + (HEADER_HEIGHT - f64::from(title_height)) / 2.0,
                )
                    .into(),
            ),
            None,
            TextWeight::Semibold,
        );
        if networks.is_empty() {
            let message = "Scanning for networks…";
            let (width, height) = text.measure(message, theme.tokens.typography.scale.sm, None);
            text.draw_text(
                scene,
                message,
                theme.tokens.typography.scale.sm,
                colors.text_dim,
                transform.pre_translate(
                    (
                        panel.center().x - f64::from(width) / 2.0,
                        panel.y0 + LIST_TOP + 24.0 - f64::from(height) / 2.0,
                    )
                        .into(),
                ),
                None,
            );
            return;
        }
        for (index, (network, row)) in networks.iter().zip(&self.row_rects).enumerate() {
            let hovered = cursor.is_some_and(|point| row.contains(point));
            if index == 0 || hovered {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.neutral[2].with_alpha_factor(if hovered { 0.75 } else { 0.50 }),
                    None,
                    &RoundedRect::from_rect(*row, 12.0),
                );
            }
            let glyph = Rect::new(
                row.x0 + 12.0,
                row.y0 + (row.height() - 20.0) / 2.0,
                row.x0 + 32.0,
                row.y0 + f64::midpoint(row.height(), 20.0),
            );
            ui::icons::draw_icon(
                scene,
                transform,
                glyph,
                ui::icons::Icon::Wifi,
                colors.text_primary,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            text.draw_text_weighted(
                scene,
                &network.ssid,
                theme.tokens.typography.scale.sm,
                colors.text_primary,
                transform
                    .pre_translate((glyph.x1 + 12.0, row.y0 + (row.height() - 14.0) / 2.0).into()),
                None,
                TextWeight::Medium,
            );
            let baseline = row.y0 + row.height() / 2.0 + 6.0;
            for bar in 0..4_u8 {
                let x = row.x1 - 62.0 + f64::from(bar) * 4.0;
                let height = f64::from(bar.saturating_add(1)) * 3.0;
                scene.fill(
                    Fill::NonZero,
                    transform,
                    if bar < network.signal {
                        colors.text_primary.with_alpha_factor(0.85)
                    } else {
                        colors.text_primary.with_alpha_factor(0.20)
                    },
                    None,
                    &RoundedRect::from_rect(
                        Rect::new(x, baseline - height, x + 3.0, baseline),
                        1.0,
                    ),
                );
            }
            if network.connected {
                scene.fill(
                    Fill::NonZero,
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
    struct RecordingActions(RefCell<Vec<(String, Option<String>)>>);

    impl QsActions for RecordingActions {
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
        fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
            self.0
                .borrow_mut()
                .push((ssid.to_owned(), password.map(str::to_owned)));
            Ok(())
        }
        fn set_power_profile(&self, _: os_actions::PowerProfile) -> Result<(), String> {
            Ok(())
        }
    }

    fn network(ssid: &str, secure: bool) -> WifiNetwork {
        WifiNetwork {
            ssid: ssid.into(),
            signal: 3,
            secure,
            connected: false,
        }
    }

    #[test]
    fn protected_and_open_rows_take_distinct_paths() {
        let mut pane = QuickSettingsWifiPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut state = QuickSettingsState::default();
        state.wifi_networks = vec![network("Cafe", true), network("Open", false)];
        let actions = RecordingActions::default();
        pane.layout(panel, 2);
        assert_eq!(
            pane.handle_click(pane.row_rects[0].center(), panel, &mut state, &actions),
            WifiPaneOutcome::Changed
        );
        assert_eq!(state.wifi_password.as_ref().unwrap().ssid, "Cafe");
        state.wifi_password = None;
        assert_eq!(
            pane.handle_click(pane.row_rects[1].center(), panel, &mut state, &actions),
            WifiPaneOutcome::None
        );
        assert_eq!(actions.0.borrow().as_slice(), [("Open".into(), None)]);
    }

    #[test]
    fn back_and_empty_space_never_join_a_network() {
        let mut pane = QuickSettingsWifiPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut state = QuickSettingsState::default();
        state.wifi_networks = vec![network("Cafe", true)];
        let actions = RecordingActions::default();
        pane.layout(panel, 1);
        assert_eq!(
            pane.handle_click(pane.back_rect.center(), panel, &mut state, &actions),
            WifiPaneOutcome::Back
        );
        assert_eq!(
            pane.handle_click(panel.center(), panel, &mut state, &actions),
            WifiPaneOutcome::None
        );
        assert!(actions.0.borrow().is_empty());
        assert!(state.wifi_password.is_none());
    }
}
