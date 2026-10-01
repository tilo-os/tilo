//! Containers as a [`tilo_resource::Resource`]: view preferences sync in the
//! shared document; Podman engine rows and in-flight pulls stay live extras
//! when hosted.

use serde::{Deserialize, Serialize};
use ui::automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{
    Action, ActionError, ActionParam, ActionRegistry, ActionSpec, EncodedAction, OperationKind,
    ParamType, Resource as PublishedResource,
};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource};
use ui::widget::Element;

use crate::app::ContainersApp;
use crate::doc::{CreateField, DetailTab, Section, ViewDoc};

/// Collaborative view preferences. Engine inventory and pull jobs are not
/// document state — they ride live extras / standalone overlay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContainersAction {
    SetSection {
        section: String,
    },
    SetSelected {
        id: Option<String>,
    },
    SetTab {
        tab: String,
    },
    /// Replace one create-form draft field.
    SetCreateField {
        field: String,
        value: String,
    },
    /// Clear the create-form draft after a container is submitted.
    ClearCreate,
}

impl Action for ContainersAction {
    fn apply(&self, doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        match self {
            Self::SetSection { section } => {
                ViewDoc::set_section(doc, Section::parse(section));
            }
            Self::SetSelected { id } => {
                ViewDoc::set_selected(doc, id.as_deref());
            }
            Self::SetTab { tab } => {
                ViewDoc::set_tab(doc, DetailTab::parse(tab));
            }
            Self::SetCreateField { field, value } => {
                let Some(field) = CreateField::ALL
                    .into_iter()
                    .find(|candidate| candidate.key() == field)
                else {
                    return Err(ActionError::Refused("unknown create field".into()));
                };
                ViewDoc::set_create_field(doc, field, value);
            }
            Self::ClearCreate => ViewDoc::clear_create(doc),
        }
        Ok(())
    }
}

/// View projection stored in the shared document.
#[derive(Clone, Debug)]
pub struct ContainersState {
    pub section: Section,
    pub selected: Option<String>,
    pub tab: DetailTab,
    pub search: String,
}

impl Default for ContainersState {
    fn default() -> Self {
        Self {
            section: Section::Containers,
            selected: None,
            tab: DetailTab::Logs,
            search: String::new(),
        }
    }
}

/// The containers engine resource.
pub struct Containers;

impl Resource for Containers {
    const KIND: &'static str = "containers.engine";
    const LABEL: &'static str = "Containers";
    const ACTION_TYPE: &'static str = "containers.action";

    type State = ContainersState;
    type Action = ContainersAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<ContainersAction>(Self::ACTION_TYPE);
    }

    fn read(doc: &Doc) -> Result<ContainersState, ModelError> {
        Ok(ContainersState {
            section: ViewDoc::section(doc),
            selected: ViewDoc::selected(doc),
            tab: ViewDoc::tab(doc),
            search: ViewDoc::search(doc),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "set-section",
                "Show section",
                vec![
                    ActionParam::required(
                        "section",
                        ParamType::Text,
                        "containers | images | volumes | distrobox",
                    )
                    .with_choices([
                        "containers",
                        "images",
                        "volumes",
                        "distrobox",
                    ]),
                ],
                |args, _| {
                    let section = args
                        .text("section")
                        .ok_or_else(|| "missing section".to_owned())?
                        .to_owned();
                    Ok(ContainersAction::SetSection { section })
                },
            )
            .describe("Switch the shared containers section."),
            ActionDecl::new(
                "select-container",
                "Select container",
                vec![ActionParam::optional(
                    "id",
                    ParamType::Text,
                    "Container id, or omit to clear selection",
                )],
                |args, _| {
                    let id = args
                        .text("id")
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(ToOwned::to_owned);
                    Ok(ContainersAction::SetSelected { id })
                },
            )
            .describe("Focus a container in the shared selection."),
            ActionDecl::new(
                "set-tab",
                "Set detail tab",
                vec![
                    ActionParam::required(
                        "tab",
                        ParamType::Text,
                        "logs | stats | inspect | terminal",
                    )
                    .with_choices(["logs", "stats", "inspect", "terminal"]),
                ],
                |args, _| {
                    let tab = args
                        .text("tab")
                        .ok_or_else(|| "missing tab".to_owned())?
                        .to_owned();
                    Ok(ContainersAction::SetTab { tab })
                },
            )
            .describe("Switch the shared container detail tab."),
        ]
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Create is a View with no document delta. Per-container lifecycle
        // Effects ride live engine extras (or the standalone overlay).
        match ActionSpec::operation(
            "create",
            "Create container",
            OperationKind::View,
            "containers.engine",
            &["create"],
        ) {
            Ok(spec) => vec![spec],
            Err(_) => Vec::new(),
        }
    }
}

/// Hosted-only extras that are not document state: live engine rows and an
/// in-flight pull. The engine door stays on [`tilo_resource::publish`].
pub(crate) fn live_extras(app: &ContainersApp) -> Vec<PublishedResource> {
    app.live_engine_extras()
}

/// Framework app.
pub struct ContainersShell;

impl ResourceApp for ContainersShell {
    type Resource = Containers;
    const ID: &'static str = "so.tilo.containers";
    const TITLE: &'static str = "Containers";
    const ICON: Option<&'static str> = Some("containers");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Containers>) -> Element {
        cx.surface(ContainersApp::new(true))
    }

    fn scenarios() -> &'static [&'static str] {
        &["confirm", "prompt"]
    }
}

