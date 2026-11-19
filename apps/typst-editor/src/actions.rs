//! The Typst editor's own operations: compile, open, save and export the
//! file shown (effects of the compiler and the filesystem), and zoom and
//! arrange the preview (views). None of them changes the workspace document;
//! an open or a save that does records what it changed through the
//! workspace's verbs ([`crate::resource::Workspace`]). Live Problems
//! rows and preview pages are published beneath the workspace by the
//! view.

use ui::framework::LiveRow;
use ui::tilo_resource::row::RowBindings;
use ui::tilo_resource::{self, Effect};

/// The Problems drawer, as the row it publishes: its children are the
/// [`Diagnostic`] rows.
#[derive(tilo_resource::Row)]
#[row(kind = "typst.problems", label = "Problems")]
pub(crate) struct Problems;

/// One compile problem, as a row of the [`Problems`] drawer: going to it is
/// the effect its type declares.
#[derive(tilo_resource::Row)]
#[row(kind = "typst.diagnostic", label = "Problem", effects = TypstEffect)]
pub(crate) struct Diagnostic;

/// The preview pager, as the row it publishes: its children are the
/// [`PreviewPage`] rows.
#[derive(tilo_resource::Row)]
#[row(kind = "typst.preview-pages", label = "Preview pages")]
pub(crate) struct PreviewPages;

/// One page of the preview, as a row of the [`PreviewPages`] pager: going to
/// it is the effect its type declares.
#[derive(tilo_resource::Row)]
#[row(kind = "typst.preview-page", label = "Page", effects = TypstEffect)]
pub(crate) struct PreviewPage;

/// The outline drawer: its children are the [`Heading`] rows.
#[derive(tilo_resource::Row)]
#[row(kind = "typst.outline", label = "Outline")]
pub(crate) struct Outline;

/// One heading of the file, as a row of the [`Outline`] drawer: going to it
/// is the effect its type declares.
#[derive(tilo_resource::Row)]
#[row(kind = "typst.heading", label = "Heading", effects = TypstEffect)]
pub(crate) struct Heading;

/// Bound how many live diagnostic / preview-page rows the palette materialises.
pub const LIVE_EXTRA_RESOURCE_LIMIT: usize = 32;

/// The editor's own operations: compile, open, save and export the file
/// shown (effects of the compiler and the filesystem), and zoom and arrange
/// the preview (views).
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "typst.effect")]
pub enum TypstEffect {
    /// Recompile.
    #[effect(kind = "outside")]
    #[effect(idempotent)]
    Compile,
    /// Save Typst file.
    #[effect(kind = "outside")]
    #[effect(job, cancellable, risk = "file_risk")]
    Save {
        #[param("Absolute path to write it to; omit to save in place or choose")]
        path: Option<ui::tilo_resource::AbsolutePath>,
    },
    /// Save Typst file somewhere else, chosen now.
    #[effect(kind = "outside")]
    #[effect(job, cancellable)]
    SaveAs,
    /// Open Typst file.
    #[effect(kind = "outside")]
    #[effect(job, cancellable)]
    OpenFile {
        #[param("Absolute path of the file; omit to choose one")]
        path: Option<ui::tilo_resource::AbsolutePath>,
    },
    /// Export PDF.
    #[effect(kind = "outside")]
    #[effect(job, cancellable, risk = "file_risk")]
    ExportPdf {
        #[param("Absolute path to write the PDF to; omit to choose")]
        path: Option<ui::tilo_resource::AbsolutePath>,
    },
    /// Zoom out.
    #[effect(view)]
    ZoomOut,
    /// Zoom in.
    #[effect(view)]
    ZoomIn,
    /// Fit preview width.
    #[effect(view, idempotent)]
    FitWidth,
    /// Toggle problems.
    #[effect(view)]
    Problems,
    /// Show editor.
    #[effect(view, idempotent)]
    Editor,
    /// Show preview.
    #[effect(view, idempotent)]
    Preview,
    /// Go to diagnostic.
    #[effect(view, row, on = Diagnostic)]
    GoToDiagnostic {
        #[param("The Problems row")]
        index: usize,
    },
    /// Toggle outline.
    #[effect(view)]
    Outline,
    /// Go to heading.
    #[effect(view, row, on = Heading)]
    GoToHeading {
        #[param("The heading's one-based source line")]
        line: usize,
    },
    /// Go to page.
    #[effect(view, row, on = PreviewPage)]
    GoToPage {
        #[param("The zero-based page")]
        page: usize,
    },
}

/// What saving or exporting risks: nothing when the person chooses where in
/// the file chooser (or saves the file where it already lives, as Save does);
/// writing over whatever is at a path a caller names is destructive and asks
/// first.
fn file_risk(effect: &TypstEffect) -> ui::tilo_actions::RiskLevel {
    match effect {
        TypstEffect::Save { path: Some(_) } | TypstEffect::ExportPdf { path: Some(_) } => {
            ui::tilo_actions::RiskLevel::Destructive
        }
        _ => ui::tilo_actions::RiskLevel::None,
    }
}

