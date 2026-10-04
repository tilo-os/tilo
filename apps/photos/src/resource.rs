//! Photos as a [`tilo_resource::Resource`]: the shared library is the state,
//! edits and membership are document actions, and the gallery widget is the
//! surface. The framework host publishes through [`tilo_resource::publish`].

use std::path::PathBuf;
use std::rc::Rc;

use tilo_content::ContentStore;
use ui::framework::{Fixture, LaunchInput, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::ActionSpec;
use ui::tilo_resource::{Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{PHOTOS_ACTION_TYPE, PhotosAction, register};
use crate::app::PhotosApp;
use crate::doc::PhotosDoc;
use crate::model::{LibraryPhoto, OpenImage, PhotoLibrary};

/// The shared photograph library: every photograph, how it is filed and
/// the edits it is shown with.
#[derive(Clone, Debug, Default)]
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

    fn bound_actions(_state: &PhotosState) -> Vec<ActionSpec> {
        crate::actions::library_document_actions()
    }

    fn children(state: &PhotosState) -> Vec<tilo_actions::Resource> {
        crate::actions::library_children(&state.library)
    }

    fn local_operations() -> Vec<ActionSpec> {
        crate::actions::library_local_operations()
    }

    fn search(state: &PhotosState) -> Vec<SearchEntry> {
        state
            .library
            .photos
            .iter()
            .map(|photo| SearchEntry {
                anchor: Some(photo.id.clone()),
                title: photo.image.name.clone(),
                text: photo
                    .people
                    .iter()
                    .chain(photo.tags.iter())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" "),
                action: None,
                ..Default::default()
            })
            .collect()
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
        let content = cx.content().unwrap_or_else(ContentStore::for_app);
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

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
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

impl ResourceSurface<Photos> for PhotosApp {
    fn present(&mut self, state: &Rc<PhotosState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Photos>) {
        self.attach_framework(cx.dispatcher());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario.unwrap_or("opened"));
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
}
