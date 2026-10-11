//! Automerge document model for the editor project.
//!
//! Structure (the shared, collaborative half of the editor — playhead,
//! zoom and selection are deliberately per-peer view-state and live in
//! the widget, not here):
//!
//! ```text
//! root
//! ├── "project" (Map)  name / fps / width / height
//! ├── "media"   (Map keyed by MediaId)  → { name,kind,duration,w,h,fps,palette }
//! ├── "tracks"  (Map keyed by TrackId)  → { name,kind,index,muted,locked,solo,
//!                                            gain_db,pan }
//! └── "clips"   (Map keyed by ClipId)   → { track,media,start,in,out,opacity,
//!                                            volume,transition,transition_duration,
//!                                            palette,filters,fades,effects }
//! ```
//!
//! Every helper takes the borrowed `&mut AutoCommit` (the app's shared
//! `State::doc`). Two peers that adopt a common base converge: a moved
//! clip, a trimmed in-point, a new track all merge field-by-field.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use ui::doc::{read_bool, read_number, read_scalar, read_str};
use uuid::Uuid;

use crate::curve::{Channel, Curve, CurvePoint, Spline};
use crate::media::{self, Hydration};
use crate::model::{
    ChromaKey, Clip, ClipEffect, EffectKind, Filters, Marker, Mask, MaskShape, MediaItem,
    MediaKind, Project, Track, TrackKind, Transform, Transition,
};
use crate::param::{ClipAnimation, Interp, Keyframe, Param, ParamTrack};

pub struct VideoDoc;

