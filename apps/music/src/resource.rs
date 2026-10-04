//! Music as a [`tilo_resource::Resource`]: the shared library (playlists,
//! likes, membership) is the state; now-playing presentation is View.

use std::rc::Rc;

use ui::framework::{Command, Fixture, ResourceApp, ResourceSurface, RunMode, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionParam, ActionSpec, ParamType};
use ui::tilo_resource::{ActionDecl, Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    MUSIC_ACTION_TYPE, MusicAction, MusicResourceCtx, create_playlist_action, document_children,
    new_playlist_id, open_local_file_spec, register,
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
                        "New Playlist".to_owned()
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
        document_children(&MusicResourceCtx::from_library(state))
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Open-file is a device Effect. Navigation destinations are child
        // `music.view` resources.
        vec![open_local_file_spec()]
    }

    fn search(state: &Library) -> Vec<SearchEntry> {
        let tracks = state.tracks.values().map(|track| SearchEntry {
            anchor: Some(crate::actions::track_resource_id(&track.id)),
            title: track.title.clone(),
            text: state.artist_name(&track.artist_id).to_owned(),
            action: None,
            ..Default::default()
        });
        let playlists = state.playlists.iter().map(|playlist| SearchEntry {
            anchor: Some(crate::actions::playlist_resource_id(&playlist.id)),
            title: playlist.name.clone(),
            text: String::new(),
            action: None,
            ..Default::default()
        });
        tracks.chain(playlists).collect()
    }
}

/// The screenshot scenarios: the sample library (`empty` has none) with the
/// view state each stages. The `live-*` scenarios show the real catalogue,
/// fetched into the capture's fixture before it is built.
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
    #[cfg(feature = "live")]
    crate::capture::LIVE_HOME,
    #[cfg(feature = "live")]
    crate::capture::LIVE_LYRICS,
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

    /// The `live-*` scenarios fetch the real catalogue they show now, before
    /// the capture is built; every other scenario shows the sample library.
    fn fixture(fixture: &mut Fixture<Music>) {
        if let Some(live) = crate::capture::LiveCapture::fetch(fixture.scenario()) {
            fixture.provide(live);
        }
    }

    /// Every scenario but `empty` and the live ones shows the sample library.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Music>> {
        if matches!(name, Some("empty")) || name.is_some_and(crate::capture::is_live) {
            return Vec::new();
        }
        crate::seed::sample_actions()
            .iter()
            .filter_map(|action| Invoke::apply(action).ok())
            .collect()
    }

    fn view(cx: &mut ViewCx<Music>) -> Element {
        // Only a window someone uses plays audio, reads this device's
        // playback profile and reaches the network; a capture shows its
        // fixture and a past version shows the library alone.
        let app = match cx.mode() {
            RunMode::Live => {
                let mut app = MusicApp::new();
                app.enable_mpris();
                app
            }
            RunMode::Capture => MusicApp::for_capture(cx.fixture::<crate::capture::LiveCapture>()),
            RunMode::Past => MusicApp::offline(),
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

impl ResourceSurface<Music> for MusicApp {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::framework::testing::TestApp;

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
            if crate::capture::is_live(scenario) {
                continue;
            }
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
