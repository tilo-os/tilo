//! What the session itself tells the person, through their notifications.
//! The desktop knows things the person should: storage and battery running out,
//! the media codecs they asked for, a session or computer that stopped and started
//! again, an update waiting for a restart, a deployment it went back from, graphics
//! that fell back to software, a drive ready to open, and the state of their own
//! workspaces, agents and apps it could not read. Each becomes an ordinary
//! notification ([`tilo_notifications_service::Notification`]) published by the
//! notifications service, so the banners and the centre show it as any app's, and
//! nothing here draws. They are the whole machine's news and show on every display
//! ([`Notification::machine_wide`]); the one about the saved desk belongs to the
//! desk the person is looking at. Nothing is polled hard: storage is one `statvfs`
//! per filesystem every two minutes (twenty seconds while one is low), codecs are a
//! watch on the helper's status file, and the rest is read once per session pass.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use settings_store::system_sounds::SystemSoundEvent;
use tilo_desktop_surface::DesktopNotifications;
use tilo_desktop_surface::notifications::{
    Notification as DeskNotification, NotificationIconKind, NotificationSeverity,
    PERSISTENT_DISMISS,
};
use tilo_files::native::ChangeWatch;
use tilo_liveness::watchdog::{Policy, Watchdog, end_process};
use tilo_notifications_service::{Notification, Notifications};
use tilo_system_service::update;
use ui::format::NaiveDate;

use crate::desk_doc::StateNotice;
use crate::i18n::{t, t_args};
use crate::mount::drives::Medium;

/// The action the update notice offers: restart the machine into the staged update.
pub const RESTART_ACTION: &str = update::RESTART_TO_UPDATE;

/// The action a plugged-in drive's notice offers: open it in Files on its mount point.
pub const OPEN_MEDIUM_ACTION: &str = "open-medium";

/// The action a plugged-in drive's notice offers: eject it, so it can be pulled out.
pub const EJECT_MEDIUM_ACTION: &str = "eject-medium";

/// How often storage is looked at, and how often while one filesystem is low: it is when it changes
/// fastest.
const STORAGE_EVERY: Duration = Duration::from_secs(120);
const STORAGE_LOW_EVERY: Duration = Duration::from_secs(20);

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;

/// One notice to show, before it becomes a notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// The name the person sees it came from ("Storage").
    pub app: String,
    /// `0` low, `1` normal, `2` critical: what the banner and the centre order it by.
    pub urgency: u8,
    pub title: String,
    pub body: String,
    /// `(key, label)` pairs, as a notification's actions.
    pub actions: Vec<(String, String)>,
    /// How long it stays on screen; `None` leaves that to the person's own notification settings.
    pub expires_after: Option<Duration>,
    /// Whether every display shows it: the whole machine's news, or, for a heads-up about the pack,
    /// the display the person is using.
    pub machine_wide: bool,
    /// The event sound the notice makes, played by the session itself: a sender's category never
    /// reaches the one an app may not ask for ([`SystemSoundEvent::LowBattery`]), so the notice
    /// that needs it plays it here and tells the service no sound is owed.
    pub sound: Option<SystemSoundEvent>,
}

impl Notice {
    /// A notice `app` tells the person, at `urgency` .
    pub(crate) fn new(app: String, urgency: u8, title: String, body: String) -> Self {
        Self {
            app,
            urgency,
            title,
            body,
            actions: Vec::new(),
            expires_after: None,
            machine_wide: true,
            sound: None,
        }
    }

    /// This notice as an ordinary notification, under a fresh id.
    fn into_notification(self, notifications: &Notifications) -> Notification {
        Notification {
            id: notifications.alloc_id(),
            app_name: self.app,
            summary: self.title,
            body: self.body,
            actions: self.actions,
            expire_timeout: self.expires_after.map_or(-1, |after| {
                i32::try_from(after.as_millis()).unwrap_or(i32::MAX)
            }),
            urgency: self.urgency,
            machine_wide: self.machine_wide,
            suppress_sound: self.sound.is_some(),
            // The session raises its own notice: it is the system's word about the machine, not a
            // sender's.
            system: true,
            ..Notification::new(0)
        }
    }

    /// Show it to the person.
    pub(crate) fn tell(self, notifications: &Notifications) {
        notifications.push(self.into_notification(notifications));
    }

    /// Show it on the desk this session draws, for a session with no notifications service: what
    /// the session raises is kept there rather than lost, and it stays until the person lets it go.
    /// What the notice offers the person it offers there too, so a question keeps its answers. Says
    /// which notification went out: the notice whose answer the session waits on finds it by that
    /// id.
    pub(crate) fn tell_desk(self, desk: &DesktopNotifications) -> u64 {
        let severity = match self.urgency {
            0 => NotificationSeverity::Info,
            1 => NotificationSeverity::Warning,
            _ => NotificationSeverity::Error,
        };
        // The notice's own timing, as the notifications service would apply it: nothing asks to
        // stay, or asks for no time at all, stays.
        let dismiss_after = self
            .expires_after
            .filter(|after| !after.is_zero())
            .unwrap_or(PERSISTENT_DISMISS);
        let mut desk_notice = DeskNotification::new(self.title, self.body)
            .with_app_name(self.app)
            .with_icon_kind(NotificationIconKind::System)
            .with_severity(severity)
            .with_dismiss_after(dismiss_after);
        desk_notice.actions = self.actions;
        desk.post(if self.machine_wide {
            desk_notice.machine_wide()
        } else {
            desk_notice
        })
    }

    /// Show it to the person, and say which notification went out: a producer whose notice the
    /// person acts on needs its id to know what they meant.
    pub(crate) fn tell_as(self, notifications: &Notifications) -> u32 {
        let notification = self.into_notification(notifications);
        let id = notification.id;
        notifications.push(notification);
        id
    }
}

/// How far a filesystem has run out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Plenty,
    Low,
    Full,
}

/// Room a filesystem has left, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Workspace {
    pub capacity: u64,
    pub available: u64,
}

/// The level `room` is at, given the level it was at before. Recovering needs half as much room
/// again as falling takes, so a filesystem hovering at a limit does not announce itself over and
/// over.
#[must_use]
pub fn level(room: Workspace, before: Level) -> Level {
    let low_limit = (room.capacity / 20).clamp(2 * GIB, 10 * GIB);
    let full_limit = 512 * MIB;
    let (low, full) = match before {
        Level::Plenty => (low_limit, full_limit),
        Level::Low | Level::Full => (
            low_limit.saturating_add(low_limit / 2),
            full_limit.saturating_add(full_limit / 2),
        ),
    };
    if room.available < full {
        Level::Full
    } else if room.available < low {
        Level::Low
    } else {
        Level::Plenty
    }
}

/// Which filesystem a notice is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Volume {
    Home,
    System,
}