impl VideoDoc {
    #[expect(
        clippy::expect_used,
        reason = "a map can always be put under the document root"
    )]
    fn map(doc: &mut AutoCommit, key: &str) -> ObjId {
        match doc.get(&ROOT, key) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, key, ObjType::Map)
                .expect("put_object map"),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "callers pass a map they just read or created, which always takes a child map"
    )]
    fn ensure_child(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> ObjId {
        match doc.get(parent, key) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(parent, key, ObjType::Map)
                .expect("put_object child"),
        }
    }

    /// An `Int`, a `Uint` reinterpreted as `i64`, or an `F64` truncated.
    fn get_i64(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<i64> {
        match read_scalar(doc, obj, key)? {
            ScalarValue::Int(i) => Some(i),
            ScalarValue::Uint(u) => Some(u as i64),
            ScalarValue::F64(f) => Some(f as i64),
            _ => None,
        }
    }

    /// Whether the map at `key` exists **and** holds at least one entry.
    /// A map that was created but never written to is not content.
    fn has_entries(doc: &AutoCommit, key: &str) -> bool {
        match doc.get(&ROOT, key) {
            Ok(Some((Value::Object(ObjType::Map), id))) => doc.length(&id) > 0,
            _ => false,
        }
    }

    /// True when this document carries no editor content at all.
    ///
    /// Emptiness spans **every** map an editing session writes, not just
    /// `project`: importing media writes only the `media` map, and a document
    /// holding imported clips is plainly not empty even before a project map
    /// exists. Keying on `project` alone reported such a document empty, which
    /// let a demo seed overwrite real imported media.
    pub fn is_empty(doc: &AutoCommit) -> bool {
        !["project", "media", "tracks", "clips"]
            .iter()
            .any(|k| Self::has_entries(doc, k))
    }

    // --- writers ---------------------------------------------------------

    pub fn write_project_meta(doc: &mut AutoCommit, p: &Project) {
        let proj = Self::map(doc, "project");
        let _ = doc.put(&proj, "name", p.name.as_str());
        let _ = doc.put(&proj, "fps", p.fps);
        let _ = doc.put(&proj, "width", i64::from(p.width));
        let _ = doc.put(&proj, "height", i64::from(p.height));
        let _ = doc.put(&proj, "format_explicit", p.format_explicit);
        Self::write_marks(doc, p.mark_in, p.mark_out);
        // Markers have their own actions and map. Metadata updates deliberately
        // do not rewrite that map from a stale project snapshot.
    }

    /// Write the timeline's in and out marks.
    pub fn write_marks(doc: &mut AutoCommit, mark_in: Option<f64>, mark_out: Option<f64>) {
        let proj = Self::map(doc, "project");
        // An absent mark is an absent key, not a sentinel: a peer that reads
        // 0.0 for "no in mark" cannot tell it from a mark on the first frame.
        for (key, mark) in [("mark_in", mark_in), ("mark_out", mark_out)] {
            match mark {
                Some(t) => {
                    let _ = doc.put(&proj, key, t);
                }
                None => {
                    let _ = doc.delete(&proj, key);
                }
            }
        }
    }

    pub fn upsert_media(doc: &mut AutoCommit, m: &MediaItem) {
        let media = Self::map(doc, "media");
        let obj = Self::ensure_child(doc, &media, &m.id.to_string());
        let _ = doc.put(&obj, "name", m.name.as_str());
        let _ = doc.put(&obj, "kind", m.kind.as_str());
        let _ = doc.put(&obj, "duration", m.duration);
        let _ = doc.put(&obj, "width", i64::from(m.width));
        let _ = doc.put(&obj, "height", i64::from(m.height));
        let _ = doc.put(&obj, "fps", m.fps);
        let _ = doc.put(&obj, "palette", i64::from(m.palette));
        // FUSE-media seam: the CRDT carries the workspace path + hydration
        // state, never the bytes (see crate::media).
        let _ = doc.put(&obj, "path", m.path.as_str());
        let _ = doc.put(&obj, "hydration", m.hydration.as_str());
        let _ = doc.put(&obj, "has_audio", m.has_audio);
        // A title's text and styling. Written as a compact JSON scalar for
        // the same reason the effects stack is: the action model rewrites a
        // media item whole, so a structured sub-map would buy no finer merge
        // granularity and would cost a schema.
        match &m.text {
            Some(t) => {
                let _ = doc.put(&obj, "text", serde_json::to_string(t).unwrap_or_default());
            }
            // Explicitly cleared, so turning a title back into a plain
            // reference replicates as a change rather than as silence.
            None => {
                let _ = doc.delete(&obj, "text");
            }
        }
    }

    pub fn upsert_track(doc: &mut AutoCommit, t: &Track) {
        let tracks = Self::map(doc, "tracks");
        let obj = Self::ensure_child(doc, &tracks, &t.id.to_string());
        let _ = doc.put(&obj, "name", t.name.as_str());
        let _ = doc.put(&obj, "kind", t.kind.as_str());
        let _ = doc.put(&obj, "index", i64::from(t.index));
        let _ = doc.put(&obj, "muted", t.muted);
        let _ = doc.put(&obj, "locked", t.locked);
        let _ = doc.put(&obj, "solo", t.solo);
        let _ = doc.put(&obj, "gain_db", t.gain_db);
        let _ = doc.put(&obj, "pan", t.pan);
    }

    pub fn upsert_clip(doc: &mut AutoCommit, c: &Clip) {
        let obj = Self::write_clip_scalars(doc, c);
        // This is the explicit full-write path for seeding, import and
        // project replacement. Incremental action replay uses
        // `upsert_clip_preserving_nested` below instead.
        Self::write_effects(doc, &obj, &c.effects);
        Self::write_transform(doc, &obj, c.transform);
        Self::write_curves(doc, &obj, &c.curves);
        Self::write_key(doc, &obj, c.key);
        Self::write_mask(doc, &obj, c.mask);
        Self::write_animation(doc, &obj, &c.anim);
        Self::write_markers(doc, &obj, &c.markers);
    }

    /// Write a clip from a structural action without letting a delayed
    /// move/split/ripple payload replace nested effects, markers, or inspector
    /// records it did not author. New clips still receive their complete
    /// initial nested state; existing clips receive scalar/structural fields
    /// only and nested changes stay with their dedicated actions.
    pub fn upsert_clip_preserving_nested(doc: &mut AutoCommit, c: &Clip) {
        let is_new = Self::existing_clip(doc, c.id).is_none();
        let obj = Self::write_clip_scalars(doc, c);
        if is_new {
            Self::write_effects(doc, &obj, &c.effects);
            Self::write_transform(doc, &obj, c.transform);
            Self::write_curves(doc, &obj, &c.curves);
            Self::write_key(doc, &obj, c.key);
            Self::write_mask(doc, &obj, c.mask);
            Self::write_animation(doc, &obj, &c.anim);
            Self::write_markers(doc, &obj, &c.markers);
        }
    }

    fn write_clip_scalars(doc: &mut AutoCommit, c: &Clip) -> ObjId {
        let clips = Self::map(doc, "clips");
        let obj = Self::ensure_child(doc, &clips, &c.id.to_string());
        let _ = doc.put(&obj, "track", c.track.to_string());
        let _ = doc.put(&obj, "media", c.media.to_string());
        let _ = doc.put(&obj, "start", c.start);
        let _ = doc.put(&obj, "in", c.in_point);
        let _ = doc.put(&obj, "out", c.out_point);
        let _ = doc.put(&obj, "opacity", c.opacity);
        let _ = doc.put(&obj, "volume", c.volume);
        let _ = doc.put(&obj, "transition", c.transition.as_str());
        let _ = doc.put(&obj, "palette", i64::from(c.palette));
        let _ = doc.put(&obj, "brightness", c.filters.brightness);
        let _ = doc.put(&obj, "contrast", c.filters.contrast);
        let _ = doc.put(&obj, "saturation", c.filters.saturation);
        let _ = doc.put(&obj, "fade_in", c.fade_in);
        let _ = doc.put(&obj, "fade_out", c.fade_out);
        let _ = doc.put(&obj, "transition_duration", c.transition_duration);
        let _ = doc.put(&obj, "speed_rate", c.speed.rate);
        let _ = doc.put(&obj, "speed_reverse", c.speed.reverse);
        // The link group. Written as the plain id when there is one and
        // *deleted* when there is not, so unlinking replicates as a change
        // rather than as silence -- a peer that already held the link would
        // otherwise keep drawing the pair and keep moving both halves.
        match c.link {
            Some(link) => {
                let _ = doc.put(&obj, "link", link.to_string());
            }
            None => {
                let _ = doc.delete(&obj, "link");
            }
        }
        let _ = doc.put(&obj, "sound_split", c.sound_split);
        let _ = doc.put(&obj, "link_follows", c.link_follows);
        obj
    }

    /// Explicitly replace an effect stack while retaining individually keyed
    /// CRDT entries. Omitted ids are tombstoned, so a delayed earlier add
    /// cannot recreate an entry the authored replacement removed. Normal
    /// effect actions below mutate only their target entry.
    pub fn set_clip_effects(doc: &mut AutoCommit, id: Uuid, effects: &[ClipEffect]) -> bool {
        let Ok(Some((Value::Object(ObjType::Map), clips))) = doc.get(&ROOT, "clips") else {
            return false;
        };
        let Ok(Some((Value::Object(ObjType::Map), clip))) = doc.get(&clips, id.to_string()) else {
            return false;
        };
        Self::write_effects(doc, &clip, effects);
        true
    }

    /// Add exactly one effect entry. The ordinal is a presentation order,
    /// while the UUID is the durable identity used by actions and search.
    pub fn add_clip_effect(
        doc: &mut AutoCommit,
        clip_id: Uuid,
        effect: ClipEffect,
        ordinal: usize,
    ) -> bool {
        let Some(clip) = Self::existing_clip(doc, clip_id) else {
            return false;
        };
        if effect.id.is_nil() {
            return false;
        }
        if Self::is_tombstoned(doc, &clip, "effect_tombstones", effect.id) {
            return false;
        }
        let effects = Self::ensure_child(doc, &clip, "effects");
        if doc
            .get(&effects, effect.id.to_string())
            .ok()
            .flatten()
            .is_some()
        {
            return false;
        }
        let entry = Self::ensure_child(doc, &effects, &effect.id.to_string());
        let _ = doc.put(&entry, "kind", effect.kind.as_str());
        let _ = doc.put(&entry, "amount", effect.amount);
        let _ = doc.put(&entry, "ordinal", ordinal as i64);
        true
    }

    pub fn set_clip_effect_amount(
        doc: &mut AutoCommit,
        clip_id: Uuid,
        effect_id: Uuid,
        amount: f64,
    ) -> bool {
        let Some(clip) = Self::existing_clip(doc, clip_id) else {
            return false;
        };
        let Ok(Some((Value::Object(ObjType::Map), effects))) = doc.get(&clip, "effects") else {
            return false;
        };
        let Ok(Some((Value::Object(ObjType::Map), effect))) =
            doc.get(&effects, effect_id.to_string())
        else {
            return false;
        };
        doc.put(&effect, "amount", amount).is_ok()
    }

    fn existing_clip(doc: &AutoCommit, id: Uuid) -> Option<ObjId> {
        let Ok(Some((Value::Object(ObjType::Map), clips))) = doc.get(&ROOT, "clips") else {
            return None;
        };
        let Ok(Some((Value::Object(ObjType::Map), clip))) = doc.get(&clips, id.to_string()) else {
            return None;
        };
        Some(clip)
    }

    #[allow(
        clippy::needless_collect,
        reason = "the keys iterator borrows the document while this full replacement deletes omitted ids"
    )]
    fn write_effects(doc: &mut AutoCommit, parent: &ObjId, effects: &[ClipEffect]) {
        let effects_map = Self::ensure_child(doc, parent, "effects");
        let keep: Vec<String> = effects.iter().map(|effect| effect.id.to_string()).collect();
        for id in doc.keys(&effects_map).collect::<Vec<_>>() {
            if !keep.contains(&id) {
                let _ = doc.delete(&effects_map, &id);
                if let Ok(id) = id.parse::<Uuid>() {
                    Self::tombstone(doc, parent, "effect_tombstones", id);
                }
            }
        }
        for (ordinal, effect) in effects.iter().enumerate() {
            if effect.id.is_nil() {
                continue;
            }
            let entry = Self::ensure_child(doc, &effects_map, &effect.id.to_string());
            let _ = doc.put(&entry, "kind", effect.kind.as_str());
            let _ = doc.put(&entry, "amount", effect.amount);
            let _ = doc.put(&entry, "ordinal", ordinal as i64);
        }
    }

    #[allow(
        clippy::needless_collect,
        reason = "the keys iterator borrows the document while this full replacement deletes omitted ids"
    )]
    fn write_markers(doc: &mut AutoCommit, parent: &ObjId, markers: &[Marker]) {
        let marker_map = Self::ensure_child(doc, parent, "markers");
        let keep: Vec<String> = markers.iter().map(|marker| marker.id.to_string()).collect();
        for id in doc.keys(&marker_map).collect::<Vec<_>>() {
            if !keep.contains(&id) {
                let _ = doc.delete(&marker_map, &id);
                if let Ok(id) = id.parse::<Uuid>() {
                    Self::tombstone(doc, parent, "marker_tombstones", id);
                }
            }
        }
        for marker in markers {
            let entry = Self::ensure_child(doc, &marker_map, &marker.id.to_string());
            let _ = doc.put(&entry, "t", marker.t);
            let _ = doc.put(&entry, "name", marker.name.as_str());
            let _ = doc.put(&entry, "palette", i64::from(marker.palette));
            let _ = doc.put(&entry, "note", marker.note.as_str());
        }
    }

    pub fn add_project_marker(doc: &mut AutoCommit, marker: &Marker) -> bool {
        let project = Self::map(doc, "project");
        Self::add_marker(doc, &project, marker)
    }

    pub fn remove_project_marker(doc: &mut AutoCommit, marker_id: Uuid) -> bool {
        let Ok(Some((Value::Object(ObjType::Map), project))) = doc.get(&ROOT, "project") else {
            return false;
        };
        Self::remove_marker(doc, &project, marker_id)
    }

    /// Update one text leaf of a project marker. Marker identity and sibling
    /// fields stay untouched, so a note cannot overwrite a concurrent rename.
    pub fn set_project_marker_text(
        doc: &mut AutoCommit,
        marker_id: Uuid,
        key: &str,
        value: &str,
    ) -> bool {
        let Ok(Some((Value::Object(ObjType::Map), project))) = doc.get(&ROOT, "project") else {
            return false;
        };
        Self::set_marker_text(doc, &project, marker_id, key, value)
    }

    pub fn add_clip_marker(doc: &mut AutoCommit, clip_id: Uuid, marker: &Marker) -> bool {
        let Some(clip) = Self::existing_clip(doc, clip_id) else {
            return false;
        };
        Self::add_marker(doc, &clip, marker)
    }

    pub fn remove_clip_marker(doc: &mut AutoCommit, clip_id: Uuid, marker_id: Uuid) -> bool {
        let Some(clip) = Self::existing_clip(doc, clip_id) else {
            return false;
        };
        Self::remove_marker(doc, &clip, marker_id)
    }

    /// Update one text leaf of a clip-local marker without replacing its
    /// marker map or any other clip data.
    pub fn set_clip_marker_text(
        doc: &mut AutoCommit,
        clip_id: Uuid,
        marker_id: Uuid,
        key: &str,
        value: &str,
    ) -> bool {
        let Some(clip) = Self::existing_clip(doc, clip_id) else {
            return false;
        };
        Self::set_marker_text(doc, &clip, marker_id, key, value)
    }

    fn add_marker(doc: &mut AutoCommit, parent: &ObjId, marker: &Marker) -> bool {
        if Self::is_tombstoned(doc, parent, "marker_tombstones", marker.id) {
            return false;
        }
        let markers = Self::ensure_child(doc, parent, "markers");
        if doc
            .get(&markers, marker.id.to_string())
            .ok()
            .flatten()
            .is_some()
        {
            return false;
        }
        let entry = Self::ensure_child(doc, &markers, &marker.id.to_string());
        let _ = doc.put(&entry, "t", marker.t);
        let _ = doc.put(&entry, "name", marker.name.as_str());
        let _ = doc.put(&entry, "palette", i64::from(marker.palette));
        let _ = doc.put(&entry, "note", marker.note.as_str());
        true
    }

    fn remove_marker(doc: &mut AutoCommit, parent: &ObjId, marker_id: Uuid) -> bool {
        let Ok(Some((Value::Object(ObjType::Map), markers))) = doc.get(parent, "markers") else {
            return false;
        };
        if doc.delete(&markers, marker_id.to_string()).is_err() {
            return false;
        }
        Self::tombstone(doc, parent, "marker_tombstones", marker_id);
        true
    }

    fn set_marker_text(
        doc: &mut AutoCommit,
        parent: &ObjId,
        marker_id: Uuid,
        key: &str,
        value: &str,
    ) -> bool {
        if !matches!(key, "name" | "note") {
            return false;
        }
        let Ok(Some((Value::Object(ObjType::Map), markers))) = doc.get(parent, "markers") else {
            return false;
        };
        let Ok(Some((Value::Object(ObjType::Map), marker))) =
            doc.get(&markers, marker_id.to_string())
        else {
            return false;
        };
        doc.put(&marker, key, value).is_ok()
    }

    fn tombstone(doc: &mut AutoCommit, parent: &ObjId, key: &str, id: Uuid) {
        let tombstones = Self::ensure_child(doc, parent, key);
        let _ = doc.put(&tombstones, id.to_string(), true);
    }

    fn is_tombstoned(doc: &AutoCommit, parent: &ObjId, key: &str, id: Uuid) -> bool {
        let Ok(Some((Value::Object(ObjType::Map), tombstones))) = doc.get(parent, key) else {
            return false;
        };
        read_bool(doc, &tombstones, id.to_string()).unwrap_or(false)
    }

    fn read_markers(doc: &AutoCommit, parent: &ObjId) -> Vec<Marker> {
        let Ok(Some((Value::Object(ObjType::Map), marker_map))) = doc.get(parent, "markers") else {
            return Vec::new();
        };
        let mut markers: Vec<Marker> = doc
            .keys(&marker_map)
            .filter_map(|key| {
                let id = key.parse().ok()?;
                let Ok(Some((Value::Object(ObjType::Map), marker))) = doc.get(&marker_map, &key)
                else {
                    return None;
                };
                Some(Marker {
                    id,
                    t: read_number(doc, &marker, "t").unwrap_or(0.0),
                    name: read_str(doc, &marker, "name").unwrap_or_else(|| "Marker".into()),
                    palette: Self::get_i64(doc, &marker, "palette").unwrap_or(0) as u8,
                    note: read_str(doc, &marker, "note").unwrap_or_default(),
                })
            })
            .collect();
        markers.sort_by(|left, right| {
            left.t
                .total_cmp(&right.t)
                .then_with(|| left.id.cmp(&right.id))
        });
        markers
    }

    fn read_effects(doc: &AutoCommit, parent: &ObjId) -> Vec<ClipEffect> {
        let Ok(Some((Value::Object(ObjType::Map), effect_map))) = doc.get(parent, "effects") else {
            return Vec::new();
        };
        let mut effects: Vec<(i64, ClipEffect)> = doc
            .keys(&effect_map)
            .filter_map(|key| {
                let id = key.parse().ok()?;
                let Ok(Some((Value::Object(ObjType::Map), effect))) = doc.get(&effect_map, &key)
                else {
                    return None;
                };
                Some((
                    Self::get_i64(doc, &effect, "ordinal").unwrap_or(i64::MAX),
                    ClipEffect {
                        id,
                        kind: EffectKind::from_name(&read_str(doc, &effect, "kind")?)?,
                        amount: read_number(doc, &effect, "amount").unwrap_or(0.0),
                    },
                ))
            })
            .collect();
        effects.sort_by(|(left_order, left), (right_order, right)| {
            left_order
                .cmp(right_order)
                .then_with(|| left.id.cmp(&right.id))
        });
        effects.into_iter().map(|(_, effect)| effect).collect()
    }

    fn write_transform(doc: &mut AutoCommit, parent: &ObjId, value: Transform) {
        let map = Self::ensure_child(doc, parent, "transform");
        for (key, value) in [
            ("scale_x", value.scale_x),
            ("scale_y", value.scale_y),
            ("pos_x", value.pos_x),
            ("pos_y", value.pos_y),
            ("rotation", value.rotation),
            ("anchor_x", value.anchor_x),
            ("anchor_y", value.anchor_y),
            ("crop_left", value.crop_left),
            ("crop_right", value.crop_right),
            ("crop_top", value.crop_top),
            ("crop_bottom", value.crop_bottom),
        ] {
            let _ = doc.put(&map, key, value);
        }
    }

    fn read_transform(doc: &AutoCommit, parent: &ObjId) -> Transform {
        let Ok(Some((Value::Object(ObjType::Map), map))) = doc.get(parent, "transform") else {
            return Transform::default();
        };
        let default = Transform::default();
        Transform {
            scale_x: read_number(doc, &map, "scale_x").unwrap_or(default.scale_x),
            scale_y: read_number(doc, &map, "scale_y").unwrap_or(default.scale_y),
            pos_x: read_number(doc, &map, "pos_x").unwrap_or(default.pos_x),
            pos_y: read_number(doc, &map, "pos_y").unwrap_or(default.pos_y),
            rotation: read_number(doc, &map, "rotation").unwrap_or(default.rotation),
            anchor_x: read_number(doc, &map, "anchor_x").unwrap_or(default.anchor_x),
            anchor_y: read_number(doc, &map, "anchor_y").unwrap_or(default.anchor_y),
            crop_left: read_number(doc, &map, "crop_left").unwrap_or(default.crop_left),
            crop_right: read_number(doc, &map, "crop_right").unwrap_or(default.crop_right),
            crop_top: read_number(doc, &map, "crop_top").unwrap_or(default.crop_top),
            crop_bottom: read_number(doc, &map, "crop_bottom").unwrap_or(default.crop_bottom),
        }
    }

    fn write_key(doc: &mut AutoCommit, parent: &ObjId, value: ChromaKey) {
        let map = Self::ensure_child(doc, parent, "key");
        let _ = doc.put(&map, "enabled", value.enabled);
        let _ = doc.put(&map, "red", value.color[0]);
        let _ = doc.put(&map, "green", value.color[1]);
        let _ = doc.put(&map, "blue", value.color[2]);
        let _ = doc.put(&map, "tolerance", value.tolerance);
        let _ = doc.put(&map, "softness", value.softness);
        let _ = doc.put(&map, "spill", value.spill);
    }

    fn read_key(doc: &AutoCommit, parent: &ObjId) -> ChromaKey {
        let Ok(Some((Value::Object(ObjType::Map), map))) = doc.get(parent, "key") else {
            return ChromaKey::default();
        };
        let default = ChromaKey::default();
        ChromaKey {
            enabled: read_bool(doc, &map, "enabled").unwrap_or(default.enabled),
            color: [
                read_number(doc, &map, "red").unwrap_or(default.color[0]),
                read_number(doc, &map, "green").unwrap_or(default.color[1]),
                read_number(doc, &map, "blue").unwrap_or(default.color[2]),
            ],
            tolerance: read_number(doc, &map, "tolerance").unwrap_or(default.tolerance),
            softness: read_number(doc, &map, "softness").unwrap_or(default.softness),
            spill: read_number(doc, &map, "spill").unwrap_or(default.spill),
        }
    }

    fn write_mask(doc: &mut AutoCommit, parent: &ObjId, value: Mask) {
        let map = Self::ensure_child(doc, parent, "mask");
        let _ = doc.put(&map, "shape", value.shape.as_str());
        for (key, value) in [
            ("x", value.x),
            ("y", value.y),
            ("w", value.w),
            ("h", value.h),
            ("rotation", value.rotation),
            ("feather", value.feather),
        ] {
            let _ = doc.put(&map, key, value);
        }
        let _ = doc.put(&map, "invert", value.invert);
    }

    fn read_mask(doc: &AutoCommit, parent: &ObjId) -> Mask {
        let Ok(Some((Value::Object(ObjType::Map), map))) = doc.get(parent, "mask") else {
            return Mask::default();
        };
        let default = Mask::default();
        Mask {
            shape: read_str(doc, &map, "shape")
                .map_or(default.shape, |value| MaskShape::from_name(&value)),
            x: read_number(doc, &map, "x").unwrap_or(default.x),
            y: read_number(doc, &map, "y").unwrap_or(default.y),
            w: read_number(doc, &map, "w").unwrap_or(default.w),
            h: read_number(doc, &map, "h").unwrap_or(default.h),
            rotation: read_number(doc, &map, "rotation").unwrap_or(default.rotation),
            feather: read_number(doc, &map, "feather").unwrap_or(default.feather),
            invert: read_bool(doc, &map, "invert").unwrap_or(default.invert),
        }
    }

    // Curves and keyframes define identity by their authored x/time position:
    // setting a value at the same position replaces that value, while moving
    // it is a remove-plus-insert at the new position. The IEEE bit pattern is
    // therefore a stable key for that record's lifetime; it is never inferred
    // from a receiver's list order.
    fn point_key(x: f64) -> String {
        format!("{:016x}", x.to_bits())
    }
    #[allow(
        clippy::needless_collect,
        reason = "the keys iterator borrows the document while a curve replacement removes old points"
    )]
    fn write_spline(doc: &mut AutoCommit, parent: &ObjId, spline: &Spline) {
        let points = Self::ensure_child(doc, parent, "points");
        let keep: Vec<String> = spline
            .points
            .iter()
            .map(|point| Self::point_key(point.x))
            .collect();
        for key in doc.keys(&points).collect::<Vec<_>>() {
            if !keep.contains(&key) {
                let _ = doc.delete(&points, &key);
            }
        }
        for point in &spline.points {
            let entry = Self::ensure_child(doc, &points, &Self::point_key(point.x));
            let _ = doc.put(&entry, "x", point.x);
            let _ = doc.put(&entry, "y", point.y);
        }
    }
    fn read_spline(doc: &AutoCommit, parent: &ObjId) -> Spline {
        let Ok(Some((Value::Object(ObjType::Map), points))) = doc.get(parent, "points") else {
            return Spline::default();
        };
        let mut values: Vec<CurvePoint> = doc
            .keys(&points)
            .filter_map(|key| {
                let Ok(Some((Value::Object(ObjType::Map), point))) = doc.get(&points, &key) else {
                    return None;
                };
                Some(CurvePoint::new(
                    read_number(doc, &point, "x")?,
                    read_number(doc, &point, "y")?,
                ))
            })
            .collect();
        values.sort_by(|left, right| left.x.total_cmp(&right.x));
        Spline { points: values }.normalised()
    }
    fn write_curves(doc: &mut AutoCommit, parent: &ObjId, value: &Curve) {
        let map = Self::ensure_child(doc, parent, "curves");
        let _ = doc.put(&map, "mix", value.mix);
        for channel in Channel::ALL {
            let child = Self::ensure_child(doc, &map, channel.as_str());
            Self::write_spline(doc, &child, value.spline(channel));
        }
    }
    fn read_curves(doc: &AutoCommit, parent: &ObjId) -> Curve {
        let Ok(Some((Value::Object(ObjType::Map), map))) = doc.get(parent, "curves") else {
            return Curve::default();
        };
        Curve {
            master: doc
                .get(&map, "master")
                .ok()
                .flatten()
                .map(|(_, id)| Self::read_spline(doc, &id))
                .unwrap_or_default(),
            red: doc
                .get(&map, "red")
                .ok()
                .flatten()
                .map(|(_, id)| Self::read_spline(doc, &id))
                .unwrap_or_default(),
            green: doc
                .get(&map, "green")
                .ok()
                .flatten()
                .map(|(_, id)| Self::read_spline(doc, &id))
                .unwrap_or_default(),
            blue: doc
                .get(&map, "blue")
                .ok()
                .flatten()
                .map(|(_, id)| Self::read_spline(doc, &id))
                .unwrap_or_default(),
            mix: read_number(doc, &map, "mix").unwrap_or(1.0),
        }
        .normalised()
    }

    #[allow(
        clippy::needless_collect,
        reason = "the keys iterator borrows the document while replacement removes old tracks and keyframes"
    )]
    fn write_animation(doc: &mut AutoCommit, parent: &ObjId, value: &ClipAnimation) {
        let tracks = Self::ensure_child(doc, parent, "anim");
        let keep: Vec<String> = value
            .tracks
            .iter()
            .map(|track| track.param.as_str())
            .collect();
        for key in doc.keys(&tracks).collect::<Vec<_>>() {
            if !keep.contains(&key) {
                let _ = doc.delete(&tracks, &key);
            }
        }
        for (ordinal, track) in value.tracks.iter().enumerate() {
            let entry = Self::ensure_child(doc, &tracks, &track.param.as_str());
            let _ = doc.put(&entry, "ordinal", ordinal as i64);
            let keys = Self::ensure_child(doc, &entry, "keys");
            let wanted: Vec<String> = track
                .keys
                .iter()
                .map(|key| Self::point_key(key.t))
                .collect();
            for key in doc.keys(&keys).collect::<Vec<_>>() {
                if !wanted.contains(&key) {
                    let _ = doc.delete(&keys, &key);
                }
            }
            for key in &track.keys {
                let child = Self::ensure_child(doc, &keys, &Self::point_key(key.t));
                let _ = doc.put(&child, "t", key.t);
                let _ = doc.put(&child, "v", key.v);
                let _ = doc.put(&child, "interp", key.interp.as_str());
            }
        }
    }
    fn read_animation(doc: &AutoCommit, parent: &ObjId) -> ClipAnimation {
        let Ok(Some((Value::Object(ObjType::Map), tracks))) = doc.get(parent, "anim") else {
            return ClipAnimation::default();
        };
        let mut out = Vec::new();
        for name in doc.keys(&tracks) {
            let Some(param) = Param::from_name(&name) else {
                continue;
            };
            let Ok(Some((Value::Object(ObjType::Map), track))) = doc.get(&tracks, &name) else {
                continue;
            };
            let Ok(Some((Value::Object(ObjType::Map), keys))) = doc.get(&track, "keys") else {
                continue;
            };
            let mut values: Vec<Keyframe> = doc
                .keys(&keys)
                .filter_map(|key| {
                    let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(&keys, &key)
                    else {
                        return None;
                    };
                    Some(Keyframe {
                        t: read_number(doc, &entry, "t")?,
                        v: read_number(doc, &entry, "v")?,
                        interp: read_str(doc, &entry, "interp")
                            .map_or(Interp::Linear, |value| Interp::from_name(&value)),
                    })
                })
                .collect();
            values.sort_by(|left, right| left.t.total_cmp(&right.t));
            if !values.is_empty() {
                out.push((
                    Self::get_i64(doc, &track, "ordinal").unwrap_or(i64::MAX),
                    ParamTrack {
                        param,
                        keys: values,
                    },
                ));
            }
        }
        out.sort_by(|(left_order, left), (right_order, right)| {
            left_order
                .cmp(right_order)
                .then_with(|| left.param.as_str().cmp(&right.param.as_str()))
        });
        ClipAnimation {
            tracks: out.into_iter().map(|(_, track)| track).collect(),
        }
    }

    /// Replace an effect stack together with the animation tracks whose
    /// positional encoding follows that stack. Reorder/remove/clear actions
    /// must use this pair so collaborators never observe an effect inheriting
    /// another instance's amount curve.
    pub fn set_clip_effects_and_animation(
        doc: &mut AutoCommit,
        id: Uuid,
        effects: &[ClipEffect],
        animation: &crate::param::ClipAnimation,
    ) -> bool {
        let Ok(Some((Value::Object(ObjType::Map), clips))) = doc.get(&ROOT, "clips") else {
            return false;
        };
        let Ok(Some((Value::Object(ObjType::Map), clip))) = doc.get(&clips, id.to_string()) else {
            return false;
        };
        Self::write_effects(doc, &clip, effects);
        Self::write_animation(doc, &clip, animation);
        true
    }

    /// Replace one existing clip scalar without materialising a stale clip map.
    /// The action layer owns the allowlist; this primitive only provides the
    /// narrow CRDT write used after that validation.
    pub fn set_existing_clip_scalar(
        doc: &mut AutoCommit,
        id: Uuid,
        key: &str,
        value: impl Into<ScalarValue>,
    ) -> bool {
        Self::existing_clip(doc, id).is_some_and(|clip| doc.put(&clip, key, value).is_ok())
    }

    /// Typed compound writes used by current actions. None of them stores
    /// JSON in the document.
    pub fn set_existing_clip_transform(doc: &mut AutoCommit, id: Uuid, value: Transform) -> bool {
        let Some(clip) = Self::existing_clip(doc, id) else {
            return false;
        };
        Self::write_transform(doc, &clip, value);
        true
    }

    pub fn set_existing_clip_curves(doc: &mut AutoCommit, id: Uuid, value: &Curve) -> bool {
        let Some(clip) = Self::existing_clip(doc, id) else {
            return false;
        };
        Self::write_curves(doc, &clip, value);
        true
    }

    pub fn set_existing_clip_key(doc: &mut AutoCommit, id: Uuid, value: ChromaKey) -> bool {
        let Some(clip) = Self::existing_clip(doc, id) else {
            return false;
        };
        Self::write_key(doc, &clip, value);
        true
    }

    pub fn set_existing_clip_mask(doc: &mut AutoCommit, id: Uuid, value: Mask) -> bool {
        let Some(clip) = Self::existing_clip(doc, id) else {
            return false;
        };
        Self::write_mask(doc, &clip, value);
        true
    }

    pub fn set_existing_clip_animation(
        doc: &mut AutoCommit,
        id: Uuid,
        value: &ClipAnimation,
    ) -> bool {
        let Some(clip) = Self::existing_clip(doc, id) else {
            return false;
        };
        Self::write_animation(doc, &clip, value);
        true
    }

    pub fn set_existing_clip_markers(doc: &mut AutoCommit, id: Uuid, value: &[Marker]) -> bool {
        let Some(clip) = Self::existing_clip(doc, id) else {
            return false;
        };
        Self::write_markers(doc, &clip, value);
        true
    }

    /// Replace or clear the membership key of an existing clip. `None` is a
    /// deletion rather than an empty sentinel, preserving the document's
    /// compatibility meaning for projects predating linked clips.
    pub fn set_existing_clip_link(doc: &mut AutoCommit, id: Uuid, value: Option<&str>) -> bool {
        let Some(clip) = Self::existing_clip(doc, id) else {
            return false;
        };
        match value {
            Some(value) => doc.put(&clip, "link", value).is_ok(),
            None => doc.delete(&clip, "link").is_ok(),
        }
    }

    pub fn remove_clip(doc: &mut AutoCommit, id: Uuid) {
        let clips = Self::map(doc, "clips");
        let _ = doc.delete(&clips, id.to_string().as_str());
    }

    pub fn remove_media(doc: &mut AutoCommit, id: Uuid) {
        let media = Self::map(doc, "media");
        let _ = doc.delete(&media, id.to_string().as_str());
    }

    pub fn remove_track(doc: &mut AutoCommit, id: Uuid) {
        let tracks = Self::map(doc, "tracks");
        let _ = doc.delete(&tracks, id.to_string().as_str());
    }

    /// Mirror an entire in-memory project into the document. Used to seed
    /// the sample project and to push a freshly-loaded project back so
    /// new clips (e.g. a split) replicate.
    pub fn write_project(doc: &mut AutoCommit, p: &Project) {
        Self::write_project_meta(doc, p);
        let project = Self::map(doc, "project");
        Self::write_markers(doc, &project, &p.markers);
        for m in &p.media {
            Self::upsert_media(doc, m);
        }
        for t in &p.tracks {
            Self::upsert_track(doc, t);
        }
        for c in &p.clips {
            Self::upsert_clip(doc, c);
        }
    }

    /// Every key currently present in the root map `key`.
    fn keys_of(doc: &AutoCommit, key: &str) -> Vec<String> {
        match doc.get(&ROOT, key) {
            Ok(Some((Value::Object(ObjType::Map), id))) => doc.keys(&id).collect(),
            _ => Vec::new(),
        }
    }

    /// Make the document hold **exactly** `p` — the write behind opening a
    /// project file.
    ///
    /// The difference from [`write_project`](Self::write_project) is the
    /// removals: anything in the document that `p` does not have goes. Without
    /// that, opening a second project would *merge* it into the first and the
    /// user would get both timelines at once.
    ///
    /// Entries `p` also has are upserted rather than deleted and recreated.
    /// That keeps re-opening the project you already have open close to a
    /// no-op, and — because the keys are stable ids — it is what lets two
    /// peers who open the same file converge on one project instead of two
    /// copies of it.
    pub fn replace_project(doc: &mut AutoCommit, p: &Project) {
        let keep_media: Vec<String> = p.media.iter().map(|m| m.id.to_string()).collect();
        let keep_tracks: Vec<String> = p.tracks.iter().map(|t| t.id.to_string()).collect();
        let keep_clips: Vec<String> = p.clips.iter().map(|c| c.id.to_string()).collect();

        for (map_key, keep) in [
            ("media", &keep_media),
            ("tracks", &keep_tracks),
            ("clips", &keep_clips),
        ] {
            let obj = Self::map(doc, map_key);
            for existing in Self::keys_of(doc, map_key) {
                if !keep.contains(&existing) {
                    let _ = doc.delete(&obj, existing.as_str());
                }
            }
        }

        Self::write_project(doc, p);
    }

    // --- reader ----------------------------------------------------------

    /// Project the whole document back into the in-memory model. Returns a
    /// default empty project if nothing has been seeded.
    pub fn load_project(doc: &AutoCommit) -> Project {
        let mut p = Project::new("Untitled");
        if let Ok(Some((Value::Object(ObjType::Map), proj))) = doc.get(&ROOT, "project") {
            if let Some(n) = read_str(doc, &proj, "name") {
                p.name = n;
            }
            p.fps = read_number(doc, &proj, "fps").unwrap_or(30.0);
            p.width = Self::get_i64(doc, &proj, "width").unwrap_or(1920) as u32;
            p.height = Self::get_i64(doc, &proj, "height").unwrap_or(1080) as u32;
            p.format_explicit = read_bool(doc, &proj, "format_explicit").unwrap_or(false);
            p.mark_in = read_number(doc, &proj, "mark_in");
            p.mark_out = read_number(doc, &proj, "mark_out");
            p.markers = Self::read_markers(doc, &proj);
        }
        Self::read_media_into(doc, &mut p);
        Self::read_tracks_into(doc, &mut p);
        Self::read_clips_into(doc, &mut p);
        Self::in_a_settled_order(&mut p);
        p
    }

    /// Append every media item in the document.
    fn read_media_into(doc: &AutoCommit, p: &mut Project) {
        if let Ok(Some((Value::Object(ObjType::Map), media))) = doc.get(&ROOT, "media") {
            for key in doc.keys(&media) {
                let Ok(id) = key.parse::<Uuid>() else {
                    continue;
                };
                let Ok(Some((Value::Object(ObjType::Map), obj))) = doc.get(&media, key.as_str())
                else {
                    continue;
                };
                let name = read_str(doc, &obj, "name").unwrap_or_default();
                let path = read_str(doc, &obj, "path")
                    .unwrap_or_else(|| crate::media::workspace_path(&name));
                p.media.push(MediaItem {
                    id,
                    name,
                    kind: MediaKind::from_name(&read_str(doc, &obj, "kind").unwrap_or_default()),
                    duration: read_number(doc, &obj, "duration").unwrap_or(0.0),
                    width: Self::get_i64(doc, &obj, "width").unwrap_or(0) as u32,
                    height: Self::get_i64(doc, &obj, "height").unwrap_or(0) as u32,
                    fps: read_number(doc, &obj, "fps").unwrap_or(30.0),
                    palette: Self::get_i64(doc, &obj, "palette").unwrap_or(0) as u8,
                    path,
                    hydration: crate::media::Hydration::from_name(
                        &read_str(doc, &obj, "hydration").unwrap_or_default(),
                    ),
                    // Absent for every decoded source, and for any document
                    // written before titles existed.
                    text: read_str(doc, &obj, "text").and_then(|s| serde_json::from_str(&s).ok()),
                    // Absent in any document written before the mixer could
                    // find a video file's sound. False is the honest default:
                    // it reads as "no sound track was found".
                    has_audio: read_bool(doc, &obj, "has_audio").unwrap_or(false),
                });
            }
        }
    }

    /// Append every track in the document.
    fn read_tracks_into(doc: &AutoCommit, p: &mut Project) {
        if let Ok(Some((Value::Object(ObjType::Map), tracks))) = doc.get(&ROOT, "tracks") {
            for key in doc.keys(&tracks) {
                let Ok(id) = key.parse::<Uuid>() else {
                    continue;
                };
                let Ok(Some((Value::Object(ObjType::Map), obj))) = doc.get(&tracks, key.as_str())
                else {
                    continue;
                };
                p.tracks.push(Track {
                    id,
                    name: read_str(doc, &obj, "name").unwrap_or_default(),
                    kind: TrackKind::from_name(&read_str(doc, &obj, "kind").unwrap_or_default()),
                    index: Self::get_i64(doc, &obj, "index").unwrap_or(0) as u32,
                    muted: read_bool(doc, &obj, "muted").unwrap_or(false),
                    locked: read_bool(doc, &obj, "locked").unwrap_or(false),
                    // Absent in documents written before solo existed, which
                    // reads as false — the right default.
                    solo: read_bool(doc, &obj, "solo").unwrap_or(false),
                    // Absent in every document written before the lane had a
                    // fader. `0.0` is unity and centre, so such a project
                    // plays back at exactly the level it always did rather
                    // than arriving silent or hard-panned.
                    gain_db: read_number(doc, &obj, "gain_db").unwrap_or(0.0),
                    pan: read_number(doc, &obj, "pan").unwrap_or(0.0),
                });
            }
        }
    }

    /// Append every clip whose track and media ids parse.
    fn read_clips_into(doc: &AutoCommit, p: &mut Project) {
        if let Ok(Some((Value::Object(ObjType::Map), clips))) = doc.get(&ROOT, "clips") {
            for key in doc.keys(&clips) {
                let Ok(id) = key.parse::<Uuid>() else {
                    continue;
                };
                let Ok(Some((Value::Object(ObjType::Map), obj))) = doc.get(&clips, key.as_str())
                else {
                    continue;
                };
                let track = read_str(doc, &obj, "track").and_then(|s| s.parse::<Uuid>().ok());
                let media = read_str(doc, &obj, "media").and_then(|s| s.parse::<Uuid>().ok());
                let (Some(track), Some(media)) = (track, media) else {
                    continue;
                };
                let clip = Clip {
                    id,
                    track,
                    media,
                    start: read_number(doc, &obj, "start").unwrap_or(0.0),
                    in_point: read_number(doc, &obj, "in").unwrap_or(0.0),
                    out_point: read_number(doc, &obj, "out").unwrap_or(0.0),
                    opacity: read_number(doc, &obj, "opacity").unwrap_or(1.0),
                    volume: read_number(doc, &obj, "volume").unwrap_or(1.0),
                    transition: Transition::from_name(
                        &read_str(doc, &obj, "transition").unwrap_or_default(),
                    ),
                    palette: Self::get_i64(doc, &obj, "palette").unwrap_or(0) as u8,
                    filters: Filters {
                        brightness: read_number(doc, &obj, "brightness").unwrap_or(0.5),
                        contrast: read_number(doc, &obj, "contrast").unwrap_or(0.5),
                        saturation: read_number(doc, &obj, "saturation").unwrap_or(0.5),
                    },
                    fade_in: read_number(doc, &obj, "fade_in").unwrap_or(0.0),
                    fade_out: read_number(doc, &obj, "fade_out").unwrap_or(0.0),
                    // Absent before the field existed; 0.0 means "default"
                    // (see Clip::transition_len), not "instant cut".
                    transition_duration: read_number(doc, &obj, "transition_duration")
                        .unwrap_or(0.0),
                    effects: Self::read_effects(doc, &obj),
                    // Each of these is absent in a document written before it
                    // existed, and each default restores the old behaviour:
                    // identity geometry, normal speed, no key, no mask, no
                    // curves, no markers.
                    transform: Self::read_transform(doc, &obj),
                    speed: crate::model::Speed {
                        rate: read_number(doc, &obj, "speed_rate").unwrap_or(1.0),
                        reverse: read_bool(doc, &obj, "speed_reverse").unwrap_or(false),
                    },
                    curves: Self::read_curves(doc, &obj),
                    key: Self::read_key(doc, &obj),
                    mask: Self::read_mask(doc, &obj),
                    anim: Self::read_animation(doc, &obj),
                    markers: Self::read_markers(doc, &obj),
                    // Absent in every document written before links existed,
                    // which reads as "not linked" -- the independent clip that
                    // peer is already editing.
                    link: read_str(doc, &obj, "link").and_then(|s| s.parse::<Uuid>().ok()),
                    // Absent before the sound could be split off, which reads
                    // as "this clip still carries its own" -- what that peer
                    // is already playing.
                    sound_split: read_bool(doc, &obj, "sound_split").unwrap_or(false),
                    // Absent in any document written before unlinking kept
                    // the group. True there, because a clip that has a group
                    // at all was linked when it was written.
                    link_follows: read_bool(doc, &obj, "link_follows").unwrap_or(true),
                };
                p.clips.push(clip);
            }
        }
    }

    /// Put a freshly-loaded project's collections into an order that does not
    /// depend on how the document happened to be keyed.
    ///
    /// Automerge hands a map's entries back in key order, and every id here is
    /// a UUID minted when the project was made, so for a project seeded in
    /// this process that order is re-rolled on every run. Left as it arrived,
    /// one project loaded twice put its tracks, clips and media in two
    /// different orders -- and a timeline drawn in a different order is a
    /// different picture. That is what made two `--shots` captures of one
    /// scenario impossible to diff: same shapes, same count, shuffled.
    ///
    /// Ordering is by what the thing *is* rather than by its id: a track by
    /// the lane it occupies, a clip by where it sits on that lane, a source by
    /// its name. Two peers that agree about the edit therefore agree about the
    /// order, which an id-based sort could not promise.
    fn in_a_settled_order(p: &mut Project) {
        p.tracks
            .sort_by(|a, b| a.index.cmp(&b.index).then_with(|| a.name.cmp(&b.name)));
        let lane: std::collections::HashMap<Uuid, usize> = p
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id, i))
            .collect();
        p.clips.sort_by(|a, b| {
            lane.get(&a.track)
                .unwrap_or(&usize::MAX)
                .cmp(lane.get(&b.track).unwrap_or(&usize::MAX))
                .then_with(|| a.start.total_cmp(&b.start))
                .then_with(|| a.in_point.total_cmp(&b.in_point))
        });
        p.media.sort_by(|a, b| a.name.cmp(&b.name));
    }

    /// Build the demo project so the editor opens populated (and `--shots`
    /// renders a real edit, not an empty canvas), then seed it into `doc`.
    pub fn seed_sample(doc: &mut AutoCommit) {
        let p = sample_project();
        Self::write_project(doc, &p);
    }
}

