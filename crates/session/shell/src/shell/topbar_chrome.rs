//! The native host for the shared top bar.
//!
//! [`tilo_desktop::topbar::TopBarChrome`] draws the bar; this module answers
//! it from the compositor's chrome state — status, clock, overlays, the
//! Clocks app's live activity — and carries out what it asks for.
use crate::shell::chrome_state;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};
use tilo_desktop::chrome::{
    ActiveMediaUse, AppTrayIcon, AppTrayItem, AppTrayRequest, AppTrayStatus, Chrome, ChromeIntent,
    ChromeOverlays, ClockActivity, ClockSnapshot, DesktopChromeSnapshot, DesktopChromeSource,
    LinkKind, LinkStatus, MediaKind, OwnChip, StripPlaces, SystemStatus, VolumeState,
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
        pill: Cell::new(Rect::ZERO),
        people: RefCell::new(None),
    })
}

/// Who is where, as the bar shows it: this person's own chip and the
/// strips people are on, worked out once per chrome revision.
#[derive(Clone)]
struct People {
    key: (u64, u64),
    own: OwnChip,
    places: StripPlaces,
}

/// The name this person goes by here, numbered like anyone else's when
/// someone in the room shares it; never "You", never their key.
pub(crate) fn own_shown_name(state: &chrome_state::ChromeState) -> String {
    // An owned fixture is nobody's desktop: it reads no person's identity.
    let own_name = (!state.hosted_inputs().is_fixture())
        .then(ui::local_identity::load)
        .flatten()
        .map(|identity| identity.display_name);
    // As the people here see this person: their name, else "Guest", never
    // a key or a device.
    let own_name = Some(crate::shell::network::shared_person_name(own_name));
    let local_user = state.active_local_user();
    let (_, shown) = tilo_desktop::collaborators::collaborator_roster_rows_named(
        &state.tree().desktop,
        &local_user,
        own_name.as_deref(),
    );
    shown.or(own_name).unwrap_or_default()
}

/// The colour this person wears here; an owned fixture derives it from the
/// identity it was given rather than reading the person's own.
pub(crate) fn own_rgb(state: &chrome_state::ChromeState) -> (u8, u8, u8) {
    let hue = if state.hosted_inputs().is_fixture() {
        ui::tokens::PresenceHue::of_identity(&state.active_local_user())
    } else {
        crate::shell::state::local_presence_hue()
    };
    ui::presence::identity_rgb(hue)
}

/// The strips of the active workspace people are on: the others at the
/// strip holding the tab their presence names, this person on the one shown.
fn strip_places(state: &chrome_state::ChromeState) -> StripPlaces {
    let local_user = state.active_local_user();
    let tree = state.tree();
    let desktop = &tree.desktop;
    let others: Vec<tilo_desktop_model::StripId> = desktop
        .active_workspace()
        .map(|workspace| {
            workspace
                .collaborators
                .iter()
                .filter(|person| person.id != local_user)
                .filter_map(|person| {
                    let tab = person.active_tab?;
                    workspace
                        .strips
                        .iter()
                        .find(|strip| {
                            strip
                                .window_groups
                                .iter()
                                .any(|group| group.tabs.iter().any(|t| t.id == tab))
                        })
                        .map(|strip| strip.id)
                })
                .collect()
        })
        .unwrap_or_default();
    let places = StripPlaces::in_active_workspace(desktop, &others);
    drop(tree);
    StripPlaces {
        can_add: state.can_add_strip(),
        ..places
    }
}

