//! Telling the person, once, that the desktop stopped and started again.
//!
//! A desktop that panicked or stopped responding ends its session: the
//! person is back at the login screen with no word of why. Both leave a
//! record in the crash directory (`crate::crash_log`): a panic its report, a
//! main loop the watchdog judged hung its freeze record. At the next start a
//! worker thread looks for records written since the last look and queues one
//! notice for the newest that ended the desktop; the compositor loop drains
//! it into the notification centre. The records stay on this computer.
//!
//! What has been looked at is a marker file beside the records naming the
//! newest one. Without a marker (the first start that knows of it) the
//! records already there are history, not news: the marker is written and
//! nothing is said. A notice is only queued once the marker moved past its
//! record, so a directory that cannot be written is silent rather than
//! repeating itself at every start.

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use chrono::NaiveDateTime;
use tilo_desktop::notifications::{Notification, NotificationSeverity};
use tilo_lock::MutexRecover as _;

use crate::crash_log::{FILE_TIMESTAMP, FREEZE_SUFFIX};
use crate::shell::i18n::{t, t_args};

/// The marker naming the newest record already looked at.
pub const NOTICED: &str = ".noticed";

/// How the desktop ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// Its main loop stopped responding and the watchdog ended it.
    Froze,
    /// It panicked outside a boundary that restarts the failing part.
    Crashed,
    /// The whole computer stopped and started again: the kernel's record
    /// from before the reset (`crate::shell::crash_records`).
    Stopped,
}

/// A record that ended the desktop, written since the last look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct News {
    pub ended: Ended,
    /// Local wall-clock time of the record.
    pub at: NaiveDateTime,
}

/// The local time a record's file name starts with.
#[must_use]
pub fn written_at(name: &str) -> Option<NaiveDateTime> {
    let stamp = name.get(..FILE_TIMESTAMP_LEN)?;
    NaiveDateTime::parse_from_str(stamp, FILE_TIMESTAMP).ok()
}

/// `2026-10-24-225201`: how long [`FILE_TIMESTAMP`] renders.
const FILE_TIMESTAMP_LEN: usize = 17;

/// How the record `name` says the desktop ended, if it did. A freeze always
/// ended it; a panic only when no boundary contained it, which its
/// `contained:` line says (read with `body` only for a panic).
#[must_use]
pub fn ended_by(name: &str, body: impl FnOnce() -> Option<String>) -> Option<Ended> {
    if name.ends_with(FREEZE_SUFFIX) {
        return Some(Ended::Froze);
    }
    if name.ends_with(crate::shell::crash_records::STOPPED_SUFFIX) {
        // An oops the system ran on from stopped nothing; a record that
        // cannot be read is still the computer stopping.
        let fault = body()
            .and_then(|body| crate::shell::crash_records::summarize(name, &body))
            .is_some_and(|record| {
                record.kind == tilo_actions::remote::crash_records::CrashKind::SystemFault
            });
        return (!fault).then_some(Ended::Stopped);
    }
    let body = body()?;
    let contained = body
        .lines()
        .find_map(|line| line.strip_prefix("contained:"))?;
    contained
        .trim_start()
        .starts_with("no")
        .then_some(Ended::Crashed)
}

/// Look at the records in `dir` written since the last look, move the marker
/// past them, and say what the newest one that ended the desktop was.
#[must_use]
pub fn take_news(dir: &Path) -> Option<News> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            let is_record = Path::new(&name).extension() == Some("log".as_ref())
                && written_at(&name).is_some()
                && entry.file_type().ok()?.is_file();
            is_record.then_some(name)
        })
        .collect();
    names.sort_unstable();
    let marker = dir.join(NOTICED);
    let newest = names.last().cloned().unwrap_or_default();
    let Ok(noticed) = std::fs::read_to_string(&marker) else {
        let _ = tilo_files::native::private::write_atomic(&marker, newest.as_bytes());
        return None;
    };
    let noticed = noticed.trim();
    let news = names
        .iter()
        .rev()
        .take_while(|name| name.as_str() > noticed)
        .find_map(|name| {
            let ended = ended_by(name, || {
                let mut body = String::new();
                std::io::Read::read_to_string(
                    &mut tilo_files::native::private::read(&dir.join(name)).ok()?,
                    &mut body,
                )
                .ok()?;
                Some(body)
            })?;
            Some(News {
                ended,
                at: written_at(name)?,
            })
        });
    if newest.as_str() > noticed {
        tilo_files::native::private::write_atomic(&marker, newest.as_bytes()).ok()?;
    }
    news
}

/// The notice for `news`. `today` is the local date now: a record from today
/// names the time of day, an older one its date as well.
#[must_use]
pub fn notice_for(news: &News, today: chrono::NaiveDate) -> Notification {
    let time = if news.at.date() == today {
        ui::format::time_of(&news.at)
    } else {
        ui::format::timestamp(&news.at)
    };
    let (title, body) = match news.ended {
        Ended::Froze => ("crash-notice-title", "crash-notice-froze-body"),
        Ended::Crashed => ("crash-notice-title", "crash-notice-crashed-body"),
        Ended::Stopped => ("crash-notice-stopped-title", "crash-notice-stopped-body"),
    };
    Notification::new(t(title), t_args(body, &[("time", time.into())]))
        .with_severity(NotificationSeverity::Warning)
        .with_app_name(t("crash-notice-app-name"))
}

