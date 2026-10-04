//! The Typst editor's own operations: compile, open, save and export the
//! file shown (effects of the compiler and the filesystem), and zoom and
//! arrange the preview (views). None of them changes the workspace document;
//! an open or a save that does records what it changed through the
//! workspace's actions ([`crate::resource::TypstAction`]). Live Problems
//! rows and preview pages are published beneath the workspace by the
//! surface.

use serde::{Deserialize, Serialize};
use tilo_actions::{
    ActionArgs, ActionContract, ActionParam, ActionSpec, EncodedAction, ExecutionMode,
    OperationKind, ParamType, Resource, ResultSpec, RetryPolicy,
};

pub const OPERATION_TYPE: &str = "typst.operation";

/// Bound how many live diagnostic / preview-page rows the palette materialises.
pub const LIVE_EXTRA_RESOURCE_LIMIT: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypstOperation {
    Compile,
    Save {
        path: Option<String>,
    },
    Open {
        path: Option<String>,
    },
    ExportPdf {
        path: Option<String>,
    },
    ZoomOut,
    ZoomIn,
    FitWidth,
    ToggleProblems,
    ShowEditor,
    ShowPreview,
    /// Jump the editor caret to a live Problems-panel row (presentation cache).
    GoToDiagnostic {
        index: usize,
    },
    /// Scroll the preview to a compiled page index (0-based, presentation cache).
    GoToPage {
        page: usize,
    },
}

/// Live presentation inputs for the resource tree — not CRDT.
pub struct TypstResourceCtx<'a> {
    pub diagnostics: &'a [crate::compile::Diagnostic],
    pub page_count: usize,
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::expect_used,
    reason = "the variant is deliberately matched by value to select its exact action contract; serde-derived operation values are infallibly serializable"
)]
fn operation(id: &str, label: &str, kind: OperationKind, value: TypstOperation) -> ActionSpec {
    let spec = ActionSpec::operation(id, label, kind, OPERATION_TYPE, &value)
        .expect("TypstOperation is always serializable");
    match value {
        TypstOperation::Compile => spec.retry_policy(RetryPolicy::Idempotent),
        TypstOperation::Save { .. }
        | TypstOperation::Open { .. }
        | TypstOperation::ExportPdf { .. } => spec
            .execution(ExecutionMode::Background { cancellable: true })
            .returns(ResultSpec::Job),
        TypstOperation::ZoomOut
        | TypstOperation::ZoomIn
        | TypstOperation::ToggleProblems
        | TypstOperation::GoToDiagnostic { .. }
        | TypstOperation::GoToPage { .. } => spec.with_contract(ActionContract::view()),
        TypstOperation::FitWidth | TypstOperation::ShowEditor | TypstOperation::ShowPreview => spec
            .with_contract(ActionContract::view())
            .retry_policy(RetryPolicy::Idempotent),
    }
}

#[allow(
    clippy::expect_used,
    reason = "serde-derived operation values are infallibly serializable"
)]
fn path_operation(
    id: &'static str,
    label: &'static str,
    variant: fn(Option<String>) -> TypstOperation,
) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        label,
        "path (omit to use the system file picker)",
        OperationKind::Effect,
        OPERATION_TYPE,
        vec![ActionParam::optional(
            "path",
            ParamType::Text,
            "filesystem path",
        )],
        move |args: &ActionArgs| {
            EncodedAction::encode_operation(
                OPERATION_TYPE,
                &variant(
                    args.text("path")
                        .filter(|p| !p.is_empty())
                        .map(str::to_owned),
                ),
            )
            .expect("TypstOperation is always serializable")
        },
    )
    .destructive()
    .execution(ExecutionMode::Background { cancellable: true })
    .returns(ResultSpec::Job)
}

