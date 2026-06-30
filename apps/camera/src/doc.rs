//! Automerge document model for the Camera app.
//!
//! Layout under the app root object:
//!
//! ```text
//! root
//! ├── "screen"   (Str)   # camera | scanner | gallery | settings
//! ├── "mode"     (Str)   # photo | video  (camera capture sub-mode)
//! ├── "facing"   (Str)   # back | front | wide
//! ├── "settings" (Map)   # scalar settings (see Settings)
//! └── "captures" (List)  # [ { id, kind, ts, w, h, dur_ms, label, favorite, facing, scene } ]
//! ```
//!
//! All state the UI persists / collaborates on lives here. The widget keeps
//! only render caches. Mirrors the notes app's `doc.rs` pattern.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};

use crate::model::{
    Capture, CaptureKind, Facing, CaptureMode, PhotoFormat, Resolution, Screen, Settings, Timer,
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
    get_str(doc, root, "screen").and_then(|s| Screen::from_str(&s)).unwrap_or(Screen::Camera)
}

pub fn write_screen(doc: &mut AutoCommit, root: &ObjId, screen: Screen) {
    let _ = doc.put(root, "screen", screen.as_str());
}

pub fn read_mode(doc: &AutoCommit, root: &ObjId) -> CaptureMode {
    get_str(doc, root, "mode").map(|s| CaptureMode::from_str(&s)).unwrap_or(CaptureMode::Photo)
}

pub fn write_mode(doc: &mut AutoCommit, root: &ObjId, mode: CaptureMode) {
    let _ = doc.put(root, "mode", mode.as_str());
}

pub fn read_facing(doc: &AutoCommit, root: &ObjId) -> Facing {
    get_str(doc, root, "facing").map(|s| Facing::from_str(&s)).unwrap_or(Facing::Back)
}

pub fn write_facing(doc: &mut AutoCommit, root: &ObjId, facing: Facing) {
    let _ = doc.put(root, "facing", facing.as_str());
}

// ---- settings --------------------------------------------------------

pub fn read_settings(doc: &AutoCommit, root: &ObjId) -> Settings {
    let mut s = Settings::default();
    let Some(m) = map_at(doc, root, "settings") else { return s };
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
        None => doc.put_object(root, "settings", ObjType::Map).expect("settings map"),
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

// ---- captures --------------------------------------------------------

fn captures_list(doc: &mut AutoCommit, root: &ObjId) -> ObjId {
    match list_at(doc, root, "captures") {
        Some(l) => l,
        None => doc.put_object(root, "captures", ObjType::List).expect("captures list"),
    }
}

pub fn read_captures(doc: &AutoCommit, root: &ObjId) -> Vec<Capture> {
    let Some(list) = list_at(doc, root, "captures") else { return Vec::new() };
    let n = doc.length(&list);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i) else { continue };
        let id = get_str(doc, &m, "id").unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        out.push(Capture {
            id,
            kind: CaptureKind::from_str(&get_str(doc, &m, "kind").unwrap_or_default()),
            ts: get_int(doc, &m, "ts").unwrap_or(0),
            w: get_int(doc, &m, "w").unwrap_or(0) as u32,
            h: get_int(doc, &m, "h").unwrap_or(0) as u32,
            dur_ms: get_int(doc, &m, "dur_ms").unwrap_or(0),
            label: get_str(doc, &m, "label").unwrap_or_default(),
            favorite: get_bool(doc, &m, "favorite").unwrap_or(false),
            facing: Facing::from_str(&get_str(doc, &m, "facing").unwrap_or_default()),
            scene: get_int(doc, &m, "scene").unwrap_or(0) as u8,
        });
    }
    out
}

pub fn push_capture(doc: &mut AutoCommit, root: &ObjId, c: &Capture) {
    let list = captures_list(doc, root);
    // Newest first.
    let m = doc.insert_object(&list, 0, ObjType::Map).expect("insert capture map");
    let _ = doc.put(&m, "id", c.id.as_str());
    let _ = doc.put(&m, "kind", c.kind.as_str());
    let _ = doc.put(&m, "ts", c.ts);
    let _ = doc.put(&m, "w", c.w as i64);
    let _ = doc.put(&m, "h", c.h as i64);
    let _ = doc.put(&m, "dur_ms", c.dur_ms);
    let _ = doc.put(&m, "label", c.label.as_str());
    let _ = doc.put(&m, "favorite", c.favorite);
    let _ = doc.put(&m, "facing", c.facing.as_str());
    let _ = doc.put(&m, "scene", c.scene as i64);
}

