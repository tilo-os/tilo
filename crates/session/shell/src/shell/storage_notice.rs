//! Telling the person when storage runs low.
//!
//! A full disk must never take the desktop down, and it must not take the
//! person by surprise either. A worker thread looks at the free space of the
//! home and system filesystems every so often (one `statvfs` each, no
//! wakeups of the compositor while nothing changes) and queues a notice when
//! a filesystem becomes low or full. The compositor loop drains the queue
//! into the notification centre.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tilo_desktop::notifications::{Notification, NotificationSeverity};

use crate::shell::i18n::{t, t_args};

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;

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

/// The level `room` is at, given the level it was at before. Recovering
/// needs clearly more room than falling, so a filesystem hovering at a limit
/// does not announce itself over and over.
#[must_use]
pub fn level(room: Room, before: Level) -> Level {
    let low_limit = (room.capacity / 20).clamp(2 * GIB, 10 * GIB);
    let full_limit = 512 * MIB;
    let (low, full) = match before {
        Level::Plenty => (low_limit, full_limit),
        // Leaving a level takes half as much room again as entering it.
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
pub fn notice_for(volume: Volume, before: Level, after: Level, room: Room) -> Option<Notification> {
    if after <= before {
        return None;
    }
    let free = format_bytes(room.available);
    let volume = match volume {
        Volume::Home => t("storage-volume-home"),
        Volume::System => t("storage-volume-system"),
    };
    let args = [("free", free.into()), ("volume", volume.into())];
    let (title, body, severity) = match after {
        Level::Plenty => return None,
        Level::Low => (
            t("storage-low-title"),
            t_args("storage-low-body", &args),
            NotificationSeverity::Warning,
        ),
        Level::Full => (
            t("storage-full-title"),
            t_args("storage-full-body", &args),
            NotificationSeverity::Error,
        ),
    };
    Some(
        Notification::new(title, body)
            .with_severity(severity)
            .with_app_name(t("storage-app-name")),
    )
}

fn format_bytes(bytes: u64) -> String {
    ui::format::size(bytes)
}

fn room(path: &Path) -> Option<Room> {
    let stat = rustix::fs::statvfs(path).ok()?;
    Some(Room {
        capacity: stat.f_blocks.saturating_mul(stat.f_frsize),
        available: stat.f_bavail.saturating_mul(stat.f_frsize),
    })
}

/// The filesystems to watch: home, and the system's state directory when it
/// is a different filesystem.
fn volumes() -> Vec<(Volume, PathBuf)> {
    use std::os::unix::fs::MetadataExt as _;
    let mut volumes = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(home) = &home {
        volumes.push((Volume::Home, home.clone()));
    }
    let system = PathBuf::from("/var");
    let same_device = home
        .as_deref()
        .and_then(|home| {
            Some(std::fs::metadata(home).ok()?.dev() == std::fs::metadata(&system).ok()?.dev())
        })
        .unwrap_or(false);
    if !same_device {
        volumes.push((Volume::System, system));
    }
    volumes
}

fn queue() -> &'static Mutex<Vec<Notification>> {
    static QUEUE: OnceLock<Mutex<Vec<Notification>>> = OnceLock::new();
    QUEUE.get_or_init(|| Mutex::new(Vec::new()))
}

/// Start watching, once per process. `wake` brings the compositor loop round
/// to [`drain`] the queue.
pub(crate) fn start(wake: crate::session_runtime::Wake) {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let spawned = std::thread::Builder::new()
            .name("storage-watch".into())
            .spawn(move || watch(&wake));
        if let Err(error) = spawned {
            tracing::warn!(%error, "storage warnings are unavailable");
        }
    });
}

fn watch(wake: &crate::session_runtime::Wake) {
    let volumes = volumes();
    let mut levels = vec![Level::Plenty; volumes.len()];
    loop {
        let mut queued = false;
        for ((volume, path), before) in volumes.iter().zip(levels.iter_mut()) {
            let Some(room) = room(path) else { continue };
            let after = level(room, *before);
            if let Some(notice) = notice_for(*volume, *before, after, room)
                && let Ok(mut queue) = queue().lock()
            {
                queue.push(notice);
                queued = true;
            }
            // Room to write came back: a certificate renewal that could
            // not write tries again.
            if *before == Level::Full && after != Level::Full {
                tilo_identity_host::provisioning::RENEWAL_CAUSES.changed();
            }
            *before = after;
        }
        if queued {
            wake();
        }
        // Low storage is checked more often: it is when it changes fastest.
        let next = if levels.iter().any(|level| *level != Level::Plenty) {
            Duration::from_secs(20)
        } else {
            Duration::from_secs(120)
        };
        std::thread::sleep(next);
    }
}

/// Notices queued since the last call.
pub fn drain() -> Vec<Notification> {
    queue()
        .lock()
        .map(|mut queue| std::mem::take(&mut *queue))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

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
            "recovery needs more room"
        );
        assert_eq!(level(room(44, 4 * 1024), Level::Low), Level::Plenty);
    }

    #[test]
    fn only_getting_worse_is_announced() {
        let full = room(44, 20);
        assert!(notice_for(Volume::System, Level::Plenty, Level::Low, full).is_some());
        assert!(notice_for(Volume::System, Level::Low, Level::Full, full).is_some());
        assert!(notice_for(Volume::System, Level::Full, Level::Full, full).is_none());
        assert!(notice_for(Volume::System, Level::Full, Level::Plenty, full).is_none());
        assert!(notice_for(Volume::Home, Level::Low, Level::Low, full).is_none());
    }

    /// Free space is a size on a drive: SI units, as the toolkit writes
    /// every stored size (2.5 GiB is 2.7 GB, never "2.5 GB").
    #[test]
    fn sizes_read_plainly() {
        assert_eq!(format_bytes(2_684_354_560), "2.7 GB");
        assert_eq!(format_bytes(20 * MIB), "21 MB");
    }
}