/// The notice for a move from `before` to `after` , if it is worth one: only getting worse is
/// announced.
#[must_use]
pub fn storage_notice(
    volume: Volume,
    before: Level,
    after: Level,
    room: Workspace,
) -> Option<Notice> {
    if after <= before {
        return None;
    }
    let free = ui::format::size(room.available);
    let volume = match volume {
        Volume::Home => t("storage-volume-home"),
        Volume::System => t("storage-volume-system"),
    };
    let args = [("free", free.into()), ("volume", volume.into())];
    Some(match after {
        Level::Plenty => return None,
        Level::Low => Notice::new(
            t("storage-app-name"),
            1,
            t("storage-low-title"),
            t_args("storage-low-body", &args),
        ),
        Level::Full => Notice::new(
            t("storage-app-name"),
            2,
            t("storage-full-title"),
            t_args("storage-full-body", &args),
        ),
    })
}

/// The notice that the battery is running out: a heads-up at low charge, and at critical charge one
/// that stays until the person acts and, when the policy sleeps the machine for it, says so.
#[must_use]
pub fn battery_notice(percent: u8, critical: bool, suspending: bool) -> Notice {
    let percent = percent.to_string();
    let args = [("percent", percent.as_str().into())];
    let (urgency, title, body) = if critical {
        let body = if suspending {
            t_args("battery-critical-suspend-body", &args)
        } else {
            t_args("battery-critical-plug-body", &args)
        };
        (2, t("battery-critical-title"), body)
    } else {
        (1, t("battery-low-title"), t_args("battery-low-body", &args))
    };
    Notice {
        app: t("battery-app-name"),
        urgency,
        title,
        body,
        actions: Vec::new(),
        // The critical warning stays until the person lets it go; the heads-up leaves on the
        // person's own notification timing.
        expires_after: critical.then_some(Duration::ZERO),
        machine_wide: critical,
        sound: Some(SystemSoundEvent::LowBattery),
    }
}

/// The notice for the medium a person just plugged in: what it is called and how much it holds, and
/// the two things they want done with it.
#[must_use]
pub fn medium_notice(medium: &Medium) -> Notice {
    let name = medium.label.clone().unwrap_or_else(|| t("drives-unnamed"));
    let title = t_args(
        "drives-medium-title",
        &[
            ("name", name.into()),
            ("size", ui::format::size(medium.size).into()),
        ],
    );
    let mut notice = Notice::new(t("drives-app-name"), 1, title, t("drives-body"));
    notice.actions = vec![
        (OPEN_MEDIUM_ACTION.to_owned(), t("drives-open")),
        (EJECT_MEDIUM_ACTION.to_owned(), t("drives-eject")),
    ];
    notice
}

/// Tell the person what they just plugged in, and return the notification's id, so what they ask of
/// it can be carried out.
#[must_use]
pub fn announce_medium(notifications: &Notifications, medium: &Medium) -> u32 {
    medium_notice(medium).tell_as(notifications)
}

/// The notice for how a request to wake one of the person's devices ended,
/// or `None` while there is nothing to tell: the signal went out and the
/// session is still waiting for the device.
#[must_use]
pub fn wake_notice(event: &tilo_device_fabric::wake::WakeEvent) -> Option<Notice> {
    use tilo_device_fabric::WakeOutcome;
    let device = event.label.as_str();
    let (title, body) = match event.outcome {
        WakeOutcome::Sent { .. } => return None,
        WakeOutcome::Rejoined => (
            t_args("device-wake-rejoined", &[("device", device.into())]),
            String::new(),
        ),
        WakeOutcome::NotRejoined => (
            t_args("device-wake-not-rejoined", &[("device", device.into())]),
            t("device-wake-not-rejoined-body"),
        ),
        WakeOutcome::NotOnTargetLan | WakeOutcome::SendFailed => (
            t_args("device-wake-failed", &[("device", device.into())]),
            t("device-wake-failed-body"),
        ),
    };
    Some(Notice::new(t("device-wake-app-name"), 1, title, body))
}

/// Tell the person the desk this device saved was not opened: it is still on disk and untouched,
/// and this session starts on an empty desk and keeps nothing, so it cannot be written over. It
/// stays until they let it go — a desk that is not theirs looks exactly like theirs.
pub(crate) fn tell_desk_not_opened(notices: &DesktopNotifications, refusal: &StateNotice) {
    notices.post(
        DeskNotification::new(refusal.title(), refusal.explains())
            .with_app_name(t("desk-not-opened-app-name"))
            .with_severity(NotificationSeverity::Error)
            .with_dismiss_after(PERSISTENT_DISMISS),
    );
}

/// Tell the person the desk style they chose could not be shown: its package did not load, or
/// its view failed when it ran, so the desk draws the built-in strips while their choice stays
/// as it was (`shell.md` §6, "Nothing can lock a person out"). `style` is the style as the
/// switch names it, and `reason` what failed. It stays until they let it go: it says what the
/// desk is showing instead of what they picked.
pub(crate) fn desk_style_failed_notice(style: &str, reason: &str) -> Notice {
    let mut notice = Notice::new(
        t("desk-style-failed-app-name"),
        2,
        t("desk-style-failed-title"),
        t_args(
            "desk-style-failed-body",
            &[
                ("style", style.to_owned().into()),
                ("reason", reason.to_owned().into()),
            ],
        ),
    );
    notice.machine_wide = false;
    notice.expires_after = Some(Duration::from_secs(30));
    notice
}

/// What a paste of files has to tell the person, or `None` when it went through: the files it could
/// not paste and why, what stopped it before it began, or that a paste the last session was in the
/// middle of was taken back.
pub(crate) fn paste_notice(outcome: &crate::clipboard::paste::Outcome) -> Option<Notice> {
    use crate::clipboard::paste::Refused;
    // A paste a stopped session was in the middle of is answered for on its own: what it kept is
    // right, and what it could not put back is said plainly, because the next session tries again.
    if outcome.undone {
        let body = match &outcome.refusal {
            Some(Refused::Stopped(reason)) => t_args(
                "paste-undone-trouble",
                &[
                    ("landed", outcome.landed.to_string().into()),
                    ("total", outcome.total.to_string().into()),
                    ("reason", reason.as_str().into()),
                ],
            ),
            _ => t_args(
                "paste-undone-body",
                &[
                    ("landed", outcome.landed.to_string().into()),
                    ("total", outcome.total.to_string().into()),
                ],
            ),
        };
        return Some(Notice::new(
            t("paste-app-name"),
            0,
            t("paste-undone-title"),
            body,
        ));
    }
    if let Some(refusal) = &outcome.refusal {
        let body = match refusal {
            Refused::Nothing => t("paste-nothing-to-paste"),
            Refused::NotAFolder(path) => t_args(
                "paste-not-a-folder",
                &[("path", path.display().to_string().into())],
            ),
            Refused::Stopped(reason) => {
                t_args("paste-stopped", &[("reason", reason.as_str().into())])
            }
        };
        return Some(Notice::new(
            t("paste-app-name"),
            1,
            t("paste-refused-title"),
            body,
        ));
    }
    if !outcome.failed.is_empty() {
        let (name, reason) = outcome.failed.first()?;
        let body = t_args(
            "paste-failed-body",
            &[
                ("failed", outcome.failed.len().to_string().into()),
                ("total", outcome.total.to_string().into()),
                ("name", name.as_str().into()),
                ("reason", reason.as_str().into()),
            ],
        );
        return Some(Notice::new(
            t("paste-app-name"),
            1,
            t("paste-failed-title"),
            body,
        ));
    }
    None
}

