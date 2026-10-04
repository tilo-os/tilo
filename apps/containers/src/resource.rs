//! Containers as a [`tilo_resource::Resource`]: the new-container draft is
//! the document; the podman engine's containers, images, volumes and
//! distroboxes are live rows the surface publishes; moving around the app is
//! the surface's own, offered to every caller as effects.

use std::rc::Rc;

use serde::{Deserialize, Serialize};
use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, EffectStatus, Effected, Fixture, LiveRow, ResourceApp,
    ResourceSurface, RunMode, SurfaceCx, ViewCx,
};
use ui::tilo_actions::{Action, ActionError, Resource as PublishedResource};
use ui::tilo_resource::{
    self, ActionDecl, Doc, Effect, Invoke, ModelError, PlainText, Resource, TextEdit,
    child_resource_id, publish_child,
};
use ui::widget::Element;

use crate::app::ContainersApp;
use crate::doc::{CreateDoc, CreateDraft, CreateField};
use crate::model::{DetailTab, Engine, Section};

/// An edit of the new-container draft.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContainersAction {
    /// Edit one field of the draft.
    EditCreate { field: CreateField, edit: TextEdit },
    /// Empty the draft, once a container was created from it.
    ClearCreate,
}

impl Action for ContainersAction {
    fn apply(&self, doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        match self {
            Self::EditCreate { field, edit } => {
                let text = CreateDoc::field_obj_mut(doc, *field)?;
                edit.apply_under(doc, &text)
            }
            Self::ClearCreate => {
                for field in CreateField::ALL {
                    if let Some(text) = CreateDoc::field_obj(doc, field) {
                        TextEdit::ReplaceText {
                            text: String::new(),
                        }
                        .apply_under(doc, &text)?;
                    }
                }
                Ok(())
            }
        }
    }
}

/// What the document holds: the new-container draft.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContainersState {
    pub draft: CreateDraft,
}

/// The containers engine resource.
pub struct Containers;

impl Resource for Containers {
    const KIND: &'static str = "containers.engine";
    const LABEL: &'static str = "Containers";
    const ACTION_TYPE: &'static str = "containers.action";

    type State = ContainersState;
    type Action = ContainersAction;

    fn read(doc: &Doc) -> Result<ContainersState, ModelError> {
        Ok(ContainersState {
            draft: CreateDoc::read(doc),
        })
    }

    /// Every replica types into the same draft texts.
    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        CreateDoc::initialize(doc).map_err(|error| ModelError::invalid(error.to_string()))
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new("clear-create", "Clear new container", vec![], |_, _| {
                Ok(ContainersAction::ClearCreate)
            })
            .describe("Empty every field of the new-container draft."),
        ]
    }

    /// Each field of the draft is a plain text of its own, so the palette,
    /// agents and scripts fill the form in like someone typing.
    fn children(state: &ContainersState) -> Vec<PublishedResource> {
        CreateField::ALL
            .into_iter()
            .map(|field| {
                publish_child::<PlainText, Self>(
                    &create_field_id(field),
                    &format!("New container {}", field.title()),
                    Some(&state.draft.get(field).to_owned()),
                    move |edit| ContainersAction::EditCreate { field, edit },
                )
            })
            .collect()
    }
}

/// The id a draft field is published under, as a plain-text child.
pub(crate) fn create_field_id(field: CreateField) -> String {
    child_resource_id::<Containers>(&format!("create-{}", field.key()))
}

/// Edits of one draft field through the resource's dispatcher.
pub(crate) fn create_field_dispatcher(
    dispatcher: &Dispatcher<Containers>,
    field: CreateField,
) -> Dispatcher<PlainText> {
    dispatcher.child::<PlainText>(&create_field_id(field), move |edit| {
        ContainersAction::EditCreate { field, edit }
    })
}

/// Moving around the app, for every caller: what the rail, the list and the
/// detail tabs do. None of it changes the document; it is where this window
/// is.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "containers.view")]
pub enum ContainersEffect {
    /// Show section.
    #[effect(permission = "read")]
    SetSection {
        #[param("containers | images | volumes | distrobox")]
        section: String,
    },
    /// Select container.
    #[effect(permission = "read")]
    SelectContainer {
        #[param("Container id, or omit to clear the selection")]
        id: Option<String>,
    },
    /// Set detail tab.
    #[effect(permission = "read")]
    SetTab {
        #[param("logs | stats | inspect | terminal")]
        tab: String,
    },
    /// Create container.
    #[effect(permission = "read")]
    Create,
}

