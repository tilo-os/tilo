//! Native service adapter for the canonical portable topbar.
use crate::shell::chrome_state;
use qos_desktop::chrome::*;
pub use qos_desktop::topbar::*;
use std::cell::RefCell;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::rc::Rc;
use vello::kurbo::Rect;

pub fn native_source(chrome: chrome_state::Chrome) -> Chrome {
    Rc::new(NativeChrome {
        chrome,
        clock_notified_id: RefCell::new(None),
    })
}

fn publish_clock_command(command: &str, activity_id: Option<&str>) {
    let Some(path) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .map(|root| root.join("tilo/clocks-command.json"))
    else {
        return;
    };
    let tmp = path.with_extension("json.new");
    let payload = serde_json::json!({
        "version": 1,
        "command": command,
        "activity_id": activity_id,
    });
    if std::fs::write(&tmp, payload.to_string()).is_ok() {
        let _ = std::fs::rename(tmp, path);
    }
}

fn refresh_clock_activity(
    state: &mut chrome_state::ChromeState,
    notified: &RefCell<Option<String>>,
) {
    let Some(path) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .map(|root| root.join("tilo/clocks.json"))
    else {
        return;
    };
    let Ok(metadata) = std::fs::metadata(&path) else {
        return;
    };
    #[cfg(unix)]
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
        return;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    if bytes.len() > 4096 {
        return;
    }
    let Ok(activity) = serde_json::from_slice::<chrome_state::ClockActivity>(&bytes) else {
        return;
    };
    if activity.version != 1
        || activity.producer_pid == 0
        || !std::path::Path::new(&format!("/proc/{}", activity.producer_pid)).exists()
    {
        return;
    }
    if let Some(notification_id) = activity.notification_id.as_ref()
        && notified.borrow().as_ref() != Some(notification_id)
    {
        state.push_notification(
            if activity.notification_title.is_empty() {
                "Clocks"
            } else {
                &activity.notification_title
            },
            if activity.notification_body.is_empty() {
                "Time is up"
            } else {
                &activity.notification_body
            },
        );
        *notified.borrow_mut() = Some(notification_id.clone());
    }
    if activity.active {
        state.overlays.clock = activity.overlay;
        state.clock_activity = Some(activity);
    } else {
        state.overlays.clock = false;
        state.clock_activity = None;
    }
}

struct NativeChrome {
    chrome: chrome_state::Chrome,
    clock_notified_id: RefCell<Option<String>>,
}
impl DesktopChromeSource for NativeChrome {
    fn tree(&self) -> qos_desktop::WorkspaceTree {
        self.chrome.borrow().tree().desktop.clone()
    }
    fn snapshot(&self) -> DesktopChromeSnapshot {
        let mut state = self.chrome.borrow_mut();
        refresh_clock_activity(&mut state, &self.clock_notified_id);
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
                clock: state.overlays.clock,
            },
            fullscreen: state.is_fullscreen(),
            notification_badge: state.notification_badge_total(),
            activity_glance: ActivityGlance {
                headline: state.activity_glance.headline.clone(),
                unread: state.activity_glance.unread,
                pulse: state.activity_glance.pulse,
            },
            viewport_size: state.viewport_size(),
            clock_activity: state.clock_activity.as_ref().map(|activity| ClockActivity {
                activity_id: activity.activity_id.clone(),
                label: activity.label.clone(),
                remaining_secs: activity.remaining_secs,
                stopwatch: activity.stopwatch,
                paused: activity.paused,
                overlay: activity.overlay,
            }),
            active_exposures: state
                .active_exposures()
                .iter()
                .map(|exposure| ActiveExposure {
                    peer: exposure.peer,
                    surface: exposure.surface,
                    can_control: exposure.can_control,
                })
                .collect(),
        }
    }
    fn intent(&self, intent: ChromeIntent) -> bool {
        let mut state = self.chrome.borrow_mut();
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
            ChromeIntent::HideClock => state.overlays.clock = false,
            ChromeIntent::ClockCommand {
                command,
                activity_id,
            } => {
                drop(state);
                publish_clock_command(&command, activity_id.as_deref());
                return true;
            }
        }
        state.bump();
        true
    }
    fn settings_anchor(&self, rect: Rect) {
        self.chrome.borrow_mut().workspace_settings_anchor = rect;
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
