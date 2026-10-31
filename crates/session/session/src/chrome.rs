//! What the desktop's layers ask of the native session: the bar's source
//! (the clock, the person, their notifications) and the rules of what the
//! person may change.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use tilo_action_policy::Role;
use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::chrome::{
    ActiveMediaUse, AppTrayIcon, AppTrayItem, AppTrayStatus, AvatarAsk, ChromeIntent,
    ChromeOverlays, ClockActivity, ClockSnapshot, DesktopChromeSnapshot, DesktopChromeSource,
    LinkKind, LinkStatus, MediaKind, OwnChip, SystemStatus, VolumeState,
};
use tilo_desktop::desktop_host::DesktopRules;
use tilo_desktop::desktop_host::{DesktopAsk, DesktopChrome};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::DesktopTree;
use tilo_desktop_model::commands::{DesktopCommand, Standing, may_command, may_create_workspace};
use tilo_desktop_model::ownership::Authors;
use tilo_desktop_surface::notifications::Notification;
use tilo_desktop_surface::{DesktopHandle, DesktopNotifications};
use tilo_surface_host::{Placement, WidgetHost};

use crate::notifications::NoticeSignals;

/// The app whose live activity the bar shows as its clock.
pub(crate) const CLOCKS_APP: &str = "so.tilo.clocks";

/// The clock's live activity: the board every app shows its activity on,
/// and the moment the person hid the floating timer from the bar.
pub(crate) struct Clock {
    board: Arc<runtime_collab::LiveActivities>,
    /// The board revision the person hid the overlay at; a change of the
    /// activity is the app's answer and shows it again.
    hidden_at: Cell<Option<u64>>,
}

impl Clock {
    pub(crate) fn new(board: Arc<runtime_collab::LiveActivities>) -> Self {
        Self {
            board,
            hidden_at: Cell::new(None),
        }
    }

    /// The board the apps show their live activities on.
    pub(crate) fn board(&self) -> &Arc<runtime_collab::LiveActivities> {
        &self.board
    }

    /// Hand a control pressed on the floating clock back to Clocks.
    pub(crate) fn press(&self, command: &str, activity_id: Option<&str>) {
        self.board.press(
            CLOCKS_APP,
            serde_json::json!({ "command": command, "activity_id": activity_id }),
        );
    }

    /// The person closed the overlay: it stays away until Clocks shows
    /// something new.
    pub(crate) fn hide(&self) {
        self.hidden_at.set(Some(self.board.revision()));
    }

    /// What the clocks app shows now.
    fn activity(&self) -> Option<ClocksActivity> {
        self.board
            .activity(CLOCKS_APP)
            .and_then(|activity| serde_json::from_value(activity).ok())
    }

    /// Whether the activity floats over the windows for the person to see.
    fn overlay(&self, activity: &ClocksActivity) -> bool {
        activity.overlay && self.hidden_at.get() != Some(self.board.revision())
    }
}

/// What the clocks app puts on its live activity, as the bar reads it: the
/// timer or stopwatch running, and the due timer it wants announced.
#[derive(serde::Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "active, stopwatch, paused and overlay are independent fields of the activity Clocks shows"
)]
struct ClocksActivity {
    #[serde(default = "active_unless_told")]
    active: bool,
    #[serde(default)]
    activity_id: Option<String>,
    label: String,
    remaining_secs: u64,
    #[serde(default)]
    stopwatch: bool,
    #[serde(default)]
    paused: bool,
    #[serde(default)]
    overlay: bool,
    #[serde(default)]
    notification_id: Option<String>,
    #[serde(default)]
    notification_title: String,
    #[serde(default)]
    notification_body: String,
}

/// An app that names no `active` shows its activity.
fn active_unless_told() -> bool {
    true
}

