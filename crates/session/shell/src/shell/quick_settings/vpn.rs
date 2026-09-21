//! VPN drill-in for quick settings.
//!
//! Owns its painted geometry, shared toggle faces, hit-testing, optimistic
//! local state and the OS dispatch. The overlay host only supplies the panel
//! bounds and maps `Back`/`Changed` into navigation and chrome invalidation.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, Toggle, ToggleFace};

use crate::shell::quick_settings::actions::{QsActions, log_qs_result};
use crate::shell::quick_settings::drill_in::{DrillInList, DrillInOutcome};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VpnPaneOutcome {
    DrillIn(DrillInOutcome),
    Reconcile,
    ActionFailed { profile: String, error: String },
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "bounded screen-space widths retain sufficient f32 precision"
)]
fn f64_to_f32_lossy(value: f64) -> f32 {
    value as f32
}

#[derive(Default)]
pub struct QuickSettingsVpnPane {
    list: DrillInList,
    keyboard_focus: Option<usize>,
}

impl QuickSettingsVpnPane {
    const BACK_FOCUS: usize = 0;
    const PROFILE_FOCUS_START: usize = 1;

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
        profiles: &[os_actions::VpnConnectionInfo],
    ) -> bool {
        let [0, index] = path else {
            return false;
        };
        if *index >= Self::PROFILE_FOCUS_START.saturating_add(profiles.len()) {
            return false;
        }
        self.keyboard_focus = Some(*index);
        if let Some(profile) = index.checked_sub(Self::PROFILE_FOCUS_START) {
            self.list
                .ensure_visible(panel, profiles.len(), 56.0, profile);
        }
        true
    }

    pub fn semantic_rects(
        &self,
        panel: Rect,
        profiles: &[os_actions::VpnConnectionInfo],
    ) -> Vec<(String, Rect)> {
        let mut rects = vec![("quick_settings_vpn_back".into(), Self::back_rect(panel))];
        rects.extend(
            self.list
                .visible_rects(panel, profiles.len(), 56.0)
                .into_iter()
                .map(|(index, rect)| (format!("quick_settings_vpn_profile[{index}]"), rect)),
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
            let profile = index.checked_sub(Self::PROFILE_FOCUS_START)?;
            self.list
                .visible_rects(panel, self.list.row_count(), 56.0)
                .into_iter()
                .find_map(|(index, rect)| (index == profile).then_some(rect))
        }
    }

    pub fn accessibility_snapshot(
        &self,
        profiles: &[os_actions::VpnConnectionInfo],
    ) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};

        let mut children = vec![
            AccessNode::new(Role::Button)
                .with_name("Back")
                .with_state(AccessState::new().focusable(true)),
        ];
        children.extend(profiles.iter().enumerate().map(|(index, profile)| {
            AccessNode::new(Role::Switch)
                .with_name(profile.name.clone())
                .with_description(if profile.active {
                    "VPN connected"
                } else {
                    "VPN disconnected"
                })
                .with_position(index.saturating_add(1), profiles.len())
                .with_state(AccessState::new().focusable(true).checked(profile.active))
        }));
        AccessNode::new(Role::Dialog)
            .with_name("VPN")
            .with_description(if profiles.is_empty() {
                "No VPN profiles configured"
            } else {
                "Connect or disconnect a configured VPN profile"
            })
            .with_state(AccessState::new().modal(true))
            .with_children(children)
    }

    pub fn focus_next(
        &mut self,
        panel: Rect,
        profiles: &[os_actions::VpnConnectionInfo],
        backwards: bool,
    ) {
        let count = Self::PROFILE_FOCUS_START.saturating_add(profiles.len());
        self.keyboard_focus = Some(match (self.keyboard_focus, backwards) {
            (Some(index), true) => index.checked_sub(1).unwrap_or(count.saturating_sub(1)),
            (Some(index), false) => index.saturating_add(1) % count,
            (None, true) => count.saturating_sub(1),
            (None, false) => 0,
        });
        if let Some(profile) = self
            .keyboard_focus
            .and_then(|index| index.checked_sub(Self::PROFILE_FOCUS_START))
        {
            self.list
                .ensure_visible(panel, profiles.len(), 56.0, profile);
        }
    }

    pub fn scroll_wheel(
        &mut self,
        panel: Rect,
        profiles: &[os_actions::VpnConnectionInfo],
        rows: f64,
    ) -> bool {
        self.list.scroll_wheel(panel, profiles.len(), 56.0, rows)
    }

    pub fn activate_focused(
        &mut self,
        panel: Rect,
        profiles: &mut [os_actions::VpnConnectionInfo],
        actions: &dyn QsActions,
    ) -> VpnPaneOutcome {
        let Some(index) = self.keyboard_focus else {
            return VpnPaneOutcome::DrillIn(DrillInOutcome::None);
        };
        let point = if index == Self::BACK_FOCUS {
            Self::back_rect(panel).center()
        } else {
            self.list.layout(panel, profiles.len());
            let profile = index.saturating_sub(Self::PROFILE_FOCUS_START);
            self.list
                .ensure_visible(panel, profiles.len(), 56.0, profile);
            let Some(row) = self.list.row_for(profile) else {
                return VpnPaneOutcome::DrillIn(DrillInOutcome::None);
            };
            row.center()
        };
        self.handle_click(point, panel, profiles, actions)
    }

    pub fn handle_key(
        &mut self,
        logical_key: &Key,
        backwards: bool,
        panel: Rect,
        profiles: &mut [os_actions::VpnConnectionInfo],
        actions: &dyn QsActions,
    ) -> Option<VpnPaneOutcome> {
        match logical_key {
            Key::Named(NamedKey::Tab) => {
                self.focus_next(panel, profiles, backwards);
                Some(VpnPaneOutcome::DrillIn(DrillInOutcome::None))
            }
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowLeft) => {
                self.focus_next(panel, profiles, true);
                Some(VpnPaneOutcome::DrillIn(DrillInOutcome::None))
            }
            Key::Named(NamedKey::ArrowDown | NamedKey::ArrowRight) => {
                self.focus_next(panel, profiles, false);
                Some(VpnPaneOutcome::DrillIn(DrillInOutcome::None))
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                Some(self.activate_focused(panel, profiles, actions))
            }
            _ => None,
        }
    }

    pub fn handle_click(
        &mut self,
        point: Point,
        panel: Rect,
        profiles: &mut [os_actions::VpnConnectionInfo],
        actions: &dyn QsActions,
    ) -> VpnPaneOutcome {
        self.list.layout(panel, profiles.len());
        if self.list.back_contains(point) {
            return VpnPaneOutcome::DrillIn(DrillInOutcome::Back);
        }
        let Some(index) = self.list.row_at(point) else {
            return VpnPaneOutcome::DrillIn(DrillInOutcome::None);
        };
        let Some(profile) = profiles.get_mut(index) else {
            return VpnPaneOutcome::DrillIn(DrillInOutcome::None);
        };
        let next = !profile.active;
        let result = actions.set_vpn_connection(&profile.name, next);
        if let Err(error) = result {
            log_qs_result("set_vpn_connection", Err(error.clone()));
            return VpnPaneOutcome::ActionFailed {
                profile: profile.name.clone(),
                error,
            };
        }
        log_qs_result("set_vpn_connection", Ok(()));
        profile.active = next;
        VpnPaneOutcome::Reconcile
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, matching the sibling \
                  wifi/bluetooth drill-in panes this is called alongside"
    )]
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        cursor: Option<Point>,
        profiles: &[os_actions::VpnConnectionInfo],
    ) {
        self.list.layout(panel, profiles.len());
        let colors = &theme.tokens.colors;
        self.list
            .draw_header(scene, transform, text, theme, panel, cursor, "VPN");
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
        if profiles.is_empty() {
            DrillInList::draw_empty(
                scene,
                transform,
                text,
                theme,
                panel,
                "No VPN profiles yet · add one in Settings",
            );
            return;
        }

        for (index, row) in self.list.visible_indices().zip(self.list.rows().iter()) {
            let Some(profile) = profiles.get(index) else {
                continue;
            };
            let hovered = cursor.is_some_and(|point| row.contains(point));
            // A discovery row is not a selected tab. Wi-Fi and Bluetooth
            // stay quiet and reserve blue for an actual live state; VPN is
            // a control row, so its enabled state belongs to the trailing
            // switch while the text reads "Connected". Keeping the surface
            // neutral avoids saying the same thing three different ways.
            let fill = if hovered {
                colors.neutral[3].with_alpha_factor(0.72)
            } else {
                colors.neutral[2].with_alpha_factor(0.42)
            };
            scene.fill(
                Fill::NonZero,
                transform,
                fill,
                None,
                &RoundedRect::from_rect(*row, 12.0),
            );
            if self.keyboard_focus == Some(Self::PROFILE_FOCUS_START.saturating_add(index)) {
                Button::paint(
                    scene,
                    transform,
                    text,
                    theme,
                    &ButtonFace::new(*row, "")
                        .ghost()
                        .focused(true)
                        .accessible_name(&profile.name),
                );
            }
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::new(row.x0 + 12.0, row.y0 + 16.0, row.x0 + 32.0, row.y0 + 36.0),
                ui::icons::Icon::Shield,
                colors.text_dim,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            let toggle = Rect::new(
                row.x1 - 56.0,
                row.center().y - 12.0,
                row.x1 - 12.0,
                row.center().y + 12.0,
            );
            Toggle::paint(
                scene,
                transform,
                theme,
                &ToggleFace::new(toggle, profile.active).hovered(hovered),
            );
            let text_width = (toggle.x0 - 12.0 - (row.x0 + 44.0)).max(1.0);
            let name = text.elide_to_width(
                &profile.name,
                theme.tokens.typography.scale.sm,
                f64_to_f32_lossy(text_width),
            );
            text.draw_text_weighted(
                scene,
                &name,
                theme.tokens.typography.scale.sm,
                colors.text_primary,
                transform.pre_translate((row.x0 + 44.0, row.y0 + 9.0).into()),
                None,
                TextWeight::Medium,
            );
            text.draw_text(
                scene,
                if profile.active {
                    "Connected"
                } else {
                    "Disconnected"
                },
                theme.tokens.typography.scale.xs,
                if profile.active {
                    colors.accent[5]
                } else {
                    colors.text_dim
                },
                transform.pre_translate((row.x0 + 44.0, row.y0 + 29.0).into()),
                Some(f64_to_f32_lossy(text_width)),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordingActions {
        calls: RefCell<Vec<(String, bool)>>,
        reject: bool,
    }

    impl os_actions::Dispatcher for RecordingActions {
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
        fn set_power_profile(&self, _: os_actions::PowerProfile) -> Result<(), String> {
            Ok(())
        }
        fn set_vpn_connection(&self, name: &str, enabled: bool) -> Result<(), String> {
            self.calls.borrow_mut().push((name.to_owned(), enabled));
            if self.reject {
                Err("VPN transition rejected".into())
            } else {
                Ok(())
            }
        }
    }

    fn profile(active: bool) -> os_actions::VpnConnectionInfo {
        os_actions::VpnConnectionInfo {
            name: "Work".into(),
            uuid: "test-work-vpn-uuid".into(),
            active,
            kind: "wireguard".into(),
        }
    }

    #[test]
    fn row_click_dispatches_and_updates_the_matching_profile() {
        let mut pane = QuickSettingsVpnPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut profiles = vec![profile(false)];
        let actions = RecordingActions::default();
        pane.list.layout(panel, profiles.len());
        let outcome =
            pane.handle_click(pane.list.rows()[0].center(), panel, &mut profiles, &actions);
        assert_eq!(outcome, VpnPaneOutcome::Reconcile);
        assert!(profiles[0].active);
        assert_eq!(actions.calls.borrow().as_slice(), [("Work".into(), true)]);
    }

    #[test]
    fn back_and_empty_space_do_not_dispatch_hardware_actions() {
        let mut pane = QuickSettingsVpnPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut profiles = vec![profile(true)];
        let actions = RecordingActions::default();
        pane.list.layout(panel, profiles.len());
        assert_eq!(
            pane.handle_click(pane.list.back_center(), panel, &mut profiles, &actions),
            VpnPaneOutcome::DrillIn(DrillInOutcome::Back)
        );
        assert_eq!(
            pane.handle_click(panel.center(), panel, &mut profiles, &actions),
            VpnPaneOutcome::DrillIn(DrillInOutcome::None)
        );
        assert!(actions.calls.borrow().is_empty());
        assert!(profiles[0].active);
    }

    #[test]
    fn failed_action_does_not_publish_a_false_connected_state() {
        let mut pane = QuickSettingsVpnPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut profiles = vec![profile(false)];
        let actions = RecordingActions {
            reject: true,
            ..Default::default()
        };
        pane.list.layout(panel, profiles.len());

        assert_eq!(
            pane.handle_click(pane.list.rows()[0].center(), panel, &mut profiles, &actions),
            VpnPaneOutcome::ActionFailed {
                profile: "Work".into(),
                error: "VPN transition rejected".into(),
            }
        );
        assert!(!profiles[0].active);
        assert_eq!(actions.calls.borrow().as_slice(), [("Work".into(), true)]);
    }

    #[test]
    fn keyboard_wraps_to_a_profile_and_uses_the_same_action_path() {
        let mut pane = QuickSettingsVpnPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut profiles = vec![profile(false)];
        let actions = RecordingActions::default();

        assert_eq!(
            pane.handle_key(
                &Key::Named(NamedKey::Tab),
                true,
                panel,
                &mut profiles,
                &actions,
            ),
            Some(VpnPaneOutcome::DrillIn(DrillInOutcome::None))
        );
        assert_eq!(
            pane.handle_key(
                &Key::Named(NamedKey::Enter),
                false,
                panel,
                &mut profiles,
                &actions,
            ),
            Some(VpnPaneOutcome::Reconcile)
        );
        assert!(profiles[0].active);

        pane.reset_focus();
        let _ = pane.handle_key(
            &Key::Named(NamedKey::ArrowDown),
            false,
            panel,
            &mut profiles,
            &actions,
        );
        assert_eq!(
            pane.handle_key(
                &Key::Named(NamedKey::Space),
                false,
                panel,
                &mut profiles,
                &actions,
            ),
            Some(VpnPaneOutcome::DrillIn(DrillInOutcome::Back))
        );
    }

    #[test]
    fn semantics_match_back_then_profiles() {
        let pane = QuickSettingsVpnPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let profiles = vec![profile(true)];
        let names = pane
            .semantic_rects(panel, &profiles)
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            ["quick_settings_vpn_back", "quick_settings_vpn_profile[0]"]
        );
        let snapshot = pane.accessibility_snapshot(&profiles);
        assert_eq!(snapshot.name.as_deref(), Some("VPN"));
        assert_eq!(snapshot.children.len(), names.len());
        assert_eq!(snapshot.children[1].name.as_deref(), Some("Work"));
        assert_eq!(
            snapshot.children[1].description.as_deref(),
            Some("VPN connected")
        );
    }

    #[test]
    fn compact_keyboard_focus_scrolls_the_last_profile_into_view() {
        let mut pane = QuickSettingsVpnPane::default();
        let panel = Rect::new(600.0, 40.0, 948.0, 240.0);
        let mut profiles = (0..8)
            .map(|index| os_actions::VpnConnectionInfo {
                name: format!("Profile {index}"),
                uuid: format!("profile-{index}"),
                active: false,
                kind: "wireguard".into(),
            })
            .collect::<Vec<_>>();
        let actions = RecordingActions::default();

        let _ = pane.handle_key(
            &Key::Named(NamedKey::Tab),
            true,
            panel,
            &mut profiles,
            &actions,
        );
        assert_eq!(
            pane.keyboard_focus,
            Some(QuickSettingsVpnPane::PROFILE_FOCUS_START + 7)
        );
        assert_eq!(pane.list.visible_indices(), 6..8);
        let names = pane
            .semantic_rects(panel, &profiles)
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"quick_settings_vpn_profile[7]".to_string()));
        assert!(!names.contains(&"quick_settings_vpn_profile[0]".to_string()));
    }
}
