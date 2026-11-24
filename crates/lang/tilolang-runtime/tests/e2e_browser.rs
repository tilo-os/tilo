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
}
