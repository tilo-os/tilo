//! On-disk persistence for the calendar's CRDT document, plus the
//! filesystem locations the import/export actions use.
//!
//! The whole calendar (projects / events / tasks and every item's
//! rich-text notes) lives in one Automerge document. The document's `save()`
//! bytes are written atomically to a single file, so events and notes survive
//! a restart and peers can converge on the same bytes. A fresh machine starts
//! with an empty document.

use std::io;
use std::path::{Path, PathBuf};

use automerge::AutoCommit;

/// Load a previously-saved calendar document. Returns `None` when the file
/// does not exist yet (first launch) or cannot be parsed.
pub fn load(path: &Path) -> Option<AutoCommit> {
    let bytes = std::fs::read(path).ok()?;
    AutoCommit::load(&bytes).ok()
}

/// Write the document to `path` atomically (temp file + rename) so a crash
/// mid-write can never leave a half-written, unloadable file behind.
///
/// # Errors
///
/// Returns an error if `path`'s parent directory cannot be created, or if
/// the atomic write (temp file creation, write, or rename) fails.
pub fn save(path: &Path, doc: &mut AutoCommit) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_file::write(path, &doc.save())
}

/// Directory the import/export actions read from and write to. In place of a
/// file picker, use the
/// user's `Downloads` folder (overridable via `TILO_CALENDAR_IMPORT_DIR`),
/// which is where downloaded `.ics` invites and exported files naturally
/// live.
pub fn transfer_dir() -> PathBuf {
    if let Some(over) = std::env::var_os("TILO_CALENDAR_IMPORT_DIR") {
        return PathBuf::from(over);
    }
    ui::sysdirs::UserDirs::from_system()
        .downloads
        .unwrap_or_else(std::env::temp_dir)
}

/// The most-recently-modified `.ics` file in `dir`, if any. Backs the
/// "Import" action so it pulls in the calendar file the user just
/// downloaded rather than a bundled sample.
pub fn newest_ics(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let is_ics = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("ics"));
        if !is_ics {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(m, _)| modified > *m) {
            best = Some((modified, path));
        }
    }
    best.map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{ObjType, ROOT, ReadDoc};

    fn tmp_dir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("cal-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn save_then_load_round_trips_and_a_missing_file_is_none() {
        let dir = tmp_dir();
        let path = dir.join("calendar.automerge");
        let mut doc = AutoCommit::new();
        doc.put_object(&ROOT, "events", ObjType::Map).unwrap();
        save(&path, &mut doc).unwrap();

        let loaded = load(&path).expect("file present after save");
        assert!(matches!(
            loaded.get(&ROOT, "events"),
            Ok(Some((automerge::Value::Object(ObjType::Map), _)))
        ));
        std::fs::remove_dir_all(&dir).ok();

        // Missing file.
        let dir = tmp_dir();
        assert!(load(&dir.join("nope.automerge")).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn newest_ics_picks_the_latest_calendar_file_and_ignores_directories() {
        // Picks latest; ignores non-.ics files.
        let dir = tmp_dir();
        std::fs::write(dir.join("a.ics"), "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("b.ics"), "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").unwrap();
        std::fs::write(dir.join("c.txt"), "not a calendar").unwrap();
        let picked = newest_ics(&dir).expect("an ics file");
        assert_eq!(picked.file_name().unwrap(), "b.ics");
        std::fs::remove_dir_all(&dir).ok();

        // Ignores directories named like calendar files (fresh dir).
        let dir = tmp_dir();
        let file = dir.join("invite.ICS");
        std::fs::write(&file, "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").unwrap();
        std::fs::create_dir(dir.join("newer.ics")).unwrap();
        assert_eq!(newest_ics(&dir), Some(file));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