/// Named parameters for a deterministic demo clip placement. Source and
/// timeline times sit beside normalized values, so positional arguments would
/// make accidental transposition far too easy.
struct DemoPlacement {
    track: Uuid,
    media: Uuid,
    start: f64,
    source: std::ops::Range<f64>,
    palette: u8,
    opacity: f64,
    volume: f64,
    transition: Transition,
    filters: Filters,
    fade_in: f64,
    fade_out: f64,
}

impl DemoPlacement {
    fn new(
        track: Uuid,
        media: Uuid,
        start: f64,
        source: std::ops::Range<f64>,
        palette: u8,
    ) -> Self {
        Self {
            track,
            media,
            start,
            source,
            palette,
            opacity: 1.0,
            volume: 1.0,
            transition: Transition::None,
            filters: Filters::default(),
            fade_in: 0.0,
            fade_out: 0.0,
        }
    }

    fn transition(mut self, transition: Transition) -> Self {
        self.transition = transition;
        self
    }

    fn opacity(mut self, opacity: f64) -> Self {
        self.opacity = opacity;
        self
    }

    fn volume(mut self, volume: f64) -> Self {
        self.volume = volume;
        self
    }

    fn filters(mut self, filters: Filters) -> Self {
        self.filters = filters;
        self
    }

