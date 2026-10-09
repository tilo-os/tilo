//! Music's library as the framework resource: the shared catalog,
//! playlists, likes and recommendation feedback are the [`Library`] model;
//! what this device plays and keeps offline is the surface's.
//!
//! Likes, saved albums, followed artists, recommendation feedback and a
//! playlist's name, cover and tracks are field edits at their paths; the
//! verbs are the operations that are more than setting a field: making and
//! deleting a playlist, and bringing a catalogue song into the library.

use std::rc::Rc;

use ui::framework::{
    Command, EffectAnswer, EffectCx, LiveRow, ResourceApp, ResourceSurface, RunMode, SurfaceCx,
    ViewCx,
};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::{self, ActionCx, Id, Invoke, Refusal, SearchEntry, resource};
use ui::widget::Element;

use crate::actions::MusicEffect;
use crate::app::MusicApp;
use crate::model::{CatalogueTrack, Library, Playlist};

/// Music's Fluent catalog of history sentences.
pub const LOCALES: &[(&str, &str)] = &[("en-US", include_str!("../locales/en-US.ftl"))];

static CATALOG: EmbeddedCatalog =
    EmbeddedCatalog::new("en-US", LOCALES, "music-create-playlist").with_system_locale();

#[resource(
    kind = "music.library",
    label = "Music",
    params = library,
    search = Library::search_entries
)]
impl Library {
    /// Create a new playlist.
    #[action]
    pub fn create_playlist(
        &mut self,
        cx: &mut ActionCx,
        #[param("Playlist name; blank for New playlist")] name: String,
        #[param("A song of the library to put in it first")] track: Option<String>,
    ) -> Result<(), Refusal> {
        let name = name.trim();
        let name = if name.is_empty() {
            "New playlist"
        } else {
            name
        };
        let id = cx.id();
        let mut playlist = Playlist::named(id.as_str(), name);
        if let Some(track) = track {
            if !self.tracks.contains_key(&track) {
                return Err(Refusal::new("the song is not in the library"));
            }
            playlist.push(&track);
        }
        self.playlists.push(id, playlist);
        Ok(())
    }

    /// Delete a playlist.
    #[action(destructive)]
    pub fn delete_playlist(&mut self, playlist: &Id) -> Result<(), Refusal> {
        self.playlists
            .remove(playlist)
            .map(drop)
            .ok_or_else(|| Refusal::new("playlist is no longer in the library"))
    }

    /// Add a song to the library.
    ///
    /// A catalogue song, with its album and artist, so it stays playable,
    /// likeable and listable on every device. What the library holds
    /// already is kept as it is. Content from outside the library: nobody's
    /// undo step takes it back.
    #[action(external)]
    pub fn import_track(
        &mut self,
        #[param("The song with its own album and artist")] song: CatalogueTrack,
    ) -> Result<(), Refusal> {
        let CatalogueTrack {
            track,
            album,
            artist,
        } = song;
        if track.id.is_empty()
            || album.id.is_empty()
            || artist.id.is_empty()
            || track.album_id != album.id
            || track.artist_id != artist.id
            || album.artist_id != artist.id
        {
            return Err(Refusal::new(
                "a track is imported with its own album and artist",
            ));
        }
        self.artists.entry(artist.id.clone()).or_insert(artist);
        self.albums.entry(album.id.clone()).or_insert(album);
        self.tracks.entry(track.id.clone()).or_insert(track);
        Ok(())
    }
}

impl Library {
    /// Songs, albums, artists, playlists and what each playlist holds, and
    /// the recommendations' settings.
    fn search_entries(&self) -> Vec<SearchEntry> {
        let mut entries = crate::search_type::library_entries(self);
        entries.push(
            SearchEntry::new(
                ui::i18n!("music-recommendations-tune"),
                ui::i18n!("music-recommendations-help"),
            )
            .anchored(crate::recommendations::PREFERENCES),
        );
        entries
    }

    /// The path of the playlist `id`.
    pub fn playlist_paths(id: &str) -> <Playlist as tilo_resource::Model>::Paths<Library> {
        Library::paths().playlists.item(&Id::from_raw(id))
    }

    /// The edit that likes or unlikes `track`.
    pub fn like(track: &str, liked: bool) -> tilo_resource::Edit<Library> {
        let liked_songs = Library::paths().liked;
        if liked {
            liked_songs.insert(&track.to_owned())
        } else {
            liked_songs.remove(&track.to_owned())
        }
    }

