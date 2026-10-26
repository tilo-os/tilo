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

ui::embedded_catalog!(static CATALOG, ["en-US"]);

#[resource(
    kind = "music.library",
    label = "Music",
    params = library,
    version = 2,
    migrate = crate::library_migration::migrate,
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
                crate::i18n::t("music-recommendations-tune"),
                crate::i18n::t("music-recommendations-help"),
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
    "nowplaying-page",
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
    "library",
    "search",
    "search-chips",
    "search-remote-loading",
    "search-actions",
    "search-facet",
    "kept-view",
];

/// Framework app: the music widget is the surface over [`Library`].
pub struct MusicShell;

impl ResourceApp for MusicShell {
    type Resource = Library;
    const ID: &'static str = "so.tilo.music";
    const TITLE: &'static str = "Music";
    const ICON: Option<&'static str> = Some("music");
    /// A local audio file or a `music://` link to play or open.
    const ARGUMENTS: usize = 1;

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
        // The shell keeps the sidebar (the destinations, kept views and
        // playlists as a list), its width, its drawer in a narrow window
        // and its keys, and the transport across the window's foot: what
        // plays belongs to the window, whatever page is open.
        let places = ui::widget::NavFeed::new();
        let transport = ui::widget::TransportFeed::new();
        let pane = ui::LocalState::new(ui::widget::NavPane::Content);
        let shell = ui::widget::NavShell::new()
            .with_id("music-shell")
            .content_kind(ui::widget::ContentKind::List)
            .with_window_toolbar()
            .with_pane_state(&pane);
        let actions = app.transport_actions(&cx.actions());
        let app = app.in_shell(&places, &transport, shell.window_bar_link(), &pane);
        let sidebar = ui::widget::NavList::new(Vec::new())
            .with_id("music.places")
            .with_accessible_name("Music navigation")
            .fed_by(&places);
        Box::new(
            shell
                .sidebar(Box::new(sidebar))
                .content(cx.surface(app))
                .bottom_band(Box::new(ui::widget::MediaTransport::new(
                    &transport, actions,
                ))),
        )
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
        // The person's kept views follow them; the search wakes this window
        // when the catalogue answers.
        self.attach_views(cx.settings::<ui::framework::views::KeptViews>(), cx.waker());
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
        // The song that plays is the one playing as the effect runs, not
        // the one a control saw when it was bound.
        let effect = match effect {
            MusicEffect::ToggleLikePlaying => match self.playing_track() {
                Some(track) => MusicEffect::ToggleLike { track },
                None => return Err("Nothing is playing".into()),
            },
            effect => effect,
        };
        if let MusicEffect::ToggleLike { track } = &effect {
            if cx.dispatcher().read_only() {
                return Err("Liking a song requires permission to edit the library".into());
            }
            let current = cx.live().get();
            let _gesture = cx.dispatcher().begin_group();
            if !current.tracks.contains_key(track) {
                cx.submit(self.catalogue_import(track)?);
            }
            cx.submit(
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

    const ROW_DECLARATIONS: bool = true;

    /// The library's field edits bound to the row they act on: saving an
    /// album, following an artist, taking a song out of a playlist or
    /// moving it, and the recommendations' switches and feedback.
    fn row_declarations(&self, row: &str) -> Vec<tilo_resource::ActionDecl<Library>> {
        self.row_edits(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::framework::testing::TestApp;

    fn edit(edit: &tilo_resource::Edit<Library>) -> Invoke<Library> {
        Invoke::edit(edit).unwrap()
    }

    /// History stands beside Music's slots in the window's bar: no band of
    /// the window's own above the page and no control of Music's own; the
    /// bar's History opens the framework's History.
    #[test]
    fn history_is_the_window_bars_beside_musics_slots() {
        use ui::widget::Widget as _;
        let mut app = TestApp::<MusicShell>::capture_at(Some("nowplaying"), (1440, 810)).unwrap();
        app.harness()
            .set_theme(ui::theme::Theme::dark().with_ui_scale(1.5));
        app.harness().relayout();
        let surface = app.surface_mut::<MusicApp>();
        assert!(
            surface.get_rect().y0.abs() < f64::EPSILON,
            "no History band above the page"
        );
        assert!(
            !surface.children().into_iter().any(|child| child
                .accessibility()
                .is_some_and(|node| node.name.as_deref() == Some("History"))),
            "Music holds no History control of its own"
        );
        assert!(
            ui::window_toolbar::declared().is_some(),
            "Music's controls are its window bar's slots"
        );
        assert_eq!(
            app.harness().window_history_control(),
            Some(ui::widget::HistoryControl::Closed),
            "the window's bar offers History beside them"
        );
        app.harness().press_window_history().unwrap();
        assert_eq!(
            app.harness().window_history_control(),
            Some(ui::widget::HistoryControl::Open),
            "the bar's History opens the framework's History"
        );
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
    fn spotify_account_status_is_published_on_the_empty_library() {
        use ui::tilo_resource::Effect as _;
        let mut app = TestApp::<MusicShell>::new().unwrap();
        app.settle();
        let surface = app.agent_surface().to_string();
        assert!(surface.contains("spotify-account-status"), "{surface}");
        assert!(surface.contains("music.library"), "{surface}");
        let spec = MusicEffect::specs()
            .into_iter()
            .find(|spec| spec.id == "spotify-account-status")
            .expect("the semantic diagnostic must be registered");
        assert_eq!(spec.contract.kind, tilo_actions::ActionKind::Read);
        let encoded = spec.build_json(&serde_json::Map::new()).unwrap();
        assert!(matches!(
            MusicEffect::from_payload(&encoded.payload).unwrap(),
            MusicEffect::SpotifyAccountStatus
        ));
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
        let label = crate::i18n::t("music-recommendations-restore");
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

    /// Deleting a playlist runs at once, since Undo takes it back: the host
    /// asks nothing, and one Undo brings the playlist back with its songs.
    #[test]
    fn deleting_a_playlist_runs_at_once_and_one_undo_brings_it_back() {
        let mut app = TestApp::<MusicShell>::new().unwrap();
        app.invoke(imported("yt:abc123")).unwrap();
        app.invoke(Library::CREATE_PLAYLIST.with(library::CreatePlaylist {
            name: "Road mix".into(),
            track: Some("yt:abc123".into()),
        }))
        .unwrap();
        let mix = app.state().playlists().next().unwrap().id.clone();

        app.invoke(Library::DELETE_PLAYLIST.with(Id::from_raw(mix.as_str())))
            .expect("an undoable delete asks nothing");
        assert!(app.asking().is_none(), "the host asks nothing");
        assert!(
            app.state().playlist(&mix).is_none(),
            "the delete lands at once"
        );

        assert!(app.undo(), "the delete is one undo step");
        let state = app.state();
        let back = state.playlist(&mix).expect("Undo brings the playlist back");
        assert_eq!(back.name, "Road mix");
        assert_eq!(back.track_ids(), ["yt:abc123"], "and its songs with it");
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

    /// Rows offer the library's edits bound to what they show: an album
    /// is saved, an artist followed, a recommendation input switched, a
    /// suggestion hidden and shown again, an artist preferred; a song in a
    /// playlist is moved and taken out.
    #[test]
    fn rows_offer_their_bound_field_edits() {
        let mut app = TestApp::<MusicShell>::capture(Some("sidebar")).unwrap();
        let offered = |app: &mut TestApp<MusicShell>, row: &str, id: &str| {
            app.call_on(row, id, serde_json::json!({})).map(drop)
        };
        let album = crate::actions::album_resource_id("al_strata");
        let saved = app.state().is_album_saved("al_strata");
        offered(&mut app, &album, "toggle-save").unwrap();
        assert_eq!(app.state().is_album_saved("al_strata"), !saved);
        assert!(app.undo(), "a bound edit is an undo step");
        assert_eq!(app.state().is_album_saved("al_strata"), saved);

        let artist = crate::actions::artist_resource_id("ar_kavi");
        let followed = app.state().is_artist_followed("ar_kavi");
        offered(&mut app, &artist, "toggle-follow").unwrap();
        assert_eq!(app.state().is_artist_followed("ar_kavi"), !followed);

        offered(&mut app, "music.recommendation:tk_g2", "dismiss").unwrap();
        assert!(app.state().recommendations.is_dismissed("tk_g2"));
        offered(&mut app, "music.recommendation:tk_g2", "restore").unwrap();
        assert!(!app.state().recommendations.is_dismissed("tk_g2"));
        offered(
            &mut app,
            "music.recommendation-artist:ar_kavi",
            "set-preferred",
        )
        .unwrap();
        assert!(app.state().recommendations.prefers("ar_kavi"));

        let playlist = app.state().playlist("pl_morning").cloned().unwrap();
        let ids = playlist.track_ids();
        let surface = app.surface_mut::<MusicApp>();
        // What each row offers follows what it shows.
        let inputs: Vec<&str> = surface
            .row_edits("music.recommendation-input:likes")
            .iter()
            .map(|decl| decl.id)
            .collect();
        assert_eq!(inputs, ["set-input"]);
        let first = crate::actions::playlist_track_resource_id("pl_morning", &ids[0]);
        let edits: Vec<&str> = surface
            .row_edits(&first)
            .iter()
            .map(|decl| decl.id)
            .collect();
        assert_eq!(
            edits,
            ["remove", "move-down"],
            "the first song moves only down"
        );
        let last = crate::actions::playlist_track_resource_id("pl_morning", &ids[ids.len() - 1]);
        let edits: Vec<&str> = surface
            .row_edits(&last)
            .iter()
            .map(|decl| decl.id)
            .collect();
        assert_eq!(edits, ["remove", "move-up"]);
        assert!(
            surface
                .row_edits(&crate::actions::playlist_track_resource_id(
                    "pl_morning",
                    "tk_gone"
                ))
                .is_empty(),
            "a song the playlist does not hold offers nothing"
        );
    }

    /// A playlist's row renames it, sets its cover and adds a song of the
    /// library at its end, each asking for its value.
    #[test]
    fn playlist_rows_rename_cover_and_add_songs() {
        let mut app = TestApp::<MusicShell>::capture(Some("sidebar")).unwrap();
        let morning = crate::actions::playlist_resource_id("pl_morning");
        app.call_on(
            &morning,
            "rename",
            serde_json::json!({ "name": "Slow start" }),
        )
        .unwrap();
        assert_eq!(
            app.state().playlist("pl_morning").unwrap().name,
            "Slow start"
        );
        app.call_on(
            &morning,
            "add-track",
            serde_json::json!({ "track": "music.track:tk_a1" }),
        )
        .unwrap();
        assert_eq!(
            app.state()
                .playlist("pl_morning")
                .unwrap()
                .track_ids()
                .last(),
            Some(&"tk_a1".to_owned())
        );
        assert!(
            app.call_on(
                &morning,
                "add-track",
                serde_json::json!({ "track": "tk_gone" })
            )
            .is_err(),
            "only a song of the library is added"
        );
        assert!(
            app.call_on(
                &morning,
                "set-cover-url",
                serde_json::json!({ "cover": "file:///tmp/cover.png" })
            )
            .is_err(),
            "a cover is portable"
        );
        app.call_on(
            &morning,
            "set-cover-url",
            serde_json::json!({ "cover": "https://example.test/cover.png" }),
        )
        .unwrap();
        assert_eq!(
            app.state().playlist("pl_morning").unwrap().cover,
            "https://example.test/cover.png"
        );
    }

    /// An agent reaches the edits of rows shown inside other rows through
    /// the published tree: a song of a playlist moves and comes out, an
    /// input of the recommendations switches.
    #[test]
    fn nested_rows_offer_their_edits_in_the_published_tree() {
        let mut app = TestApp::<MusicShell>::capture(Some("sidebar")).unwrap();
        let ids = app.state().playlist("pl_morning").unwrap().track_ids();
        let row = |track: &str| crate::actions::playlist_track_resource_id("pl_morning", track);
        let published = app.harness().resources();
        let first = tilo_actions::flatten(&published)
            .into_iter()
            .filter(|flat| flat.resource_id.as_str() == row(&ids[0]))
            .map(|flat| flat.action.id.clone())
            .collect::<Vec<_>>();
        for offered in ["remove", "move-down"] {
            assert!(
                first.iter().any(|id| id == offered),
                "{offered} in {first:?}"
            );
        }

        app.call_on(&row(&ids[0]), "move-down", serde_json::json!({}))
            .unwrap();
        let moved = app.state().playlist("pl_morning").unwrap().track_ids();
        assert_eq!(
            (moved[0].as_str(), moved[1].as_str()),
            (ids[1].as_str(), ids[0].as_str())
        );
        app.call_on(&row(&ids[0]), "move-up", serde_json::json!({}))
            .unwrap();
        assert_eq!(app.state().playlist("pl_morning").unwrap().track_ids(), ids);
        app.call_on(&row(&ids[1]), "remove", serde_json::json!({}))
            .unwrap();
        let left = app.state().playlist("pl_morning").unwrap().track_ids();
        assert!(!left.contains(&ids[1]), "the song came out of the playlist");
        assert!(
            app.state().tracks.contains_key(&ids[1]),
            "and stays in the library"
        );

        let likes = app.state().recommendations.likes;
        app.call_on(
            "music.recommendation-input:likes",
            "set-input",
            serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(app.state().recommendations.likes, !likes);
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

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// The walk's slices: a drive of one scenario walks dozens of actions,
    /// so each slice is its own test and they run side by side.
    const SLICES: usize = 6;

    /// Every action the app publishes is reachable by address in a drive of
    /// every `SLICES`th scenario from `slice`: none is advertised yet
    /// handled only by a click. The scenarios stage fixtures; the app
    /// someone opens acts on this machine, so it is not walked.
    fn walk(slice: usize) {
        for scenario in super::MusicShell::scenarios()
            .iter()
            .skip(slice)
            .step_by(SLICES)
        {
            let mut app = TestApp::<super::MusicShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }

    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action_0() {
        walk(0);
    }

    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action_1() {
        walk(1);
    }

    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action_2() {
        walk(2);
    }

    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action_3() {
        walk(3);
    }

    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action_4() {
        walk(4);
    }

    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action_5() {
        walk(5);
    }
}
