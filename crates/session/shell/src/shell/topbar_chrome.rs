//! What the shell knows that the top bar shows, worked out from the
//! compositor's chrome state: the person's own name and colour, the status,
//! the time, the Clocks app's live activity and the running applications'
//! tray items. The bar itself is an app the overlay host mounts and feeds
//! (`overlay_host::topbar_host`).
use crate::shell::chrome_state;
use std::time::{Duration, Instant};
use tilo_desktop::chrome::{
    ActiveMediaUse, AppTrayIcon, AppTrayItem, AppTrayRequest, AppTrayStatus, ClockSnapshot,
    LinkKind, LinkStatus, MediaKind, SystemStatus, VolumeState,
};
pub use tilo_desktop::topbar::*;

/// How often the Clocks app's live activity is taken in. The bar asks for a
/// snapshot many times a frame; the activity changes at most once a second.
const CLOCK_ACTIVITY_REFRESH: Duration = Duration::from_millis(250);

/// The name this person goes by here, numbered like anyone else's when
/// someone in the room shares it; never "You", never their key.
pub(crate) fn own_shown_name(state: &chrome_state::ChromeState) -> String {
    let own_name = Some(own_person_name(state));
    let local_user = state.active_local_user();
    let (_, shown) = tilo_desktop::collaborators::collaborator_roster_rows_named(
        &state.tree().desktop,
        &local_user,
        own_name.as_deref(),
    );
    shown.or(own_name).unwrap_or_default()
}

/// This person's own name, before the roster numbers it: as the people here
/// see them, their name, else "Guest", never a key or a device.
pub(crate) fn own_person_name(state: &chrome_state::ChromeState) -> String {
    // An owned fixture is nobody's desktop: it reads no person's identity.
    let own_name = (!state.hosted_inputs().is_fixture())
        .then(ui::local_identity::load)
        .flatten()
        .map(|identity| identity.display_name);
    crate::shell::network::own_person_name(own_name)
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

/// When the Clocks app's live activity was last taken in, and the timer
/// whose end was last announced.
#[derive(Default)]
pub(crate) struct ClockPoll {
    checked: Option<Instant>,
    notified_id: Option<String>,
}

/// The app whose live activity the bar shows as its clock.
pub(crate) const CLOCKS_APP: &str = "so.tilo.clocks";

/// Hand a control pressed on the floating clock to Clocks, over its channel.
pub(crate) fn publish_clock_command(command: &str, activity_id: Option<&str>) {
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
pub(crate) fn refresh_clock_activity(state: &mut chrome_state::ChromeState, poll: &mut ClockPoll) {
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
    let mut snapshot = ClockSnapshot::now();
    if let Some(pinned) = std::env::var("TILO_CLOCK")
        .ok()
        .filter(|pinned| !pinned.is_empty())
    {
        snapshot.clock = pinned;
    }
    snapshot
}

pub(crate) fn system_status(status: &tilo_quick_settings::SystemStatus) -> SystemStatus {
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

/// A click on a running application's tray item, as its host takes it.
pub(crate) fn tray_request(request: AppTrayRequest) -> tilo_tray_host::TrayRequest {
    match request {
        AppTrayRequest::Activate { x, y } => tilo_tray_host::TrayRequest::Activate { x, y },
        AppTrayRequest::SecondaryActivate { x, y } => {
            tilo_tray_host::TrayRequest::SecondaryActivate { x, y }
        }
        AppTrayRequest::ContextMenu { x, y } => tilo_tray_host::TrayRequest::ContextMenu { x, y },
    }
}

/// The running applications' tray items, as the bar shows them.
pub(crate) fn app_tray_items() -> Vec<AppTrayItem> {
    crate::tray::global()
        .snapshot()
        .into_iter()
        .map(|item| AppTrayItem {
            key: item.key,
            id: item.id.clone(),
            owner_app: item.address.owner_app.clone(),
            title: if item.title.trim().is_empty() {
                item.id
            } else {
                item.title
            },
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
