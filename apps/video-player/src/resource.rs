//! Video Player as a resource: the shared library ([`Library`]: playlists,
//! timestamp annotations, playback watched together) is the document; the
//! player on this device is the surface over it, with its effects and the
//! live rows of what it shows.

use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, LiveRow, ResourceApp, ResourceSurface, SurfaceCx, ViewCx,
};
use ui::tilo_resource::{ActionDecl, Invoke};
use ui::widget::Element;

use crate::actions::{self, VideoEffect};
use crate::app::PlayerApp;
use crate::shared::{Library, library};

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

/// Framework app: the player widget is the surface over the shared [`Library`].
pub struct VideoPlayerShell;

impl ResourceApp for VideoPlayerShell {
    type Resource = Library;
    const ID: &'static str = "so.tilo.video-player";
    const TITLE: &'static str = "Video Player";
    const ICON: Option<&'static str> = Some("video-player");
    /// The video to play: a path or a stream's URL.
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Library>) -> Element {
        cx.surface(PlayerApp::new(cx.mode()))
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    /// The shared library scenario's playlist and the review notes on the
    /// demo clip.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Library>> {
        if name != Some("shared") {
            return Vec::new();
        }
        let media_ref = crate::app::demo_media_ref();
        vec![
            Library::CREATE_PLAYLIST.with("Watch later"),
            Library::ADD_ANNOTATION.with(library::AddAnnotation {
                media_ref: media_ref.clone(),
                time_ms: 8_000,
                text: Some("Opening detail".into()),
            }),
            Library::ADD_ANNOTATION.with(library::AddAnnotation {
                media_ref,
                time_ms: 45_000,
                text: None,
            }),
        ]
    }

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::i18n::CATALOG)
    }

    fn label(_state: &Library) -> String {
        "Video Player".to_owned()
    }
}

impl ResourceSurface<Library, VideoEffect> for PlayerApp {
    fn present(&mut self, state: &Rc<Library>) {
        self.present_library(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Library>) {
        self.attach_framework(&cx);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        let launched = self.capture_launched.take();
        self.stage_capture(launched.as_deref(), scenario);
    }

    fn effect(&mut self, effect: VideoEffect, cx: &mut EffectCx<Library, Self>) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }

    fn effects(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }

