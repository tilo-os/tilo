//! Video Player as a [`tilo_resource::Resource`]: shared playlists and
//! annotations in the document; transport and open are View/Effect operations.
//! Live decoder audio/subtitle/chapter rows stay on the surface — hosted
//! [`PublishesResources`](ui::resources::PublishesResources) returns only that
//! slice beside the framework door.

use std::rc::Rc;

use automerge::AutoCommit;
use std::sync::Mutex;

use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::launch::{LaunchArgs, LaunchError};
use ui::tilo_actions::{ActionSpec, EncodedAction, OperationKind};
use ui::tilo_resource::{Doc, Invoke, ModelError, Resource, SearchEntry};
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
        actions::application_tree(&ctx, &doc)
            .into_iter()
            .next()
            .map(|root| root.children)
            .unwrap_or_default()
    }

    fn local_operations() -> Vec<ActionSpec> {
        let ctx = actions::empty_resource_ctx();
        let doc = automerge::AutoCommit::new();
        actions::application_tree(&ctx, &doc)
            .into_iter()
            .flat_map(|resource| resource.actions)
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
            text: format!(
                "Bookmark at {}",
                ui::format::timecode(annotation.time_ms / 1000)
            ),
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
        cx.surface(launch_player())
    }

    fn scenarios() -> &'static [&'static str] {
        crate::app::CAPTURE_SCENARIOS
    }

    /// The shared tab shows playlists and bookmarks, which live in the
    /// document: a capture writes them through the same typed action a
    /// script would use.
    fn scenario(name: Option<&str>) -> Vec<Invoke<VideoPlayer>> {
        if name != Some("shared") {
            return Vec::new();
        }
        let media_ref = crate::app::demo_media_ref();
        let actions = [
            SharedAction::CreatePlaylist {
                id: "watch-later".into(),
                name: "Watch later".into(),
            },
            SharedAction::CreatePlaylist {
                id: "weekend-films".into(),
                name: "Weekend films".into(),
            },
            SharedAction::CreateAnnotation {
                id: "opening".into(),
                media_ref: media_ref.clone(),
                time_ms: 8_000,
                text: "Opening detail".into(),
            },
            SharedAction::CreateAnnotation {
                id: "bookmark".into(),
                media_ref,
                time_ms: 45_000,
                text: String::new(),
            },
        ];
        actions
            .iter()
            .filter_map(|action| Invoke::apply(action).ok())
            .collect()
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

    fn stage(&mut self, scenario: Option<&str>) {
        self.detach_collab();
        self.stage_capture(scenario);
    }
}

/// What a live launch asked to play (`--open PATH`, a path or a URL), kept
/// for the first window the framework builds.
static LAUNCH_SOURCE: Mutex<Option<media::Source>> = Mutex::new(None);

/// The source named on the command line of a live launch (`--open PATH`, a
/// path or a URL). Captures name theirs through the scenario instead.
fn requested_source_from_env() -> Option<media::Source> {
    let mut args = std::env::args().skip(1);
    let mut positional: Option<String> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--shots" | "--responsive-matrix" | "--native-audit" | "--headless" => return None,
            "--open" => {
                return args
                    .next()
                    .map(|path| crate::source_for_path(std::path::Path::new(&path)));
            }
            "--size" | "--viewport" | "--scenario" | "--data-dir" | "--theme" => {
                let _ = args.next();
            }
            flag => {
                if let Some(path) = flag.strip_prefix("--open=") {
                    return Some(crate::source_for_path(std::path::Path::new(path)));
                }
                if !flag.starts_with('-') && positional.is_none() {
                    positional = Some(flag.to_owned());
                }
            }
        }
    }
    positional.map(|value| {
        if value.starts_with("http://") || value.starts_with("https://") {
            media::Source::Remote { url: value }
        } else {
            crate::source_for_path(std::path::Path::new(&value))
        }
    })
}