/// Room `path` 's filesystem has left, when the path exists.
fn room(path: &Path) -> Option<Workspace> {
    let stat = rustix::fs::statvfs(path).ok()?;
    Some(Workspace {
        capacity: stat.f_blocks.saturating_mul(stat.f_frsize),
        available: stat.f_bavail.saturating_mul(stat.f_frsize),
    })
}

/// The filesystems to watch: the person's files, and the system's own when it is a different
/// filesystem.
fn volumes() -> Vec<(Volume, PathBuf)> {
    use std::os::unix::fs::MetadataExt as _;
    let mut volumes = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(home) = &home {
        volumes.push((Volume::Home, home.clone()));
    }
    let system = PathBuf::from("/var");
    let same_device = home.as_deref().is_some_and(|home| {
        match (std::fs::metadata(home), std::fs::metadata(&system)) {
            (Ok(home), Ok(system)) => home.dev() == system.dev(),
            _ => false,
        }
    });
    if !same_device {
        volumes.push((Volume::System, system));
    }
    volumes
}

/// One look at the watched filesystems, each level carried from `levels` to what it is now; `tell`
/// gets the notices that are due. Returns whether any filesystem is low or full, which decides how
/// soon to look again.
pub fn sweep_rooms(
    watched: &[(Volume, Option<Workspace>)],
    levels: &mut [Level],
    mut tell: impl FnMut(Notice),
) -> bool {
    let mut low = false;
    for ((volume, room), before) in watched.iter().zip(levels.iter_mut()) {
        let Some(room) = room else { continue };
        let after = level(*room, *before);
        if let Some(notice) = storage_notice(*volume, *before, after, *room) {
            tell(notice);
        }
        *before = after;
        low |= after != Level::Plenty;
    }
    low
}

/// The codecs notice for the status moving from `before` to `after` : the install finishing (with
/// or without the hardware part), the hardware part arriving later, and the install failing.
/// Routine refreshes and the person's own removal say nothing.
#[must_use]
pub fn codec_notice(
    before: &os_actions::codec_pack::CodecPackStatus,
    after: &os_actions::codec_pack::CodecPackStatus,
) -> Option<Notice> {
    use os_actions::codec_pack::{MesaState, PackState};
    let applies = || after.when_it_applies().unwrap_or_default().to_string();
    match (before.state, after.state) {
        (PackState::Installed, PackState::Installed) => (before.mesa != MesaState::Enabled
            && after.mesa == MesaState::Enabled)
            .then(|| Notice::new(t("codec-app-name"), 0, t("codec-hardware-ready"), applies())),
        (_, PackState::Installed) => Some(Notice::new(
            t("codec-app-name"),
            0,
            t("codec-installed"),
            if after.mesa == MesaState::Waiting {
                t_args(
                    "codec-installed-mesa-waiting",
                    &[("applies", applies().into())],
                )
            } else {
                applies()
            },
        )),
        (PackState::Installing | PackState::WaitingForNetwork, PackState::Failed) => {
            Some(Notice::new(
                t("codec-app-name"),
                2,
                t("codec-failed"),
                if after.last_error.is_empty() {
                    t("codec-failed-retry")
                } else {
                    t_args(
                        "codec-failed-with-error",
                        &[("error", after.last_error.as_str().into())],
                    )
                },
            ))
        }
        _ => None,
    }
}

/// The notice for `news` : a session or a computer that stopped and started again since the last
/// start. `today` is the local date now: a record from today names the time of day, an older one
/// its date as well.
#[must_use]
pub fn news_notice(news: &tilo_crash_records::News, today: NaiveDate) -> Notice {
    let time = if news.at.date() == today {
        ui::format::time_of(&news.at)
    } else {
        ui::format::timestamp(&news.at)
    };
    let (urgency, title, body) = match news.ended {
        tilo_crash_records::Ended::Froze => (
            1,
            t("crash-notice-title"),
            t_args("crash-notice-froze-body", &[("time", time.into())]),
        ),
        tilo_crash_records::Ended::Crashed => (
            1,
            t("crash-notice-title"),
            t_args("crash-notice-crashed-body", &[("time", time.into())]),
        ),
        tilo_crash_records::Ended::Stopped => (
            1,
            t("crash-notice-stopped-title"),
            t("crash-notice-stopped-body"),
        ),
    };
    Notice::new(t("crash-notice-app-name"), urgency, title, body)
}

/// The notice that this machine's graphics stopped: the renderers could not bring the GPU back, so
/// every frame is drawn in software from now on and only a restart of the computer brings the speed
/// back.
fn graphics_notice() -> Notice {
    Notice::new(
        t("graphics-app-name"),
        1,
        t("graphics-stopped-title"),
        t("graphics-stopped-body"),
    )
}

/// Whether the person was already told that this machine's graphics stopped.
static GRAPHICS_TOLD: AtomicBool = AtomicBool::new(false);

/// Tell the person, once, that the graphics stopped and everything keeps working, only slower. The
/// renderers of this process record that fallback where the shared rendering code decides it
/// (`ui::gpu::recover`), so each pass of the session's loop reads one record and says nothing until
/// it is set: a GPU that stops between two frames is told on the next pass, and never twice.
pub fn tell_graphics_fallback(notifications: &Notifications) {
    if TELLING.get().is_none() || GRAPHICS_TOLD.load(Ordering::Relaxed) {
        return;
    }
    if ui::gpu::software_fallback().is_none() {
        return;
    }
    GRAPHICS_TOLD.store(true, Ordering::Relaxed);
    graphics_notice().tell(notifications);
}

/// The notice that an update waits for a restart of the machine, offering the action that takes it:
/// raised by the update's own clock ([`crate::update_finish`]), at most every two days.
pub(crate) fn update_notice() -> Notice {
    Notice {
        app: t("system-update-app-name"),
        urgency: 1,
        title: t("system-update-restart-title"),
        body: t("system-update-restart-body"),
        actions: vec![(RESTART_ACTION.to_owned(), t("power-restart"))],
        // It waits in the notification centre; the banner goes after the person has had time to
        // read it.
        expires_after: Some(Duration::from_secs(30)),
        machine_wide: true,
        sound: None,
    }
}

/// The notice that the update state the services last published does not read back: this machine
/// cannot tell how far its last update got, so it acts on nothing it could not read and says so
/// once. Raised by the update's own clock ([`crate::update_finish`]), which keeps the warning for
/// operators.
pub(crate) fn update_state_notice() -> Notice {
    Notice {
        app: t("system-update-app-name"),
        urgency: 1,
        title: t("update-state-unreadable-title"),
        body: t("update-state-unreadable-body"),
        actions: Vec::new(),
        expires_after: None,
        machine_wide: true,
        sound: None,
    }
}