/// The app's own operations, published on the workspace: open a file;
/// save, recompile and export the file shown; zoom and arrange the preview.
pub fn local_operations() -> Vec<ActionSpec> {
    let views = [
        ("zoom-in", "Zoom in", TypstOperation::ZoomIn),
        ("zoom-out", "Zoom out", TypstOperation::ZoomOut),
        ("fit-width", "Fit preview width", TypstOperation::FitWidth),
        ("preview", "Show preview", TypstOperation::ShowPreview),
        (
            "problems",
            "Toggle problems",
            TypstOperation::ToggleProblems,
        ),
        ("editor", "Show editor", TypstOperation::ShowEditor),
    ]
    .into_iter()
    .map(|(id, label, op)| operation(id, label, OperationKind::View, op));
    [
        path_operation("open", "Open Typst file", |path| TypstOperation::Open {
            path,
        }),
        path_operation("save", "Save Typst file", |path| TypstOperation::Save {
            path,
        }),
        path_operation("export-pdf", "Export PDF", |path| {
            TypstOperation::ExportPdf { path }
        }),
        operation(
            "compile",
            "Recompile",
            OperationKind::Effect,
            TypstOperation::Compile,
        ),
    ]
    .into_iter()
    .chain(views)
    .collect()
}

/// `resource`, a live entity of the surface, as the row it publishes.
pub fn live_row(resource: Resource) -> ui::framework::LiveRow {
    let mut row = ui::framework::LiveRow::new(resource.id.as_str(), resource.kind, resource.label);
    for action in resource.actions {
        row = row.operation(action);
    }
    for child in resource.children {
        row = row.row(live_row(child));
    }
    row
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
    let loc = match (diag.line, diag.col) {
        (Some(line), Some(col)) => format!("{line}:{col}"),
        (Some(line), None) => format!("{line}"),
        _ => "—".to_owned(),
    };
    let label = format!("{severity} · {loc} · {}", truncate_label(&diag.message, 48));
    Resource::new(
        format!("typst.diagnostic:{index}"),
        "typst.diagnostic",
        label,
    )
    .with_action(operation(
        "open",
        "Go to diagnostic",
        OperationKind::View,
        TypstOperation::GoToDiagnostic { index },
    ))
}

fn preview_page_resource(page: usize) -> Resource {
    Resource::new(
        format!("typst.preview-page:{page}"),
        "typst.preview-page",
        format!("Page {}", page + 1),
    )
    .with_action(operation(
        "open",
        "Go to page",
        OperationKind::View,
        TypstOperation::GoToPage { page },
    ))
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
            format!("Problems · {}", ctx.diagnostics.len()),
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
            format!("Preview pages · {}", ctx.page_count),
        );
        for page in 0..ctx.page_count.min(LIVE_EXTRA_RESOURCE_LIMIT) {
            pages = pages.with_child(preview_page_resource(page));
        }
        extras.push(pages);
    }
    extras
}

/// Decode one operation payload advertised by this app.
///
/// # Errors
///
/// Returns an error when a payload for this app's operation type is malformed.
pub fn decode(type_id: &str, payload: &[u8]) -> Result<Option<TypstOperation>, String> {
    if type_id != OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_effects_bind_direct_paths_and_keep_job_confirmation_contracts() {
        let operations = local_operations();
        for id in ["open", "save", "export-pdf"] {
            let spec = operations.iter().find(|spec| spec.id == id).unwrap();
            assert_eq!(spec.params()[0].name, "path");
            assert!(matches!(
                spec.contract.execution,
                ExecutionMode::Background { cancellable: true }
            ));
            assert_eq!(spec.contract.result, ResultSpec::Job);
            assert!(!matches!(
                spec.contract.confirmation,
                tilo_actions::ConfirmationPolicy::None
            ));
            let args = serde_json::json!({"path": format!("/tmp/{id}")})
                .as_object()
                .unwrap()
                .clone();
            let encoded = spec.build_json(&args).unwrap();
            let decoded = decode(OPERATION_TYPE, &encoded.payload).unwrap().unwrap();
            let (TypstOperation::Open { path }
            | TypstOperation::Save { path }
            | TypstOperation::ExportPdf { path }) = decoded
            else {
                unreachable!();
            };
            assert_eq!(path.as_deref(), Some(format!("/tmp/{id}").as_str()));
        }
        assert!(operations.iter().any(|spec| {
            spec.id == "compile" && spec.kind == tilo_actions::OperationKind::Effect
        }));
        assert!(operations.iter().any(|spec| {
            spec.id == "zoom-in" && spec.kind == tilo_actions::OperationKind::View
        }));
        assert!(
            operations
                .iter()
                .all(|spec| spec.kind != tilo_actions::OperationKind::Document),
            "the app's own operations never change the document"
        );
    }
}
