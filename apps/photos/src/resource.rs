//! Photos as a [`tilo_resource::Resource`]: the shared library is the state,
//! edits and membership are document actions, and the gallery widget is the
//! surface. The framework host publishes through [`tilo_resource::publish`].

use std::path::PathBuf;
use std::rc::Rc;

use tilo_content::ContentStore;
use ui::framework::{
    EffectAnswer, EffectCx, Effected, Fixture, LaunchInput, LiveRow, ResourceApp, ResourceSurface,
    SurfaceCx, ViewCx,
};
use ui::tilo_resource::Effect as _;
use ui::tilo_resource::effect::Answers;
use ui::tilo_resource::{ActionDecl, Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{PHOTOS_ACTION_TYPE, PhotosAction, PhotosEffect, register};
use crate::app::PhotosApp;
use crate::doc::PhotosDoc;
use crate::model::{LibraryPhoto, OpenImage, PhotoLibrary};

/// The shared photograph library: every photograph, how it is filed and
/// the edits it is shown with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PhotosState {
    pub library: PhotoLibrary,
}

/// The photos library resource.
pub struct Photos;

impl Resource for Photos {
    const KIND: &'static str = "photos.library";
    const LABEL: &'static str = "Photos";
    const ACTION_TYPE: &'static str = PHOTOS_ACTION_TYPE;

    type State = PhotosState;
    type Action = PhotosAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<PhotosState, ModelError> {
        Ok(PhotosState {
            library: PhotosDoc::library(doc),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        crate::actions::library_declarations()
    }

    fn children(state: &PhotosState) -> Vec<tilo_actions::Resource> {
        crate::actions::library_children(&state.library)
    }

    fn child(state: &PhotosState, id: &str) -> Option<tilo_actions::Resource> {
        crate::actions::library_child(&state.library, id)
    }

    /// Every photograph by its name and everything shared about it — the
    /// people, tags, folder, place, capture date and analysed text — and the
    /// timeline, its months and days, places, albums, folders and labels by
    /// theirs. Choosing a hit opens it.
    fn search(state: &PhotosState) -> Vec<SearchEntry> {
        crate::search::library_search_entries(&state.library)
    }
}

/// Framework app: the photos widget is the surface over [`Photos`].
pub struct PhotosShell;

impl ResourceApp for PhotosShell {
    type Resource = Photos;
    const ID: &'static str = "so.tilo.photos";
    const TITLE: &'static str = "Photos";
    const ICON: Option<&'static str> = Some("photos");
    const OPENS: &'static [&'static str] = &["image/png", "image/jpeg", "image/webp"];
    /// A photograph to open, or a folder of them to import, into the library.
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Photos>) -> Element {
        let content = cx.content().unwrap_or_else(ContentStore::memory);
        let app = if cx.mode().is_live() {
            PhotosApp::new_persistent(content)
                .launched_on(cx.launch_arguments().first().map(PathBuf::from))
        } else {
            PhotosApp::with_content(content)
        };
        cx.surface(app)
    }

    /// A photograph handed to the launch is kept in the content store and
    /// imported into the library; the window shows it, and imports a folder
    /// ([`PhotosApp::launched_on`]).
    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<Photos>> {
        let (Some(file), Some(content)) = (input.file(), input.content()) else {
            return Vec::new();
        };
        let Some(image) = crate::pixels::decode(file.bytes()) else {
            tracing::warn!(
                name = file.name(),
                "photos: the launched file is not an image"
            );
            return Vec::new();
        };
        let hash = match content.put(file.bytes()) {
            Ok(hash) => hash,
            Err(error) => {
                tracing::warn!(%error, "photos: could not keep the launched photograph");
                return Vec::new();
            }
        };
        let open = OpenImage::from_path(file.name(), image.w, image.h)
            .with_content(hash)
            .portable();
        Invoke::apply(&PhotosAction::ImportPhoto {
            photo: LibraryPhoto::from_image(open),
        })
        .into_iter()
        .collect()
    }

