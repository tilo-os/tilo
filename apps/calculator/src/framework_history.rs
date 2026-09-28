//! Frozen domain History presentation and declared authored-content restoration.
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use ui::history_mode::adapter::{FieldType, navigate_pages, validate_fields};
use ui::history_mode::restore::{HistoryRestoreRoot, HistoryRestoreSchema, RestoreValueType};
use ui::history_mode::{
    HistoryDefinition, HistoryPresentation, HistoryPreview, HistoryScopeDefinition,
    PreviewAccessibility, PreviewInput, PreviewNavigation, PreviewView,
};
use ui::{TextRenderer, Theme};
use vello::{
    Scene,
    kurbo::{Affine, Rect},
    peniko::{BlendMode, Fill},
};
pub static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "so.tilo.calculator",
    scopes: &[HistoryScopeDefinition {
        key: "calculator",
        title: "Calculations and graphs",
        presentation: HistoryPresentation::Domain { create: preview },
        content_schema: Some(&SCHEMA),
    }],
};
pub static SCHEMA: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "calculator.content",
    version: 1,
    roots: &[
        HistoryRestoreRoot {
            key: "history",
            required: false,
            expected_type: RestoreValueType::Object(ObjType::Map),
        },
        HistoryRestoreRoot {
            key: "funcs",
            required: false,
            expected_type: RestoreValueType::Object(ObjType::Map),
        },
        HistoryRestoreRoot {
            key: "surfaces",
            required: false,
            expected_type: RestoreValueType::Object(ObjType::Map),
        },
    ],
    selectors: &[],
    preserved_roots: &["calculator_resources_v2"],
    preserved: &[],
    validate,
    validate_result: validate,
};

fn collection(doc: &AutoCommit, key: &str) -> Result<Option<ObjId>, String> {
    ui::history_mode::adapter::map_collection(doc, key).map_err(|error| {
        format!("{error}; restoring legacy list-format content is unsupported; select a map-format version")
    })
}

// Closed, local navigation only. The host owns the accessible page controls at
// every width; no pointer/key events or live Widget actions enter this object.