    /// Edits bound to the playhead: bookmark the moment shown, add the open
    /// video to the shared Watch later playlist.
    fn bound_declarations(&self) -> Vec<ActionDecl<Library>> {
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
                        .values()
                        .all(|note| note.media_ref == crate::app::demo_media_ref()),
                    "the notes belong to the clip the capture plays"
                );
            } else {
                assert_eq!(app.state(), Library::default());
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
            .effect(&VideoEffect::OpenVideo {
                path: Some(missing.display().to_string()),
            })
            .expect_err("a missing file does not open");
        assert!(format!("{error:?}").contains("does not exist"));
    }

    #[test]
    fn opening_a_video_answers_once_its_first_picture_decodes() {
        let sample = std::path::Path::new(env!("OUT_DIR")).join("h264-aac.mp4");
        let mut app = TestApp::<VideoPlayerShell>::new().unwrap();
        app.effect(&VideoEffect::OpenVideo {
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
        app.invoke(Library::CREATE_PLAYLIST.with("Trip")).unwrap();
        assert_eq!(app.state().playlists.values().next().unwrap().name, "Trip");
        assert!(app.undo(), "creating a playlist is one undo step");
        assert!(app.state().playlists.is_empty());

        let surface_rows = app.agent_surface().to_string();
        for action in [
            "create-playlist",
            "add-annotation",
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
        app.effect(&VideoEffect::OpenVideo {
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
        let mut peer = ui::tilo_resource::Replica::<Library>::from_doc(app.peer_document());
        peer.apply(Library::SET_TRANSPORT.with(library::SetTransport {
            media_ref,
            playing: false,
            position_ms: 2_000,
            rate: 1.0,
            epoch_ms,
        }))
        .unwrap();
        app.merge_peer(peer.doc_mut()).unwrap();
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

    /// An agent bookmarks the moment the player shows, adds the open video
    /// to Watch later and edits the shared library, one call each.
    #[test]
    fn an_agent_works_the_shared_library_one_call_each() {
        let sample = std::path::Path::new(env!("OUT_DIR")).join("h264-aac.mp4");
        let mut app = TestApp::<VideoPlayerShell>::new().unwrap();
        app.effect(&VideoEffect::OpenVideo {
            path: Some(sample.display().to_string()),
        })
        .expect("the video opens");
        app.call("bookmark-current-time", serde_json::json!({}))
            .unwrap();
        app.call("add-this-video-to-watch-later", serde_json::json!({}))
            .unwrap();
        let state = app.state();
        let (note, bookmark) = state.annotations.iter().next().unwrap();
        assert!(bookmark.text.is_empty(), "a bookmark says nothing");
        let watch_later = ui::tilo_resource::Id::from_raw(crate::shared::WATCH_LATER);
        assert!(
            state.playlists.contains(&watch_later),
            "the Watch later playlist was made"
        );
        assert_eq!(state.items_of(&watch_later).count(), 1);
        app.call(
            "field-edit",
            serde_json::json!({
                "edit": Library::paths().annotations.item(note).text.replace("Check the colour")
            }),
        )
        .unwrap();
        assert_eq!(
            app.state().annotations.get(note).unwrap().text,
            "Check the colour"
        );
        let item = ui::tilo_resource::items::item_resource_id::<Library>("annotations", note);
        app.call_confirmed_on(&item, "delete-annotation", serde_json::json!({}))
            .unwrap();
        assert!(app.state().annotations.is_empty());
        assert!(app.undo(), "deleting is one step");
        assert_eq!(app.state().annotations.len(), 1);
    }

    /// What an agent does in the shared library reads as the player's
    /// sentences, in English and German.
    #[test]
    fn history_entries_render_as_the_apps_sentences() {
        use ui::tilo_actions::{ActionInvoker, ActorKind};
        use ui::tilo_resource::items::item_resource_id;

        let mut app = TestApp::<VideoPlayerShell>::new().unwrap();
        let agent = ActionInvoker::actor("agent:claude")
            .with_kind(ActorKind::Agent)
            .acting_for("ada");
        let kind = <Library as ui::tilo_resource::Resource>::KIND;
        let call = |app: &mut TestApp<VideoPlayerShell>, resource: &str, action: &str, args| {
            app.call_by(agent.clone(), resource, action, args).unwrap();
        };
        call(
            &mut app,
            kind,
            "create-playlist",
            serde_json::json!({"name": "Trip"}),
        );
        let trip = app.state().playlists.ids().next().unwrap().clone();
        let trip_item = item_resource_id::<Library>("playlists", &trip);
        call(
            &mut app,
            &trip_item,
            "add-to-playlist",
            serde_json::json!({"media_ref": "clip-1", "title": "Harbour"}),
        );
        call(
            &mut app,
            kind,
            "add-to-watch-later",
            serde_json::json!({"media_ref": "clip-2", "title": "Lighthouse"}),
        );
        call(
            &mut app,
            kind,
            "add-annotation",
            serde_json::json!({"media_ref": "clip-1", "time_ms": 8000, "text": "Opening"}),
        );
        let note = app.state().annotations.ids().next().unwrap().clone();
        let paths = Library::paths();
        for edit in [
            paths.playlists.item(&trip).name.replace("Holiday"),
            paths.annotations.item(&note).text.replace("Opening shot"),
            paths.annotations.item(&note).time_ms.set(9000),
        ] {
            call(
                &mut app,
                kind,
                "field-edit",
                serde_json::json!({ "edit": edit }),
            );
        }
        app.call_confirmed_on(
            &item_resource_id::<Library>("annotations", &note),
            "delete-annotation",
            serde_json::json!({}),
        )
        .unwrap();

        let sentences = |locale: &str| -> Vec<String> {
            let source = crate::i18n::LOCALES
                .iter()
                .find(|(candidate, _)| *candidate == locale)
                .map(|(_, source)| *source)
                .unwrap();
            let catalogue = tilo_history::Catalogue::new(locale, &[source]);
            let reader = tilo_history::Reader::new(locale).with_app(&catalogue);
            app.history()
                .into_iter()
                .filter(|entry| {
                    entry
                        .invoker
                        .as_ref()
                        .is_some_and(|invoker| invoker.actor == "agent:claude")
                })
                .map(|entry| reader.sentence(&entry))
                .collect()
        };
        assert_eq!(
            sentences("en-US"),
            [
                "Created the playlist Trip",
                "Added Harbour to Trip",
                "Added Lighthouse to Watch later",
                "Added a note to a video",
                "Renamed the playlist Trip",
                "Edited the note Opening",
                "Moved the note Opening shot",
            ]
        );
        assert_eq!(sentences("de-DE")[0], "Wiedergabeliste Trip angelegt");
    }
}
