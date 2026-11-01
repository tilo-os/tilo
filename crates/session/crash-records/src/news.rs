//! Telling the person, once, that the session stopped and started again.
//!
//! A session that panicked or stopped responding ends its run: the person is
//! back at the login screen with no word of why. Both leave a record in the
//! crash directory ([`super::directory`]): a panic its report, a main loop the
//! watchdog judged hung its freeze record, and both the kernel's record of the
//! computer stopping. [`take_news`] says what the newest record written since
//! the last look was; the session tells the person once through its
//! notifications.
//!
//! What has been looked at is a marker file beside the records naming the
//! newest one. Without a marker (the first start that knows of it) the
//! records already there are history, not news: the marker is written and
//! nothing is said. A notice is only queued once the marker moved past its
//! record, so a directory that cannot be written is silent rather than
//! repeating itself at every start.

use std::path::Path;

use chrono::NaiveDateTime;
use tilo_actions::remote::crash_records::CrashKind;

use super::records::{is_record_name, read_start, summarize, written_at};

/// The marker naming the newest record already looked at.
pub const NOTICED: &str = ".noticed";

/// How much of a record is read to tell how it ended.
const HEAD_BYTES: u64 = 4 * 1024;

/// How the session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// Its main loop stopped responding and the watchdog ended it.
    Froze,
    /// It panicked: this process has no boundary that restarts a part of it.
    Crashed,
    /// The whole computer stopped and started again: the kernel's record
    /// from before the reset.
    Stopped,
}

impl Ended {
    /// How a record of `kind` ended the session, if it did: a part that
    /// restarted and a fault the system ran on from ended nothing.
    #[must_use]
    const fn of(kind: CrashKind) -> Option<Self> {
        match kind {
            CrashKind::Froze => Some(Self::Froze),
            CrashKind::Crashed => Some(Self::Crashed),
            CrashKind::ComputerStopped => Some(Self::Stopped),
            CrashKind::PartRestarted | CrashKind::SystemFault => None,
        }
    }
}

/// A record that ended the session, written since the last look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct News {
    pub ended: Ended,
    /// Local wall-clock time of the record.
    pub at: NaiveDateTime,
}

/// Look at the records in `dir` written since the last look, move the marker
/// past them, and say what the newest one that ended the session was.
#[must_use]
pub fn take_news(dir: &Path) -> Option<News> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            (is_record_name(&name) && entry.file_type().ok()?.is_file()).then_some(name)
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
            let (head, _) = read_start(&dir.join(name), HEAD_BYTES).ok()?;
            let record = summarize(name, &head)?;
            Some(News {
                ended: Ended::of(record.kind)?,
                at: written_at(name)?,
            })
        });
    if newest.as_str() > noticed {
        tilo_files::native::private::write_atomic(&marker, newest.as_bytes()).ok()?;
    }
    news
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        tilo_files::native::private::directory(dir.path()).unwrap();
        dir
    }

    fn record(dir: &Path, name: &str, body: &str) {
        crate::write_record(dir, name, body).unwrap();
    }

    const UNCONTAINED: &str =
        "tilo session panic\n==================\ncontained: no: the session ends\n";
    const CONTAINED: &str =
        "tilo session panic\n==================\ncontained: yes: the part restarts\n";
    const STOPPED: &str = "tilo computer stopped\n=====================\nreason:    panic\n";

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
    fn the_newest_record_that_ended_the_session_is_told_once() {
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
        assert_eq!(
            news.at,
            NaiveDateTime::parse_from_str("2026-10-24 22:52:01", "%Y-%m-%d %H:%M:%S").unwrap()
        );

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
    fn the_computer_stopping_is_news_and_a_fault_is_not() {
        let dir = private_dir();
        assert_eq!(take_news(dir.path()), None);
        record(dir.path(), "2026-10-24-225201-4f00-stopped.log", STOPPED);
        assert_eq!(
            take_news(dir.path()).map(|news| news.ended),
            Some(Ended::Stopped)
        );
        let fault = STOPPED.replace("panic", "oops");
        record(dir.path(), "2026-10-25-225201-4f00-stopped.log", &fault);
        assert_eq!(take_news(dir.path()), None, "the system ran on");
    }

    #[test]
    fn a_missing_directory_is_no_news() {
        assert_eq!(take_news(Path::new("/nonexistent/tilo/crashes")), None);
    }
}
