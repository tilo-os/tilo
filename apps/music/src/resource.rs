//! Music as a [`tilo_resource::Resource`]: the shared library (playlists,
//! likes, membership) is the state; now-playing presentation is View.

use std::rc::Rc;

use ui::framework::{
    Command, EffectAnswer, EffectCx, LiveRow, ResourceApp, ResourceSurface, RunMode, SurfaceCx,
    ViewCx,
};
use ui::tilo_actions::{ActionParam, ParamType};
use ui::tilo_resource::{ActionDecl, Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    MUSIC_ACTION_TYPE, MusicAction, MusicEffect, MusicResourceCtx, create_playlist_action,
    document_children, new_playlist_id, register,
};
use crate::app::MusicApp;
use crate::doc;
use crate::model::Library;

/// The music library resource.
pub struct Music;

impl Resource for Music {
    const KIND: &'static str = "music.library";
    const LABEL: &'static str = "Music";
    const ACTION_TYPE: &'static str = MUSIC_ACTION_TYPE;

    type State = Library;
    type Action = MusicAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<Library, ModelError> {
        Ok(doc::load_library(doc))
    }

    /// A new library holds no music yet: the containers every replica
    /// shares, so the first edits on two devices merge into one library.
    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        doc::seed(doc, &Library::default());
        Ok(())
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "new-playlist",
                "New playlist",
                vec![ActionParam::required(
                    tilo_actions::PROMPTED_PARAM,
                    ParamType::Text,
                    "playlist name…",
                )],
                |args, _| {
                    let name = args
                        .text(tilo_actions::PROMPTED_PARAM)
                        .unwrap_or_default()
                        .trim()
                        .to_owned();
                    let name = if name.is_empty() {
                        "New playlist".to_owned()
                    } else {
                        name
                    };
                    Ok(create_playlist_action(&new_playlist_id(), &name))
                },
            )
            .describe("Create a new empty playlist."),
        ]
    }

    fn children(state: &Library) -> Vec<tilo_actions::Resource> {
        let mut children = document_children(&MusicResourceCtx::from_library(state));
        children.extend(crate::recommendations::resources(state));
        children
    }

    fn child(state: &Library, id: &str) -> Option<tilo_actions::Resource> {
        if id.starts_with("music.recommendation") {
            return crate::recommendations::resource_by_id(state, id);
        }
        let ctx = MusicResourceCtx::for_document_id(state, id)?;
        crate::actions::resource_by_id(&ctx, id)
    }

    /// Songs, albums, artists, playlists and what each playlist holds.
    fn search(state: &Library) -> Vec<SearchEntry> {
        let mut entries = crate::search_type::library_entries(state);
        entries.push(
            SearchEntry::new(
                crate::i18n::t("music-recommendations-tune"),
                crate::i18n::t("music-recommendations-help"),
            )
            .anchored(crate::recommendations::PREFERENCES)
            .invoking(tilo_actions::ActionAddress::new(
                crate::recommendations::PREFERENCES,
                "open",
            )),
        );
        entries
    }
}

/// The screenshot scenarios: the sample library (`empty` has none) with the
/// view state each stages. A capture never reaches the catalogue, so every
/// scenario comes out the same on every machine.
const SCENARIOS: &[&str] = &[
    "sidebar",
    "collapsed",
    "context",
    "new-playlist",
    "empty",
    "search-focus",
    "skeleton",
    "nowplaying",
    "queue",
    "lyrics",
    "downloading",
    "offline-cached",
    "offline-error",
    "search-tabs",
    "search-songs",
    "search-artists",
    "artist-page",
    "album-page",
    "error",
    "membership-open",
    "spotify-home",
    "spotify-devices",
    "spotify-error",
    "spotify-queue",
    "spotify-create",
    "recommendations",
    "recommendations-home",
    "recommendations-feedback",
    "recommendations-empty",
];

/// Framework app: the music widget is the surface over [`Music`].
pub struct MusicShell;

