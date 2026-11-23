//! `page.call`: the page a window shows is asked once, its answer is a journal entry a replay
//! answers from, and a window that shows no page is a value the run carries on with.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    use serde_json::json;
    use tilolang_runtime::page::PageHost;
    use tilolang_runtime::run::JournalEntry;
    use tilolang_runtime::{AmStore, Runtime, SimpleAuthority, Value};

    const READER: &str = r#"app so.tilo.pagedemo "Page demo"

type Feed {
    text: Text
}

action look(f: Feed) uses page {
    f.text = page.call("{\"request\":\"outline\"}")
}
"#;

    /// What the window's page answers, and every request it was asked.
    #[derive(Clone, Default)]
    struct FakePage {
        requests: Rc<RefCell<Vec<String>>>,
        answer: Rc<RefCell<Option<String>>>,
    }

    impl FakePage {
        fn answering(answer: &str) -> Self {
            Self {
                requests: Rc::new(RefCell::new(Vec::new())),
                answer: Rc::new(RefCell::new(Some(answer.to_owned()))),
            }
        }

        fn requests(&self) -> Vec<String> {
            self.requests.borrow().clone()
        }
    }

    impl PageHost for FakePage {
        fn call(&self, request: &str) -> Result<String, String> {
            self.requests.borrow_mut().push(request.to_owned());
            Ok(self
                .answer
                .borrow()
                .clone()
                .unwrap_or_else(|| json!({"error": "the test gave no answer"}).to_string()))
        }
    }

    struct World {
        rt: Runtime,
        reader: App,
        feed: tilolang_runtime::ResourceRef,
    }

    fn world(page: Option<&FakePage>) -> World {
        let reader = App::compile(READER, &[]);
        let store = AmStore::new();
        let rt = Runtime::new(store.clone(), SimpleAuthority::new()).unwrap();
        if let Some(page) = page {
            rt.set_page(page.clone());
        }
        reader.load(&rt);
        let feed = rt.create("Feed").unwrap();
        World { rt, reader, feed }
    }

    fn feed(rt: &Runtime, feed: &tilolang_runtime::ResourceRef) -> String {
        match rt.read(feed, "text").unwrap() {
            Value::Text(text) => text,
            other => panic!("feed is {other:?}"),
        }
    }

    fn effects(
        rt: &Runtime,
        feed: &tilolang_runtime::ResourceRef,
    ) -> Vec<JournalEntry> {
        rt.history(&feed.doc)
            .unwrap()
            .last()
            .expect("the change the action made")
            .effects
            .clone()
    }

    tilo_test_env::scenario! {
        fn a_page_call_is_asked_once_and_kept_with_its_change;

        phase the_windows_answer_is_the_actions_value_and_its_journal_entry;
        {
            let page = FakePage::answering("{\"outline\":\"First section\"}");
            let world = world(Some(&page));
            world
                .rt
                .invoke_action(
                    &me(),
                    (
                        world.reader.action("look").as_str(),
                        vec![Value::resource(&world.feed)],
                    ),
                )
                .unwrap();

            assert_eq!(feed(&world.rt, &world.feed), "{\"outline\":\"First section\"}");
            let asked = page.requests();
            assert_eq!(asked, ["{\"request\":\"outline\"}"], "the page was asked once");
            assert_eq!(
                effects(&world.rt, &world.feed),
                [JournalEntry::Page {
                    request: "{\"request\":\"outline\"}".to_owned(),
                    answer: "{\"outline\":\"First section\"}".to_owned(),
                }],
                "the answer is recorded beside the change"
            );
        }

        phase a_window_that_shows_no_page_answers_why;
        {
            let world = world(None);
            world
                .rt
                .invoke_action(
                    &me(),
                    (
                        world.reader.action("look").as_str(),
                        vec![Value::resource(&world.feed)],
                    ),
                )
                .unwrap();
            let answer: serde_json::Value =
                serde_json::from_str(&feed(&world.rt, &world.feed)).unwrap();
            assert!(
                answer.get("error").is_some(),
                "the page call answers a refusal as a value: {answer}"
            );
        }
    }
}
