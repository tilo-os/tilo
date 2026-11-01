//! What the session itself tells the person, through their notifications.
//!
//! Six things the desktop knows and the person should: that storage is
//! running out, that the media codecs they asked for finished or failed, that
//! the session or the computer stopped and started again, that an update
//! waits for a restart, that the machine's graphics stopped and every frame is
//! drawn in software from now on, and that a drive they plugged in is ready to
//! open or eject. Each becomes an ordinary notification
//! ([`tilo_notifications_service::Notification`]) published by the
//! notifications service: the banners and the centre read it from the same
//! resource any app's notification is read from, the person closes or
//! acts on it as they would on any other, and nothing here draws.
//!
//! They are the whole machine's news, so they are shown on every display
//! ([`Notification::machine_wide`]).
//!
//! The one about the desk this device saved is the desktop's own: the person
//! sees it in the centre of the desk they are looking at, which is not the one
//! they left.
//!
//! Nothing is polled hard: storage is one `statvfs` per filesystem every two
//! minutes (every twenty seconds while one of them is low), the codecs are a
//! watch on the helper's status file, the crash records are read once at
//! start, the update state once the desktop has settled and then hourly, and
//! the graphics fallback is a read of what the renderers recorded, once per
//! pass of the session's loop.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use tilo_desktop_surface::DesktopNotifications;
use tilo_desktop_surface::notifications::{
    Notification as DeskNotification, NotificationSeverity, PERSISTENT_DISMISS,
};
use tilo_files::native::ChangeWatch;
use tilo_liveness::watchdog::{Policy, Watchdog, end_process};
use tilo_notifications_service::{Notification, Notifications, Taken};
use tilo_system_service::update;
use ui::format::NaiveDate;

use crate::desk_layout::StateNotice;
use crate::i18n::{t, t_args};
use crate::mount::drives::Medium;

/// The action the update notice offers: restart the machine into the staged
/// update.
pub const RESTART_ACTION: &str = "restart-to-update";

/// The action a plugged-in drive's notice offers: open it in Files on its
/// mount point.
pub const OPEN_MEDIUM_ACTION: &str = "open-medium";

/// The action a plugged-in drive's notice offers: eject it, so it can be
/// pulled out.
pub const EJECT_MEDIUM_ACTION: &str = "eject-medium";

/// The shortest time between two notices that an update waits for a restart.
/// They batch instead of interrupting the person every day.
pub const ASK_INTERVAL: Duration = Duration::from_secs(2 * 24 * 60 * 60);

/// When the session first looks for a staged update: once it has settled.
const FIRST_LOOK_AFTER: Duration = Duration::from_secs(10 * 60);

/// How often it looks again. Staging happens a few times a day at most.
const LOOK_EVERY: Duration = Duration::from_secs(60 * 60);

/// How often storage is looked at, and how often while one filesystem is low:
/// it is when it changes fastest.
const STORAGE_EVERY: Duration = Duration::from_secs(120);
const STORAGE_LOW_EVERY: Duration = Duration::from_secs(20);

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;

/// One notice to show, before it becomes a notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// The name the person sees it came from ("Storage").
    pub app: String,
    /// `0` low, `1` normal, `2` critical: what the banner and the centre
    /// order it by.
    pub urgency: u8,
    pub title: String,
    pub body: String,
    /// `(key, label)` pairs, as a notification's actions.
    pub actions: Vec<(String, String)>,
    /// How long it stays on screen; `None` leaves that to the person's own
    /// notification settings.
    pub expires_after: Option<Duration>,
}

impl Notice {
    fn new(app: String, urgency: u8, title: String, body: String) -> Self {
        Self {
            app,
            urgency,
            title,
            body,
            actions: Vec::new(),
            expires_after: None,
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
            machine_wide: true,
            ..Notification::new(0)
        }
    }

    /// Show it to the person.
    pub(crate) fn tell(self, notifications: &Notifications) {
        notifications.push(self.into_notification(notifications));
    }