    /// The edit that saves or unsaves `album`.
    pub fn save_album(album: &str, saved: bool) -> tilo_resource::Edit<Library> {
        let saved_albums = Library::paths().saved_albums;
        if saved {
            saved_albums.insert(&album.to_owned())
        } else {
            saved_albums.remove(&album.to_owned())
        }
    }

    /// The edit that follows or unfollows `artist`.
    pub fn follow_artist(artist: &str, followed: bool) -> tilo_resource::Edit<Library> {
        let followed_artists = Library::paths().followed_artists;
        if followed {
            followed_artists.insert(&artist.to_owned())
        } else {
            followed_artists.remove(&artist.to_owned())
        }
    }

    /// The edit that adds `track` to the end of `playlist`.
    pub fn add_to_playlist(
        &self,
        playlist: &str,
        track: &str,
    ) -> Option<tilo_resource::Edit<Library>> {
        let at = self.playlist(playlist)?.len();
        Some(Self::playlist_paths(playlist).tracks.insert_as(
            &Id::from_raw(track),
            at,
            &track.to_owned(),
        ))
    }

    /// The edit that takes `track` out of `playlist`.
    pub fn remove_from_playlist(playlist: &str, track: &str) -> tilo_resource::Edit<Library> {
        Self::playlist_paths(playlist)
            .tracks
            .remove(&Id::from_raw(track))
    }

