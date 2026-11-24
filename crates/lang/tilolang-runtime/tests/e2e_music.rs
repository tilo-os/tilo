//! The Music app (`apps/tl/music/app.tl`) end to end: the songs a catalogue brought in, a like
//! and a kept album that turn into suggestions with their reason, a hidden song and a preferred
//! artist, and the home and taste pages that show them.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{InvokeOutcome, ResourceRef, RunOutcome, Runtime, Value, ViewInstance};

    const SOURCE: &str = include_str!("../../../../apps/tl/music/app.tl");
    /// The session Music plays through, so its `use` resolves and a suggestion plays.
    const PLAYBACK: &str = include_str!("../../../../apps/tl/playback/app.tl");

    /// Lets every call run: this test is about what the app does, not what a workspace lets it.
    #[derive(Clone, Copy, Default)]
    struct Trusting;

    impl tilolang_runtime::Authority for Trusting {
        fn decide(
            &self,
            _invoker: &tilolang_runtime::Actor,
            _chain: &[tilolang_runtime::ids::ModuleHash],
            _action: &tilolang_runtime::ActionInfo,
        ) -> tilolang_runtime::Decision {
            tilolang_runtime::Decision::Allow
        }
    }

    struct Shelf {
        rt: Runtime,
        app: App,
        lib: ResourceRef,
        dispatch: Dispatch,
    }

    impl Shelf {
        fn new() -> Self {
            let rt = Runtime::new(tilolang_runtime::AmStore::new(), Trusting).unwrap();
            let dispatch = Dispatch::new();
            rt.set_foreign_invoker(dispatch.clone());
            let playback = App::compile(PLAYBACK, &[]);
            playback.load(&rt);
            let app = App::compile(SOURCE, &[playback.meta().clone()]);
            app.load(&rt);
            let lib = rt.create_root("Library").unwrap();
            let shelf = Self {
                rt,
                app,
                lib,
                dispatch,
            };
            // The app's own start, which the host runs when the library is made.
            shelf.call("start", vec![Value::resource(&shelf.lib)]);
            shelf
        }

        /// Runs `action`, playing through the session where it asks to: the player's own actions
        /// are the playback app's, so each call it waits on is answered as that app would.
        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            let mut outcome = self
                .rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|why| panic!("{action}: {why}"));
            while let InvokeOutcome::Waits { run, .. } = outcome {
                outcome = match self.dispatch.allow(&self.rt, &run) {
                    RunOutcome::Done(value) => InvokeOutcome::Done(value),
                    RunOutcome::Waits { run, call } => InvokeOutcome::Waits { run, call },
                };
            }
            outcome
        }

        /// A song the catalogue brought in, as its answer writes one.
        fn song(&self, title: &str, artist: &str, artist_key: &str, album: &str) -> ResourceRef {
            seed_origin(
                &self.rt,
                "music.track",
                &self.lib,
                &field_id(&self.app, "Library", "tracks"),
                &type_id(&self.app, "Track"),
                vec![
                    ("key", Value::text(format!("k-{title}"))),
                    ("title", Value::text(title)),
                    ("artist", Value::text(artist)),
                    ("artist_key", Value::text(artist_key)),
                    ("album", Value::text(album)),
                    ("album_key", Value::text(format!("a-{album}"))),
                    ("seconds", Value::Int(200)),
                    (
                        "stream",
                        Value::text(format!("https://music.example/{title}.wav")),
                    ),
                ],
            )
        }

        fn artist(&self, name: &str, key: &str) -> ResourceRef {
            seed_origin(
                &self.rt,
                "music.artist",
                &self.lib,
                &field_id(&self.app, "Library", "artists"),
                &type_id(&self.app, "Artist"),
                vec![("key", Value::text(key)), ("name", Value::text(name))],
            )
        }

        fn like(&self, t: &ResourceRef) {
            self.call("like", vec![Value::resource(t), Value::Bool(true)]);
        }

        /// Sets a field of a song, as the catalogue's answer would have.
        fn write_field(&self, at: &ResourceRef, ty: &str, field: &str, value: Value) {
            let write = tilolang_runtime::origin::OriginWrite::Set {
                target: at.clone(),
                fields: vec![(field_id(&self.app, ty, field), value)],
            };
            self.rt
                .write_origin("music.track", None, |change| change.write(vec![write]))
                .unwrap_or_else(|why| panic!("writing {ty}.{field}: {why}"));
        }

        /// The payloads the app asked the catalogue for through `capability`, in order.
        fn asked_for(&self, capability: &str) -> Vec<serde_json::Value> {
            self.rt
                .pending_outbox()
                .into_iter()
                .filter(|entry| entry.capability == capability)
                .map(|entry| entry.payload)
                .collect()
        }

        fn window(&self) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(
                    &me(),
                    &self.app.view("library"),
                    vec![Value::resource(&self.lib)],
                )
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            window
        }

        /// The titles of the suggestions the home page shows, in order. Nothing to suggest is no
        /// section at all, so that is no titles.
        fn suggestions(&self) -> Vec<String> {
            let window = self.window();
            let tree = window.tree();
            let Some(section) = of_kind(tree, "ui.Section").into_iter().find(|s| {
                s.props.get("title").and_then(Value::as_text) == Some("Made from your library")
            }) else {
                return Vec::new();
            };
            section
                .of_kind("ui.ListRow")
                .into_iter()
                .filter_map(|row| row.props.get("title").and_then(Value::as_text))
                .map(str::to_owned)
                .collect()
        }

        /// The reason the home page gives for `title`, when it suggests it.
        fn reason(&self, title: &str) -> Option<String> {
            let window = self.window();
            let tree = window.tree();
            of_kind(tree, "ui.ListRow")
                .into_iter()
                .find(|row| row.props.get("title").and_then(Value::as_text) == Some(title))
                .and_then(|row| row.props.get("secondary").and_then(Value::as_text))
                .map(str::to_owned)
        }

        /// The titles of the rows on the taste page under `section`.
        fn taste_rows(&self, section: &str) -> Vec<String> {
            let mut page = self
                .rt
                .open_view(
                    &me(),
                    &self.app.view("taste_page"),
                    vec![Value::resource(&self.lib)],
                )
                .unwrap();
            page.set_lazy_rows(false).unwrap();
            let tree = page.tree();
            let Some(found) = of_kind(tree, "ui.Section")
                .into_iter()
                .find(|s| s.props.get("title").and_then(Value::as_text) == Some(section))
            else {
                return Vec::new();
            };
            found
                .of_kind("ui.ListRow")
                .into_iter()
                .filter_map(|row| row.props.get("title").and_then(Value::as_text))
                .map(str::to_owned)
                .collect()
        }
    }

    tilo_test_env::scenario! {
        fn music_suggests_from_the_library_and_says_why;

        phase a_like_brings_the_artist_s_other_song_with_its_reason;
        {
            let s = Shelf::new();
            let first = s.song("First Light", "Ava", "ava", "Morning");
            let blue = s.song("Blue Hour", "Ava", "ava", "Morning");
            s.song("Elsewhere", "Kai", "kai", "Night");
            s.like(&first);
            assert_eq!(s.suggestions(), vec!["Blue Hour".to_owned()]);
            assert_eq!(
                s.reason("Blue Hour").as_deref(),
                Some("Because you liked First Light.")
            );
            // A suggested song plays through the shared session.
            s.call("play_rec", vec![Value::resource(&blue)]);
            let playing = match s.rt.read(&s.lib, "playing").unwrap() {
                Value::Opt(Some(playback)) => match *playback {
                    Value::Ref(r) => std::rc::Rc::unwrap_or_clone(r.target),
                    other => panic!("playing reads {other:?}"),
                },
                other => panic!("playing reads {other:?}"),
            };
            assert_eq!(
                s.rt.read(&playing, "track").unwrap(),
                Value::text(blue.id_string())
            );
        }

        phase a_kept_album_and_a_followed_artist_are_reasons_of_their_own;
        {
            let s = Shelf::new();
            s.song("Kept", "Ava", "ava", "Morning");
            s.song("Later", "Ava", "ava", "Morning");
            let release = seed_origin(
                &s.rt,
                "music.album",
                &s.lib,
                &field_id(&s.app, "Library", "albums"),
                &type_id(&s.app, "Album"),
                vec![
                    ("key", Value::text("a-Morning")),
                    ("title", Value::text("Morning")),
                    ("artist", Value::text("Ava")),
                    ("artist_key", Value::text("ava")),
                ],
            );
            s.call("save_album", vec![Value::resource(&release), Value::Bool(true)]);
            // The artist is followed, which is the reason that comes first.
            let ava = s.artist("Ava", "ava");
            s.call("follow_artist", vec![Value::resource(&ava), Value::Bool(true)]);
            assert_eq!(
                s.reason("Later").as_deref(),
                Some("Because you follow Ava."),
                "a followed artist explains a song before a kept album does"
            );
            // With the artist unfollowed the kept album is the reason.
            s.call("follow_artist", vec![Value::resource(&ava), Value::Bool(false)]);
            assert_eq!(s.reason("Later").as_deref(), Some("From an album you kept."));
        }

        phase hiding_a_song_and_turning_an_input_off_take_it_away;
        {
            let s = Shelf::new();
            let first = s.song("First Light", "Ava", "ava", "Morning");
            let blue = s.song("Blue Hour", "Ava", "ava", "Morning");
            s.like(&first);
            assert_eq!(s.suggestions(), vec!["Blue Hour".to_owned()]);
            s.call("hide_rec", vec![Value::resource(&s.lib), Value::resource(&blue)]);
            assert!(s.suggestions().is_empty(), "a hidden song is not suggested");
            assert_eq!(s.taste_rows("Hidden songs"), vec!["Blue Hour".to_owned()]);
            s.call("restore_rec", vec![Value::resource(&s.lib), Value::resource(&blue)]);
            assert_eq!(s.suggestions(), vec!["Blue Hour".to_owned()], "and comes back");
            // A taste input the person turned off withdraws what it drew on.
            s.call(
                "update_taste",
                vec![
                    Value::resource(&s.lib),
                    Value::text("liked_songs"),
                    Value::Bool(false),
                ],
            );
            assert!(s.suggestions().is_empty(), "likes are off, so nothing comes from them");
        }

        phase a_preferred_artist_s_songs_come_first;
        {
            let s = Shelf::new();
            let ava_song = s.song("First Light", "Ava", "ava", "Morning");
            let kai_song = s.song("Elsewhere", "Kai", "kai", "Night");
            s.like(&ava_song);
            s.like(&kai_song);
            s.song("Blue Hour", "Ava", "ava", "Morning");
            s.song("Night Bus", "Kai", "kai", "Night");
            let kai = s.artist("Kai", "kai");
            s.call("prefer_artist", vec![Value::resource(&s.lib), Value::resource(&kai)]);
            assert_eq!(
                s.suggestions().first().map(String::as_str),
                Some("Night Bus"),
                "the artist asked for comes first"
            );
        }

        phase a_catalogue_playlists_words_stand_on_its_page;
        {
            let s = Shelf::new();
            let playlist = seed_origin(
                &s.rt,
                "music.playlist",
                &s.lib,
                &field_id(&s.app, "Library", "playlists"),
                &type_id(&s.app, "Playlist"),
                vec![
                    ("key", Value::text("pl-1")),
                    ("title", Value::text("Roads")),
                    ("description", Value::text("Songs for the drive home")),
                    ("found_for", Value::text("roads")),
                ],
            );
            let mut page = s
                .rt
                .open_view(&me(), &s.app.view("playlist"), vec![Value::resource(&playlist)])
                .unwrap();
            page.set_lazy_rows(false).unwrap();
            let words: Vec<String> = of_kind(page.tree(), "ui.Text")
                .into_iter()
                .filter_map(|node| node.props.get("value").and_then(Value::as_text))
                .map(str::to_owned)
                .collect();
            assert!(
                words.iter().any(|word| word == "Songs for the drive home"),
                "the playlist's own words stand on its page: {words:?}"
            );
        }

        phase a_stream_cut_short_says_where_and_plays_on_from_there;
        {
            let s = Shelf::new();
            let song = s.song("Blue Hour", "Ava", "ava", "Morning");
            s.call("play_song", vec![Value::resource(&song)]);
            // The engine stopped at 1:23 and said why.
            let window = s.window();
            let band = of_kind(window.tree(), "ui.MediaTransport")
                .into_iter()
                .next()
                .expect("the player is in the window");
            window
                .fire_with(
                    &band.key,
                    "fail",
                    Some(&Value::List(vec![
                        Value::text("the source refused the rest of the stream"),
                        Value::Float(83.0),
                    ])),
                )
                .expect("the player reports why it stopped");
            let window = s.window();
            let shown = format!("{:?}", window.tree());
            assert!(
                shown.contains("The stream stopped at 1:23"),
                "the notice names where it stopped: {shown}"
            );
            let banner = of_kind(window.tree(), "ui.Banner")
                .into_iter()
                .next()
                .expect("the notice offers to continue there");
            let action = match banner.props.get("action") {
                Some(Value::Text(label)) => Some(label.as_str()),
                Some(Value::Opt(Some(label))) => label.as_text(),
                _ => None,
            };
            assert_eq!(action, Some("Try again"), "and one press continues it");
            // The press asks the catalogue for a fresh address, and the session stands where the
            // stream stopped.
            let mut outcome = window
                .fire_with(&banner.key, "act", None)
                .expect("the notice's press runs");
            while let InvokeOutcome::Waits { run, .. } = outcome {
                outcome = match s.dispatch.allow(&s.rt, &run) {
                    RunOutcome::Done(value) => InvokeOutcome::Done(value),
                    RunOutcome::Waits { run, call } => InvokeOutcome::Waits { run, call },
                };
            }
            assert_eq!(
                s.asked_for("music.stream"),
                vec![serde_json::json!("k-Blue Hour")],
                "the catalogue is asked where the song's bytes are now"
            );
            assert!(
                !format!("{:?}", s.window().tree()).contains("The stream stopped"),
                "the notice goes once the person answered it"
            );
            // What the catalogue answered is what plays, from where the stream stopped.
            s.write_field(
                &song,
                "Track",
                "stream",
                Value::text("https://music.example/blue-hour-fresh.wav"),
            );
            let window = s.window();
            let band = of_kind(window.tree(), "ui.MediaTransport")
                .into_iter()
                .next()
                .expect("the player is in the window");
            let source = match band.props.get("source") {
                Some(Value::Text(source)) => Some(source.as_str()),
                Some(Value::Opt(Some(source))) => source.as_text(),
                _ => None,
            };
            assert_eq!(
                source,
                Some("https://music.example/blue-hour-fresh.wav"),
                "the player opens the address the catalogue answered with"
            );
            let position = match band.props.get("position") {
                Some(Value::Float(position)) => Some(*position),
                Some(Value::Opt(Some(position))) => match &**position {
                    Value::Float(seconds) => Some(*seconds),
                    _ => None,
                },
                _ => None,
            };
            assert_eq!(position, Some(83.0), "and plays on from where it stopped");
        }
    }
}

