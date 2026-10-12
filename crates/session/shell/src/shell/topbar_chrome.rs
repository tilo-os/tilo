//! The native host for the shared top bar.
//!
//! [`tilo_desktop::topbar::TopBarChrome`] draws the bar; this module answers
//! it from the compositor's chrome state — status, clock, overlays, the
//! Clocks app's live activity, the person's centre widgets — and carries out
//! what it asks for.
use crate::shell::chrome_state;
use settings_store::hardware::{TopBarWidget as StoredWidget, TopBarWidgetsConfig};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tilo_desktop::chrome::{
    ActiveMediaUse, AppTrayIcon, AppTrayItem, AppTrayRequest, AppTrayStatus, Chrome, ChromeIntent,
    ChromeOverlays, ClockActivity, ClockSnapshot, DesktopChromeSnapshot, DesktopChromeSource,
    LinkKind, LinkStatus, MediaKind, SystemStatus, TelemetrySnapshot, TopBarWidget, TopBarWidgets,
    VolumeState,
};
pub use tilo_desktop::topbar::*;
use vello::kurbo::Rect;

/// How often the Clocks app's live activity is taken in. The bar asks for a
/// snapshot many times a frame; the activity changes at most once a second.
const CLOCK_ACTIVITY_REFRESH: Duration = Duration::from_millis(250);

/// The bar's chrome source for the compositor's `chrome`.
pub fn native_source(chrome: chrome_state::Chrome) -> Chrome {
    // Owned fixtures paint at an owned clock value. Reading wall time here
    // would change an otherwise identical scene at a minute boundary.
    let fixture_clock = chrome.borrow().hosted_inputs.fixture_clock();
    Rc::new(NativeChrome {
        chrome,
        clock: RefCell::new(ClockPoll::default()),
        fixture_clock,
        widgets: RefCell::new(stored_widgets(TopBarWidgetsConfig::load())),
    })
}

#[derive(Default)]
struct ClockPoll {
    checked: Option<Instant>,
    notified_id: Option<String>,
}

fn stored_widgets(config: TopBarWidgetsConfig) -> TopBarWidgets {
    TopBarWidgets {
        widgets: config
            .normalised()
            .widgets
            .into_iter()
            .map(|widget| match widget {
                StoredWidget::Clock => TopBarWidget::Clock,
                StoredWidget::SystemMonitor => TopBarWidget::SystemMonitor,
                StoredWidget::Calendar => TopBarWidget::Calendar,
            })
            .collect(),
    }
}

fn save_widgets(widgets: &TopBarWidgets) {
    let config = TopBarWidgetsConfig {
        widgets: widgets
            .widgets
            .iter()
            .map(|widget| match widget {
                TopBarWidget::Clock => StoredWidget::Clock,
                TopBarWidget::SystemMonitor => StoredWidget::SystemMonitor,
                TopBarWidget::Calendar => StoredWidget::Calendar,
            })
            .collect(),
    };
    if let Err(error) = config.save() {
        tracing::warn!(%error, "could not keep the top bar's widgets");
    }
}

/// The app whose live activity the bar shows as its clock.
const CLOCKS_APP: &str = "so.tilo.clocks";

/// Hand a control pressed on the floating clock to Clocks, over its channel.
fn publish_clock_command(command: &str, activity_id: Option<&str>) {
    crate::shell::live_activities::board().press(
        CLOCKS_APP,
        serde_json::json!({
            "command": command,
            "activity_id": activity_id,
        }),
    );
}