/// The notice that a collaborator's device speaks another peer protocol version, so the two cannot
/// work together until one of them updates: this device fetches the newest system when theirs is
/// the newer one, and otherwise theirs is the one to update. `device` is the person's name for it,
/// when the fabric holds the endpoint it was heard at. Raised once per device and version by the
/// clock that finishes the update ([`crate::update_finish`]).
pub(crate) fn version_mismatch_notice(
    mismatch: &crate::network::VersionMismatch,
    device: Option<&str>,
) -> Notice {
    let (title, body) = match (mismatch.theirs_is_newer(), device) {
        (true, Some(device)) => (
            t_args(
                "version-mismatch-updating-title",
                &[("device", device.into())],
            ),
            t_args(
                "version-mismatch-updating-body",
                &[("device", device.into())],
            ),
        ),
        (false, Some(device)) => (
            t_args(
                "version-mismatch-needs-update-title",
                &[("device", device.into())],
            ),
            t_args(
                "version-mismatch-needs-update-body",
                &[("device", device.into())],
            ),
        ),
        // The device was never met on the person's fabric, so its endpoint is all this device knows
        // of it: said plainly rather than as a hex string nobody reads.
        (true, None) => (
            t("version-mismatch-unknown-updating-title"),
            t("version-mismatch-unknown-updating-body"),
        ),
        (false, None) => (
            t("version-mismatch-unknown-needs-update-title"),
            t("version-mismatch-unknown-needs-update-body"),
        ),
    };
    Notice {
        app: t("system-update-app-name"),
        urgency: 1,
        title,
        body,
        actions: Vec::new(),
        expires_after: None,
        machine_wide: true,
        sound: None,
    }
}

/// The notice of a deployment this machine went back from: what did not start, how many tries it
/// spent, what runs instead, and where the failed tries left what they found. Raised once per
/// fallback by the update's own clock ([`crate::update_finish`]), and it stays until the person
/// dismisses it: a computer that went back from an update is not news to lose.
pub(crate) fn went_back_notice(fallback: &update::Fallback) -> Notice {
    let what = fallback
        .version
        .clone()
        .or_else(|| fallback.image.clone())
        .unwrap_or_else(|| t("system-update-went-back-unnamed"));
    let tries = fallback.tries.map_or_else(
        || t("system-update-went-back-several-tries"),
        |tries| tries.to_string(),
    );
    let running = fallback
        .running
        .clone()
        .unwrap_or_else(|| t("system-update-went-back-runs-before"));
    let args = [
        ("what", what.into()),
        ("tries", tries.into()),
        ("running", running.into()),
        ("summary", fallback.summary.as_str().into()),
    ];
    Notice {
        app: t("system-update-app-name"),
        urgency: 2,
        title: t("system-update-went-back-title"),
        body: t_args("system-update-went-back-body", &args),
        actions: Vec::new(),
        expires_after: None,
        machine_wide: true,
        sound: None,
    }
}

/// The notice that the workspaces this computer remembered do not read back: the list starts empty,
/// so it says why rather than letting an empty list look like workspaces that are gone. Raised
/// once, by the pass that opens the list ([`crate::workspaces::Personal::open`]), which keeps the
/// warning for operators.
pub(crate) fn workspaces_notice() -> Notice {
    Notice {
        app: t("workspaces-app-name"),
        urgency: 1,
        title: t("workspaces-unreadable-title"),
        body: t("workspaces-unreadable-body"),
        actions: Vec::new(),
        expires_after: None,
        machine_wide: true,
        sound: None,
    }
}

/// The notice that the "Always allow" answers this computer kept for the agents at its door do not
/// read back: `agent` is the one the answer was given to, as far as the kept file still says, and
/// `None` when it does not say even that. Raised by the door's own mount
/// ([`crate::agent_door::mount_named`]), which keeps the warning for operators; each answer is
/// asked for again the next time its agent connects.
pub(crate) fn agent_bindings_notice(agent: Option<&str>) -> Notice {
    let (title, body) = match agent {
        Some(agent) => (
            t_args("agent-bindings-lost-title", &[("agent", agent.into())]),
            t_args("agent-bindings-lost-body", &[("agent", agent.into())]),
        ),
        None => (
            t("agent-bindings-unreadable-title"),
            t("agent-bindings-unreadable-body"),
        ),
    };
    Notice {
        app: t("agent-bindings-app-name"),
        urgency: 1,
        title,
        body,
        actions: Vec::new(),
        expires_after: None,
        machine_wide: true,
        sound: None,
    }
}

/// The notice that the registry of the programs the person downloaded does not read back: none of
/// their apps is listed until it does, so it says why rather than letting them look gone. Raised
/// once, by the pass that reads it ([`crate::Shared::downloaded_apps`]), which keeps the warning
/// for operators.
pub(crate) fn downloaded_apps_notice() -> Notice {
    Notice {
        app: t("downloaded-apps-app-name"),
        urgency: 1,
        title: t("downloaded-apps-unreadable-title"),
        body: t("downloaded-apps-unreadable-body"),
        actions: Vec::new(),
        expires_after: None,
        machine_wide: true,
        sound: None,
    }
}

/// Why a device or a workspace did not take a change, in the person's own words: the rest of the
/// sentence that says the change stays here.
#[must_use]
pub(crate) fn reason_in_words(reason: neighbour_wire::RefusalReason) -> String {
    use neighbour_wire::RefusalReason;
    t(match reason {
        RefusalReason::Revoked => "refusal-reason-revoked",
        RefusalReason::Expired => "refusal-reason-expired",
        RefusalReason::NotCertified => "refusal-reason-not-certified",
        RefusalReason::Forbidden => "refusal-reason-forbidden",
        RefusalReason::Malformed => "refusal-reason-malformed",
        RefusalReason::MissingDependency => "refusal-reason-waiting",
        RefusalReason::Conflict => "refusal-reason-conflict",
        RefusalReason::Full => "refusal-reason-full",
        RefusalReason::Other => "refusal-reason-other",
    })
}

/// The notice that a device stopped taking one of this device's writes, and why: waiting changes
/// nothing, so the change stays on the devices that took it and the person hears it once. Raised by
/// the pass that serves the activity ([`crate::Shared::serve_activity`]).
pub(crate) fn refused_write_notice(device: &str, reason: neighbour_wire::RefusalReason) -> Notice {
    Notice {
        app: t("refused-write-app"),
        urgency: 2,
        title: t_args("refused-write-title", &[("device", device.into())]),
        body: t_args(
            "refused-write-body",
            &[("reason", reason_in_words(reason).into())],
        ),
        actions: Vec::new(),
        expires_after: None,
        machine_wide: true,
        sound: None,
    }
}