    /// Show it to the person, and say which notification went out: a producer
    /// whose notice the person acts on needs its id to know what they meant.
    fn tell_as(self, notifications: &Notifications) -> u32 {
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
pub struct Room {
    pub capacity: u64,
    pub available: u64,
}

/// The level `room` is at, given the level it was at before. Recovering needs
/// half as much room again as falling takes, so a filesystem hovering at a
/// limit does not announce itself over and over.
#[must_use]
pub fn level(room: Room, before: Level) -> Level {
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

/// The notice for a move from `before` to `after`, if it is worth one: only
/// getting worse is announced.
#[must_use]
pub fn storage_notice(volume: Volume, before: Level, after: Level, room: Room) -> Option<Notice> {
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

/// The notice for the medium a person just plugged in: what it is called and
/// how much it holds, and the two things they want done with it.
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

/// Tell the person what they just plugged in, and return the notification's
/// id, so what they ask of it can be carried out.
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

/// Tell the person the desk this device saved was not opened: it is still on
/// disk and untouched, and this session starts on an empty desk and keeps
/// nothing, so it cannot be written over. It stays until they let it go — a
/// desk that is not theirs looks exactly like theirs.
pub(crate) fn tell_desk_not_opened(notices: &DesktopNotifications, refusal: &StateNotice) {
    notices.post(
        DeskNotification::new(refusal.title(), refusal.explains())
            .with_app_name(t("desk-not-opened-app-name"))
            .with_severity(NotificationSeverity::Error)
            .with_dismiss_after(PERSISTENT_DISMISS),
    );
}

/// Room `path`'s filesystem has left, when the path exists.
fn room(path: &Path) -> Option<Room> {
    let stat = rustix::fs::statvfs(path).ok()?;
    Some(Room {
        capacity: stat.f_blocks.saturating_mul(stat.f_frsize),
        available: stat.f_bavail.saturating_mul(stat.f_frsize),
    })
}

/// The filesystems to watch: the person's files, and the system's own when it
/// is a different filesystem.
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

/// One look at the watched filesystems, each level carried from `levels` to
/// what it is now; `tell` gets the notices that are due. Returns whether any
/// filesystem is low or full, which decides how soon to look again.
pub fn sweep_rooms(
    watched: &[(Volume, Option<Room>)],
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

/// The codecs notice for the status moving from `before` to `after`: the
/// install finishing (with or without the hardware part), the hardware part
/// arriving later, and the install failing. Routine refreshes and the
/// person's own removal say nothing.
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

/// The notice for `news`: a session or a computer that stopped and started
/// again since the last start. `today` is the local date now: a record from
/// today names the time of day, an older one its date as well.
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

/// The notice that this machine's graphics stopped: the renderers could not
/// bring the GPU back, so every frame is drawn in software from now on and
/// only a restart of the computer brings the speed back.
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

/// Tell the person, once, that the graphics stopped and everything keeps
/// working, only slower. The renderers of this process record that fallback
/// where the shared rendering code decides it (`ui::gpu::recover`), so each
/// pass of the session's loop reads one record and says nothing until it is
/// set: a GPU that stops between two frames is told on the next pass, and
/// never twice.
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

/// The notice that a staged update waits for a restart, offering the action
/// that takes it.
fn update_notice() -> Notice {
    Notice {
        app: t("system-update-app-name"),
        urgency: 1,
        title: t("system-update-restart-title"),
        body: t("system-update-restart-body"),
        actions: vec![(RESTART_ACTION.to_owned(), t("power-restart"))],
        // It waits in the notification centre; the banner goes after the
        // person has had time to read it.
        expires_after: Some(Duration::from_secs(30)),
    }
}

/// Whether to ask now: a restart is needed and the last request is at least
/// [`ASK_INTERVAL`] old, or there was none.
#[must_use]
pub fn update_due(needed: bool, last_asked: Option<SystemTime>, now: SystemTime) -> bool {
    needed
        && last_asked.is_none_or(|asked| {
            now.duration_since(asked)
                .is_ok_and(|since| since >= ASK_INTERVAL)
        })
}

/// Whether a staged update needs a restart of the machine: what the live
/// update says, or, where none runs, that an image is staged at all. An
/// update that applied live and waits only for the desktop says nothing here.
fn restart_needed() -> bool {
    update::restart_needed(update::staged(), update::live_state().as_ref())
}

/// When the person was last asked, as recorded at `path`.
fn last_asked_at(path: &Path) -> Option<SystemTime> {
    let seconds = std::fs::read_to_string(path).ok()?.trim().parse().ok()?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

fn record_asked_at(path: &Path, now: SystemTime) {
    let seconds = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    if let Err(error) =
        tilo_files::native::atomic::write_creating_parent(path, seconds.to_string().as_bytes())
    {
        tracing::warn!(%error, "could not record when a restart was last asked for");
    }
}

fn asked_path() -> Option<PathBuf> {
    tilo_paths::state_dir().map(|root| root.join("tilo/update-restart-asked"))
}

/// Restart the machine into the staged update, as the notice's action asks.
fn restart() {
    if let Err(error) =
        os_actions::session::perform_session_action(os_actions::SessionAction::Restart)
    {
        tracing::warn!(%error, "the machine could not restart into the update");
    }
}

/// Tell the person what the crash records in `dir` say happened before this
/// start: a session that stopped responding, one that panicked, a computer
/// that stopped. The records decide it, so nothing is said twice.
pub fn tell_crash_news(notifications: &Notifications, dir: &Path) {
    let Some(news) = tilo_crash_records::take_news(dir) else {
        return;
    };
    let today = ui::clock::local_now().date_naive();
    news_notice(&news, today).tell(notifications);
}

/// One look for a staged update that waits for a restart: ask the person at
/// most every [`ASK_INTERVAL`].
fn look_for_update(notifications: &Notifications) {
    let Some(path) = asked_path() else {
        return;
    };
    let now = SystemTime::now();
    if !update_due(restart_needed(), last_asked_at(&path), now) {
        return;
    }
    record_asked_at(&path, now);
    update_notice().tell(notifications);
}

/// The codecs status last seen by the watch.
static CODECS: Mutex<Option<os_actions::codec_pack::CodecPackStatus>> = Mutex::new(None);
/// The watch itself, held for the life of the process.
static CODECS_WATCH: Mutex<Option<ChangeWatch>> = Mutex::new(None);

/// Follow the codecs helper's status file, once per process, and tell the
/// person of each transition worth telling.
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
        Err(error) => tracing::debug!(%error, "codec pack notices are unavailable"),
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

/// Start telling the person what this desktop knows about its machine: the
/// crash records of the last start now, then storage, the codecs and a staged
/// update as they change. Once per process.
pub fn start(notifications: &Notifications) {
    if TELLING.set(()).is_err() {
        return;
    }
    // The records are files, and the kernel's are read through commands: the
    // main thread walks no path.
    let news = notifications.clone();
    let _ = std::thread::Builder::new()
        .name("crash-news".into())
        .spawn(move || {
            let Some(dir) = tilo_crash_records::directory() else {
                return;
            };
            // What the kernel kept of the computer stopping becomes a record
            // first, so it is news like the session's own.
            tilo_crash_records::stopped::take_in(&dir);
            tell_crash_news(&news, &dir);
        });
    follow_codecs(notifications);

    let storage = notifications.clone();
    let _ = std::thread::Builder::new()
        .name("storage-watch".into())
        .spawn(move || watch_storage(&storage));

    let updates = notifications.clone();
    let _ = std::thread::Builder::new()
        .name("update-watch".into())
        .spawn(move || {
            std::thread::sleep(FIRST_LOOK_AFTER);
            loop {
                look_for_update(&updates);
                std::thread::sleep(LOOK_EVERY);
            }
        });

    notifications.on_taken(move |_id, taken| {
        if let Taken::Action { key, .. } = taken
            && key == RESTART_ACTION
        {
            restart();
        }
    });
}

/// Watch the free space of the person's filesystems, announcing each one that
/// gets worse.
fn watch_storage(notifications: &Notifications) {
    let watched = volumes();
    let mut levels = vec![Level::Plenty; watched.len()];
    loop {
        let rooms: Vec<(Volume, Option<Room>)> = watched
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
    use tilo_service::Service;

    fn room(capacity_gib: u64, available_mib: u64) -> Room {
        Room {
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

    /// A full filesystem reaches the person as an ordinary notification: it is
    /// published as a resource like any app's, reads as its copy, and the
    /// person closes it through the resource's own action.
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

    /// A medium a person plugs in is announced by what it is called and how
    /// much it holds, and ejecting it from the notice reaches the eject.
    #[test]
    fn a_plugged_in_drive_is_announced_with_its_contents() {
        use crate::mount::drives::{Drives, Media, Medium};

        /// The udisks of a machine with one drive plugged into it, which
        /// remembers what it was asked to eject.
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

    /// A filesystem that cannot be read says nothing, and keeps the level it
    /// had for when it is back.
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
        // The first look marks where the history ends: what is already there
        // is not news.
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
        // The notice sources speak only in the session that owns the machine
        // (`start`); a test tells as that session does.
        TELLING.get_or_init(|| ());
        ui::gpu::reset_software_fallback_for_test();
        let notifications = Notifications::new();

        // While the GPU draws the frames there is nothing to say.
        tell_graphics_fallback(&notifications);
        assert!(
            notifications.live().is_empty(),
            "a working GPU says nothing"
        );

        // What the renderers decide for a device lost before it finished a
        // frame: that GPU cannot be brought back, and every frame after is
        // drawn in software.
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

    #[test]
    fn a_staged_update_asks_for_a_restart_at_most_every_two_days() {
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let hours = |h: u64| start + Duration::from_secs(h * 60 * 60);
        assert!(
            !update_due(false, None, start),
            "nothing staged, nothing asked"
        );
        assert!(
            update_due(true, None, start),
            "the first staged update asks"
        );
        assert!(!update_due(true, Some(start), hours(47)));
        assert!(update_due(true, Some(start), hours(48)));
        assert!(!update_due(false, Some(start), hours(100)));
        let notice = update_notice();
        assert_eq!(notice.title, "Restart to finish updating");
        assert_eq!(
            notice.actions,
            [(RESTART_ACTION.to_owned(), "Restart".to_owned())]
        );
    }

    /// The two days between requests survive a restart of the session.
    #[test]
    fn the_last_request_survives_a_restart_of_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tilo/update-restart-asked");
        assert_eq!(last_asked_at(&path), None);
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        record_asked_at(&path, at);
        assert_eq!(last_asked_at(&path), Some(at));
    }

    /// The compositor's hook marks the loop's turns as the watchdog counts
    /// them, so a loop that waits for events is never judged.
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
}
