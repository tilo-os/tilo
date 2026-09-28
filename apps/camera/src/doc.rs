//! Automerge document model for the Camera app.
//!
//! Layout under the app root object:
//!
//! ```text
//! root
//! ├── "mode"      (Str)   # photo | video  (camera capture sub-mode)
//! ├── "facing"    (Str)   # back | front | wide
//! ├── "settings"  (Map)   # scalar settings (see Settings)
//! ├── "scans"     (List)  # [ { raw, ts } ] newest-first scan history
//! └── "recent_capture" (Map) # one portable content-addressed handoff
//! ```
//!
//! The photo/video *pixels* live as verified bytes in the app's content store (see
//! [`crate::media`]); a local capture file is merely an optional projection.
//! The CRDT stores collaborative metadata and its content-addressed media ID,
//! plus scan history and capture preferences. Camera deliberately retains one
//! recent handoff, not a parallel photo/video library.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, Value};
use ui::doc::{read_bool, read_i64_coerced, read_str};

use crate::model::{
    CaptureKind, CaptureMode, Facing, PhotoFormat, Resolution, ScanEntry, Screen, Settings, Timer,
};

/// Portable capture metadata. Pixel bytes live under `media_id` in the app's
/// content store, verified on read; the document deliberately contains no path.
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
    read_str(doc, root, "mode").map_or(CaptureMode::Photo, |s| CaptureMode::parse_str(&s))
}

pub fn write_mode(doc: &mut AutoCommit, root: &ObjId, mode: CaptureMode) {
    let _ = doc.put(root, "mode", mode.as_str());
}

pub fn read_facing(doc: &AutoCommit, root: &ObjId) -> Facing {
    read_str(doc, root, "facing").map_or(Facing::Back, |s| Facing::parse_str(&s))
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
    if let Some(v) = read_str(doc, &m, "resolution") {
        s.resolution = Resolution::parse_str(&v);
    }
    if let Some(v) = read_str(doc, &m, "photo_format") {
        s.photo_format = PhotoFormat::parse_str(&v);
    }
    if let Some(v) = read_i64_coerced(doc, &m, "timer") {
        s.timer = Timer::from_seconds(v);
    }
    if let Some(v) = read_bool(doc, &m, "grid") {
        s.grid = v;
    }
    if let Some(v) = read_bool(doc, &m, "level") {
        s.level = v;
    }
    if let Some(v) = read_bool(doc, &m, "mirror_front") {
        s.mirror_front = v;
    }
    if let Some(v) = read_bool(doc, &m, "geotag") {
        s.geotag = v;
    }
    if let Some(v) = read_bool(doc, &m, "hdr") {
        s.hdr = v;
    }
    if let Some(v) = read_str(doc, &m, "default_facing") {
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

// ---- scan history ----------------------------------------------------

const MAX_SCANS: usize = 24;

/// Read the scan history, newest first.
pub fn read_scans(doc: &AutoCommit, root: &ObjId) -> Vec<ScanEntry> {
    let Some(list) = list_at(doc, root, "scans") else {
        return Vec::new();
    };
    // A hostile document must not turn opening the Scanner into an
    // unbounded UI-thread traversal. The durable writer below keeps normal
    // documents at this cap; excess rows are deliberately ignored.
    let n = doc.length(&list).min(MAX_SCANS);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i) else {
            continue;
        };
        let raw = read_str(doc, &m, "raw").unwrap_or_default();
        if raw.is_empty() {
            continue;
        }
        out.push(ScanEntry {
            raw,
            ts: read_i64_coerced(doc, &m, "ts").unwrap_or(0),
        });
    }
    out
}

/// Record a scanned code at the front of the history. Re-scanning the same
/// payload moves it to the front rather than duplicating; the list is capped.
///
/// # Errors
///
/// Returns an explicit refusal for invalid payloads, oversized lists, or
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
            && read_str(doc, &m, "raw").as_deref() == Some(raw)
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

// ---- recent capture handoff -----------------------------------------

/// Read Camera's single contextual handoff in O(1). The selected Automerge
/// object is atomic under concurrent replacement, so fields from two captures
/// can never be spliced together.
pub fn read_recent_capture(doc: &AutoCommit, root: &ObjId) -> Option<SharedCapture> {
    let recent = map_at(doc, root, "recent_capture")?;
    let (Some(id), Some(media_id)) = (
        read_str(doc, &recent, "id"),
        read_str(doc, &recent, "media"),
    ) else {
        return None;
    };
    if id.is_empty() || id.len() > 160 || !crate::media::is_media_id(&media_id) {
        return None;
    }
    let kind = match read_str(doc, &recent, "kind").as_deref() {
        Some("video") => CaptureKind::Video,
        _ => CaptureKind::Photo,
    };
    let w = read_i64_coerced(doc, &recent, "w").and_then(|n| u32::try_from(n).ok())?;
    let h = read_i64_coerced(doc, &recent, "h").and_then(|n| u32::try_from(n).ok())?;
    if w == 0 || h == 0 || u64::from(w).saturating_mul(u64::from(h)) > 33_554_432 {
        return None;
    }
    Some(SharedCapture {
        id,
        media_id,
        kind,
        ts: read_i64_coerced(doc, &recent, "ts").unwrap_or(0).max(0),
        w,
        h,
        dur_ms: read_i64_coerced(doc, &recent, "dur").unwrap_or(0).max(0),
        label: read_str(doc, &recent, "label")
            .filter(|s| s.len() <= 256)
            .unwrap_or_else(|| "Capture".into()),
        facing: read_str(doc, &recent, "facing").map_or(Facing::Back, |s| Facing::parse_str(&s)),
    })
}

/// # Errors
///
/// Returns an explicit refusal for malformed metadata, a hostile oversized
/// index, or a failed Automerge write.
pub fn register_recent_capture(
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
    let item = doc
        .put_object(root, "recent_capture", ObjType::Map)
        .map_err(|_| "couldn't register recent capture")?;
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
    Ok(())
}

// ---- first-run defaults ----------------------------------------------

/// True once the doc has been initialised by this app.
pub fn is_seeded(doc: &AutoCommit, root: &ObjId) -> bool {
    read_str(doc, root, "mode").is_some()
}

/// Write capture preferences on first run (empty doc). Navigation is local
/// presentation state, deliberately excluded from collaboration/history.
/// No sample content is fabricated. Idempotent: a no-op once initialised.
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
    fn camera_doc_round_trips_preferences_and_bounds_history_without_seeding() {
        // Phase `settings_round_trip`.
        {
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
        // Phase `init_defaults_is_idempotent_and_seeds_no_content`.
        {
            let mut doc = AutoCommit::new();
            let root = ROOT;
            assert!(!is_seeded(&doc, &root));
            init_defaults(&mut doc, &root, Screen::Scanner);
            assert!(is_seeded(&doc, &root));
            // No fabricated media content.
            assert!(read_recent_capture(&doc, &root).is_none());
            assert!(read_scans(&doc, &root).is_empty());
            // Second init leaves durable preferences untouched; screens are local.
            init_defaults(&mut doc, &root, Screen::Camera);
            assert_eq!(read_mode(&doc, &root), CaptureMode::Photo);
        }
        // Phase `scans_are_newest_first_deduped_and_capped`.
        {
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
    }
}