/// The clock reading the bar shows, and whether it floats: Clocks'
/// activity as it stands, with a newly due timer announced once.
fn clock_reading(
    clock: &Clock,
    notified: &RefCell<Option<String>>,
    notices: &DesktopNotifications,
) -> (Option<ClockActivity>, bool) {
    let Some(activity) = clock.activity() else {
        return (None, false);
    };
    // A due timer the person has not been told about yet.
    let due = activity
        .notification_id
        .clone()
        .filter(|id| notified.borrow().as_deref() != Some(id.as_str()));
    if let Some(id) = due {
        let title = if activity.notification_title.is_empty() {
            crate::i18n::t("clock-due-title")
        } else {
            activity.notification_title.clone()
        };
        let body = if activity.notification_body.is_empty() {
            crate::i18n::t("clock-due-body")
        } else {
            activity.notification_body.clone()
        };
        notices.post(Notification::new(title, body));
        notified.replace(Some(id));
    }
    if !activity.active {
        return (None, false);
    }
    let overlay = clock.overlay(&activity);
    (
        Some(ClockActivity {
            activity_id: activity.activity_id,
            label: activity.label,
            remaining_secs: activity.remaining_secs,
            stopwatch: activity.stopwatch,
            paused: activity.paused,
            overlay: activity.overlay,
        }),
        overlay,
    )
}

/// What the bar reads of the session besides the desk's tree: the
/// notifications' state, the device's readings, the apps' live activities,
/// the running applications' tray items, and where the session's own
/// notices go.
pub(crate) struct BarFeeds {
    pub(crate) signals: Rc<NoticeSignals>,
    pub(crate) readings: crate::panels::Readings,
    pub(crate) clock: Rc<Clock>,
    pub(crate) notices: DesktopNotifications,
    /// The applications' tray items, when their watcher serves.
    pub(crate) tray: Option<Arc<tilo_tray_host::Tray>>,
}

/// The bar's view of the session: the desktop's tree, the wall clock, and
/// the person at this device.
pub(crate) struct SessionChrome {
    pub(crate) handle: DesktopHandle,
    /// Where the bar's intents go: the chrome carries them out.
    pub(crate) intents: Rc<RefCell<Vec<ChromeIntent>>>,
    pub(crate) person: String,
    /// What the bar reads of the notifications: the unread count, the
    /// silenced bell and each app's own badge.
    pub(crate) signals: Rc<NoticeSignals>,
    /// The session has no quick settings panel: the status cluster is the
    /// bell that opens the notification centre.
    pub(crate) notifications_only: bool,
    /// Who is here, when this session takes part in the collaboration
    /// plane: the bar's trees carry them as the active workspace's
    /// collaborators.
    pub(crate) sync: Option<Rc<crate::sync::Client>>,
    /// The device as the quick settings panel reads it: the status cluster's
    /// battery, network and volume glyphs, and the streams being captured.
    pub(crate) readings: crate::panels::Readings,
    /// The clocks app's live activity, which the bar shows as its clock.
    pub(crate) clock: Rc<Clock>,
    /// The desk's notifications, where a due timer is announced.
    pub(crate) notices: DesktopNotifications,
    /// The running applications' status-notifier items, as their host keeps
    /// them.
    pub(crate) tray: Option<Arc<tilo_tray_host::Tray>>,
    /// The due timer announced last, so an unchanged one is announced once.
    notified: RefCell<Option<String>>,
    /// `(tree revision, minute, the last wave heard, the ask on the
    /// person's avatar, the tray's revision)` the snapshot revision was last
    /// bumped for.
    seen: Cell<(u64, i64, Option<String>, u64, u64)>,
    revision: Cell<u64>,
}

impl SessionChrome {
    pub(crate) fn new(
        handle: DesktopHandle,
        intents: Rc<RefCell<Vec<ChromeIntent>>>,
        person: String,
        feeds: BarFeeds,
        notifications_only: bool,
        sync: Option<Rc<crate::sync::Client>>,
    ) -> Self {
        let BarFeeds {
            signals,
            readings,
            clock,
            notices,
            tray,
        } = feeds;
        Self {
            handle,
            intents,
            person,
            signals,
            notifications_only,
            sync,
            readings,
            clock,
            notices,
            tray,
            notified: RefCell::new(None),
            seen: Cell::new((u64::MAX, 0, None, u64::MAX, u64::MAX)),
            revision: Cell::new(0),
        }
    }

