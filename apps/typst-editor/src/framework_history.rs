//! Immutable source and embedded-font compilation; no library, worker or IO.
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, Value};
use ui::history_mode::adapter::navigate_pages;
use ui::history_mode::restore::{HistoryRestoreRoot, HistoryRestoreSchema, RestoreValueType};
use ui::history_mode::{
    HistoryDefinition, HistoryPresentation, HistoryPreview, HistoryScopeDefinition,
    PreviewAccessibility, PreviewInput, PreviewNavigation, PreviewView,
};
use ui::{TextRenderer, Theme};
use vello::{
    Scene,
    kurbo::{Affine, Rect},
};
pub static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "so.tilo.typst-editor",
    scopes: &[HistoryScopeDefinition {
        key: "source",
        title: "Typst source",
        presentation: HistoryPresentation::Domain { create: preview },
        content_schema: Some(&SCHEMA),
    }],
};
pub static SCHEMA: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "typst-editor.source",
    version: 1,
    roots: &[HistoryRestoreRoot {
        key: "source",
        required: true,
        expected_type: RestoreValueType::Object(ObjType::Text),
    }],
    selectors: &[],
    preserved_roots: &[],
    preserved: &[],
    validate,
    validate_result: validate,
};
fn validate(doc: &AutoCommit) -> Result<(), String> {
    match doc.get(ROOT, "source").map_err(|e| e.to_string())? {
        None | Some((Value::Object(ObjType::Text), _)) => Ok(()),
        _ => Err("Recorded source must be Text".into()),
    }
}
fn preview(input: PreviewInput) -> Result<Box<dyn HistoryPreview>, String> {
    let d = AutoCommit::load(input.snapshot.bytes()).map_err(|e| e.to_string())?;
    from_document(&d, input.initial_view).map(|p| Box::new(p) as Box<dyn HistoryPreview>)
}
fn from_document(d: &AutoCommit, initial_view: PreviewView) -> Result<TypstPreview, String> {
    validate(d)?;
    let source = d
        .get(ROOT, "source")
        .map_err(|e| e.to_string())?
        .map(|(_, id)| d.text(&id).map_err(|e| e.to_string()))
        .transpose()?;
    let editor = crate::editor::SourceEditor::snapshot(source.as_deref().unwrap_or_default());
    let mut pane = crate::preview::PreviewPane::new();
    let output = source
        .as_ref()
        .map(|s| crate::compile::compile_source(&crate::compile::WorldParts::new(), s, 1.));
    let mut diagnostics = crate::diagnostics::DiagnosticsPanel::new();
    let mut pages = vec![("source".into(), "Recorded source".into())];
    let mut compiled_text = Vec::new();
    if let Some(out) = output {
        pane.set_pages_with_text(&out.pages, &out.page_text);
        for (i, runs) in out.page_text.iter().enumerate() {
            pages.push((format!("page:{i}"), format!("Compiled page {}", i + 1)));
            compiled_text.push(
                runs.iter()
                    .flat_map(|r| r.cells.iter().map(|c| c.text.as_str()))
                    .collect::<String>(),
            );
        }
        if !out.diagnostics.is_empty() {
            pages.push(("diagnostics".into(), "Compilation diagnostics".into()));
        }
        diagnostics.set_diagnostics(out.diagnostics);
    }
    let mut view = initial_view;
    if !pages.iter().any(|(id, _)| Some(id) == view.page.as_ref()) {
        view.page = Some("source".into());
    }
    Ok(TypstPreview {
        source,
        editor,
        pane,
        diagnostics,
        pages,
        compiled_text,
        view,
        bounds: Rect::ZERO,
    })
}
struct TypstPreview {
    source: Option<String>,
    editor: crate::editor::SourceEditor,
    pane: crate::preview::PreviewPane,
    diagnostics: crate::diagnostics::DiagnosticsPanel,
    pages: Vec<(String, String)>,
    compiled_text: Vec<String>,
    view: PreviewView,
    bounds: Rect,
}
impl TypstPreview {
    fn page_index(&self) -> Option<usize> {
        self.view
            .page
            .as_deref()?
            .strip_prefix("page:")?
            .parse()
            .ok()
    }
}
impl HistoryPreview for TypstPreview {
    fn layout(&mut self, bounds: Rect, tr: &mut TextRenderer, theme: &Theme) -> Result<(), String> {
        self.bounds = bounds;
        self.editor.set_rect(bounds);
        self.pane.set_rect(bounds);
        // The rows are measured in the theme before the panel is scrolled, so
        // the saved offset is clamped against what is really on screen.
        self.diagnostics.measure_rows(tr, theme);
        self.diagnostics.set_rect(bounds);
        self.pane.apply_theme(theme);
        if self.view.page.as_deref() == Some("source") {
            self.editor.prepare_snapshot_layout(tr);
            self.editor.reset_cursor();
            self.editor.scroll_by(
                0.,
                self.view.scroll.1 as f32,
                self.source.as_deref().unwrap_or_default(),
            );
            self.view.scroll = (0., self.editor.snapshot_scroll_y());
        } else if self.view.page.as_deref() == Some("diagnostics") {
            self.diagnostics.scroll_by(-100_000.);
            self.diagnostics.scroll_by(self.view.scroll.1);
            self.view.scroll = (0., self.diagnostics.scroll_y());
        }
        if let Some(page) = self.page_index() {
            self.view.scroll = self
                .pane
                .set_snapshot_view(page, self.view.zoom, self.view.scroll);
        }
        Ok(())
    }
    fn draw(&mut self, s: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        if self.source.is_none() {
            ui::widget::EmptyState::paint(
                &mut ui::paint::PaintCtx::new(s, t, tr, theme),
                self.bounds,
                &ui::widget::EmptyStateContent {
                    icon: Some(ui::icons::Icon::FileText),
                    message: Some("Restore is unavailable for this version."),
                    ..ui::widget::EmptyStateContent::new("No source recorded in this version")
                },
                vello::kurbo::Point::new(-1., -1.),
            );
        } else if self.view.page.as_deref() == Some("diagnostics") {
            self.diagnostics.draw(s, t, tr, theme);
        } else if self.page_index().is_some() {
            self.pane.draw(s, t, tr, theme);
        } else {
            self.editor.draw(s, t, tr, theme, self.diagnostics.diags());
            // Drawing may refine wrapped content metrics after a font/size change.
            self.view.scroll = (0., self.editor.snapshot_scroll_y());
        }
    }
    fn navigate(&mut self, c: PreviewNavigation) -> bool {
        if let PreviewNavigation::Zoom { factor, anchor } = c {
            if self.page_index().is_none()
                || !factor.is_finite()
                || factor <= 0.
                || !anchor.x.is_finite()
                || !anchor.y.is_finite()
            {
                return false;
            }
            self.view.zoom = (self.view.zoom * factor).clamp(0.25, 4.);
            return true;
        }
        let old = self.view.clone();
        if !navigate_pages(&mut self.view, c, &self.pages) {
            return false;
        }
        if self.view.page != old.page {
            self.editor.reset_cursor();
            self.diagnostics.scroll_by(-100_000.);
        }
        let dy = self.view.scroll.1 - old.scroll.1;
        if self.view.page.as_deref() == Some("source") {
            self.editor
                .scroll_by(0., dy as f32, self.source.as_deref().unwrap_or_default());
            self.view.scroll = (0., self.editor.snapshot_scroll_y());
        }
        if self.view.page.as_deref() == Some("diagnostics") {
            self.diagnostics.scroll_by(dy);
            self.view.scroll = (0., self.diagnostics.scroll_y());
        }
        if let Some(page) = self.page_index() {
            self.view.scroll = self
                .pane
                .set_snapshot_view(page, self.view.zoom, self.view.scroll);
        }
        true
    }
    fn view(&self) -> PreviewView {
        self.view.clone()
    }
    fn accessibility(&self) -> PreviewAccessibility {
        let description = if let Some(source) = &self.source {
            if let Some(i) = self.page_index() {
                format!(
                    "Compiled page {}\n{}",
                    i + 1,
                    self.compiled_text.get(i).map_or("", String::as_str)
                )
            } else if self.view.page.as_deref() == Some("diagnostics") {
                self.diagnostics
                    .diags()
                    .iter()
                    .map(|d| format!("Line {}: {}", d.line.unwrap_or(0), d.message))
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                let mut s = format!("Recorded source\n{source}");
                if self.diagnostics.error_count() > 0 {
                    s.push_str("\nCompilation failed; diagnostics available. No current or last-good pages are shown.");
                }
                s
            }
        } else {
            "No source recorded in this version. Restore is unavailable.".into()
        };
        PreviewAccessibility {
            description,
            pages: self.pages.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    fn scroll_fixture(source: &str) -> AutoCommit {
        let mut doc = AutoCommit::new();
        let obj = doc.put_object(ROOT, "source", ObjType::Text).unwrap();
        doc.splice_text(&obj, 0, 0, source).unwrap();
        doc.get_heads();
        doc
    }

    fn paint_frame(
        p: &mut TypstPreview,
        bounds: Rect,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> Scene {
        p.layout(bounds, tr, theme).unwrap();
        let mut scene = Scene::new();
        p.draw(&mut scene, Affine::IDENTITY, tr, theme);
        scene
    }

    fn content_probe(p: &TypstPreview, theme: &Theme) -> vello::kurbo::Point {
        match p.view.page.as_deref().unwrap() {
            "source" => p
                .editor
                .history_test_caret_rect(theme, p.source.as_ref().unwrap().rfind("row").unwrap())
                .origin(),
            "diagnostics" => vello::kurbo::Point::new(
                0.,
                p.diagnostics.row_rect(p.diagnostics.diags().len() - 3).y0,
            ),
            _ => p.pane.history_test_point(2, 495.),
        }
    }

    fn scroll_preview(doc: &AutoCommit, view: PreviewView) -> TypstPreview {
        let mut p = from_document(doc, view).unwrap();
        if p.view.page.as_deref() == Some("diagnostics") {
            // Typst stops at its first error. Exercise the same renderer's long
            // diagnostic-list state using copies of that real compiler result.
            let diagnostic = p.diagnostics.diags()[0].clone();
            p.diagnostics.set_diagnostics(vec![diagnostic; 40]);
        }
        p
    }

    // Mirrors HistoryCanvas: navigate, then layout and draw on every frame.
    fn check_clamped_scroll_cycle(doc: &AutoCommit, page: &str) {
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        for width in [360., 1000.] {
            let clip = Rect::new(0., 0., width, 240.);
            let view = PreviewView {
                page: Some(page.into()),
                zoom: 2.,
                ..Default::default()
            };
            let mut p = scroll_preview(doc, view.clone());
            assert_eq!(p.view.page.as_deref(), Some(page));
            if page == "diagnostics" {
                assert!(p.diagnostics.diags().len() >= 20);
            }
            paint_frame(&mut p, clip, &mut tr, &theme);
            let origin = content_probe(&p, &theme);
            p.navigate(PreviewNavigation::Scroll {
                dx: 0.,
                dy: -100_000.,
            });
            let end_scene = paint_frame(&mut p, clip, &mut tr, &theme);
            let end = content_probe(&p, &theme);
            assert!(
                end.y >= clip.y0 && end.y < clip.y1,
                "end content reachable: {page} {width}: {end:?}"
            );
            p.navigate(PreviewNavigation::Scroll { dx: 0., dy: 32. });
            let reversed_scene = paint_frame(&mut p, clip, &mut tr, &theme);
            let reversed = content_probe(&p, &theme);
            assert!(
                (reversed.y - end.y - 32.).abs() < 0.02,
                "immediate reverse: {page} {width}: {end:?} -> {reversed:?}"
            );
            assert!((p.view.scroll.1 - (origin.y - reversed.y)).abs() < 0.02);
            if page == "diagnostics" {
                let row_h = p
                    .diagnostics
                    .row_rect(p.diagnostics.diags().len() - 3)
                    .height();
                for (scene, row) in [(end_scene, end.y), (reversed_scene, reversed.y)] {
                    assert!(
                        scene.encoding().resources.glyph_runs.iter().any(|run| {
                            let y = f64::from(run.transform.translation[1]);
                            y >= row && y <= row + row_h
                        }),
                        "the diagnostic row's text moves with its viewport geometry"
                    );
                }
            }
            p.navigate(PreviewNavigation::Scroll {
                dx: -100_000.,
                dy: -100_000.,
            });
            paint_frame(&mut p, clip, &mut tr, &theme);
            if page.starts_with("page:") {
                let right = content_probe(&p, &theme).x;
                p.navigate(PreviewNavigation::Scroll { dx: 32., dy: 0. });
                paint_frame(&mut p, clip, &mut tr, &theme);
                assert!((content_probe(&p, &theme).x - right - 32.).abs() < 0.02);
                assert!((p.view.scroll.0 - (origin.x - content_probe(&p, &theme).x)).abs() < 0.02);
            } else {
                assert!(p.view.scroll.0.abs() < 0.01);
            }
            let before_resize = p.view.scroll.1;
            let larger = Rect::new(0., 0., width, 440.);
            paint_frame(&mut p, larger, &mut tr, &theme);
            assert!(
                p.view.scroll.1 < before_resize,
                "resize reduces saved range"
            );
            let resized = content_probe(&p, &theme);
            let mut zero = scroll_preview(doc, view);
            paint_frame(&mut zero, larger, &mut tr, &theme);
            assert!((p.view.scroll.1 - (content_probe(&zero, &theme).y - resized.y)).abs() < 0.02);
            let saved = p.view();
            let mut recreated = scroll_preview(doc, saved.clone());
            for _ in 0..2 {
                paint_frame(&mut recreated, larger, &mut tr, &theme);
                assert_eq!(recreated.view(), saved);
                assert!((content_probe(&recreated, &theme).y - resized.y).abs() < 0.02);
                assert!((content_probe(&recreated, &theme).x - resized.x).abs() < 0.02);
            }
            recreated.navigate(PreviewNavigation::Scroll { dx: 0., dy: 32. });
            paint_frame(&mut recreated, larger, &mut tr, &theme);
            assert!((content_probe(&recreated, &theme).y - resized.y - 32.).abs() < 0.02);
        }
    }

    #[test]
    fn source_and_diagnostics_overscroll_reverse_resize_and_recreate_with_effective_view() {
        // Case `source_overscroll_reverse_resize_and_recreation_use_effective_view`.
        {
            check_clamped_scroll_cycle(
                &scroll_fixture(&"row of historical source\n".repeat(80)),
                "source",
            );
        }
        // Case `diagnostics_overscroll_reverse_resize_and_recreation_use_effective_view`.
        {
            check_clamped_scroll_cycle(
                &scroll_fixture(&"#missing_variable\n".repeat(40)),
                "diagnostics",
            );
        }
    }

    #[test]
    fn pages_overscroll_reverse_resize_and_recreate_with_effective_view() {
        // Case `pages_overscroll_reverse_resize_and_recreation_use_effective_view`.
        {
            check_clamped_scroll_cycle(
                &scroll_fixture(
                    "#set page(width: 200pt, height: 500pt)\nOne\n#pagebreak()\nTwo\n#pagebreak()\nThree",
                ),
                "page:1",
            );
        }
    }

    #[test]
    fn tall_source_and_compiled_pages_move_content_into_the_clip_at_both_widths() {
        {
            use std::fmt::Write;
            let source = (0..160).fold(String::new(), |mut text, i| {
                writeln!(text, "Line {i} of historical source.\n").unwrap();
                text
            });
            let byte = source.find("Line 8 ").unwrap();
            let mut doc = AutoCommit::new();
            let obj = doc.put_object(ROOT, "source", ObjType::Text).unwrap();
            doc.splice_text(&obj, 0, 0, &source).unwrap();
            doc.get_heads();
            let mut p = from_document(&doc, PreviewView::default()).unwrap();
            let mut tr = TextRenderer::new();
            let theme = Theme::dark();
            for width in [360., 1000.] {
                let clip = Rect::new(0., 0., width, 240.);
                p.navigate(PreviewNavigation::SelectPage("source".into()));
                p.layout(clip, &mut tr, &theme).unwrap();
                p.draw(&mut Scene::new(), Affine::IDENTITY, &mut tr, &theme);
                let before = p.editor.history_test_caret_rect(&theme, byte);
                assert!(before.y0 > clip.y1);
                p.navigate(PreviewNavigation::Scroll {
                    dx: 0.,
                    dy: -before.y0 + 80.,
                });
                p.draw(&mut Scene::new(), Affine::IDENTITY, &mut tr, &theme);
                let after = p.editor.history_test_caret_rect(&theme, byte);
                assert!(
                    after.y0 >= clip.y0 && after.y1 <= clip.y1,
                    "{width}: {after:?}"
                );
                let mut recreated = from_document(&doc, p.view()).unwrap();
                recreated.layout(clip, &mut tr, &theme).unwrap();
                recreated.draw(&mut Scene::new(), Affine::IDENTITY, &mut tr, &theme);
                let recreated_caret = recreated.editor.history_test_caret_rect(&theme, byte);
                assert!((recreated_caret.y0 - after.y0).abs() < 0.01);
                for _ in 0..2 {
                    recreated.layout(clip, &mut tr, &theme).unwrap();
                    recreated.draw(&mut Scene::new(), Affine::IDENTITY, &mut tr, &theme);
                    assert!(
                        (recreated.editor.history_test_caret_rect(&theme, byte).y0 - after.y0)
                            .abs()
                            < 0.01
                    );
                }
                tr.bump_generation();
                recreated.layout(clip, &mut tr, &theme).unwrap();
                recreated.draw(&mut Scene::new(), Affine::IDENTITY, &mut tr, &theme);
                assert!(
                    (recreated.editor.history_test_caret_rect(&theme, byte).y0 - after.y0).abs()
                        < 0.01
                );

                p.navigate(PreviewNavigation::SelectPage("page:0".into()));
                p.layout(clip, &mut tr, &theme).unwrap();
                let before = p.pane.history_test_point(0, 400.);
                p.navigate(PreviewNavigation::Scroll { dx: 0., dy: -180. });
                p.layout(clip, &mut tr, &theme).unwrap();
                let after = p.pane.history_test_point(0, 400.);
                assert!(
                    (before.y - after.y - 180.).abs() < 0.01,
                    "compiled page must move by the requested pixels"
                );
            }
        }
    }
}
