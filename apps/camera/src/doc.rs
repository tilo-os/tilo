//! Automerge document model for the Camera app.
//!
//! Layout under the app root object:
//!
//! ```text
//! root
//! ├── "screen"    (Str)   # camera | scanner | gallery | settings
//! ├── "mode"      (Str)   # photo | video  (camera capture sub-mode)
//! ├── "facing"    (Str)   # back | front | wide
//! ├── "settings"  (Map)   # scalar settings (see Settings)
//! ├── "favorites" (Map)   # { <file name>: true } — starred library items
//! └── "scans"     (List)  # [ { raw, ts } ] newest-first scan history
//! ```
//!
//! The photo/video *pixels* are ordinary files on disk (see
//! [`crate::library`]) — the CRDT only holds the collaborative metadata:
//! which items are favorited, the scan history, and capture preferences.

use std::collections::HashSet;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};

use crate::model::{
    CaptureMode, Facing, PhotoFormat, Resolution, ScanEntry, Screen, Settings, Timer,
};

// ---- scalar helpers --------------------------------------------------

fn get_str(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Str(s) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn get_bool(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<bool> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Boolean(b) => Some(*b),
            _ => None,
        },
        _ => None,
    }
}

fn get_int(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<i64> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => v.to_i64(),
        _ => None,
    }
}

fn map_at(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(obj, key) {
        Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
        _ => None,
    }
}

fn list_at(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(obj, key) {
        Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
        _ => None,
    }
}

// ---- screen / mode / facing -----------------------------------------

pub fn read_screen(doc: &AutoCommit, root: &ObjId) -> Screen {
    get_str(doc, root, "screen")
        .and_then(|s| Screen::from_str(&s))
        .unwrap_or(Screen::Camera)
}

pub fn write_screen(doc: &mut AutoCommit, root: &ObjId, screen: Screen) {
    let _ = doc.put(root, "screen", screen.as_str());
}

pub fn read_mode(doc: &AutoCommit, root: &ObjId) -> CaptureMode {
    get_str(doc, root, "mode").map_or(CaptureMode::Photo, |s| CaptureMode::from_str(&s))
}

pub fn write_mode(doc: &mut AutoCommit, root: &ObjId, mode: CaptureMode) {
    let _ = doc.put(root, "mode", mode.as_str());
}

pub fn read_facing(doc: &AutoCommit, root: &ObjId) -> Facing {
    get_str(doc, root, "facing").map_or(Facing::Back, |s| Facing::from_str(&s))
}

pub fn write_facing(doc: &mut AutoCommit, root: &ObjId, facing: Facing) {
    let _ = doc.put(root, "facing", facing.as_str());
}

// ---- settings --------------------------------------------------------

pub fn read_settings(doc: &AutoCommit, root: &ObjId) -> Settings {
    let mut s = Settings::default();
    let Some(m) = map_at(doc, root, "settings") else {
        return s;
    };
    if let Some(v) = get_str(doc, &m, "resolution") {
        s.resolution = Resolution::from_str(&v);
    }
    if let Some(v) = get_str(doc, &m, "photo_format") {
        s.photo_format = PhotoFormat::from_str(&v);
    }
    if let Some(v) = get_int(doc, &m, "timer") {
        s.timer = Timer::from_seconds(v);
    }
    if let Some(v) = get_bool(doc, &m, "grid") {
        s.grid = v;
    }
    if let Some(v) = get_bool(doc, &m, "level") {
        s.level = v;
    }
    if let Some(v) = get_bool(doc, &m, "mirror_front") {
        s.mirror_front = v;
    }
    if let Some(v) = get_bool(doc, &m, "geotag") {
        s.geotag = v;
    }
    if let Some(v) = get_bool(doc, &m, "hdr") {
        s.hdr = v;
    }
    if let Some(v) = get_str(doc, &m, "default_facing") {
        s.default_facing = Facing::from_str(&v);
    }
    s
}

pub fn write_settings(doc: &mut AutoCommit, root: &ObjId, s: &Settings) {
    let m = match map_at(doc, root, "settings") {
        Some(m) => m,
        None => doc
            .put_object(root, "settings", ObjType::Map)
            .expect("settings map"),
    };
    let _ = doc.put(&m, "resolution", s.resolution.as_str());
    let _ = doc.put(&m, "photo_format", s.photo_format.as_str());
    let _ = doc.put(&m, "timer", s.timer.seconds());
    let _ = doc.put(&m, "grid", s.grid);
    let _ = doc.put(&m, "level", s.level);
    let _ = doc.put(&m, "mirror_front", s.mirror_front);
    let _ = doc.put(&m, "geotag", s.geotag);
    let _ = doc.put(&m, "hdr", s.hdr);
    let _ = doc.put(&m, "default_facing", s.default_facing.as_str());
}

// ---- favorites -------------------------------------------------------

/// The set of favorited library item ids (file names).
pub fn read_favorites(doc: &AutoCommit, root: &ObjId) -> HashSet<String> {
    let mut set = HashSet::new();
    let Some(m) = map_at(doc, root, "favorites") else {
        return set;
    };
    for key in doc.keys(&m) {
        if get_bool(doc, &m, &key).unwrap_or(false) {
            set.insert(key);
        }
    }
    set
}