    fn fades(mut self, fade_in: f64, fade_out: f64) -> Self {
        self.fade_in = fade_in;
        self.fade_out = fade_out;
        self
    }

    fn into_clip(self) -> Clip {
        Clip {
            id: Uuid::new_v4(),
            track: self.track,
            media: self.media,
            start: self.start,
            in_point: self.source.start,
            out_point: self.source.end,
            opacity: self.opacity,
            volume: self.volume,
            transition: self.transition,
            palette: self.palette,
            filters: self.filters,
            fade_in: self.fade_in,
            fade_out: self.fade_out,
            transition_duration: 0.0,
            effects: Vec::new(),
            ..Default::default()
        }
    }
}

struct DemoMedia {
    drone: Uuid,
    interview: Uuid,
    market: Uuid,
    sunset: Uuid,
    logo: Uuid,
    voiceover: Uuid,
    pad: Uuid,
}

struct DemoTracks {
    video_main: Uuid,
    video_overlay: Uuid,
    audio_voiceover: Uuid,
    audio_bed: Uuid,
}

fn demo_clips(media: &DemoMedia, tracks: &DemoTracks) -> Vec<Clip> {
    let graded = Filters {
        brightness: 0.62,
        contrast: 0.64,
        saturation: 0.58,
    };
    let placements = [
        DemoPlacement::new(tracks.video_main, media.drone, 0.0, 2.0..10.0, 0)
            .transition(Transition::FadeIn)
            .filters(graded)
            .fades(1.5, 0.0),
        DemoPlacement::new(tracks.video_main, media.interview, 8.0, 5.0..17.0, 1)
            .transition(Transition::CrossDissolve),
        DemoPlacement::new(tracks.video_main, media.market, 20.0, 0.0..6.0, 2).fades(0.0, 1.0),
        DemoPlacement::new(tracks.video_overlay, media.logo, 1.0, 0.0..4.0, 4)
            .transition(Transition::FadeIn)
            .opacity(0.9),
        DemoPlacement::new(tracks.video_overlay, media.sunset, 21.0, 0.0..6.0, 3)
            .transition(Transition::CrossDissolve)
            .opacity(0.85)
            .fades(0.0, 1.2),
        DemoPlacement::new(tracks.audio_voiceover, media.voiceover, 8.0, 1.5..13.5, 5)
            .fades(0.3, 0.5),
        DemoPlacement::new(tracks.audio_bed, media.pad, 0.0, 0.0..28.0, 6)
            .transition(Transition::FadeIn)
            .volume(0.4)
            .fades(2.0, 3.0),
    ];
    let mut clips: Vec<_> = placements
        .into_iter()
        .map(DemoPlacement::into_clip)
        .collect();

    // Stacked effects make the demo exercise the effects panel as well as
    // timeline placement and transitions.
    if let Some(drone) = clips.first_mut() {
        drone.add_effect(EffectKind::Warm);
        drone.add_effect(EffectKind::Vignette);
    }
    clips
}

