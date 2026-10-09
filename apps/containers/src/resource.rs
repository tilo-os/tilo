//! Containers as a resource: the new-container draft is the record
//! ([`Containers`]), each field edited at its path; the podman engine's
//! containers, images, volumes and distroboxes are live rows the surface
//! publishes; moving around the app is the surface's own, offered to every
//! caller as effects.

use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, Fixture, LiveRow, ResourceApp, ResourceSurface,
    RunMode, SurfaceCx, ViewCx,
};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::{self, Effect, Invoke, Model, resource};
use ui::widget::Element;

use crate::app::ContainersApp;
use crate::draft::{CreateDraft, CreateField};
use crate::model::{DetailTab, Engine, Section};

/// Containers' Fluent catalogs, source locale first.
pub const LOCALES: &[(&str, &str)] = &[("en-US", include_str!("../locales/en-US.ftl"))];

static CATALOG: EmbeddedCatalog =
    EmbeddedCatalog::new("en-US", LOCALES, "containers-clear-create").with_system_locale();

/// What the record holds: the new-container draft.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
pub struct Containers {
    /// New container
    pub draft: CreateDraft,
}

#[resource(kind = "containers.engine", label = "Containers")]
impl Containers {
    /// Clear new container
    ///
    /// Empty every field of the new-container draft.
    #[action]
    pub fn clear_create(&mut self) {
        self.draft = CreateDraft::default();
    }
}

/// Moving around the app, for every caller: what the rail, the list and the
/// detail tabs do. None of it changes the document; it is where this window
/// is.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "containers.view")]
pub enum ContainersEffect {
    /// Show section.
    #[effect(view, kind = "read")]
    SetSection {
        #[param("containers | images | volumes | distrobox")]
        section: String,
    },
    /// Select container.
    #[effect(view, kind = "read")]
    SelectContainer {
        #[param("Container id, or omit to clear the selection")]
        id: Option<String>,
    },
    /// Set detail tab.
    #[effect(view, kind = "read")]
    SetTab {
        #[param("logs | stats | inspect | terminal")]
        tab: String,
    },
    /// Create container.
    #[effect(kind = "read")]
    Create,
    /// Open logs.
    #[effect(view, row)]
    OpenLogs {
        #[param("Container id")]
        container: String,
    },
    /// Start.
    #[effect(row, idempotent, privileged)]
    Start {
        #[param("Container id")]
        container: String,
    },
    /// Stop.
    #[effect(row, idempotent, privileged)]
    Stop {
        #[param("Container id")]
        container: String,
    },
    /// Restart.
    #[effect(row, privileged)]
    Restart {
        #[param("Container id")]
        container: String,
    },
    /// Pause.
    #[effect(row, idempotent, privileged)]
    Pause {
        #[param("Container id")]
        container: String,
    },
    /// Resume.
    #[effect(row, idempotent, privileged)]
    Unpause {
        #[param("Container id")]
        container: String,
    },
    /// Remove.
    #[effect(row, destructive)]
    RemoveContainer {
        #[param("Container id")]
        container: String,
    },
    /// Open shell.
    #[effect(row, privileged)]
    Shell {
        #[param("Container id")]
        container: String,
    },
    /// Show image.
    #[effect(view, row)]
    ShowImage {
        #[param("Image reference")]
        image: String,
    },
    /// Remove image.
    #[effect(row, destructive)]
    RemoveImage {
        #[param("Image reference")]
        image: String,
    },
    /// Show volume.
    #[effect(view, row)]
    ShowVolume {
        #[param("Volume name")]
        volume: String,
    },
    /// Remove volume.
    #[effect(row, destructive)]
    RemoveVolume {
        #[param("Volume name")]
        volume: String,
    },
    /// Show distrobox.
    #[effect(view, row)]
    ShowDistrobox {
        #[param("Distrobox name")]
        distrobox: String,
    },
    /// Enter distrobox.
    #[effect(row, privileged)]
    EnterDistrobox {
        #[param("Distrobox name")]
        distrobox: String,
    },
}