    /// The device as the quick settings panel last read it, in the bar's own
    /// terms: what its battery, network and volume glyphs draw and what its
    /// status cluster says of them. A session without a panel reads nothing,
    /// and draws no glyph (absence, never a zero).
    fn status(&self) -> SystemStatus {
        let feed = self.readings.borrow().clone();
        feed.map_or_else(SystemStatus::default, |feed| {
            system_status(&feed.current().status)
        })
    }
}

/// The bar's status from the panel's reading of the device: the shell's
/// `system_status`, field for field.
fn system_status(status: &tilo_quick_settings::SystemStatus) -> SystemStatus {
    SystemStatus {
        battery_percent: status.battery_percent(),
        battery_charging: status.battery_is_charging(),
        battery_low: status.battery_is_low(),
        battery_critical: status.battery_is_critical(),
        volume: status.volume.map(|volume| VolumeState {
            level: volume.level,
            muted: volume.muted,
        }),
        link: status.link.as_ref().map(|link| LinkStatus {
            kind: match link.kind {
                os_actions::LinkKind::Offline => LinkKind::Offline,
                os_actions::LinkKind::Wired => LinkKind::Wired,
                os_actions::LinkKind::Wifi => LinkKind::Wifi,
            },
            name: link.name.clone(),
            signal: link.signal,
        }),
        media: status
            .media
            .iter()
            .map(|media| ActiveMediaUse {
                client_id: media.client_id,
                label: media.label.clone(),
                kind: match media.kind {
                    os_actions::MediaKind::Microphone => MediaKind::Microphone,
                    os_actions::MediaKind::Camera => MediaKind::Camera,
                },
            })
            .collect(),
        summary: status.summary(),
    }
}

impl DesktopChromeSource for SessionChrome {
    fn snapshot(&self) -> DesktopChromeSnapshot {
        let clock = ClockSnapshot::now();
        let waved = self.sync.as_ref().and_then(|sync| sync.wave_notice());
        let (clock_activity, clock_overlay) =
            clock_reading(&self.clock, &self.notified, &self.notices);
        // The bar's pixels for an ask that comes, opens, folds, is answered
        // or is put off change with it, so the revision moves for those as
        // it does for the tree.
        let seen = (
            self.handle.revision(),
            clock.unix_seconds.div_euclid(60),
            waved.as_ref().map(|notice| notice.from.clone()),
            self.handle.personal_ask_revision(),
            self.app_tray_revision(),
        );
        if self.seen.replace(seen.clone()) != seen {
            self.revision.set(self.revision.get().wrapping_add(1));
        }
        DesktopChromeSnapshot {
            revision: self.revision.get(),
            status: self.status(),
            clock,
            overlays: ChromeOverlays {
                clock: clock_overlay,
                ..ChromeOverlays::default()
            },
            clock_activity,
            waved,
            own: Some(OwnChip {
                name: self.person.clone(),
                rgb: ui::presence::user_color(&self.person),
                identity: String::new(),
            }),
            notification_badge: self.signals.unread(),
            notifications_quiet: self.signals.quiet(),
            notifications_only: self.notifications_only,
            ..DesktopChromeSnapshot::default()
        }
    }

    fn tree(&self) -> DesktopTree {
        let mut tree = self.handle.read(Clone::clone);
        // Who is here, as the active workspace's collaborators: the bar's
        // faces and every roster read the tree, not a list of their own.
        if let Some(sync) = &self.sync {
            sync.show(&mut tree, &|_: &str| None);
        }
        tree
    }

    fn app_notification_badge(&self, app_id: &str) -> u32 {
        self.signals.badge(app_id)
    }

    /// Changes whenever an application's tray item appears, changes or
    /// goes, so the bar redraws that icon and nothing else.
    fn app_tray_revision(&self) -> u64 {
        self.tray.as_ref().map_or(0, |tray| tray.revision())
    }

    /// The running applications' tray items, in the order they registered:
    /// each one's own title, the app that registered it, and its picture or
    /// letter badge.
    fn app_tray(&self) -> Vec<AppTrayItem> {
        let Some(tray) = &self.tray else {
            return Vec::new();
        };
        tray.snapshot().into_iter().map(app_tray_item).collect()
    }

