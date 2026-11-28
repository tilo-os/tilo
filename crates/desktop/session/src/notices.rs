//! What the machine's own news tells the person, and where the session reads it.
//! The desktop knows things the person should: storage and battery running out, the media codecs
//! they asked for, a session or computer that stopped and started again, an update waiting for a
//! restart, a deployment it went back from, graphics that fell back to software, a drive ready to
//! open, and the state of their own workspaces, agents and apps it could not read. Each is one
//! [`Notice`], shown by the notifications this host serves like any app's, and nothing here draws.
//! The sources stay here: storage is one `statvfs` per filesystem every two minutes (twenty
//! seconds while one is low), codecs are a watch on the helper's status file, and the rest is read
//! once per session pass.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use settings_store::system_sounds::SystemSoundEvent;
use tilo_files::native::ChangeWatch;
use tilo_notifications_service::Notifications;
use tilo_system_service::update;
use tilo_ui_base::format::Date;

use crate::drives::Medium;
use crate::i18n::{t, t_args};
use crate::notifications::tell_daemon;
pub(crate) use crate::platform::notice::Notice;

/// The action the update notice offers: restart the machine into the staged update.
pub const RESTART_ACTION: &str = update::RESTART_TO_UPDATE;

/// The action a plugged-in drive's notice offers: open it in Files on its mount point.
pub const OPEN_MEDIUM_ACTION: &str = "open-medium";

/// The action a plugged-in drive's notice offers: eject it, so it can be pulled out.
pub const EJECT_MEDIUM_ACTION: &str = "eject-medium";

/// How often storage is looked at, and how often while one filesystem is low.
const STORAGE_EVERY: Duration = Duration::from_secs(120);
const STORAGE_LOW_EVERY: Duration = Duration::from_secs(20);

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;

/// How far a filesystem has run out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Plenty,
    Low,
    Full,
}

/// Free space a filesystem has, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FreeSpace {
    pub capacity: u64,
    pub available: u64,
}