impl ContainersEffect {
    /// The engine operation a row effect asks for: its target's kind, the
    /// verb, and the target.
    pub(crate) fn engine_operation(&self) -> Option<(&'static str, &'static str, &str)> {
        Some(match self {
            Self::Start { container } => ("containers.container", "start", container),
            Self::Stop { container } => ("containers.container", "stop", container),
            Self::Restart { container } => ("containers.container", "restart", container),
            Self::Pause { container } => ("containers.container", "pause", container),
            Self::Unpause { container } => ("containers.container", "unpause", container),
            Self::RemoveContainer { container } => ("containers.container", "remove", container),
            Self::Shell { container } => ("containers.container", "shell", container),
            Self::RemoveImage { image } => ("containers.image", "remove", image),
            Self::RemoveVolume { volume } => ("containers.volume", "remove", volume),
            Self::EnterDistrobox { distrobox } => ("containers.distrobox", "enter", distrobox),
            _ => return None,
        })
    }
}

/// The screenshots: the default container detail, each section and detail
/// tab, the new-container form, a pull in progress, no engine at all, and
/// the confirm and text dialogs.
const SCENARIOS: &[&str] = &[
    "confirm",
    "prompt",
    "create",
    "images",
    "volumes",
    "distrobox",
    "stats",
    "inspect",
    "terminal",
    "pull",
    "empty",
];

/// Framework app.
pub struct ContainersShell;

impl ResourceApp for ContainersShell {
    type Resource = Containers;
    const ID: &'static str = "so.tilo.containers";
    const TITLE: &'static str = "Containers";
    const ICON: Option<&'static str> = Some("containers");

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn view(cx: &mut ViewCx<Containers>) -> Element {
        let app = ContainersApp::new();
        // A search hit, or an agent's `open`, shows the row it names.
        cx.on_open(app.opener());
        cx.surface(app)
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
                connection: crate::podman::UNREACHABLE.into(),
                live: false,
                ..Engine::default()
            }
        } else {
            crate::model::sample_engine()
        });
    }

    /// The create form is shown filled in.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Containers>> {
        if name != Some("create") {
            return Vec::new();
        }
        [
            (CreateField::Name, "web-frontend"),
            (CreateField::Image, "docker.io/library/nginx:1.27-alpine"),
            (CreateField::Ports, "8080:80, 8443:443"),
            (CreateField::Env, "TZ=UTC"),
            (CreateField::Volumes, "site:/usr/share/nginx/html"),
        ]
        .into_iter()
        .filter_map(|(field, text)| Invoke::edit(&field.path().replace(text)).ok())
        .collect()
    }
}

impl ResourceSurface<Containers, ContainersEffect> for ContainersApp {
    fn present(&mut self, state: &Rc<Containers>) {
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
        self.perform_effect(effect).map(|()| Effected::done())
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

    /// Containers with their recent logs, images, volumes and distroboxes:
    /// what the engine holds, not the document.
    fn search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        crate::search::engine_entries(&self.engine)
    }
}

