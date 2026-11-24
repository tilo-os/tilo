//! The browser app (`apps/tl/browser/app.tl`) end to end: what a page downloads and what the
//! person does about it, a whole form answered in one ask, a saved address filled into a form a
//! site asked, the addresses the settings edit, and a sign-in turned down.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use serde_json::{Value as Json, json};
    use std::cell::RefCell;
    use std::rc::Rc;
    use tilolang_runtime::page::PageHost;
    use tilolang_runtime::{AmStore, InvokeOutcome, ResourceRef, Runtime, Value, ViewInstance};

    const SOURCE: &str = include_str!("../../../../apps/tl/browser/app.tl");

    /// The page a window holds, as the test plays it: every ask it was given, and what it last
    /// reported of itself.
    #[derive(Clone, Default)]
    struct Page {
        asked: Rc<RefCell<Vec<Json>>>,
        asks: Rc<RefCell<Json>>,
    }

    impl Page {
        fn ops(&self) -> Vec<String> {
            self.asked
                .borrow()
                .iter()
                .filter_map(|ask| ask.get("op").and_then(Json::as_str).map(str::to_owned))
                .collect()
        }

        fn last(&self) -> Json {
            self.asked.borrow().last().cloned().expect("an ask")
        }

        fn answer_asks(&self, asks: Json) {
            *self.asks.borrow_mut() = asks;
        }
    }

    impl PageHost for Page {
        fn call(&self, request: &str) -> Result<String, String> {
            let ask: Json = serde_json::from_str(request).unwrap_or(Json::Null);
            let op = ask
                .get("op")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_owned();
            self.asked.borrow_mut().push(ask);
            if op == "asks" {
                return Ok(self.asks.borrow().to_string());
            }
            Ok(json!({"ok": true}).to_string())
        }
    }

    struct Browser {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        page: Page,
        opened: ResourceRef,
    }

    impl Browser {
        fn new() -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let page = Page::default();
            rt.set_page(page.clone());
            rt.set_clock(|| "2026-11-24T09:00:00Z".to_owned());
            let app = App::compile(SOURCE, &[]);
            authority.trust(&app.load(&rt));
            let root = rt.create_root("Browser").unwrap();
            let made = Browser::call(&app, &rt, "new_page", &[("b", Value::resource(&root))])
                .expect("the browser opens a page");
            let opened = returned_ref(made);
            Self {
                rt,
                app,
                root,
                page,
                opened,
            }
        }

        fn call(
            app: &App,
            rt: &Runtime,
            action: &str,
            given: &[(&str, Value)],
        ) -> Result<InvokeOutcome, tilolang_runtime::RuntimeError> {
            let id = app.action(action);
            let args = app.args(rt, action, given);
            rt.invoke_action(&me(), (id.as_str(), args))
        }

        fn run(&self, action: &str, given: &[(&str, Value)]) -> InvokeOutcome {
            Self::call(&self.app, &self.rt, action, given)
                .unwrap_or_else(|error| panic!("{action}: {error:?}"))
        }

        fn page(&self, action: &str, given: &[(&str, Value)]) {
            let mut all = vec![("p", Value::resource(&self.opened))];
            all.extend_from_slice(given);
            self.run(action, &all);
        }

        fn view(&self, name: &str, args: Vec<Value>) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(&me(), &self.app.view(name), args)
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            window
        }

        fn page_view(&self) -> ViewInstance {
            self.view("page_view", vec![Value::resource(&self.opened)])
        }

        fn read(&self, at: &ResourceRef, field: &str) -> Value {
            self.rt.read(at, field).unwrap()
        }

        fn text(&self, at: &ResourceRef, field: &str) -> String {
            self.read(at, field)
                .as_text()
                .unwrap_or_else(|| panic!("{field} is text"))
                .to_owned()
        }
    }

    fn button_key(view: &ViewInstance, label: &str) -> String {
        of_kind(view.tree(), "ui.Button")
            .into_iter()
            .find(|b| matches!(b.props.get("label"), Some(Value::Text(l)) if l == label))
            .unwrap_or_else(|| panic!("no button {label} in\n{}", dump(view.tree())))
            .key
            .clone()
    }

    fn row_secondary(view: &ViewInstance, title: &str) -> String {
        let row = of_kind(view.tree(), "ui.ListRow")
            .into_iter()
            .find(|r| r.props.get("title") == Some(&Value::text(title)))
            .unwrap_or_else(|| panic!("no row {title} in\n{}", dump(view.tree())));
        match row.props.get("secondary") {
            Some(Value::Text(text)) => text.clone(),
            other => panic!("{title}'s secondary is {other:?}"),
        }
    }

    fn text_inputs(view: &ViewInstance) -> Vec<String> {
        of_kind(view.tree(), "ui.TextInput")
            .into_iter()
            .map(|node| node.key.clone())
            .collect()
    }

    #[test]
    fn a_download_the_page_starts_is_read_back_and_what_the_person_does_reaches_the_page() {
        let b = Browser::new();
        b.page(
            "download",
            &[("url", Value::text("https://example.org/report.pdf"))],
        );
        assert_eq!(b.page.ops(), ["start_download"]);
        assert_eq!(
            b.page.last()["url"].as_str(),
            Some("https://example.org/report.pdf")
        );

        // The engine's next report of itself: the transfer under way, two of its eight kilobytes.
        b.page.answer_asks(json!({
            "downloads": [{
                "id": 4,
                "state": "downloading",
                "name": "report.pdf",
                "url": "https://example.org/report.pdf",
                "path": "",
                "retryable": false,
                "received": 2048,
                "total": 8192,
                "error": "",
            }],
        }));
        b.page("refresh_page", &[]);
        let view = b.page_view();
        assert_eq!(row_secondary(&view, "report.pdf"), "2.0 KB of 8.1 KB");

        let cancel = button_key(&view, "Cancel");
        view.fire(&cancel, "press").unwrap();
        assert_eq!(
            b.page.ops().last().map(String::as_str),
            Some("cancel_download")
        );
        assert_eq!(b.page.last()["id"].as_i64(), Some(4));

        // Cancel every download is the one call that stops all of them.
        let view = b.page_view();
        let all = button_key(&view, "Cancel every download");
        view.fire(&all, "press").unwrap();
        assert_eq!(
            b.page.ops().last().map(String::as_str),
            Some("cancel_all_downloads")
        );
    }

    #[test]
    fn a_failed_download_offers_a_retry_that_names_it() {
        let b = Browser::new();
        b.page.answer_asks(json!({
            "downloads": [{
                "id": 9,
                "state": "failed",
                "name": "map.pdf",
                "url": "https://example.org/map.pdf",
                "path": "",
                "retryable": true,
                "received": 0,
                "total": 0,
                "error": "The site didn't answer",
            }],
        }));
        b.page("refresh_page", &[]);
        let view = b.page_view();
        assert_eq!(row_secondary(&view, "map.pdf"), "The site didn't answer");
        let retry = button_key(&view, "Retry");
        view.fire(&retry, "press").unwrap();
        assert_eq!(
            b.page.ops().last().map(String::as_str),
            Some("retry_download")
        );
        assert_eq!(b.page.last()["id"].as_i64(), Some(9));
    }

    #[test]
    fn a_whole_form_is_answered_in_one_ask() {
        let b = Browser::new();
        let answers = Value::Map(vec![
            (Value::text("name"), Value::text("Ada Lovelace")),
            (Value::text("city"), Value::text("London")),
        ]);
        b.page(
            "fill_page_form",
            &[
                ("form", Value::text("checkout-1a2b")),
                ("answers", answers),
                ("submit", Value::Bool(true)),
            ],
        );
        assert_eq!(b.page.ops(), ["fill_form"]);
        let ask = b.page.last();
        assert_eq!(ask["form"].as_str(), Some("checkout-1a2b"));
        assert_eq!(ask["answers"]["name"].as_str(), Some("Ada Lovelace"));
        assert_eq!(ask["answers"]["city"].as_str(), Some("London"));
        assert_eq!(ask["submit"].as_bool(), Some(true));
    }

    #[test]
    fn a_saved_address_answers_the_questions_a_form_asks_by_their_names() {
        let b = Browser::new();
        b.run(
            "remember_form",
            &[
                ("b", Value::resource(&b.root)),
                ("origin", Value::text("https://shop.example.org")),
                ("signature", Value::text("checkout-1a2b")),
                ("label", Value::text("Delivery address")),
                ("page", Value::text("/checkout")),
                (
                    "questions",
                    Value::List(vec![
                        Value::text("full_name\ttext\tFull name\ttrue\t"),
                        Value::text("city\ttext\tCity\ttrue\t"),
                        Value::text("nickname\ttext\tNickname\tfalse\t"),
                    ]),
                ),
                ("whole", Value::Bool(true)),
            ],
        );
        let address = returned_ref(b.run("add_address", &[("b", Value::resource(&b.root))]));
        let view = b.view("address_page", vec![Value::resource(&address)]);
        let inputs = text_inputs(&view);
        for (at, text) in inputs.iter().zip([
            "Home",
            "Ada Lovelace",
            "Ada",
            "Lovelace",
            "12 Analytical Way",
            "London",
            "London",
            "N1 9AB",
            "United Kingdom",
        ]) {
            view.edit(at, &Value::text(text)).unwrap();
        }
        assert_eq!(b.text(&address, "city"), "London");

        b.page(
            "fill_address",
            &[
                ("form", Value::text("checkout-1a2b")),
                ("a", Value::resource(&address)),
            ],
        );
        assert_eq!(b.page.ops(), ["fill_form"]);
        let ask = b.page.last();
        assert_eq!(ask["form"].as_str(), Some("checkout-1a2b"));
        assert_eq!(ask["answers"]["full_name"].as_str(), Some("Ada Lovelace"));
        assert_eq!(ask["answers"]["city"].as_str(), Some("London"));
        assert!(
            ask["answers"].get("nickname").is_none(),
            "a question an address answers nothing for is left alone: {ask}"
        );
    }

    #[test]
    fn a_sign_in_turned_down_sends_no_credentials() {
        let b = Browser::new();
        b.page(
            "cancel_sign_in",
            &[
                ("id", Value::Int(7)),
                ("asker", Value::text("proxy.example")),
            ],
        );
        assert_eq!(b.page.ops(), ["answer_sign_in"]);
        let ask = b.page.last();
        assert_eq!(ask["id"].as_i64(), Some(7));
        assert_eq!(ask["asker"].as_str(), Some("proxy.example"));
        assert!(ask["username"].is_null(), "no user name is sent: {ask}");
        assert!(ask["password"].is_null(), "no password is sent: {ask}");
    }

    #[test]
    fn a_bookmark_is_renamed_and_put_in_a_folder() {
        let b = Browser::new();
        let bookmark = b.rt.create("Bookmark").unwrap();
        b.run(
            "name_bookmark",
            &[
                ("k", Value::resource(&bookmark)),
                ("title", Value::text("A guide")),
            ],
        );
        b.run(
            "move_bookmark",
            &[
                ("k", Value::resource(&bookmark)),
                ("folder", Value::text("Work/Reading")),
            ],
        );
        assert_eq!(b.text(&bookmark, "title"), "A guide");
        assert_eq!(b.text(&bookmark, "folder"), "Work/Reading");

        // The bookmark's own window edits both, through the fields the controls bind.
        let view = b.view("bookmark_page", vec![Value::resource(&bookmark)]);
        let inputs = text_inputs(&view);
        view.edit(&inputs[0], &Value::text("Reading list")).unwrap();
        view.edit(&inputs[1], &Value::text("")).unwrap();
        assert_eq!(b.text(&bookmark, "title"), "Reading list");
        assert_eq!(b.text(&bookmark, "folder"), "");
    }
}
