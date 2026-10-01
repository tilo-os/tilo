//! The native host for the shared top bar.
//!
//! [`tilo_desktop::topbar::TopBarChrome`] draws the bar; this module answers
//! it from the compositor's chrome state — status, clock, overlays, the
//! Clocks app's live activity, the person's centre widgets — and carries out
//! what it asks for.
use crate::shell::chrome_state;
use settings_store::hardware::{TopBarWidget as StoredWidget, TopBarWidgetsConfig};
use std::cell::RefCell;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
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

/// How often the Clocks app's snapshot is re-read. The bar asks for a
/// snapshot many times a frame; the file changes at most once a second.
const CLOCK_ACTIVITY_REFRESH: Duration = Duration::from_millis(250);

/// The bar's chrome source for the compositor's `chrome`.
pub fn native_source(chrome: chrome_state::Chrome) -> Chrome {
    Rc::new(NativeChrome {
        chrome,
        clock: RefCell::new(ClockPoll::default()),
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

fn publish_clock_command(command: &str, activity_id: Option<&str>) {
    let Some(path) =
        xdg_paths::xdg::runtime_dir().map(|root| root.join("tilo/clocks-command.json"))
    else {
        return;
    };
    let payload = serde_json::json!({
        "version": 1,
        "command": command,
        "activity_id": activity_id,
    });
    let _ = tilo_files::native::atomic::write(&path, payload.to_string().as_bytes());
}

/// Read the Clocks app's private snapshot of its running timer or
/// stopwatch into chrome, and announce a newly due timer once.
fn refresh_clock_activity(state: &mut chrome_state::ChromeState, poll: &mut ClockPoll) {
    let now = Instant::now();
    if poll
        .checked
        .is_some_and(|checked| now.duration_since(checked) < CLOCK_ACTIVITY_REFRESH)
    {
        return;
    }
    poll.checked = Some(now);
    let Some(path) = xdg_paths::xdg::runtime_dir().map(|root| root.join("tilo/clocks.json")) else {
        return;
    };
    let Ok(metadata) = std::fs::metadata(&path) else {
        return;
    };
    #[cfg(unix)]
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode().trailing_zeros() < 6 {
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
    let now = chrono::Local::now();
    ClockSnapshot {
        unix_seconds: now.timestamp(),
        clock: std::env::var("TILO_CLOCK")
            .ok()
            .filter(|pinned| !pinned.is_empty())
            .unwrap_or_else(|| now.format("%H:%M").to_string()),
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
    widgets: RefCell<TopBarWidgets>,
}

impl DesktopChromeSource for NativeChrome {
    fn tree(&self) -> tilo_desktop_model::DesktopTree {
        self.chrome.borrow().tree().desktop.clone()
    }

    fn snapshot(&self) -> DesktopChromeSnapshot {
        let mut state = self.chrome.borrow_mut();
        refresh_clock_activity(&mut state, &mut self.clock.borrow_mut());
        DesktopChromeSnapshot {
            revision: state.revision,
            status: system_status(&state.status),
            clock: clock_snapshot(),
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
}