/// The workspace did not take what the person changed on its shared desk: what they changed stays
/// here.
pub(crate) fn desk_not_shared_notice(
    workspace: &str,
    reason: neighbour_wire::RefusalReason,
) -> Notice {
    Notice {
        app: t("desk-not-shared-app"),
        urgency: 2,
        title: t_args("desk-not-shared-title", &[("workspace", workspace.into())]),
        body: t_args(
            "desk-not-shared-body",
            &[("reason", reason_in_words(reason).into())],
        ),
        actions: Vec::new(),
        expires_after: None,
        machine_wide: true,
        sound: None,
    }
}

/// Tell the person what the workspace did not take of their shared desk: it is on the desk they
/// used, once, naming the workspace and why.
pub(crate) fn tell_desk_not_taken(
    notifications: &std::cell::RefCell<crate::notifications::SessionNotifications>,
    tree: &tilo_desktop_model::DesktopTree,
    (room, why): (tilo_desktop_model::WorkspaceId, String),
) {
    let workspace = tree
        .workspaces
        .iter()
        .find(|workspace| workspace.id() == room)
        .map_or_else(String::new, |workspace| workspace.name.clone());
    let reason = neighbour_wire::RefusalReason::of(&why);
    notifications
        .borrow()
        .tell(desk_not_shared_notice(&workspace, reason));
}

/// Tell the person what the crash records in `dir` say happened before this start: a session that
/// stopped responding, one that panicked, a computer that stopped. The records decide it, so
/// nothing is said twice.
pub fn tell_crash_news(notifications: &Notifications, dir: &Path) {
    let Some(news) = tilo_crash_records::take_news(dir) else {
        return;
    };
    let today = ui::clock::local_now().date_naive();
    news_notice(&news, today).tell(notifications);
}

/// The codecs status last seen by the watch.
static CODECS: Mutex<Option<os_actions::codec_pack::CodecPackStatus>> = Mutex::new(None);
/// The watch itself, held for the life of the process.
static CODECS_WATCH: Mutex<Option<ChangeWatch>> = Mutex::new(None);

/// Follow the codecs helper's status file, once per process, and tell the person of each transition
/// worth telling.
fn follow_codecs(notifications: &Notifications) {
    if !os_actions::codec_pack::available() {
        return;
    }
    if let Ok(mut last) = CODECS.lock() {
        *last = Some(os_actions::codec_pack::CodecPackStatus::read());
    }
    let held = notifications.clone();
    let watched = os_actions::codec_pack::watch(move || {
        let after = os_actions::codec_pack::CodecPackStatus::read();
        let Ok(mut last) = CODECS.lock() else {
            return;
        };
        if let Some(before) = last.as_ref()
            && let Some(notice) = codec_notice(before, &after)
        {
            notice.tell(&held);
        }
        *last = Some(after);
    });
    match watched {
        Ok(watch) => {
            if let Ok(mut held) = CODECS_WATCH.lock() {
                *held = Some(watch);
            }
        }
        Err(error) => tracing::warn!(%error, "codec pack notices are unavailable"),
    }
}

/// Watch the compositor's main loop and end the process when it has stopped
/// for good: record why first, then exit, so the person comes back to the
/// login screen rather than to a frozen display. The loop reports its turns
/// through [`tilo_compositor::LoopTurns`]; a turn it never ends is judged by
/// what the kernel says the thread is doing
/// ([`tilo_liveness::watchdog`]).
///
/// # Errors
/// The watchdog thread could not be started.
pub fn guard_main_loop(compositor: &mut tilo_compositor::Compositor) -> std::io::Result<()> {
    let watchdog = Watchdog::watch_this_thread(Policy::COMPOSITOR, |hang| {
        match tilo_crash_records::record_freeze(
            hang.tid,
            hang.blocked_for,
            &hang.report,
            tilo_crash_records::RECORD_WITHIN,
        ) {
            Some(path) => eprintln!("tilo session: hang recorded in {}", path.display()),
            None => eprintln!("tilo session: the record of the hang was not written"),
        }
        end_process(hang);
    })?;
    compositor.watch_turns(Some(Arc::new(Turns(watchdog))));
    Ok(())
}

/// The loop's turns as the watchdog counts them.
struct Turns(Watchdog);

impl tilo_compositor::LoopTurns for Turns {
    fn parking(&self) {
        self.0.parking();
    }

    fn unparked(&self) {
        self.0.unparked();
    }
}

/// Whether this session is the one that tells the person what it knows of
/// the machine: a session in a window of another desktop, or a headless one,
/// leaves that machine's news to the session that owns it.
static TELLING: OnceLock<()> = OnceLock::new();

/// Start telling the person what this desktop knows about its machine: the crash records of the
/// last start now, then storage and the codecs as they change. Once per process.
pub fn start(notifications: &Notifications) {
    if TELLING.set(()).is_err() {
        return;
    }
    // The records are files, and the kernel's are read through commands: the main thread walks no
    // path.
    let news = notifications.clone();
    let crash_news = std::thread::Builder::new()
        .name("crash-news".into())
        .spawn(move || {
            let Some(dir) = tilo_crash_records::directory() else {
                return;
            };
            // What the kernel kept of the computer stopping becomes a record first, so it is news
            // like the session's own.
            tilo_crash_records::stopped::take_in(&dir);
            tell_crash_news(&news, &dir);
        });
    if let Err(error) = crash_news {
        tracing::warn!(%error, "crash records are not watched for");
    }
    follow_codecs(notifications);

    let storage = notifications.clone();
    let storage_watch = std::thread::Builder::new()
        .name("storage-watch".into())
        .spawn(move || watch_storage(&storage));
    if let Err(error) = storage_watch {
        tracing::warn!(%error, "storage is not watched for");
    }
}