/// What the app asks of Spotify and what it shows once Spotify answered: the person's likes,
/// saved albums and followed artists reach their Spotify library, the playlists they make are
/// Spotify's, Spotify's queue is visible, an ended sign-in says so, and a kind of music the
/// library listens to brings artists who play it.
#[cfg(test)]
mod spotify_cases {
    use crate::common::*;
    use tilolang_runtime::{InvokeOutcome, ResourceRef, RunOutcome, Runtime, Value, ViewInstance};

    const SOURCE: &str = include_str!("../../../../apps/tl/music/app.tl");
    const PLAYBACK: &str = include_str!("../../../../apps/tl/playback/app.tl");

    #[derive(Clone, Copy, Default)]
    struct Trusting;

    impl tilolang_runtime::Authority for Trusting {
        fn decide(
            &self,
            _invoker: &tilolang_runtime::Actor,
            _chain: &[tilolang_runtime::ids::ModuleHash],
            _action: &tilolang_runtime::ActionInfo,
        ) -> tilolang_runtime::Decision {
            tilolang_runtime::Decision::Allow
        }
    }

    struct Shelf {
        rt: Runtime,
        app: App,
        lib: ResourceRef,
        dispatch: Dispatch,
    }

    impl Shelf {
        fn new() -> Self {
            let rt = Runtime::new(tilolang_runtime::AmStore::new(), Trusting).unwrap();
            let dispatch = Dispatch::new();
            rt.set_foreign_invoker(dispatch.clone());
            let playback = App::compile(PLAYBACK, &[]);
            playback.load(&rt);
            let app = App::compile(SOURCE, &[playback.meta().clone()]);
            app.load(&rt);
            let lib = rt.create_root("Library").unwrap();
            let shelf = Self {
                rt,
                app,
                lib,
                dispatch,
            };
            shelf.call("start", vec![Value::resource(&shelf.lib)]);
            shelf
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            let mut outcome = self
                .rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|why| panic!("{action}: {why}"));
            while let InvokeOutcome::Waits { run, .. } = outcome {
                outcome = match self.dispatch.allow(&self.rt, &run) {
                    RunOutcome::Done(value) => InvokeOutcome::Done(value),
                    RunOutcome::Waits { run, call } => InvokeOutcome::Waits { run, call },
                };
            }
            outcome
        }

