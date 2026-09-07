//! Automerge document model for the Camera app.
//!
//! Layout under the app root object:
//!
//! ```text
//! root
//! ├── "mode"      (Str)   # photo | video  (camera capture sub-mode)
//! ├── "facing"    (Str)   # back | front | wide
//! ├── "settings"  (Map)   # scalar settings (see Settings)
//! ├── "favorites" (Map)   # { <file name>: true } — starred library items
//! ├── "scans"     (List)  # [ { raw, ts } ] newest-first scan history
//! └── "captures"  (List)  # portable qdoc-backed gallery metadata
//! ```
//!
//! The photo/video *pixels* travel as verified qdoc blobs (see
//! [`crate::media`]); a local library file is merely an optional projection.
//! The CRDT stores collaborative metadata and its content-addressed media ID,
//! plus favorites, scan history, and capture preferences.

use std::collections::HashSet;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};

use crate::model::{
    CaptureKind, CaptureMode, Facing, PhotoFormat, Resolution, ScanEntry, Screen, Settings, Timer,
};

/// Portable capture metadata. Pixel bytes live under `media_id` in qdoc's
/// verified blob transport; the document deliberately contains no path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedCapture {
    pub id: String,
    pub media_id: String,
    pub kind: CaptureKind,
    pub ts: i64,
    pub w: u32,
    pub h: u32,
    pub dur_ms: i64,
    pub label: String,
    pub facing: Facing,
}

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

// ---- mode / facing --------------------------------------------------

pub fn read_mode(doc: &AutoCommit, root: &ObjId) -> CaptureMode {
    get_str(doc, root, "mode").map_or(CaptureMode::Photo, |s| CaptureMode::parse_str(&s))
}

pub fn write_mode(doc: &mut AutoCommit, root: &ObjId, mode: CaptureMode) {
    let _ = doc.put(root, "mode", mode.as_str());
}

pub fn read_facing(doc: &AutoCommit, root: &ObjId) -> Facing {
    get_str(doc, root, "facing").map_or(Facing::Back, |s| Facing::parse_str(&s))
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
        s.resolution = Resolution::parse_str(&v);
    }
    if let Some(v) = get_str(doc, &m, "photo_format") {
        s.photo_format = PhotoFormat::parse_str(&v);
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
        s.default_facing = Facing::parse_str(&v);
    }
    s
}

