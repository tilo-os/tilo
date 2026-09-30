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
    let (dir, name) = tilo_files::file_at(path).ok()?;
    let bytes = tilo_files::blocking::read(&dir, &name).ok()?;
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
    let (dir, name) = tilo_files::file_at(path).map_err(io::Error::other)?;
    tilo_files::blocking::write_atomic(&dir, &name, &doc.save()).map_err(io::Error::other)
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
    // Without a Downloads folder, the session's scratch directory; a page has
    // neither, and its file operations on the relative path simply fail.
    ui::sysdirs::UserDirs::from_system()
        .downloads
        .or_else(|| {
            tilo_paths::AppPaths::for_app("so.tilo.calendar")
                .runtime()
                .ok()
                .and_then(|location| location.native_path().ok().map(PathBuf::from))
        })
        .unwrap_or_default()
}

/// The most-recently-modified `.ics` file in `dir`, if any. Backs the
/// "Import" action so it pulls in the calendar file the user just
/// downloaded rather than a bundled sample.
pub fn newest_ics(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(ui::time::SystemTime, PathBuf)> = None;
    let folder = tilo_files::Dir::open(tilo_files::Location::Native(dir.to_path_buf())).ok()?;
    let root = tilo_files::VPath::root();
    for entry in tilo_files::blocking::list(&folder, &root).ok()? {
        let path = dir.join(&entry.name);
        let is_ics = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("ics"));
        if !is_ics || entry.kind != tilo_files::Kind::File {
            continue;
        }
        let Ok(name) = tilo_files::VPath::new(&entry.name) else {
            continue;
        };
        let Ok(metadata) = tilo_files::blocking::metadata(&folder, &name) else {
            continue;
        };
        let modified = metadata.modified;
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