    fn scenarios() -> &'static [&'static str] {
        crate::sample::SCENARIOS
    }

    /// The photographs a scenario shows live in the content store, not the
    /// document: their bytes are put there before the view is built.
    fn fixture(fixture: &mut Fixture<Photos>) {
        crate::sample::put_content(fixture.content());
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Photos>> {
        crate::sample::scenario(name)
            .iter()
            .filter_map(|action| Invoke::apply(action).ok())
            .collect()
    }
}

impl ResourceSurface<Photos, PhotosEffect> for PhotosApp {
    fn present(&mut self, state: &Rc<PhotosState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Photos>) {
        self.attach_framework(cx.dispatcher(), cx.actions());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario.unwrap_or("opened"));
    }

    fn effect(&mut self, effect: PhotosEffect, cx: &mut EffectCx<Photos, Self>) -> EffectAnswer {
        let job = PhotosEffect::declarations()
            .iter()
            .any(|decl| decl.id == effect.id() && matches!(decl.answers, Answers::Later { .. }));
        let invocation = cx.invocation().unwrap_or_default();
        // A job's work reports under its invocation from the moment it
        // starts, so its answer is kept before the work begins.
        if job {
            let reply = cx.reply();
            self.register_reply(invocation, reply);
        }
        let state = cx.live().get();
        match self.answer_effect(effect, invocation, &state.library) {
            Ok(Some(output)) if job => {
                self.settle_now(invocation, Ok(output));
                Ok(Effected::Pending)
            }
            Ok(Some(output)) => Ok(Effected::Done(output)),
            Ok(None) if job => Ok(Effected::Pending),
            Ok(None) => Ok(Effected::done()),
            Err(error) => {
                self.forget_reply(invocation);
                Err(error)
            }
        }
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.search_status_row().into_iter().collect()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.search_status_row().filter(|row| row.id() == id)
    }
}

#[cfg(test)]
mod tests {
    use ui::framework::testing::TestApp;

    use super::*;

    fn surface<T>(app: &mut TestApp<PhotosShell>, read: impl FnOnce(&mut PhotosApp) -> T) -> T {
        app.surface(read)
            .expect("the window hosts the photos surface")
    }

    #[test]
    fn every_scenario_stages_through_the_library_and_settles() {
        for &name in crate::sample::SCENARIOS {
            let mut app = TestApp::<PhotosShell>::capture(Some(name))
                .unwrap_or_else(|error| panic!("{name} does not capture: {error}"));
            let state = app.state();
            let library = crate::sample::shows_library(name);
            match name {
                "empty" => assert!(state.library.photos.is_empty()),
                "loading" | "error" | "opened" | "cropping" | "edited" => {
                    assert_eq!(state.library.photos.len(), 1, "{name}");
                }
                _ => assert!(state.library.photos.len() > 1, "{name} shows a library"),
            }
            let opened = surface(&mut app, |photos| photos.open_photo_id().map(str::to_owned));
            assert_eq!(
                opened,
                crate::sample::opened(name),
                "{name} opens its photograph"
            );
            let shown_edits = opened
                .and_then(|id| state.library.photo(&id).map(|photo| photo.edits))
                .unwrap_or_default();
            assert_eq!(
                !shown_edits.is_identity(),
                matches!(name, "cropping" | "edited"),
                "{name} shows an edit exactly when it stages one"
            );
            let (in_library, has_image) = surface(&mut app, |photos| {
                (photos.shows_library(), photos.has_image())
            });
            assert_eq!(in_library, library, "{name} poses the library view");
            assert_eq!(
                has_image,
                !matches!(name, "empty" | "loading" | "error"),
                "{name} draws its photograph only when its bytes are an image here"
            );
        }
    }

