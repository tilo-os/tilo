//! Root page of the quick-settings popover.
//!
//! Owns root layout, painting, hit-testing and slider-drag presentation state.
//! Shell-wide effects remain outcomes for the overlay host to apply.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "the root uses fixed, tested action/slider/toggle index domains and guards subtraction by their range branches"
)]

use crate::shell::chrome_state::QuickSettingsState;
use crate::shell::quick_settings::{PowerProfile, QsLevel};
use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, Slider, SliderFace, Toggle, ToggleFace};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
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
    /// The immediate-mode root still needs a real keyboard focus model.  The
    /// index follows paint order: four actions, two sliders, nine system
    /// controls, then power profile.
    keyboard_focus: Option<usize>,
}

impl QuickSettingsRoot {
    const ACTION_COUNT: usize = 4;
    const SLIDER_COUNT: usize = 2;
    const TOGGLE_COUNT: usize = 9;
    const POWER_FOCUS: usize = Self::ACTION_COUNT + Self::SLIDER_COUNT + Self::TOGGLE_COUNT;
    const WIFI_DETAILS_FOCUS: usize = Self::POWER_FOCUS + 1;
    const BLUETOOTH_DETAILS_FOCUS: usize = Self::POWER_FOCUS + 2;
    const FOCUSABLE_COUNT: usize = Self::BLUETOOTH_DETAILS_FOCUS + 1;

    pub fn reset_focus(&mut self) {
        self.keyboard_focus = Some(0);
    }

    pub fn focus_next(&mut self, state: &QuickSettingsState, backwards: bool) -> bool {
        let mut next = match self.keyboard_focus {
            Some(current) if backwards => {
                current.checked_sub(1).unwrap_or(Self::FOCUSABLE_COUNT - 1)
            }
            Some(current) => (current + 1) % Self::FOCUSABLE_COUNT,
            None if backwards => Self::FOCUSABLE_COUNT - 1,
            None => 0,
        };
        // Unavailable mobile data is visible status, but not a keyboard stop.
        for _ in 0..Self::FOCUSABLE_COUNT {
            if Self::focusable(next, state) {
                break;
            }
            next = if backwards {
                next.checked_sub(1).unwrap_or(Self::FOCUSABLE_COUNT - 1)
            } else {
                (next + 1) % Self::FOCUSABLE_COUNT
            };
        }
        self.keyboard_focus = Some(next);
        true
    }

    #[cfg(test)]
    pub fn focused_index(&self) -> Option<usize> {
        self.keyboard_focus
    }

    /// Activate the focused non-range control through exactly the same root
    /// action path as a pointer click. Sliders deliberately use
    /// [`Self::adjust_focused_slider`] so Enter never jumps a range to 50%.
    pub fn activate_focused(&mut self, panel: Rect, state: &mut QuickSettingsState) -> RootAction {
        self.panel = panel;
        let Some(index) = self.keyboard_focus else {
            return RootAction::None;
        };
        if !Self::focusable(index, state) {
            return RootAction::None;
        }
        if index == Self::WIFI_DETAILS_FOCUS {
            return RootAction::Enter(QsLevel::Wifi);
        }
        if index == Self::BLUETOOTH_DETAILS_FOCUS {
            return RootAction::Enter(QsLevel::Bluetooth);
        }
        if (Self::ACTION_COUNT..Self::ACTION_COUNT + Self::SLIDER_COUNT).contains(&index) {
            return RootAction::None;
        }
        let point = if index < Self::ACTION_COUNT {
            self.quick_settings_action_geometry(index).center()
        } else if index < Self::ACTION_COUNT + Self::SLIDER_COUNT + Self::TOGGLE_COUNT {
            self.quick_settings_toggle_geometry(index - Self::ACTION_COUNT - Self::SLIDER_COUNT)
                .center()
        } else {
            self.quick_settings_power_profile_geometry().center()
        };
        self.handle_click(point, self.panel, state)
    }

    /// Arrow-key adjustment for a focused range. `step` is deliberately
    /// small enough for accurate brightness/volume changes yet deterministic
    /// for keyboard and accessibility automation.
    pub fn adjust_focused_slider(
        &self,
        state: &mut QuickSettingsState,
        direction: f32,
    ) -> Option<SliderCommit> {
        let index = self.keyboard_focus?.checked_sub(Self::ACTION_COUNT)?;
        if index >= Self::SLIDER_COUNT {
            return None;
        }
        let value = if index == 0 {
            &mut state.brightness
        } else {
            &mut state.volume
        };
        *value = (*value + direction * 0.05).clamp(0.0, 1.0);
        Some(if index == 0 {
            SliderCommit::Brightness(*value * 100.0)
        } else {
            SliderCommit::Volume(*value * 100.0)
        })
    }