/// Watch the free space of the person's filesystems, announcing each one that gets worse.
fn watch_storage(notifications: &Notifications) {
    let watched = volumes();
    let mut levels = vec![Level::Plenty; watched.len()];
    loop {
        let rooms: Vec<(Volume, Option<Workspace>)> = watched
            .iter()
            .map(|(volume, path)| (*volume, room(path)))
            .collect();
        let low = sweep_rooms(&rooms, &mut levels, |notice| notice.tell(notifications));
        std::thread::sleep(if low {
            STORAGE_LOW_EVERY
        } else {
            STORAGE_EVERY
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use os_actions::codec_pack::CodecPackStatus;
    use tilo_compositor::LoopTurns as _;
    use tilo_notifications_service::Taken;
    use tilo_service::Service;

    fn room(capacity_gib: u64, available_mib: u64) -> Workspace {
        Workspace {
            capacity: capacity_gib.saturating_mul(GIB),
            available: available_mib.saturating_mul(MIB),
        }
    }

    #[test]
    fn levels_follow_the_room_left() {
        assert_eq!(level(room(500, 100 * 1024), Level::Plenty), Level::Plenty);
        // 5 % of 500 GB is capped at 10 GB.
        assert_eq!(level(room(500, 9 * 1024), Level::Plenty), Level::Low);
        // A small disk still warns with 2 GB left.
        assert_eq!(level(room(20, 1500), Level::Plenty), Level::Low);
        assert_eq!(level(room(44, 20), Level::Plenty), Level::Full);
    }

    #[test]
    fn hovering_at_a_limit_does_not_flap() {
        let at_limit = room(44, 2300);
        assert_eq!(level(at_limit, Level::Plenty), Level::Plenty);
        assert_eq!(
            level(at_limit, Level::Low),
            Level::Low,
            "recovery needs more room than falling"
        );
        assert_eq!(level(room(44, 4 * 1024), Level::Low), Level::Plenty);
    }

    #[test]
    fn only_getting_worse_is_announced() {
        let full = room(44, 20);
        assert!(storage_notice(Volume::System, Level::Plenty, Level::Low, full).is_some());
        assert!(storage_notice(Volume::System, Level::Low, Level::Full, full).is_some());
        assert!(storage_notice(Volume::System, Level::Full, Level::Full, full).is_none());
        assert!(storage_notice(Volume::System, Level::Full, Level::Plenty, full).is_none());
        assert!(storage_notice(Volume::Home, Level::Low, Level::Low, full).is_none());
    }

    /// The battery running out reaches the person as an ordinary notification: an early heads-up,
    /// then one that names what happens if the machine sleeps for it and stays until they act.
    #[test]
    fn a_battery_running_out_reaches_the_person_as_a_notification() {
        let notifications = Notifications::new();
        battery_notice(15, false, false).tell(&notifications);
        battery_notice(5, true, true).tell(&notifications);
        let live = notifications.live();
        assert_eq!(live.len(), 2);
        let low = live
            .iter()
            .find(|notification| notification.summary == "Battery low")
            .expect("the heads-up");
        assert!(low.body.starts_with("15%"), "{}", low.body);
        assert_eq!(low.urgency, 1);
        assert_eq!(
            low.expire_timeout, -1,
            "the heads-up leaves with the person's own notification timing"
        );
        assert!(
            low.suppress_sound,
            "the session plays the low-battery sound itself"
        );
        assert!(!low.machine_wide, "the heads-up is for the display in use");
        assert_eq!(low.app_name, "Power");

        let critical = live
            .iter()
            .find(|notification| notification.summary == "Battery critically low")
            .expect("the critical warning");
        assert!(critical.body.contains("Suspending"), "{}", critical.body);
        assert_eq!(critical.urgency, 2);
        assert_eq!(
            critical.expire_timeout, 0,
            "the critical warning stays until the person acts"
        );
        assert!(critical.machine_wide, "the whole machine's news");

        let plug_in = battery_notice(3, true, false);
        assert!(
            plug_in.body.contains("plug in"),
            "a policy that does not sleep says so instead: {}",
            plug_in.body
        );
    }

    /// A full filesystem reaches the person as an ordinary notification: it is published as a
    /// resource like any app's, reads as its copy, and the person closes it through the resource's
    /// own action.
    #[test]
    fn a_filesystem_running_out_reaches_the_person_as_a_notification() {
        let notifications = Notifications::new();
        let mut levels = [Level::Plenty];
        let watched = [(Volume::Home, Some(room(44, 300)))];
        let low = sweep_rooms(&watched, &mut levels, |notice| {
            notice.tell(&notifications);
        });
        assert!(low);
        assert_eq!(levels, [Level::Full]);

        let live = notifications.live();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].summary, "Storage is full");
        assert!(live[0].body.starts_with("Only "), "{}", live[0].body);
        assert!(live[0].machine_wide, "the whole machine's news");
        assert_eq!(live[0].app_name, "Storage");

        sweep_rooms(&watched, &mut levels, |_| {
            panic!("the same room twice says nothing");
        });

        let resource = tilo_notifications_service::notification_resource(live[0].id);
        let mut published = notifications.clone();
        let read = published.read(&resource).expect("the notice is published");
        assert_eq!(read["summary"], "Storage is full");
        published
            .invoke(&resource, "close", &serde_json::Map::new())
            .expect("the person closes it");
        assert!(notifications.live().is_empty());
    }

    /// A medium a person plugs in is announced by what it is called and how much it holds, and
    /// ejecting it from the notice reaches the eject.
    #[test]
    fn a_plugged_in_drive_is_announced_with_its_contents() {
        use crate::mount::drives::{Drives, Media, Medium};

        /// The udisks of a machine with one drive plugged into it, which remembers what it was
        /// asked to eject.
        #[derive(Default)]
        struct FakeUdisks {
            plugged_in: std::cell::RefCell<Vec<Medium>>,
            ejected: std::cell::RefCell<Vec<Medium>>,
        }

        impl Media for FakeUdisks {
            fn appeared(&self) -> Vec<Medium> {
                std::mem::take(&mut *self.plugged_in.borrow_mut())
            }

            fn eject(&self, medium: &Medium) {
                self.ejected.borrow_mut().push(medium.clone());
            }
        }

        let photos = Medium {
            device: "/org/freedesktop/UDisks2/block_devices/sdb1".into(),
            label: Some("Photos".into()),
            size: 3_400_000_000,
            mount: "/run/media/person/Photos".into(),
        };
        let notifications = Notifications::new();
        let udisks = std::rc::Rc::new(FakeUdisks::default());
        udisks.plugged_in.borrow_mut().push(photos.clone());
        let drives = Drives::new(udisks.clone(), notifications.clone());
        drives.follow(None);

        let live = notifications.live();
        assert_eq!(live.len(), 1, "one drive, one notice");
        assert_eq!(live[0].app_name, "Drives");
        assert_eq!(live[0].summary, "Photos (3.4 GB)");
        assert_eq!(
            live[0]
                .actions
                .iter()
                .map(|(_, label)| label.as_str())
                .collect::<Vec<_>>(),
            ["Open", "Eject"]
        );

        drives.follow(None);
        assert_eq!(
            notifications.live().len(),
            1,
            "the same drive twice says nothing"
        );

        let resource = tilo_notifications_service::notification_resource(live[0].id);
        let mut arguments = serde_json::Map::new();
        arguments.insert("action".into(), EJECT_MEDIUM_ACTION.into());
        let mut published = notifications;
        published
            .invoke(&resource, "invoke", &arguments)
            .expect("the notice offers an eject");
        drives.follow(None);
        assert_eq!(*udisks.ejected.borrow(), [photos]);
    }

    /// A filesystem that cannot be read says nothing, and keeps the level it had for when it is
    /// back.
    #[test]
    fn an_unreadable_filesystem_is_quiet() {
        let mut levels = [Level::Low];
        let low = sweep_rooms(&[(Volume::System, None)], &mut levels, |_| {
            panic!("nothing to say")
        });
        assert!(!low);
        assert_eq!(levels, [Level::Low]);
    }

    fn status(json: &str) -> CodecPackStatus {
        CodecPackStatus::parse(json)
    }

    #[test]
    fn a_deferred_install_that_finishes_online_is_announced() {
        let notice = codec_notice(
            &status(r#"{"state":"waiting-for-network","desired":true}"#),
            &status(
                r#"{"state":"installed","parts":{"tilo-codecs-mesa":"enabled"},"desired":true}"#,
            ),
        )
        .expect("the install finished");
        assert_eq!(notice.title, "More video and audio formats are on");
        assert!(
            notice.body.contains("Sign out and back in"),
            "{}",
            notice.body
        );
        assert_eq!(notice.urgency, 0);
    }

    #[test]
    fn a_software_only_install_says_no_restart_and_that_hardware_follows() {
        let notice = codec_notice(
            &status(r#"{"state":"installing"}"#),
            &status(r#"{"state":"installed","parts":{"tilo-codecs-mesa":"waiting"}}"#),
        )
        .expect("the install finished");
        assert!(notice.body.contains("No restart is needed"));
        assert!(notice.body.contains("Hardware H.264/HEVC follows"));
    }

    #[test]
    fn a_later_matching_mesa_build_is_announced_once() {
        let before = status(r#"{"state":"installed","parts":{"tilo-codecs-mesa":"waiting"}}"#);
        let after = status(r#"{"state":"installed","parts":{"tilo-codecs-mesa":"enabled"}}"#);
        assert_eq!(
            codec_notice(&before, &after)
                .expect("the hardware part arrived")
                .title,
            "Hardware H.264/HEVC is ready"
        );
        assert_eq!(
            codec_notice(&after, &after),
            None,
            "routine refreshes are quiet"
        );
    }

    #[test]
    fn failures_point_to_settings_and_progress_is_quiet() {
        let failed = codec_notice(
            &status(r#"{"state":"installing"}"#),
            &status(r#"{"state":"failed","last_error":"signature check failed"}"#),
        )
        .expect("the install failed");
        assert_eq!(failed.urgency, 2);
        assert!(failed.body.starts_with("signature check failed"));
        assert!(failed.body.contains("Settings, Updates"));
        assert_eq!(
            codec_notice(
                &status(r#"{"state":"not-installed"}"#),
                &status(r#"{"state":"installing"}"#)
            ),
            None
        );
        assert_eq!(
            codec_notice(
                &status(r#"{"state":"installed"}"#),
                &status(r#"{"state":"not-installed"}"#)
            ),
            None,
            "removing is the owner's own action"
        );
    }

    #[test]
    fn the_notice_of_a_session_that_restarted_says_when() {
        let dir = tempfile::tempdir().unwrap();
        tilo_files::native::private::directory(dir.path()).unwrap();
        let notifications = Notifications::new();
        // The first look marks where the history ends: what is already there is not news.
        tell_crash_news(&notifications, dir.path());
        assert!(notifications.live().is_empty());

        tilo_crash_records::write_record(
            dir.path(),
            "2026-10-24-225201-9-freeze.log",
            "tilo session freeze\n===================\nversion:   0.1.0\n",
        )
        .unwrap();
        tell_crash_news(&notifications, dir.path());
        let live = notifications.live();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].summary, "The desktop restarted");
        let at =
            ui::format::NaiveDateTime::parse_from_str("2026-10-24 22:52:01", "%Y-%m-%d %H:%M:%S")
                .unwrap();
        assert!(
            live[0].body.contains(&ui::format::timestamp(&at)),
            "{}",
            live[0].body
        );

        tell_crash_news(&notifications, dir.path());
        assert_eq!(notifications.live().len(), 1, "said once");
    }

    /// A machine whose GPU cannot be brought back draws every frame in
    /// software, and the person hears it once: one warning from "Graphics",
    /// an ordinary notification like any app's, and one entry in the centre
    /// however many passes follow.
    #[test]
    fn a_gpu_given_up_is_told_once() {
        use ui::vello::wgpu::{AdapterInfo, Backend, DeviceType};
        // The notice sources speak only in the session that owns the machine (`start`); a test
        // tells as that session does.
        TELLING.get_or_init(|| ());
        ui::gpu::reset_software_fallback_for_test();
        let notifications = Notifications::new();

        // While the GPU draws the frames there is nothing to say.
        tell_graphics_fallback(&notifications);
        assert!(
            notifications.live().is_empty(),
            "a working GPU says nothing"
        );

        // What the renderers decide for a device lost before it finished a frame: that GPU cannot
        // be brought back, and every frame after is drawn in software.
        let adapter = AdapterInfo {
            name: "Adreno X1-85".to_owned(),
            vendor: 0,
            device: 0,
            device_type: DeviceType::IntegratedGpu,
            device_pci_bus_id: String::new(),
            driver: String::new(),
            driver_info: String::new(),
            backend: Backend::Vulkan,
            subgroup_min_size: 4,
            subgroup_max_size: 128,
            transient_saves_memory: false,
        };
        assert_eq!(
            ui::gpu::recover(&adapter, false, "Device is lost"),
            ui::gpu::Recovery::Software,
            "a device lost before its first frame is on a GPU that cannot come back"
        );

        tell_graphics_fallback(&notifications);
        let live = notifications.live();
        assert_eq!(live.len(), 1, "one notice, not one for every frame");
        assert_eq!(live[0].app_name, "Graphics");
        assert_eq!(live[0].summary, "Graphics stopped responding");
        assert_eq!(
            live[0].body,
            "Everything keeps working, but may be slower until you restart the computer."
        );
        assert_eq!(live[0].urgency, 1, "a warning, not an error");
        assert!(live[0].machine_wide, "the whole machine's news");

        tell_graphics_fallback(&notifications);
        assert_eq!(notifications.live().len(), 1, "said once");

        // The centre holds that one entry, read and closed as any app's.
        let resource = tilo_notifications_service::notification_resource(live[0].id);
        let mut published = notifications.clone();
        let read = published.read(&resource).expect("the notice is published");
        assert_eq!(read["summary"], "Graphics stopped responding");
        published
            .invoke(&resource, "close", &serde_json::Map::new())
            .expect("the person closes it");
        assert!(notifications.live().is_empty());

        ui::gpu::reset_software_fallback_for_test();
    }

    #[test]
    fn a_computer_that_stopped_is_its_own_notice() {
        let news = tilo_crash_records::News {
            ended: tilo_crash_records::Ended::Stopped,
            at: ui::format::NaiveDateTime::parse_from_str(
                "2026-10-24 22:52:01",
                "%Y-%m-%d %H:%M:%S",
            )
            .unwrap(),
        };
        let notice = news_notice(&news, news.at.date().succ_opt().unwrap());
        assert_eq!(notice.title, "The computer restarted");
        assert!(notice.body.contains("stays on this computer"));
    }

    /// What this computer could not read of the person's workspaces and of its
    /// own update reaches them as ordinary notifications: what did not read,
    /// and what is still true for them.
    #[test]
    fn what_this_computer_could_not_read_reaches_the_person() {
        let notifications = Notifications::new();
        workspaces_notice().tell(&notifications);
        update_state_notice().tell(&notifications);

        let live = notifications.live();
        assert_eq!(live.len(), 2, "each failure is its own notice");

        let workspaces = live
            .iter()
            .find(|notification| notification.summary == "Your workspaces could not be read")
            .expect("the workspaces notice");
        assert_eq!(workspaces.app_name, "Workspaces");
        assert!(
            workspaces.body.contains("Nothing was removed"),
            "an empty list is not lost workspaces: {}",
            workspaces.body
        );
        assert!(
            workspaces.machine_wide,
            "the person's list is their news on every display"
        );

        let update = live
            .iter()
            .find(|notification| notification.summary == "The update state could not be read")
            .expect("the update notice");
        assert_eq!(update.app_name, "Updates");
        assert!(
            update.body.contains("did not act on it"),
            "the person learns nothing was done on a state it could not read: {}",
            update.body
        );
    }

    #[test]
    fn the_update_notice_offers_the_restart_of_the_machine() {
        let notice = update_notice();
        assert_eq!(notice.title, "Restart to finish updating");
        assert_eq!(
            notice.actions,
            [(RESTART_ACTION.to_owned(), "Restart".to_owned())]
        );
    }

    /// The compositor's hook marks the loop's turns as the watchdog counts them, so a loop that
    /// waits for events is never judged.
    #[test]
    fn the_compositor_hook_marks_the_loops_turns() {
        let watchdog = Watchdog::watch_this_thread(
            Policy {
                period: Duration::from_millis(50),
                hung_after: Duration::from_secs(60 * 60),
                report_within: Duration::from_secs(1),
            },
            |_| panic!("a loop that marks its turns is never judged hung"),
        )
        .expect("the watchdog thread starts");
        let turns = Turns(watchdog);
        for _ in 0..3 {
            turns.parking();
            std::thread::sleep(Duration::from_millis(20));
            turns.unparked();
        }
    }

    /// Acting on the update notice reaches the tap the session installed,
    /// through the notification's own published action.
    #[test]
    fn the_update_notice_carries_the_restart_action() {
        let notifications = Notifications::new();
        let tapped = std::sync::Arc::new(Mutex::new(Vec::new()));
        let seen = tapped.clone();
        notifications.on_taken(move |id, taken| {
            if let Taken::Action { key, .. } = taken
                && let Ok(mut seen) = seen.lock()
            {
                seen.push((id, key.to_owned()));
            }
        });
        let notice = update_notice();
        let id = notifications.alloc_id();
        let resource = tilo_notifications_service::notification_resource(id);
        notifications.push(Notification {
            id,
            app_name: notice.app,
            summary: notice.title,
            body: notice.body,
            actions: notice.actions,
            machine_wide: true,
            ..Notification::new(id)
        });
        let mut arguments = serde_json::Map::new();
        arguments.insert("action".into(), RESTART_ACTION.into());
        let mut published = notifications;
        published
            .invoke(&resource, "invoke", &arguments)
            .expect("the action is offered");
        assert_eq!(*tapped.lock().unwrap(), [(id, RESTART_ACTION.to_owned())]);
    }

    /// A collaborator's device at another protocol version reaches the person as a calm notice
    /// naming the device and what to do about it; one this device only knows by its endpoint is
    /// said plainly rather than spelled out. It waits until it is read.
    #[test]
    fn a_device_on_another_version_is_named_and_told_what_to_do() {
        let ours: u32 = 30;
        let newer = crate::network::VersionMismatch {
            endpoint: "zpeerexample".into(),
            theirs: ours.wrapping_add(1),
            ours,
        };
        let notice = version_mismatch_notice(&newer, Some("Framework 16"));
        assert!(
            notice.title.contains("Framework 16") && notice.title.contains("newer Tilo"),
            "the device names the title: {}",
            notice.title
        );
        assert!(
            notice.body.contains("restarts"),
            "the body says what happens here: {}",
            notice.body
        );
        assert!(notice.machine_wide, "the whole machine's news");
        assert!(notice.expires_after.is_none(), "it waits until it is read");

        let older = crate::network::VersionMismatch {
            theirs: ours.wrapping_sub(1),
            ..newer.clone()
        };
        let notice = version_mismatch_notice(&older, Some("Framework 16"));
        assert!(
            notice.title.contains("Framework 16") && notice.title.contains("needs to update"),
            "the device names the title: {}",
            notice.title
        );
        assert!(
            notice.body.contains("Update Tilo there"),
            "the one behind is the one that updates: {}",
            notice.body
        );

        for mismatch in [&newer, &older] {
            let unknown = version_mismatch_notice(mismatch, None);
            assert!(
                unknown.title.to_lowercase().contains("another device"),
                "a device the fabric cannot name is still named plainly: {}",
                unknown.title
            );
            assert!(
                !unknown.title.contains("zpeerexample") && !unknown.body.contains("zpeerexample"),
                "its endpoint is not the person's language"
            );
        }
    }

    /// A desk with no notifications service to show a notice still shows it as the system's: its
    /// own name and the system's mark, never a sender's claim and a generic window.
    #[test]
    fn a_notice_with_no_service_to_show_it_is_still_the_systems() {
        let desks = DesktopNotifications::default();
        let newer = crate::network::VersionMismatch {
            endpoint: "zpeerexample".into(),
            theirs: 31,
            ours: 30,
        };
        version_mismatch_notice(&newer, Some("Framework 16")).tell_desk(&desks);
        let banner = desks.log().active().first().cloned().expect("it is shown");
        assert_eq!(banner.app_name.as_deref(), Some("Updates"));
        assert_eq!(banner.mark().icon, Some(ui::icons::Icon::Bell));
    }

    /// The banner a person reads: the title names the device on one line and the body says what
    /// happens here in one sentence. Both are painted whole at the banner's own width, never cut
    /// with an ellipsis.
    #[test]
    fn a_version_mismatch_notice_reads_whole_on_its_banner() {
        let ours: u32 = 30;
        let newer = crate::network::VersionMismatch {
            endpoint: "zpeerexample".into(),
            theirs: ours.wrapping_add(1),
            ours,
        };
        let older = crate::network::VersionMismatch {
            theirs: ours.wrapping_sub(1),
            ..newer.clone()
        };
        for mismatch in [&newer, &older] {
            for device in [Some("Framework 16"), None] {
                let notice = version_mismatch_notice(mismatch, device);
                let mut text = ui::text_render::TextRenderer::new();
                let theme = ui::theme::Theme::dark();
                let words = |text: &mut ui::text_render::TextRenderer, width| {
                    ui::widget::toast::painted_words(
                        text,
                        &theme,
                        &notice.title,
                        &notice.body,
                        &notice.app,
                        width,
                    )
                };
                // A banner card is 360 points wide; at 2000 points nothing needs eliding or
                // clamping. A line break may drop the space that ended it, so the banner may paint
                // exactly one glyph fewer per extra line and not one more.
                let (banner_glyphs, banner_lines) = words(&mut text, 360.0);
                let (whole_glyphs, whole_lines) = words(&mut text, 2000.0);
                assert_eq!(
                    banner_glyphs + (banner_lines - whole_lines),
                    whole_glyphs,
                    "the banner cut the notice: {} / {}",
                    notice.title,
                    notice.body
                );
            }
        }
    }
}