/// A short documentary-style cut: drone B-roll under an interview, an
/// overlaid logo and a sunset insert with a dissolve, narration on A1 and
/// an ambient bed on A2. Built deterministically so screenshots are
/// stable across runs.
pub fn sample_project() -> Project {
    let mut p = Project::new("Coastline_Doc_v3");
    // A cut this far along has a format: it is 1080p at 30, and the next clip
    // to land on it must not re-shape the timeline it is already cut against.
    p.set_frame_size(1920, 1080);
    p.set_fps(30.0);

    let mk = |name: &str, kind: MediaKind, dur: f64, w: u32, h: u32, fps: f64, pal: u8| {
        MediaItem {
            id: Uuid::new_v4(),
            name: name.into(),
            kind,
            // Sample media stands in for footage that was never probed.
            has_audio: false,
            duration: dur,
            width: w,
            height: h,
            fps,
            palette: pal,
            // Sample media lives under the workspace media prefix; the
            // bundled sample-media/ dir hydrates these for --shots.
            path: media::workspace_path(name),
            hydration: Hydration::Projected,
            text: None,
        }
    };

    let drone = mk(
        "Drone_Coastline.mov",
        MediaKind::Video,
        24.0,
        3840,
        2160,
        24.0,
        0,
    );
    let interview = mk(
        "Interview_A.mp4",
        MediaKind::Video,
        38.5,
        1920,
        1080,
        30.0,
        1,
    );
    let market = mk(
        "Cutaway_Market.mp4",
        MediaKind::Video,
        12.0,
        1920,
        1080,
        30.0,
        2,
    );
    let sunset = mk(
        "Sunset_Timelapse.mp4",
        MediaKind::Video,
        9.0,
        3840,
        2160,
        30.0,
        3,
    );
    let logo = mk("Logo_Bug.png", MediaKind::Image, 6.0, 800, 800, 30.0, 4);
    let vo = mk("VO_Take3.wav", MediaKind::Audio, 32.0, 0, 0, 0.0, 5);
    let pad = mk("Ambient_Pad.wav", MediaKind::Audio, 60.0, 0, 0, 0.0, 6);

    let media = DemoMedia {
        drone: drone.id,
        interview: interview.id,
        market: market.id,
        sunset: sunset.id,
        logo: logo.id,
        voiceover: vo.id,
        pad: pad.id,
    };
    p.media = vec![drone, interview, market, sunset, logo, vo, pad];

    let mk_track = |name: &str, kind: TrackKind, index: u32| Track {
        id: Uuid::new_v4(),
        name: name.into(),
        kind,
        index,
        muted: false,
        locked: false,
        solo: false,
        gain_db: 0.0,
        pan: 0.0,
    };
    let v1 = mk_track("V1", TrackKind::Video, 1);
    let v2 = mk_track("V2", TrackKind::Video, 2);
    let a1 = mk_track("A1", TrackKind::Audio, 1);
    let a2 = mk_track("A2", TrackKind::Audio, 2);
    let tracks = DemoTracks {
        video_main: v1.id,
        video_overlay: v2.id,
        audio_voiceover: a1.id,
        audio_bed: a2.id,
    };
    p.tracks = vec![v1, v2, a1, a2];
    p.clips = demo_clips(&media, &tracks);

    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_automerge::ApplyIncoming;

    use crate::model::{Speed, TextAlign, TextSource, Transform};
    use crate::param::{Interp, Param};

    /// Write `p` and read it straight back — the round-trip every peer
    /// makes, and the one an autosaved project makes on reopen.
    fn round_trip(p: &Project) -> Project {
        let mut doc = AutoCommit::new();
        VideoDoc::write_project(&mut doc, p);
        VideoDoc::load_project(&doc)
    }

    /// A project with one clip, so a field can be set on it and chased
    /// through the document.
    fn one_clip() -> (Project, Uuid) {
        let mut p = Project::new("rt");
        let mid = Uuid::new_v4();
        p.media.push(MediaItem {
            id: mid,
            name: "v.mp4".into(),
            kind: MediaKind::Video,
            duration: 30.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            path: "media/v.mp4".into(),
            hydration: Hydration::Local,
            ..Default::default()
        });
        let tid = Uuid::new_v4();
        p.tracks.push(crate::model::Track {
            id: tid,
            name: "V1".into(),
            index: 1,
            ..Default::default()
        });
        let cid = p.add_clip(tid, mid, 0.0).expect("media exists");
        (p, cid)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    #[expect(
        clippy::float_cmp,
        reason = "a round trip through the document stores the same f64 bits it was given"
    )]
    fn clip_grading_and_geometry_survive_the_document() {
        // Case `a_key_and_a_mask_survive_the_document`.
        {
            use crate::model::{ChromaKey, Mask, MaskShape};
            let (mut p, cid) = one_clip();
            let key = ChromaKey {
                enabled: true,
                color: [0.11, 0.68, 0.24],
                tolerance: 0.27,
                softness: 0.09,
                spill: 0.63,
            };
            let mask = Mask {
                shape: MaskShape::Ellipse,
                x: -0.18,
                y: 0.24,
                w: 0.41,
                h: 0.77,
                rotation: -22.0,
                feather: 0.31,
                invert: true,
            };
            {
                let c = p.clip_mut(cid).unwrap();
                c.key = key;
                c.mask = mask;
            }
            let back = round_trip(&p);
            let c = back.clip(cid).unwrap();
            assert_eq!(c.key, key, "every field of the key, not just the switch");
            assert_eq!(c.mask, mask, "and every field of the mask");
        }
        // Case `a_tone_curve_survives_the_document_with_every_channel`.
        {
            use crate::curve::Curve;
            let (mut p, cid) = one_clip();
            let mut want = Curve::default();
            // One point on each channel, all at different heights, so a bake
            // that dropped or transposed a channel cannot come back equal.
            want.master.set_point(0.5, 0.72);
            want.red.set_point(0.3, 0.44);
            want.green.set_point(0.6, 0.21);
            want.blue.set_point(0.8, 0.93);
            want.mix = 0.65;
            p.clip_mut(cid).unwrap().curves = want.clone();

            let back = round_trip(&p);
            let c = back.clip(cid).unwrap();
            assert_eq!(
                c.curves, want,
                "every channel and the amount, not just the master"
            );
        }
        // Case `a_clip_written_before_curves_existed_reads_as_ungraded`.
        {
            // The `curves` key is simply absent in such a document, and the
            // default has to be the straight line — a curve that read back as
            // anything else would re-grade footage nobody touched.
            let (p, cid) = one_clip();
            let back = round_trip(&p);
            let c = back.clip(cid).unwrap();
            assert!(
                c.curves.is_identity(),
                "an untouched clip comes back ungraded"
            );
        }
        // Case `a_transform_survives_the_document`.
        {
            let (mut p, cid) = one_clip();
            let want = Transform {
                scale_x: 0.4,
                scale_y: 0.55,
                pos_x: -0.2,
                pos_y: 0.31,
                rotation: -14.5,
                anchor_x: 0.1,
                anchor_y: 0.9,
                crop_left: 0.05,
                crop_right: 0.06,
                crop_top: 0.07,
                crop_bottom: 0.08,
            };
            p.clip_mut(cid).unwrap().transform = want;
            let back = round_trip(&p);
            assert_eq!(back.clip(cid).unwrap().transform, want);
        }
        // Case `speed_survives_the_document_in_both_directions`.
        {
            let (mut p, cid) = one_clip();
            p.clip_mut(cid).unwrap().speed = Speed::new(2.5, true);
            let back = round_trip(&p);
            let got = back.clip(cid).unwrap();
            assert_eq!(got.speed, Speed::new(2.5, true));
            assert_eq!(got.duration(), p.clip(cid).unwrap().duration());
        }
        // Case `keyframe_curves_survive_the_document_with_their_interpolation`.
        {
            let (mut p, cid) = one_clip();
            {
                let c = p.clip_mut(cid).unwrap();
                c.set_key_at(Param::Opacity, 0.0, 0.0, Interp::Ease);
                c.set_key_at(Param::Opacity, 3.0, 1.0, Interp::Hold);
                c.set_key_at(Param::PosX, 1.0, -0.5, Interp::Linear);
            }
            let back = round_trip(&p);
            let got = back.clip(cid).unwrap();
            assert!(got.anim.is_animated(Param::Opacity));
            assert!(got.anim.is_animated(Param::PosX));
            let keys = &got.anim.track(Param::Opacity).unwrap().keys;
            assert_eq!(keys.len(), 2);
            assert_eq!(
                keys[0].interp,
                Interp::Ease,
                "the shape of the curve comes back too"
            );
            assert_eq!(keys[1].interp, Interp::Hold);
            // And the sampled value is identical, which is what actually matters.
            for t in [0.0, 0.7, 1.5, 2.9, 5.0] {
                assert_eq!(
                    got.param_at(Param::Opacity, t),
                    p.clip(cid).unwrap().param_at(Param::Opacity, t),
                    "the curve reads the same at {t}s",
                );
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    #[expect(
        clippy::float_cmp,
        reason = "a round trip through the document stores the same f64 bits it was given"
    )]
    fn markers_titles_and_links_survive_and_replicate() {
        // Case `markers_survive_on_both_the_project_and_the_clip`.
        {
            let (mut p, cid) = one_clip();
            p.add_marker(4.0, "act two", 3);
            p.clip_mut(cid)
                .unwrap()
                .add_marker(2.0, "beat", 5)
                .expect("inside");
            let back = round_trip(&p);
            assert_eq!(back.markers.len(), 1);
            assert_eq!(back.markers[0].name, "act two");
            assert_eq!(back.markers[0].palette, 3);
            let cm = &back.clip(cid).unwrap().markers;
            assert_eq!(cm.len(), 1);
            assert_eq!(cm[0].name, "beat");
            assert_eq!(cm[0].t, 2.0, "still clip-local");
        }
        // Case `a_title_survives_the_document_with_its_styling`.
        {
            let mut p = Project::new("t");
            let mid = Uuid::new_v4();
            let text = TextSource {
                content: "Two\nlines".into(),
                size: 0.22,
                color: [0.9, 0.1, 0.4, 0.8],
                align: TextAlign::Right,
                font: "Some Family".into(),
                bold: false,
                italic: true,
                backdrop: 0.35,
            };
            p.media.push(MediaItem {
                id: mid,
                name: "Title".into(),
                kind: MediaKind::Text,
                duration: 5.0,
                text: Some(text.clone()),
                ..Default::default()
            });
            let back = round_trip(&p);
            let m = back.media(mid).expect("the title is in the bin");
            assert_eq!(m.kind, MediaKind::Text);
            assert_eq!(m.text.as_ref(), Some(&text));
        }
        // Case `clearing_a_title_replicates_as_a_change_rather_than_as_silence`.
        {
            // Turning a title back into a plain reference has to *remove* the
            // text, or a peer that already has it would keep drawing it.
            let mut p = Project::new("t");
            let mid = Uuid::new_v4();
            p.media.push(MediaItem {
                id: mid,
                name: "Title".into(),
                kind: MediaKind::Text,
                text: Some(TextSource::new("gone soon")),
                ..Default::default()
            });
            let mut doc = AutoCommit::new();
            VideoDoc::write_project(&mut doc, &p);
            assert!(
                VideoDoc::load_project(&doc)
                    .media(mid)
                    .unwrap()
                    .text
                    .is_some()
            );

            p.media[0].text = None;
            p.media[0].kind = MediaKind::Video;
            VideoDoc::upsert_media(&mut doc, &p.media[0]);
            assert_eq!(
                VideoDoc::load_project(&doc).media(mid).unwrap().text,
                None,
                "the title's text is gone from the document, not merely ignored",
            );
        }
        // Case `a_link_survives_the_document_and_breaking_it_replicates`.
        {
            let (mut p, cid) = one_clip();
            let group = Uuid::new_v4();
            {
                let c = p.clip_mut(cid).unwrap();
                c.link = Some(group);
                c.sound_split = true;
            }
            let back = round_trip(&p);
            let c = back.clip(cid).expect("the clip");
            assert_eq!(c.link, Some(group), "the group id replicates");
            assert!(c.sound_split, "and so does where the sound comes from");

            // Unlinking has to *remove* the key, or a peer that already holds the
            // link would keep drawing the pair and keep moving both halves.
            let mut doc = AutoCommit::new();
            VideoDoc::write_project(&mut doc, &p);
            let mut cut = p.clone();
            cut.unlink_clips(&[cid]);
            VideoDoc::upsert_clip(&mut doc, cut.clip(cid).expect("the clip"));
            // The claim is about the *peer*, not about which key carries it: a
            // reader of this document must stop following the link. It used to be
            // spelled "the link key is gone"; the group id now survives a break
            // so the editor can still find the sound a shot was split from, and
            // `link_follows` is what a peer reads instead.
            let seen = VideoDoc::load_project(&doc);
            assert!(
                !seen.is_linked(cid),
                "an unlink that writes nothing is an unlink a peer never sees",
            );
            assert!(
                seen.link_partners(cid).is_empty(),
                "a peer that still moves both halves has not seen the break",
            );
        }
        // Case `a_clip_written_before_links_existed_reads_as_an_ordinary_clip`.
        {
            // The compatibility half: no `link` key and no `sound_split` key must
            // mean "not linked, and still carrying its own sound" -- which is what
            // that document's author is already playing.
            let (p, cid) = one_clip();
            let mut doc = AutoCommit::new();
            VideoDoc::write_project(&mut doc, &p);
            let clips = VideoDoc::map(&mut doc, "clips");
            let obj = VideoDoc::ensure_child(&mut doc, &clips, &cid.to_string());
            let _ = doc.delete(&obj, "link");
            let _ = doc.delete(&obj, "sound_split");
            let c = VideoDoc::load_project(&doc)
                .clip(cid)
                .expect("the clip")
                .clone();
            assert!(c.link.is_none());
            assert!(
                !c.sound_split,
                "an old project's picture clip has to keep carrying its sound",
            );
        }
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "a round trip through the document stores the same f64 bits it was given"
    )]
    fn sparse_and_empty_documents_load_and_classify_correctly() {
        // Case `a_document_written_before_any_of_this_still_loads_unchanged`.
        {
            // The compatibility contract: every new key is absent in an older
            // document, and every default is the behaviour that document had.
            let (p, cid) = one_clip();
            let mut doc = AutoCommit::new();
            VideoDoc::write_project(&mut doc, &p);
            // Strip the new keys, leaving exactly what an older peer wrote.
            let clips = VideoDoc::map(&mut doc, "clips");
            let obj = VideoDoc::ensure_child(&mut doc, &clips, &cid.to_string());
            for key in [
                "transform",
                "speed_rate",
                "speed_reverse",
                "anim",
                "markers",
                "key",
                "mask",
            ] {
                let _ = doc.delete(&obj, key);
            }
            let media = VideoDoc::map(&mut doc, "media");
            let mobj = VideoDoc::ensure_child(&mut doc, &media, &p.media[0].id.to_string());
            let _ = doc.delete(&mobj, "text");
            let proj = VideoDoc::map(&mut doc, "project");
            let _ = doc.delete(&proj, "markers");

            let back = VideoDoc::load_project(&doc);
            let c = back.clip(cid).expect("the clip still loads");
            assert!(
                c.transform.is_identity(),
                "no geometry means the aspect-fit placement"
            );
            assert!(c.speed.is_normal(), "no speed means normal speed");
            assert!(c.anim.is_empty(), "no curves");
            assert!(!c.key.is_active(), "no key means nothing is keyed out");
            assert!(
                !c.mask.is_active(),
                "and no mask means the layer is not cut"
            );
            assert!(c.markers.is_empty());
            assert!(back.markers.is_empty());
            assert_eq!(back.media(p.media[0].id).unwrap().text, None);
            assert_eq!(
                c.duration(),
                p.clip(cid).unwrap().duration(),
                "and the same length"
            );
        }
        // Case `a_document_holding_only_imported_media_is_not_empty`.
        {
            // Import writes the `media` map and nothing else. A document in that
            // state holds real user content, so it must never report empty — that
            // is what let a demo seed run over an imported clip.
            let mut doc = AutoCommit::new();
            assert!(VideoDoc::is_empty(&doc), "a fresh document is empty");
            VideoDoc::upsert_media(
                &mut doc,
                &MediaItem {
                    id: Uuid::new_v4(),
                    name: "Imported.mp4".into(),
                    kind: MediaKind::Video,
                    duration: 12.0,
                    width: 1920,
                    height: 1080,
                    fps: 25.0,
                    palette: 0,
                    path: "media/Imported.mp4".into(),
                    hydration: Hydration::Local,
                    ..Default::default()
                },
            );
            assert!(
                !VideoDoc::is_empty(&doc),
                "a document with imported media is not empty"
            );
            // The project map is still absent — emptiness no longer keys on it alone.
            assert!(
                !matches!(
                    doc.get(&ROOT, "project"),
                    Ok(Some((Value::Object(ObjType::Map), _)))
                ),
                "import wrote no project map"
            );
        }
        // Case `an_untouched_map_is_not_content`.
        {
            // `map()` materialises an empty map as a side effect of any read-modify
            // helper; an empty map is structure, not content, so the document is
            // still empty.
            let mut doc = AutoCommit::new();
            let _ = VideoDoc::map(&mut doc, "media");
            let _ = VideoDoc::map(&mut doc, "project");
            assert!(VideoDoc::is_empty(&doc), "empty maps carry no content");
        }
        // Case `sample_round_trips_through_doc`.
        {
            let mut doc = AutoCommit::new();
            assert!(VideoDoc::is_empty(&doc));
            VideoDoc::seed_sample(&mut doc);
            assert!(!VideoDoc::is_empty(&doc));

            let p = VideoDoc::load_project(&doc);
            assert_eq!(p.name, "Coastline_Doc_v3");
            assert_eq!(p.media.len(), 7);
            assert_eq!(p.tracks.len(), 4);
            assert_eq!(p.clips.len(), 7);
            // A clip's source window survived the round-trip.
            let interview = p
                .media
                .iter()
                .find(|m| m.name == "Interview_A.mp4")
                .unwrap();
            let clip = p.clips.iter().find(|c| c.media == interview.id).unwrap();
            assert!((clip.duration() - 12.0).abs() < 1e-9);
            assert_eq!(clip.transition, Transition::CrossDissolve);
        }
    }

    #[test]
    fn effect_stacks_round_trip_with_deterministic_ids() {
        // Case `effects_stack_round_trips_through_the_doc`.
        {
            use crate::model::{ClipEffect, EffectKind};
            let mut doc = AutoCommit::new();
            VideoDoc::seed_sample(&mut doc);
            let mut p = VideoDoc::load_project(&doc);
            let cid = p.clips[0].id;
            {
                let c = p.clip_mut(cid).unwrap();
                c.effects = vec![
                    ClipEffect {
                        id: Uuid::new_v4(),
                        kind: EffectKind::Sepia,
                        amount: 0.6,
                    },
                    ClipEffect {
                        id: Uuid::new_v4(),
                        kind: EffectKind::Invert,
                        amount: 1.0,
                    },
                ];
            }
            VideoDoc::upsert_clip(&mut doc, p.clip(cid).unwrap());
            let reload = VideoDoc::load_project(&doc);
            let effects = &reload.clip(cid).unwrap().effects;
            assert_eq!(
                effects.len(),
                2,
                "the effects stack survived the round-trip in order"
            );
            assert_eq!(effects[0].kind, EffectKind::Sepia);
            assert!((effects[0].amount - 0.6).abs() < 1e-9);
            assert_eq!(effects[1].kind, EffectKind::Invert);
        }
    }

    #[test]
    fn clip_upserts_removals_and_project_replacement_edit_in_place() {
        // Case `upsert_clip_mutates_in_place`.
        {
            let mut doc = AutoCommit::new();
            VideoDoc::seed_sample(&mut doc);
            let mut p = VideoDoc::load_project(&doc);
            let cid = p.clips[0].id;
            if let Some(c) = p.clip_mut(cid) {
                c.start = 99.0;
            }
            VideoDoc::upsert_clip(&mut doc, p.clip(cid).unwrap());
            let reload = VideoDoc::load_project(&doc);
            assert_eq!(
                reload.clips.len(),
                p.clips.len(),
                "no duplicate clip created"
            );
            assert!((reload.clip(cid).unwrap().start - 99.0).abs() < 1e-9);
        }
        // Case `remove_clip_deletes_from_doc`.
        {
            let mut doc = AutoCommit::new();
            VideoDoc::seed_sample(&mut doc);
            let p = VideoDoc::load_project(&doc);
            let cid = p.clips[0].id;
            VideoDoc::remove_clip(&mut doc, cid);
            let reload = VideoDoc::load_project(&doc);
            assert!(reload.clip(cid).is_none());
            assert_eq!(reload.clips.len(), 6);
        }
        // Case `replacing_a_project_removes_what_the_new_one_does_not_have`.
        {
            // The difference between opening a project and merging one. Without
            // the removals a user who opens a second project gets both timelines
            // stacked on top of each other and no way to tell which is which.
            let mut doc = AutoCommit::new();
            VideoDoc::seed_sample(&mut doc);
            let before = VideoDoc::load_project(&doc);
            assert!(before.clips.len() > 1 && !before.media.is_empty());

            let mut other = Project::new("Other");
            let mid = uuid::Uuid::new_v4();
            other.media.push(MediaItem {
                id: mid,
                name: "Only.mp4".into(),
                kind: MediaKind::Video,
                duration: 5.0,
                ..Default::default()
            });
            let tid = uuid::Uuid::new_v4();
            other.tracks.push(Track {
                id: tid,
                name: "V1".into(),
                index: 1,
                ..Default::default()
            });
            let cid = uuid::Uuid::new_v4();
            other.clips.push(Clip {
                id: cid,
                track: tid,
                media: mid,
                out_point: 5.0,
                ..Default::default()
            });

            VideoDoc::replace_project(&mut doc, &other);
            let after = VideoDoc::load_project(&doc);

            assert_eq!(after.name, "Other");
            assert_eq!(after.media.len(), 1, "the old bin is gone, not merged");
            assert_eq!(after.tracks.len(), 1, "the old tracks are gone");
            assert_eq!(after.clips.len(), 1, "the old timeline is gone");
            assert_eq!(after.clips[0].id, cid);
            for old in &before.clips {
                assert!(
                    after.clip(old.id).is_none(),
                    "no clip of the old project survives"
                );
            }
        }
        // Case `reopening_the_same_project_keeps_its_ids`.
        {
            // Replace is upsert-plus-remove, not delete-everything-and-recreate.
            // Ids are the CRDT keys, so re-minting them would turn "two peers
            // opened the same file" into two disjoint projects that merge into
            // duplicates rather than converging.
            let mut doc = AutoCommit::new();
            VideoDoc::seed_sample(&mut doc);
            let before = VideoDoc::load_project(&doc);

            VideoDoc::replace_project(&mut doc, &before);
            let after = VideoDoc::load_project(&doc);

            assert_eq!(after.clips.len(), before.clips.len());
            for c in &before.clips {
                let same = after.clip(c.id).expect("the same clip is still here");
                assert!((same.start - c.start).abs() < 1e-9);
                assert_eq!(same.media, c.media);
            }
        }
    }

    #[test]
    fn concurrent_peers_converge_on_clips_effects_and_markers() {
        // Case `two_peers_converge_on_a_moved_clip`.
        {
            let mut a = AutoCommit::new();
            VideoDoc::seed_sample(&mut a);
            let mut b = AutoCommit::load(&a.save()).unwrap();

            // B moves a clip.
            let pb = VideoDoc::load_project(&b);
            let cid = pb.clips[0].id;
            let mut moved = pb.clip(cid).unwrap().clone();
            moved.start = 42.0;
            VideoDoc::upsert_clip(&mut b, &moved);

            let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
            a.merge_incoming(&mut b_loaded).unwrap();
            let pa = VideoDoc::load_project(&a);
            assert!((pa.clip(cid).unwrap().start - 42.0).abs() < 1e-9);
        }
        // Case `concurrent_effect_and_marker_adds_keep_both_authored_resources`.
        {
            let mut a = AutoCommit::new();
            VideoDoc::seed_sample(&mut a);
            let mut b = AutoCommit::load(&a.save()).expect("forks the shared base");
            let clip = VideoDoc::load_project(&a).clips[0].id;
            let a_effect = ClipEffect {
                id: Uuid::new_v4(),
                kind: EffectKind::Sepia,
                amount: 0.4,
            };
            let b_effect = ClipEffect {
                id: Uuid::new_v4(),
                kind: EffectKind::Vignette,
                amount: 0.7,
            };
            let a_marker = Marker::new(2.0, "Ada cut", 2);
            let b_marker = Marker::new(5.0, "Bo beat", 5);

            assert!(VideoDoc::add_clip_effect(&mut a, clip, a_effect, 80));
            assert!(VideoDoc::add_project_marker(&mut a, &a_marker));
            assert!(VideoDoc::add_clip_effect(&mut b, clip, b_effect, 81));
            assert!(VideoDoc::add_project_marker(&mut b, &b_marker));

            let mut b_for_a = AutoCommit::load(&b.save()).expect("loads B change");
            a.merge_incoming(&mut b_for_a)
                .expect("merges independent entries");
            let project = VideoDoc::load_project(&a);
            assert!(
                project
                    .clip(clip)
                    .expect("clip survives")
                    .effects
                    .iter()
                    .any(|effect| effect.id == a_effect.id)
            );
            assert!(
                project
                    .clip(clip)
                    .expect("clip survives")
                    .effects
                    .iter()
                    .any(|effect| effect.id == b_effect.id)
            );
            assert!(
                project
                    .markers
                    .iter()
                    .any(|marker| marker.id == a_marker.id)
            );
            assert!(
                project
                    .markers
                    .iter()
                    .any(|marker| marker.id == b_marker.id)
            );
        }
        // Case `explicit_collection_replacement_tombstones_omitted_effects_and_markers`.
        {
            let (mut project, clip) = one_clip();
            let effect = ClipEffect::new(EffectKind::Sepia);
            let marker = Marker::new(3.0, "cut", 2);
            project.markers.push(marker.clone());
            let mut doc = AutoCommit::new();
            VideoDoc::write_project(&mut doc, &project);
            assert!(VideoDoc::add_clip_effect(&mut doc, clip, effect, 0));

            assert!(VideoDoc::set_clip_effects(&mut doc, clip, &[]));
            assert!(
                !VideoDoc::add_clip_effect(&mut doc, clip, effect, 0),
                "a delayed add cannot revive an effect omitted by an explicit replacement"
            );

            project.markers.clear();
            VideoDoc::write_project(&mut doc, &project);
            assert!(
                !VideoDoc::add_project_marker(&mut doc, &marker),
                "a delayed add cannot revive a project marker omitted by replacement"
            );
        }
    }
}