pub(crate) fn validate(doc: &AutoCommit) -> Result<(), String> {
    for key in [crate::doc::HISTORY, crate::doc::FUNCS, crate::doc::SURFACES] {
        let Some(map) = collection(doc, key)? else {
            continue;
        };
        for id in doc.keys(&map) {
            if id.is_empty() {
                return Err("Empty calculator row identity".into());
            }
            let (Value::Object(ObjType::Map), row) = doc
                .get(&map, &id)
                .map_err(|e| e.to_string())?
                .ok_or("Missing calculator row")?
            else {
                return Err("Invalid calculator row".into());
            };
            let allowed: &[(&str, FieldType)] = if key == crate::doc::HISTORY {
                &[
                    ("expr", FieldType::Text),
                    ("result", FieldType::Text),
                    ("rank", FieldType::Integer),
                    ("deleted", FieldType::Boolean),
                ]
            } else {
                &[
                    ("expr", FieldType::Text),
                    ("visible", FieldType::Boolean),
                    ("rank", FieldType::Integer),
                    ("deleted", FieldType::Boolean),
                ]
            };
            validate_fields(doc, &row, allowed)?;
            for (field, _) in allowed {
                if doc
                    .get(&row, *field)
                    .map_err(|error| error.to_string())?
                    .is_none()
                {
                    return Err(format!("Missing calculator field: {field}"));
                }
            }
        }
    }
    if let Some((v, _)) = doc
        .get(ROOT, "calculator_resources_v2")
        .map_err(|e| e.to_string())?
        && v.as_bool() != Some(true)
    {
        return Err("Unsupported calculator schema marker".into());
    }
    Ok(())
}
pub(crate) fn project(doc: &AutoCommit) -> Result<crate::model::CalcModel, String> {
    let model = crate::model::CalcModel {
        history: crate::doc::load_history(doc),
        funcs: crate::doc::load_graph(doc, crate::doc::FUNCS),
        surfaces: crate::doc::load_graph(doc, crate::doc::SURFACES),
        ..crate::model::CalcModel::default()
    };
    for key in [crate::doc::HISTORY, crate::doc::FUNCS, crate::doc::SURFACES] {
        match doc.get(ROOT, key).map_err(|e| e.to_string())? {
            None | Some((Value::Object(ObjType::Map), _)) => {}
            _ => return Err(format!("Unsupported calculator content: {key}")),
        }
    }
    Ok(model)
}
fn preview(input: PreviewInput) -> Result<Box<dyn HistoryPreview>, String> {
    let doc = AutoCommit::load(input.snapshot.bytes()).map_err(|e| e.to_string())?;
    let model = project(&doc)?;
    let mut p = CalculatorPreview {
        model,
        bounds: Rect::ZERO,
        view: input.initial_view,
        g2: crate::graph2d::Graph2dScreen::default(),
        g3: crate::graph3d::Graph3dScreen::default(),
        formula: None,
    };
    let pages = p.accessibility().pages;
    if !pages.iter().any(|(id, _)| Some(id) == p.view.page.as_ref()) {
        p.view.page = Some("calculations".into());
    }
    Ok(Box::new(p))
}
struct CalculatorPreview {
    model: crate::model::CalcModel,
    bounds: Rect,
    view: PreviewView,
    g2: crate::graph2d::Graph2dScreen,
    g3: crate::graph3d::Graph3dScreen,
    formula: Option<(String, crate::mathfield::MathField)>,
}
impl HistoryPreview for CalculatorPreview {
    fn layout(&mut self, bounds: Rect, _: &mut TextRenderer, _: &Theme) -> Result<(), String> {
        self.bounds = bounds;
        Ok(())
    }
    fn draw(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        scene.fill(Fill::NonZero, t, theme.bg_primary, None, &self.bounds);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1., t, &self.bounds);
        // One rhythm: the panel gutter is `s4`, recorded rows step by `s3`.
        let pad = f64::from(theme.tokens.spacing.s4);
        let row_gap = f64::from(theme.tokens.spacing.s3);
        let x = self.bounds.x0 + pad;
        let page = self.view.page.as_deref().unwrap_or("calculations");
        let mut y = self.bounds.y0 + pad
            - if page == "calculations" {
                self.view.scroll.1
            } else {
                0.
            };
        if page == "calculations" {
            if self.model.history.is_empty() {
                tr.draw_text(
                    scene,
                    "No calculations in this historical version",
                    18.,
                    theme.tokens.colors.text_dim,
                    t * Affine::translate((x, y)),
                    Some((self.bounds.width() - 2. * pad) as f32),
                );
            }
            // The live drawer's row height (`keypad::draw_recorded_row` seats a
            // 13 px expression line over an 18 px result line in it).
            let row_h = 70.;
            for entry in &self.model.history {
                crate::keypad::draw_recorded_row(
                    scene,
                    t,
                    tr,
                    theme,
                    Rect::new(x, y, self.bounds.x1 - pad, y + row_h),
                    entry,
                );
                y += row_h + row_gap;
            }
        } else {
            let selected = page
                .split_once(':')
                .and_then(|(kind, index)| index.parse::<usize>().ok().map(|i| (kind, i)));
            if let Some((kind, index)) = selected {
                let row = if kind == "2d" {
                    self.model.funcs.get(index)
                } else {
                    self.model.surfaces.get(index)
                };
                if let Some(row) = row {
                    if self.formula.as_ref().is_some_and(|(id, _)| id != page) {
                        self.formula = None;
                    }
                    let (_, field) = self.formula.get_or_insert_with(|| {
                        (
                            page.to_owned(),
                            crate::mathfield::MathField::detached(&row.expr),
                        )
                    });
                    field.set_text_scale(theme.text_scale);
                    field.set_rect(Rect::new(x, y, self.bounds.x1 - pad, y + 80.), tr);
                    let offset = field.scroll_offset();
                    field.scroll_by(self.view.scroll.0 - offset.0, self.view.scroll.1 - offset.1);
                    self.view.scroll = field.scroll_offset();
                    field.draw(scene, t, tr, theme, theme.text_primary, false);
                    y += 90.;
                    tr.draw_text(
                        scene,
                        if row.visible {
                            "Recorded as visible"
                        } else {
                            "Recorded as hidden"
                        },
                        12.,
                        theme.tokens.colors.text_dim,
                        t * Affine::translate((x, y)),
                        None,
                    );
                    y += 28.;
                    let area = Rect::new(
                        x,
                        y,
                        self.bounds.x1 - pad,
                        (self.bounds.y1 - pad).max(y + 120.),
                    );
                    let mut model = self.model.clone();
                    if kind == "2d" {
                        model.funcs = vec![row.clone()];
                        self.g2
                            .draw_history_canvas(scene, t, tr, theme, &model, area);
                    } else {
                        model.surfaces = vec![row.clone()];
                        self.g3.draw_history_canvas(scene, t, theme, &model, area);
                    }
                }
            }
        }
        scene.pop_layer();
    }
    fn navigate(&mut self, command: PreviewNavigation) -> bool {
        if let PreviewNavigation::Scroll { dx, dy } = &command
            && dx.is_finite()
            && dy.is_finite()
            && let Some((page, field)) = &mut self.formula
            && self.view.page.as_ref() == Some(page)
        {
            field.scroll_by(-dx, -dy);
            self.view.scroll = field.scroll_offset();
            return true;
        }
        let pages = self.accessibility().pages;
        navigate_pages(&mut self.view, command, &pages)
    }
    fn view(&self) -> PreviewView {
        self.view.clone()
    }
    fn accessibility(&self) -> PreviewAccessibility {
        let mut pages = vec![("calculations".into(), "Recorded calculations".into())];
        for (kind, rows) in [("2d", &self.model.funcs), ("3d", &self.model.surfaces)] {
            pages.extend(
                rows.iter()
                    .enumerate()
                    .map(|(i, row)| (format!("{kind}:{i}"), format!("{kind}: {}", row.expr))),
            );
        }
        let description = match self.view.page.as_deref().and_then(|s| s.split_once(':')) {
            Some((kind, index)) => index
                .parse::<usize>()
                .ok()
                .and_then(|i| {
                    if kind == "2d" {
                        self.model.funcs.get(i)
                    } else {
                        self.model.surfaces.get(i)
                    }
                })
                .map(|row| {
                    format!(
                        "{kind}: {} · Recorded visibility: {}",
                        row.expr, row.visible
                    )
                })
                .unwrap_or_default(),
            None if self.model.history.is_empty() => {
                "No calculations in this historical version".into()
            }
            None => self
                .model
                .history
                .iter()
                .map(|h| format!("{} = {}", h.expr, h.result))
                .collect::<Vec<_>>()
                .join("\n"),
        };
        PreviewAccessibility { description, pages }
    }
}

