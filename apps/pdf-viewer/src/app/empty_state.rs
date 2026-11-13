//! The empty window: what it says before a document is open, and the tree
//! that says the same thing to assistive technology. Its words come from the
//! catalogue, so a German window paints and reads German.

use ui::a11y::{AccessNode, AccessState, Politeness, Role};
use vello::kurbo::Rect;

use super::PdfApp;
use tilo_content::ContentState;

impl PdfApp {
    pub(super) fn source_wait_text(&self) -> Option<(String, String, bool)> {
        let (state, _) = self.source_wait?;
        let name = self
            .record_source
            .as_ref()
            .map(|source| source.name.as_str())
            .filter(|name| !name.is_empty());
        let name = || name.map_or_else(|| crate::t("pdf-empty-the-document"), str::to_owned);
        let (title, message, error) = match state {
            ContentState::Waiting => ("pdf-empty-waiting-title", "pdf-empty-waiting", false),
            ContentState::Unavailable => ("pdf-empty-elsewhere-title", "pdf-empty-elsewhere", true),
            ContentState::Pending | ContentState::Resolved => {
                ("pdf-empty-arriving-title", "pdf-empty-arriving", false)
            }
        };
        Some((
            crate::t(title),
            crate::t_args(message, &[("name", name().into())]),
            error,
        ))
    }

    pub(super) fn open_failure_text(&self) -> Option<String> {
        let error = self.load_error.as_deref()?.to_ascii_lowercase();
        let key = if error.contains("password") || error.contains("encrypt") {
            "pdf-empty-detail-password"
        } else if error.contains("file chooser") {
            "pdf-empty-detail-picker"
        } else if error.contains("engine") || error.contains("worker") {
            "pdf-empty-detail-engine"
        } else if error.contains("unavailable") || error.contains("not a readable pdf") {
            "pdf-empty-detail-unreadable"
        } else {
            "pdf-empty-detail-other"
        };
        Some(crate::t(key))
    }

    pub(super) fn empty_state_nodes(&self) -> Vec<AccessNode> {
        let button = |name: &str, target: &str, bounds: Rect| {
            AccessNode::new(Role::Button)
                .with_name(name)
                .with_action_target(target)
                .with_bounds(bounds)
                .with_state(AccessState::new().focusable(true))
        };
        let heading = |name: &str| {
            AccessNode::new(Role::Heading { level: 1 })
                .with_name(name)
                .with_hashed_key("pdf.empty.heading")
        };
        let mut children = Vec::new();
        if self.opening() {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(crate::t("pdf-empty-opening"))
                    .with_hashed_key("pdf.empty.loading")
                    .with_live(Politeness::Polite),
            );
        } else if let Some((title, message, error)) = self.source_wait_text() {
            children.push(heading(&title));
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(message)
                    .with_hashed_key("pdf.empty.wait")
                    .with_live(if error {
                        Politeness::Assertive
                    } else {
                        Politeness::Polite
                    }),
            );
        } else if let Some(failure) = self.open_failure_text() {
            children.push(heading(&crate::t("pdf-empty-open-failed")));
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(failure)
                    .with_hashed_key("pdf.empty.error")
                    .with_live(Politeness::Assertive),
            );
        } else {
            children.push(heading(&crate::t("pdf-empty-heading")));
            let drawn = self.empty_open_button_rect();
            let bounds = if drawn.area() > 0.0 {
                drawn
            } else {
                self.canvas_rect
            };
            for (index, document) in self.workspace_documents.iter().enumerate() {
                let bounds = self
                    .workspace_document_rows()
                    .get(index)
                    .copied()
                    .unwrap_or(self.canvas_rect);
                children.push(
                    button(
                        &document.name,
                        &format!("pdf.empty.document:{}", document.target),
                        bounds,
                    )
                    .with_description(document.caption.clone()),
                );
            }
            children.push(button(&crate::t("pdf-empty-open"), "pdf.open", bounds));
        }
        children
    }
}
