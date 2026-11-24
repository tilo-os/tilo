//! The browser app (`apps/tl/browser/app.tl`) end to end: what a page downloads and what the
//! person does about it, a whole form answered in one ask, a saved address filled into a form a
//! site asked, the addresses the settings edit, and a sign-in turned down.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use serde_json::{Value as Json, json};
    use std::cell::RefCell;
    use std::rc::Rc;
    use tilolang_runtime::files::{Entry, FileTarget, FilesHost};
    use tilolang_runtime::page::PageHost;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, RunOutcome, Runtime, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/browser/app.tl");
    /// Files' published place to show: its own action, which the Browser calls rather than
    /// reaching the device's files itself.
    const FILES_SOURCE: &str = include_str!("../../tilolang/tests/fixtures/file_explorer.tl");

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
            // The engine stops presenting the page: its next report of itself says so.
            if op == "exit_fullscreen" {
                let mut asks = self.asks.borrow_mut();
                if let Some(fields) = asks.as_object_mut() {
                    fields.insert("fullscreen".to_owned(), Json::Bool(false));
                }
            }
            Ok(json!({"ok": true}).to_string())
        }
    }

    /// The device's files, as a download's open asks them: what was opened, in order.
    #[derive(Clone, Default)]
    struct Files {
        opened: Rc<RefCell<Vec<String>>>,
    }

    impl FilesHost for Files {
        fn list(&self, _path: &str) -> Result<Vec<Entry>, String> {
            Ok(Vec::new())
        }

        fn stat(&self, _path: &str) -> Result<Option<Entry>, String> {
            Ok(None)
        }

        fn read(&self, _path: &str) -> Result<Vec<u8>, String> {
            Err("the test keeps no files".to_owned())
        }

        fn watch(&self, _path: &str, _run: &str) -> Result<String, String> {
            Err("the test watches no folder".to_owned())
        }

        fn open(&self, file: FileTarget<'_>) -> Result<(), String> {
            if let FileTarget::Path(path) = file {
                self.opened.borrow_mut().push(path.to_owned());
            }
            Ok(())
        }
    }

    struct Browser {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        page: Page,
        files: Files,
        opened: ResourceRef,
    }

    impl Browser {
        fn new() -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let page = Page::default();
            rt.set_page(page.clone());
            let files = Files::default();
            rt.set_device_files(files.clone());
            rt.set_clock(|| "2026-11-24T09:00:00Z".to_owned());
            // The Browser shows where a download stands by asking Files, whose own action knows
            // the device's files.
            let files_app = App::compile(FILES_SOURCE, &[]);
            files_app.load(&rt);
            let app = App::compile(SOURCE, &[files_app.meta().clone()]);
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
                files,
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

    /// The key of the window's own plain control named `label`, not the action of the same name
    /// the app publishes into its window's action menu: a view's control says it is plain.
    fn plain_button(view: &ViewInstance, label: &str) -> Option<String> {
        of_kind(view.tree(), "ui.Button")
            .into_iter()
            .find(|b| {
                b.props.get("label") == Some(&Value::text(label))
                    && b.props.get("role") == Some(&Value::text("plain"))
            })
            .map(|b| b.key.clone())
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
    fn the_window_search_finds_in_the_open_page_and_clearing_it_takes_the_find_away() {
        let b = Browser::new();
        let mut view = b.page_view();
        let shell = of_kind(view.tree(), "ui.NavShell")[0].key.clone();
        view.fire_with(&shell, "search", Some(&Value::text("harbour")))
            .unwrap();
        assert!(
            b.page.ops().iter().any(|op| op == "find_in_page"),
            "the words are found in the page: {:?}",
            b.page.ops()
        );
        assert_eq!(b.page.last()["text"], Json::from("harbour"));
        assert_eq!(
            b.text(&b.opened, "find_text"),
            "harbour",
            "the words are kept for the next and previous match"
        );

        view.fire_with(&shell, "search", Some(&Value::text("")))
            .unwrap();
        assert!(
            b.page.ops().iter().any(|op| op == "stop_finding"),
            "clearing the search leaves the page as it was: {:?}",
            b.page.ops()
        );
        assert_eq!(b.text(&b.opened, "find_text"), "");
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

    /// A file input is answered with the paths a person or an agent chose there, through the
    /// page's own validation of what it accepts.
    #[test]
    fn a_file_input_is_filled_with_the_paths_it_was_given() {
        let b = Browser::new();
        b.page(
            "fill_page_files",
            &[
                ("element", Value::text("d1")),
                (
                    "paths",
                    Value::List(vec![
                        Value::text("/home/ada/picture.png"),
                        Value::text("/home/ada/second.png"),
                    ]),
                ),
            ],
        );
        assert_eq!(b.page.ops(), ["fill_files"]);
        let ask = b.page.last();
        assert_eq!(ask["element"].as_str(), Some("d1"));
        let paths: Vec<&str> = ask["paths"]
            .as_array()
            .expect("the paths are an array")
            .iter()
            .filter_map(Json::as_str)
            .collect();
        assert_eq!(paths, ["/home/ada/picture.png", "/home/ada/second.png"]);
    }

    /// The page's sound and its own back and forward pages are read into its record, and the
    /// person's mute and history asks reach the engine.
    #[test]
    fn a_pages_sound_and_its_own_history_are_read_and_asked_for() {
        let b = Browser::new();
        b.page.answer_asks(json!({
            "muted": true,
            "back": [{"id": 2, "url": "https://example.org/earlier", "title": "Earlier"}],
            "forward": [{"id": 3, "url": "https://example.org/later", "title": ""}],
        }));
        b.page("refresh_page", &[]);
        assert!(
            matches!(b.read(&b.opened, "muted"), Value::Bool(true)),
            "the page's sound is muted"
        );
        let back = b.rt.project(&b.read(&b.opened, "back"));
        assert_eq!(
            back["items"][0]["fields"]["url"].as_str(),
            Some("https://example.org/earlier")
        );
        assert_eq!(
            back["items"][0]["fields"]["title"].as_str(),
            Some("Earlier")
        );
        let after = b.rt.project(&b.read(&b.opened, "forward"));
        assert_eq!(
            after["items"][0]["fields"]["title"].as_str(),
            Some("https://example.org/later"),
            "a page the engine has no title for is named by its address"
        );

        b.page("mute", &[("muted", Value::Bool(false))]);
        assert_eq!(b.page.ops().last().map(String::as_str), Some("mute"));
        let ask = b.page.last();
        assert_eq!(ask["muted"].as_bool(), Some(false), "the mute ask: {ask}");

        b.page("read_back_history", &[]);
        assert_eq!(b.page.ops().last().map(String::as_str), Some("history"));
        assert_eq!(b.page.last()["direction"].as_str(), Some("back"));

        b.page("read_forward_history", &[]);
        assert_eq!(b.page.last()["direction"].as_str(), Some("forward"));

        b.page("go_history_entry", &[("entry", Value::Int(2))]);
        assert_eq!(
            b.page.ops().last().map(String::as_str),
            Some("history_entry")
        );
        assert_eq!(b.page.last()["entry"].as_i64(), Some(2));
    }

    /// A page that goes fullscreen shows alone with the way out over it, and pressing the way
    /// out asks the engine to leave: the page's record leaves fullscreen, and its window shows
    /// its own controls again.
    #[test]
    fn a_page_in_fullscreen_shows_the_way_out_and_leaving_asks_the_engine() {
        let b = Browser::new();
        b.page.answer_asks(json!({"fullscreen": false}));
        b.page("refresh_page", &[]);
        let window = b.page_view();
        assert!(
            of_kind(window.tree(), "ui.Fullscreen").is_empty(),
            "at rest the page shows its own controls:\n{}",
            dump(window.tree())
        );

        b.page.answer_asks(json!({"fullscreen": true}));
        b.page("refresh_page", &[]);
        assert!(
            matches!(b.read(&b.opened, "fullscreen"), Value::Bool(true)),
            "the page fills the screen"
        );
        let window = b.page_view();
        let presented = of_kind(window.tree(), "ui.Fullscreen")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("the page is presented:\n{}", dump(window.tree())));
        assert!(
            matches!(
                presented.props.get("showing"),
                Some(Value::Bool(true)) | Some(Value::Int(1))
            ),
            "it presents the page: {:?}",
            presented.props
        );
        assert!(
            presented.children.len() == 1 && presented.children[0].kind == "ui.WebView",
            "the page alone is presented:\n{}",
            dump(window.tree())
        );

        window.fire(&presented.key, "exit").unwrap();
        assert_eq!(
            b.page.ops().last().map(String::as_str),
            Some("exit_fullscreen"),
            "the way out asks the engine to leave"
        );
        assert!(
            matches!(b.read(&b.opened, "fullscreen"), Value::Bool(false)),
            "the record leaves fullscreen"
        );
        // The engine's own report of itself agrees, so what the window shows and what the page
        // says stay one thing.
        b.page("refresh_page", &[]);
        let window = b.page_view();
        assert!(
            of_kind(window.tree(), "ui.Fullscreen").is_empty(),
            "the page shows its own controls again:\n{}",
            dump(window.tree())
        );
    }

    /// What a site's connection and permissions are is read into the page, and the person's
    /// clearing, blocking and asking again reach the engine for this site and this document.
    #[test]
    fn a_site_is_read_and_its_data_and_permissions_are_acted_on() {
        let b = Browser::new();
        b.page(
            "went_to",
            &[("url", Value::text("https://example.org/private"))],
        );
        b.page.answer_asks(json!({
            "site": {
                "origin": "https://example.org",
                "generation": 4,
                "blocked": 7,
                "protected": true,
                "security": "secure",
                "certificate": 0,
                "issuer": "Example CA",
                "subject": "example.org",
                "permissions": [{"kind": "camera", "state": "ask"}],
                "can_reset": true,
            },
        }));
        b.page("refresh_page", &[]);
        let view = b.page_view();
        assert_eq!(row_secondary(&view, "https://example.org"), "secure");
        assert_eq!(row_secondary(&view, "Requests blocked"), "7");
        assert_eq!(row_secondary(&view, "Certificate"), "Example CA");

        b.page("read_site", &[]);
        assert_eq!(b.page.ops().last().map(String::as_str), Some("site_status"));
        assert_eq!(
            b.page.last()["origin"].as_str(),
            Some("https://example.org")
        );

        b.page("clear_site_data", &[]);
        assert_eq!(b.page.ops().last().map(String::as_str), Some("site_action"));
        assert_eq!(b.page.last()["doing"].as_str(), Some("clear"));
        assert_eq!(b.page.last()["generation"].as_i64(), Some(4));

        b.page("block_site_permissions", &[]);
        assert_eq!(b.page.last()["doing"].as_str(), Some("revoke"));

        b.page("ask_site_permissions_again", &[]);
        assert_eq!(b.page.last()["doing"].as_str(), Some("reset"));

        b.run(
            "set_site_protection",
            &[
                ("b", Value::resource(&b.root)),
                ("origin", Value::text("https://example.org")),
                ("enabled", Value::Bool(false)),
            ],
        );
        b.page("sync_site_exceptions", &[]);
        assert_eq!(
            b.page.ops().last().map(String::as_str),
            Some("site_exceptions")
        );
        let ask = b.page.last();
        let origins: Vec<&str> = ask["origins"]
            .as_array()
            .expect("the origins are an array")
            .iter()
            .filter_map(Json::as_str)
            .collect();
        assert_eq!(origins, ["https://example.org"]);
    }

    /// The person's history stays within its bound: the oldest visits go and the newest stay.
    #[test]
    fn the_history_stays_within_its_bound() {
        let b = Browser::new();
        for (at, name) in [
            ("2026-11-24T09:00:00Z", "a"),
            ("2026-11-24T10:00:00Z", "b"),
            ("2026-11-24T11:00:00Z", "c"),
        ] {
            b.run(
                "visit",
                &[
                    ("b", Value::resource(&b.root)),
                    ("url", Value::text(format!("https://example.org/{name}"))),
                    ("title", Value::text(name)),
                    ("at", Value::text(at)),
                ],
            );
        }
        b.run(
            "trim_history",
            &[("b", Value::resource(&b.root)), ("most", Value::Int(2))],
        );
        let visits = b.rt.project(&b.read(&b.root, "visits"));
        let urls: Vec<&str> = visits["items"]
            .as_array()
            .expect("visits are a list")
            .iter()
            .filter_map(|visit| visit["fields"]["url"].as_str())
            .collect();
        assert_eq!(urls.len(), 2, "two visits stay: {urls:?}");
        assert!(
            !urls.contains(&"https://example.org/a"),
            "the oldest goes: {urls:?}"
        );
        assert!(urls.contains(&"https://example.org/c"), "the newest stays");
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

    /// A private window settles the page's privacy before its window reads anything, and what it
    /// opens joins no history the next window reads.
    #[test]
    fn a_private_window_keeps_nothing_of_what_it_opens() {
        let b = Browser::new();
        let made = returned_ref(b.run("new_private_page", &[("b", Value::resource(&b.root))]));
        assert_eq!(b.read(&made, "private"), Value::Bool(true));

        b.run("opened_page", &[("p", Value::resource(&made))]);
        assert_eq!(
            b.page.ops(),
            ["private", "asks"],
            "the privacy is settled before the page is read"
        );

        b.run(
            "went_to",
            &[
                ("p", Value::resource(&made)),
                ("url", Value::text("https://example.org/private")),
            ],
        );
        let visits = b.rt.project(&b.read(&b.root, "visits"));
        assert_eq!(
            visits["items"].as_array().map(Vec::len),
            Some(0),
            "a private page's addresses join no history: {visits}"
        );

        let view = b.view("page_view", vec![Value::resource(&made)]);
        assert!(
            !of_kind(view.tree(), "ui.Banner").is_empty(),
            "the window says it is private:\n{}",
            dump(view.tree())
        );

        // An ordinary page says nothing on the same window.
        let open = returned_ref(b.run("new_page", &[("b", Value::resource(&b.root))]));
        let view = b.view("page_view", vec![Value::resource(&open)]);
        assert!(
            of_kind(view.tree(), "ui.Banner").is_empty(),
            "an ordinary page shows no private notice"
        );
    }

    /// The extensions' toolbar buttons are read from the page, a press reaches the engine with
    /// the extension it stands for, and one the extension dimmed takes no press.
    #[test]
    fn the_extensions_toolbar_buttons_are_read_and_pressed_through_the_page() {
        let b = Browser::new();
        b.page(
            "remember_extensions",
            &[
                (
                    "installed",
                    Value::text(
                        r#"[{"id":"abcdefghijklmnopabcdefghijklmnop","name":"Reader","version":"1.0","manifest_version":3,"description":"","permissions":[],"enabled":true}]"#,
                    ),
                ),
                ("message", Value::text("")),
            ],
        );
        b.page.answer_asks(json!({
            "extension_buttons": [
                {"id":"abcdefghijklmnopabcdefghijklmnop","title":"","badge":"3","enabled":true,"popup":false},
                {"id":"ponmlkjihgfedcbaponmlkjihgfedcba","title":"Dimmer","badge":"","enabled":false,"popup":false},
            ],
        }));
        b.page("refresh_page", &[]);
        let view = b.page_view();
        // A button with no title of its own is named by its extension, with its badge.
        view.fire(&button_key(&view, "Reader · 3"), "press")
            .unwrap();
        assert_eq!(
            b.page.last()["op"].as_str(),
            Some("extension_click"),
            "the engine is asked to press the extension"
        );
        assert_eq!(
            b.page.last()["extension"].as_str(),
            Some("abcdefghijklmnopabcdefghijklmnop")
        );

        // The dimmed one does not answer a press, and its refusal says why.
        let Value::List(items) = b.read(&b.opened, "buttons") else {
            panic!("the buttons are a list");
        };
        let Value::Ref(dimmer) = items.get(1).expect("the dimmed button") else {
            panic!("a button is a resource");
        };
        let refused = Browser::call(
            &b.app,
            &b.rt,
            "click_extension",
            &[
                ("p", Value::resource(&b.opened)),
                ("b", Value::resource(&dimmer.target)),
            ],
        );
        assert!(refused.is_err(), "a press on a dimmed button is refused");
    }

    /// A notification an extension showed is drawn on the page with its buttons, and what the
    /// person presses reaches the engine with the extension, its notification and the button.
    #[test]
    fn an_extensions_notification_is_shown_and_answered_on_the_page() {
        let b = Browser::new();
        b.page.answer_asks(json!({
            "extension_notices": [{
                "id": "abcdefghijklmnopabcdefghijklmnop",
                "notification": "n1",
                "title": "Saved for later",
                "message": "Three pages kept",
                "buttons": ["Open", "Later"],
            }],
        }));
        b.page("refresh_page", &[]);
        let view = b.page_view();
        assert!(
            of_kind(view.tree(), "ui.Banner")
                .iter()
                .any(|banner| banner.props.get("title") == Some(&Value::text("Saved for later"))),
            "the notification is a banner on the page:\n{}",
            dump(view.tree())
        );

        view.fire(&button_key(&view, "Later"), "press").unwrap();
        assert_eq!(
            b.page.last()["op"].as_str(),
            Some("extension_notice_chosen"),
            "the engine hears which button was pressed"
        );
        assert_eq!(
            b.page.last()["extension"].as_str(),
            Some("abcdefghijklmnopabcdefghijklmnop")
        );
        assert_eq!(b.page.last()["notification"].as_str(), Some("n1"));
        assert_eq!(
            b.page.last()["button"].as_str(),
            Some("1"),
            "the second button is the second one"
        );

        let view = b.page_view();
        view.fire(&button_key(&view, "Dismiss"), "press").unwrap();
        assert_eq!(
            b.page.last()["op"].as_str(),
            Some("extension_notice_chosen")
        );
        assert!(
            b.page.last()["button"].is_null(),
            "taking it away presses no button: {}",
            b.page.last()
        );
    }

    /// A saved download is shown in Files by calling Files' own action: the Browser names the
    /// place and Files, the app that knows the device's files, shows it. The Browser never reads
    /// a folder itself.
    #[test]
    fn a_saved_download_is_shown_in_files_through_its_own_action() {
        let authority = TestAuthority::new();
        let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
        rt.set_clock(|| "2026-11-24T09:00:00Z".to_owned());
        let files = App::compile(FILES_SOURCE, &[]);
        files.load(&rt);
        // The Browser is not trusted with another app's actions, so its call to Files asks.
        let app = App::compile(SOURCE, &[files.meta().clone()]);
        app.load(&rt);
        let files_root = rt.create_root("Files").expect("the Files root");
        let dispatch = Dispatch::new();
        rt.set_foreign_invoker(dispatch.clone());

        let out = Browser::call(
            &app,
            &rt,
            "reveal_download",
            &[("path", Value::text("/home/ada/Downloads/report.pdf"))],
        )
        .unwrap_or_else(|error| panic!("the download is shown: {error:?}"));
        let InvokeOutcome::Waits { run, .. } = out else {
            panic!("the call to Files asks first: {out:?}");
        };
        let (action, args) = dispatch.asked();
        assert_eq!(
            action,
            files.action("show"),
            "Files' own action, by name, not a second path of its own"
        );
        assert!(
            args.contains(&Value::text("/home/ada/Downloads/report.pdf")),
            "the call names the download: {args:?}"
        );

        let RunOutcome::Done(_) = dispatch.allow(&rt, &run) else {
            panic!("Files shows the place");
        };
        assert_eq!(
            rt.read(&files_root, "place").unwrap().as_text(),
            Some("/home/ada/Downloads/report.pdf"),
            "Files shows the download's place"
        );

        // Nothing to show for a download that was never saved.
        let refused = Browser::call(&app, &rt, "reveal_download", &[("path", Value::text(""))]);
        assert!(refused.is_err(), "a download with no path shows nothing");
    }

    /// The extensions this device keeps are read into the Browser's record, installed from the
    /// package the person names, turned on and off and removed through their rows, and the
    /// engine's own answer is shown in the person's words.
    #[test]
    fn extensions_are_read_installed_turned_on_and_removed_through_the_page() {
        let b = Browser::new();
        b.page("list_extensions", &[]);
        assert_eq!(b.page.ops(), ["extension_list"]);

        b.page(
            "remember_extensions",
            &[
                (
                    "installed",
                    Value::text(
                        r#"[{"id":"abcdefghijklmnopabcdefghijklmnop","name":"Reader","version":"1.2.3","manifest_version":3,"description":"Reads pages","permissions":["storage","tabs"],"enabled":true}]"#,
                    ),
                ),
                ("message", Value::text("")),
            ],
        );
        let view = b.page_view();
        assert_eq!(row_secondary(&view, "Reader"), "On (1.2.3)");

        b.page(
            "install_extension",
            &[("path", Value::text("/home/ada/Downloads/reader.crx"))],
        );
        assert_eq!(
            b.page.last()["op"].as_str(),
            Some("extension_install"),
            "the engine is asked to install the package the person named"
        );
        assert_eq!(
            b.page.last()["path"].as_str(),
            Some("/home/ada/Downloads/reader.crx")
        );
        let refused = Browser::call(
            &b.app,
            &b.rt,
            "install_extension",
            &[("p", Value::resource(&b.opened)), ("path", Value::text(""))],
        );
        assert!(refused.is_err(), "no package named, nothing installed");

        // Turning it off names the extension and says which way.
        let view = b.page_view();
        view.fire(&button_key(&view, "Turn the extension off"), "press")
            .unwrap();
        assert_eq!(b.page.last()["op"].as_str(), Some("extension_enable"));
        assert_eq!(
            b.page.last()["extension"].as_str(),
            Some("abcdefghijklmnopabcdefghijklmnop")
        );
        assert_eq!(b.page.last()["enabled"].as_bool(), Some(false));

        let view = b.page_view();
        view.fire(&button_key(&view, "Remove"), "press").unwrap();
        assert_eq!(b.page.last()["op"].as_str(), Some("extension_remove"));
        assert_eq!(
            b.page.last()["extension"].as_str(),
            Some("abcdefghijklmnopabcdefghijklmnop")
        );

        // What the engine read of a package it could not install is shown as the person's words.
        b.page(
            "note_extension",
            &[
                ("ok", Value::Bool(false)),
                ("kind", Value::text("damaged")),
                ("message", Value::text("the package is damaged: signature")),
                ("id", Value::text("")),
            ],
        );
        assert_eq!(
            b.text(&b.root, "extension_notice"),
            "The package is damaged, or signed for another extension"
        );
        b.page(
            "note_extension",
            &[
                ("ok", Value::Bool(true)),
                ("kind", Value::text("")),
                ("message", Value::text("")),
                ("id", Value::text("abcdefghijklmnopabcdefghijklmnop")),
            ],
        );
        assert_eq!(b.text(&b.root, "extension_notice"), "");
    }

    /// The logins the engine keeps for a site are found, captured, saved, filled and forgotten
    /// through the engine's own credential store: the app names the site and the form, and the
    /// password never enters it.
    #[test]
    fn a_login_is_found_saved_filled_and_forgotten_through_the_page() {
        let b = Browser::new();
        b.page(
            "went_to",
            &[("url", Value::text("https://example.org/login"))],
        );

        b.page("discover_logins", &[]);
        assert_eq!(b.page.ops(), ["credential"]);
        let ask = b.page.last();
        assert_eq!(ask["credential"].as_str(), Some("discover"));
        assert_eq!(ask["origin"].as_str(), Some("https://example.org"));
        assert!(
            ask.get("document").is_none(),
            "the window names the document the page is at: {ask}"
        );

        // What the engine's store answered reaches the page's record.
        let answered = |did: &str, capture: &str, accounts: &str| {
            vec![
                ("did", Value::text(did)),
                ("success", Value::Bool(true)),
                ("message", Value::text("")),
                (
                    "forms",
                    Value::text(r#"[{"form":"1","username":"ada","kind":"login"}]"#),
                ),
                ("accounts", Value::text(accounts)),
                ("capture", Value::text(capture)),
                ("username", Value::text("ada")),
                ("late", Value::Bool(false)),
            ]
        };
        b.page("remember_credential", &answered("discover", "", "[]"));
        let view = b.page_view();
        assert_eq!(
            row_secondary(&view, "ada"),
            "login",
            "the form the engine found is on the page"
        );

        // The person takes the login from that form; the engine answers with a capture.
        view.fire(&button_key(&view, "Save this login"), "press")
            .unwrap();
        assert_eq!(b.page.last()["credential"].as_str(), Some("capture"));
        assert_eq!(b.page.last()["form"].as_str(), Some("1"));
        b.page("remember_credential", &answered("capture", "7", "[]"));

        // Saving names the capture, and the capture is spent once the engine has it.
        let view = b.page_view();
        view.fire(&button_key(&view, "Save the login"), "press")
            .unwrap();
        assert_eq!(b.page.last()["credential"].as_str(), Some("save"));
        assert_eq!(b.page.last()["capture"].as_str(), Some("7"));
        b.page("remember_credential", &answered("save", "", "[]"));
        assert_eq!(b.text(&b.opened, "capture"), "", "the capture is spent");

        // The logins the engine keeps for the site fill the page's form.
        b.page("list_saved_logins", &[]);
        assert_eq!(b.page.last()["credential"].as_str(), Some("list"));
        b.page(
            "remember_credential",
            &answered(
                "list",
                "",
                r#"[{"account":"a","username":"ada@example.org"}]"#,
            ),
        );
        let view = b.page_view();
        assert_eq!(
            row_secondary(&view, "ada@example.org"),
            "Saved for this site"
        );
        view.fire(&button_key(&view, "Fill"), "press").unwrap();
        assert_eq!(b.page.last()["credential"].as_str(), Some("fill"));
        assert_eq!(b.page.last()["form"].as_str(), Some("1"));
        assert_eq!(b.page.last()["account"].as_str(), Some("a"));

        view.fire(&button_key(&view, "Delete"), "press").unwrap();
        assert_eq!(b.page.last()["credential"].as_str(), Some("delete"));
        assert_eq!(b.page.last()["account"].as_str(), Some("a"));

        // A login the engine could not save leaves the capture for another try and says why.
        b.page("remember_credential", &answered("capture", "9", "[]"));
        b.page(
            "remember_credential",
            &[
                ("did", Value::text("save")),
                ("success", Value::Bool(false)),
                ("message", Value::text("Capture expired; capture again")),
                ("forms", Value::text("[]")),
                ("accounts", Value::text("[]")),
                ("capture", Value::text("")),
                ("username", Value::text("")),
                ("late", Value::Bool(false)),
            ],
        );
        assert_eq!(
            b.text(&b.opened, "capture"),
            "9",
            "the capture is kept for another try"
        );
        assert_eq!(
            b.text(&b.opened, "login_notice"),
            "Capture expired; capture again",
            "the engine's own words are shown"
        );
    }

    /// Another browser's bookmarks come in with their folders: a flat entry's folder and a
    /// tree's nested folder both reach the library as the path the bookmark stands under.
    #[test]
    fn imported_bookmarks_keep_their_folders_and_their_tree() {
        let b = Browser::new();
        b.run(
            "import_bookmarks",
            &[
                ("b", Value::resource(&b.root)),
                (
                    "bundle",
                    Value::text(
                        r#"{"bookmarks":[
                            {"url":"https://example.org/a","title":"A","folder":"Work"},
                            {"title":"Reading","children":[
                                {"url":"https://example.org/b","title":"B"}
                            ]}
                        ]}"#,
                    ),
                ),
            ],
        );
        let made = b.rt.project(&b.read(&b.root, "bookmarks"));
        let items = made["items"].as_array().expect("the bookmarks are a list");
        let folder = |url: &str| {
            items
                .iter()
                .find(|item| item["fields"]["url"].as_str() == Some(url))
                .unwrap_or_else(|| panic!("no bookmark for {url}: {made}"))["fields"]["folder"]
                .as_str()
                .map(str::to_owned)
        };
        assert_eq!(folder("https://example.org/a").as_deref(), Some("Work"));
        assert_eq!(folder("https://example.org/b").as_deref(), Some("Reading"));
    }

    /// A saved download opens in the app that opens its kind, through the system's one open,
    /// and one that was never saved is refused before anything is opened.
    /// The engine's own developer tools are asked for by the page's action.
    /// A page the page opened in a window of its own becomes a page of the browser.
    #[test]
    fn a_popup_a_page_opened_becomes_a_page_of_the_browser() {
        let b = Browser::new();
        b.page(
            "open_from_page",
            &[
                ("url", Value::text("https://example.org/sign-in")),
                ("popup", Value::Bool(true)),
            ],
        );
        let pages = b.rt.project(&b.read(&b.root, "pages"));
        let urls: Vec<&str> = pages["items"]
            .as_array()
            .expect("pages are a list")
            .iter()
            .filter_map(|page| page["fields"]["url"].as_str())
            .collect();
        assert!(
            urls.contains(&"https://example.org/sign-in"),
            "the popup is kept as a page: {urls:?}"
        );
    }

    #[test]
    fn developer_tools_are_asked_for() {
        let b = Browser::new();
        b.page("developer_tools", &[]);
        assert_eq!(
            b.page.ops().last().map(String::as_str),
            Some("developer_tools")
        );
    }

    /// What the window hands over while a transfer moves and when the page's history is read is
    /// written into the same page record the window draws, so the view moves while a person looks.
    #[test]
    fn a_moving_download_and_the_pages_it_can_go_to_reach_the_record() {
        let b = Browser::new();
        let download = |received: i64| {
            vec![
                ("id", Value::Int(4)),
                ("state", Value::text("downloading")),
                ("name", Value::text("report.pdf")),
                ("url", Value::text("https://example.org/report.pdf")),
                ("path", Value::text("")),
                ("retryable", Value::Bool(false)),
                ("received", Value::Int(received)),
                ("total", Value::Int(8192)),
                ("error", Value::text("")),
            ]
        };
        b.page("update_download", &download(2048));
        assert_eq!(
            row_secondary(&b.page_view(), "report.pdf"),
            "2.0 KB of 8.1 KB",
            "the record moves with the transfer"
        );
        b.page("update_download", &download(4096));
        assert_eq!(
            row_secondary(&b.page_view(), "report.pdf"),
            "4.0 KB of 8.1 KB",
            "one download, moved"
        );

        b.page(
            "remember_history",
            &[
                ("direction", Value::text("back")),
                (
                    "entries",
                    Value::text(
                        r#"[{"id":2,"url":"https://example.org/earlier","title":"Earlier"}]"#,
                    ),
                ),
            ],
        );
        let back = b.rt.project(&b.read(&b.opened, "back"));
        assert_eq!(
            back["items"][0]["fields"]["title"].as_str(),
            Some("Earlier")
        );
    }

    #[test]
    fn a_saved_download_opens_through_the_systems_open() {
        let b = Browser::new();
        let refused = Browser::call(
            &b.app,
            &b.rt,
            "open_download",
            &[
                ("name", Value::text("report.pdf")),
                ("path", Value::text("")),
            ],
        );
        assert!(refused.is_err(), "a download with no path is refused");
        assert!(b.files.opened.borrow().is_empty(), "nothing was opened");

        b.run(
            "open_download",
            &[
                ("name", Value::text("report.pdf")),
                ("path", Value::text("/home/ada/report.pdf")),
            ],
        );
        assert_eq!(
            b.files.opened.borrow().as_slice(),
            ["/home/ada/report.pdf"],
            "the system opened exactly the download's path"
        );
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

    #[test]
    fn a_page_with_a_video_offers_picture_in_picture_and_its_view_asks_to_float_it() {
        let b = Browser::new();
        // A page that reports no video offers nothing to float. (The app's published action of
        // the same name stands in the window's action menu whatever the page shows: this is the
        // page window's own control.)
        b.page("refresh_page", &[]);
        let view = b.page_view();
        assert!(
            plain_button(&view, "Picture in picture").is_none(),
            "no picture-in-picture control without a video:\n{}",
            dump(view.tree())
        );

        // The engine says what video the page shows: where it is in the viewport, how big the
        // picture is and what it is called.
        b.page.answer_asks(json!({
            "media": {
                "element": "video:1",
                "viewport": [0.1, 0.2, 0.5, 0.3],
                "intrinsic": [1280, 720],
                "label": "A talk on maps",
            },
        }));
        b.page("refresh_page", &[]);
        assert!(
            matches!(b.read(&b.opened, "media"), Value::Opt(Some(_))),
            "the page's record keeps the video the engine reported"
        );

        let view = b.page_view();
        let float = plain_button(&view, "Picture in picture")
            .expect("the page's control, once it shows a video");
        let webview = of_kind(view.tree(), "ui.WebView").remove(0);
        assert_eq!(
            webview.props.get("pip"),
            Some(&Value::Int(0)),
            "the view floats nothing before the person asks"
        );

        // Pressing the control asks the window to float the page's video.
        view.fire(&float, "press").unwrap();
        assert_eq!(b.read(&b.opened, "pip"), Value::Bool(true));
        let view = b.page_view();
        let webview = of_kind(view.tree(), "ui.WebView").remove(0);
        assert_eq!(
            webview.props.get("pip"),
            Some(&Value::Int(1)),
            "the page's view asks the window to float its video:\n{}",
            dump(view.tree())
        );
        // The page's menu offers the way back too.
        assert!(
            of_kind(view.tree(), "ui.MenuItem")
                .iter()
                .any(|row| row.props.get("label")
                    == Some(&Value::text("Return from picture in picture"))),
            "the menu offers the way back:\n{}",
            dump(view.tree())
        );

        // Leaving picture in picture puts the video back in the page.
        let back = plain_button(&view, "Return from picture in picture")
            .expect("the control, while the video floats");
        view.fire(&back, "press").unwrap();
        assert_eq!(b.read(&b.opened, "pip"), Value::Bool(false));
        let view = b.page_view();
        let webview = of_kind(view.tree(), "ui.WebView").remove(0);
        assert_eq!(
            webview.props.get("pip"),
            Some(&Value::Int(0)),
            "the view stops floating the video"
        );
        assert_eq!(
            plain_button(&view, "Picture in picture"),
            Some(float),
            "the same control asks again"
        );
    }
}
