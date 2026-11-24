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
                    ("key", Value::text(&format!("k-{title}"))),
                    ("title", Value::text(title)),
                    ("artist", Value::text(artist)),
                    ("artist_key", Value::text(artist_key)),
                    ("album", Value::text(album)),
                    ("album_key", Value::text(&format!("a-{album}"))),
                    ("seconds", Value::Int(200)),
                    (
                        "stream",
                        Value::text(&format!("https://music.example/{title}.wav")),
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
                Value::text(&blue.id_string())
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
    }
}
