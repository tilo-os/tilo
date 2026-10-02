//! Native Automerge persistence for the Themes catalog.
//!
//! Custom themes live in a map keyed by stable id, so unrelated themes and
//! fields merge without whole-list writes.

use crate::model::{FONTS, ThemeSpec, TokenId, hex, parse_hex, presets};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, Value};
use ui::doc::{read_bool, read_number, read_str};

pub const KEY_THEMES: &str = "themes";
pub const KEY_THEME_TOMBSTONES: &str = "theme_tombstones";
pub const KEY_ACTIVE: &str = "active";

const MAX_CUSTOM_THEMES: usize = 256;
const MAX_ID_LEN: usize = 128;
const MAX_NAME_LEN: usize = 128;
const MAX_FONT_LEN: usize = 64;

pub fn put_str(doc: &mut AutoCommit, root: &ObjId, key: &str, val: &str) {
    let _ = ui::doc::put_if_changed(doc, root, key, val);
}
pub fn put_f64(doc: &mut AutoCommit, root: &ObjId, key: &str, val: f64) {
    if val.is_finite() {
        let _ = ui::doc::put_if_changed(doc, root, key, val);
    }
}
fn map(doc: &AutoCommit, root: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(root, key).ok().flatten() {
        Some((Value::Object(ObjType::Map), id)) => Some(id),
        _ => None,
    }
}
fn ensure_map(doc: &mut AutoCommit, root: &ObjId, key: &str) -> Option<ObjId> {
    map(doc, root, key).or_else(|| doc.put_object(root, key, ObjType::Map).ok())
}
/// Install the shared map containers during first-run seeding. This avoids two
/// offline peers concurrently creating the same parent object before they add
/// different keyed records.
pub fn initialize_catalog(doc: &mut AutoCommit, root: &ObjId) {
    let _ = ensure_map(doc, root, KEY_THEMES);
    let _ = ensure_map(doc, root, KEY_THEME_TOMBSTONES);
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
fn valid_name(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= MAX_NAME_LEN
}
fn valid_font(s: &str) -> bool {
    s.len() <= MAX_FONT_LEN && FONTS.contains(&s)
}
fn is_tombstoned(doc: &AutoCommit, root: &ObjId, id: &str) -> bool {
    map(doc, root, KEY_THEME_TOMBSTONES)
        .and_then(|m| read_bool(doc, &m, id))
        .unwrap_or(false)
}
fn record(doc: &AutoCommit, root: &ObjId, id: &str) -> Option<ObjId> {
    if !valid_id(id) || is_tombstoned(doc, root, id) {
        return None;
    }
    let themes = map(doc, root, KEY_THEMES)?;
    match doc.get(&themes, id).ok().flatten() {
        Some((Value::Object(ObjType::Map), r)) => Some(r),
        _ => None,
    }
}
fn read_scale(doc: &AutoCommit, r: &ObjId, key: &str) -> f32 {
    read_number(doc, r, key)
        .filter(|value| value.is_finite() && *value >= 0.0 && *value <= 4.0)
        .map_or(1.0, |value| value as f32)
}

fn read_record(doc: &AutoCommit, id: &str, r: &ObjId) -> Option<ThemeSpec> {
    let name = read_str(doc, r, "name")?;
    let font = read_str(doc, r, "font")?;
    if !valid_id(id) || !valid_name(&name) || !valid_font(&font) {
        return None;
    }
    let c = |t: TokenId| read_str(doc, r, t.key()).and_then(|x| parse_hex(&x));
    let radius = read_number(doc, r, "radius_norm")? as f32;
    let spacing = read_number(doc, r, "spacing_norm")? as f32;
    if !radius.is_finite() || !spacing.is_finite() {
        return None;
    }
    let order = read_number(doc, r, "order")
        .filter(|x| x.is_finite() && *x >= 0.0 && *x <= f64::from(u32::MAX))
        .map_or(u32::MAX, |x| x as u32);
    Some(ThemeSpec {
        id: id.into(),
        name,
        builtin: false,
        dark_base: read_bool(doc, r, "dark_base").unwrap_or(true),
        accent: c(TokenId::Accent)?,
        accent_hover: c(TokenId::AccentHover)?,
        surface_base: c(TokenId::SurfaceBase)?,
        surface_raised: c(TokenId::SurfaceRaised)?,
        text_primary: c(TokenId::TextPrimary)?,
        text_dim: c(TokenId::TextDim)?,
        border: c(TokenId::Border)?,
        success: c(TokenId::Success)?,
        warning: c(TokenId::Warning)?,
        danger: c(TokenId::Danger)?,
        info: c(TokenId::Info)?,
        radius_norm: radius.clamp(0.0, 1.0),
        spacing_norm: spacing.clamp(0.0, 1.0),
        font,
        motion_scale: read_scale(doc, r, "motion_scale"),
        material_scale: read_scale(doc, r, "material_scale"),
        icon_scale: read_scale(doc, r, "icon_scale"),
        order,
    })
}
fn put_record(doc: &mut AutoCommit, r: &ObjId, s: &ThemeSpec) {
    put_str(doc, r, "name", &s.name);
    let _ = ui::doc::put_if_changed(doc, r, "dark_base", s.dark_base);
    for t in TokenId::ALL {
        put_str(doc, r, t.key(), &hex(s.token(t)));
    }
    put_f64(
        doc,
        r,
        "radius_norm",
        f64::from(s.radius_norm.clamp(0.0, 1.0)),
    );
    put_f64(
        doc,
        r,
        "spacing_norm",
        f64::from(s.spacing_norm.clamp(0.0, 1.0)),
    );
    put_str(doc, r, "font", &s.font);
    put_f64(doc, r, "motion_scale", f64::from(s.motion_scale));
    put_f64(doc, r, "material_scale", f64::from(s.material_scale));
    put_f64(doc, r, "icon_scale", f64::from(s.icon_scale));
    let _ = ui::doc::put_if_changed(doc, r, "order", u64::from(s.order));
}

/// Create a record exactly once. Existing records are never replaced by an old
/// whole spec, which prevents stale UI payloads from clobbering remote fields.
pub fn create_custom_theme(doc: &mut AutoCommit, root: &ObjId, s: &ThemeSpec) -> bool {
    if s.builtin
        || !valid_id(&s.id)
        || !valid_name(&s.name)
        || !valid_font(&s.font)
        || presets().iter().any(|preset| preset.id == s.id)
        || is_tombstoned(doc, root, &s.id)
    {
        return false;
    }
    let Some(themes) = ensure_map(doc, root, KEY_THEMES) else {
        return false;
    };
    if doc.get(&themes, &s.id).ok().flatten().is_some()
        || doc.keys(&themes).count() >= MAX_CUSTOM_THEMES
    {
        return false;
    }
    let Ok(r) = doc.put_object(&themes, &s.id, ObjType::Map) else {
        return false;
    };
    put_record(doc, &r, s);
    true
}
pub fn set_name(doc: &mut AutoCommit, root: &ObjId, id: &str, value: &str) -> bool {
    if !valid_name(value) {
        return false;
    }
    record(doc, root, id).is_some_and(|r| ui::doc::put_if_changed(doc, &r, "name", value))
}
pub fn set_token(
    doc: &mut AutoCommit,
    root: &ObjId,
    id: &str,
    token: TokenId,
    value: [u8; 3],
) -> bool {
    record(doc, root, id).is_some_and(|r| ui::doc::put_if_changed(doc, &r, token.key(), hex(value)))
}
pub fn set_radius(doc: &mut AutoCommit, root: &ObjId, id: &str, value: f32) -> bool {
    value.is_finite()
        && record(doc, root, id).is_some_and(|r| {
            ui::doc::put_if_changed(doc, &r, "radius_norm", f64::from(value.clamp(0.0, 1.0)))
        })
}
pub fn set_spacing(doc: &mut AutoCommit, root: &ObjId, id: &str, value: f32) -> bool {
    value.is_finite()
        && record(doc, root, id).is_some_and(|r| {
            ui::doc::put_if_changed(doc, &r, "spacing_norm", f64::from(value.clamp(0.0, 1.0)))
        })
}
pub fn set_scale(doc: &mut AutoCommit, root: &ObjId, id: &str, key: &str, value: f32) -> bool {
    value.is_finite()
        && (0.0..=4.0).contains(&value)
        && record(doc, root, id)
            .is_some_and(|r| ui::doc::put_if_changed(doc, &r, key, f64::from(value)))
}
pub fn set_font(doc: &mut AutoCommit, root: &ObjId, id: &str, value: &str) -> bool {
    valid_font(value)
        && record(doc, root, id).is_some_and(|r| ui::doc::put_if_changed(doc, &r, "font", value))
}
pub fn delete_custom_theme(doc: &mut AutoCommit, root: &ObjId, id: &str) -> bool {
    if record(doc, root, id).is_none() {
        return false;
    }
    ensure_map(doc, root, KEY_THEME_TOMBSTONES)
        .is_some_and(|m| ui::doc::put_if_changed(doc, &m, id, true))
}

pub fn load_catalog(doc: &AutoCommit, root: &ObjId) -> Option<Vec<ThemeSpec>> {
    let mut customs = Vec::new();
    if let Some(themes) = map(doc, root, KEY_THEMES) {
        for id in doc.keys(&themes).take(MAX_CUSTOM_THEMES) {
            if !presets().iter().any(|preset| preset.id == id)
                && !is_tombstoned(doc, root, &id)
                && let Ok(Some((Value::Object(ObjType::Map), r))) = doc.get(&themes, &id)
                && let Some(s) = read_record(doc, &id, &r)
            {
                customs.push(s);
            }
        }
    }
    customs.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));
    customs.truncate(MAX_CUSTOM_THEMES);
    let mut all = presets();
    all.extend(customs);
    Some(all)
}
/// Additive importer, not an authoritative whole-catalog replacement.
pub fn save_catalog(doc: &mut AutoCommit, root: &ObjId, catalog: &[ThemeSpec]) {
    initialize_catalog(doc, root);
    for s in catalog
        .iter()
        .filter(|s| !s.builtin)
        .take(MAX_CUSTOM_THEMES)
    {
        let _ = create_custom_theme(doc, root, s);
    }
}
/// Validate the authored catalog without migration, seeding, or OS-store access.
pub(crate) fn validate_history_catalog(doc: &AutoCommit) -> Result<(), String> {
    use ui::history_mode::adapter::map_collection;
    if let Some(map) = map_collection(doc, KEY_THEMES)? {
        for id in doc.keys(&map) {
            let Some((Value::Object(ObjType::Map), row)) =
                doc.get(&map, &id).map_err(|e| e.to_string())?
            else {
                return Err("Invalid theme record".into());
            };
            if read_record(doc, &id, &row).is_none() {
                return Err("Incomplete or invalid theme record".into());
            }
            for key in doc.keys(&row) {
                if TokenId::ALL.iter().any(|t| t.key() == key)
                    || [
                        "name",
                        "font",
                        "radius_norm",
                        "spacing_norm",
                        "order",
                        "dark_base",
                    ]
                    .contains(&key.as_str())
                {
                } else {
                    return Err(format!("Unknown theme field: {key}"));
                }
                if key == "dark_base" && read_bool(doc, &row, &key).is_none() {
                    return Err("Invalid dark_base".into());
                }
                if key == "order"
                    && !read_number(doc, &row, &key)
                        .is_some_and(|v| v.is_finite() && v >= 0. && v <= f64::from(u32::MAX))
                {
                    return Err("Invalid order".into());
                }
            }
        }
    }
    if let Some(map) = map_collection(doc, KEY_THEME_TOMBSTONES)? {
        for id in doc.keys(&map) {
            if !valid_id(&id) || read_bool(doc, &map, &id).is_none() {
                return Err("Invalid theme tombstone".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::seed_custom;
    use automerge::ROOT;
    use tilo_automerge::ApplyIncoming;
    /// Theme documents merge fields and records, let deletes win, and reject oversized ones.
    #[test]
    fn theme_records_merge_delete_and_reject_oversize() {
        // Case `fields_merge`.
        {
            let mut a = AutoCommit::new();
            save_catalog(&mut a, &ROOT, &[seed_custom()]);
            let mut b = AutoCommit::load(&a.save()).unwrap();
            assert!(set_name(&mut a, &ROOT, "sunset", "Dawn"));
            assert!(set_token(
                &mut b,
                &ROOT,
                "sunset",
                TokenId::Accent,
                [1, 2, 3]
            ));
            a.merge_incoming(&mut b).unwrap();
            let s = load_catalog(&a, &ROOT)
                .unwrap()
                .into_iter()
                .find(|x| x.id == "sunset")
                .unwrap();
            assert_eq!(s.name, "Dawn");
            assert_eq!(s.accent, [1, 2, 3]);
        }

        // Case `records_merge`.
        {
            let mut a = AutoCommit::new();
            // The app creates the container during first-run seeding. Once this
            // common parent exists, peer creation touches different stable keys.
            save_catalog(&mut a, &ROOT, &[seed_custom()]);
            let mut b = AutoCommit::load(&a.save()).unwrap();
            let mut x = seed_custom();
            x.id = "one".into();
            let mut y = seed_custom();
            y.id = "two".into();
            assert!(create_custom_theme(&mut a, &ROOT, &x));
            assert!(create_custom_theme(&mut b, &ROOT, &y));
            a.merge_incoming(&mut b).unwrap();
            let ids: Vec<_> = load_catalog(&a, &ROOT)
                .unwrap()
                .into_iter()
                .map(|x| x.id)
                .collect();
            assert!(ids.contains(&"one".into()) && ids.contains(&"two".into()));
        }

        // Case `delete_wins`.
        {
            let mut a = AutoCommit::new();
            save_catalog(&mut a, &ROOT, &[seed_custom()]);
            let mut b = AutoCommit::load(&a.save()).unwrap();
            assert!(delete_custom_theme(&mut a, &ROOT, "sunset"));
            assert!(set_name(&mut b, &ROOT, "sunset", "edit"));
            a.merge_incoming(&mut b).unwrap();
            assert!(
                !load_catalog(&a, &ROOT)
                    .unwrap()
                    .iter()
                    .any(|x| x.id == "sunset")
            );
        }

        // Case `rejects_untrusted_oversized_records_without_growing_the_catalog`.
        {
            let mut doc = AutoCommit::new();
            let mut bad = seed_custom();
            bad.id = "x".repeat(MAX_ID_LEN + 1);
            assert!(!create_custom_theme(&mut doc, &ROOT, &bad));
            bad.id = "safe".into();
            bad.name = "x".repeat(MAX_NAME_LEN + 1);
            assert!(!create_custom_theme(&mut doc, &ROOT, &bad));
            assert!(
                !load_catalog(&doc, &ROOT)
                    .unwrap()
                    .iter()
                    .any(|theme| theme.id == "safe")
            );
        }
    }
}
