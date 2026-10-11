//! Video Player as a [`tilo_resource::Resource`]: shared playlists and
//! timestamp annotations are the document; the player on this device is the
//! surface over it, with its effects and the live rows of what it shows.

use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, LiveRow, ResourceApp, ResourceSurface, SurfaceCx, ViewCx,
};
use ui::tilo_resource::{ActionDecl, Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{self, VideoEffect, annotation_resource_id, playlist_resource_id};
use crate::app::PlayerApp;
use crate::shared::{self, SharedAction, SharedLibrary};

/// Collaborative library resource (playlists and timestamp annotations).
pub struct VideoPlayer;

impl Resource for VideoPlayer {
    const KIND: &'static str = "video-player.application";
    const LABEL: &'static str = "Video Player";
    const ACTION_TYPE: &'static str = shared::ACTION_TYPE;

    type State = SharedLibrary;
    type Action = SharedAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        shared::register(registry);
    }

    fn read(doc: &Doc) -> Result<SharedLibrary, ModelError> {
        Ok(shared::read(doc))
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        actions::declarations()
    }

    fn children(state: &SharedLibrary) -> Vec<tilo_actions::Resource> {
        actions::document_children(state)
    }

    fn child(state: &SharedLibrary, id: &str) -> Option<tilo_actions::Resource> {
        actions::document_child(state, id)
    }

    fn search(state: &SharedLibrary) -> Vec<SearchEntry> {
        let playlists = state.playlists.iter().map(|playlist| {
            SearchEntry::new(playlist.name.clone(), "Playlist")
                .anchored(playlist_resource_id(&playlist.id))
        });
        let annotations = state.annotations.iter().map(|annotation| {
            SearchEntry::new(
                annotation.text.clone(),
                format!("{} ms", annotation.time_ms),
            )
            .anchored(annotation_resource_id(&annotation.id))
        });
        playlists.chain(annotations).collect()
    }
}

/// The screenshot scenarios: the first run before a video is chosen, a
/// video loading, failing to open (fixture copy, or the decoder's own error
/// for a bad file), playing and paused, the settings and track menus, the
/// device's queue and recent videos (and the queue with keyboard focus), the
/// shared library, and fullscreen.
pub(crate) const SCENARIOS: &[&str] = &[
    "empty",
    "loading",
    "error",
    "missing-codec",
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
];

/// Framework app: the player widget is the surface over [`VideoPlayer`].
pub struct VideoPlayerShell;