#[cfg(test)]
mod viewport_tests {
    use super::*;
    use automerge::transaction::Transactable;

    #[test]
    fn detached_graph_formula_scroll_reaches_content_end() {
        for width in [360., 1000.] {
            let expression = format!("{}+((1/x)/(1/y))/((1/x)/(1/y))", "x+".repeat(120));
            let mut live = AutoCommit::new();
            crate::doc::store(
                &mut live,
                &crate::model::CalcModel {
                    funcs: vec![crate::model::PlotFn {
                        expr: expression,
                        visible: false,
                    }],
                    ..Default::default()
                },
            );
            let heads = live.get_heads();
            let frozen = live.fork_at(&heads).unwrap();
            live.put(ROOT, "pending_live_input", "preserve").unwrap();
            let pending = live.pending_ops();
            let mut p = CalculatorPreview {
                model: project(&frozen).unwrap(),
                bounds: Rect::ZERO,
                view: PreviewView {
                    page: Some("2d:0".into()),
                    ..Default::default()
                },
                g2: crate::graph2d::Graph2dScreen::default(),
                g3: crate::graph3d::Graph3dScreen::default(),
                formula: None,
            };
            let mut tr = TextRenderer::new();
            let theme = Theme::dark();
            p.layout(Rect::new(0., 0., width, 600.), &mut tr, &theme)
                .unwrap();
            let mut before = Scene::new();
            p.draw(&mut before, Affine::IDENTITY, &mut tr, &theme);
            let (clip, start) = p.formula.as_ref().unwrap().1.viewport_geometry();
            assert!(start.width() > clip.width() && start.height() > clip.height());
            assert!(start.x0 >= clip.x0 && start.y0 >= clip.y0);
            assert!(p.navigate(PreviewNavigation::Scroll {
                dx: -100_000.,
                dy: -100_000.
            }));
            let mut after = Scene::new();
            p.draw(&mut after, Affine::IDENTITY, &mut tr, &theme);
            let transforms = |scene: &Scene| {
                scene
                    .encoding()
                    .resources
                    .glyph_runs
                    .iter()
                    .map(|run| format!("{:?}", run.transform))
                    .collect::<Vec<_>>()
            };
            assert!(
                transforms(&before) != transforms(&after),
                "navigation must move rendered formula content at width {width}"
            );
            let (clip, end) = p.formula.as_ref().unwrap().1.viewport_geometry();
            assert!(
                (end.x1 - clip.x1).abs() < 0.001,
                "full horizontal extent reachable"
            );
            assert!(
                (end.y1 - clip.y1).abs() < 0.001,
                "full vertical extent reachable"
            );
            p.draw(&mut Scene::new(), Affine::IDENTITY, &mut tr, &theme);
            assert_eq!(
                p.formula.as_ref().unwrap().1.viewport_geometry(),
                (clip, end),
                "draw retains actual viewport"
            );
            p.navigate(PreviewNavigation::Scroll {
                dx: 100_000.,
                dy: 100_000.,
            });
            p.draw(&mut Scene::new(), Affine::IDENTITY, &mut tr, &theme);
            assert_eq!(p.formula.as_ref().unwrap().1.viewport_geometry().1, start);
            assert_eq!(live.base_heads(), heads);
            assert_eq!(live.pending_ops(), pending);
            assert_eq!(
                live.get(ROOT, "pending_live_input")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str(),
                Some("preserve")
            );
        }
    }
}