/// Star / unstar a library item by id. Unstarring removes the key so the
/// favorites map does not accumulate tombstoned `false`s.
pub fn set_favorite(doc: &mut AutoCommit, root: &ObjId, id: &str, on: bool) {
    let m = match map_at(doc, root, "favorites") {
        Some(m) => m,
        None => doc
            .put_object(root, "favorites", ObjType::Map)
            .expect("favorites map"),
    };
    if on {
        let _ = doc.put(&m, id, true);
    } else {
        let _ = doc.delete(&m, id);
    }
}

// ---- scan history ----------------------------------------------------

const MAX_SCANS: usize = 24;

/// Read the scan history, newest first.
pub fn read_scans(doc: &AutoCommit, root: &ObjId) -> Vec<ScanEntry> {
    let Some(list) = list_at(doc, root, "scans") else {
        return Vec::new();
    };
    let n = doc.length(&list);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i) else {
            continue;
        };
        let raw = get_str(doc, &m, "raw").unwrap_or_default();
        if raw.is_empty() {
            continue;
        }
        out.push(ScanEntry {
            raw,
            ts: get_int(doc, &m, "ts").unwrap_or(0),
        });
    }
    out
}

/// Record a scanned code at the front of the history. Re-scanning the same
/// payload moves it to the front rather than duplicating; the list is capped.
pub fn record_scan(doc: &mut AutoCommit, root: &ObjId, raw: &str, ts: i64) {
    let list = match list_at(doc, root, "scans") {
        Some(l) => l,
        None => doc
            .put_object(root, "scans", ObjType::List)
            .expect("scans list"),
    };
    // Drop any existing entry with the same payload.
    let n = doc.length(&list);
    for i in (0..n).rev() {
        if let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i)
            && get_str(doc, &m, "raw").as_deref() == Some(raw)
        {
            let _ = doc.delete(&list, i);
        }
    }
    let m = doc
        .insert_object(&list, 0, ObjType::Map)
        .expect("insert scan");
    let _ = doc.put(&m, "raw", raw);
    let _ = doc.put(&m, "ts", ts);
    // Trim the tail beyond the cap.
    while doc.length(&list) > MAX_SCANS {
        let _ = doc.delete(&list, doc.length(&list) - 1);
    }
}

// ---- first-run defaults ----------------------------------------------

/// True once the doc has been initialised by this app.
pub fn is_seeded(doc: &AutoCommit, root: &ObjId) -> bool {
    get_str(doc, root, "screen").is_some()
}

/// Write capture preferences + the initial screen on first run (empty doc).
/// No sample content is fabricated — the library starts empty and reflects
/// whatever is actually on disk. Idempotent: a no-op once initialised.
pub fn init_defaults(doc: &mut AutoCommit, root: &ObjId, initial: Screen) {
    if is_seeded(doc, root) {
        return;
    }
    write_screen(doc, root, initial);
    write_mode(doc, root, CaptureMode::Photo);
    write_facing(doc, root, Facing::Back);
    write_settings(doc, root, &Settings::default());
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ROOT;

    #[test]
    fn settings_round_trip() {
        let mut doc = AutoCommit::new();
        let root = ROOT;
        let mut s = Settings::default();
        s.grid = false;
        s.timer = Timer::S10;
        s.resolution = Resolution::R4K;
        s.hdr = false;
        write_settings(&mut doc, &root, &s);
        let got = read_settings(&doc, &root);
        assert_eq!(got, s);
    }

    #[test]
    fn favorites_star_and_unstar() {
        let mut doc = AutoCommit::new();
        let root = ROOT;
        assert!(read_favorites(&doc, &root).is_empty());
        set_favorite(&mut doc, &root, "IMG_a.png", true);
        set_favorite(&mut doc, &root, "IMG_b.png", true);
        let favs = read_favorites(&doc, &root);
        assert!(favs.contains("IMG_a.png") && favs.contains("IMG_b.png"));
        set_favorite(&mut doc, &root, "IMG_a.png", false);
        let favs = read_favorites(&doc, &root);
        assert!(!favs.contains("IMG_a.png"));
        assert!(favs.contains("IMG_b.png"));
    }

    #[test]
    fn scans_are_newest_first_deduped_and_capped() {
        let mut doc = AutoCommit::new();
        let root = ROOT;
        record_scan(&mut doc, &root, "https://a", 100);
        record_scan(&mut doc, &root, "https://b", 200);
        // Re-scanning "a" moves it to the front, not a duplicate.
        record_scan(&mut doc, &root, "https://a", 300);
        let scans = read_scans(&doc, &root);
        assert_eq!(scans.len(), 2);
        assert_eq!(scans[0].raw, "https://a");
        assert_eq!(scans[0].ts, 300);
        assert_eq!(scans[1].raw, "https://b");
        // Cap is enforced.
        for i in 0..(MAX_SCANS + 10) {
            record_scan(&mut doc, &root, &format!("code-{i}"), i as i64);
        }
        assert!(read_scans(&doc, &root).len() <= MAX_SCANS);
    }

    #[test]
    fn init_defaults_is_idempotent_and_seeds_no_content() {
        let mut doc = AutoCommit::new();
        let root = ROOT;
        assert!(!is_seeded(&doc, &root));
        init_defaults(&mut doc, &root, Screen::Scanner);
        assert!(is_seeded(&doc, &root));
        assert_eq!(read_screen(&doc, &root), Screen::Scanner);
        // No fabricated library content.
        assert!(read_favorites(&doc, &root).is_empty());
        assert!(read_scans(&doc, &root).is_empty());
        // Second init must not change the persisted screen.
        init_defaults(&mut doc, &root, Screen::Camera);
        assert_eq!(read_screen(&doc, &root), Screen::Scanner);
    }
}