/// The screenshots: the default container detail, each section and detail
/// tab, the create form (with its image suggestions open), a pull in
/// progress, the collapsed rail, no engine at all, the search palette, and
/// the confirm and text dialogs.
const SCENARIOS: &[&str] = &[
    "images",
    "volumes",
    "distrobox",
    "stats",
    "inspect",
    "terminal",
    "create",
    "combo",
    "pull",
    "collapsed",
    "empty",
    "palette",
    "confirm",
    "prompt",
];

/// Framework app.
pub struct ContainersShell;

impl ResourceApp for ContainersShell {
    type Resource = Containers;
    const ID: &'static str = "so.tilo.containers";
    const TITLE: &'static str = "Containers";
    const ICON: Option<&'static str> = Some("containers");

    fn view(cx: &mut ViewCx<Containers>) -> Element {
        cx.surface(ContainersApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    /// A screenshot shows a made-up engine (or, for `empty`, none at all)
    /// instead of the podman of the machine it is taken on.
    fn fixture(fixture: &mut Fixture<Containers>) {
        fixture.provide(if fixture.scenario() == Some("empty") {
            Engine {
                connection: "No container engine — is podman installed?".into(),
                live: false,
                ..Engine::default()
            }
        } else {
            crate::model::sample_engine()
        });
    }

    /// The create form is shown filled in.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Containers>> {
        if !matches!(name, Some("create" | "combo")) {
            return Vec::new();
        }
        [
            (CreateField::Name, "web-frontend"),
            (CreateField::Image, "nginx"),
            (CreateField::Ports, "8080:80, 8443:443"),
            (CreateField::Env, "TZ=UTC"),
            (CreateField::Volumes, "site:/usr/share/nginx/html"),
        ]
        .into_iter()
        .filter_map(|(field, text)| {
            Invoke::apply(&ContainersAction::EditCreate {
                field,
                edit: TextEdit::ReplaceText { text: text.into() },
            })
            .ok()
        })
        .collect()
    }
}

impl ResourceSurface<Containers, ContainersEffect> for ContainersApp {
    fn present(&mut self, state: &Rc<ContainersState>) {
        self.present_draft(&state.draft);
    }

    fn attach(&mut self, cx: SurfaceCx<Containers>) {
        let dispatcher = cx.dispatcher();
        match cx.mode() {
            RunMode::Live => self.connect(),
            RunMode::Capture => {
                if let Some(engine) = cx.fixture::<Engine>() {
                    self.show_engine((*engine).clone());
                }
            }
            RunMode::Past => self.show_past(),
        }
        self.attach_dispatcher(dispatcher);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scene(scenario);
    }

    fn effect(
        &mut self,
        effect: ContainersEffect,
        _cx: &mut EffectCx<Containers, Self>,
    ) -> EffectAnswer {
        match effect {
            ContainersEffect::SetSection { section } => {
                let section = Section::parse(&section)
                    .ok_or_else(|| format!("there is no section `{section}`"))?;
                self.show_section(section);
            }
            ContainersEffect::SelectContainer { id } => {
                let id = id
                    .map(|id| id.trim().to_owned())
                    .filter(|id| !id.is_empty());
                if let Some(id) = &id
                    && self.engine.find(id).is_none()
                {
                    return Err(format!("there is no container `{id}`"));
                }
                self.select_container(id);
            }
            ContainersEffect::SetTab { tab } => {
                let tab =
                    DetailTab::parse(&tab).ok_or_else(|| format!("there is no tab `{tab}`"))?;
                self.show_tab(tab);
            }
            ContainersEffect::Create => self.open_create(),
        }
        Ok(Effected::done())
    }

