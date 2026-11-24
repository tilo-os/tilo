//! The YouTube app (`apps/tl/tube/app.tl`) end to end: the settings place that holds each
//! playback preference, a comments switch its video page obeys, and the speed the person chose
//! following them to the next video.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::sync::Arc;

    use crate::common::*;
    use tilolang_grayjay::testing::Peer;
    use tilolang_runtime::host_work::{EngineHost, JobHandlers};
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

        fn comment(&self, key: &str, video_key: &str, text: &str) -> ResourceRef {
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
            )
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

        phase a_comment_is_liked_and_the_heart_says_so;
        {
            let w = Watch::new();
            w.channel("mkbhd", "MKBHD");
            let video = w.video("v1", "A week on one charge", "mkbhd", "MKBHD");
            let comment = w.comment("c1", "v1", "The ferry was late");
            w.call("like_comment", vec![Value::resource(&w.yt), Value::resource(&comment)]);
            let liked = match w.rt.read(&w.yt, "liked_comments").unwrap() {
                Value::List(items) => items,
                other => panic!("liked_comments reads {other:?}"),
            };
            assert_eq!(liked, vec![Value::text("c1")], "the person liked it");
            // The page's heart is on for the comment the person liked.
            let page = w.open("video_page", vec![Value::resource(&video)]);
            let heart = of_kind(page.tree(), "ui.Button")
                .into_iter()
                .find(|b| b.props.get("icon") == Some(&Value::text("heart")))
                .expect("the comment offers a heart");
            assert_eq!(heart.props.get("toggled"), Some(&Value::Int(1)));
            // Pressing it again takes the like back.
            w.call(
                "like_comment",
                vec![Value::resource(&w.yt), Value::resource(&comment), Value::Bool(false)],
            );
            let liked = match w.rt.read(&w.yt, "liked_comments").unwrap() {
                Value::List(items) => items,
                other => panic!("liked_comments reads {other:?}"),
            };
            assert!(liked.is_empty(), "the like is taken back");
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

    /// This device's Grayjay bridge: the app's `engine` calls, answered over a key the test
    /// holds, so what it reads and writes stays in this process. It keeps the actions each job
    /// runs when it ends, as the device does.
    #[derive(Clone)]
    struct Bridge {
        engine: tilolang_grayjay::Engine,
        handlers: Rc<RefCell<HashMap<String, JobHandlers>>>,
        /// The record of each job the bridge told ended, as the device keeps it.
        ended: Arc<(std::sync::Mutex<HashMap<String, serde_json::Value>>, std::sync::Condvar)>,
    }

    impl Bridge {
        /// The job's record as the device reads it, while it runs or once it ended.
        fn record(&self, job: &str) -> Option<serde_json::Value> {
            if let Some(ended) = self.ended.0.lock().unwrap().get(job) {
                return Some(ended.clone());
            }
            let said = self.engine.status(job).ok()?;
            tilolang_tilo::engine::record_of(job, &said)
        }

        /// The record of `job` once the bridge tells it ended.
        fn ended(&self, job: &str) -> serde_json::Value {
            let (held, told) = &*self.ended;
            let (held, waited) = told
                .wait_timeout_while(held.lock().unwrap(), std::time::Duration::from_secs(6), |held| {
                    !held.contains_key(job)
                })
                .unwrap();
            assert!(!waited.timed_out(), "the Grayjay job {job} did not end");
            held[job].clone()
        }
    }

    impl EngineHost for Bridge {
        fn call(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
            assert_eq!(app, "so.tilo.tube", "only YouTube asks this bridge");
            self.engine.call(op, input)
        }

        fn start(
            &self,
            app: &str,
            op: &str,
            input: &str,
            handlers: &JobHandlers,
        ) -> Result<String, String> {
            assert_eq!(app, "so.tilo.tube", "only YouTube asks this bridge");
            // The host names each job, as the device does.
            let job = format!("grayjay-{}", self.handlers.borrow().len() + 1);
            let (named, ended) = (job.clone(), Arc::clone(&self.ended));
            self.engine.start(
                &job,
                op,
                input,
                Arc::new(move |_, detail| {
                    let record = tilolang_tilo::engine::record_of(&named, &detail.to_string())
                        .expect("the bridge tells a job's end as an object");
                    ended.0.lock().unwrap().insert(named.clone(), record);
                    ended.1.notify_all();
                }),
            )?;
            self.handlers
                .borrow_mut()
                .insert(job.clone(), handlers.clone());
            Ok(job)
        }

        fn job(&self, job: &str) -> Result<Option<serde_json::Value>, String> {
            Ok(self.record(job))
        }

        fn cancel(&self, job: &str) -> bool {
            self.engine.cancel(job)
        }
    }

    /// The YouTube app over its Grayjay bridge, with a Grayjay device the test runs.
    struct Linked {
        rt: Runtime,
        app: App,
        yt: ResourceRef,
        dispatch: Dispatch,
        bridge: Bridge,
    }

    impl Linked {
        fn new() -> Self {
            let engine = tilolang_grayjay::Engine::with_key_source(Arc::new(
                tilolang_grayjay::FixedKey::new(vec![7_u8; 32]),
            ));
            let rt = Runtime::new(tilolang_runtime::AmStore::new(), TestAuthority::new()).unwrap();
            rt.set_clock(|| "2026-11-23T09:00:00Z".to_owned());
            let bridge = Bridge {
                engine,
                handlers: Rc::new(RefCell::new(HashMap::new())),
                ended: Arc::default(),
            };
            rt.set_engine(bridge.clone());
            let playback = App::compile(PLAYBACK, &[]);
            playback.load(&rt);
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
                bridge,
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|why| panic!("{action}: {why}"))
        }

        /// Waits for the job under way to end, then runs the action its start named for the
        /// end with the job's record, as the device does once the bridge's work is over.
        fn settle(&self) {
            loop {
                let job = self.text("sync_job");
                if job.is_empty() {
                    return;
                }
                let record = self.bridge.ended(&job);
                self.rt.set_job(&job, &record);
                let done = self.bridge.handlers.borrow()[&job]
                    .done
                    .clone()
                    .expect("the Grayjay job names what runs when it ends");
                let call = self.rt.job_call(&done, &record).unwrap();
                let outcome = self.rt.invoke_action(&me(), call).unwrap();
                assert!(
                    matches!(outcome, InvokeOutcome::Done(_)),
                    "the Grayjay job's end came to {outcome:?}"
                );
                assert_ne!(self.text("sync_job"), job, "the job's end lets it go");
            }
        }

        fn text(&self, field: &str) -> String {
            match self.rt.read(&self.yt, field).unwrap() {
                Value::Text(text) => text,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn devices(&self) -> Vec<ResourceRef> {
            match self.rt.read(&self.yt, "devices").unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|item| match item {
                        Value::Ref(reference) => Some(Rc::unwrap_or_clone(reference.target)),
                        _ => None,
                    })
                    .collect(),
                other => panic!("devices reads {other:?}"),
            }
        }

        fn device_field(&self, device: &ResourceRef, field: &str) -> Value {
            self.rt.read(device, field).unwrap()
        }

        /// The keys the app is subscribed to, as its own records hold them.
        fn subscribed_keys(&self) -> Vec<String> {
            match self.rt.read(&self.yt, "subscriptions").unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|item| match item {
                        Value::Ref(reference) => {
                            let held = Rc::unwrap_or_clone(reference.target);
                            match self.rt.read(&held, "key").unwrap() {
                                Value::Text(key) => Some(key),
                                _ => None,
                            }
                        }
                        _ => None,
                    })
                    .collect(),
                other => panic!("subscriptions reads {other:?}"),
            }
        }

        /// The video keys the app's history holds.
        fn watched_keys(&self) -> Vec<String> {
            match self.rt.read(&self.yt, "history").unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|item| match item {
                        Value::Ref(reference) => {
                            let held = Rc::unwrap_or_clone(reference.target);
                            match self.rt.read(&held, "key").unwrap() {
                                Value::Text(key) => Some(key),
                                _ => None,
                            }
                        }
                        _ => None,
                    })
                    .collect(),
                other => panic!("history reads {other:?}"),
            }
        }

        fn open(&self, view: &str, args: Vec<Value>) -> ViewInstance {
            let mut opened = self
                .rt
                .open_view(&me(), &self.app.view(view), args)
                .unwrap_or_else(|why| panic!("{view}: {why}"));
            opened.set_lazy_rows(false).unwrap();
            opened
        }

        fn texts(&self, view: &ViewInstance) -> Vec<String> {
            of_kind(view.tree(), "ui.Text")
                .into_iter()
                .map(|node| text_prop(node, "value"))
                .collect()
        }

        /// A channel the service answered with, which the app's subscriptions key.
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
    }

    /// A Grayjay device that holds one channel's subscription and one watched video.
    fn grayjay_peer() -> Peer {
        Peer::start(
            vec![tilolang_grayjay::SharedSubscription {
                url: "https://youtube.com/@alice".into(),
                name: "Alice".into(),
            }],
            vec!["https://youtube.com/watch?v=one".into()],
            Some("SECRET42".into()),
        )
    }

    tilo_test_env::scenario! {
        fn youtube_pairs_approves_revokes_and_syncs_with_grayjay;

        phase an_invite_records_a_device_waiting_for_approval;
        {
            let app = Linked::new();
            let peer = grayjay_peer();
            app.call(
                "pair_device",
                vec![Value::resource(&app.yt), Value::text(&peer.invite())],
            );
            app.settle();

            let devices = app.devices();
            assert_eq!(
                devices.len(),
                1,
                "the paired device is recorded; trouble {:?}, notice {:?}",
                app.text("sync_trouble"),
                app.text("sync_notice")
            );
            assert_eq!(
                app.device_field(&devices[0], "key"),
                Value::text(&peer.route().key),
                "the device is known by its key, the fingerprint Grayjay shows"
            );
            assert_eq!(app.device_field(&devices[0], "approved"), Value::Bool(false));
            assert_eq!(app.device_field(&devices[0], "revoked"), Value::Bool(false));
            assert!(
                app.text("sync_notice").contains("approve it"),
                "the person is asked to check the fingerprint: {}",
                app.text("sync_notice")
            );

            // The settings place shows the device with the fingerprint to check.
            let page = app.open("settings_page", vec![Value::resource(&app.yt)]);
            let sections: Vec<String> = of_kind(page.tree(), "ui.Section")
                .into_iter()
                .filter_map(|section| section.props.get("title").and_then(Value::as_text))
                .map(str::to_owned)
                .collect();
            assert!(sections.iter().any(|title| title == "Grayjay sync"), "{sections:?}");
            let rows: Vec<String> = of_kind(page.tree(), "ui.ListRow")
                .into_iter()
                .filter_map(|row| row.props.get("secondary").and_then(Value::as_text))
                .map(str::to_owned)
                .collect();
            assert!(
                rows.iter().any(|row| row.contains(&peer.route().key)),
                "the fingerprint is shown: {rows:?}"
            );
            let buttons: Vec<String> = of_kind(page.tree(), "ui.Button")
                .into_iter()
                .map(|button| text_prop(button, "label"))
                .collect();
            assert!(buttons.iter().any(|label| label == "Pair device"), "{buttons:?}");
        }

        phase an_approved_device_carries_subscriptions_both_ways_and_history_in;
        {
            let app = Linked::new();
            let peer = grayjay_peer();
            app.call(
                "pair_device",
                vec![Value::resource(&app.yt), Value::text(&peer.invite())],
            );
            app.settle();
            let device = app.devices().remove(0);
            app.call("approve_device", vec![Value::resource(&app.yt), Value::resource(&device)]);
            assert_eq!(app.device_field(&device, "approved"), Value::Bool(true));

            // The client follows one channel of its own, which the other app receives.
            let channel = app.channel("mkbhd", "MKBHD");
            app.call(
                "subscribe",
                vec![Value::resource(&channel), Value::Bool(true)],
            );

            app.call("sync_devices", vec![Value::resource(&app.yt)]);
            app.settle();

            assert!(
                app.subscribed_keys().iter().any(|key| key == "@alice"),
                "the channel Grayjay follows is subscribed here: {:?}; notice {:?}, trouble {:?}",
                app.subscribed_keys(),
                app.text("sync_notice"),
                app.text("sync_trouble"),
            );
            assert!(
                app.watched_keys().iter().any(|key| key == "one"),
                "the video Grayjay watched is in the history: {:?}",
                app.watched_keys()
            );
            assert_eq!(
                peer.pushed(),
                vec![vec![tilolang_grayjay::SharedSubscription {
                    url: "https://youtube.com/@mkbhd".into(),
                    name: "MKBHD".into(),
                }]],
                "the client's subscription went to the other app"
            );
            assert_eq!(app.device_field(&device, "last_error"), Value::text(""));
            assert_eq!(
                app.text("sync_notice"),
                "Synced: 1 new subscription and 1 new watched video came in."
            );

            // A second exchange finds nothing new and says so.
            app.call("sync_devices", vec![Value::resource(&app.yt)]);
            app.settle();
            assert_eq!(app.text("sync_notice"), "Grayjay and this client are in step.");
        }

        phase a_revoked_device_syncs_nothing;
        {
            let app = Linked::new();
            let peer = grayjay_peer();
            app.call(
                "pair_device",
                vec![Value::resource(&app.yt), Value::text(&peer.invite())],
            );
            app.settle();
            let device = app.devices().remove(0);
            app.call("approve_device", vec![Value::resource(&app.yt), Value::resource(&device)]);
            app.call(
                "revoke_device",
                vec![Value::resource(&app.yt), Value::resource(&device)],
            );
            assert_eq!(app.device_field(&device, "revoked"), Value::Bool(true));
            assert_eq!(app.device_field(&device, "approved"), Value::Bool(false));

            app.call("sync_devices", vec![Value::resource(&app.yt)]);
            assert_eq!(
                app.text("sync_trouble"),
                "Approve a Grayjay device first",
                "a revoked device is not exchanged with"
            );
            assert!(peer.pushed().is_empty(), "nothing was sent to the revoked device");

            // The settings place says the device is gone from the exchange.
            let page = app.open("settings_page", vec![Value::resource(&app.yt)]);
            let texts = app.texts(&page);
            assert!(
                texts.iter().any(|text| text == "No approved devices yet. Pairing needs a confirmation on both ends."),
                "{texts:?}"
            );
        }

        phase a_device_that_does_not_answer_says_why;
        {
            let app = Linked::new();
            let peer = grayjay_peer();
            app.call(
                "pair_device",
                vec![Value::resource(&app.yt), Value::text(&peer.invite())],
            );
            app.settle();
            let device = app.devices().remove(0);
            app.call("approve_device", vec![Value::resource(&app.yt), Value::resource(&device)]);

            // The device goes away before the exchange.
            drop(peer);
            app.call("sync_devices", vec![Value::resource(&app.yt)]);
            app.settle();

            let said = match app.device_field(&device, "last_error") {
                Value::Text(error) => error,
                other => panic!("last_error reads {other:?}"),
            };
            assert!(!said.is_empty(), "the device's failure is recorded");
            assert_eq!(app.text("sync_notice"), "1 of 1 devices did not exchange.");
        }

        phase a_subscription_from_another_site_is_not_taken;
        {
            let app = Linked::new();
            let peer = Peer::start(
                vec![
                    tilolang_grayjay::SharedSubscription {
                        url: "https://example.com/@spam".into(),
                        name: "Spam".into(),
                    },
                    tilolang_grayjay::SharedSubscription {
                        url: "https://youtube.com/@real".into(),
                        name: "Real".into(),
                    },
                ],
                vec![
                    "https://example.com/watch?v=no".into(),
                    "https://youtube.com/watch?v=yes".into(),
                ],
                None,
            );
            app.call(
                "pair_device",
                vec![Value::resource(&app.yt), Value::text(&peer.invite())],
            );
            app.settle();
            let device = app.devices().remove(0);
            app.call(
                "approve_device",
                vec![Value::resource(&app.yt), Value::resource(&device)],
            );
            app.call("sync_devices", vec![Value::resource(&app.yt)]);
            app.settle();

            assert_eq!(
                app.subscribed_keys(),
                vec!["@real".to_string()],
                "only a YouTube channel is subscribed"
            );
            assert_eq!(
                app.watched_keys(),
                vec!["yes".to_string()],
                "only a YouTube video is kept in the history"
            );
        }

        phase an_invite_that_does_not_read_says_why;
        {
            let app = Linked::new();
            app.call(
                "pair_device",
                vec![Value::resource(&app.yt), Value::text("not an invite")],
            );
            app.settle();
            assert!(
                app.text("sync_trouble").contains("Pairing failed"),
                "the refusal is the person's to read: {}",
                app.text("sync_trouble")
            );
            assert!(app.devices().is_empty(), "nothing was recorded");
        }
    }
}
