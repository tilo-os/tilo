//! Photos as a [`tilo_resource::Resource`]: the shared library is the state,
//! edits and membership are document actions, and the gallery widget is the
//! surface. The framework host publishes through [`tilo_resource::publish`].

use std::rc::Rc;

use automerge::AutoCommit;
use tilo_content::ContentStore;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::ActionSpec;
use ui::tilo_resource::{Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{PHOTOS_ACTION_TYPE, PhotosAction, register};
use crate::app::PhotosApp;
use crate::doc::PhotosDoc;
use crate::model::{Edits, OpenImage, PhotoLibrary};

/// Shared photograph library plus the open image (if any).
#[derive(Clone, Debug, Default)]
pub struct PhotosState {
    pub library: PhotoLibrary,
    pub open: Option<OpenImage>,
    pub edits: Edits,
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
            open: PhotosDoc::image(doc),
            edits: PhotosDoc::edits(doc),
        })
    }

    fn bound_actions(_state: &PhotosState) -> Vec<ActionSpec> {
        crate::actions::library_document_actions()
    }

    fn children(state: &PhotosState) -> Vec<tilo_actions::Resource> {
        let mut children = Vec::new();
        if let Some(image) = &state.open {
            children.push(crate::actions::open_image_resource(image, &state.edits));
        }
        children.extend(crate::actions::library_children(&state.library));
        children
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
    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    fn view(cx: &mut ViewCx<Photos>) -> Element {
        cx.surface(PhotosApp::new_persistent(ContentStore::for_app()))
    }

    fn scenarios() -> &'static [&'static str] {
        crate::SHOT_SCENARIOS
    }

    fn label(state: &PhotosState) -> String {
        match &state.open {
            Some(image) if !image.name.is_empty() => image.name.clone(),
            _ => "Photos".into(),
        }
    }
}

impl ResourceSurface<Photos> for PhotosApp {
    fn present(&mut self, state: &Rc<PhotosState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Photos>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Photos>>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Photos>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &PhotosAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        if let Err(error) = self.try_dispatch_with_context(doc, type_id, action, context) {
            tracing::warn!(error = %error, type_id, "photos action dispatch failed");
        }
    }

    pub(crate) fn try_dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &PhotosAction,
        context: ui::tilo_actions::ActionContext,
    ) -> Result<(), tilo_actions::ActionError> {
        match &self.framework {
            Some(dispatcher) => dispatcher.apply_now_with(doc, action, context),
            None => self
                .local
                .try_dispatch_with_context(doc, type_id, action, context),
        }
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }
}

/// Default live launch: the framework-hosted resource app.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<PhotosShell>()
}

/// Capture, import and explicit file opens stay on the standalone widget path.
pub fn standalone_launch() -> bool {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                let _ = args.next();
            }
            "--shots"
            | "--responsive-matrix"
            | "--native-audit"
            | "--headless"
            | "--library"
            | "--analyze-imports" => return true,
            "--open" | "--import-directory" | "--size" => {
                let _ = args.next();
                return true;
            }
            flag if flag.starts_with("--shots=")
                || flag.starts_with("--open=")
                || flag.starts_with("--import-directory=") =>
            {
                return true;
            }
            file if !file.starts_with('-') => return true,
            _ => {}
        }
    }
    false
}