    fn effects(&self) -> Vec<EffectStatus> {
        if self.engine.live {
            Vec::new()
        } else {
            vec![EffectStatus::unavailable(
                ContainersEffect::CREATE,
                "engine_offline",
                "Creating containers requires a connected Podman engine",
            )]
        }
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.engine_rows()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.engine_row(id)
    }
}

/// The resource as the palette, agents and scripts see it with `state`,
/// before any surface adds its rows.
#[cfg(test)]
fn published(state: &ContainersState) -> PublishedResource {
    tilo_resource::publish::<Containers>(Containers::KIND, Containers::LABEL, state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::prelude::TestApp;
    use ui::tilo_resource::Replica;

    fn draft(app: &TestApp<ContainersShell>, field: CreateField) -> String {
        app.state().draft.get(field).to_owned()
    }

    #[test]
    fn the_draft_is_edited_by_splices_and_cleared_whole() {
        let mut replica = Replica::<Containers>::new();
        replica
            .apply(
                Invoke::apply(&ContainersAction::EditCreate {
                    field: CreateField::Image,
                    edit: TextEdit::ReplaceText {
                        text: "nginx".into(),
                    },
                })
                .unwrap(),
            )
            .unwrap();
        replica
            .apply(
                Invoke::apply(&ContainersAction::EditCreate {
                    field: CreateField::Image,
                    edit: TextEdit::Splice {
                        at: 5,
                        delete: 0,
                        insert: ":1".into(),
                    },
                })
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            replica.state().unwrap().draft.get(CreateField::Image),
            "nginx:1"
        );

        replica
            .apply(Invoke::apply(&ContainersAction::ClearCreate).unwrap())
            .unwrap();
        assert!(replica.state().unwrap().draft.is_empty());
    }

    #[test]
    fn the_published_resource_offers_the_draft_and_no_view_state() {
        let state = ContainersState {
            draft: CreateDraft::with(&[(CreateField::Image, "nginx")]),
        };
        let published = published(&state);
        assert_eq!(published.kind, Containers::KIND);
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "clear-create"),
            "clearing the draft is an action"
        );
        for field in CreateField::ALL {
            let child = published
                .children
                .iter()
                .find(|child| child.id.as_str() == create_field_id(field))
                .unwrap_or_else(|| panic!("the {} field is a child", field.key()));
            assert!(
                child
                    .actions
                    .iter()
                    .any(|action| action.id == "replace-text"),
                "{} is filled in like a text",
                field.key()
            );
        }
        for gone in ["set-section", "select-container", "set-tab"] {
            assert!(
                published.actions.iter().all(|action| action.id != gone),
                "`{gone}` is where a window is, not a document action"
            );
        }
    }

    /// An agent fills the form in through the published draft fields and
    /// clears it with the declared action; undo takes the clear back.
    #[test]
    fn agents_fill_the_draft_and_undo_takes_a_clear_back() {
        let mut app = TestApp::<ContainersShell>::capture(None).unwrap();
        app.invoke_child::<PlainText>(
            &create_field_id(CreateField::Image),
            Invoke::<PlainText>::apply(&TextEdit::ReplaceText {
                text: "quay.io/example/app:2".into(),
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(draft(&app, CreateField::Image), "quay.io/example/app:2");

        app.call("clear-create", serde_json::json!({})).unwrap();
        assert!(app.state().draft.is_empty());
        assert!(app.undo());
        assert_eq!(draft(&app, CreateField::Image), "quay.io/example/app:2");
    }

    /// Every screenshot is the framework's capture of the sample engine,
    /// staged where the scenario shows.
    #[test]
    fn captures_stage_the_sample_engine() {
        let mut detail = TestApp::<ContainersShell>::capture(None).unwrap();
        assert_eq!(detail.harness().inspection().viewport, [1280, 800]);
        let frame = detail.harness().inspection_json().unwrap();
        assert!(frame.contains("web-frontend"), "{frame}");
        assert!(detail.state().draft.is_empty());

        let create = TestApp::<ContainersShell>::capture(Some("create")).unwrap();
        assert_eq!(draft(&create, CreateField::Image), "nginx");
        assert_eq!(draft(&create, CreateField::Ports), "8080:80, 8443:443");

        let mut empty = TestApp::<ContainersShell>::capture(Some("empty")).unwrap();
        let frame = empty.harness().inspection_json().unwrap();
        assert!(!frame.contains("web-frontend"), "{frame}");
    }
}
