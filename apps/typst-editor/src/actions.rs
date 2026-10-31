//! The Typst editor's own operations: compile, open, save and export the
//! file shown (effects of the compiler and the filesystem), and zoom and
//! arrange the preview (views). None of them changes the workspace document;
//! an open or a save that does records what it changed through the
//! workspace's verbs ([`crate::resource::Workspace`]). Live Problems
//! rows and preview pages are published beneath the workspace by the
//! view.

use ui::framework::LiveRow;
use ui::tilo_resource::{self, Effect};

/// Bound how many live diagnostic / preview-page rows the palette materialises.
pub const LIVE_EXTRA_RESOURCE_LIMIT: usize = 32;

/// The editor's own operations: compile, open, save and export the file
/// shown (effects of the compiler and the filesystem), and zoom and arrange
/// the preview (views).
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "typst.effect")]
pub enum TypstEffect {
    /// Recompile.
    #[effect(idempotent)]
    Compile,
    /// Save Typst file.
    #[effect(job, cancellable, risk = "file_risk")]
    Save {
        #[param("Absolute path to write it to; omit to save in place or choose")]
        path: Option<ui::tilo_resource::AbsolutePath>,
    },
    /// Save Typst file somewhere else, chosen now.
    #[effect(job, cancellable)]
    SaveAs,
    /// Open Typst file.
    #[effect(job, cancellable)]
    OpenFile {
        #[param("Absolute path of the file; omit to choose one")]
        path: Option<ui::tilo_resource::AbsolutePath>,
    },
    /// Export PDF.
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
    #[effect(view, row)]
    GoToDiagnostic {
        #[param("The Problems row")]
        index: usize,
    },
    /// Toggle outline.
    #[effect(view)]
    Outline,
    /// Go to heading.
    #[effect(view, row)]
    GoToHeading {
        #[param("The heading's one-based source line")]
        line: usize,
    },
    /// Go to page.
    #[effect(view, row)]
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
    LiveRow::new(
        format!("typst.diagnostic:{index}"),
        "typst.diagnostic",
        label,
    )
    .effect(&TypstEffect::GoToDiagnostic { index })
}

fn preview_page_row(page: usize) -> LiveRow {
    LiveRow::new(
        format!("typst.preview-page:{page}"),
        "typst.preview-page",
        format!("Page {}", page + 1),
    )
    .effect(&TypstEffect::GoToPage { page })
}

/// Live Problems rows and preview-page hits — presentation cache, not
/// document state — which the view publishes beneath the workspace, so
/// agents keep jump targets.
pub fn live_extras(ctx: &TypstResourceCtx<'_>) -> Vec<LiveRow> {
    let mut extras = Vec::new();
    if !ctx.diagnostics.is_empty() {
        let mut problems = LiveRow::new(
            "typst.problems",
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
        let mut pages = LiveRow::new(
            "typst.preview-pages",
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