pub fn write_settings(doc: &mut AutoCommit, root: &ObjId, s: &Settings) {
    let m = if let Some(m) = map_at(doc, root, "settings") {
        m
    } else {
        let Ok(m) = doc.put_object(root, "settings", ObjType::Map) else {
            return;
        };
        m
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

/// Locate or initialise the settings map for one granular preference write.
/// Each canonical Camera action calls only the key it owns, preserving other
/// preferences that may have changed concurrently on a collaborating peer.
fn settings_map(doc: &mut AutoCommit, root: &ObjId) -> Option<ObjId> {
    map_at(doc, root, "settings").or_else(|| doc.put_object(root, "settings", ObjType::Map).ok())
}

pub fn set_resolution(doc: &mut AutoCommit, root: &ObjId, value: Resolution) {
    if let Some(map) = settings_map(doc, root) {
        let _ = doc.put(&map, "resolution", value.as_str());
    }
}
pub fn set_photo_format(doc: &mut AutoCommit, root: &ObjId, value: PhotoFormat) {
    if let Some(map) = settings_map(doc, root) {
        let _ = doc.put(&map, "photo_format", value.as_str());
    }
}
pub fn set_timer(doc: &mut AutoCommit, root: &ObjId, value: Timer) {
    if let Some(map) = settings_map(doc, root) {
        let _ = doc.put(&map, "timer", value.seconds());
    }
}
pub fn set_grid(doc: &mut AutoCommit, root: &ObjId, value: bool) {
    if let Some(map) = settings_map(doc, root) {
        let _ = doc.put(&map, "grid", value);
    }
}
pub fn set_level(doc: &mut AutoCommit, root: &ObjId, value: bool) {
    if let Some(map) = settings_map(doc, root) {
        let _ = doc.put(&map, "level", value);
    }
}
pub fn set_mirror_front(doc: &mut AutoCommit, root: &ObjId, value: bool) {
    if let Some(map) = settings_map(doc, root) {
        let _ = doc.put(&map, "mirror_front", value);
    }
}
pub fn set_geotag(doc: &mut AutoCommit, root: &ObjId, value: bool) {
    if let Some(map) = settings_map(doc, root) {
        let _ = doc.put(&map, "geotag", value);
    }
}
pub fn set_hdr(doc: &mut AutoCommit, root: &ObjId, value: bool) {
    if let Some(map) = settings_map(doc, root) {
        let _ = doc.put(&map, "hdr", value);
    }
}
pub fn set_default_facing(doc: &mut AutoCommit, root: &ObjId, value: Facing) {
    if let Some(map) = settings_map(doc, root) {
        let _ = doc.put(&map, "default_facing", value.as_str());
    }
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
    let m = if let Some(m) = map_at(doc, root, "favorites") {
        m
    } else {
        let Ok(m) = doc.put_object(root, "favorites", ObjType::Map) else {
            return;
        };
        m
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
    // A legacy or hostile document must not turn opening the Scanner into an
    // unbounded UI-thread traversal. The durable writer below keeps normal
    // documents at this cap; excess legacy rows are deliberately ignored.
    let n = doc.length(&list).min(MAX_SCANS);
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
///
/// # Errors
///
/// Returns an explicit refusal for invalid payloads, hostile legacy lists, or
/// an Automerge write that could not be completed.
pub fn record_scan(doc: &mut AutoCommit, root: &ObjId, raw: &str, ts: i64) -> Result<(), String> {
    if raw.is_empty() || raw.len() > 16 * 1024 {
        return Err("scan payload is invalid or too large".into());
    }
    let list = if let Some(l) = list_at(doc, root, "scans") {
        l
    } else {
        doc.put_object(root, "scans", ObjType::List)
            .map_err(|_| "couldn't create scan history")?
    };
    // Drop any existing entry with the same payload.
    let n = doc.length(&list);
    // Do not make a local scan action perform unbounded repair of a malformed
    // collaborative list. A later explicit migration can handle it off the
    // interaction path; this action explicitly refuses in the meantime.
    if n > MAX_SCANS {
        return Err("scan history exceeds the supported safety limit".into());
    }
    for i in (0..n).rev() {
        if let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i)
            && get_str(doc, &m, "raw").as_deref() == Some(raw)
        {
            doc.delete(&list, i)
                .map_err(|_| "couldn't update scan history")?;
        }
    }
    let m = doc
        .insert_object(&list, 0, ObjType::Map)
        .map_err(|_| "couldn't update scan history")?;
    doc.put(&m, "raw", raw)
        .map_err(|_| "couldn't update scan history")?;
    doc.put(&m, "ts", ts)
        .map_err(|_| "couldn't update scan history")?;
    // At most one row can now exceed the cap, so this remains bounded.
    if doc.length(&list) > MAX_SCANS {
        doc.delete(&list, doc.length(&list) - 1)
            .map_err(|_| "couldn't trim scan history")?;
    }
    Ok(())
}

// ---- shared capture index -------------------------------------------

const MAX_SHARED_CAPTURES: usize = 2_000;

pub fn read_shared_captures(doc: &AutoCommit, root: &ObjId) -> Vec<SharedCapture> {
    let Some(list) = list_at(doc, root, "captures") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for i in 0..doc.length(&list).min(MAX_SHARED_CAPTURES) {
        let Ok(Some((Value::Object(ObjType::Map), item))) = doc.get(&list, i) else {
            continue;
        };
        let (Some(id), Some(media_id)) = (get_str(doc, &item, "id"), get_str(doc, &item, "media"))
        else {
            continue;
        };
        if id.is_empty() || id.len() > 160 || !crate::media::is_media_id(&media_id) {
            continue;
        }
        let kind = match get_str(doc, &item, "kind").as_deref() {
            Some("video") => CaptureKind::Video,
            _ => CaptureKind::Photo,
        };
        let w = get_int(doc, &item, "w")
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0);
        let h = get_int(doc, &item, "h")
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0);
        if w == 0 || h == 0 || u64::from(w).saturating_mul(u64::from(h)) > 33_554_432 {
            continue;
        }
        out.push(SharedCapture {
            id,
            media_id,
            kind,
            ts: get_int(doc, &item, "ts").unwrap_or(0).max(0),
            w,
            h,
            dur_ms: get_int(doc, &item, "dur").unwrap_or(0).max(0),
            label: get_str(doc, &item, "label")
                .filter(|s| s.len() <= 256)
                .unwrap_or_else(|| "Capture".into()),
            facing: get_str(doc, &item, "facing").map_or(Facing::Back, |s| Facing::parse_str(&s)),
        });
    }
    out
}

/// # Errors
///
/// Returns an explicit refusal for malformed metadata, a hostile oversized
/// index, or a failed Automerge write.
pub fn register_shared_capture(
    doc: &mut AutoCommit,
    root: &ObjId,
    capture: &SharedCapture,
) -> Result<(), String> {
    if capture.id.is_empty()
        || capture.id.len() > 160
        || !crate::media::is_media_id(&capture.media_id)
        || capture.w == 0
        || capture.h == 0
        || u64::from(capture.w).saturating_mul(u64::from(capture.h)) > 33_554_432
        || capture.label.len() > 256
    {
        return Err("capture metadata is invalid".into());
    }
    let list = if let Some(list) = list_at(doc, root, "captures") {
        list
    } else {
        doc.put_object(root, "captures", ObjType::List)
            .map_err(|_| "couldn't create shared capture index")?
    };
    let length = doc.length(&list);
    // A peer/legacy document can contain an arbitrarily long list. Do not
    // turn a single capture into an unbounded traversal or trimming loop on
    // the UI mutation path; fail closed until an explicit bounded migration
    // is available.
    if length > MAX_SHARED_CAPTURES {
        return Err("shared capture index exceeds the supported safety limit".into());
    }
    for i in (0..length).rev() {
        if let Ok(Some((Value::Object(ObjType::Map), item))) = doc.get(&list, i)
            && get_str(doc, &item, "id").as_deref() == Some(capture.id.as_str())
        {
            doc.delete(&list, i)
                .map_err(|_| "couldn't replace shared capture")?;
        }
    }
    let item = doc
        .insert_object(&list, 0, ObjType::Map)
        .map_err(|_| "couldn't register shared capture")?;
    doc.put(&item, "id", &capture.id)
        .map_err(|_| "couldn't register shared capture")?;
    doc.put(&item, "media", &capture.media_id)
        .map_err(|_| "couldn't register shared capture")?;
    doc.put(&item, "kind", capture.kind.as_str())
        .map_err(|_| "couldn't register shared capture")?;
    doc.put(&item, "ts", capture.ts)
        .map_err(|_| "couldn't register shared capture")?;
    doc.put(&item, "w", i64::from(capture.w))
        .map_err(|_| "couldn't register shared capture")?;
    doc.put(&item, "h", i64::from(capture.h))
        .map_err(|_| "couldn't register shared capture")?;
    doc.put(&item, "dur", capture.dur_ms)
        .map_err(|_| "couldn't register shared capture")?;
    doc.put(&item, "label", &capture.label)
        .map_err(|_| "couldn't register shared capture")?;
    doc.put(&item, "facing", capture.facing.as_str())
        .map_err(|_| "couldn't register shared capture")?;
    // The validated precondition means insertion can exceed the cap by only
    // one, keeping this recovery operation strictly bounded.
    if doc.length(&list) > MAX_SHARED_CAPTURES {
        doc.delete(&list, doc.length(&list) - 1)
            .map_err(|_| "couldn't trim shared capture index")?;
    }
    Ok(())
}

/// Remove every materialized row for a shared capture identity. The qdoc blob
/// itself is immutable/content-addressed and deliberately not deleted here:
/// another capture can safely reference the same bytes.
///
/// # Errors
///
/// Returns an explicit refusal for malformed identities, an oversized index,
/// or a failed Automerge write.
pub fn remove_shared_capture(doc: &mut AutoCommit, root: &ObjId, id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 160 {
        return Err("capture identity is invalid".into());
    }
    let Some(list) = list_at(doc, root, "captures") else {
        return Ok(());
    };
    let length = doc.length(&list);
    // Same fail-closed rule as registration: deletion must never scan an
    // unbounded hostile list on the UI/canonical-action path.
    if length > MAX_SHARED_CAPTURES {
        return Err("shared capture index exceeds the supported safety limit".into());
    }
    for index in (0..length).rev() {
        if let Ok(Some((Value::Object(ObjType::Map), item))) = doc.get(&list, index)
            && get_str(doc, &item, "id").as_deref() == Some(id)
        {
            doc.delete(&list, index)
                .map_err(|_| "couldn't remove shared capture")?;
        }
    }
    Ok(())
}

/// Check the bounded invariant before an external file deletion. This keeps a
/// hostile collaborative index from turning a failed canonical removal into a
/// locally destructive operation.
///
/// # Errors
///
/// Returns an explicit refusal if the index is over the supported cap.
pub fn can_mutate_shared_captures(doc: &AutoCommit, root: &ObjId) -> Result<(), String> {
    let Some(list) = list_at(doc, root, "captures") else {
        return Ok(());
    };
    (doc.length(&list) <= MAX_SHARED_CAPTURES)
        .then_some(())
        .ok_or_else(|| "shared capture index exceeds the supported safety limit".into())
}

// ---- first-run defaults ----------------------------------------------

/// True once the doc has been initialised by this app.
pub fn is_seeded(doc: &AutoCommit, root: &ObjId) -> bool {
    get_str(doc, root, "mode").is_some()
}

/// Write capture preferences on first run (empty doc). Navigation is local
/// presentation state, deliberately excluded from collaboration/history.
/// No sample content is fabricated — the library starts empty and reflects
/// whatever is actually on disk. Idempotent: a no-op once initialised.
pub fn init_defaults(doc: &mut AutoCommit, root: &ObjId, _initial: Screen) {
    if is_seeded(doc, root) {
        return;
    }
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
        let s = Settings {
            grid: false,
            timer: Timer::S10,
            resolution: Resolution::R4K,
            hdr: false,
            ..Settings::default()
        };
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
        record_scan(&mut doc, &root, "https://a", 100).unwrap();
        record_scan(&mut doc, &root, "https://b", 200).unwrap();
        // Re-scanning "a" moves it to the front, not a duplicate.
        record_scan(&mut doc, &root, "https://a", 300).unwrap();
        let scans = read_scans(&doc, &root);
        assert_eq!(scans.len(), 2);
        assert_eq!(scans[0].raw, "https://a");
        assert_eq!(scans[0].ts, 300);
        assert_eq!(scans[1].raw, "https://b");
        // Cap is enforced.
        for i in 0..(MAX_SCANS + 10) {
            record_scan(&mut doc, &root, &format!("code-{i}"), i as i64).unwrap();
        }
        assert!(read_scans(&doc, &root).len() <= MAX_SCANS);
    }

    #[test]
    fn hostile_oversized_capture_list_is_inert_for_mutations() {
        let mut doc = AutoCommit::new();
        let root = ROOT;
        let list = doc
            .put_object(&root, "captures", ObjType::List)
            .expect("capture list");
        for index in 0..=MAX_SHARED_CAPTURES {
            doc.insert(&list, index, "legacy row")
                .expect("legacy list row");
        }
        let capture = SharedCapture {
            id: "IMG_new.jpg".into(),
            media_id: format!("{}{}", crate::media::MEDIA_PREFIX, "a".repeat(64)),
            kind: CaptureKind::Photo,
            ts: 1,
            w: 64,
            h: 48,
            dur_ms: 0,
            label: "New capture".into(),
            facing: Facing::Back,
        };

        assert!(register_shared_capture(&mut doc, &root, &capture).is_err());
        assert!(remove_shared_capture(&mut doc, &root, "IMG_new.jpg").is_err());

        // Both canonical actions fail closed; neither traverses or modifies
        // an unbounded legacy list from the interaction path.
        assert_eq!(doc.length(&list), MAX_SHARED_CAPTURES + 1);
    }

    #[test]
    fn init_defaults_is_idempotent_and_seeds_no_content() {
        let mut doc = AutoCommit::new();
        let root = ROOT;
        assert!(!is_seeded(&doc, &root));
        init_defaults(&mut doc, &root, Screen::Scanner);
        assert!(is_seeded(&doc, &root));
        // No fabricated library content.
        assert!(read_favorites(&doc, &root).is_empty());
        assert!(read_scans(&doc, &root).is_empty());
        // Second init leaves durable preferences untouched; screens are local.
        init_defaults(&mut doc, &root, Screen::Camera);
        assert_eq!(read_mode(&doc, &root), CaptureMode::Photo);
    }
}
