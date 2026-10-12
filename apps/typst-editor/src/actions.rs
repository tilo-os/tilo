//! The Typst editor's own operations: compile, open, save and export the
//! file shown (effects of the compiler and the filesystem), and zoom and
//! arrange the preview (views). None of them changes the workspace document;
//! an open or a save that does records what it changed through the
//! workspace's verbs ([`crate::resource::Workspace`]). Live Problems
//! rows and preview pages are published beneath the workspace by the
//! surface.

use ui::tilo_actions::Resource;
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
    #[effect(job, cancellable, destructive)]
    Save {
        #[param("Where to write it; omit to save in place or choose")]
        path: Option<String>,
    },
    /// Open Typst file.
    #[effect(job, cancellable, destructive)]
    OpenFile {
        #[param("Path of the file; omit to choose one")]
        path: Option<String>,
    },
    /// Export PDF.
    #[effect(job, cancellable, destructive)]
    ExportPdf {
        #[param("Where to write the PDF; omit to choose")]
        path: Option<String>,
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
    /// Go to page.
    #[effect(view, row)]
    GoToPage {
        #[param("The zero-based page")]
        page: usize,
    },
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

fn diagnostic_resource(index: usize, diag: &crate::compile::Diagnostic) -> Resource {
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
    Resource::new(
        format!("typst.diagnostic:{index}"),
        "typst.diagnostic",
        label,
    )
    .with_action(TypstEffect::GoToDiagnostic { index }.bound())
}

fn preview_page_resource(page: usize) -> Resource {
    Resource::new(
        format!("typst.preview-page:{page}"),
        "typst.preview-page",
        format!("Page {}", page + 1),
    )
    .with_action(TypstEffect::GoToPage { page }.bound())
}

/// Live Problems rows and preview-page hits — presentation cache, not
/// document state — which the surface publishes beneath the workspace, so
/// agents keep jump targets.
pub fn live_extras(ctx: &TypstResourceCtx<'_>) -> Vec<Resource> {
    let mut extras = Vec::new();
    if !ctx.diagnostics.is_empty() {
        let mut problems = Resource::new(
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
            problems = problems.with_child(diagnostic_resource(index, diag));
        }
        extras.push(problems);
    }
    if ctx.page_count > 0 {
        let mut pages = Resource::new(
            "typst.preview-pages",
            "typst.preview-pages",
            format!("Preview pages ({})", ctx.page_count),
        );
        for page in 0..ctx.page_count.min(LIVE_EXTRA_RESOURCE_LIMIT) {
            pages = pages.with_child(preview_page_resource(page));
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
        let specs = TypstEffect::specs();
        for id in ["open-file", "save", "export-pdf"] {
            let spec = specs.iter().find(|spec| spec.id == id).unwrap();
            assert_eq!(spec.params()[0].name, "path");
            assert!(matches!(
                spec.contract.execution,
                ui::tilo_actions::ExecutionMode::Background { cancellable: true }
            ));
            assert_eq!(spec.contract.result, ui::tilo_actions::ResultSpec::Job);
            assert!(!matches!(
                spec.contract.confirmation,
                ui::tilo_actions::ConfirmationPolicy::None
            ));
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
                path: Some("/tmp/saved.typ".into())
            }
        );
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
