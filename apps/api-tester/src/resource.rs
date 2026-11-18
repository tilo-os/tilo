//! Framework app: the window around the workspace resource, which lives in `api-tester-model`
//! with its verbs.

use ui::framework::{ResourceApp, ViewCx};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::Invoke;
use ui::widget::Element;

pub use crate::workspace::Workspace;
// The parameter structs each verb takes, generated beside its impl.
pub use api_tester_model::workspace_verbs::workspace;

/// Framework app.
pub struct ApiTesterShell;

impl ApiTesterShell {
    /// The view state a screenshot scenario stages: the sample request and
    /// its response (`default`, the response's headers), a contextual form,
    /// the rail, the URL's focus, a sent request's actions, a failed send.
    pub const SCENARIOS: &'static [&'static str] = &[
        "default",
        "response-headers",
        "empty",
        "rail-open",
        "error",
        "focus-url",
        "form-new-collection",
        "form-save-request",
        "form-rename-collection",
        "form-rename-request",
        "form-new-environment",
        "form-rename-environment",
        "form-new-variable",
        "form-delete-variable",
        "history-actions",
    ];
}

impl ResourceApp for ApiTesterShell {
    type Resource = Workspace;
    const ID: &'static str = ui::apps::API_TESTER;
    const TITLE: &'static str = "API Tester";
    const TITLE_KEY: &'static str = "api-tester-app-title";
    const ICON: Option<&'static str> = Some("api-tester");

    fn view(cx: &mut ViewCx<Workspace>) -> Element {
        crate::app::window(cx)
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    fn scenarios() -> &'static [&'static str] {
        Self::SCENARIOS
    }

    /// Every scenario but the empty one shows the sample workspace.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Workspace>> {
        if name == Some("empty") {
            return Vec::new();
        }
        crate::seed::workspace()
    }
}

#[cfg(test)]
mod tests;

ui::reach_tests!(ApiTesterShell);
