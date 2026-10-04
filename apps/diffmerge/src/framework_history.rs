//! Frozen merged output. The unsaved external originals cannot be reconstructed.
use crate::doc::MergeDoc;
use crate::i18n::{t, t_args, text};
use crate::pane;
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
use ui::tokens::TextRole;
use ui::{TextRenderer, Theme};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
    peniko::{BlendMode, Fill},
};
pub static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "so.tilo.diffmerge",
    scopes: &[HistoryScopeDefinition {
        key: "output",
        title: "Merged result",
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
        // The preview stands on the window's content ground; it paints no
        // surface of its own.
        let inset = f64::from(theme.tokens.spacing.s4);
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        ctx.scene
            .push_layer(Fill::NonZero, BlendMode::default(), 1., t, &self.bounds);
        if self.view.page.as_deref() == Some("output") && self.recorded {
            let x = self.bounds.x0 + inset - self.view.scroll.0;
            let pitch = ctx.line_box(TextRole::Code).ceil();
            // Line numbers get a measured gutter (widest number + s3) so the
            // code column never sits on a fixed width it can outgrow.
            let count = self.output.lines().count().max(1);
            let number = pane::number_style(theme);
            let (gutter_w, _) = ctx.text.measure_style(&count.to_string(), None, &number);
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
                ctx.text(
                    &(i + 1).to_string(),
                    &number,
                    theme.text_dim,
                    Point::new(x, y),
                    None,
                );
                crate::highlight::draw_line(
                    &mut ctx,
                    Point::new(x + gutter, y),
                    line,
                    spans.get(i).map_or(&[][..], Vec::as_slice),
                );
            }
        } else {
            let description = self.accessibility().description;
            ctx.text_role(
                &description,
                TextRole::Body,
                theme.text_dim,
                Point::new(
                    self.bounds.x0 + inset,
                    self.bounds.y0 + inset - self.view.scroll.1,
                ),
                Some(pane::to_f32(self.bounds.width() - 2.0 * inset)),
            );
        }
        ctx.scene.pop_layer();
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
            let mut s = t("history-originals");
            if let Some(label) = &self.session {
                let _ = write!(
                    s,
                    "\n{}",
                    t_args("history-session", &[("label", text(label))])
                );
            }
            for (h, choice) in &self.resolutions {
                let _ = write!(
                    s,
                    "\n{}",
                    t_args(
                        "history-choice",
                        &[("change", text(&h.to_string())), ("choice", text(choice))]
                    )
                );
            }
            s
        } else if self.recorded {
            format!("{}\n{}", t("history-output"), self.output)
        } else {
            t("history-none")
        };
        PreviewAccessibility {
            description,
            pages: vec![
                ("output".into(), t("history-page-output")),
                ("originals".into(), t("history-page-originals")),
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
        tr.draw_style(
            &mut reference,
            "200",
            theme.text_dim,
            Affine::IDENTITY,
            None,
            &pane::number_style(&theme),
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
        let (_, height) = tr.measure_style("Ag", None, &theme.text(TextRole::Code));
        let pitch = f64::from(height).ceil();
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
