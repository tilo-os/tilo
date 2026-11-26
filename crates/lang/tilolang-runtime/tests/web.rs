//! `web.get` and `web.post`: a request runs once, on the device whose code makes it, its answer
//! is a journal entry a replay answers from, a host the package did not declare is refused as a
//! value, and a network failure is a value too.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::rc::Rc;

    use serde_json::json;

    use crate::common::*;
    use tilolang_runtime::run::{JournalEntry, RunOutcome, WebRequest};
    use tilolang_runtime::web::{WebReply, WebTransport};
    use tilolang_runtime::{AmStore, InvokeOutcome, ResourceRef, Runtime, Store, Value};

    const DEMO: &str = r#"app so.tilo.feeddemo "Feed demo"

type Board {
    marks: Int merge counter
    feed: Text
}

action mark(b: Board) {
    b.marks += 1
}
"#;

    const READER: &str = r#"app so.tilo.reader "Reader" (web: ["example.com"])

use so.tilo.feeddemo as demo

type Feed {
    text: Text
    status: Text
}

action fetch(f: Feed) uses web {
    let r = web.get("https://example.com/feed", headers: ["Accept: text/plain"], if_none_match: "v1")
    f.text = r.text()
}

action post_to(f: Feed) uses web {
    let r = web.post("https://example.com/send", body: "hi", headers: ["Content-Type: text/plain"])
    f.text = "{r.status} {r.status_text}"
}

action post_bytes_to(f: Feed) uses web {
    let r = web.post("https://example.com/upload", bytes: 4096)
    f.text = "{r.status} {r.status_text}"
}

action post_too_much(f: Feed) uses web {
    let r = web.post("https://example.com/upload", bytes: 100000000)
    f.text = "{r.status} {r.status_text}"
}

action timed(f: Feed) uses web {
    let r = web.get("https://example.com/feed")
    f.text = "{r.duration_ms}"
}

action ask_elsewhere(f: Feed) uses web {
    let r = web.get("https://evil.example/x")
    f.text = "{r.status} {r.status_text}"
}