/// The level `space` is at, given the one before. Recovering needs half as much room again as falling
/// takes, so a filesystem hovering at a limit does not announce itself over and over.
#[must_use]
pub fn level(space: FreeSpace, before: Level) -> Level {
    let low_limit = (space.capacity / 20).clamp(2 * GIB, 10 * GIB);
    let full_limit = 512 * MIB;
    let (low, full) = match before {
        Level::Plenty => (low_limit, full_limit),
        Level::Low | Level::Full => (
            low_limit.saturating_add(low_limit / 2),
            full_limit.saturating_add(full_limit / 2),
        ),
    };
    if space.available < full {
        Level::Full
    } else if space.available < low {
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

/// The notice for a filesystem moving from `before` to `after`: only getting worse is announced.
#[must_use]
pub fn storage_notice(
    volume: Volume,
    before: Level,
    after: Level,
    space: FreeSpace,
) -> Option<Notice> {
    if after <= before {
        return None;
    }
    let free = tilo_ui_base::format::size(space.available);
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

/// The notice that the battery is running out, saying so when the policy will sleep the machine.
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
    let notice = Notice::new(t("battery-app-name"), urgency, title, body)
        // The critical warning stays until the person lets it go; the heads-up leaves on the
        // person's own notification timing.
        .staying(critical.then_some(Duration::ZERO))
        .sounding(SystemSoundEvent::LowBattery);
    if critical { notice } else { notice.here() }
}

/// The notice for the medium a person just plugged in, offering to open or eject it.
#[must_use]
pub fn medium_notice(medium: &Medium) -> Notice {
    let name = medium.label.clone().unwrap_or_else(|| t("drives-unnamed"));
    let title = t_args(
        "drives-medium-title",
        &[
            ("name", name.into()),
            ("size", tilo_ui_base::format::size(medium.size).into()),
        ],
    );
    Notice::new(t("drives-app-name"), 1, title, t("drives-body")).choosing(vec![
        (OPEN_MEDIUM_ACTION.to_owned(), t("drives-open")),
        (EJECT_MEDIUM_ACTION.to_owned(), t("drives-eject")),
    ])
}

/// Tell the person what they plugged in: the notification's id is how their answer is found.
#[must_use]
pub fn announce_medium(notifications: &Notifications, medium: &Medium) -> u32 {
    tell_daemon(&medium_notice(medium), notifications)
}

/// The notice for how a wake of `device`, one of the person's devices, stands; `None` while it is
/// still going.
#[must_use]
pub fn wake_notice(
    device: &str,
    state: &tilo_workspace_service::workspaces::person_workspace::wake::WakeState,
) -> Option<Notice> {
    use tilo_workspace_service::workspaces::person_workspace::wake::WakeState;
    let (title, body) = match state {
        WakeState::Asked | WakeState::Sent { .. } => return None,
        WakeState::Awake => (
            t_args("device-wake-rejoined", &[("device", device.into())]),
            String::new(),
        ),
        WakeState::NotAwake => (
            t_args("device-wake-not-rejoined", &[("device", device.into())]),
            t("device-wake-not-rejoined-body"),
        ),
        WakeState::Unsent => (
            t_args("device-wake-failed", &[("device", device.into())]),
            t("device-wake-failed-body"),
        ),
    };
    Some(Notice::new(t("device-wake-app-name"), 1, title, body))
}

/// Tell the person the style they chose did not load or failed, as `shell.md` §6 has it: the
/// shell draws the built-in strips while their choice stays as it was.
pub(crate) fn style_failed_notice(style: &str, reason: &str) -> Notice {
    // What the style's own view said is for whoever fixes it: the person is told what the shell
    // does instead, and that their choice is kept.
    tracing::warn!(%style, %reason, "the shell style the person chose did not load");
    Notice::new(
        t("style-failed-app-name"),
        2,
        t("style-failed-title"),
        t_args("style-failed-body", &[("style", style.to_owned().into())]),
    )
    .here()
    .staying(Some(Duration::from_secs(30)))
}

/// Tell the person a press of theirs did not run, saying what the device's dispatch refused: a
/// press made before the services answered it waits for them, and one they refuse is said here.
pub(crate) fn press_refused_notice(reason: &str) -> Notice {
    Notice::new(
        t("press-refused-app-name"),
        1,
        t("press-refused-title"),
        t_args(
            "press-refused-body",
            &[("reason", reason.to_owned().into())],
        ),
    )
    .here()
    .staying(Some(Duration::from_secs(30)))
}

/// What a paste of files has to tell the person, or `None` when it went through.
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
fn free_space(path: &Path) -> Option<FreeSpace> {
    let stat = rustix::fs::statvfs(path).ok()?;
    Some(FreeSpace {
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

/// One look at the watched filesystems; `tell` gets the notices that are due. Returns whether any
/// is low or full, which decides how soon to look again.
pub fn sweep_storage(
    watched: &[(Volume, Option<FreeSpace>)],
    levels: &mut [Level],
    mut tell: impl FnMut(Notice),
) -> bool {
    let mut low = false;
    for ((volume, space), before) in watched.iter().zip(levels.iter_mut()) {
        let Some(space) = space else { continue };
        let after = level(*space, *before);
        if let Some(notice) = storage_notice(*volume, *before, after, *space) {
            tell(notice);
        }
        *before = after;
        low |= after != Level::Plenty;
    }
    low
}

/// The codecs notice for the status moving from `before` to `after`: the install finishing (with or
/// without the hardware part), the hardware part arriving later, and the install failing.
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

/// The notice for `news`, a session or computer that stopped and started again; `today` decides
/// whether it names the time of day or the date too.
#[must_use]
pub fn news_notice(news: &tilo_crash_reports::News, today: Date) -> Notice {
    let time = if news.at.date() == today {
        tilo_ui_base::format::time_of(&news.at)
    } else {
        tilo_ui_base::format::timestamp(&news.at)
    };
    let (urgency, title, body) = match news.ended {
        tilo_crash_reports::Ended::Froze => (
            1,
            t("crash-notice-title"),
            t_args("crash-notice-froze-body", &[("time", time.into())]),
        ),
        tilo_crash_reports::Ended::Crashed => (
            1,
            t("crash-notice-title"),
            t_args("crash-notice-crashed-body", &[("time", time.into())]),
        ),
        tilo_crash_reports::Ended::Stopped => (
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

/// Whether the person was already told this machine's graphics stopped.
static GRAPHICS_TOLD: AtomicBool = AtomicBool::new(false);

/// Tell the person, once, that the graphics stopped and everything keeps working, only slower: each
/// pass reads one record (`tilo_ui_base::gpu::recover`) and says nothing until it is set.
pub fn tell_graphics_fallback(notifications: &Notifications) {
    if TELLING.get().is_none() || GRAPHICS_TOLD.load(Ordering::Relaxed) {
        return;
    }
    if tilo_ui_base::gpu::software_fallback().is_none() {
        return;
    }
    GRAPHICS_TOLD.store(true, Ordering::Relaxed);
    tell_daemon(&graphics_notice(), notifications);
}

/// The notice that an update waits for a restart of the machine, offering the action that takes it:
/// raised by the update's own clock ([`crate::update_finish`]), at most every two days.
pub(crate) fn update_notice() -> Notice {
    Notice::new(
        t("system-update-app-name"),
        1,
        t("system-update-restart-title"),
        t("system-update-restart-body"),
    )
    .choosing(vec![(RESTART_ACTION.to_owned(), t("power-restart"))])
    // It waits in the notification centre; the banner goes after the person has had time to
    // read it.
    .staying(Some(Duration::from_secs(30)))
}

/// The reminder that the person's identity is not backed up yet, offering the backup and a way to
/// stop being reminded: raised by the reminder's own clock ([`crate::backup_reminder`]), every few
/// days after they left the backup for later.
pub(crate) fn backup_notice() -> Notice {
    Notice::new(
        t("identity-backup-app-name"),
        1,
        t("identity-backup-reminder-title"),
        t("identity-backup-reminder-body"),
    )
    .choosing(vec![
        (
            crate::backup_reminder::BACK_UP_ACTION.to_owned(),
            t("identity-backup-reminder-now"),
        ),
        (
            crate::backup_reminder::STOP_ACTION.to_owned(),
            t("identity-backup-reminder-off"),
        ),
    ])
    .staying(Some(Duration::from_secs(30)))
}

/// The notice that the backup the reminder offered did not open, saying why.
pub(crate) fn backup_failed_notice(why: &str) -> Notice {
    Notice::new(
        t("identity-backup-app-name"),
        1,
        t("identity-backup-failed-title"),
        why.to_owned(),
    )
}

/// The notice that the update state the services published does not read back: this machine acts
/// on nothing it could not read, and says so once.
pub(crate) fn update_state_notice() -> Notice {
    Notice::new(
        t("system-update-app-name"),
        1,
        t("update-state-unreadable-title"),
        t("update-state-unreadable-body"),
    )
}

/// The notice that the staged update could not be applied while the computer ran and was taken
/// back, offering the restart that completes it. It stays until the person dismisses it.
pub(crate) fn live_failed_notice() -> Notice {
    Notice::new(
        t("system-update-app-name"),
        2,
        t("system-update-live-failed-title"),
        t("system-update-live-failed-body"),
    )
    .choosing(vec![(RESTART_ACTION.to_owned(), t("power-restart"))])
}

/// The notice of a deployment this machine went back from, and it stays until the person dismisses
/// it: a computer that went back from an update is not news to lose.
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
    Notice::new(
        t("system-update-app-name"),
        2,
        t("system-update-went-back-title"),
        t_args("system-update-went-back-body", &args),
    )
}

/// The notice that a workspace this session made is not on this computer's workspace service yet:
/// founding it there is asked for again, and until it lands the workspace exists only here.
pub(crate) fn workspace_not_founded_notice(name: &str, reason: &str) -> Notice {
    Notice::new(
        t("workspaces-app-name"),
        1,
        t_args("workspace-not-founded-title", &[("name", name.into())]),
        t_args("workspace-not-founded-body", &[("reason", reason.into())]),
    )
}

/// The notice that the "Always allow" answers kept for the agents at this API do not read back;
/// `agent` is the one it was given to, where the kept file still says.
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
    Notice::new(t("agent-bindings-app-name"), 1, title, body)
}

/// The notice that the registry of the programs the person downloaded does not read back: none of
/// their apps is listed until it does.
pub(crate) fn downloaded_apps_notice() -> Notice {
    Notice::new(
        t("downloaded-apps-app-name"),
        1,
        t("downloaded-apps-unreadable-title"),
        t("downloaded-apps-unreadable-body"),
    )
}

/// The notice that a setting the person changed was not left where this computer's own programs
/// read it: `reason` says why, and they may still act on what was set before.
pub(crate) fn settings_not_saved_notice(reason: &str) -> Notice {
    Notice::new(
        t("settings-app-name"),
        2,
        t("settings-not-saved-title"),
        t_args("settings-not-saved-body", &[("reason", reason.into())]),
    )
}

/// The notice that this session has no collaboration: what the person does here stays on this
/// computer, and `reason` says why their other devices and the people they work with see none of
/// it.
pub(crate) fn collab_unavailable_notice(reason: &str) -> Notice {
    Notice::new(
        t("collab-app-name"),
        2,
        t("collab-unavailable-title"),
        t_args("collab-unavailable-body", &[("reason", reason.into())]),
    )
}

/// Why a device or a workspace did not take a change, in the person's own words.
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
/// nothing, so the change stays on the devices that took it.
pub(crate) fn refused_write_notice(device: &str, reason: neighbour_wire::RefusalReason) -> Notice {
    Notice::new(
        t("refused-write-app"),
        2,
        t_args("refused-write-title", &[("device", device.into())]),
        t_args(
            "refused-write-body",
            &[("reason", reason_in_words(reason).into())],
        ),
    )
}

/// The notice that the workspace did not take what the person changed on its shared shell.
/// `workspace` is the name the person reads for it. A shell stores no name for a workspace nobody
/// named, and a workspace that left the shell since has no row to read one from (`None`): the title
/// says which of the two it is, and never opens with a name nobody gave.
pub(crate) fn shell_not_shared_notice(
    workspace: Option<&str>,
    reason: neighbour_wire::RefusalReason,
) -> Notice {
    let title = match workspace.map(str::trim) {
        Some("") => t("shell-not-shared-title-unnamed"),
        Some(name) => t_args("shell-not-shared-title", &[("workspace", name.into())]),
        None => t("shell-not-shared-title-gone"),
    };
    Notice::new(
        t("shell-not-shared-app"),
        2,
        title,
        t_args(
            "shell-not-shared-body",
            &[("reason", reason_in_words(reason).into())],
        ),
    )
}

/// Tell the person what the workspace did not take of their shared shell, naming it and why.
pub(crate) fn tell_shell_not_taken(
    notifications: &std::cell::RefCell<crate::notifications::SessionNotifications>,
    tree: &tilo_desktop_model::Layout,
    (id, reason): (
        tilo_desktop_model::WorkspaceId,
        neighbour_wire::RefusalReason,
    ),
) {
    let workspace = tree
        .workspaces
        .iter()
        .find(|workspace| workspace.id() == id)
        .map(|workspace| workspace.name.as_str());
    notifications
        .borrow()
        .tell(&shell_not_shared_notice(workspace, reason));
}

/// Tell the person what the crash records in `dir` say happened before this start.
pub fn tell_crash_news(notifications: &Notifications, dir: &Path) {
    let Some(news) = tilo_crash_reports::take_news(dir) else {
        return;
    };
    let today = tilo_ui_base::clock::local_now().date();
    tell_daemon(&news_notice(&news, today), notifications);
}

/// The codecs status last seen by the watch.
static CODECS: Mutex<Option<os_actions::codec_pack::CodecPackStatus>> = Mutex::new(None);
/// The watch itself, held for the life of the process.
static CODECS_WATCH: Mutex<Option<ChangeWatch>> = Mutex::new(None);

/// Follow the codecs helper's status file, once per process.
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
            tell_daemon(&notice, &held);
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
            let Some(dir) = tilo_crash_reports::directory() else {
                return;
            };
            // What the kernel kept of the computer stopping becomes a record first, so it is news
            // like the session's own.
            tilo_crash_reports::stopped::take_in(&dir);
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
        let spaces: Vec<(Volume, Option<FreeSpace>)> = watched
            .iter()
            .map(|(volume, path)| (*volume, free_space(path)))
            .collect();
        let low = sweep_storage(&spaces, &mut levels, |notice| {
            tell_daemon(&notice, notifications);
        });
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
    use tilo_notifications_service::Taken;
    use tilo_service::Service;

    fn space(capacity_gib: u64, available_mib: u64) -> FreeSpace {
        FreeSpace {
            capacity: capacity_gib.saturating_mul(GIB),
            available: available_mib.saturating_mul(MIB),
        }
    }

    tilo_test_env::scenario! {
        fn storage_and_a_battery_running_out_are_announced_only_as_they_get_worse;

        phase levels_follow_the_room_left;
        {
            assert_eq!(level(space(500, 100 * 1024), Level::Plenty), Level::Plenty);
            // 5 % of 500 GB is capped at 10 GB.
            assert_eq!(level(space(500, 9 * 1024), Level::Plenty), Level::Low);
            // A small disk still warns with 2 GB left.
            assert_eq!(level(space(20, 1500), Level::Plenty), Level::Low);
            assert_eq!(level(space(44, 20), Level::Plenty), Level::Full);
        }

        phase hovering_at_a_limit_does_not_flap;
        {
            let at_limit = space(44, 2300);
            assert_eq!(level(at_limit, Level::Plenty), Level::Plenty);
            assert_eq!(
                level(at_limit, Level::Low),
                Level::Low,
                "recovery needs more room than falling"
            );
            assert_eq!(level(space(44, 4 * 1024), Level::Low), Level::Plenty);
        }

        phase only_getting_worse_is_announced;
        {
            let full = space(44, 20);
            assert!(storage_notice(Volume::System, Level::Plenty, Level::Low, full).is_some());
            assert!(storage_notice(Volume::System, Level::Low, Level::Full, full).is_some());
            assert!(storage_notice(Volume::System, Level::Full, Level::Full, full).is_none());
            assert!(storage_notice(Volume::System, Level::Full, Level::Plenty, full).is_none());
            assert!(storage_notice(Volume::Home, Level::Low, Level::Low, full).is_none());
        }

        phase a_battery_running_out_reaches_the_person_as_a_notification;
        // The battery running out reaches the person as an ordinary notification: an early heads-up,
        // then one that names what happens if the machine sleeps for it and stays until they act.
        {
            let notifications = Notifications::new();
            tell_daemon(&battery_notice(15, false, false), &notifications);
            tell_daemon(&battery_notice(5, true, true), &notifications);
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

        phase a_filesystem_running_out_reaches_the_person_as_a_notification;
        // A full filesystem reaches the person as an ordinary notification: it is published as a
        // resource like any app's, reads as its copy, and the person closes it through the resource's
        // own action.
        {
            let notifications = Notifications::new();
            let mut levels = [Level::Plenty];
            let watched = [(Volume::Home, Some(space(44, 300)))];
            let low = sweep_storage(&watched, &mut levels, |notice| {
                tell_daemon(&notice, &notifications);
            });
            assert!(low);
            assert_eq!(levels, [Level::Full]);

            let live = notifications.live();
            assert_eq!(live.len(), 1);
            assert_eq!(live[0].summary, "Storage is full");
            assert!(live[0].body.starts_with("Only "), "{}", live[0].body);
            assert!(live[0].machine_wide, "the whole machine's news");
            assert_eq!(live[0].app_name, "Storage");

            sweep_storage(&watched, &mut levels, |_| {
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

        phase an_unreadable_filesystem_is_quiet;
        // A filesystem that cannot be read says nothing, and keeps the level it had for when it is
        // back.
        {
            let mut levels = [Level::Low];
            let low = sweep_storage(&[(Volume::System, None)], &mut levels, |_| {
                panic!("nothing to say")
            });
            assert!(!low);
            assert_eq!(levels, [Level::Low]);
        }
    }

    /// A medium a person plugs in is announced by what it is called and how much it holds, and
    /// ejecting it from the notice reaches the eject.
    #[test]
    fn a_plugged_in_drive_is_announced_with_its_contents() {
        use crate::drives::{Drives, Media, Medium};

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

    fn status(json: &str) -> CodecPackStatus {
        CodecPackStatus::parse(json)
    }

    tilo_test_env::scenario! {
        fn a_codec_install_is_announced_by_how_it_ended;

        phase a_deferred_install_that_finishes_online_is_announced;
        {
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

        phase a_software_only_install_says_no_restart_and_that_hardware_follows;
        {
            let notice = codec_notice(
                &status(r#"{"state":"installing"}"#),
                &status(r#"{"state":"installed","parts":{"tilo-codecs-mesa":"waiting"}}"#),
            )
            .expect("the install finished");
            assert!(notice.body.contains("No restart is needed"));
            assert!(notice.body.contains("Hardware H.264/HEVC follows"));
        }

        phase a_later_matching_mesa_build_is_announced_once;
        {
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

        phase failures_point_to_settings_and_progress_is_quiet;
        {
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
    }

    /// What the person is told when a setting they changed could not be left for their other
    /// devices, and when this session has no collaboration: what happened, and the reason it was
    /// given.
    #[test]
    fn a_failed_setting_and_a_session_without_collaboration_say_what_and_why() {
        let setting = settings_not_saved_notice("the file is read-only");
        assert_eq!(setting.title, "Setting not saved");
        assert!(
            setting.body.contains("the file is read-only"),
            "{}",
            setting.body
        );
        assert_eq!(setting.urgency, 2);
        let collab = collab_unavailable_notice("this desktop was not started by the daemon");
        assert_eq!(collab.title, "Collaboration unavailable");
        assert!(
            collab.body.contains("not started by the daemon"),
            "{}",
            collab.body
        );
        assert_eq!(collab.urgency, 2);
    }

    #[test]
    fn the_notice_of_a_session_that_restarted_says_when() {
        let dir = tempfile::tempdir().unwrap();
        tilo_files::native::private::directory(dir.path()).unwrap();
        let notifications = Notifications::new();
        // The first look marks where the history ends: what is already there is not news.
        tell_crash_news(&notifications, dir.path());
        assert!(notifications.live().is_empty());

        tilo_crash_reports::write_report(
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
            tilo_ui_base::format::DateTime::strptime("%Y-%m-%d %H:%M:%S", "2026-10-24 22:52:01")
                .unwrap();
        assert!(
            live[0].body.contains(&tilo_ui_base::format::timestamp(&at)),
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
        tilo_ui_base::gpu::reset_software_fallback_for_test();
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
            tilo_ui_base::gpu::recover(&adapter, false, "Device is lost"),
            tilo_ui_base::gpu::Recovery::Software,
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

        tilo_ui_base::gpu::reset_software_fallback_for_test();
    }

    tilo_test_env::scenario! {
        fn what_happened_to_this_computer_is_a_notice_of_its_own;

        phase a_computer_that_stopped_is_its_own_notice;
        {
            let news = tilo_crash_reports::News {
                ended: tilo_crash_reports::Ended::Stopped,
                at: tilo_ui_base::format::DateTime::strptime("%Y-%m-%d %H:%M:%S", "2026-10-24 22:52:01")
                .unwrap(),
            };
            let notice = news_notice(&news, news.at.date().tomorrow().unwrap());
            assert_eq!(notice.title, "The computer restarted");
            assert!(notice.body.contains("stays on this computer"));
        }

        phase what_this_computer_could_not_read_reaches_the_person;
        // What this computer could not read of its own update reaches the person as an ordinary
        // notification: what did not read, and what is still true for them.
        {
            let notifications = Notifications::new();
            tell_daemon(&update_state_notice(), &notifications);

            let live = notifications.live();
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
    }

    tilo_test_env::scenario! {
        fn the_update_notice_offers_and_carries_the_restart;

        phase the_update_notice_offers_the_restart_of_the_machine;
        {
            let notice = update_notice();
            assert_eq!(notice.title, "Restart to finish updating");
            assert_eq!(
                notice.actions,
                [(RESTART_ACTION.to_owned(), "Restart".to_owned())]
            );
        }

        phase the_update_notice_carries_the_restart_action;
        // Acting on the update notice reaches the tap the session installed,
        // through the notification's own published action.
        {
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
            assert_eq!(notice.title, "Restart to finish updating");
            assert_eq!(
                notice.actions,
                [(RESTART_ACTION.to_owned(), "Restart".to_owned())]
            );
            let resource =
                tilo_notifications_service::notification_resource(tell_daemon(&notice, &notifications));
            let id = notifications.live()[0].id;
            let mut arguments = serde_json::Map::new();
            arguments.insert("action".into(), RESTART_ACTION.into());
            let mut published = notifications;
            published
                .invoke(&resource, "invoke", &arguments)
                .expect("the action is offered");
            assert_eq!(*tapped.lock().unwrap(), [(id, RESTART_ACTION.to_owned())]);
        }
    }

    /// The notice that a workspace did not take the window layout names the workspace by
    /// the name the person gave it. One nobody named, and one that left the shell since, are said
    /// to be that: the title never opens with an empty name, or with a word in place of one.
    #[test]
    fn a_shell_the_workspace_did_not_take_names_the_workspace_or_says_it_has_no_name() {
        let title = |workspace| {
            shell_not_shared_notice(workspace, neighbour_wire::RefusalReason::Forbidden).title
        };
        let words = || [Some("Research"), Some(""), Some("  "), None].map(title);
        let english = words();
        tilo_ui_base::i18n::I18nManager::set_locale("de-DE");
        let german = words();
        tilo_ui_base::i18n::I18nManager::set_locale("en-US");
        assert_eq!(
            english,
            [
                "Research did not take your window layout",
                "A workspace with no name did not take your window layout",
                "A workspace with no name did not take your window layout",
                "A workspace this device is no longer in did not take your window layout",
            ]
        );
        assert_eq!(
            german,
            [
                "Research hat dein Layout nicht übernommen",
                "Ein Arbeitsbereich ohne Namen hat dein Layout nicht übernommen",
                "Ein Arbeitsbereich ohne Namen hat dein Layout nicht übernommen",
                "Ein Arbeitsbereich, in dem dieses Gerät nicht mehr ist, hat dein Layout nicht \
                 übernommen",
            ]
        );
        for title in english.iter().chain(&german) {
            assert!(
                !title.starts_with(' ')
                    && !title.starts_with("Workspace ")
                    && !title.starts_with("Arbeitsbereich "),
                "{title:?} names no workspace"
            );
        }
    }
}