/// Take the live activity Clocks shows — its running timer or stopwatch —
/// into chrome, and announce a newly due timer once.
fn refresh_clock_activity(state: &mut chrome_state::ChromeState, poll: &mut ClockPoll) {
    let now = Instant::now();
    if poll
        .checked
        .is_some_and(|checked| now.duration_since(checked) < CLOCK_ACTIVITY_REFRESH)
    {
        return;
    }
    poll.checked = Some(now);
    let Some(activity) = crate::shell::live_activities::board()
        .activity(CLOCKS_APP)
        .and_then(|activity| serde_json::from_value::<chrome_state::ClockActivity>(activity).ok())
    else {
        state.overlays.clock = false;
        state.clock_activity = None;
        return;
    };
    if let Some(notification_id) = activity.notification_id.as_ref()
        && poll.notified_id.as_ref() != Some(notification_id)
    {
        let title = if activity.notification_title.is_empty() {
            crate::shell::i18n::t("topbar-clocks-notification-title")
        } else {
            activity.notification_title.clone()
        };
        let body = if activity.notification_body.is_empty() {
            crate::shell::i18n::t("topbar-clocks-notification-body")
        } else {
            activity.notification_body.clone()
        };
        state.push_notification(title, body);
        poll.notified_id = Some(notification_id.clone());
    }
    if activity.active {
        state.overlays.clock = activity.overlay;
        state.clock_activity = Some(activity);
    } else {
        state.overlays.clock = false;
        state.clock_activity = None;
    }
}

/// `TILO_CLOCK` pins the bar's clock, for the same reason `TILO_SHOTS_THEME`
/// pins the palette: a screenshot harness comparing whole frames needs every
/// part still. Unset (every real session) reads the wall clock.
fn clock_snapshot() -> ClockSnapshot {
    let now = ui::clock::local_now();
    ClockSnapshot {
        unix_seconds: now.timestamp(),
        clock: std::env::var("TILO_CLOCK")
            .ok()
            .filter(|pinned| !pinned.is_empty())
            .unwrap_or_else(|| ui::clock::time_of_day(&now)),
        weekday: now.format("%a").to_string(),
        date: now.format("%-d %B").to_string(),
    }
}

fn system_status(status: &crate::shell::status::SystemStatus) -> SystemStatus {
    SystemStatus {
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
    }
}

struct NativeChrome {
    chrome: chrome_state::Chrome,
    clock: RefCell<ClockPoll>,
    fixture_clock: Option<ClockSnapshot>,
    widgets: RefCell<TopBarWidgets>,
}

impl DesktopChromeSource for NativeChrome {
    fn tree(&self) -> tilo_desktop_model::DesktopTree {
        self.chrome.borrow().tree().desktop.clone()
    }

    fn take_topbar_editor_request(&self) -> bool {
        self.chrome.borrow_mut().take_topbar_editor_request()
    }

    fn snapshot(&self) -> DesktopChromeSnapshot {
        let mut state = self.chrome.borrow_mut();
        refresh_clock_activity(&mut state, &mut self.clock.borrow_mut());
        DesktopChromeSnapshot {
            revision: state.revision,
            status: system_status(&state.status),
            clock: self.fixture_clock.clone().unwrap_or_else(clock_snapshot),
            telemetry: TelemetrySnapshot::default(),
            overlays: ChromeOverlays {
                overview: state.overlays.overview,
                ws_settings: state.overlays.ws_settings,
                clock: state.overlays.clock,
            },
            // A client-requested fullscreen owns the whole output. User
            // maximize still fills the workspace but keeps the bar.
            fullscreen: state.fullscreen_group().is_some() && state.is_fullscreen(),
            notification_badge: state.notification_badge_total(),
            activity_glance: state.activity_glance.clone(),
            viewport_size: state.viewport_size(),
            clock_activity: state.clock_activity.as_ref().map(|activity| ClockActivity {
                activity_id: activity.activity_id.clone(),
                label: activity.label.clone(),
                remaining_secs: activity.remaining_secs,
                stopwatch: activity.stopwatch,
                paused: activity.paused,
                overlay: activity.overlay,
            }),
            active_exposures: state.active_exposures().to_vec(),
            topbar_widgets: Some(self.widgets.borrow().clone()),
        }
    }

