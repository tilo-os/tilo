//! Themes as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path for the theme library.

use automerge::{AutoCommit, ROOT};
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, EncodedAction, OperationKind, Resource as PublishedResource};
use ui::tilo_resource::{Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    THEMES_ACTION_TYPE, ThemesAction, document_bound_actions, document_children, register,
    root_local_operations,
};
use crate::app::ThemesApp;
use crate::doc::{self, KEY_ACTIVE};
use crate::model::ThemeSpec;

/// Catalog plus the active theme id.
#[derive(Clone, Debug, Default)]
pub struct ThemesState {
    pub catalog: Vec<ThemeSpec>,
    pub active: Option<String>,
}

/// The themes library resource.
pub struct Themes;

impl Resource for Themes {
    const KIND: &'static str = "themes.library";
    const LABEL: &'static str = "Themes";
    const ACTION_TYPE: &'static str = THEMES_ACTION_TYPE;

    type State = ThemesState;
    type Action = ThemesAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<ThemesState, ModelError> {
        Ok(ThemesState {
            catalog: doc::load_catalog(doc, &ROOT).unwrap_or_default(),
            active: ui::doc::read_str(doc, &ROOT, KEY_ACTIVE),
        })
    }

    fn bound_actions(state: &ThemesState) -> Vec<ActionSpec> {
        document_bound_actions(state)
    }

    fn children(state: &ThemesState) -> Vec<PublishedResource> {
        document_children(state)
    }

    fn local_operations() -> Vec<ActionSpec> {
        // New / apply-selected / export-selected are Effects with no document
        // delta. Per-theme apply / export ride each child from [`children`].
        root_local_operations()
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }

    fn search(state: &ThemesState) -> Vec<SearchEntry> {
        state
            .catalog
            .iter()
            .map(|theme| SearchEntry {
                anchor: Some(format!("themes.theme:{}", theme.id)),
                title: theme.name.clone(),
                text: theme.id.clone(),
                action: None,
            })
            .collect()
    }
}

/// Hosted-only extras that are not document state: an in-flight apply job and
/// the last export preview. The library door stays on [`tilo_resource::publish`].
pub(crate) fn live_extras(app: &ThemesApp) -> Vec<PublishedResource> {
    let mut extras = Vec::new();
    if let Some(label) = app.live_apply_job_label() {
        extras.push(PublishedResource::new(
            "themes.apply-job",
            "themes.apply-job",
            label,
        ));
    }
    if let Some(preview) = app.live_export_preview_resource() {
        extras.push(preview);
    }
    extras
}

/// Framework app: the themes widget is the surface over [`Themes`].
pub struct ThemesShell;

impl ResourceApp for ThemesShell {
    type Resource = Themes;
    const ID: &'static str = "so.tilo.themes";
    const TITLE: &'static str = "Themes";
    const ICON: Option<&'static str> = Some("themes");
    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    fn view(cx: &mut ViewCx<Themes>) -> Element {
        cx.surface(ThemesApp::new())
    }
}

impl ResourceSurface<Themes> for ThemesApp {
    fn present(&mut self, state: &std::rc::Rc<ThemesState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Themes>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<Themes>>,
    /// Set by [`Self::attach`]; tests can flip it without a real dispatcher.
    pub(crate) hosted: bool,
    pub(crate) last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            hosted: false,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Themes>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &ThemesAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        match &self.framework {
            Some(dispatcher) => {
                self.last = Some(crate::actions::encode(action));
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, type_id, action, context),
        }
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }

    /// Test-only: mark the sink hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{provide_resources, state_from_doc};
    use crate::doc;
    use crate::model::demo_catalog;
    use automerge::ROOT;
    use ui::widget::Widget;

    #[test]
    fn resource_publish_owns_library_door_and_hosted_keeps_only_live_extras() {
        let mut doc = AutoCommit::new();
        doc::save_catalog(&mut doc, &ROOT, &demo_catalog());
        doc.commit();

        let state = Themes::read(&doc).expect("seeded themes state");
        let published = ui::tilo_resource::publish::<Themes>("themes.library", "Themes", &state);
        assert_eq!(published.kind, "themes.library");
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "new-theme"),
            "new-theme bound_action must be on the framework resource"
        );
        assert!(
            published.actions.iter().any(|action| action.id == "new"),
            "new Effect must be on the framework resource"
        );
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "apply-selected"),
            "apply-selected Effect must be on the framework resource"
        );
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "export-selected"),
            "export-selected Effect must be on the framework resource"
        );
        assert!(
            published
                .children
                .iter()
                .any(|child| child.kind == "themes.theme"),
            "theme children must be on the framework resource"
        );
        assert!(
            published
                .children
                .iter()
                .all(|child| child.kind != "themes.apply-job"),
            "shared projection must not carry live apply jobs"
        );

        let mut app = ThemesApp::new_demo();
        let mut seeded = doc.clone();
        app.sync(&mut seeded, &ui::automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&seeded);
        assert_eq!(standalone[0].kind, "themes.library");
        assert!(
            standalone[0]
                .children
                .iter()
                .any(|child| child.kind == "themes.theme"),
            "standalone publishes theme children"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        let extras = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "themes.library"),
            "hosted must not republish themes.library beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );

        let door = provide_resources(&seeded);
        assert_eq!(door[0].kind, "themes.library");
        assert_eq!(state_from_doc(&seeded).catalog.len(), state.catalog.len());
    }
}