#[derive(Default)]
struct ClockPoll {
    checked: Option<Instant>,
    notified_id: Option<String>,
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
pub(crate) fn clock_snapshot() -> ClockSnapshot {
    let now = ui::clock::local_now();
    ClockSnapshot {
        unix_seconds: now.timestamp(),
        clock: std::env::var("TILO_CLOCK")
            .ok()
            .filter(|pinned| !pinned.is_empty())
            .unwrap_or_else(|| ui::clock::time_of_day(&now)),
        weekday: ui::format::weekday_short(chrono::Datelike::weekday(&now)),
        date: ui::format::month_day(now.date_naive()),
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
    /// Where this bar last drew its workspace name: its menu opens there.
    pill: Cell<Rect>,
    people: RefCell<Option<People>>,
}

impl NativeChrome {
    fn people(&self, state: &chrome_state::ChromeState) -> People {
        let key = (state.revision, ui::local_identity::generation());
        if let Some(people) = self.people.borrow().as_ref().filter(|p| p.key == key) {
            return people.clone();
        }
        let people = People {
            key,
            own: OwnChip {
                name: own_shown_name(state),
                rgb: own_rgb(state),
            },
            places: strip_places(state),
        };
        *self.people.borrow_mut() = Some(people.clone());
        people
    }
}

impl DesktopChromeSource for NativeChrome {
    fn tree(&self) -> tilo_desktop_model::DesktopTree {
        self.chrome.borrow().tree().desktop.clone()
    }

    fn snapshot(&self) -> DesktopChromeSnapshot {
        let mut state = self.chrome.borrow_mut();
        refresh_clock_activity(&mut state, &mut self.clock.borrow_mut());
        let people = self.people(&state);
        DesktopChromeSnapshot {
            revision: state.revision,
            status: system_status(&state.status),
            clock: self.fixture_clock.clone().unwrap_or_else(clock_snapshot),
            overlays: ChromeOverlays {
                overview: state.overlays.overview,
                ws_settings: state.overlays.ws_settings,
                clock: state.overlays.clock,
            },
            // A fullscreen owns its display: over the primary one, the full
            // bar steps aside. User maximize still fills the workspace but
            // keeps the bar.
            fullscreen: state.fullscreen_covers_primary(),
            notification_badge: state.notification_badge_total(),
            notifications_quiet: state.quiet_status_now().is_some(),
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
            // Native presence has no spectator role yet.
            watching: 0,
            strip_places: Some(people.places),
            top_bar: state.top_bar_presence(),
            own: Some(people.own),
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
            ChromeIntent::SwitchStrip(strip) => return state.focus_strip(strip),
            // The overlay host opens the strip switcher's card on its next
            // update, as Super+S does, with the focused strip chosen.
            ChromeIntent::ShowStrips => state.request_strip_switcher(),
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
                        tilo_tray_host::TrayRequest::Activate { x, y }
                    }
                    AppTrayRequest::SecondaryActivate { x, y } => {
                        tilo_tray_host::TrayRequest::SecondaryActivate { x, y }
                    }
                    AppTrayRequest::ContextMenu { x, y } => {
                        tilo_tray_host::TrayRequest::ContextMenu { x, y }
                    }
                };
                if let Err(error) = crate::tray::global().request(&key, request) {
                    tracing::debug!(target: "shell::tray", %key, %error, "tray click not delivered");
                    return false;
                }
                return true;
            }
            ChromeIntent::AddStrip { name } => return state.add_named_strip(&name),
            ChromeIntent::RenameWorkspace { name } => {
                if !state.may_rename_active_workspace() {
                    return false;
                }
                state.rename_active_workspace(&name);
                return true;
            }
            // The overlay host shows both popovers; it takes the request
            // in the same dispatch.
            ChromeIntent::ToggleWorkspaceMenu => {
                state.bar_popover_request = Some(chrome_state::BarPopoverRequest::WorkspaceMenu {
                    anchor: self.pill.get(),
                });
            }
            ChromeIntent::ToggleProfile => {
                state.bar_popover_request = Some(chrome_state::BarPopoverRequest::Profile);
            }
            // The native bar carries no Tilo mark: its About is in Settings.
            ChromeIntent::ToggleTiloMenu => return false,
        }
        state.bump();
        true
    }

    fn settings_anchor(&self, rect: Rect) {
        self.pill.set(rect);
        self.chrome.borrow_mut().workspace_settings_anchor = rect;
    }

    fn continuity_offer(&self) -> Option<String> {
        let suggestion = self.chrome.borrow().continuity_suggestion(Instant::now())?;
        Some(crate::shell::continuity_suggestion::continue_label(
            &suggestion,
        ))
    }

    fn app_tray_revision(&self) -> u64 {
        crate::tray::global().revision()
    }

    fn app_tray(&self) -> Vec<AppTrayItem> {
        crate::tray::global()
            .snapshot()
            .into_iter()
            .map(|item| AppTrayItem {
                key: item.key,
                id: item.id,
                status: match item.status {
                    tilo_tray_host::ItemStatus::Passive => AppTrayStatus::Passive,
                    tilo_tray_host::ItemStatus::Active => AppTrayStatus::Active,
                    tilo_tray_host::ItemStatus::NeedsAttention => AppTrayStatus::NeedsAttention,
                },
                icon: match item.icon {
                    tilo_tray_host::TrayIcon::Pixels {
                        width,
                        height,
                        rgba,
                    } => AppTrayIcon::Pixels {
                        width,
                        height,
                        rgba,
                    },
                    tilo_tray_host::TrayIcon::Letter(letter) => AppTrayIcon::Letter(letter),
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

    /// A click on the strip's name in the top bar asks for the strip
    /// switcher, not the overview.
    #[test]
    fn the_strip_name_asks_for_the_strip_switcher() {
        let chrome = chrome_state::ChromeState::new(WorkspaceTree::bootstrap_demo());
        let source = native_source(chrome.clone());
        assert!(source.intent(ChromeIntent::ShowStrips));
        assert!(!chrome.borrow().overlays.overview, "not the overview");
        assert!(chrome.borrow_mut().take_strip_switcher_request());
        assert!(
            !chrome.borrow_mut().take_strip_switcher_request(),
            "one request, taken once"
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