impl ResourceApp for VideoPlayerShell {
    type Resource = VideoPlayer;
    const ID: &'static str = "so.tilo.video-player";
    const TITLE: &'static str = "Video Player";
    const ICON: Option<&'static str> = Some("video-player");
    /// The video to play: a path or a stream's URL.
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<VideoPlayer>) -> Element {
        cx.surface(PlayerApp::new(cx.mode()))
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    /// The shared library scenario's playlist and the review notes on the
    /// demo clip.
    fn scenario(name: Option<&str>) -> Vec<Invoke<VideoPlayer>> {
        if name != Some("shared") {
            return Vec::new();
        }
        let media_ref = crate::app::demo_media_ref();
        [
            SharedAction::CreatePlaylist {
                id: "watch-later".into(),
                name: "Watch later".into(),
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
        ]
        .iter()
        .filter_map(|action| Invoke::apply(action).ok())
        .collect()
    }

    fn label(_state: &SharedLibrary) -> String {
        "Video Player".to_owned()
    }
}

impl ResourceSurface<VideoPlayer, VideoEffect> for PlayerApp {
    fn present(&mut self, state: &Rc<SharedLibrary>) {
        self.present_library(state);
    }

    fn attach(&mut self, cx: SurfaceCx<VideoPlayer>) {
        self.attach_framework(&cx);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        let launched = self.capture_launched.take();
        self.stage_capture(launched.as_deref(), scenario);
    }

    fn effect(
        &mut self,
        effect: VideoEffect,
        cx: &mut EffectCx<VideoPlayer, Self>,
    ) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }

    fn effects(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }

    /// Edits bound to the playhead: bookmark the moment shown, add the open
    /// video to the shared Watch later playlist.
    fn bound_declarations(&self) -> Vec<ActionDecl<VideoPlayer>> {
        self.playhead_declarations()
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        actions::live_rows(&self.player_rows(), actions::CONTEXTUAL_CHILD_LIMIT)
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        actions::live_row(&self.player_rows(), id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::framework::testing::TestApp;

    fn surface<T>(
        app: &mut TestApp<VideoPlayerShell>,
        read: impl FnOnce(&mut PlayerApp) -> T,
    ) -> T {
        app.with_surface::<PlayerApp, T>(read)
            .expect("the window shows the player")
    }

    fn activate(app: &mut TestApp<VideoPlayerShell>, target: &str) {
        app.harness()
            .perform_named(target, &ui::a11y::Action::Activate)
            .unwrap_or_else(|error| panic!("{target}: {error}"));
        app.settle();
    }

    #[test]
    fn every_scenario_captures_and_the_shared_one_stages_the_library() {
        for scenario in SCENARIOS {
            let app = TestApp::<VideoPlayerShell>::capture(Some(scenario))
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            if *scenario == "shared" {
                let state = app.state();
                assert_eq!(state.playlists.len(), 1, "the playlist is an action");
                assert_eq!(state.annotations.len(), 2, "the notes are actions");
                assert!(
                    state
                        .annotations
                        .iter()
                        .all(|note| note.media_ref == crate::app::demo_media_ref()),
                    "the notes belong to the clip the capture plays"
                );
            } else {
                assert_eq!(app.state(), SharedLibrary::default());
            }
        }
    }

    #[test]
    fn library_controls_reach_the_player_through_the_door() {
        let mut app = TestApp::<VideoPlayerShell>::new().unwrap();
        activate(&mut app, "video-player.library");
        assert!(surface(&mut app, |player| player.library_visible()));
        activate(&mut app, "video-player.library.tab.recent");
        activate(&mut app, "video-player.library.close");
        assert!(!surface(&mut app, |player| player.library_visible()));
        activate(&mut app, "video-player.library");
        assert_eq!(
            surface(&mut app, |player| player.library_tab_name()),
            "recent",
            "the library reopens on the tab chosen last"
        );
        app.effect(&VideoEffect::ShowLibrary {
            tab: "shared".into(),
        })
        .expect("agents show the library the same way");
        assert_eq!(
            surface(&mut app, |player| player.library_tab_name()),
            "shared"
        );
    }

    #[test]
    fn playback_effects_need_an_open_video() {
        let mut app = TestApp::<VideoPlayerShell>::new().unwrap();
        assert!(
            app.effect(&VideoEffect::TogglePlayback).is_err(),
            "nothing to play"
        );
        let missing = std::env::temp_dir().join("tilo-video-player-missing.mp4");
        let error = app
            .effect(&VideoEffect::Open {
                path: Some(missing.display().to_string()),
            })
            .expect_err("a missing file does not open");
        assert!(format!("{error:?}").contains("does not exist"));
    }

    #[test]
    fn opening_a_video_answers_once_its_first_picture_decodes() {
        let sample = std::path::Path::new(env!("OUT_DIR")).join("h264-aac.mp4");
        let mut app = TestApp::<VideoPlayerShell>::new().unwrap();
        app.effect(&VideoEffect::Open {
            path: Some(sample.display().to_string()),
        })
        .expect("the video opens");
        assert!(surface(&mut app, |player| player
            .player()
            .latest_frame()
            .is_some()));
        let surface_rows = app.agent_surface().to_string();
        assert!(
            surface_rows.contains("video-player.queue.item."),
            "the opened video is a queue row: {surface_rows}"
        );
        assert!(
            surface_rows.contains("bookmark-current-time"),
            "the playhead's edits are offered: {surface_rows}"
        );
    }

    #[test]
    fn the_shared_library_changes_through_the_door_as_one_step_each() {
        let mut app = TestApp::<VideoPlayerShell>::new().unwrap();
        app.invoke(
            Invoke::apply(&SharedAction::CreatePlaylist {
                id: "trip".into(),
                name: "Trip".into(),
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(app.state().playlists[0].name, "Trip");
        assert!(app.undo(), "creating a playlist is one undo step");
        assert!(app.state().playlists.is_empty());

        let surface_rows = app.agent_surface().to_string();
        for action in [
            "create-playlist",
            "create-annotation",
            "open",
            "toggle-playback",
        ] {
            assert!(
                surface_rows.contains(action),
                "`{action}` is published: {surface_rows}"
            );
        }
    }

    #[test]
    fn everyone_with_the_video_open_watches_it_together() {
        let sample = std::path::Path::new(env!("OUT_DIR")).join("h264-aac.mp4");
        let mut app = TestApp::<VideoPlayerShell>::new().unwrap();
        app.effect(&VideoEffect::Open {
            path: Some(sample.display().to_string()),
        })
        .expect("the video opens");
        let media_ref = surface(&mut app, |player| player.media_ref().map(str::to_owned))
            .expect("an open video has a shareable identity");
        let deadline = ui::time::now() + std::time::Duration::from_secs(10);
        while app.state().transport_for(&media_ref).is_none() && ui::time::now() < deadline {
            app.settle();
        }
        let said = app
            .state()
            .transport_for(&media_ref)
            .cloned()
            .expect("playing the video says where playback stands");
        assert!(said.playing);
        assert!(!app.undo(), "playing is nobody's undo step");

        // Someone else pauses at two seconds: this player follows.
        let epoch_ms = tilo_clock::unix_now_ms();
        app.merge_peer_edit(|doc| {
            tilo_actions::Action::apply(
                &SharedAction::SetTransport {
                    media_ref: media_ref.clone(),
                    playing: false,
                    position_ms: 2_000,
                    rate: 1.0,
                    epoch_ms,
                },
                doc,
            )
            .unwrap();
        });
        app.settle();
        app.settle();
        let (playing, position) = surface(&mut app, |player| {
            (player.player().is_playing(), player.player().position_ms())
        });
        assert!(!playing, "the peer's pause is followed");
        assert!(
            position.abs_diff(2_000) <= media::DRIFT_SEEK_THRESHOLD_MS,
            "the peer's position is followed: {position}"
        );
    }
}
