//! Native Automerge persistence for the Themes catalog.
//!
//! `catalog` is the legacy JSON text. New records live in a map keyed
//! by stable id, so unrelated themes and fields merge without whole-list writes.

use crate::model::{FONTS, ThemeSpec, TokenId, hex, parse_hex, presets};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, Value};
use ui::doc::{read_bool, read_number, read_str, read_text};

pub const KEY_CATALOG: &str = "catalog";
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
pub fn put_text(doc: &mut AutoCommit, root: &ObjId, key: &str, val: &str) {
    let id = match doc.get(root, key) {
        Ok(Some((Value::Object(ObjType::Text), id))) => id,
        _ => match doc.put_object(root, key, ObjType::Text) {
            Ok(id) => id,
            Err(_) => return,
        },
    };
    ui::doc::splice_text_diffed(doc, &id, val);
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
/// Materialize only the addressed valid legacy record for an explicit edit.
/// Validation of the requested field happens before this function is called.
fn editable_record(doc: &mut AutoCommit, root: &ObjId, id: &str) -> Option<ObjId> {
    if let Some(row) = record(doc, root, id) {
        return Some(row);
    }
    if !valid_id(id) || is_tombstoned(doc, root, id) || presets().iter().any(|s| s.id == id) {
        return None;
    }
    // A malformed or competing native entry is never replaced by legacy data.
    if map(doc, root, KEY_THEMES).is_some_and(|m| doc.get(&m, id).ok().flatten().is_some()) {
        return None;
    }
    let spec = legacy_customs(doc, root).into_iter().find(|s| s.id == id)?;
    if !spec.radius_norm.is_finite() || !spec.spacing_norm.is_finite() {
        return None;
    }
    if !create_custom_theme(doc, root, &spec) {
        return None;
    }
    record(doc, root, id)
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
    editable_record(doc, root, id).is_some_and(|r| ui::doc::put_if_changed(doc, &r, "name", value))
}
pub fn set_token(
    doc: &mut AutoCommit,
    root: &ObjId,
    id: &str,
    token: TokenId,
    value: [u8; 3],
) -> bool {
    editable_record(doc, root, id)
        .is_some_and(|r| ui::doc::put_if_changed(doc, &r, token.key(), hex(value)))
}
pub fn set_radius(doc: &mut AutoCommit, root: &ObjId, id: &str, value: f32) -> bool {
    value.is_finite()
        && editable_record(doc, root, id).is_some_and(|r| {
            ui::doc::put_if_changed(doc, &r, "radius_norm", f64::from(value.clamp(0.0, 1.0)))
        })
}
pub fn set_spacing(doc: &mut AutoCommit, root: &ObjId, id: &str, value: f32) -> bool {
    value.is_finite()
        && editable_record(doc, root, id).is_some_and(|r| {
            ui::doc::put_if_changed(doc, &r, "spacing_norm", f64::from(value.clamp(0.0, 1.0)))
        })
}
pub fn set_font(doc: &mut AutoCommit, root: &ObjId, id: &str, value: &str) -> bool {
    valid_font(value)
        && editable_record(doc, root, id)
            .is_some_and(|r| ui::doc::put_if_changed(doc, &r, "font", value))
}
pub fn delete_custom_theme(doc: &mut AutoCommit, root: &ObjId, id: &str) -> bool {
    if editable_record(doc, root, id).is_none() {
        return false;
    }
    ensure_map(doc, root, KEY_THEME_TOMBSTONES)
        .is_some_and(|m| ui::doc::put_if_changed(doc, &m, id, true))
}

fn legacy_customs(doc: &AutoCommit, root: &ObjId) -> Vec<ThemeSpec> {
    read_text(doc, root, KEY_CATALOG)
        .and_then(|x| serde_json::from_str::<Vec<ThemeSpec>>(&x).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|s| !s.builtin)
        .take(MAX_CUSTOM_THEMES)
        .collect()
}
/// Imports only missing legacy ids. Native edits win; legacy-only themes remain.
pub fn migrate_legacy_catalog(doc: &mut AutoCommit, root: &ObjId) {
    for s in legacy_customs(doc, root) {
        let _ = create_custom_theme(doc, root, &s);
    }
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
    for s in legacy_customs(doc, root) {
        if !presets().iter().any(|preset| preset.id == s.id)
            && !is_tombstoned(doc, root, &s.id)
            && !customs.iter().any(|x: &ThemeSpec| x.id == s.id)
            && valid_id(&s.id)
            && valid_name(&s.name)
            && valid_font(&s.font)
        {
            customs.push(s);
        }
    }
    customs.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));
    customs.truncate(MAX_CUSTOM_THEMES);
    let mut all = presets();
    all.extend(customs);
    Some(all)
}
/// Compatibility importer, not an authoritative whole-catalog replacement.
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
    let root = &automerge::ROOT;
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
    if let Some((value, obj)) = doc.get(root, KEY_CATALOG).map_err(|e| e.to_string())? {
        if value != Value::Object(ObjType::Text) {
            return Err("Legacy catalog must be Text".into());
        }
        let specs: Vec<ThemeSpec> =
            serde_json::from_str(&doc.text(&obj).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let mut ids = std::collections::HashSet::new();
        for s in specs {
            if !valid_id(&s.id)
                || !valid_name(&s.name)
                || !valid_font(&s.font)
                || !s.radius_norm.is_finite()
                || !s.spacing_norm.is_finite()
                || !ids.insert(s.id)
            {
                return Err("Invalid legacy theme".into());
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
    #[test]
    fn fields_merge() {
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
        a.merge(&mut b).unwrap();
        let s = load_catalog(&a, &ROOT)
            .unwrap()
            .into_iter()
            .find(|x| x.id == "sunset")
            .unwrap();
        assert_eq!(s.name, "Dawn");
        assert_eq!(s.accent, [1, 2, 3]);
    }
    #[test]
    fn records_merge() {
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
        a.merge(&mut b).unwrap();
        let ids: Vec<_> = load_catalog(&a, &ROOT)
            .unwrap()
            .into_iter()
            .map(|x| x.id)
            .collect();
        assert!(ids.contains(&"one".into()) && ids.contains(&"two".into()));
    }
    #[test]
    fn delete_wins() {
        let mut a = AutoCommit::new();
        save_catalog(&mut a, &ROOT, &[seed_custom()]);
        let mut b = AutoCommit::load(&a.save()).unwrap();
        assert!(delete_custom_theme(&mut a, &ROOT, "sunset"));
        assert!(set_name(&mut b, &ROOT, "sunset", "edit"));
        a.merge(&mut b).unwrap();
        assert!(
            !load_catalog(&a, &ROOT)
                .unwrap()
                .iter()
                .any(|x| x.id == "sunset")
        );
    }
    #[test]
    fn legacy_and_native_consolidate_idempotently() {
        let mut a = AutoCommit::new();
        put_text(
            &mut a,
            &ROOT,
            KEY_CATALOG,
            &serde_json::to_string(&[seed_custom()]).unwrap(),
        );
        let mut b = AutoCommit::load(&a.save()).unwrap();
        let mut other = seed_custom();
        other.id = "other".into();
        assert!(create_custom_theme(&mut b, &ROOT, &other));
        migrate_legacy_catalog(&mut b, &ROOT);
        b.merge(&mut a).unwrap();
        migrate_legacy_catalog(&mut b, &ROOT);
        let n = b.save().len();
        migrate_legacy_catalog(&mut b, &ROOT);
        assert_eq!(b.save().len(), n);
        let ids: Vec<_> = load_catalog(&b, &ROOT)
            .unwrap()
            .into_iter()
            .map(|x| x.id)
            .collect();
        assert!(ids.contains(&"sunset".into()) && ids.contains(&"other".into()));
    }

    #[test]
    fn rejects_untrusted_oversized_records_without_growing_the_catalog() {
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