    #[test]
    fn a_launched_photograph_is_imported_into_the_library_and_opened() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("lake.png");
        let png = crate::pixels::encode_png(&crate::shots_pattern(30, 20)).unwrap();
        std::fs::write(&path, &png).unwrap();
        let mut app = TestApp::<PhotosShell>::launched(&[path.to_str().unwrap()]).unwrap();
        app.harness().relayout();
        let state = app.state();
        assert_eq!(state.library.photos.len(), 1);
        let hash = tilo_content::hash_bytes(&png);
        assert_eq!(
            state.library.photos[0].image.content.as_deref(),
            Some(hash.as_str())
        );
        assert_eq!(state.library.photos[0].image.name, "lake.png");
        let opened = surface(&mut app, |photos| photos.open_photo_id().map(str::to_owned));
        assert_eq!(opened.as_deref(), Some(state.library.photos[0].id.as_str()));
        assert!(surface(&mut app, |photos| photos.has_image()));
    }

    #[test]
    fn effects_reach_the_window_from_the_library_and_from_its_photographs() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("lake.png");
        let png = crate::pixels::encode_png(&crate::shots_pattern(30, 20)).unwrap();
        std::fs::write(&path, &png).unwrap();
        let mut app = TestApp::<PhotosShell>::launched(&[path.to_str().unwrap()]).unwrap();
        app.harness().relayout();
        let photo = app.state().library.photos[0].clone();

        app.effect(&PhotosEffect::ViewLibrary)
            .expect("the library opens");
        assert!(surface(&mut app, |photos| photos.shows_library()));

        // A photograph's own row opens it, as the palette or an agent does:
        // the row offers the effect under the effect's own id.
        app.effect_on(
            &photo.id,
            &PhotosEffect::View {
                photo_id: photo.id.clone(),
            },
        )
        .expect("the photograph opens from its row");
        app.harness().relayout();
        assert!(!surface(&mut app, |photos| photos.shows_library()));
        assert_eq!(
            surface(&mut app, |photos| photos.open_photo_id().map(str::to_owned)).as_deref(),
            Some(photo.id.as_str())
        );

        // The edited pixels come out as content of their own.
        app.invoke(
            Invoke::apply(&PhotosAction::set(
                &photo.id,
                &crate::model::Edits::default().rotated_right(),
            ))
            .unwrap(),
        )
        .unwrap();
        let edits = crate::model::Edits::default().rotated_right();
        let output = app
            .effect_on(
                &photo.id,
                &PhotosEffect::ExportEditedContent {
                    resource_id: photo.id.clone(),
                    source_content: photo.image.content.clone().unwrap(),
                    edits,
                },
            )
            .expect("the photograph exports its edits");
        let produced = tilo_actions::content_hash(&output.resources[0]).expect("content");
        assert_ne!(Some(produced), photo.image.content.as_deref());
    }

    #[test]
    fn the_windows_controls_are_bound_to_its_effects() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("lake.png");
        let png = crate::pixels::encode_png(&crate::shots_pattern(30, 20)).unwrap();
        std::fs::write(&path, &png).unwrap();
        let mut app = TestApp::<PhotosShell>::launched(&[path.to_str().unwrap()]).unwrap();
        app.harness().relayout();
        let photo = app.state().library.photos[0].id.clone();

        // A toolbar control reaches the window through its effect, as every
        // other caller does.
        app.effect(&PhotosEffect::RotateShownRight)
            .expect("rotating the shown photograph is offered");
        app.harness().relayout();
        assert_eq!(
            app.state()
                .library
                .photo(&photo)
                .unwrap()
                .edits
                .rotate_quarters,
            1
        );

        // Every retained control is bound to an effect the window offers.
        let bindings = surface(&mut app, |photos| {
            ui::widget::controls::ActionControls::control_action_bindings(photos)
        });
        assert!(!bindings.is_empty());
        let offered: Vec<_> = PhotosEffect::specs()
            .into_iter()
            .map(|spec| spec.id)
            .collect();
        assert!(offered.iter().any(|id| id == "rotate-shown-right"));
    }

    /// The saved scenario `tilo-app test` drives runs against the framework
    /// app: its targets name the window's controls as the live window does.
    #[test]
    fn the_saved_library_entry_scenario_drives_the_framework_window() {
        let spec = ui::headless::parse_spec(&format!(
            "{{\"actions\": {}}}",
            include_str!("../tests/scenarios/library-entry.json")
        ))
        .expect("the scenario parses");
        let mut app = TestApp::<PhotosShell>::new().unwrap();
        app.harness().relayout();
        for action in &spec.actions {
            app.harness()
                .perform(action)
                .unwrap_or_else(|error| panic!("scenario step failed: {error}"));
        }
        app.harness().relayout();
        assert!(surface(&mut app, |photos| photos.shows_library()));
    }
}