static QUEUE: Mutex<Vec<Notification>> = Mutex::new(Vec::new());

/// Look once, per process, on a thread of its own: the records are files,
/// and the compositor thread walks no path. `wake` brings the compositor
/// loop round to [`drain`] the queue.
pub(crate) fn start(wake: crate::session_runtime::Wake) {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let spawned = std::thread::Builder::new()
            .name("crash-notice".into())
            .spawn(move || {
                let Some(dir) = crate::crash_log::directory() else {
                    return;
                };
                // What the kernel kept of the computer stopping becomes a
                // record first, so it is news like the desktop's own.
                crate::shell::crash_records::take_in(&dir);
                let Some(news) = take_news(&dir) else {
                    return;
                };
                let today = ui::clock::local_now().date_naive();
                QUEUE.lock_recover().push(notice_for(&news, today));
                wake();
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "the notice of a desktop that restarted is unavailable");
        }
    });
}

/// Notices queued since the last call.
pub fn drain() -> Vec<Notification> {
    std::mem::take(&mut *QUEUE.lock_recover())
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNCONTAINED: &str =
        "tilo shell panic\n================\ncontained: no: the session ends\n";
    const CONTAINED: &str = "tilo shell panic\n================\ncontained: yes: the failing helper restarts and the session continues\n";

    fn record(dir: &Path, name: &str, body: &str) {
        crate::crash_log::write_record(dir, name, body).unwrap();
    }

    fn private_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        tilo_files::native::private::directory(dir.path()).unwrap();
        dir
    }

    #[test]
    fn a_record_says_how_the_desktop_ended() {
        let unread = || -> Option<String> { panic!("a freeze record is known by its name") };
        assert_eq!(
            ended_by("2026-10-24-225201-412-freeze.log", unread),
            Some(Ended::Froze)
        );
        assert_eq!(
            ended_by("2026-10-24-225201-412.log", || Some(UNCONTAINED.into())),
            Some(Ended::Crashed)
        );
        assert_eq!(
            ended_by("2026-10-24-225201-412.log", || Some(CONTAINED.into())),
            None,
            "a helper that restarted did not end the desktop"
        );
        assert_eq!(ended_by("2026-10-24-225201-412.log", || None), None);
        assert_eq!(
            written_at("2026-10-24-225201-412-freeze.log"),
            NaiveDateTime::parse_from_str("2026-10-24 22:52:01", "%Y-%m-%d %H:%M:%S").ok()
        );
        assert_eq!(written_at("notes.log"), None);
    }

    #[test]
    fn records_from_before_the_first_look_are_history() {
        let dir = private_dir();
        record(dir.path(), "2026-10-20-080000-7-freeze.log", "old");
        assert_eq!(take_news(dir.path()), None);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(NOTICED)).unwrap(),
            "2026-10-20-080000-7-freeze.log"
        );
    }

    #[test]
    fn the_newest_record_that_ended_the_desktop_is_told_once() {
        let dir = private_dir();
        assert_eq!(
            take_news(dir.path()),
            None,
            "the first look marks the start"
        );

        record(dir.path(), "2026-10-24-210000-5.log", UNCONTAINED);
        record(dir.path(), "2026-10-24-225201-9-freeze.log", "freeze");
        record(dir.path(), "2026-10-24-230000-9.log", CONTAINED);
        let news = take_news(dir.path()).expect("a freeze since the last look");
        assert_eq!(news.ended, Ended::Froze);
        assert_eq!(written_at("2026-10-24-225201-9-freeze.log"), Some(news.at));

        assert_eq!(take_news(dir.path()), None, "told once");
        record(dir.path(), "2026-10-25-090000-11.log", UNCONTAINED);
        assert_eq!(
            take_news(dir.path()).map(|news| news.ended),
            Some(Ended::Crashed)
        );
        record(dir.path(), "2026-10-25-100000-12.log", CONTAINED);
        assert_eq!(take_news(dir.path()), None, "a contained panic is no news");
    }

    #[test]
    fn the_notice_says_when_in_plain_words() {
        let at = NaiveDateTime::parse_from_str("2026-10-24 22:52:01", "%Y-%m-%d %H:%M:%S").unwrap();
        let froze = notice_for(
            &News {
                ended: Ended::Froze,
                at,
            },
            at.date(),
        );
        assert!(!froze.title.is_empty());
        assert!(
            froze.body.contains(&ui::format::time_of(&at)),
            "{}",
            froze.body
        );
        let crashed = notice_for(
            &News {
                ended: Ended::Crashed,
                at,
            },
            at.date().succ_opt().unwrap(),
        );
        assert!(
            crashed.body.contains(&ui::format::timestamp(&at)),
            "{}",
            crashed.body
        );
        assert_ne!(froze.body, crashed.body);
    }
}