action start(f: Feed, b: demo.Board) -> Text uses web {
    let r = web.get("https://example.com/feed")
    demo.mark(b)
    return r.text()
}
"#;

    /// What the invoking device's network answers, and every request it was asked to make.
    #[derive(Clone, Default)]
    struct FakeWeb {
        requests: Rc<RefCell<Vec<WebRequest>>>,
        reply: Rc<RefCell<Option<WebReply>>>,
        /// How long the device's network takes to answer, for a test that reads the time.
        delay: Rc<RefCell<Option<std::time::Duration>>>,
    }

    impl FakeWeb {
        fn answering(reply: WebReply) -> Self {
            Self {
                requests: Rc::new(RefCell::new(Vec::new())),
                reply: Rc::new(RefCell::new(Some(reply))),
                delay: Rc::new(RefCell::new(None)),
            }
        }

        fn after(delay: std::time::Duration, reply: WebReply) -> Self {
            let web = Self::answering(reply);
            *web.delay.borrow_mut() = Some(delay);
            web
        }

        fn requests(&self) -> Vec<WebRequest> {
            self.requests.borrow().clone()
        }
    }

    impl WebTransport for FakeWeb {
        fn fetch(&self, request: &WebRequest) -> WebReply {
            self.requests.borrow_mut().push(request.clone());
            if let Some(delay) = *self.delay.borrow() {
                std::thread::sleep(delay);
            }
            self.reply
                .borrow()
                .clone()
                .unwrap_or_else(|| WebReply::failed("the test gave no answer"))
        }
    }

    fn hello() -> WebReply {
        WebReply {
            status: 200,
            status_text: "OK".to_owned(),
            headers: vec!["ETag: \"v2\"".to_owned()],
            body: b"hello".to_vec(),
        }
    }

    struct World {
        rt: Runtime,
        store: AmStore,
        reader: App,
        feed: ResourceRef,
        board: ResourceRef,
        dispatch: Dispatch,
    }

    fn world(web: &FakeWeb) -> World {
        let demo = App::compile(DEMO, &[]);
        let reader = App::compile(READER, &[demo.meta().clone()]);
        let store = AmStore::new();
        let rt = Runtime::new(store.clone(), TestAuthority::new()).unwrap();
        rt.set_web(web.clone());
        let dispatch = Dispatch::new();
        rt.set_foreign_invoker(dispatch.clone());
        demo.load(&rt);
        reader.load(&rt);
        let feed = rt.create("Feed").unwrap();
        let board = rt.create("Board").unwrap();
        World {
            rt,
            store,
            reader,
            feed,
            board,
            dispatch,
        }
    }

    fn feed(rt: &Runtime, feed: &ResourceRef) -> String {
        match rt.read(feed, "text").unwrap() {
            Value::Text(text) => text,
            other => panic!("feed is {other:?}"),
        }
    }

    fn effects(rt: &Runtime, feed: &ResourceRef) -> Vec<JournalEntry> {
        rt.history(&feed.doc)
            .unwrap()
            .last()
            .expect("the change the action made")
            .effects
            .clone()
    }

    tilo_test_env::scenario! {
        fn a_web_request_is_declared_journaled_and_answered_as_a_value;

        phase an_actions_request_runs_once_and_is_kept_with_its_change;
        {
            let web = FakeWeb::answering(hello());
            let world = world(&web);
            world
                .rt
                .invoke_action(
                    &me(),
                    (
                        world.reader.action("fetch").as_str(),
                        vec![Value::resource(&world.feed)],
                    ),
                )
                .unwrap();

            assert_eq!(feed(&world.rt, &world.feed), "hello");
            let seen = web.requests();
            assert_eq!(seen.len(), 1, "the request ran once");
            assert_eq!(seen[0].method, "GET");
            assert_eq!(seen[0].url, "https://example.com/feed");
            assert_eq!(seen[0].headers, ["Accept: text/plain"]);
            assert_eq!(seen[0].if_none_match.as_deref(), Some("v1"));
            let recorded = effects(&world.rt, &world.feed);
            let [JournalEntry::Web { request, response }] = recorded.as_slice() else {
                panic!("the answer is recorded beside the change: {recorded:?}");
            };
            assert_eq!(request, &seen[0]);
            assert_eq!(response.status, 200);
            assert_eq!(response.status_text, "OK");
            assert_eq!(response.headers, ["ETag: \"v2\""]);
            assert_eq!(response.body, Some(json!("hello")));
        }

        phase a_run_resumed_answers_from_its_journal;
        {
            let web = FakeWeb::answering(hello());
            let mut world = world(&web);
            let out = world
                .rt
                .invoke_action(
                    &me(),
                    (
                        world.reader.action("start").as_str(),
                        vec![Value::resource(&world.feed), Value::resource(&world.board)],
                    ),
                )
                .unwrap();
            let InvokeOutcome::Waits { run, .. } = out else {
                panic!("the other package's action must ask: {out:?}");
            };
            assert_eq!(
                web.requests().len(),
                1,
                "the request ran before the run suspended"
            );
            let journal = world
                .store
                .load_journal(&run)
                .unwrap()
                .expect("a suspended run's journal");
            assert!(
                matches!(journal.entries.first(), Some(JournalEntry::Web { .. })),
                "the answer is in the journal: {:?}",
                journal.entries
            );

            let out = world.dispatch.allow(&world.rt, &run);
            assert_eq!(out, RunOutcome::Done(Value::text("hello")));
            assert_eq!(
                web.requests().len(),
                1,
                "the resumed run answers from its journal, it does not ask again"
            );
        }

        phase a_host_the_package_did_not_declare_is_refused_as_a_value;
        {
            let web = FakeWeb::answering(hello());
            let world = world(&web);
            world
                .rt
                .invoke_action(
                    &me(),
                    (
                        world.reader.action("ask_elsewhere").as_str(),
                        vec![Value::resource(&world.feed)],
                    ),
                )
                .unwrap();

            assert!(web.requests().is_empty(), "no request left the device");
            let why = feed(&world.rt, &world.feed);
            assert!(
                why.contains("may not reach") && why.contains("evil.example"),
                "the value says why: {why}"
            );
            let recorded = effects(&world.rt, &world.feed);
            assert!(
                matches!(recorded.as_slice(), [JournalEntry::Web { response, .. }] if response.status == 0),
                "the refusal is recorded too: {recorded:?}"
            );
        }

        phase a_post_sends_its_body_and_a_failure_is_a_value;
        {
            let web = FakeWeb::answering(WebReply::failed("no answer within 30 seconds"));
            let world = world(&web);
            world
                .rt
                .invoke_action(
                    &me(),
                    (
                        world.reader.action("post_to").as_str(),
                        vec![Value::resource(&world.feed)],
                    ),
                )
                .unwrap();

            let seen = web.requests();
            assert_eq!(seen.len(), 1);
            assert_eq!(seen[0].method, "POST");
            assert_eq!(seen[0].body.as_deref(), Some("hi"));
            assert_eq!(seen[0].headers, ["Content-Type: text/plain"]);
            assert_eq!(
                feed(&world.rt, &world.feed),
                "0 no answer within 30 seconds",
                "a timeout is the value the action reads, not a trap"
            );
        }

        phase a_body_of_zero_bytes_is_sent_as_its_size;
        {
            let web = FakeWeb::answering(hello());
            let world = world(&web);
            world
                .rt
                .invoke_action(
                    &me(),
                    (
                        world.reader.action("post_bytes_to").as_str(),
                        vec![Value::resource(&world.feed)],
                    ),
                )
                .unwrap();
            let seen = web.requests();
            assert_eq!(seen.len(), 1);
            assert_eq!(seen[0].method, "POST");
            assert_eq!(seen[0].body, None, "the body is no text");
            assert_eq!(seen[0].bytes, Some(4096));
            assert_eq!(
                seen[0].body_bytes(),
                Some(vec![0; 4096]),
                "the network client sends the zeroes"
            );
            assert_eq!(feed(&world.rt, &world.feed), "200 OK");
        }

        phase a_body_larger_than_a_request_may_send_is_refused_as_a_value;
        {
            let web = FakeWeb::answering(hello());
            let world = world(&web);
            world
                .rt
                .invoke_action(
                    &me(),
                    (
                        world.reader.action("post_too_much").as_str(),
                        vec![Value::resource(&world.feed)],
                    ),
                )
                .unwrap();
            assert!(web.requests().is_empty(), "nothing was sent");
            assert!(
                feed(&world.rt, &world.feed).contains("larger than"),
                "the value says why: {}",
                feed(&world.rt, &world.feed)
            );
        }

        phase a_reply_carries_how_long_the_request_took;
        {
            let web = FakeWeb::after(
                std::time::Duration::from_millis(20),
                WebReply {
                    status: 200,
                    status_text: "OK".to_owned(),
                    headers: Vec::new(),
                    body: b"hello".to_vec(),
                },
            );
            let world = world(&web);
            world
                .rt
                .invoke_action(
                    &me(),
                    (
                        world.reader.action("timed").as_str(),
                        vec![Value::resource(&world.feed)],
                    ),
                )
                .unwrap();
            let took: u64 = feed(&world.rt, &world.feed)
                .parse()
                .expect("the time reads as milliseconds");
            assert!(took >= 20, "the request took {took} ms");
            let recorded = effects(&world.rt, &world.feed);
            let [JournalEntry::Web { response, .. }] = recorded.as_slice() else {
                panic!("the answer is recorded");
            };
            assert_eq!(response.duration_ms, Some(took), "the journal keeps the time it took");
        }
    }
}
