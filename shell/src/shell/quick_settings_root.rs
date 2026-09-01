//! Root page of the quick-settings popover.
//!
//! Owns root layout, painting, hit-testing and slider-drag presentation state.
//! Shell-wide effects remain outcomes for the overlay host to apply.

use crate::shell::chrome_state::QuickSettingsState;
use crate::shell::quick_settings::{PowerProfile, QsLevel};
use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;

fn usize_to_f64_lossy(value: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}
fn f64_to_f32_lossy(value: f64) -> f32 {
    #[allow(clippy::cast_possible_truncation)]
    {
        value as f32
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RootAction {
    None,
    OpenSettings,
    Privacy,
    OpenNotifications,
    OpenPowerMenu,
    DisplayNotice,
    Enter(QsLevel),
    Changed(PendingRootActions),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PendingRootActions {
    pub wifi: Option<bool>,
    pub bluetooth: Option<bool>,
    pub wired: Option<bool>,
    pub airplane: Option<bool>,
    pub hotspot: Option<bool>,
    pub mobile: Option<bool>,
    pub night_light: Option<bool>,
    pub power: Option<PowerProfile>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SliderCommit {
    Brightness(f32),
    Volume(f32),
}

#[derive(Default)]
pub struct QuickSettingsRoot {
    panel: Rect,
    slider_drag: Option<usize>,
}

impl QuickSettingsRoot {
    pub fn handle_click(
        &mut self,
        point: Point,
        panel: Rect,
        state: &mut QuickSettingsState,
    ) -> RootAction {
        self.panel = panel;
        for index in [0, 1, 5] {
            if self
                .quick_settings_toggle_chevron_rect(index)
                .contains(point)
            {
                return RootAction::Enter(match index {
                    0 => QsLevel::Wifi,
                    1 => QsLevel::Bluetooth,
                    _ => QsLevel::Vpn,
                });
            }
        }
        for index in 0..4 {
            if self.quick_settings_action_geometry(index).contains(point) {
                return match index {
                    0 => RootAction::OpenSettings,
                    1 => RootAction::Privacy,
                    2 => RootAction::OpenNotifications,
                    _ => RootAction::OpenPowerMenu,
                };
            }
        }
        let mut pending = PendingRootActions::default();
        let mut changed = false;
        for index in 0..2 {
            let (bar, _) = self.quick_settings_slider_geometry(index);
            let hit = Rect::new(bar.x0 - 4.0, bar.y0 - 12.0, bar.x1 + 4.0, bar.y1 + 12.0);
            if hit.contains(point) {
                let value = f64_to_f32_lossy(((point.x - bar.x0) / bar.width()).clamp(0.0, 1.0));
                if index == 0 {
                    state.brightness = value;
                } else {
                    state.volume = value;
                }
                self.slider_drag = Some(index);
                changed = true;
            }
        }
        for index in 0..9 {
            let card = self.quick_settings_toggle_geometry(index);
            let hit = Rect::new(card.x0 - 4.0, card.y0 - 4.0, card.x1 + 4.0, card.y1 + 4.0);
            if !hit.contains(point) {
                continue;
            }
            match index {
                0 => {
                    state.wifi = !state.wifi;
                    pending.wifi = Some(state.wifi);
                }
                1 => {
                    state.bluetooth = !state.bluetooth;
                    pending.bluetooth = Some(state.bluetooth);
                }
                2 => {
                    if let Some(wired) = state.wired.as_mut() {
                        wired.connected = !wired.connected;
                        pending.wired = Some(wired.connected);
                    }
                }
                3 => {
                    state.radios.airplane_mode = !state.radios.airplane_mode;
                    pending.airplane = Some(state.radios.airplane_mode);
                }
                4 => {
                    state.radios.hotspot_active = !state.radios.hotspot_active;
                    pending.hotspot = Some(state.radios.hotspot_active);
                }
                5 => return RootAction::Enter(QsLevel::Vpn),
                6 => {
                    state.night_light = !state.night_light;
                    pending.night_light = Some(state.night_light);
                }
                7 => return RootAction::DisplayNotice,
                _ if state.radios.wwan_available => {
                    state.radios.wwan_enabled = !state.radios.wwan_enabled;
                    pending.mobile = Some(state.radios.wwan_enabled);
                }
                _ => {}
            }
            changed = true;
        }
        if self.quick_settings_power_profile_geometry().contains(point) {
            let next = match state.power_profile {
                PowerProfile::Saver => PowerProfile::Balanced,
                PowerProfile::Balanced => PowerProfile::Performance,
                PowerProfile::Performance => PowerProfile::Saver,
            };
            state.power_profile = next;
            pending.power = Some(next);
            changed = true;
        }
        if changed {
            RootAction::Changed(pending)
        } else {
            RootAction::None
        }
    }

    pub fn update_drag(
        &mut self,
        point: Point,
        panel: Rect,
        state: &mut QuickSettingsState,
    ) -> bool {
        self.panel = panel;
        let Some(index) = self.slider_drag else {
            return false;
        };
        let (bar, _) = self.quick_settings_slider_geometry(index);
        let value = f64_to_f32_lossy(((point.x - bar.x0) / bar.width()).clamp(0.0, 1.0));
        if index == 0 {
            state.brightness = value;
        } else {
            state.volume = value;
        }
        true
    }
    pub fn finish_drag(&mut self, state: &QuickSettingsState) -> Option<SliderCommit> {
        match self.slider_drag.take()? {
            0 => Some(SliderCommit::Brightness(state.brightness * 100.0)),
            _ => Some(SliderCommit::Volume(state.volume * 100.0)),
        }
    }
    pub fn drag_active(&self) -> bool {
        self.slider_drag.is_some()
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        state: &QuickSettingsState,
    ) {
        self.panel = panel;
        self.draw_quick_settings_action_row(scene, transform, text_renderer, theme, state);
        self.draw_quick_settings_slider_card(scene, transform, theme, panel, state);
        self.draw_quick_settings_toggle_grid(
            scene,
            transform,
            text_renderer,
            theme,
            state,
            "Not connected",
        );
        self.draw_quick_settings_power_profile(scene, transform, text_renderer, theme, state);
    }

    pub fn slider_geometry(panel: Rect, index: usize) -> (Rect, f64) {
        let row_y = panel.y0 + 100.0 + usize_to_f64_lossy(index) * 36.0;
        let bar_y_mid = row_y + 12.0;
        (
            Rect::new(
                panel.x0 + 44.0,
                bar_y_mid - 3.0,
                panel.x1 - 16.0,
                bar_y_mid + 3.0,
            ),
            row_y,
        )
    }
    pub fn toggle_geometry(panel: Rect, index: usize) -> Rect {
        let card_w = (panel.width() - 48.0) / 3.0;
        let row = index / 3;
        let col = index % 3;
        let x0 = panel.x0 + 16.0 + usize_to_f64_lossy(col) * (card_w + 8.0);
        let y0 = panel.y0 + 176.0 + usize_to_f64_lossy(row) * 96.0;
        Rect::new(x0, y0, x0 + card_w, y0 + 88.0)
    }
    pub fn action_geometry(panel: Rect, index: usize) -> Rect {
        let tile_w = (panel.width() - 56.0) / 4.0;
        let x0 = panel.x0 + 16.0 + usize_to_f64_lossy(index) * (tile_w + 8.0);
        Rect::new(x0, panel.y0 + 16.0, x0 + tile_w, panel.y0 + 72.0)
    }
    pub fn power_profile_geometry(panel: Rect) -> Rect {
        Rect::new(
            panel.x0 + 16.0,
            panel.y0 + 472.0,
            panel.x1 - 16.0,
            panel.y0 + 520.0,
        )
    }
    fn quick_settings_slider_geometry(&self, index: usize) -> (Rect, f64) {
        Self::slider_geometry(self.panel, index)
    }
    fn quick_settings_toggle_geometry(&self, index: usize) -> Rect {
        Self::toggle_geometry(self.panel, index)
    }
    fn quick_settings_action_geometry(&self, index: usize) -> Rect {
        Self::action_geometry(self.panel, index)
    }
    fn quick_settings_power_profile_geometry(&self) -> Rect {
        Self::power_profile_geometry(self.panel)
    }
    fn quick_settings_toggle_chevron_rect(&self, index: usize) -> Rect {
        let card = self.quick_settings_toggle_geometry(index);
        let cx = card.x1 - 16.0;
        let cy = card.y0 + 22.0;
        Rect::new(cx - 16.0, cy - 16.0, cx + 16.0, cy + 16.0)
    }

    fn draw_quick_settings_action_row(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        qs: &crate::shell::chrome_state::QuickSettingsState,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let white_10 = colors.text_primary.with_alpha_factor(0.10);
        for i in 0..4 {
            let tile = self.quick_settings_action_geometry(i);
            // Control role: 12px — the radius scale deliberately omits
            // 16, so a literal 16 here would match nothing else on screen.
            let tile_rr = RoundedRect::from_rect(
                tile,
                ui::chrome::ChromeRole::Control.radius(theme, tile.height()),
            );
            // Power tile's red wash must expose its r/g/b signature so
            // the concept-parity spec test detects it.
            let bg = if i == 3 {
                colors.semantic.danger.with_alpha_factor(0.20)
            } else {
                white_10
            };
            scene.fill(Fill::NonZero, transform, bg, None, &tile_rr);

            let glyph_color = if i == 3 {
                colors.semantic.danger
            } else {
                colors.text_primary
            };
            let glyph_rect = Rect::new(
                tile.x0 + (tile.width() - 20.0) / 2.0,
                tile.y0 + 7.0,
                tile.x0 + f64::midpoint(tile.width(), 20.0),
                tile.y0 + 27.0,
            );
            let glyph = match i {
                0 => ui::icons::Icon::Settings2,
                1 => ui::icons::Icon::Camera,
                2 => ui::icons::Icon::Bell,
                _ => ui::icons::Icon::Power,
            };
            ui::icons::draw_icon(
                scene,
                transform,
                glyph_rect,
                glyph,
                glyph_color,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            if i == 1 {
                Self::draw_quick_settings_privacy_indicator(scene, transform, tile, colors, qs);
            }
            let action_label = match i {
                0 => "Settings",
                1 if !qs.privacy.active_media.is_empty() => "Stop access",
                1 => "Privacy",
                2 => "History",
                _ => "Power",
            };
            let (label_w, _) = text_renderer.measure_weighted(
                action_label,
                tokens.typography.scale.xs,
                None,
                TextWeight::Medium,
            );
            // `Affine`'s `Mul` composes transform matrices with plain
            // float multiply-adds — it cannot overflow or panic — but
            // `arithmetic_side_effects` can't see that for a
            // non-primitive `Mul` impl.
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine composition is float matrix multiplication; it cannot overflow or panic"
            )]
            let label_xform = transform
                * Affine::translate((tile.center().x - f64::from(label_w) / 2.0, tile.y0 + 34.0));
            text_renderer.draw_text_weighted(
                scene,
                action_label,
                tokens.typography.scale.xs,
                glyph_color,
                label_xform,
                None,
                TextWeight::Medium,
            );
        }
    }

    /// The camera/mic privacy overlay on the Privacy action tile (index
    /// 1): a status dot, plus a mic-off glyph when muted or killed.
    fn draw_quick_settings_privacy_indicator(
        scene: &mut Scene,
        transform: Affine,
        tile: Rect,
        colors: &ui::tokens::colors::Colors,
        qs: &crate::shell::chrome_state::QuickSettingsState,
    ) {
        let blocked = !matches!(
            qs.privacy.camera_kill,
            os_actions::KillState::Allowed | os_actions::KillState::Unavailable
        );
        scene.fill(
            Fill::NonZero,
            transform,
            if blocked {
                colors.semantic.danger
            } else if !qs.privacy.active_media.is_empty() {
                colors.semantic.success
            } else {
                colors.text_dim.with_alpha_factor(0.45)
            },
            None,
            &vello::kurbo::Circle::new(Point::new(tile.x1 - 12.0, tile.y0 + 12.0), 4.0),
        );
        if qs.privacy.microphone_muted.unwrap_or(false)
            || !matches!(
                qs.privacy.microphone_kill,
                os_actions::KillState::Allowed | os_actions::KillState::Unavailable
            )
        {
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::new(tile.x0 + 7.0, tile.y0 + 7.0, tile.x0 + 21.0, tile.y0 + 21.0),
                ui::icons::Icon::MicOff,
                colors.semantic.danger,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
        }
    }

    /// Section 2: Slider card (Brightness / Volume).
    fn draw_quick_settings_slider_card(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
        qs: &crate::shell::chrome_state::QuickSettingsState,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let slider_card = Rect::new(
            rect.x0 + 16.0,
            rect.y0 + 88.0,
            rect.x1 - 16.0,
            rect.y0 + 88.0 + 72.0,
        );
        let white_05 = colors.text_primary.with_alpha_factor(0.05);
        scene.fill(
            Fill::NonZero,
            transform,
            white_05,
            None,
            &RoundedRect::from_rect(slider_card, 16.0),
        );
        let slider_values = [qs.brightness, qs.volume];
        for i in 0..2 {
            let (bar, row_y) = self.quick_settings_slider_geometry(i);
            let icon_x = rect.x0 + 16.0 + 12.0 + 8.0;
            let icon_cy = row_y + 12.0;
            let icon_rect = Rect::new(icon_x - 7.0, icon_cy - 7.0, icon_x + 7.0, icon_cy + 7.0);
            ui::icons::draw_icon(
                scene,
                transform,
                icon_rect,
                if i == 0 {
                    ui::icons::Icon::Sun
                } else {
                    ui::icons::Icon::Volume2
                },
                colors.text_dim,
                1.5,
            );

            // Solid white via `text_primary`, not indigo — the
            // concept-parity spec pins that delta to zero.
            let track_rr = RoundedRect::from_rect(bar, 3.0);
            scene.fill(
                Fill::NonZero,
                transform,
                colors.text_primary.with_alpha_factor(0.10),
                None,
                &track_rr,
            );
            let fill_w = bar.width()
                * f64::from(slider_values.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0));
            if fill_w > 0.0 {
                let filled = Rect::new(bar.x0, bar.y0, bar.x0 + fill_w, bar.y1);
                let filled_rr = RoundedRect::from_rect(filled, 3.0);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.text_primary,
                    None,
                    &filled_rr,
                );
            }
        }
    }

    /// The 9 toggle-card rows (on/off, label, sub-line) section 3 draws
    /// one card per. Split out of the drawing loop so each is a plain
    /// data computation.
    fn quick_settings_toggle_rows<'a>(
        qs: &'a crate::shell::chrome_state::QuickSettingsState,
        not_connected: &'a str,
    ) -> [(bool, &'a str, &'a str); 9] {
        let wifi_sub = if qs.wifi {
            qs.wifi_networks
                .iter()
                .find(|n| n.connected)
                .map_or(not_connected, |n| n.ssid.as_str())
        } else {
            not_connected
        };
        let bt_sub = if qs.bluetooth {
            qs.bt_devices
                .iter()
                .find(|d| d.connected)
                .map_or(not_connected, |d| d.name.as_str())
        } else {
            not_connected
        };
        let wired_on = qs.wired.as_ref().is_some_and(|wired| wired.connected);
        let wired_sub = qs
            .wired
            .as_ref()
            .and_then(|wired| wired.device.as_deref())
            .unwrap_or(if wired_on {
                "Connected"
            } else {
                "Not connected"
            });
        let vpn_active = qs.vpns.iter().any(|vpn| vpn.active);
        let vpn_sub = qs
            .vpns
            .iter()
            .find(|vpn| vpn.active)
            .or_else(|| qs.vpns.first())
            .map_or("No profiles", |vpn| vpn.name.as_str());
        let mobile_sub = if qs.radios.wwan_available {
            if qs.radios.wwan_enabled {
                "Enabled"
            } else {
                "Disabled"
            }
        } else {
            "Unavailable"
        };
        [
            (qs.wifi, "WLAN", wifi_sub),
            (qs.bluetooth, "Bluetooth", bt_sub),
            (wired_on, "LAN", wired_sub),
            (qs.radios.airplane_mode, "Airplane mode", "All radios"),
            (qs.radios.hotspot_active, "Hotspot", "qOS Hotspot"),
            (vpn_active, "VPN", vpn_sub),
            (qs.night_light, "Night Light", "Display comfort"),
            (false, "Displays", "Switch output"),
            (qs.radios.wwan_enabled, "Mobile data", mobile_sub),
        ]
    }

    /// Section 3: Toggle card grid. ON cards use an alpha wash rather
    /// than solid indigo-500 — the concept-parity spec pins the
    /// closed→open delta to zero.
    fn draw_quick_settings_toggle_grid(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        qs: &crate::shell::chrome_state::QuickSettingsState,
        not_connected: &str,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let toggle_rows = Self::quick_settings_toggle_rows(qs, not_connected);
        for (i, (on, label, sub_line)) in toggle_rows.iter().enumerate() {
            let card = self.quick_settings_toggle_geometry(i);
            let card_rr = RoundedRect::from_rect(card, 16.0);
            // ON → indigo wash; OFF → slate-800 — the wash keeps the
            // spec test's solid-indigo signature clear.
            let bg = if *on {
                colors.accent[5].with_alpha_factor(0.20)
            } else {
                colors.neutral[2]
            };
            scene.fill(Fill::NonZero, transform, bg, None, &card_rr);

            // Wifi for slot 0, Bluetooth for slot 1.
            let glyph_box = Rect::new(
                card.x0 + 12.0,
                card.y0 + 12.0,
                card.x0 + 12.0 + 20.0,
                card.y0 + 12.0 + 20.0,
            );
            // Slot 0 (Wifi) and slot 4 (Hotspot) both fall to the
            // catch-all Wifi glyph below, same as anything past slot 7
            // (e.g. Mobile data).
            let icon = match i {
                1 => ui::icons::Icon::Bluetooth,
                2 => ui::icons::Icon::EthernetPort,
                3 => ui::icons::Icon::WifiOff,
                5 => ui::icons::Icon::Shield,
                6 => ui::icons::Icon::Sun,
                7 => ui::icons::Icon::Monitor,
                _ => ui::icons::Icon::Wifi,
            };
            ui::icons::draw_icon(
                scene,
                transform,
                glyph_box,
                icon,
                colors.text_primary,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );

            // Trailing chevron — placeholder caret for now.
            let chev_cx = card.x1 - 16.0;
            let chev_y = card.y0 + 22.0;
            let chev = vello::kurbo::Circle::new(Point::new(chev_cx, chev_y), 3.0);
            scene.fill(
                Fill::NonZero,
                transform,
                colors.text_primary.with_alpha_factor(0.60),
                None,
                &chev,
            );

            text_renderer.draw_text_weighted(
                scene,
                label,
                tokens.typography.scale.sm,
                colors.text_primary,
                transform.pre_translate((card.x0 + 12.0, card.y1 - 36.0).into()),
                None,
                TextWeight::Semibold,
            );
            text_renderer.draw_text(
                scene,
                sub_line,
                tokens.typography.scale.xs,
                colors.text_primary.with_alpha_factor(0.70),
                transform.pre_translate((card.x0 + 12.0, card.y1 - 18.0).into()),
                None,
            );
        }
    }

    /// Section 4: Power Profile row.
    fn draw_quick_settings_power_profile(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        qs: &crate::shell::chrome_state::QuickSettingsState,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let white_05 = colors.text_primary.with_alpha_factor(0.05);
        let pp = self.quick_settings_power_profile_geometry();
        scene.fill(
            Fill::NonZero,
            transform,
            white_05,
            None,
            &RoundedRect::from_rect(pp, 16.0),
        );
        let battery_rect = Rect::new(
            pp.x0 + 12.0,
            pp.y0 + 14.0,
            pp.x0 + 12.0 + 20.0,
            pp.y0 + 34.0,
        );
        ui::icons::draw_icon(
            scene,
            transform,
            battery_rect,
            ui::icons::Icon::Battery,
            colors.semantic.success,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        let profile_label = qs.power_profile.label();
        text_renderer.draw_text_weighted(
            scene,
            profile_label,
            tokens.typography.scale.sm,
            colors.text_primary,
            transform.pre_translate((pp.x0 + 44.0, pp.y0 + 8.0).into()),
            None,
            TextWeight::Semibold,
        );
        let battery_summary = if qs.battery.present {
            match qs.battery.health_percent() {
                Some(health) => format!("{}% battery · {health}% health", qs.battery.capacity),
                None => format!("{}% battery", qs.battery.capacity),
            }
        } else {
            "AC power".to_string()
        };
        let power_detail = match qs.firmware_updates {
            Some(count) if count > 0 => format!("{battery_summary} · {count} firmware update(s)"),
            _ => battery_summary,
        };
        text_renderer.draw_text(
            scene,
            &power_detail,
            tokens.typography.scale.xs,
            colors.text_dim,
            transform.pre_translate((pp.x0 + 44.0, pp.y0 + 26.0).into()),
            None,
        );
        let chev_cx = pp.x1 - 16.0;
        let chev_y = f64::midpoint(pp.y0, pp.y1);
        let chev = vello::kurbo::Circle::new(Point::new(chev_cx, chev_y), 3.0);
        scene.fill(Fill::NonZero, transform, colors.text_dim, None, &chev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PANEL: Rect = Rect::new(100.0, 48.0, 420.0, 600.0);
    #[test]
    fn chevron_navigation_wins_over_toggle_card() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        let wifi_before = state.wifi;
        let card = QuickSettingsRoot::toggle_geometry(PANEL, 0);
        let point = Point::new(card.x1 - 16.0, card.y0 + 22.0);
        assert_eq!(
            root.handle_click(point, PANEL, &mut state),
            RootAction::Enter(QsLevel::Wifi)
        );
        assert_eq!(state.wifi, wifi_before);
    }
    #[test]
    fn slider_drag_updates_and_commits_the_same_geometry() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        let (bar, _) = QuickSettingsRoot::slider_geometry(PANEL, 0);
        assert!(matches!(
            root.handle_click(bar.center(), PANEL, &mut state),
            RootAction::Changed(_)
        ));
        assert_eq!(state.brightness, 0.5);
        assert_eq!(
            root.finish_drag(&state),
            Some(SliderCommit::Brightness(50.0))
        );
        assert!(!root.drag_active());
    }
    #[test]
    fn unavailable_mobile_data_is_a_consumed_no_op() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        let card = QuickSettingsRoot::toggle_geometry(PANEL, 8);
        assert_eq!(
            root.handle_click(card.center(), PANEL, &mut state),
            RootAction::Changed(PendingRootActions::default())
        );
        assert!(!state.radios.wwan_enabled);
    }
}