impl ResourceApp for MusicShell {
    type Resource = Music;
    const ID: &'static str = "so.tilo.music";
    const TITLE: &'static str = "Music";
    const ICON: Option<&'static str> = Some("music");
    /// A local audio file or a `music://` link to play or open.
    const ARGUMENTS: usize = 1;

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn commands() -> &'static [Command] {
        crate::commands::COMMANDS
    }

    /// Every scenario but `empty` shows the sample library.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Music>> {
        if matches!(
            name,
            Some(
                "empty"
                    | "recommendations-empty"
                    | "spotify-home"
                    | "spotify-devices"
                    | "spotify-error"
                    | "spotify-queue"
                    | "spotify-create"
            )
        ) {
            return Vec::new();
        }
        let mut actions = crate::seed::sample_actions();
        if name == Some("recommendations-feedback") {
            actions.extend([
                MusicAction::SetRecommendationDismissed {
                    track: "tk_g2".into(),
                    dismissed: true,
                },
                MusicAction::SetRecommendationArtist {
                    artist: "ar_kavi".into(),
                    preferred: true,
                },
            ]);
        }
        actions
            .iter()
            .filter_map(|action| Invoke::apply(action).ok())
            .collect()
    }

    fn view(cx: &mut ViewCx<Music>) -> Element {
        // Only a window someone uses plays audio, reads this device's
        // playback profile and reaches the network; a capture and a past
        // version show the library alone.
        let app = match cx.mode() {
            RunMode::Live => {
                let mut app = MusicApp::new();
                app.enable_mpris();
                app
            }
            RunMode::Capture | RunMode::Past => MusicApp::offline(),
        };
        cx.surface(app)
    }

    fn label(state: &Library) -> String {
        if state.tracks.is_empty() && state.playlists.is_empty() {
            "Music".into()
        } else {
            format!(
                "{} tracks",
                state.tracks.len().saturating_add(state.playlists.len())
            )
        }
    }
}

impl ResourceSurface<Music, MusicEffect> for MusicApp {
    fn present(&mut self, state: &Rc<Library>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Music>) {
        self.attach_dispatcher(cx.dispatcher());
        if cx.mode().is_live() {
            self.open_launch(&cx.launch());
        }
    }

    fn stage(&mut self, scenario: Option<&str>) {
        if let Some(scenario) = scenario {
            self.stage_scenario(scenario);
        }
    }

    /// The songs the live search found that the library does not hold.
    fn search_query(&mut self, query: &ui::search::SearchQuery) {
        self.queue_catalogue_query(query.text());
    }

    fn search_entries(&self) -> Vec<SearchEntry> {
        self.live_search_entries()
    }

    fn effect(&mut self, effect: MusicEffect, cx: &mut EffectCx<Music, Self>) -> EffectAnswer {
        if let MusicEffect::ToggleLike { track } = &effect {
            if cx.dispatcher().read_only() {
                return Err("Liking a song requires permission to edit the library".into());
            }
            let current = cx.live().get();
            let _gesture = cx.dispatcher().begin_group();
            if !current.tracks.contains_key(track) {
                cx.apply(&self.catalogue_import_action(track)?);
            }
            cx.apply(&crate::actions::set_liked_action(
                track,
                !current.is_liked(track),
            ));
            return Ok(ui::framework::Effected::done());
        }
        let invocation = cx.invocation().unwrap_or_default();
        let answer = self.begin_effect(invocation, effect)?;
        if matches!(answer, ui::framework::Effected::Pending) {
            self.await_effect(invocation, cx.reply());
        }
        Ok(answer)
    }