    /// The ask waiting on the person's own avatar: what the session put to
    /// them there (see [`crate::identity::offer`]), open beside their face
    /// until they answer it or put it off.
    fn avatar_ask(&self) -> Option<AvatarAsk> {
        self.handle.personal_ask().map(|ask| AvatarAsk {
            question: ask.title,
            detail: ask.body,
            answer: ask.action.title,
            open: ask.open,
        })
    }

    fn intent(&self, intent: ChromeIntent) -> bool {
        self.intents.borrow_mut().push(intent);
        true
    }
}

/// One tray item as the bar draws it: the app's own words for it, the app
/// it demonstrably belongs to, and the pixels it sent or named, with a
/// letter badge for an icon nothing could draw.
fn app_tray_item(item: tilo_tray_host::TrayItem) -> AppTrayItem {
    AppTrayItem {
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
    }
}

/// The person at this device owns its desktop: the desktop's one policy
/// answers what they may change, as it answers a room's owner.
pub(crate) struct SessionRules {
    pub(crate) handle: DesktopHandle,
    pub(crate) person: String,
    /// Who made each strip and opened each window.
    pub(crate) authors: RefCell<Authors>,
}

impl SessionRules {
    fn standing(&self) -> Standing<'_> {
        Standing {
            person: &self.person,
            role: Some(Role::Owner),
            layout_shared: true,
            members: None,
        }
    }
}

impl DesktopRules for SessionRules {
    fn tree(&self) -> DesktopTree {
        self.handle.read(Clone::clone)
    }

    fn author(&self) -> String {
        self.person.clone()
    }

    fn may(&self, command: &DesktopCommand) -> Result<(), String> {
        let authors = self.authors.borrow();
        self.handle.read(|tree| {
            let workspace = tree
                .active_workspace()
                .ok_or_else(|| "There is no workspace open".to_owned())?;
            may_command(&self.standing(), &authors, workspace, command)
                .map_err(|refusal| refusal.to_string())
        })
    }

    fn may_create_workspace(&self) -> Result<(), String> {
        may_create_workspace(&self.standing()).map_err(|refusal| refusal.to_string())
    }

    fn made_strip(&self, strip: tilo_desktop_model::StripId) {
        self.authors
            .borrow_mut()
            .insert(strip.0, self.person.clone());
    }
}

