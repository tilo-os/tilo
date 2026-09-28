//! Video Player as a [`tilo_resource::Resource`]: shared playlists and
//! annotations in the document; transport and open are View/Effect operations.

use std::rc::Rc;

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, EncodedAction, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{self, annotation_resource_id, playlist_resource_id};
use crate::app::PlayerApp;
use crate::shared::{self, SharedAction, SharedLibrary};

/// Collaborative library resource (playlists and timestamp annotations).
pub struct VideoPlayer;

impl Resource for VideoPlayer {
    /// Matches the application root already published by the action tree.
    const KIND: &'static str = "video-player.application";
    const LABEL: &'static str = "Video Player";
    const ACTION_TYPE: &'static str = shared::ACTION_TYPE;

    type State = SharedLibrary;
    type Action = SharedAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        actions::register(registry);
    }

    fn read(doc: &Doc) -> Result<SharedLibrary, ModelError> {
        Ok(shared::read(doc))
    }

    fn children(state: &SharedLibrary) -> Vec<tilo_actions::Resource> {
        let ctx = actions::empty_resource_ctx();
        let doc = actions::doc_from_library(state);
        actions::resources(&ctx, &doc)
            .into_iter()
            .next()
            .map(|root| root.children)
            .unwrap_or_default()
    }

    fn local_operations() -> Vec<ActionSpec> {
        let ctx = actions::empty_resource_ctx();
        let doc = automerge::AutoCommit::new();
        actions::resources(&ctx, &doc)
            .into_iter()
            .flat_map(|resource| {
                let nested = resource
                    .children
                    .into_iter()
                    .flat_map(|child| child.actions);
                resource.actions.into_iter().chain(nested)
            })
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }

    fn search(state: &SharedLibrary) -> Vec<SearchEntry> {
        let playlists = state.playlists.iter().map(|playlist| SearchEntry {
            anchor: Some(playlist_resource_id(&playlist.id)),
            title: playlist.name.clone(),
            text: "Playlist".to_owned(),
        });
        let annotations = state.annotations.iter().map(|annotation| SearchEntry {
            anchor: Some(annotation_resource_id(&annotation.id)),
            title: annotation.text.clone(),
            text: format!("{} ms", annotation.time_ms),
        });
        playlists.chain(annotations).collect()
    }
}

/// Framework app: the player widget is the surface over [`VideoPlayer`].
pub struct VideoPlayerShell;

impl ResourceApp for VideoPlayerShell {
    type Resource = VideoPlayer;
    const ID: &'static str = "so.tilo.video-player";
    const TITLE: &'static str = "Video Player";
    const ICON: Option<&'static str> = Some("video-player");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<VideoPlayer>) -> Element {
        cx.surface(PlayerApp::new().with_collab())
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "empty",
            "loading",
            "error",
            "native-error",
            "playing",
            "paused",
            "settings",
            "metadata",
            "library",
            "library-focus",
            "recent",
            "shared",
            "fullscreen",
        ]
    }

    fn label(_state: &SharedLibrary) -> String {
        "Video Player".to_owned()
    }
}

impl ResourceSurface<VideoPlayer> for PlayerApp {
    fn present(&mut self, state: &Rc<SharedLibrary>) {
        self.adopt_shared_library(state);
    }

    fn attach(&mut self, cx: SurfaceCx<VideoPlayer>) {
        self.set_framework_hosted(true);
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher for shared playlists and annotations. Transport stays local.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<VideoPlayer>>,
    last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<VideoPlayer>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &SharedAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        if let Err(error) = self.try_dispatch_with_context(doc, type_id, action, context) {
            tracing::warn!(error = %error, type_id, "video player action dispatch failed");
        }
    }

    pub(crate) fn try_dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &SharedAction,
        context: ui::tilo_actions::ActionContext,
    ) -> Result<(), tilo_actions::ActionError> {
        match &self.framework {
            Some(dispatcher) => {
                self.last = EncodedAction::encode(shared::ACTION_TYPE, action).ok();
                dispatcher.apply_now_with(doc, action, context)
            }
            None => self
                .local
                .try_dispatch_with_context(doc, type_id, action, context),
        }
    }

    /// Hosted commits are recorded by the dispatcher. Standalone tests still
    /// read the local host's rising edge.
    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }
}

/// Live launch on the framework host. Capture modes keep the standalone path.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<VideoPlayerShell>()
        .allowed_live_options(&["--data-dir", "--open"])
        .max_positionals(1)
}