    fn intent(&self, intent: ChromeIntent) -> bool {
        let mut state = self.chrome.borrow_mut();
        match intent {
            ChromeIntent::SwitchWorkspace(dir) => return state.switch_workspace(dir),
            ChromeIntent::ToggleOverview => state.overlays.overview = !state.overlays.overview,
            ChromeIntent::ToggleWorkspaceSettings => state.toggle_workspace_settings(),
            ChromeIntent::ToggleQuickSettings => {
                state.toggle_quick_settings();
                return true;
            }
            ChromeIntent::ShowQuickSettings => {
                state.open_quick_settings();
                return true;
            }
            ChromeIntent::ToggleCollaborators => {
                state.overlays.collaborators = !state.overlays.collaborators;
                if state.overlays.collaborators {
                    state.overlays.room_activity = false;
                }
            }
            ChromeIntent::ShowActivity(focus) => {
                let focus = focus.or_else(|| state.activity_glance.focus.clone());
                state.open_room_activity(focus);
                return true;
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
            ChromeIntent::AcceptContinuity => {
                return state.accept_continuity_suggestion(Instant::now()).is_some();
            }
            ChromeIntent::DismissContinuity => {
                return state
                    .dismiss_continuity_suggestion(Instant::now())
                    .is_some();
            }
            ChromeIntent::AppTray { key, request } => {
                drop(state);
                let request = match request {
                    AppTrayRequest::Activate { x, y } => {
                        crate::tray::TrayRequest::Activate { x, y }
                    }
                    AppTrayRequest::SecondaryActivate { x, y } => {
                        crate::tray::TrayRequest::SecondaryActivate { x, y }
                    }
                    AppTrayRequest::ContextMenu { x, y } => {
                        crate::tray::TrayRequest::ContextMenu { x, y }
                    }
                };
                if let Err(error) = crate::tray::Tray::global().request(&key, request) {
                    tracing::debug!(target: "shell::tray", %key, %error, "tray click not delivered");
                    return false;
                }
                return true;
            }
            ChromeIntent::SetTopBarWidgets(widgets) => {
                let widgets = widgets.normalised();
                save_widgets(&widgets);
                *self.widgets.borrow_mut() = widgets;
            }
        }
        state.bump();
        true
    }

    fn settings_anchor(&self, rect: Rect) {
        self.chrome.borrow_mut().workspace_settings_anchor = rect;
    }

    fn continuity_offer(&self) -> Option<String> {
        let suggestion = self.chrome.borrow().continuity_suggestion(Instant::now())?;
        Some(crate::shell::continuity_suggestion::continue_label(
            &suggestion,
        ))
    }

    fn app_tray_revision(&self) -> u64 {
        crate::tray::Tray::global().revision()
    }

    fn app_tray(&self) -> Vec<AppTrayItem> {
        crate::tray::Tray::global()
            .snapshot()
            .into_iter()
            .map(|item| AppTrayItem {
                key: item.key,
                id: item.id,
                status: match item.status {
                    crate::tray::ItemStatus::Passive => AppTrayStatus::Passive,
                    crate::tray::ItemStatus::Active => AppTrayStatus::Active,
                    crate::tray::ItemStatus::NeedsAttention => AppTrayStatus::NeedsAttention,
                },
                icon: match item.icon {
                    crate::tray::TrayIcon::Pixels {
                        width,
                        height,
                        rgba,
                    } => AppTrayIcon::Pixels {
                        width,
                        height,
                        rgba,
                    },
                    crate::tray::TrayIcon::Letter(letter) => AppTrayIcon::Letter(letter),
                },
                item_is_menu: item.item_is_menu,
            })
            .collect()
    }
}

#[cfg(test)]
mod native_source_tests {
    use super::*;
    use crate::shell::state::WorkspaceTree;

    #[test]
    fn fixture_clock_is_owned_without_freezing_the_live_source() {
        let parent = tempfile::tempdir().unwrap();
        let inputs =
            crate::shell::hosted_inputs::HostedTileInputs::fixture_in(parent.path()).unwrap();
        let chrome = chrome_state::ChromeState::new_with_fixture_handle(
            WorkspaceTree::bootstrap_default().shared(),
            inputs,
        );
        let source = native_source(chrome);
        assert_eq!(source.snapshot().clock, ClockSnapshot::default());

        let live = native_source(chrome_state::ChromeState::new(
            WorkspaceTree::bootstrap_default(),
        ));
        assert!(live.snapshot().clock.unix_seconds > 0);
    }

