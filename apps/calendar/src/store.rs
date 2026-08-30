//! On-disk persistence for the calendar's CRDT document, plus the
//! filesystem locations the import/export actions use.
//!
//! The whole calendar (projects / events / tasks and every item's
//! rich-text notes) lives in one Automerge document. Persistence is just
//! that document's `save()` bytes written atomically to a single file, so
//! a user's events and notes survive a restart and two peers can still
//! converge on the same bytes. There is **no** sample content here — a
//! fresh machine loads nothing and starts empty.

use std::io;
use std::path::{Path, PathBuf};

use automerge::AutoCommit;

/// Where the calendar document is stored. Honors `QOS_CALENDAR_DATA` (used
/// by tests and by callers who want an explicit location), then
/// `XDG_DATA_HOME`, then `$HOME/.local/share`. Returns `None` only when no
/// home can be resolved, in which case the app simply runs without
/// persistence.
pub fn default_path() -> Option<PathBuf> {
    if let Some(over) = std::env::var_os("QOS_CALENDAR_DATA") {
        return Some(PathBuf::from(over));
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share")))?;
    Some(base.join("qos").join("calendar").join("calendar.automerge"))
}

/// Load a previously-saved calendar document. Returns `None` when the file
/// does not exist yet (first launch) or cannot be parsed.
pub fn load(path: &Path) -> Option<AutoCommit> {
    let bytes = std::fs::read(path).ok()?;
    AutoCommit::load(&bytes).ok()
}

/// Write the document to `path` atomically (temp file + rename) so a crash
/// mid-write can never leave a half-written, unloadable file behind.
pub fn save(path: &Path, doc: &mut AutoCommit) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_file::write(path, &doc.save())
}

/// Directory the import/export actions read from and write to. A real
/// calendar client opens a file picker; with none available here we use the
/// user's `Downloads` folder (overridable via `QOS_CALENDAR_IMPORT_DIR`),
/// which is where downloaded `.ics` invites and exported files naturally
/// live.
pub fn transfer_dir() -> PathBuf {
    if let Some(over) = std::env::var_os("QOS_CALENDAR_IMPORT_DIR") {
        return PathBuf::from(over);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return Path::new(&home).join("Downloads");
    }
    std::env::temp_dir()
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
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
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
    fn save_then_load_round_trips_document() {
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
    }

    #[test]
    fn load_missing_file_is_none() {
        let dir = tmp_dir();
        assert!(load(&dir.join("nope.automerge")).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn newest_ics_picks_latest() {
        let dir = tmp_dir();
        std::fs::write(dir.join("a.ics"), "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("b.ics"), "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").unwrap();
        std::fs::write(dir.join("c.txt"), "not a calendar").unwrap();
        let picked = newest_ics(&dir).expect("an ics file");
        assert_eq!(picked.file_name().unwrap(), "b.ics");
        std::fs::remove_dir_all(&dir).ok();
    }
}
