//! Saved-place History. Forecasts, air quality and device preferences are not versions.
use crate::{
    doc::WeatherDoc,
    model::{LocationId, SavedLocation},
    units::Units,
};
use automerge::{AutoCommit, ObjId, ObjType, Prop, ROOT, ReadDoc, ScalarValue, Value};
use ui::history_mode::restore::{HistoryRestoreRoot, HistoryRestoreSchema, RestoreValueType};
use ui::history_mode::{
    HistoryDefinition, HistoryPresentation, HistoryPreview, HistoryScopeDefinition,
    PreviewAccessibility, PreviewInput, PreviewNavigation, PreviewView,
};
use ui::{TextRenderer, Theme, paint::PaintCtx};
use vello::{
    Scene,
    kurbo::{Affine, Rect},
    peniko::{BlendMode, Fill},
};

pub static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "so.tilo.weather",
    scopes: &[HistoryScopeDefinition {
        key: "saved-places",
        title: "Saved places (forecasts are not recorded)",
        presentation: HistoryPresentation::Domain { create: preview },
        content_schema: Some(&SCHEMA),
    }],
};
pub static SCHEMA: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "weather.saved-places",
    version: 1,
    roots: &[
        HistoryRestoreRoot {
            key: "saved_locations",
            required: false,
            expected_type: RestoreValueType::Object(ObjType::Map),
        },
        HistoryRestoreRoot {
            key: "location_order",
            required: false,
            expected_type: RestoreValueType::Object(ObjType::List),
        },
        HistoryRestoreRoot {
            key: "location_tombstones",
            required: false,
            expected_type: RestoreValueType::Object(ObjType::Map),
        },
    ],
    selectors: &[],
    preserved_roots: &["selected_location"],
    preserved: &[],
    validate,
    validate_result: validate,
};
fn single<'a>(
    d: &'a AutoCommit,
    o: &ObjId,
    p: impl Into<Prop>,
) -> Result<Option<(Value<'a>, ObjId)>, String> {
    let mut values = d.get_all(o, p).map_err(|e| e.to_string())?;
    if values.len() > 1 {
        return Err(
            "Conflicting saved-place content must be resolved before History preview or restore"
                .into(),
        );
    }
    Ok(values.pop())
}
fn object(
    d: &AutoCommit,
    o: &ObjId,
    p: impl Into<Prop>,
    kind: ObjType,
) -> Result<Option<ObjId>, String> {
    match single(d, o, p)? {
        None => Ok(None),
        Some((Value::Object(k), id)) if k == kind => Ok(Some(id)),
        _ => Err("Invalid saved-place object type".into()),
    }
}
fn string(d: &AutoCommit, o: &ObjId, p: impl Into<Prop>) -> Result<String, String> {
    match single(d, o, p)? {
        Some((Value::Scalar(v), _)) if matches!(v.as_ref(), ScalarValue::Str(_)) => {
            Ok(v.as_str().unwrap_or_default().to_string())
        }
        _ => Err("Missing or invalid saved-place text".into()),
    }
}
fn uuid(s: &str) -> Result<LocationId, String> {
    s.parse().map_err(|_| "Invalid saved-place UUID".into())
}
fn place(d: &AutoCommit, o: &ObjId, key: &str) -> Result<LocationId, String> {
    let id = uuid(&string(d, o, "id")?)?;
    if uuid(key).ok() != Some(id) {
        return Err("Saved-place UUID disagrees with its map key".into());
    }
    for f in ["name", "region", "country", "country_code", "tz"] {
        string(d, o, f)?;
    }
    for (f, limit) in [("lat", 90.), ("lon", 180.)] {
        let n = single(d, o, f)?
            .and_then(|(v, _)| match v {
                Value::Scalar(n) => match n.as_ref() {
                    ScalarValue::F64(x) => Some(*x),
                    ScalarValue::Int(x) => Some(*x as f64),
                    ScalarValue::Uint(x) => Some(*x as f64),
                    _ => None,
                },
                Value::Object(_) => None,
            })
            .ok_or("Missing or invalid coordinates")?;
        if !n.is_finite() || n.abs() > limit {
            return Err("Saved-place coordinate outside geographic bounds".into());
        }
    }
    Ok(id)
}
/// Validate supported unknown descendants too: no conflicting/unsupported data is silently projected away.
fn tree(d: &AutoCommit, o: &ObjId, depth: usize, budget: &mut usize) -> Result<(), String> {
    if depth > 32 || *budget > 100_000 || d.length(o) > 100_000 {
        return Err("Saved-place content exceeds safe limits".into());
    }
    let props: Vec<Prop> = match d.object_type(o).map_err(|e| e.to_string())? {
        ObjType::Map => d.keys(o).map(Prop::Map).collect(),
        ObjType::List => (0..d.length(o)).map(Prop::Seq).collect(),
        ObjType::Text => {
            *budget += d.length(o);
            return Ok(());
        }
        ObjType::Table => return Err("Unsupported saved-place object".into()),
    };
    for p in props {
        *budget += 1;
        match single(d, o, p)?.ok_or("Missing saved-place value")? {
            (Value::Object(_), id) => tree(d, &id, depth + 1, budget)?,
            (Value::Scalar(s), _) => match s.as_ref() {
                ScalarValue::Str(s) => {
                    *budget += s.len();
                }
                ScalarValue::F64(n) if !n.is_finite() => {
                    return Err("Non-finite saved-place value".into());
                }
                ScalarValue::Unknown { .. } | ScalarValue::Counter(_) => {
                    return Err("Unsupported saved-place scalar".into());
                }
                _ => {}
            },
        }
    }
    if *budget > 100_000 {
        return Err("Saved-place content exceeds safe limits".into());
    }
    Ok(())
}
pub(crate) fn validate(d: &AutoCommit) -> Result<(), String> {
    let mut budget = 0;
    for (key, kind) in [
        ("saved_locations", ObjType::Map),
        ("location_order", ObjType::List),
        ("location_tombstones", ObjType::Map),
    ] {
        if let Some(o) = object(d, &ROOT, key, kind)? {
            tree(d, &o, 0, &mut budget)?;
        }
    }
    if let Some(o) = object(d, &ROOT, "saved_locations", ObjType::Map)? {
        let mut ids = std::collections::HashSet::new();
        for key in d.keys(&o) {
            let row = object(d, &o, key.as_str(), ObjType::Map)?.ok_or("Missing place")?;
            if !ids.insert(place(d, &row, &key)?) {
                return Err("Duplicate saved-place UUID".into());
            }
        }
    }
    if let Some(o) = object(d, &ROOT, "location_order", ObjType::List)? {
        let mut ids = std::collections::HashSet::new();
        for i in 0..d.length(&o) {
            if !ids.insert(uuid(&string(d, &o, i)?)?) {
                return Err("Duplicate saved-place order entry".into());
            }
        }
    }
    if let Some(o) = object(d, &ROOT, "location_tombstones", ObjType::Map)? {
        for key in d.keys(&o) {
            let row = object(d, &o, key.as_str(), ObjType::Map)?.ok_or("Missing tombstone")?;
            if uuid(&key)? != uuid(&string(d, &row, "id")?)? {
                return Err("Tombstone UUID disagrees with map key".into());
            }
        }
    }
    Ok(())
}
const NOTICE: &str = "Historical weather observations are unavailable. Forecasts and air quality are external data, not recorded versions.";
fn preview(input: PreviewInput) -> Result<Box<dyn HistoryPreview>, String> {
    let d = AutoCommit::load(input.snapshot.bytes()).map_err(|e| e.to_string())?;
    validate(&d)?;
    let model = WeatherDoc::read_model(&d);
    let mut view = input.initial_view;
    if !model
        .locations
        .iter()
        .any(|l| view.page.as_ref() == Some(&l.id.to_string()))
    {
        view.page = model.selected_location().map(|l| l.id.to_string());
    }
    // No observation is displayed, so device-derived units have no role here.
    // Freeze an explicit unit value instead of reading ambient Settings.
    Ok(Box::new(PlacesPreview {
        places: model.locations,
        view,
        units: Units::METRIC,
        bounds: Rect::ZERO,
        max_scroll: 0.,
        row_rects: vec![],
        detail: Rect::ZERO,
    }))
}
struct PlacesPreview {
    places: Vec<SavedLocation>,
    view: PreviewView,
    units: Units,
    bounds: Rect,
    max_scroll: f64,
    row_rects: Vec<Rect>,
    detail: Rect,
}
impl PlacesPreview {
    fn geometry(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let wide = self.bounds.width() >= 760. * f64::from(theme.text_scale.max(1.));
        let detail_w = if wide {
            self.bounds.width() - 340.
        } else {
            self.bounds.width()
        };
        let notice_h = f64::from(
            tr.measure(NOTICE, 14., Some((detail_w - 40.).max(20.) as f32))
                .1,
        );
        let detail_h = 150. * f64::from(theme.text_scale.max(1.)) + notice_h;
        let m = crate::core::ChipMetrics::new(&theme.tokens.spacing, tr.text_scale());
        let list_top = if wide { 20. } else { detail_h + 20. };
        let height = (list_top + self.places.len() as f64 * m.pitch() + 20.).max(detail_h);
        self.max_scroll = (height - self.bounds.height()).max(0.);
        self.view.scroll.1 = self.view.scroll.1.clamp(0., self.max_scroll);
        let y = self.bounds.y0 - self.view.scroll.1;
        self.detail = Rect::new(
            if wide {
                self.bounds.x0 + 340.
            } else {
                self.bounds.x0
            },
            y,
            self.bounds.x1,
            y + detail_h,
        );
        self.row_rects = self
            .places
            .iter()
            .enumerate()
            .map(|(i, _)| {
                Rect::new(
                    self.bounds.x0 + 16.,
                    y + list_top + i as f64 * m.pitch(),
                    if wide {
                        self.bounds.x0 + 324.
                    } else {
                        self.bounds.x1 - 16.
                    },
                    y + list_top + i as f64 * m.pitch() + m.chip_h,
                )
            })
            .collect();
    }
}
impl HistoryPreview for PlacesPreview {
    fn layout(&mut self, bounds: Rect, tr: &mut TextRenderer, theme: &Theme) -> Result<(), String> {
        self.bounds = bounds;
        self.geometry(tr, theme);
        Ok(())
    }
    fn draw(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        self.geometry(tr, theme);
        scene.fill(Fill::NonZero, t, theme.bg_primary, None, &self.bounds);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1., t, &self.bounds);
        // Legacy Weather text helpers paint in absolute canvas coordinates. The
        // supplied transform is applied to their complete scene as one unit.
        let mut content = Scene::new();
        for (l, r) in self.places.iter().zip(&self.row_rects) {
            crate::presentation::draw_saved_place(
                &mut PaintCtx::new(&mut content, Affine::IDENTITY, tr, theme),
                &crate::presentation::SavedPlaceFace {
                    chip: *r,
                    loc: l,
                    snapshot: None,
                    units: self.units,
                    selected: self.view.page.as_ref() == Some(&l.id.to_string()),
                    hovered: false,
                    accent: theme.tokens.colors.accent[5],
                },
            );
        }
        let mut y = self.detail.y0 + 20.;
        let x = self.detail.x0 + 20.;
        let w = (self.detail.width() - 40.).max(20.);
        if let Some(l) = self
            .places
            .iter()
            .find(|l| self.view.page.as_ref() == Some(&l.id.to_string()))
        {
            y = crate::presentation::draw_identity(&mut content, tr, theme, l, x, y, w)
                + f64::from(tr.measure("Ag", 13., None).1)
                + 16.;
            let coordinates = format!("{:.5}°, {:.5}°", l.latitude, l.longitude);
            tr.draw_text(
                &mut content,
                &coordinates,
                14.,
                theme.text_dim,
                Affine::translate((x, y)),
                Some(w as f32),
            );
            y += f64::from(tr.measure(&coordinates, 14., Some(w as f32)).1) + 16.;
        } else {
            tr.draw_text(
                &mut content,
                "No saved places in this version",
                18.,
                theme.text_primary,
                Affine::translate((x, y)),
                Some(w as f32),
            );
            y += f64::from(
                tr.measure("No saved places in this version", 18., Some(w as f32))
                    .1,
            ) + 16.;
        }
        tr.draw_text(
            &mut content,
            NOTICE,
            14.,
            theme.text_dim,
            Affine::translate((x, y)),
            Some(w as f32),
        );
        scene.append(&content, Some(t));
        scene.pop_layer();
    }
    fn navigate(&mut self, c: PreviewNavigation) -> bool {
        let pages = self.accessibility().pages;
        ui::history_mode::adapter::navigate_pages(&mut self.view, c, &pages)
    }
    fn view(&self) -> PreviewView {
        self.view.clone()
    }
    fn accessibility(&self) -> PreviewAccessibility {
        let selected = self
            .places
            .iter()
            .find(|l| self.view.page.as_ref() == Some(&l.id.to_string()));
        let description = selected.map_or_else(
            || "No saved places in this version".into(),
            |l| {
                format!(
                    "{} — {}\n{:.5}°, {:.5}°",
                    l.name,
                    l.subtitle(),
                    l.latitude,
                    l.longitude
                )
            },
        );
        PreviewAccessibility {
            description: format!("{description}\n{NOTICE}"),
            pages: self
                .places
                .iter()
                .map(|l| (l.id.to_string(), l.name.clone()))
                .collect(),
        }
    }
}