fn capture_index(doc: &AutoCommit, list: &ObjId, id: &str) -> Option<usize> {
    let n = doc.length(list);
    for i in 0..n {
        if let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(list, i) {
            if get_str(doc, &m, "id").as_deref() == Some(id) {
                return Some(i);
            }
        }
    }
    None
}

pub fn toggle_favorite(doc: &mut AutoCommit, root: &ObjId, id: &str) {
    let list = captures_list(doc, root);
    if let Some(i) = capture_index(doc, &list, id) {
        if let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i) {
            let cur = get_bool(doc, &m, "favorite").unwrap_or(false);
            let _ = doc.put(&m, "favorite", !cur);
        }
    }
}

pub fn delete_capture(doc: &mut AutoCommit, root: &ObjId, id: &str) {
    let list = captures_list(doc, root);
    if let Some(i) = capture_index(doc, &list, id) {
        let _ = doc.delete(&list, i);
    }
}

pub fn record_scan(doc: &mut AutoCommit, root: &ObjId, raw: &str, ts: i64) {
    let m = match map_at(doc, root, "last_scan") {
        Some(m) => m,
        None => doc.put_object(root, "last_scan", ObjType::Map).expect("scan map"),
    };
    let _ = doc.put(&m, "raw", raw);
    let _ = doc.put(&m, "ts", ts);
}

// ---- seeding ---------------------------------------------------------

/// True once the doc has been initialised by this app.
pub fn is_seeded(doc: &AutoCommit, root: &ObjId) -> bool {
    get_str(doc, root, "screen").is_some()
}

/// Seed default settings + a populated sample gallery + the initial screen
/// on first run (empty doc). Idempotent: a no-op once seeded.
pub fn seed(doc: &mut AutoCommit, root: &ObjId, samples: &[Capture], initial: Screen) {
    if is_seeded(doc, root) {
        return;
    }
    write_screen(doc, root, initial);
    write_mode(doc, root, CaptureMode::Photo);
    write_facing(doc, root, Facing::Back);
    let settings = Settings::default();
    write_settings(doc, root, &settings);
    // `push_capture` inserts at index 0 (newest-on-top). Seed oldest first
    // so the first sample (most recent) lands on top after all pushes.
    for c in samples.iter().rev() {
        push_capture(doc, root, c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ROOT;

    fn sample(id: &str, fav: bool) -> Capture {
        Capture {
            id: id.into(),
            kind: CaptureKind::Photo,
            ts: 1_700_000_000,
            w: 4032,
            h: 3024,
            dur_ms: 0,
            label: "Sample".into(),
            favorite: fav,
            facing: Facing::Back,
            scene: 1,
        }
    }

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
    fn captures_round_trip_newest_first() {
        let mut doc = AutoCommit::new();
        let root = ROOT;
        push_capture(&mut doc, &root, &sample("a", false));
        push_capture(&mut doc, &root, &sample("b", true));
        let caps = read_captures(&doc, &root);
        assert_eq!(caps.len(), 2);
        // b was pushed last and inserted at index 0 => newest first.
        assert_eq!(caps[0].id, "b");
        assert_eq!(caps[1].id, "a");
    }

    #[test]
    fn toggle_and_delete_capture() {
        let mut doc = AutoCommit::new();
        let root = ROOT;
        push_capture(&mut doc, &root, &sample("a", false));
        toggle_favorite(&mut doc, &root, "a");
        assert!(read_captures(&doc, &root)[0].favorite);
        delete_capture(&mut doc, &root, "a");
        assert!(read_captures(&doc, &root).is_empty());
    }

    #[test]
    fn seed_is_idempotent() {
        let mut doc = AutoCommit::new();
        let root = ROOT;
        assert!(!is_seeded(&doc, &root));
        seed(&mut doc, &root, &[sample("a", false)], Screen::Scanner);
        assert!(is_seeded(&doc, &root));
        assert_eq!(read_screen(&doc, &root), Screen::Scanner);
        // Second seed must not duplicate.
        seed(&mut doc, &root, &[sample("b", false)], Screen::Camera);
        assert_eq!(read_captures(&doc, &root).len(), 1);
        assert_eq!(read_screen(&doc, &root), Screen::Scanner);
    }
}