/// Live presentation inputs for the resource tree — not CRDT.
pub struct TypstResourceCtx<'a> {
    pub diagnostics: &'a [crate::compile::Diagnostic],
    pub page_count: usize,
}

fn truncate_label(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_owned();
    }
    let cut: String = trimmed.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{cut}…")
}

fn diagnostic_row(index: usize, diag: &crate::compile::Diagnostic) -> LiveRow {
    use crate::compile::DiagKind;
    let severity = match diag.kind {
        DiagKind::Error => "Error",
        DiagKind::Warning => "Warning",
    };
    let message = truncate_label(&diag.message, 48);
    let label = match (diag.line, diag.col) {
        (Some(line), Some(col)) => format!("{severity} at {line}:{col}: {message}"),
        (Some(line), None) => format!("{severity} on line {line}: {message}"),
        _ => format!("{severity}: {message}"),
    };
    let bindings =
        RowBindings::new().bind(TypstEffect::GO_TO_DIAGNOSTIC, [("index", index.into())]);
    LiveRow::typed_with::<Diagnostic>(format!("typst.diagnostic:{index}"), label, &bindings)
}

fn preview_page_row(page: usize) -> LiveRow {
    let bindings = RowBindings::new().bind(TypstEffect::GO_TO_PAGE, [("page", page.into())]);
    LiveRow::typed_with::<PreviewPage>(
        format!("typst.preview-page:{page}"),
        format!("Page {}", page + 1),
        &bindings,
    )
}

/// Live Problems rows and preview-page hits — presentation cache, not
/// document state — which the view publishes beneath the workspace, so
/// agents keep jump targets.
pub fn live_extras(ctx: &TypstResourceCtx<'_>) -> Vec<LiveRow> {
    let mut extras = Vec::new();
    if !ctx.diagnostics.is_empty() {
        let mut problems = LiveRow::typed::<Problems>(
            "typst.problems",
            format!("Problems ({})", ctx.diagnostics.len()),
        );
        for (index, diag) in ctx
            .diagnostics
            .iter()
            .take(LIVE_EXTRA_RESOURCE_LIMIT)
            .enumerate()
        {
            problems = problems.row(diagnostic_row(index, diag));
        }
        extras.push(problems);
    }
    if ctx.page_count > 0 {
        let mut pages = LiveRow::typed::<PreviewPages>(
            "typst.preview-pages",
            format!("Preview pages ({})", ctx.page_count),
        );
        for page in 0..ctx.page_count.min(LIVE_EXTRA_RESOURCE_LIMIT) {
            pages = pages.row(preview_page_row(page));
        }
        extras.push(pages);
    }
    extras
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_effects_take_a_path_and_keep_job_confirmation_contracts() {
        use ui::tilo_actions::ConfirmationPolicy;
        let specs = TypstEffect::specs();
        for id in ["open-file", "save", "export-pdf"] {
            let spec = specs.iter().find(|spec| spec.id == id).unwrap();
            assert_eq!(spec.params()[0].name, "path");
            assert!(matches!(
                spec.contract.execution,
                ui::tilo_actions::ExecutionMode::Background { cancellable: true }
            ));
            assert_eq!(spec.contract.result, ui::tilo_actions::ResultSpec::Job);
            // The person's own Open, Save or Export, answered in the file
            // chooser, asks nothing more.
            let chosen = serde_json::Map::new();
            assert_eq!(
                spec.confirmation_for_json(&chosen),
                ConfirmationPolicy::None,
                "{id} through the chooser"
            );
        }
        // Writing over whatever is at a path a caller names asks first.
        for id in ["save", "export-pdf"] {
            let spec = specs.iter().find(|spec| spec.id == id).unwrap();
            let named = serde_json::json!({"path": "/tmp/somewhere"});
            assert_eq!(
                spec.confirmation_for_json(named.as_object().unwrap()),
                ConfirmationPolicy::Confirm,
                "{id} to a named path"
            );
        }
        let save = TypstEffect::decode(
            "save",
            serde_json::json!({"path": "/tmp/saved.typ"})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            save,
            TypstEffect::Save {
                path: Some("/tmp/saved.typ".parse().unwrap())
            }
        );
        for id in ["save", "open-file", "export-pdf"] {
            let error = TypstEffect::decode(
                id,
                serde_json::json!({"path": "saved.typ"})
                    .as_object()
                    .unwrap(),
            )
            .unwrap_err();
            assert!(
                matches!(
                    &error,
                    ui::tilo_resource::EffectDecodeError::Argument { name, source }
                        if name == "path" && source.to_string().contains("absolute")
                ),
                "{id}: {error}"
            );
        }
        for id in ["zoom-in", "problems"] {
            let spec = specs.iter().find(|spec| spec.id == id).unwrap();
            assert_eq!(
                spec.contract.risk,
                ui::tilo_actions::RiskLevel::None,
                "{id} is a view"
            );
        }
        assert!(
            specs.iter().all(|spec| spec.id != "go-to-page"),
            "a page is gone to from its own row"
        );
    }
}