/// The chrome above the windows (the top bar, overview, menus) as the
/// session hosts it: its own tree beside the desk's, covering the outputs.
pub(crate) fn host(
    handle: &DesktopHandle,
    options: &crate::Options,
    intents: Rc<RefCell<Vec<ChromeIntent>>>,
    asks: Rc<RefCell<Vec<DesktopAsk>>>,
    feeds: BarFeeds,
    sync: Option<Rc<crate::sync::Client>>,
    overview: &crate::mount::overview::Overview,
) -> WidgetHost {
    let rules = Rc::new(SessionRules {
        handle: handle.clone(),
        person: options.person.clone(),
        authors: RefCell::new(std::collections::BTreeMap::new()),
    });
    let tree = handle.read(Clone::clone);
    // The bar's source hands its intents to the chrome it sits in.
    let source = Rc::new(SessionChrome::new(
        handle.clone(),
        Rc::clone(&intents),
        options.person.clone(),
        feeds,
        options.quick_settings.is_none(),
        sync,
    ));
    let bar = TopBarChrome::from_tree(&tree)
        .with_chrome(source)
        .with_tilo_mark();
    let mut chrome = DesktopChrome::new(
        handle.clone(),
        crate::GENERATION.into(),
        rules,
        bar,
        TiloMenu::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    )
    .with_intents(intents)
    .with_asks(asks)
    .with_overview(overview.flight(), overview.open_flag());
    chrome.set_shortcuts(options.config.shortcuts.clone());
    let keyboard = chrome.keyboard();
    let placement = Placement {
        origin: (0, 0),
        size: (1, 1),
        scale: options.scale,
    };
    let mut host = WidgetHost::new(Box::new(chrome), placement, false);
    host.set_theme((options.presentation)().theme);
    let pictures = overview.clone();
    host.with_window_pictures(Rc::new(move || pictures.pictures()))
        .covering_outputs()
        .with_keyboard(keyboard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use os_actions::{
        LinkKind as OsLinkKind, LinkStatus as OsLinkStatus, MediaKind as OsMediaKind,
    };
    use tilo_power_mgmt::{BatterySnapshot, BatteryState};
    use tilo_quick_settings::SystemStatus as Reading;

    fn reading() -> Reading {
        Reading {
            battery: Some(BatterySnapshot {
                present: true,
                percent: 42,
                state: BatteryState::Charging,
            }),
            battery_time: None,
            link: Some(OsLinkStatus {
                kind: OsLinkKind::Wifi,
                name: Some("Home".to_owned()),
                signal: 3,
            }),
            volume: Some(media::audio::default_node::VolumeState {
                level: 0.6,
                muted: false,
            }),
            media: vec![os_actions::ActiveMediaUse {
                client_id: 7,
                pid: 4242,
                app_id: "so.tilo.meet".to_owned(),
                label: "Meet".to_owned(),
                binary: std::path::PathBuf::from("/usr/bin/meet"),
                kind: OsMediaKind::Microphone,
            }],
        }
    }

    /// The bar's glyphs: the battery's charge, the network's name and
    /// strength, the volume's level, and the stream being captured.
    #[test]
    fn the_bar_reads_the_machines_battery_network_and_volume() {
        let status = system_status(&reading());
        assert_eq!(status.battery_percent(), Some(42));
        assert!(status.battery_is_charging());
        assert!(!status.battery_is_low());
        let link = status.link.clone().expect("a link");
        assert_eq!(link.kind, LinkKind::Wifi);
        assert_eq!(link.name.as_deref(), Some("Home"));
        assert_eq!(link.signal, 3);
        let volume = status.volume.expect("a level");
        assert!((volume.level - 0.6).abs() < f32::EPSILON);
        assert!(!volume.muted);
        assert!(!status.audio_is_silent());
        assert_eq!(status.media.len(), 1);
        assert_eq!(status.media[0].client_id, 7);
        assert_eq!(status.media[0].label, "Meet");
        assert_eq!(status.media[0].kind, MediaKind::Microphone);
    }

    /// A machine that read nothing (no panel mounted yet) draws no glyph:
    /// absence, never a zero battery or a full network.
    #[test]
    fn a_machine_with_no_readings_shows_no_glyph() {
        let status = system_status(&Reading::default());
        assert_eq!(status.battery_percent(), None);
        assert!(status.link.is_none());
        assert!(status.volume.is_none());
        assert!(status.media.is_empty());
    }

    /// Clocks' live activity in the shape its app publishes it: the label
    /// and remaining time every activity carries, and what `extra` adds.
    fn timer(extra: &serde_json::Value) -> serde_json::Value {
        let mut activity = serde_json::json!({ "label": "Tea", "remaining_secs": 90 });
        if let (Some(activity), Some(extra)) = (activity.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                activity.insert(key.clone(), value.clone());
            }
        }
        activity
    }

    /// A running timer is the bar's clock and floats over the windows; the
    /// person closing the overlay keeps it away until Clocks shows something
    /// new, and a timer that ended leaves neither.
    #[test]
    fn a_running_timer_shows_on_the_bar_and_floats_until_the_person_puts_it_away() {
        let board = std::sync::Arc::new(runtime_collab::LiveActivities::default());
        let clock = Clock::new(std::sync::Arc::clone(&board));
        let notices = DesktopNotifications::default();
        let notified = RefCell::new(None);
        board.show(
            CLOCKS_APP,
            Some(timer(&serde_json::json!({
                "active": true,
                "activity_id": "timer-1",
                "stopwatch": false,
                "overlay": true,
            }))),
        );
        let (activity, overlay) = clock_reading(&clock, &notified, &notices);
        let activity = activity.expect("the timer is the bar's clock");
        assert_eq!(activity.label, "Tea");
        assert_eq!(activity.remaining_secs, 90);
        assert_eq!(activity.activity_id.as_deref(), Some("timer-1"));
        assert!(overlay, "it floats over the windows");

        clock.hide();
        let (activity, overlay) = clock_reading(&clock, &notified, &notices);
        assert!(activity.is_some(), "the bar still reads the time");
        assert!(!overlay, "but nothing floats");

        // Clocks shows the same timer paused, a change: it is back.
        board.show(
            CLOCKS_APP,
            Some(timer(&serde_json::json!({
                "active": true,
                "activity_id": "timer-1",
                "paused": true,
                "overlay": true,
            }))),
        );
        assert!(clock_reading(&clock, &notified, &notices).1, "shown again");

        board.show(CLOCKS_APP, None);
        let (activity, overlay) = clock_reading(&clock, &notified, &notices);
        assert!(activity.is_none(), "the timer ended");
        assert!(!overlay);
    }

    /// An activity the app marked inactive is not the bar's clock.
    #[test]
    fn an_inactive_activity_leaves_the_clock_bare() {
        let board = std::sync::Arc::new(runtime_collab::LiveActivities::default());
        let clock = Clock::new(std::sync::Arc::clone(&board));
        let notices = DesktopNotifications::default();
        board.show(
            CLOCKS_APP,
            Some(timer(
                &serde_json::json!({ "active": false, "overlay": true }),
            )),
        );
        let (activity, overlay) = clock_reading(&clock, &RefCell::new(None), &notices);
        assert!(activity.is_none());
        assert!(!overlay);
    }

    /// A due timer is announced once, in the app's own words when it has
    /// them and in the desktop's otherwise.
    #[test]
    fn a_due_timer_is_announced_once() {
        let board = std::sync::Arc::new(runtime_collab::LiveActivities::default());
        let clock = Clock::new(std::sync::Arc::clone(&board));
        let notices = DesktopNotifications::default();
        let notified = RefCell::new(None);
        // What the banners show now, by title and body.
        let announced = || {
            notices
                .log()
                .active()
                .iter()
                .map(|notification| (notification.title.clone(), notification.body.clone()))
                .collect::<Vec<_>>()
        };
        let due = |activity: &serde_json::Value| {
            board.show(CLOCKS_APP, Some(timer(activity)));
        };
        due(&serde_json::json!({
            "active": true,
            "overlay": true,
            "notification_id": "due-1",
            "notification_title": "Bread",
            "notification_body": "Out of the oven",
        }));
        clock_reading(&clock, &notified, &notices);
        let live = announced();
        assert_eq!(live.len(), 1, "one announcement");
        assert_eq!(live[0].0, "Bread");
        assert_eq!(live[0].1, "Out of the oven");
        assert_eq!(notified.borrow().as_deref(), Some("due-1"));

        // The same due timer, read again, is not announced again.
        clock_reading(&clock, &notified, &notices);
        assert_eq!(announced().len(), 1, "still one");

        // A timer that names its own due time in no words gets the
        // desktop's.
        board.show(CLOCKS_APP, None);
        due(&serde_json::json!({
            "active": true,
            "overlay": true,
            "notification_id": "due-2",
        }));
        clock_reading(&clock, &notified, &notices);
        let live = announced();
        assert_eq!(live.len(), 2, "the next timer is its own announcement");
        assert_eq!(live[1].0, crate::i18n::t("clock-due-title"));
        assert_eq!(live[1].1, crate::i18n::t("clock-due-body"));
    }

    /// A control pressed on the floating clock reaches Clocks with the timer
    /// it belongs to.
    #[tokio::test]
    async fn a_pressed_control_reaches_clocks() {
        let board = std::sync::Arc::new(runtime_collab::LiveActivities::default());
        let clock = Clock::new(std::sync::Arc::clone(&board));
        let clocks = runtime_collab::live_activity::LiveActivityServer::new(
            CLOCKS_APP,
            std::sync::Arc::clone(&board),
            std::sync::Arc::new(|| Ok(())),
        );
        clock.press("add-minute", Some("timer-1"));
        let (_, controls) = clocks.controls(0).await.expect("Clocks takes the control");
        let controls: serde_json::Value = serde_json::from_slice(&controls).expect("JSON");
        assert_eq!(
            controls,
            serde_json::json!([{ "command": "add-minute", "activity_id": "timer-1" }])
        );
    }
}