#[cfg(test)]
mod painting_tests {
    use super::*;
    #[test]
    fn saved_rows_and_selected_detail_use_real_geometry_at_all_widths_and_scales() {
        let places: Vec<_> = (0..24)
            .map(|i| {
                SavedLocation::new(
                    format!("Place {i}"),
                    "Region",
                    "Country",
                    "CC",
                    10. + f64::from(i),
                    20.,
                    "auto",
                )
            })
            .collect();
        let mut p = PlacesPreview {
            places,
            view: PreviewView::default(),
            units: Units::METRIC,
            bounds: Rect::ZERO,
            max_scroll: 0.,
            row_rects: vec![],
            detail: Rect::ZERO,
        };
        let first = p.places[0].id.to_string();
        let last = p.places[23].id.to_string();
        let mut tr = TextRenderer::new();
        for width in [320., 1000.] {
            for scale in [1., 2.] {
                let theme = Theme::dark().with_text_scale(scale);
                tr.set_text_scale(scale);
                p.navigate(PreviewNavigation::SelectPage(first.clone()));
                p.layout(Rect::new(0., 0., width, 420.), &mut tr, &theme)
                    .unwrap();
                let mut before = Scene::new();
                p.draw(&mut before, Affine::IDENTITY, &mut tr, &theme);
                assert!(before.encoding().resources.glyph_runs.len() > 48);
                assert!(p.row_rects[23].y0 > 420.);
                assert!(p.max_scroll > 0.);
                p.navigate(PreviewNavigation::Scroll {
                    dx: 0.,
                    dy: -100_000.,
                });
                let mut after = Scene::new();
                p.draw(&mut after, Affine::IDENTITY, &mut tr, &theme);
                let r = p.row_rects[23];
                assert!(r.y0 >= 0. && r.y1 <= 420.);
                assert!(after.encoding().resources.glyph_runs.iter().any(|g| {
                    let y = f64::from(g.transform.translation[1]);
                    y >= r.y0 && y <= r.y1
                }));
                assert!(p.navigate(PreviewNavigation::SelectPage(last.clone())));
                assert!(p.view.scroll.1.abs() < f64::EPSILON);
                let mut selected = Scene::new();
                p.draw(&mut selected, Affine::IDENTITY, &mut tr, &theme);
                assert!(p.accessibility().description.contains("Place 23"));
                assert!(p.accessibility().description.contains("33.00000"));
                assert_ne!(
                    before
                        .encoding()
                        .resources
                        .glyphs
                        .iter()
                        .map(|g| (g.id, g.x, g.y))
                        .collect::<Vec<_>>(),
                    selected
                        .encoding()
                        .resources
                        .glyphs
                        .iter()
                        .map(|g| (g.id, g.x, g.y))
                        .collect::<Vec<_>>()
                );
                assert!(
                    p.accessibility()
                        .description
                        .contains("Historical weather observations are unavailable")
                );
            }
        }
    }
}
