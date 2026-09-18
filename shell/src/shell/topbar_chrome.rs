//! Native service adapter for the canonical portable topbar.
use crate::shell::chrome_state;
use qos_desktop::chrome::*;
pub use qos_desktop::topbar::*;
use std::rc::Rc;
use vello::kurbo::Rect;

pub fn native_source(chrome: chrome_state::Chrome) -> Chrome {
    Rc::new(NativeChrome(chrome))
}
struct NativeChrome(chrome_state::Chrome);
impl DesktopChromeSource for NativeChrome {
    fn tree(&self) -> qos_desktop::WorkspaceTree {
        self.0.borrow().tree().desktop.clone()
    }
    fn snapshot(&self) -> DesktopChromeSnapshot {
        let state = self.0.borrow();
        let status = &state.status;
        let now = chrono::Local::now();
        DesktopChromeSnapshot {
            revision: state.revision,
            clock: ClockSnapshot {
                unix_seconds: now.timestamp(),
                clock: std::env::var("QOS_CLOCK")
                    .ok()
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| now.format("%H:%M").to_string()),
                weekday: now.format("%a").to_string(),
                date: now.format("%-d %B").to_string(),
            },
            telemetry: TelemetrySnapshot::default(),
            status: SystemStatus {
                battery_percent: status.battery_percent(),
                battery_charging: status.battery_is_charging(),
                battery_low: status.battery_is_low(),
                battery_critical: status.battery_is_critical(),
                volume: status.volume.map(|v| VolumeState {
                    level: v.level,
                    muted: v.muted,
                }),
                link: status.link.as_ref().map(|l| LinkStatus {
                    kind: match l.kind {
                        os_actions::LinkKind::Offline => LinkKind::Offline,
                        os_actions::LinkKind::Wired => LinkKind::Wired,
                        os_actions::LinkKind::Wifi => LinkKind::Wifi,
                    },
                    name: l.name.clone(),
                    signal: l.signal,
                }),
                media: status
                    .media
                    .iter()
                    .map(|m| ActiveMediaUse {
                        client_id: m.client_id,
                        label: m.label.clone(),
                        kind: match m.kind {
                            os_actions::MediaKind::Microphone => MediaKind::Microphone,
                            os_actions::MediaKind::Camera => MediaKind::Camera,
                        },
                    })
                    .collect(),
                summary: status.summary(),
            },
            overlays: ChromeOverlays {
                overview: state.overlays.overview,
                ws_settings: state.overlays.ws_settings,
            },
            fullscreen: state.is_fullscreen(),
            notification_badge: state.notification_badge_total(),
            activity_glance: ActivityGlance {
                headline: state.activity_glance.headline.clone(),
                unread: state.activity_glance.unread,
                pulse: state.activity_glance.pulse,
            },
        }
    }
    fn intent(&self, intent: ChromeIntent) -> bool {
        let mut state = self.0.borrow_mut();
        match intent {
            ChromeIntent::SwitchWorkspace(dir) => return state.switch_workspace(dir),
            ChromeIntent::ToggleOverview => state.overlays.overview = !state.overlays.overview,
            ChromeIntent::ToggleWorkspaceSettings => state.toggle_workspace_settings(),
            ChromeIntent::ToggleQuickSettings => {
                state.overlays.quick_settings = !state.overlays.quick_settings
            }
            ChromeIntent::ShowQuickSettings => state.overlays.quick_settings = true,
            ChromeIntent::ToggleCollaborators => {
                state.overlays.collaborators = !state.overlays.collaborators
            }
            ChromeIntent::ShowActivity => {
                let focus = state.activity_glance.focus.clone();
                state.open_room_activity(focus);
            }
        }
        state.bump();
        true
    }
    fn settings_anchor(&self, rect: Rect) {
        self.0.borrow_mut().workspace_settings_anchor = rect;
    }
}

#[cfg(test)]
mod native_source_tests {
    use super::*;
    use crate::shell::state::WorkspaceTree;
    #[test]
    fn native_chrome_intents_keep_existing_mutation_path() {
        let chrome = chrome_state::ChromeState::new_with_fixture_handle(
            WorkspaceTree::bootstrap_demo().shared(),
            crate::shell::hosted_inputs::HostedTileInputs::fixture().unwrap(),
        );
        let source = native_source(chrome.clone());
        assert!(source.intent(ChromeIntent::ToggleOverview));
        assert!(chrome.borrow().overlays.overview);
        assert!(source.intent(ChromeIntent::ShowQuickSettings));
        assert!(chrome.borrow().overlays.quick_settings);
        source.settings_anchor(Rect::new(1.0, 2.0, 30.0, 40.0));
        assert_eq!(
            chrome.borrow().workspace_settings_anchor,
            Rect::new(1.0, 2.0, 30.0, 40.0)
        );
        let snapshot = source.snapshot();
        assert_eq!(
            snapshot.status.battery_percent,
            chrome.borrow().status.battery_percent()
        );
        assert_eq!(snapshot.telemetry, TelemetrySnapshot::default());
        assert_eq!(source.tree().active, chrome.borrow().tree().active);
    }
}