    fn focusable(index: usize, state: &QuickSettingsState) -> bool {
        index != Self::ACTION_COUNT + Self::SLIDER_COUNT + 8 || state.radios.wwan_available
    }

    /// Semantic mirror for the root's immediate-mode controls. Values,
    /// checked states, disabled status and focus are derived from the exact
    /// state and indices used by paint/hit-testing, so assistive technology
    /// cannot announce a stale alternate model.
    pub fn accessibility_snapshot(&self, state: &QuickSettingsState) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};

        let focused = |index| self.keyboard_focus == Some(index);
        let mut root = AccessNode::new(Role::Dialog)
            .with_name("Quick Settings")
            .with_description("Frequently used system controls")
            .with_state(AccessState::new().modal(true));
        for (index, name) in [
            "Open Settings",
            "Privacy controls",
            "Notification history",
            "Power menu",
        ]
        .into_iter()
        .enumerate()
        {
            root.children.push(
                AccessNode::new(Role::Button)
                    .with_name(name)
                    .with_state(AccessState::new().focusable(true).focused(focused(index)))
                    .with_position(index + 1, Self::FOCUSABLE_COUNT),
            );
        }
        for (index, (name, value)) in [("Brightness", state.brightness), ("Volume", state.volume)]
            .into_iter()
            .enumerate()
        {
            let focus_index = Self::ACTION_COUNT + index;
            root.children.push(
                AccessNode::new(Role::Slider)
                    .with_name(name)
                    .with_value(format!("{:.0}%", value * 100.0))
                    .with_description("Use Left and Right Arrow to adjust")
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(focused(focus_index)),
                    )
                    .with_position(focus_index + 1, Self::FOCUSABLE_COUNT),
            );
        }
        let rows = Self::quick_settings_toggle_rows(state, "Not connected");
        for (index, (on, label, subline)) in rows.iter().enumerate() {
            let focus_index = Self::ACTION_COUNT + Self::SLIDER_COUNT + index;
            let (role, enabled) = if matches!(index, 5 | 7) {
                (Role::Button, true)
            } else {
                (Role::Switch, Self::focusable(focus_index, state))
            };
            let mut control = AccessNode::new(role)
                .with_name(*label)
                .with_description(*subline)
                .with_state(
                    AccessState::new()
                        .focusable(enabled)
                        .focused(enabled && focused(focus_index))
                        .disabled(!enabled),
                )
                .with_position(focus_index + 1, Self::FOCUSABLE_COUNT);
            if role == Role::Switch {
                control.state.checked = Some(*on);
            }
            root.children.push(control);
        }
        root.children.push(
            AccessNode::new(Role::Button)
                .with_name("Change power profile")
                .with_description(state.power_profile.label())
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(focused(Self::POWER_FOCUS)),
                )
                .with_position(Self::POWER_FOCUS + 1, Self::FOCUSABLE_COUNT),
        );
        for (focus_index, name) in [
            (Self::WIFI_DETAILS_FOCUS, "Wi-Fi details"),
            (Self::BLUETOOTH_DETAILS_FOCUS, "Bluetooth details"),
        ] {
            root.children.push(
                AccessNode::new(Role::Button)
                    .with_name(name)
                    .with_description("Open the available networks and connection details")
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(focused(focus_index)),
                    )
                    .with_position(focus_index + 1, Self::FOCUSABLE_COUNT),
            );
        }
        root
    }

    pub fn accessibility_rect_for_path(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [] => Some(self.panel),
            [index] if *index < Self::ACTION_COUNT => {
                Some(self.quick_settings_action_geometry(*index))
            }
            [index] if *index < Self::ACTION_COUNT + Self::SLIDER_COUNT => {
                let slider = *index - Self::ACTION_COUNT;
                let (bar, row_y) = self.quick_settings_slider_geometry(slider);
                Some(Rect::new(bar.x0 - 5.0, row_y, bar.x1 + 5.0, row_y + 24.0))
            }
            [index] if *index < Self::ACTION_COUNT + Self::SLIDER_COUNT + Self::TOGGLE_COUNT => {
                Some(self.quick_settings_toggle_geometry(
                    *index - Self::ACTION_COUNT - Self::SLIDER_COUNT,
                ))
            }
            [Self::WIFI_DETAILS_FOCUS] => Some(self.quick_settings_toggle_chevron_rect(0)),
            [Self::BLUETOOTH_DETAILS_FOCUS] => Some(self.quick_settings_toggle_chevron_rect(1)),
            [index] if *index == Self::POWER_FOCUS => {
                Some(self.quick_settings_power_profile_geometry())
            }
            _ => None,
        }
    }

    pub fn focus_accessibility_path(&mut self, path: &[usize], state: &QuickSettingsState) -> bool {
        let [index] = path else {
            return false;
        };
        if *index >= Self::FOCUSABLE_COUNT || !Self::focusable(*index, state) {
            return false;
        }
        self.keyboard_focus = Some(*index);
        true
    }

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

    #[allow(
        clippy::too_many_arguments,
        reason = "the root draw entry point needs the renderer context plus panel interaction state"
    )]
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        cursor: Option<Point>,
        state: &QuickSettingsState,
    ) {
        self.panel = panel;
        self.draw_quick_settings_action_row(scene, transform, text_renderer, theme, cursor, state);
        self.draw_quick_settings_slider_card(
            scene,
            transform,
            text_renderer,
            theme,
            panel,
            cursor,
            state,
        );
        self.draw_quick_settings_toggle_grid(
            scene,
            transform,
            text_renderer,
            theme,
            cursor,
            state,
            "Not connected",
        );
        self.draw_quick_settings_power_profile(
            scene,
            transform,
            text_renderer,
            theme,
            cursor,
            state,
        );
    }

    pub fn slider_geometry(panel: Rect, index: usize) -> (Rect, f64) {
        let scale = Self::vertical_scale(panel);
        let row_y = panel.y0 + 100.0 * scale + usize_to_f64_lossy(index) * 36.0 * scale;
        let bar_y_mid = row_y + 12.0 * scale;
        (
            Rect::new(
                panel.x0 + 44.0,
                bar_y_mid - 3.0 * scale,
                panel.x1 - 16.0,
                bar_y_mid + 3.0 * scale,
            ),
            row_y,
        )
    }
    pub fn toggle_geometry(panel: Rect, index: usize) -> Rect {
        let scale = Self::vertical_scale(panel);
        let card_w = (panel.width() - 48.0) / 3.0;
        let row = index / 3;
        let col = index % 3;
        let x0 = panel.x0 + 16.0 + usize_to_f64_lossy(col) * (card_w + 8.0);
        let y0 = panel.y0 + 176.0 * scale + usize_to_f64_lossy(row) * 96.0 * scale;
        Rect::new(x0, y0, x0 + card_w, y0 + 88.0 * scale)
    }
    pub fn action_geometry(panel: Rect, index: usize) -> Rect {
        let scale = Self::vertical_scale(panel);
        let tile_w = (panel.width() - 56.0) / 4.0;
        let x0 = panel.x0 + 16.0 + usize_to_f64_lossy(index) * (tile_w + 8.0);
        Rect::new(
            x0,
            panel.y0 + 16.0 * scale,
            x0 + tile_w,
            panel.y0 + 72.0 * scale,
        )
    }
    pub fn power_profile_geometry(panel: Rect) -> Rect {
        let scale = Self::vertical_scale(panel);
        Rect::new(
            panel.x0 + 16.0,
            panel.y0 + 472.0 * scale,
            panel.x1 - 16.0,
            panel.y0 + 520.0 * scale,
        )
    }

    /// A short desktop/mobile viewport compresses vertical whitespace and
    /// cards proportionally, never moving the last control below the visible
    /// popover. At the standard 552px height this is exactly 1.0.
    fn vertical_scale(panel: Rect) -> f64 {
        (panel.height() / 552.0).clamp(0.72, 1.0)
    }
    pub fn toggle_switch_geometry(panel: Rect, index: usize) -> Option<Rect> {
        // Wi-Fi and Bluetooth expose a details list, VPN is navigation, and
        // Displays is an action. A 3-column utility panel cannot fit both a
        // 32px details target and a trustworthy switch without overlap, so
        // those cards keep their established whole-card toggle + explicit
        // chevron detail target instead.
        if matches!(index, 0 | 1 | 5 | 7) {
            return None;
        }
        let card = Self::toggle_geometry(panel, index);
        Some(Rect::new(
            card.x1 - 52.0,
            card.y0 + 12.0,
            card.x1 - 12.0,
            card.y0 + 34.0,
        ))
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
        cursor: Option<Point>,
        qs: &crate::shell::chrome_state::QuickSettingsState,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        for i in 0..4 {
            let tile = self.quick_settings_action_geometry(i);
            // The labels are deliberately drawn below: at the 320px utility
            // panel width a name such as "Stop access" needs an ellipsized
            // visual treatment. The shared Button still owns every button
            // face (surface, hover, focus and disabled rules).
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(tile, "")
                    .style(if i == 3 {
                        ui::widget::ButtonStyle::Danger
                    } else {
                        ui::widget::ButtonStyle::Neutral
                    })
                    .radius(ui::chrome::ChromeRole::Control.radius(theme, tile.height()))
                    .hovered(cursor.is_some_and(|point| tile.contains(point)))
                    .focused(self.keyboard_focus == Some(i))
                    .accessible_name(match i {
                        0 => "Open Settings",
                        1 => "Privacy controls",
                        2 => "Notification history",
                        _ => "Power menu",
                    }),
            );

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
            let action_label = text_renderer.elide_to_width(
                action_label,
                tokens.typography.scale.xs,
                f64_to_f32_lossy((tile.width() - 8.0).max(1.0)),
            );
            let (label_w, _) = text_renderer.measure_weighted(
                &action_label,
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
                &action_label,
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
    #[allow(
        clippy::too_many_arguments,
        reason = "slider painting needs the shared renderer context plus panel and interaction state"
    )]
    fn draw_quick_settings_slider_card(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        cursor: Option<Point>,
        qs: &crate::shell::chrome_state::QuickSettingsState,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let scale = Self::vertical_scale(rect);
        let slider_card = Rect::new(
            rect.x0 + 16.0,
            rect.y0 + 88.0 * scale,
            rect.x1 - 16.0,
            rect.y0 + (88.0 + 72.0) * scale,
        );
        Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ButtonFace::new(slider_card, "")
                .radius(ui::chrome::ChromeRole::Control.radius(theme, slider_card.height()))
                .accessible_name("Display and sound levels"),
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

            // SliderFace takes a generous row rect and derives the narrow
            // track itself, so pointer, focus, pressed halo and visual
            // geometry share the UI toolkit's one implementation.
            let face = Rect::new(bar.x0 - 5.0, row_y, bar.x1 + 5.0, row_y + 24.0);
            Slider::paint(
                scene,
                transform,
                theme,
                &SliderFace::new(
                    face,
                    f64::from(slider_values.get(i).copied().unwrap_or(0.0)),
                )
                .hovered(cursor.is_some_and(|point| face.contains(point)))
                .dragging(self.slider_drag == Some(i))
                .focused(self.keyboard_focus == Some(Self::ACTION_COUNT + i)),
            );
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
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "the fixed toggle grid shares one geometry pass across paint, focus and hit-target publication"
    )]
    fn draw_quick_settings_toggle_grid(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        cursor: Option<Point>,
        qs: &crate::shell::chrome_state::QuickSettingsState,
        not_connected: &str,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let toggle_rows = Self::quick_settings_toggle_rows(qs, not_connected);
        for (i, (on, label, sub_line)) in toggle_rows.iter().enumerate() {
            let card = self.quick_settings_toggle_geometry(i);
            let focus_index = Self::ACTION_COUNT + Self::SLIDER_COUNT + i;
            let is_disabled = i == 8 && !qs.radios.wwan_available;
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(card, "")
                    .radius(ui::chrome::ChromeRole::Control.radius(theme, card.height()))
                    .hovered(cursor.is_some_and(|point| card.contains(point)))
                    .focused(self.keyboard_focus == Some(focus_index))
                    .disabled(is_disabled)
                    .accessible_name(label),
            );

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

            if let Some(toggle) = Self::toggle_switch_geometry(self.panel, i) {
                Toggle::paint(
                    scene,
                    transform,
                    theme,
                    &ToggleFace::new(toggle, *on)
                        .hovered(cursor.is_some_and(|point| card.contains(point)))
                        .focused(self.keyboard_focus == Some(focus_index))
                        .disabled(is_disabled),
                );
            } else {
                // A real chevron denotes navigation. It no longer appears on
                // ordinary toggles, where it misleadingly competed with the
                // switch state.
                let chevron = self.quick_settings_toggle_chevron_rect(i);
                let details_focus = match i {
                    0 => Some(Self::WIFI_DETAILS_FOCUS),
                    1 => Some(Self::BLUETOOTH_DETAILS_FOCUS),
                    _ => None,
                };
                Button::paint(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    &ButtonFace::new(chevron, "")
                        .ghost()
                        .hovered(cursor.is_some_and(|point| chevron.contains(point)))
                        .focused(
                            details_focus.is_some_and(|focus| self.keyboard_focus == Some(focus)),
                        )
                        .accessible_name(match i {
                            0 => "Wi-Fi details",
                            1 => "Bluetooth details",
                            5 => "VPN details",
                            _ => "More details",
                        }),
                );
                ui::icons::draw_icon(
                    scene,
                    transform,
                    Rect::new(
                        card.x1 - 28.0,
                        card.y0 + 14.0,
                        card.x1 - 12.0,
                        card.y0 + 30.0,
                    ),
                    ui::icons::Icon::ChevronRight,
                    colors.text_dim,
                    ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }

            let text_width = (card.width() - 24.0).max(1.0);
            let label = text_renderer.elide_to_width(
                label,
                tokens.typography.scale.sm,
                f64_to_f32_lossy(text_width),
            );
            let sub_line = text_renderer.elide_to_width(
                sub_line,
                tokens.typography.scale.xs,
                f64_to_f32_lossy(text_width),
            );
            text_renderer.draw_text_weighted(
                scene,
                &label,
                tokens.typography.scale.sm,
                colors.text_primary,
                transform.pre_translate((card.x0 + 12.0, card.y1 - 36.0).into()),
                None,
                TextWeight::Semibold,
            );
            text_renderer.draw_text(
                scene,
                &sub_line,
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
        cursor: Option<Point>,
        qs: &crate::shell::chrome_state::QuickSettingsState,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let pp = self.quick_settings_power_profile_geometry();
        Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ButtonFace::new(pp, "")
                .radius(ui::chrome::ChromeRole::Control.radius(theme, pp.height()))
                .hovered(cursor.is_some_and(|point| pp.contains(point)))
                .focused(self.keyboard_focus == Some(Self::POWER_FOCUS))
                .accessible_name("Change power profile"),
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
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::new(pp.x1 - 28.0, pp.y0 + 16.0, pp.x1 - 12.0, pp.y0 + 32.0),
            ui::icons::Icon::ChevronRight,
            colors.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
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

    #[test]
    fn keyboard_keeps_wifi_toggle_and_details_as_separate_actions() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        root.reset_focus();
        for _ in 0..6 {
            root.focus_next(&state, false);
        }
        assert_eq!(root.focused_index(), Some(6));
        assert!(matches!(
            root.activate_focused(PANEL, &mut state),
            RootAction::Changed(PendingRootActions { wifi: Some(_), .. })
        ));

        for _ in 0..9 {
            root.focus_next(&state, false);
        }
        assert_eq!(
            root.focused_index(),
            Some(QuickSettingsRoot::WIFI_DETAILS_FOCUS)
        );
        assert_eq!(
            root.activate_focused(PANEL, &mut state),
            RootAction::Enter(QsLevel::Wifi)
        );
    }

    #[test]
    fn keyboard_can_open_bluetooth_details() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        root.reset_focus();
        for _ in 0..16 {
            root.focus_next(&state, false);
        }
        assert_eq!(
            root.focused_index(),
            Some(QuickSettingsRoot::BLUETOOTH_DETAILS_FOCUS)
        );
        assert_eq!(
            root.activate_focused(PANEL, &mut state),
            RootAction::Enter(QsLevel::Bluetooth)
        );
    }

    #[test]
    fn short_popover_keeps_power_and_every_card_on_screen() {
        let short = Rect::new(12.0, 48.0, 308.0, 556.0);
        let power = QuickSettingsRoot::power_profile_geometry(short);
        assert!(power.y1 <= short.y1, "power must remain reachable");
        for index in 0..9 {
            let card = QuickSettingsRoot::toggle_geometry(short, index);
            assert!(card.y0 >= short.y0 && card.y1 <= short.y1);
            if index >= 3 {
                let previous = QuickSettingsRoot::toggle_geometry(short, index - 3);
                assert!(previous.y1 < card.y0, "grid rows must not overlap");
            }
        }
    }

    #[test]
    fn accessibility_exposes_details_buttons_separately_from_switches() {
        let mut root = QuickSettingsRoot::default();
        let state = QuickSettingsState::default();
        root.reset_focus();
        let snapshot = root.accessibility_snapshot(&state);
        assert_eq!(snapshot.children[6].role, ui::a11y::Role::Switch);
        assert_eq!(
            snapshot.children[QuickSettingsRoot::WIFI_DETAILS_FOCUS]
                .name
                .as_deref(),
            Some("Wi-Fi details")
        );
    }

    #[test]
    fn accessibility_cannot_focus_unavailable_mobile_data() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        let mobile = QuickSettingsRoot::ACTION_COUNT + QuickSettingsRoot::SLIDER_COUNT + 8;

        assert!(!root.focus_accessibility_path(&[mobile], &state));
        assert_eq!(root.focused_index(), None);

        state.radios.wwan_available = true;
        assert!(root.focus_accessibility_path(&[mobile], &state));
        assert_eq!(root.focused_index(), Some(mobile));
    }
}
