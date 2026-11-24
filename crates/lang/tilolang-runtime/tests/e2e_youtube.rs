//! The YouTube app (`apps/tl/tube/app.tl`) end to end: the settings place that holds each
//! playback preference, a comments switch its video page obeys, and the speed the person chose
//! following them to the next video.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{InvokeOutcome, ResourceRef, Runtime, Value, ViewInstance};

    const SOURCE: &str = include_str!("../../../../apps/tl/tube/app.tl");
    /// The session the app plays through, so its `use` resolves.
    const PLAYBACK: &str = include_str!("../../../../apps/tl/playback/app.tl");
    struct Watch {
        rt: Runtime,
        app: App,
        yt: ResourceRef,
        dispatch: Dispatch,
    }

    impl Watch {
        fn new() -> Self {
            let rt = Runtime::new(tilolang_runtime::AmStore::new(), TestAuthority::new()).unwrap();
            rt.set_clock(|| "2026-11-23T09:00:00Z".to_owned());
            let playback = App::compile(PLAYBACK, &[]);
            playback.load(&rt);
            // The session is the platform's own: the app names what it opened lately on it.
            let session =
                tilolang_tilo::platform_apps::session_package().expect("the session's package");
            rt.load_foreign(session.clone()).expect("the session loads");
            let app = App::compile(SOURCE, &[playback.meta().clone(), session]);
            app.load(&rt);
            let dispatch = Dispatch::new();
            rt.set_foreign_invoker(dispatch.clone());
            let yt = rt.create_root("YouTube").unwrap();
            Self {
                rt,
                app,
                yt,
                dispatch,
            }
        }

        /// Runs `action` as the person does where the workspace asks them first: the window shows
        /// what it would do, they say yes, and the session delivers the call admitted. Answers
        /// what the action came to.
        fn call_and_approve(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            use tilolang_runtime::delegate::Invoked;
            let mut outcome = self.call(action, args);
            let mut asked = false;
            loop {
                match outcome {
                    InvokeOutcome::Asked(_) => {
                        asked = true;
                        let (call, values) = self.dispatch.asked();
                        outcome = self
                            .rt
                            .invoke_admitted(&me(), (call.as_str(), values), None)
                            .unwrap_or_else(|why| panic!("{action}, the person said yes: {why}"));
                    }
                    InvokeOutcome::Waits { run, .. } => {
                        asked = true;
                        let (call, values) = self.dispatch.asked();
                        let answer = match self
                            .rt
                            .invoke_admitted(&me(), (call.as_str(), values), None)
                            .unwrap_or_else(|why| panic!("{action}, the person said yes: {why}"))
                        {
                            InvokeOutcome::Done(value) => Invoked::Done(value),
                            other => Invoked::Failed(format!("{other:?}")),
                        };
                        outcome = self
                            .rt
                            .answer_call(&run, answer)
                            .unwrap_or_else(|why| panic!("{action}'s run takes the answer: {why}"))
                            .into();
                    }
                    InvokeOutcome::Done(_) => {
                        assert!(asked, "{action} did not ask the person first");
                        return outcome;
                    }
                    other => panic!("{action}, the person said yes, came to {other:?}"),
                }
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> tilolang_runtime::InvokeOutcome {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|why| panic!("{action}: {why}"))
        }

        fn channel(&self, key: &str, name: &str) -> ResourceRef {
            seed_origin(
                &self.rt,
                "youtube.channel",
                &self.yt,
                &field_id(&self.app, "YouTube", "channels"),
                &type_id(&self.app, "Channel"),
                vec![
                    ("key", Value::text(key)),
                    ("name", Value::text(name)),
                    ("handle", Value::text(&format!("@{key}"))),
                    ("subscribers", Value::Int(1000)),
                ],
            )
        }

        fn video(&self, key: &str, title: &str, channel_key: &str, name: &str) -> ResourceRef {
            seed_origin(
                &self.rt,
                "youtube.video",
                &self.yt,
                &field_id(&self.app, "YouTube", "videos"),
                &type_id(&self.app, "Video"),
                vec![
                    ("key", Value::text(key)),
                    ("title", Value::text(title)),
                    ("channel_key", Value::text(channel_key)),
                    ("channel_name", Value::text(name)),
                    ("duration", Value::Int(600)),
                    ("published", Value::text("2026-11-01T12:00:00Z")),
                    (
                        "stream",
                        Value::text(&format!("https://video.example/{key}.mp4")),
                    ),
                ],
            )
        }

        fn comment(&self, key: &str, video_key: &str, text: &str) {
            seed_origin(
                &self.rt,
                "youtube.comment",
                &self.yt,
                &field_id(&self.app, "YouTube", "comments"),
                &type_id(&self.app, "Comment"),
                vec![
                    ("key", Value::text(key)),
                    ("video_key", Value::text(video_key)),
                    ("author", Value::text("Nils")),
                    ("text", Value::text(text)),
                    ("likes", Value::Int(3)),
                    ("published", Value::text("2026-11-02T08:00:00Z")),
                ],
            );
        }

        fn open(&self, view: &str, args: Vec<Value>) -> ViewInstance {
            let mut opened = self
                .rt
                .open_view(&me(), &self.app.view(view), args)
                .unwrap_or_else(|why| panic!("{view}: {why}"));
            opened.set_lazy_rows(false).unwrap();
            opened
        }

        /// Every text the window shows.
        fn texts(&self, view: &ViewInstance) -> Vec<String> {
            of_kind(view.tree(), "ui.Text")
                .into_iter()
                .map(|node| text_prop(node, "value"))
                .collect()
        }

        /// The playback preferences the person has, as the app keeps them.
        fn preference(&self, field: &str) -> Value {
            let held = match self.rt.read(&self.yt, "playback").unwrap() {
                Value::List(items) => items,
                other => panic!("playback reads {other:?}"),
            };
            let mine = held
                .into_iter()
                .filter_map(|item| match item {
                    Value::Ref(r) => Some(std::rc::Rc::unwrap_or_clone(r.target)),
                    _ => None,
                })
                .find(|prefs| self.rt.read(prefs, "member").unwrap() == Value::Actor(me()))
                .expect("the person's own preferences");
            self.rt.read(&mine, field).unwrap()
        }
    }

    tilo_test_env::scenario! {
        fn youtube_keeps_how_each_person_watches;

        phase the_settings_place_holds_every_playback_preference;
        {
            let w = Watch::new();
            let page = w.open("settings_page", vec![Value::resource(&w.yt)]);
            let tree = page.tree();
            let sections: Vec<String> = of_kind(tree, "ui.Section")
                .into_iter()
                .filter_map(|s| s.props.get("title").and_then(Value::as_text))
                .map(str::to_owned)
                .collect();
            assert!(sections.iter().any(|t| t == "Playback"), "{sections:?}");
            assert!(sections.iter().any(|t| t == "Community marks"), "{sections:?}");
            let labels: Vec<String> = of_kind(tree, "ui.Toggle")
                .into_iter()
                .map(|t| text_prop(t, "label"))
                .collect();
            for said in [
                "Play the next video on its own",
                "Show comments",
                "Show dislike counts",
                "Skip segments SponsorBlock marks",
                "Community titles and pictures (DeArrow)",
            ] {
                assert!(labels.iter().any(|l| l == said), "{said} is not offered: {labels:?}");
            }
            let buttons: Vec<String> = of_kind(tree, "ui.Button")
                .into_iter()
                .map(|b| text_prop(b, "label"))
                .collect();
            assert!(buttons.iter().any(|b| b == "1.5×"), "a speed is offered: {buttons:?}");
            assert!(buttons.iter().any(|b| b == "1080p"), "a quality is offered: {buttons:?}");
        }

        phase a_comments_switch_decides_what_the_video_page_shows;
        {
            let w = Watch::new();
            w.channel("mkbhd", "MKBHD");
            let video = w.video("v1", "A week on one charge", "mkbhd", "MKBHD");
            w.comment("c1", "v1", "The ferry was late");
            let page = w.open("video_page", vec![Value::resource(&video)]);
            assert!(
                w.texts(&page).iter().any(|t| t == "The ferry was late"),
                "the comment is shown while comments are on"
            );
            w.call("set_comments", vec![Value::resource(&w.yt), Value::Bool(false)]);
            let page = w.open("video_page", vec![Value::resource(&video)]);
            let texts = w.texts(&page);
            assert!(
                !texts.iter().any(|t| t == "The ferry was late"),
                "a page with comments off shows none: {texts:?}"
            );
            assert!(
                texts.iter().any(|t| t == "Comments are turned off in Settings."),
                "and says where they are turned off: {texts:?}"
            );
            w.call("set_comments", vec![Value::resource(&w.yt), Value::Bool(true)]);
            let page = w.open("video_page", vec![Value::resource(&video)]);
            assert!(w.texts(&page).iter().any(|t| t == "The ferry was late"), "and back");
        }

        phase the_speed_chosen_is_the_player_s_speed;
        {
            let w = Watch::new();
            w.channel("veritasium", "Veritasium");
            let video = w.video("v2", "Why the sky is blue", "veritasium", "Veritasium");
            w.call("set_speed", vec![Value::resource(&w.yt), Value::Float(1.5)]);
            assert_eq!(w.preference("speed"), Value::Float(1.5));
            // The video plays through the session, and the page's player starts at the speed
            // the person chose rather than at the window's default.
            // Playing reaches the device's player, so the workspace asks the person first.
            w.call_and_approve("play", vec![Value::resource(&video)]);
            let page = w.open("video_page", vec![Value::resource(&video)]);
            let player = of_kind(page.tree(), "ui.VideoPlayer")
                .into_iter()
                .next()
                .expect("the page plays the video");
            assert_eq!(
                player.props.get("rate"),
                Some(&Value::Float(1.5)),
                "the player plays at the speed kept with the person"
            );
        }
    }
}