/// The player a live window shows: the one the launch asked for.
fn launch_player() -> PlayerApp {
    let requested = LAUNCH_SOURCE
        .lock()
        .ok()
        .and_then(|mut source| source.take());
    crate::live_player(requested, std::env::var("TILO_MEDIA_HANDOFF_TITLE").ok())
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

/// The launch on the framework host, live and captured. A capture plays a
/// real file when `--open PATH` names one, and fails when that file does not
/// decode, so a failed capture is never mistaken for a screenshot of the
/// player.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<VideoPlayerShell>()
        .capture_scenario(capture)
        .allowed_options(&["--data-dir", "--open"])
        .allowed_live_options(&["--data-dir", "--open"])
        .max_positionals(1)
}

/// Remember what the command line asked to play, for the window about to
/// open. Call once, before launching.
pub fn remember_launch_request() {
    if let (Some(source), Ok(mut slot)) = (requested_source_from_env(), LAUNCH_SOURCE.lock()) {
        *slot = Some(source);
    }
}

/// How long a capture waits for a real decoder's first picture.
const CAPTURE_FRAME_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

fn capture(harness: &mut ui::headless::FrameHarness, args: &LaunchArgs) -> Result<(), LaunchError> {
    let open = args
        .value("--open")
        .map(|path| format!("{}{path}", crate::app::OPEN_CAPTURE));
    let scenario = open.as_deref().or(args.scenario.as_deref());
    ui::framework::stage::<VideoPlayerShell>(harness, scenario)?;
    if open.is_none() {
        return Ok(());
    }
    // Real decode prerolls asynchronously: wait for its first picture (or
    // its error), then insist the file actually played unless the scenario
    // asks for a failure.
    let deadline = ui::time::now() + CAPTURE_FRAME_WAIT;
    loop {
        let settled = harness
            .with_widget_mut(|app: &mut PlayerApp| app.capture_settled())
            .unwrap_or(true);
        if settled || ui::time::now() >= deadline {
            break;
        }
        harness.wait_until(std::time::Duration::from_millis(40), |_| false);
    }
    let expects_picture = !matches!(
        args.scenario.as_deref(),
        Some("empty" | "error" | "native-error" | "loading")
    );
    if expects_picture {
        let (real, picture) = harness
            .with_widget_mut(|app: &mut PlayerApp| (app.decodes_for_real(), app.has_media_frame()))
            .unwrap_or_default();
        if !real {
            return Err(LaunchError::Operation(
                "A real video capture requires the GStreamer decoder and runtime".into(),
            ));
        }
        if !picture {
            return Err(LaunchError::Operation(
                "The video decoder did not produce a frame for the requested file".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::resources::PublishesResources;
    use ui::tilo_actions::OperationKind;
    use ui::widget::Widget;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario / publish-ownership assertion kept top-to-bottom"
    )]
    fn hosted_resources_keep_track_extras_without_library_root() {
        let published = ui::tilo_resource::publish::<VideoPlayer>(
            "video-player.application",
            "Video Player",
            &SharedLibrary::default(),
        );
        assert_eq!(published.kind, "video-player.application");
        let ids: Vec<&str> = published.actions.iter().map(|a| a.id.as_str()).collect();
        for expected in ["open", "show-library", "hide-library", "apply", "group"] {
            assert!(
                ids.contains(&expected),
                "palette `{expected}` must ride publish: {ids:?}"
            );
        }
        let open = published
            .actions
            .iter()
            .find(|action| action.id == "open")
            .expect("open");
        assert_eq!(open.kind, OperationKind::Effect);
        assert!(
            published
                .children
                .iter()
                .any(|child| child.id.0 == "video-player.shared-library"
                    || child.kind == "media.shared-library"),
            "shared library document door must ride Resource children: {:?}",
            published
                .children
                .iter()
                .map(|child| child.id.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            published.children.iter().any(|child| {
                child.kind == "media.video"
                    && child.children.iter().any(|nested| {
                        nested.kind == "media.transport"
                            && nested
                                .actions
                                .iter()
                                .any(|action| action.id == "play-pause")
                    })
            }),
            "play/pause must ride media children via publish, not a second hosted root"
        );

        let mut app = PlayerApp::history_test_root();
        let mut state = ui::State::new();
        Widget::sync(&mut app, &mut state.doc, &state.root);
        assert!(
            !app.is_framework_hosted(),
            "standalone stays on the full action tree"
        );

        let standalone = app.resources(&state.doc);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "video-player.application");
        let media = standalone[0]
            .children
            .iter()
            .find(|child| child.kind == "media.video")
            .expect("standalone nests media under the application");
        let standalone_tracks = media
            .children
            .iter()
            .find(|child| child.kind == "media.tracks")
            .expect("standalone nests decoder tracks under media");
        assert!(
            !standalone_tracks.children.is_empty(),
            "history fixture must yield audio track rows for the hosted extras contract"
        );
        assert!(
            standalone_tracks
                .children
                .iter()
                .all(|track| track.kind == "media.audio-track"
                    && track
                        .actions
                        .iter()
                        .any(|action| action.id.starts_with("select-audio-"))),
            "each audio track is a selectable media.audio-track row"
        );
        assert!(
            standalone[0]
                .children
                .iter()
                .any(|child| child.id.0 == "video-player.shared-library"),
            "standalone still publishes the shared library under the application root"
        );

        app.set_framework_hosted(true);
        assert!(app.is_framework_hosted());
        assert!(ui::resources::resource(&app, &state.doc, "video-player.application").is_none());
        let extras = app.resources(&state.doc);
        assert!(
            extras.iter().all(|resource| {
                resource.kind != "video-player.application"
                    && resource.id.0 != "video-player.shared-library"
                    && resource.kind != "media.shared-library"
                    && resource.kind != "media.queue"
                    && resource.kind != "media.history"
            }),
            "hosted must not republish the application or library beside ResourceRoot: {:?}",
            extras
                .iter()
                .map(|resource| format!("{}:{}", resource.kind, resource.id.0))
                .collect::<Vec<_>>()
        );
        assert!(
            !extras.is_empty(),
            "hosted must keep live track extras when the decoder reported streams"
        );
        assert!(
            extras.iter().all(
                |resource| resource.kind == "media.tracks" || resource.kind == "media.chapters"
            ),
            "hosted extras are track/chapter slices only: {:?}",
            extras
                .iter()
                .map(|resource| resource.kind.as_str())
                .collect::<Vec<_>>()
        );
        let hosted_tracks = extras
            .iter()
            .find(|resource| resource.kind == "media.tracks")
            .expect("hosted returns the tracks slice");
        assert_eq!(
            hosted_tracks.id, standalone_tracks.id,
            "hosted tracks tree matches the standalone nested slice"
        );
        assert_eq!(
            hosted_tracks.children.len(),
            standalone_tracks.children.len(),
            "hosted keeps the same bounded track rows"
        );
        let track_id = hosted_tracks.children[0].id.as_str();
        assert!(
            ui::resources::resource(&app, &state.doc, track_id).is_some(),
            "a hosted track stays addressable"
        );

        app.clear_shot_metadata_for_test();
        assert!(
            app.resources(&state.doc).is_empty(),
            "hosted publishes no track extras when the decoder reported none"
        );
    }

    #[test]
    fn present_empty_does_not_wipe_unpublished_local_library() {
        let mut app = PlayerApp::new();
        app.seed_unpublished_playlist_for_test("local-only", "Not published yet");
        let empty = Rc::new(SharedLibrary::default());
        ResourceSurface::<VideoPlayer>::present(&mut app, &empty);
        assert_eq!(
            app.shared_playlist_ids_for_test(),
            vec!["local-only".to_owned()],
            "empty present must not wipe an unpublished local library"
        );
    }
}