    /// This device's player, queue and open file, and the tracks it keeps
    /// offline: rows beside the library, carrying their effects.
    fn live_rows(&self) -> Vec<LiveRow> {
        self.device_rows()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.device_row(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::framework::testing::TestApp;

    #[test]
    fn recommendations_preferences_are_addressable_on_an_empty_library() {
        let resource = Music::child(
            &Library::default(),
            "music.recommendation-preferences:library",
        )
        .expect("recommendation inputs must be available without any music or account");
        assert_eq!(resource.kind, "music.recommendation-preferences");
    }

    #[test]
    fn recommendations_feedback_uses_framework_history_and_undo() {
        let mut app = TestApp::<MusicShell>::new().unwrap();
        app.invoke(Invoke::apply(&imported("yt:abc123")).unwrap())
            .unwrap();
        app.invoke(
            Invoke::apply(&MusicAction::SetRecommendationDismissed {
                track: "yt:abc123".into(),
                dismissed: true,
            })
            .unwrap(),
        )
        .unwrap();
        assert!(app.state().recommendations.dismissed.contains("yt:abc123"));
        assert!(app.undo());
        assert!(!app.state().recommendations.dismissed.contains("yt:abc123"));
    }

    #[test]
    fn recommendations_viewers_cannot_change_inputs_or_feedback() {
        let mut app = TestApp::<MusicShell>::new().unwrap();
        app.invoke(Invoke::apply(&imported("yt:abc123")).unwrap())
            .unwrap();
        let before = app.state().recommendations;
        app.set_role(ui::permissions::Role::Viewer);
        for action in [
            MusicAction::SetRecommendationInput {
                input: crate::recommendations::Input::Likes,
                enabled: false,
            },
            MusicAction::SetRecommendationDismissed {
                track: "yt:abc123".into(),
                dismissed: true,
            },
            MusicAction::SetRecommendationArtist {
                artist: "ytc_aurora".into(),
                preferred: true,
            },
        ] {
            assert!(app.invoke(Invoke::apply(&action).unwrap()).is_err());
        }
        assert_eq!(app.state().recommendations, before);
        let mut visible = TestApp::<MusicShell>::capture(Some("recommendations")).unwrap();
        visible.set_role(ui::permissions::Role::Viewer);
        visible.settle();
        let name = crate::recommendations::Input::Likes.label();
        assert!(visible.harness().a11y_nodes().iter().any(|(_, node)| {
            node.label() == Some(name.as_str())
                && node.role() == ui::accesskit::Role::Switch
                && node.is_disabled()
        }));
    }

    #[test]
    fn recommendations_input_is_named_and_operable_by_assistive_technology() {
        let mut app = TestApp::<MusicShell>::capture(Some("recommendations")).unwrap();
        let name = crate::recommendations::Input::Likes.label();
        let target = app
            .harness()
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| {
                node.label() == Some(name.as_str()) && node.role() == ui::accesskit::Role::Switch
            })
            .map(|(id, _)| id)
            .expect("the input is an accessible shared switch");
        app.harness()
            .perform_a11y_action(target, &ui::a11y::Action::ToggleCheck)
            .unwrap();
        assert!(!app.state().recommendations.likes);
        assert!(app.undo());
        assert!(app.state().recommendations.likes);
    }

    #[test]
    fn recommendations_hidden_feedback_is_inspectable_and_restorable_through_accessibility() {
        let mut app =
            TestApp::<MusicShell>::capture_at(Some("recommendations-feedback"), (1280, 2400))
                .unwrap();
        assert!(app.state().recommendations.dismissed.contains("tk_g2"));
        let label = crate::i18n::t("music-recommendations-restore");
        let target = app
            .harness()
            .a11y_node_named(&label)
            .expect("Show again is exposed in the feedback section");
        app.harness()
            .perform_a11y_action(target, &ui::a11y::Action::Activate)
            .unwrap();
        assert!(!app.state().recommendations.dismissed.contains("tk_g2"));
        assert!(app.undo());
        assert!(app.state().recommendations.dismissed.contains("tk_g2"));
    }

    fn imported(id: &str) -> MusicAction {
        let track = crate::model::Track {
            id: id.into(),
            title: "Runaway".into(),
            album_id: "ytal_demons".into(),
            artist_id: "ytc_aurora".into(),
            track_no: 0,
            duration_s: 245,
            yt_id: "abc123".into(),
            plays: 0,
        };
        let album = crate::model::Album {
            id: "ytal_demons".into(),
            title: "All My Demons".into(),
            artist_id: "ytc_aurora".into(),
            year: 0,
        };
        let artist = crate::model::Artist {
            id: "ytc_aurora".into(),
            name: "AURORA".into(),
            genre: String::new(),
            monthly_listeners: 0,
        };
        crate::actions::import_track_action(&track, &album, &artist)
    }

    #[test]
    fn effects_reach_the_surface_through_the_door_and_ride_their_rows() {
        let mut app = TestApp::<MusicShell>::new().unwrap();
        for action in crate::seed::sample_actions() {
            app.invoke(Invoke::apply(&action).unwrap()).unwrap();
        }
        app.settle();

        let surface = app.agent_surface().to_string();
        assert!(surface.contains("music.player:player"), "{surface}");
        assert!(surface.contains("toggle-shuffle"));
        assert!(surface.contains("open-file"));
        assert!(surface.contains("music.source:catalogue"));
        assert!(surface.contains("connect-spotify"));
        assert!(surface.contains("disconnect-spotify"));
        assert!(surface.contains("browse-spotify"));
        assert!(surface.contains("browse-you-tube"));

        let album = crate::actions::album_resource_id("al_glasslight");
        app.effect_on(
            &album,
            &MusicEffect::Open {
                place: crate::actions::Place::Album("al_glasslight".into()),
            },
        )
        .expect("the album row opens its page");
        assert!(
            app.effect_on(
                &crate::actions::album_resource_id("al_gone"),
                &MusicEffect::Open {
                    place: crate::actions::Place::Album("al_gone".into()),
                },
            )
            .is_err(),
            "an album that is not in the library has no row to open"
        );
        assert!(
            app.effect(&MusicEffect::QueueAdd {
                track: "music.track:not-in-the-library".into(),
            })
            .is_err(),
            "a stale track is refused at the handler"
        );
        app.effect(&MusicEffect::QueueAdd {
            track: crate::actions::track_resource_id("tk_g1"),
        })
        .expect("a library track joins this device's queue");
        assert!(
            app.agent_surface()
                .to_string()
                .contains(&crate::actions::queue_item_resource_id(0, "tk_g1")),
            "the queue row is published once it holds the track"
        );
    }

    #[test]
    fn a_new_library_is_empty_and_its_containers_are_in_place() {
        let app = TestApp::<MusicShell>::new().unwrap();
        assert!(app.read_document(doc::is_seeded));
        let library = app.state();
        assert!(library.tracks.is_empty() && library.playlists.is_empty());
    }

    #[test]
    fn scenarios_stage_the_sample_library_through_actions() {
        let staged = TestApp::<MusicShell>::capture(Some("nowplaying")).unwrap();
        let library = staged.state();
        let sample = crate::seed::sample_library();
        assert_eq!(library.tracks, sample.tracks);
        assert_eq!(library.albums, sample.albums);
        assert_eq!(library.artists, sample.artists);
        assert_eq!(library.liked, sample.liked);
        assert_eq!(library.saved_albums, sample.saved_albums);
        let names: Vec<_> = library.playlists.iter().map(|p| p.name.clone()).collect();
        assert_eq!(names, ["Deep Focus", "Night Drive", "Slow Morning"]);
        for (staged, sample) in library.playlists.iter().zip(&sample.playlists) {
            assert_eq!(staged.track_ids, sample.track_ids);
        }

        let empty = TestApp::<MusicShell>::capture(Some("empty")).unwrap();
        assert!(empty.state().tracks.is_empty());
    }

    #[test]
    fn every_scenario_captures() {
        for scenario in MusicShell::scenarios() {
            TestApp::<MusicShell>::capture(Some(scenario))
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
        }
    }

    #[test]
    fn an_imported_track_joins_the_library_once_and_is_nobodys_undo_step() {
        let mut app = TestApp::<MusicShell>::new().unwrap();
        app.invoke(Invoke::apply(&imported("yt:abc123")).unwrap())
            .unwrap();
        app.invoke(Invoke::apply(&imported("yt:abc123")).unwrap())
            .unwrap();
        let library = app.state();
        assert_eq!(library.tracks.len(), 1);
        assert_eq!(library.artist_name("ytc_aurora"), "AURORA");
        assert_eq!(library.album_title("ytal_demons"), "All My Demons");
        assert!(!app.undo(), "content from outside is not an undo step");
        assert_eq!(app.state().tracks.len(), 1);
    }

    #[test]
    fn a_track_imported_without_its_own_album_is_refused() {
        let mut app = TestApp::<MusicShell>::new().unwrap();
        let MusicAction::ImportTrack(mut mismatched) = imported("yt:abc123") else {
            unreachable!()
        };
        mismatched.track.album_id = "elsewhere".into();
        let mismatched = MusicAction::ImportTrack(mismatched);
        assert!(app.invoke(Invoke::apply(&mismatched).unwrap()).is_err());
        assert!(app.state().tracks.is_empty());
    }
}