    #[test]
    fn native_source_uses_each_fixtures_explicit_clock_on_repeated_frames() {
        let parent = tempfile::tempdir().unwrap();
        for (unix_seconds, display) in [(1_700_000_000, "09:41"), (1_700_000_060, "09:42")] {
            let clock = ClockSnapshot {
                unix_seconds,
                clock: display.into(),
                weekday: "Tuesday".into(),
                date: "14 November".into(),
            };
            let inputs = crate::shell::hosted_inputs::HostedTileInputs::fixture_in(parent.path())
                .unwrap()
                .with_fixture_clock(clock.clone())
                .unwrap();
            let chrome = chrome_state::ChromeState::new_with_fixture_handle(
                WorkspaceTree::bootstrap_default().shared(),
                inputs,
            );
            let source = native_source(chrome);
            for _ in 0..3 {
                assert_eq!(source.snapshot().clock, clock);
            }
        }
        assert!(
            crate::shell::hosted_inputs::HostedTileInputs::default()
                .with_fixture_clock(ClockSnapshot::default())
                .is_err()
        );
    }

    #[test]
    fn intents_take_the_compositors_own_mutation_paths() {
        let chrome = chrome_state::ChromeState::new(WorkspaceTree::bootstrap_demo());
        let source = native_source(chrome.clone());
        assert!(source.intent(ChromeIntent::ToggleOverview));
        assert!(chrome.borrow().overlays.overview);
        assert!(source.intent(ChromeIntent::ShowQuickSettings));
        assert!(chrome.borrow().overlays.quick_settings);
        assert!(source.intent(ChromeIntent::ToggleCollaborators));
        assert!(chrome.borrow().overlays.collaborators);
        assert!(!chrome.borrow().overlays.room_activity);
        source.settings_anchor(Rect::new(1.0, 2.0, 30.0, 40.0));
        assert_eq!(
            chrome.borrow().workspace_settings_anchor,
            Rect::new(1.0, 2.0, 30.0, 40.0)
        );
    }

    #[test]
    fn the_snapshot_reports_what_chrome_holds() {
        let chrome = chrome_state::ChromeState::new(WorkspaceTree::bootstrap_demo());
        let source = native_source(chrome.clone());
        let snapshot = source.snapshot();
        assert_eq!(
            snapshot.status.battery_percent,
            chrome.borrow().status.battery_percent()
        );
        assert_eq!(snapshot.telemetry, TelemetrySnapshot::default());
        assert!(
            snapshot
                .topbar_widgets
                .is_some_and(|w| !w.widgets.is_empty())
        );
        assert_eq!(source.tree().active, chrome.borrow().tree().desktop.active);
    }

    #[test]
    fn the_floating_clock_is_clocks_live_activity_and_its_controls_go_back() {
        let board = crate::shell::live_activities::board();
        let chrome = chrome_state::ChromeState::new(WorkspaceTree::bootstrap_demo());
        let source = native_source(chrome.clone());
        board.show(
            CLOCKS_APP,
            Some(serde_json::json!({
                "active": true,
                "activity_id": "timer-1",
                "label": "Tea",
                "remaining_secs": 90,
                "stopwatch": false,
                "overlay": true,
            })),
        );
        let shown = source
            .snapshot()
            .clock_activity
            .expect("the timer is shown");
        assert_eq!(shown.label, "Tea");
        assert_eq!(shown.remaining_secs, 90);
        assert!(source.snapshot().overlays.clock, "it floats");

        assert!(source.intent(ChromeIntent::ClockCommand {
            command: "toggle".into(),
            activity_id: Some("timer-1".into()),
        }));
        board.show(CLOCKS_APP, None);
        // A fresh source takes the ended activity in at once.
        let source = native_source(chrome.clone());
        assert!(source.snapshot().clock_activity.is_none(), "it ended");
        assert!(!chrome.borrow().overlays.clock);
    }
}