        fn field_of(&self, at: &ResourceRef, ty: &str, field: &str) -> Value {
            self.rt.read(at, &field_id(&self.app, ty, field)).unwrap()
        }

        fn write_field(&self, at: &ResourceRef, ty: &str, field: &str, value: Value) {
            let write = tilolang_runtime::origin::OriginWrite::Set {
                target: at.clone(),
                fields: vec![(field_id(&self.app, ty, field), value)],
            };
            self.rt
                .write_origin("music.spotify", None, |change| change.write(vec![write]))
                .unwrap_or_else(|why| panic!("writing {ty}.{field}: {why}"));
        }

        fn write_library(&self, field: &str, value: Value) {
            self.write_field(&self.lib, "Library", field, value);
        }

        /// The library as a person who signed in to Spotify has it.
        fn connected(&self) {
            self.write_library("catalogue", Value::text("spotify"));
            self.write_library("spotify_state", Value::text("connected"));
        }

        fn collection(&self, field: &str) -> Vec<ResourceRef> {
            match self
                .rt
                .read(&self.lib, &field_id(&self.app, "Library", field))
                .unwrap()
            {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|item| match item {
                        Value::Ref(reference) => {
                            Some(std::rc::Rc::unwrap_or_clone(reference.target))
                        }
                        _ => None,
                    })
                    .collect(),
                other => panic!("Library.{field} is a list: {other:?}"),
            }
        }

        /// The payloads the app queued for `capability`, in order.
        fn asked_for(&self, capability: &str) -> Vec<serde_json::Value> {
            self.rt
                .pending_outbox()
                .into_iter()
                .filter(|entry| entry.capability == capability)
                .map(|entry| entry.payload)
                .collect()
        }

        /// A song the catalogue named, with the Spotify address it plays from.
        fn spotify_song(&self, title: &str, artist: &str, artist_key: &str) -> ResourceRef {
            let key = format!("spotify:track:{}", title.replace(' ', "-").to_lowercase());
            seed_origin(
                &self.rt,
                "music.track",
                &self.lib,
                &field_id(&self.app, "Library", "tracks"),
                &type_id(&self.app, "Track"),
                vec![
                    ("key", Value::text(&key)),
                    ("title", Value::text(title)),
                    ("artist", Value::text(artist)),
                    ("artist_key", Value::text(artist_key)),
                    ("album", Value::text("Morning")),
                    ("album_key", Value::text("spotify:album:album-1")),
                    ("seconds", Value::Int(214)),
                    ("stream", Value::text(&key)),
                ],
            )
        }

        fn genred_artist(&self, name: &str, key: &str, genres: Vec<&str>) -> ResourceRef {
            seed_origin(
                &self.rt,
                "music.artist",
                &self.lib,
                &field_id(&self.app, "Library", "artists"),
                &type_id(&self.app, "Artist"),
                vec![
                    ("key", Value::text(key)),
                    ("name", Value::text(name)),
                    (
                        "genres",
                        Value::List(genres.into_iter().map(Value::text).collect()),
                    ),
                ],
            )
        }

        fn view(&self, name: &str) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(
                    &me(),
                    &self.app.view(name),
                    vec![Value::resource(&self.lib)],
                )
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            window
        }
    }

    tilo_test_env::scenario! {
        fn music_reaches_the_persons_spotify_library_and_shows_what_spotify_says;

        phase a_like_on_a_spotify_song_is_kept_in_spotifys_library_too;
        {
            let s = Shelf::new();
            s.connected();
            let song = s.spotify_song("First Light", "Ava", "spotify:artist:artist-1");
            s.call("like", vec![Value::resource(&song), Value::Bool(true)]);
            assert_eq!(
                s.field_of(&song, "Track", "liked"),
                Value::Bool(true),
                "the song shows the like at once"
            );
            assert!(
                s.asked_for("music.spotify").contains(&serde_json::json!([
                    "like",
                    "spotify:track:first-light",
                    "true"
                ])),
                "the like is asked of Spotify, which keeps the person's library"
            );
            // Taking it back is the same write, the other way.
            s.call("like", vec![Value::resource(&song), Value::Bool(false)]);
            assert!(
                s.asked_for("music.spotify").contains(&serde_json::json!([
                    "like",
                    "spotify:track:first-light",
                    "false"
                ]))
            );
            // An album kept and an artist followed are their Spotify library's too.
            let album = seed_origin(
                &s.rt,
                "music.album",
                &s.lib,
                &field_id(&s.app, "Library", "albums"),
                &type_id(&s.app, "Album"),
                vec![
                    ("key", Value::text("spotify:album:album-1")),
                    ("title", Value::text("Morning")),
                    ("artist", Value::text("Ava")),
                    ("artist_key", Value::text("spotify:artist:artist-1")),
                ],
            );
            s.call("save_album", vec![Value::resource(&album), Value::Bool(true)]);
            assert_eq!(s.field_of(&album, "Album", "in_library"), Value::Bool(true));
            assert!(
                s.asked_for("music.spotify").contains(&serde_json::json!([
                    "save_album",
                    "spotify:album:album-1",
                    "true"
                ]))
            );
            let ava = s.genred_artist("Ava", "spotify:artist:artist-1", vec!["dream pop"]);
            s.call("follow_artist", vec![Value::resource(&ava), Value::Bool(true)]);
            assert_eq!(s.field_of(&ava, "Artist", "followed"), Value::Bool(true));
            assert!(
                s.asked_for("music.spotify").contains(&serde_json::json!([
                    "follow",
                    "spotify:artist:artist-1",
                    "true"
                ]))
            );
        }

        phase a_mixtape_made_here_is_a_spotify_playlist_the_person_edits_there;
        {
            let s = Shelf::new();
            s.connected();
            s.call(
                "new_mixtape",
                vec![Value::resource(&s.lib), Value::text("Roads")],
            );
            let mixtapes = s.collection("mixtapes");
            let mixtape = mixtapes.first().expect("the playlist the person made").clone();
            let created = s
                .asked_for("music.spotify")
                .into_iter()
                .find(|payload| payload[0] == "create_playlist")
                .expect("Spotify is asked for the playlist");
            assert_eq!(created[1], "Roads", "the playlist is called what they typed");
            assert_eq!(
                created[2].as_str(),
                Some(mixtape.id_string().as_str()),
                "the answer names the playlist here that Spotify's key goes to"
            );
            // What Spotify answered: the playlist it made.
            s.write_field(&mixtape, "Mixtape", "spotify", Value::text("spotify:playlist:pl-1"));
            let song = s.spotify_song("Blue Hour", "Ava", "spotify:artist:artist-1");
            s.call("mixtape_add", vec![Value::resource(&mixtape), Value::resource(&song)]);
            assert!(
                s.asked_for("music.spotify").contains(&serde_json::json!([
                    "playlist_add",
                    "spotify:playlist:pl-1",
                    "spotify:track:blue-hour"
                ])),
                "a song added here is put in the Spotify playlist"
            );
            s.call(
                "rename_mixtape",
                vec![Value::resource(&mixtape), Value::text("Roads at night")],
            );
            assert_eq!(
                s.field_of(&mixtape, "Mixtape", "name"),
                Value::text("Roads at night")
            );
            assert!(
                s.asked_for("music.spotify").contains(&serde_json::json!([
                    "edit_playlist",
                    "spotify:playlist:pl-1",
                    "Roads at night",
                    ""
                ])),
                "the new name is Spotify's too"
            );
            // What the playlist says about itself is the person's too, and Spotify's.
            s.call(
                "describe_mixtape",
                vec![
                    Value::resource(&mixtape),
                    Value::text("Songs for the drive home"),
                ],
            );
            assert_eq!(
                s.field_of(&mixtape, "Mixtape", "description"),
                Value::text("Songs for the drive home")
            );
            assert!(
                s.asked_for("music.spotify").contains(&serde_json::json!([
                    "edit_playlist",
                    "spotify:playlist:pl-1",
                    "Roads at night",
                    "Songs for the drive home"
                ])),
                "the description reaches the Spotify playlist"
            );
            s.call("mixtape_remove", vec![Value::resource(&mixtape), Value::resource(&song)]);
            assert!(
                s.asked_for("music.spotify").contains(&serde_json::json!([
                    "playlist_remove",
                    "spotify:playlist:pl-1",
                    "spotify:track:blue-hour"
                ]))
            );
        }


        phase an_ended_sign_in_says_so_and_offers_to_connect_again;
        {
            let s = Shelf::new();
            s.connected();
            s.write_library("spotify_state", Value::text("expired"));
            let shown = format!("{:?}", s.view("taste_page").tree());
            assert!(shown.contains("Spotify ended the sign-in"), "the reason is shown");
            assert!(shown.contains("Reconnect Spotify"), "and the button that answers it");
        }

        phase spotifys_queue_and_a_page_of_it_are_what_the_person_reads;
        {
            let s = Shelf::new();
            s.connected();
            seed_origin(
                &s.rt,
                "music.spotify_queue",
                &s.lib,
                &field_id(&s.app, "Library", "spotify_queue"),
                &type_id(&s.app, "SpotifyQueueItem"),
                vec![
                    ("key", Value::text("spotify:track:blue-hour")),
                    ("title", Value::text("Blue Hour")),
                    ("artist", Value::text("Ava")),
                ],
            );
            s.write_library("spotify_next_tracks", Value::Int(50));
            let shown = format!("{:?}", s.view("taste_page").tree());
            assert!(shown.contains("Blue Hour"), "what plays next there is visible");
            let home = format!("{:?}", s.view("home_page").tree());
            assert!(
                home.contains("More liked songs"),
                "a page Spotify said there is more of can be asked for"
            );
            s.call("spotify_more", vec![Value::resource(&s.lib), Value::text("tracks")]);
            assert!(
                s.asked_for("music.spotify").contains(&serde_json::json!([
                    "saved_page",
                    "tracks",
                    "50"
                ]))
            );
        }

        phase a_kind_of_music_the_library_listens_to_brings_artists_who_play_it;
        {
            let s = Shelf::new();
            let ava = s.genred_artist("Ava", "ava", vec!["dream pop"]);
            s.genred_artist("Kai", "kai", vec!["dream pop", "ambient"]);
            let first = seed_origin(
                &s.rt,
                "music.track",
                &s.lib,
                &field_id(&s.app, "Library", "tracks"),
                &type_id(&s.app, "Track"),
                vec![
                    ("key", Value::text("k-first")),
                    ("title", Value::text("First Light")),
                    ("artist", Value::text("Ava")),
                    ("artist_key", Value::text("ava")),
                    ("album", Value::text("Morning")),
                    ("album_key", Value::text("a-morning")),
                    ("stream", Value::text("https://music.example/first-light.wav")),
                ],
            );
            s.call("like", vec![Value::resource(&first), Value::Bool(true)]);
            s.genred_artist("Nothing", "nobody", vec!["polka"]);
            seed_origin(
                &s.rt,
                "music.track",
                &s.lib,
                &field_id(&s.app, "Library", "tracks"),
                &type_id(&s.app, "Track"),
                vec![
                    ("key", Value::text("k-night")),
                    ("title", Value::text("Night Bus")),
                    ("artist", Value::text("Kai")),
                    ("artist_key", Value::text("kai")),
                    ("album", Value::text("Night")),
                    ("album_key", Value::text("a-night")),
                    ("stream", Value::text("https://music.example/night-bus.wav")),
                ],
            );
            let home = s.view("home_page");
            let tree = home.tree();
            let row = of_kind(tree, "ui.ListRow")
                .into_iter()
                .find(|row| row.props.get("title").and_then(Value::as_text) == Some("Night Bus"))
                .expect("the song by an artist of the same kind is suggested");
            assert_eq!(
                row.props.get("secondary").and_then(Value::as_text),
                Some("Because you listen to dream pop.")
            );
            let mut page = s
                .rt
                .open_view(&me(), &s.app.view("artist"), vec![Value::resource(&ava)])
                .unwrap();
            page.set_lazy_rows(false).unwrap();
            let tree = page.tree();
            let sections: Vec<String> = of_kind(tree, "ui.Section")
                .into_iter()
                .filter_map(|section| section.props.get("title").and_then(Value::as_text))
                .map(str::to_owned)
                .collect();
            assert!(
                sections.contains(&"Artists like them".to_owned()),
                "the artist's page names the artists like them: {sections:?}"
            );
            let kindred: Vec<String> = of_kind(tree, "ui.MediaCard")
                .into_iter()
                .filter_map(|card| card.props.get("title").and_then(Value::as_text))
                .map(str::to_owned)
                .collect();
            assert!(kindred.contains(&"Kai".to_owned()), "and names them: {kindred:?}");
        }
    }
}
