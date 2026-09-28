//! Frozen merged output. The unsaved external originals cannot be reconstructed.
use crate::doc::MergeDoc;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use std::fmt::Write as _;
use ui::history_mode::adapter::{map_collection, navigate_pages};
use ui::history_mode::restore::{
    HistoryRestoreRoot, HistoryRestoreSchema, RestoreValueType, ScalarType,
};
use ui::history_mode::{
    HistoryDefinition, HistoryPresentation, HistoryPreview, HistoryScopeDefinition,
    PreviewAccessibility, PreviewInput, PreviewNavigation, PreviewView,
};
use ui::paint::PaintCtx;
use ui::{TextRenderer, Theme};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect, Stroke},
    peniko::{BlendMode, Fill},
};
pub static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "so.tilo.diffmerge",
    scopes: &[HistoryScopeDefinition {
        key: "output",
        title: "Merge output",
        presentation: HistoryPresentation::Domain { create: preview },
        content_schema: Some(&SCHEMA),
    }],
};
pub static SCHEMA: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "diffmerge.output",
    version: 1,
    roots: &[
        HistoryRestoreRoot {
            key: "merged",
            required: false,
            expected_type: RestoreValueType::Object(ObjType::Text),
        },
        HistoryRestoreRoot {
            key: "resolutions",
            required: false,
            expected_type: RestoreValueType::Object(ObjType::Map),
        },
        HistoryRestoreRoot {
            key: "session",
            required: false,
            expected_type: RestoreValueType::Scalar(ScalarType::String),
        },
    ],
    selectors: &[],
    preserved_roots: &[],
    preserved: &[],
    validate,
    validate_result: validate,
};
fn validate(doc: &AutoCommit) -> Result<(), String> {
    if let Some((value, _)) = doc.get(ROOT, "merged").map_err(|e| e.to_string())?
        && value != Value::Object(ObjType::Text)
    {
        return Err("Merged output must be Text".into());
    }
    if let Some((value, _)) = doc.get(ROOT, "session").map_err(|e| e.to_string())?
        && value.as_str().is_none()
    {
        return Err("Session must be a string".into());
    }
    if let Some(map) = map_collection(doc, "resolutions")? {
        for key in doc.keys(&map) {
            let index = key.parse::<usize>().map_err(|_| "Invalid hunk index")?;
            if index.to_string() != key {
                return Err("Noncanonical hunk index".into());
            }
            match doc.get(&map, &key).map_err(|e| e.to_string())? {
                Some((Value::Scalar(v), _)) if matches!(v.as_ref(),ScalarValue::Str(s) if ["ours","theirs","both"].contains(&s.as_str())) =>
                    {}
                _ => return Err("Invalid recorded hunk choice".into()),
            }
        }
    }
    Ok(())
}
fn preview(input: PreviewInput) -> Result<Box<dyn HistoryPreview>, String> {
    let d = AutoCommit::load(input.snapshot.bytes()).map_err(|e| e.to_string())?;
    validate(&d)?;
    let mut view = input.initial_view;
    if !["output", "originals"].contains(&view.page.as_deref().unwrap_or_default()) {
        view.page = Some("output".into());
    }
    Ok(Box::new(OutputPreview {
        output: MergeDoc::merged_text(&d),
        recorded: MergeDoc::has_merged(&d),
        resolutions: MergeDoc::resolutions(&d),
        session: MergeDoc::session(&d),
        view,
        bounds: Rect::ZERO,
    }))
}
struct OutputPreview {
    output: String,
    recorded: bool,
    resolutions: Vec<(usize, String)>,
    session: Option<String>,
    view: PreviewView,
    bounds: Rect,
}
impl HistoryPreview for OutputPreview {
    fn layout(&mut self, bounds: Rect, _: &mut TextRenderer, _: &Theme) -> Result<(), String> {
        self.bounds = bounds;
        Ok(())
    }
    fn draw(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        // The preview is a surface of its own inside the history chrome: it
        // closes with the edge derived from its fill (HIG §7, §9.5).
        let inset = f64::from(theme.tokens.spacing.s4);
        scene.fill(Fill::NonZero, t, theme.bg_primary, None, &self.bounds);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            ui::surface::edge_on(theme.bg_primary),
            None,
            &ui::surface::edge_outline(self.bounds, 0.0),
        );
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1., t, &self.bounds);
        if self.view.page.as_deref() == Some("output") && self.recorded {
            let x = self.bounds.x0 + inset - self.view.scroll.0;
            let (_, h) = tr.measure("M", 14., None);
            let pitch = f64::from(h).max(20.);
            // Line numbers get a measured gutter (widest number + s3) so the
            // code column never sits on a fixed 48 px it can outgrow.
            let count = self.output.lines().count().max(1);
            let (gutter_w, _) = tr.measure(&count.to_string(), 12., None);
            let gutter = f64::from(gutter_w) + f64::from(theme.tokens.spacing.s3);
            let grammar =
                crate::highlight::grammar_for_label(self.session.as_deref().unwrap_or_default());
            let spans = crate::highlight::document(&grammar, self.output.lines());
            for (i, line) in self.output.lines().enumerate() {
                let y = self.bounds.y0 + inset + i as f64 * pitch - self.view.scroll.1;
                if y + pitch < self.bounds.y0 {
                    continue;
                }
                if y > self.bounds.y1 {
                    break;
                }
                tr.draw_text(
                    scene,
                    &(i + 1).to_string(),
                    12.,
                    theme.text_dim,
                    t * Affine::translate((x, y)),
                    None,
                );
                crate::highlight::draw_line(
                    &mut PaintCtx::new(scene, t, tr, theme),
                    Point::new(x + gutter, y),
                    line,
                    spans.get(i).map_or(&[][..], Vec::as_slice),
                    crate::merge_view::FONT_SIZE,
                );
            }
        } else {
            tr.draw_text(
                scene,
                &self.accessibility().description,
                15.,
                theme.text_dim,
                t * Affine::translate((
                    self.bounds.x0 + inset,
                    self.bounds.y0 + inset - self.view.scroll.1,
                )),
                Some((self.bounds.width() - 2.0 * inset) as f32),
            );
        }
        scene.pop_layer();
    }
    fn navigate(&mut self, c: PreviewNavigation) -> bool {
        let pages = self.accessibility().pages;
        navigate_pages(&mut self.view, c, &pages)
    }
    fn view(&self) -> PreviewView {
        self.view.clone()
    }
    fn accessibility(&self) -> PreviewAccessibility {
        let description = if self.view.page.as_deref() == Some("originals") {
            let mut s="Original inputs unavailable: base, ours and theirs were not recorded. Recorded choices are metadata only; historical hunk identity cannot be verified.".to_string();
            if let Some(label) = &self.session {
                let _ = write!(s, "\nSession label: {label}");
            }
            for (h, choice) in &self.resolutions {
                let _ = write!(s, "\nRecorded hunk {h}: {choice}");
            }
            s
        } else if self.recorded {
            format!("Merged output\n{}", self.output)
        } else {
            "No merged output recorded in this version".into()
        };
        PreviewAccessibility {
            description,
            pages: vec![
                ("output".into(), "Merged output".into()),
                (
                    "originals".into(),
                    "Original inputs unavailable / recorded choices".into(),
                ),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_output_scroll_reaches_last_line_and_moves_wide_code_at_both_widths() {
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        let mut reference = Scene::new();
        tr.draw_text(
            &mut reference,
            "200",
            12.,
            theme.text_dim,
            Affine::IDENTITY,
            None,
        );
        let glyph_ids = |scene: &Scene, range: std::ops::Range<usize>| {
            scene.encoding().resources.glyphs[range]
                .iter()
                .map(|g| g.id)
                .collect::<Vec<_>>()
        };
        let target = glyph_ids(
            &reference,
            reference.encoding().resources.glyph_runs[0].glyphs.clone(),
        );
        let (_, height) = tr.measure("M", 14., None);
        let pitch = f64::from(height).max(20.);
        for width in [360., 1000.] {
            let clip = Rect::new(0., 0., width, 240.);
            let mut preview = OutputPreview {
                output: "abcdefghijklmno ".repeat(100) + "\n",
                recorded: true,
                resolutions: vec![],
                session: None,
                view: PreviewView {
                    page: Some("output".into()),
                    ..Default::default()
                },
                bounds: clip,
            };
            preview.output = preview.output.repeat(200);
            let mut before = Scene::new();
            preview.draw(&mut before, Affine::IDENTITY, &mut tr, &theme);
            assert!(
                !before
                    .encoding()
                    .resources
                    .glyph_runs
                    .iter()
                    .any(|r| glyph_ids(&before, r.glyphs.clone()) == target)
            );
            preview.navigate(PreviewNavigation::Scroll {
                dx: 0.,
                dy: -199. * pitch,
            });
            let mut after = Scene::new();
            preview.draw(&mut after, Affine::IDENTITY, &mut tr, &theme);
            let last_line = after
                .encoding()
                .resources
                .glyph_runs
                .iter()
                .find(|r| glyph_ids(&after, r.glyphs.clone()) == target)
                .unwrap();
            let y = f64::from(last_line.transform.translation[1]);
            assert!(y >= clip.y0 && y < clip.y1);
            let x = after
                .encoding()
                .resources
                .glyph_runs
                .last()
                .unwrap()
                .transform
                .translation[0];
            preview.navigate(PreviewNavigation::Scroll { dx: -160., dy: 0. });
            let mut horizontal = Scene::new();
            preview.draw(&mut horizontal, Affine::IDENTITY, &mut tr, &theme);
            let moved = horizontal
                .encoding()
                .resources
                .glyph_runs
                .last()
                .unwrap()
                .transform
                .translation[0];
            assert!((x - moved - 160.).abs() < 0.01);
        }
    }
}