    /// The edit that moves `track` to position `to` in `playlist`.
    pub fn move_in_playlist(
        playlist: &str,
        track: &str,
        to: usize,
    ) -> tilo_resource::Edit<Library> {
        Self::playlist_paths(playlist)
            .tracks
            .move_to(&Id::from_raw(track), to)
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
    type Resource = Library;
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
    fn scenario(name: Option<&str>) -> Vec<Invoke<Library>> {
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
        let mut invokes = crate::seed::sample_actions();
        if name == Some("recommendations-feedback") {
            let recommendations = Library::paths().recommendations;
            invokes.extend(
                [
                    recommendations.dismissed.insert(&"tk_g2".to_owned()),
                    recommendations
                        .preferred_artists
                        .insert(&"ar_kavi".to_owned()),
                ]
                .iter()
                .filter_map(|edit| Invoke::edit(edit).ok()),
            );
        }
        invokes
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn view(cx: &mut ViewCx<Library>) -> Element {
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
        // A search hit, or an agent's `open`, plays the song or shows the
        // page the row it names does.
        cx.on_open(app.opener());
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

impl ResourceSurface<Library, MusicEffect> for MusicApp {
    fn present(&mut self, state: &Rc<Library>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Library>) {
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

    fn effect(&mut self, effect: MusicEffect, cx: &mut EffectCx<Library, Self>) -> EffectAnswer {
        if let MusicEffect::ToggleLike { track } = &effect {
            if cx.dispatcher().read_only() {
                return Err("Liking a song requires permission to edit the library".into());
            }
            let current = cx.live().get();
            let _gesture = cx.dispatcher().begin_group();
            if !current.tracks.contains_key(track) {
                cx.invoke(self.catalogue_import(track)?);
            }
            cx.invoke(
                Invoke::edit(&Library::like(track, !current.is_liked(track)))
                    .map_err(|error| error.to_string())?,
            );
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

    fn edit(edit: &tilo_resource::Edit<Library>) -> Invoke<Library> {
        Invoke::edit(edit).unwrap()
    }

    #[test]
    fn recommendations_preferences_are_addressable_on_an_empty_library() {
        let resource = crate::recommendations::resource_by_id(
            &Library::default(),
            "music.recommendation-preferences:library",
        )
        .expect("recommendation inputs must be available without any music or account");
        assert_eq!(resource.kind, "music.recommendation-preferences");
    }

    #[test]
    fn recommendations_feedback_uses_framework_history_and_undo() {
        let mut app = TestApp::<MusicShell>::new().unwrap();
        app.invoke(imported("yt:abc123")).unwrap();
        let dismissed = Library::paths().recommendations.dismissed;
        app.invoke(edit(&dismissed.insert(&"yt:abc123".to_owned())))
            .unwrap();
        assert!(app.state().recommendations.is_dismissed("yt:abc123"));
        assert!(app.undo());
        assert!(!app.state().recommendations.is_dismissed("yt:abc123"));
        assert!(
            app.invoke(edit(&dismissed.insert(&"yt:gone".to_owned())))
                .is_err(),
            "only a song of the library is dismissed"
        );
    }

    #[test]
    fn recommendations_viewers_cannot_change_inputs_or_feedback() {
        let mut app = TestApp::<MusicShell>::new().unwrap();
        app.invoke(imported("yt:abc123")).unwrap();
        let before = app.state().recommendations;
        app.set_role(ui::permissions::Role::Viewer);
        let recommendations = Library::paths().recommendations;
        for change in [
            recommendations.likes.set(false),
            recommendations.dismissed.insert(&"yt:abc123".to_owned()),
            recommendations
                .preferred_artists
                .insert(&"ytc_aurora".to_owned()),
        ] {
            assert!(app.invoke(edit(&change)).is_err());
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
        assert!(app.state().recommendations.is_dismissed("tk_g2"));
        let label = ui::i18n!("music-recommendations-restore");
        let target = app
            .harness()
            .a11y_node_named(&label)
            .expect("Show again is exposed in the feedback section");
        app.harness()
            .perform_a11y_action(target, &ui::a11y::Action::Activate)
            .unwrap();
        assert!(!app.state().recommendations.is_dismissed("tk_g2"));
        assert!(app.undo());
        assert!(app.state().recommendations.is_dismissed("tk_g2"));
    }

    fn imported(id: &str) -> Invoke<Library> {
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
        Library::IMPORT_TRACK.with(crate::model::CatalogueTrack {
            track,
            album,
            artist,
        })
    }

    #[test]
    fn effects_reach_the_surface_through_the_door_and_ride_their_rows() {
        let mut app = TestApp::<MusicShell>::new().unwrap();
        for action in crate::seed::sample_actions() {
            app.invoke(action).unwrap();
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

    /// A search hit's or an agent's `open` plays the song or shows the page
    /// of the row its anchor names; an anchor naming nothing is refused.
    #[test]
    fn opening_an_anchor_plays_or_shows_its_row() {
        let mut app = TestApp::<MusicShell>::capture(Some("sidebar")).unwrap();
        app.call(
            "open",
            serde_json::json!({ "anchor": crate::actions::album_resource_id("al_glasslight") }),
        )
        .unwrap();
        app.settle();
        assert_eq!(
            app.surface_mut::<MusicApp>().view_for_test(),
            crate::app::View::Album("al_glasslight".into())
        );
        assert!(
            app.call("open", serde_json::json!({ "anchor": "music.nothing:x" }))
                .is_err()
        );
    }

    #[test]
    fn a_new_library_is_empty() {
        let app = TestApp::<MusicShell>::new().unwrap();
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
        let names: Vec<_> = library.playlists().map(|p| p.name.clone()).collect();
        assert_eq!(names, ["Deep Focus", "Night Drive", "Slow Morning"]);
        for (staged, sample) in library.playlists().zip(sample.playlists()) {
            assert_eq!(staged.track_ids(), sample.track_ids());
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
        app.invoke(imported("yt:abc123")).unwrap();
        app.invoke(imported("yt:abc123")).unwrap();
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
        let song = |album_id: &str, album_artist: &str| crate::model::CatalogueTrack {
            track: crate::model::Track {
                id: "yt:abc123".into(),
                title: "Runaway".into(),
                album_id: album_id.into(),
                artist_id: "ytc_aurora".into(),
                track_no: 0,
                duration_s: 245,
                yt_id: "abc123".into(),
                plays: 0,
            },
            album: crate::model::Album {
                id: "ytal_demons".into(),
                title: "All My Demons".into(),
                artist_id: album_artist.into(),
                year: 0,
            },
            artist: crate::model::Artist {
                id: "ytc_aurora".into(),
                name: "AURORA".into(),
                genre: String::new(),
                monthly_listeners: 0,
            },
        };
        for mismatched in [
            song("elsewhere", "ytc_aurora"),
            song("ytal_demons", "someone else"),
        ] {
            assert!(app.invoke(Library::IMPORT_TRACK.with(mismatched)).is_err());
        }
        assert!(app.state().tracks.is_empty());
    }
}

#[cfg(test)]
mod model_tests;