impl ContainersApp {
    /// Do what `effect` asks, for any caller: the rows, the palette, an
    /// agent.
    ///
    /// # Errors
    /// Returns why it could not be done.
    pub fn perform_effect(&mut self, effect: ContainersEffect) -> Result<(), String> {
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
            ContainersEffect::OpenLogs { container } => {
                self.select_container(Some(container));
                self.show_tab(DetailTab::Logs);
            }
            ContainersEffect::ShowImage { .. } => self.show_section(Section::Images),
            ContainersEffect::ShowVolume { .. } => self.show_section(Section::Volumes),
            ContainersEffect::ShowDistrobox { .. } => self.show_section(Section::Distrobox),
            engine => {
                if let Some((kind, verb, target)) = engine.engine_operation() {
                    self.run_engine_operation(kind, verb, target)?;
                }
            }
        }
        Ok(())
    }
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
        let image = CreateField::Image.path();
        replica
            .apply(Invoke::edit(&image.replace("nginx")).unwrap())
            .unwrap();
        replica
            .apply(Invoke::edit(&image.splice(5, 0, ":1")).unwrap())
            .unwrap();
        assert_eq!(
            replica.state().unwrap().draft.get(CreateField::Image),
            "nginx:1"
        );

        replica.apply(Containers::CLEAR_CREATE.with(())).unwrap();
        assert!(replica.state().unwrap().draft.is_empty());
    }

    /// Two people typing in the form at once converge per character.
    #[test]
    fn concurrent_typing_converges() {
        let mut ours = Replica::<Containers>::new();
        let image = CreateField::Image.path();
        ours.apply(Invoke::edit(&image.replace("nginx")).unwrap())
            .unwrap();
        let mut theirs = ours.fork();
        ours.apply(Invoke::edit(&image.splice(5, 0, ":1")).unwrap())
            .unwrap();
        theirs
            .apply(Invoke::edit(&image.splice(0, 0, "docker.io/")).unwrap())
            .unwrap();
        theirs
            .apply(Invoke::edit(&CreateField::Name.path().replace("web")).unwrap())
            .unwrap();
        ours.sync_with(&mut theirs).unwrap();
        assert_eq!(ours.state().unwrap(), theirs.state().unwrap());
        let merged = ours.state().unwrap().draft;
        assert_eq!(merged.get(CreateField::Image), "docker.io/nginx:1");
        assert_eq!(merged.get(CreateField::Name), "web");
    }

    #[test]
    fn the_published_resource_offers_the_draft_and_no_view_state() {
        let mut replica = Replica::<Containers>::new();
        replica
            .apply(Invoke::edit(&CreateField::Image.path().replace("nginx")).unwrap())
            .unwrap();
        let published = replica.published().unwrap();
        assert_eq!(
            published.kind,
            <Containers as tilo_resource::Resource>::KIND
        );
        for id in ["clear-create", "field-edit"] {
            assert!(
                published.actions.iter().any(|action| action.id == id),
                "{id} is offered"
            );
        }
        for gone in ["set-section", "select-container", "set-tab"] {
            assert!(
                published.actions.iter().all(|action| action.id != gone),
                "`{gone}` is where a window is, not a document action"
            );
        }
    }

    /// An agent fills the form in with field edits and clears it with the
    /// verb, one call each; undo takes the clear back.
    #[test]
    fn agents_fill_the_draft_and_undo_takes_a_clear_back() {
        let mut app = TestApp::<ContainersShell>::capture(None).unwrap();
        let edit = CreateField::Image.path().replace("quay.io/example/app:2");
        app.call("field-edit", serde_json::json!({ "edit": edit }))
            .unwrap();
        assert_eq!(draft(&app, CreateField::Image), "quay.io/example/app:2");

        app.call("clear-create", serde_json::json!({})).unwrap();
        assert!(app.state().draft.is_empty());
        assert!(app.undo());
        assert_eq!(draft(&app, CreateField::Image), "quay.io/example/app:2");
    }

    /// What an agent writes into the form is history the person reads as
    /// the app's own sentences.
    #[test]
    fn history_entries_render_as_the_apps_sentences() {
        use ui::tilo_actions::{ActionInvoker, ActorKind};

        let mut app = TestApp::<ContainersShell>::capture(None).unwrap();
        let agent = ActionInvoker::actor("agent:claude")
            .with_kind(ActorKind::Agent)
            .acting_for("ada");
        let kind = <Containers as tilo_resource::Resource>::KIND;
        for edit in [
            CreateField::Image.path().replace("nginx:alpine"),
            CreateField::Ports.path().replace("8080:80"),
        ] {
            app.call_by(
                agent.clone(),
                kind,
                "field-edit",
                serde_json::json!({ "edit": edit }),
            )
            .unwrap();
        }
        app.call_by(agent, kind, "clear-create", serde_json::json!({}))
            .unwrap();
        let catalogue = tilo_history::Catalogue::new("en-US", &[LOCALES[0].1]);
        let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
        let entries: Vec<_> = app
            .versions()
            .into_iter()
            .filter_map(|version| version.context)
            .filter(|entry| {
                entry
                    .invoker
                    .as_ref()
                    .is_some_and(|invoker| invoker.actor == "agent:claude")
            })
            .collect();
        let sentences: Vec<String> = entries.iter().map(|entry| reader.sentence(entry)).collect();
        assert_eq!(
            sentences,
            [
                "Chose the image of the new container",
                "Changed the ports of the new container",
                "Cleared the new container",
            ]
        );
        let invoker = entries[0].invoker.as_ref().unwrap();
        assert_eq!(
            (invoker.kind, invoker.on_behalf_of.as_deref()),
            (ActorKind::Agent, Some("ada"))
        );
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
        assert_eq!(
            draft(&create, CreateField::Image),
            "docker.io/library/nginx:1.27-alpine"
        );
        assert_eq!(draft(&create, CreateField::Ports), "8080:80, 8443:443");

        let mut empty = TestApp::<ContainersShell>::capture(Some("empty")).unwrap();
        let frame = empty.harness().inspection_json().unwrap();
        assert!(!frame.contains("web-frontend"), "{frame}");
    }
}