impl ResourceSurface<Containers> for ContainersApp {
    fn present(&mut self, state: &std::rc::Rc<ContainersState>) {
        // Adopt collaborative view prefs only — never clear the local engine.
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Containers>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<Containers>>,
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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Containers>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    pub(crate) fn dispatch(&mut self, doc: &mut AutoCommit, action: &ContainersAction) {
        let context =
            ui::tilo_actions::ActionContext::for_action("so.tilo.containers", "Containers", action);
        match &self.framework {
            Some(dispatcher) => {
                self.last =
                    tilo_actions::EncodedAction::encode(Containers::ACTION_TYPE, action).ok();
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, Containers::ACTION_TYPE, action, context),
        }
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }

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

/// Standalone / `--shots` / unit-test tree via [`tilo_resource::publish`].
pub fn provide_resources(state: &ContainersState) -> Vec<PublishedResource> {
    vec![ui::tilo_resource::publish::<Containers>(
        "containers",
        "Containers",
        state,
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::widget::Widget;

    #[test]
    fn resource_publish_owns_engine_door_and_hosted_keeps_only_live_extras() {
        let mut doc = AutoCommit::new();
        ContainersAction::SetSection {
            section: "images".into(),
        }
        .apply(&mut doc)
        .unwrap();
        ContainersAction::SetSelected {
            id: Some("abc123".into()),
        }
        .apply(&mut doc)
        .unwrap();
        ContainersAction::SetTab {
            tab: "stats".into(),
        }
        .apply(&mut doc)
        .unwrap();
        ViewDoc::set_search(&mut doc, "nginx");
        doc.commit();

        let state = Containers::read(&doc).expect("seeded containers state");
        let published =
            ui::tilo_resource::publish::<Containers>("containers", "Containers", &state);
        assert_eq!(published.kind, "containers.engine");
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "set-section"),
            "set-section declaration must be on the framework resource"
        );
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "select-container"),
            "select-container declaration must be on the framework resource"
        );
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "set-tab"),
            "set-tab declaration must be on the framework resource"
        );
        assert!(
            published.actions.iter().any(|action| action.id == "create"),
            "create View must be on the framework resource"
        );
        assert!(
            published.children.is_empty(),
            "shared projection must not carry live engine rows"
        );
        assert_eq!(state.section, Section::Images);
        assert_eq!(state.selected.as_deref(), Some("abc123"));
        assert_eq!(state.tab, DetailTab::Stats);
        assert_eq!(state.search, "nginx");

        let mut app = ContainersApp::new(false);
        let mut seeded = doc.clone();
        app.sync(&mut seeded, &ui::automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&seeded);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "containers.engine");
        assert!(
            !standalone[0].children.is_empty(),
            "standalone overlays live engine children"
        );
        assert!(
            standalone[0]
                .children
                .iter()
                .any(|child| child.kind == "containers.container"),
            "standalone publishes container rows from the engine"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        assert!(ui::resources::resource(&app, &seeded, "containers").is_none());
        let container_id = standalone[0]
            .children
            .iter()
            .find(|child| child.kind == "containers.container")
            .map(|child| child.id.as_str().to_owned())
            .expect("container");
        assert!(
            ui::resources::resource(&app, &seeded, &container_id).is_some(),
            "a hosted container stays the published row"
        );
        let extras = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "containers.engine"),
            "hosted must not republish containers.engine beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        assert!(
            extras
                .iter()
                .any(|resource| resource.kind == "containers.container"),
            "hosted still publishes live container rows: {:?}",
            extras.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>()
        );

        let door = provide_resources(&state);
        assert_eq!(door[0].kind, "containers.engine");
        assert!(door[0].children.is_empty());

        let mut local = AutoCommit::new();
        let mut standalone = ContainersApp::new(false);
        standalone.commit_view(
            &mut local,
            &ContainersAction::SetSection {
                section: "volumes".into(),
            },
        );
        assert!(
            standalone.last_document_action().is_some(),
            "ActionSink must record document actions on the local host"
        );
        assert_eq!(ViewDoc::section(&local), Section::Volumes);
    }

    #[test]
    fn a_standalone_view_edit_is_submitted_through_the_sink() {
        let mut app = ContainersApp::new(false);
        assert!(!app.is_hosted());
        let mut doc = AutoCommit::new();
        app.apply_view(
            &mut doc,
            &ContainersAction::SetSection {
                section: "images".into(),
            },
        );
        assert!(
            app.last_document_action().is_some(),
            "a standalone view edit is recorded on the action sink"
        );
        assert_eq!(ViewDoc::section(&doc), Section::Images);
    }

    #[test]
    fn create_draft_is_a_replayable_action() {
        let mut doc = AutoCommit::new();
        ContainersAction::SetCreateField {
            field: "image".into(),
            value: "nginx".into(),
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(ViewDoc::create_field(&doc, CreateField::Image), "nginx");
        ContainersAction::ClearCreate.apply(&mut doc).unwrap();
        assert_eq!(ViewDoc::create_field(&doc, CreateField::Image), "");
        assert!(
            ContainersAction::SetCreateField {
                field: "not-a-field".into(),
                value: "x".into(),
            }
            .apply(&mut doc)
            .is_err()
        );
    }
}
