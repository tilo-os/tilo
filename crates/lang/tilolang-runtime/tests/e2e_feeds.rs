//! The feeds app (`apps/tl/feeds/app.tl`) end to end: subscriptions, a refresh whose answers land
//! as they arrive, a site that refuses, a capsule's certificate pinned, reading marks, stars and
//! tags, OPML in and out, the reading settings, and the window's places.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use serde_json::{Value as Json, json};
    use std::cell::RefCell;
    use std::rc::Rc;
    use tilolang_runtime::blob::FileValue;
    use tilolang_runtime::host_work::EngineHost;
    use tilolang_runtime::{AmStore, InvokeOutcome, ResourceRef, Runtime, Value, ViewInstance};

    const SOURCE: &str = include_str!("../../../../apps/tl/feeds/app.tl");

    const GUARDIAN: &str = "https://www.theguardian.com/world/rss";
    const LWN: &str = "https://lwn.net/headlines/rss";
    const BROKEN: &str = "https://broken.test/feed";
    const CAPSULE: &str = "gemini://example.org/";

    /// What the device answered, and what it was asked: the engine's side of a refresh, played
    /// over the app's own JSON, so both ends of the contract are read here.
    #[derive(Clone, Default)]
    struct Device {
        asked: Rc<RefCell<Vec<(String, String)>>>,
        /// The feeds of the refresh, as the app asked for them.
        feeds: Rc<RefCell<Vec<Json>>>,
        /// The feeds whose answer the job still holds.
        ready: Rc<RefCell<Vec<String>>>,
        /// What a `take` was asked for.
        taken: Rc<RefCell<Vec<String>>>,
        /// The bytes the content store holds, by hash.
        files: Rc<RefCell<Vec<(String, Vec<u8>)>>>,
    }

    impl Device {
        fn ops(&self) -> Vec<String> {
            self.asked
                .borrow()
                .iter()
                .map(|(op, _)| op.clone())
                .collect()
        }

        fn inputs(&self) -> Vec<String> {
            self.asked
                .borrow()
                .iter()
                .map(|(_, input)| input.clone())
                .collect()
        }

        /// The file `name` holding `text`, kept in this device's store.
        fn file(&self, name: &str, text: &str) -> Value {
            let hash = format!("h{}", text.len());
            self.files
                .borrow_mut()
                .push((hash.clone(), text.as_bytes().to_vec()));
            Value::file(FileValue::new(
                name,
                "text/x-opml",
                i64::try_from(text.len()).unwrap_or(i64::MAX),
                hash,
            ))
        }
    }

    impl EngineHost for Device {
        fn call(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
            assert_eq!(app, "so.tilo.feeds", "the engine answers its own app");
            self.asked
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            let ask: Json = serde_json::from_str(input).unwrap_or(Json::Null);
            match op {
                "take" => {
                    let url = ask["url"].as_str().unwrap_or_default().to_owned();
                    self.taken.borrow_mut().push(url.clone());
                    self.ready.borrow_mut().retain(|ready| *ready != url);
                    Ok(answer(&url))
                }
                "opml" => Ok(json!({ "outlines": [
                    json!({"title": "LWN", "url": LWN, "site": "https://lwn.net", "folder": "Reading"}).to_string(),
                    json!({"title": "", "url": "https://bare.test/feed", "site": "", "folder": ""}).to_string(),
                ]})
                .to_string()),
                "opml-export" => Ok(json!({
                    "text": format!(
                        "{} feeds",
                        ask["feeds"].as_array().map_or(0, Vec::len)
                    )
                })
                .to_string()),
                other => Err(format!("`{other}` is no op this test's engine runs")),
            }
        }

        fn start(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
            assert_eq!(app, "so.tilo.feeds");
            assert_eq!(op, "refresh");
            self.asked
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            let ask: Json = serde_json::from_str(input).expect("the refresh reads as JSON");
            let feeds = ask["feeds"].as_array().cloned().unwrap_or_default();
            self.ready.borrow_mut().extend(
                feeds
                    .iter()
                    .filter_map(|feed| feed["url"].as_str().map(str::to_owned)),
            );
            *self.feeds.borrow_mut() = feeds;
            Ok("job-1".to_owned())
        }

        fn status(&self, job: &str) -> Result<String, String> {
            assert_eq!(job, "job-1");
            let feeds = self.feeds.borrow().len();
            let ready = self.ready.borrow().clone();
            let done = feeds.saturating_sub(ready.len());
            Ok(json!({
                "state": if ready.is_empty() { "done" } else { "running" },
                "done": done,
                "total": feeds,
                "ready": ready,
            })
            .to_string())
        }

        fn cancel(&self, job: &str) -> bool {
            if job != "job-1" {
                return false;
            }
            // A cancelled read answers nothing more: what it had not taken is dropped.
            self.ready.borrow_mut().clear();
            self.feeds.borrow_mut().clear();
            true
        }
    }

    /// What a subscription's fetch answered: a feed of two articles, a capsule, or a refusal.
    fn answer(url: &str) -> String {
        if url == BROKEN {
            return json!({
                "ok": false, "title": "", "pin": "", "reason": "refused", "status": 403,
                "message": "HTTP 403", "articles": [],
            })
            .to_string();
        }
        if url == CAPSULE {
            return json!({
                "ok": true, "title": "", "pin": "abc123", "reason": "", "status": 0,
                "message": "",
                "articles": [json!({
                    "title": "A capsule", "author": "", "url": CAPSULE, "published": 1_760_000_000,
                    "summary": "Notes from a small capsule.",
                    "body": "# A capsule\n\nNotes from a small capsule.",
                }).to_string()],
            })
            .to_string();
        }
        json!({
            "ok": true,
            "title": if url == GUARDIAN { "The Guardian: World" } else { "" },
            "pin": "", "reason": "", "status": 0, "message": "",
            "articles": [
                json!({
                    "title": "A quiet morning", "author": "Ada Ferrer",
                    "url": "https://www.theguardian.com/world/2026/nov/24/a-quiet-morning",
                    "published": 1_763_000_000,
                    "summary": "The streets are empty.",
                    "body": "## The streets are empty\n\nThe light is good.",
                })
                .to_string(),
                json!({
                    "title": "A second piece", "author": "",
                    "url": "https://www.theguardian.com/world/2026/nov/23/second",
                    "published": 1_762_000_000,
                    "summary": "Later news.",
                    "body": "Later news.",
                })
                .to_string(),
            ],
        })
        .to_string()
    }

    impl tilolang_runtime::blob::BlobHost for Device {
        fn read(&self, hash: &str) -> Option<Vec<u8>> {
            self.files
                .borrow()
                .iter()
                .find(|(held, _)| held == hash)
                .map(|(_, bytes)| bytes.clone())
        }

        fn choose(
            &self,
            _request: &tilolang_runtime::blob::FileRequest,
        ) -> Result<Vec<tilolang_runtime::blob::FileValue>, String> {
            Ok(Vec::new())
        }

        fn save(
            &self,
            _request: &tilolang_runtime::blob::SaveRequest,
            _content: &[u8],
        ) -> Result<(), String> {
            Err("this test writes no files out".to_owned())
        }

        fn keep(
            &self,
            name: &str,
            media: &str,
            bytes: &[u8],
        ) -> Result<tilolang_runtime::blob::FileValue, String> {
            let hash = format!("h{}", bytes.len());
            self.files.borrow_mut().push((hash.clone(), bytes.to_vec()));
            Ok(FileValue::new(
                name,
                media,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash,
            ))
        }
    }

    struct Feeds {
        rt: Runtime,
        app: App,
        lib: ResourceRef,
        /// The person's reading here.
        reader: ResourceRef,
        device: Device,
    }

    impl Feeds {
        fn new() -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let device = Device::default();
            rt.set_engine(device.clone());
            rt.set_blobs(device.clone());
            rt.set_clock(|| "2026-11-24T09:00:00Z".to_owned());
            let app = App::compile(SOURCE, &[]);
            authority.trust(&app.load(&rt));
            let lib = rt.create_root("Library").unwrap();
            let mut feeds = Self {
                rt,
                app,
                lib: lib.clone(),
                reader: lib,
                device,
            };
            // The person starts reading here: their place, size, face and paper.
            feeds.reader =
                returned_ref(feeds.call("begin_reading", &[("lib", Value::resource(&feeds.lib))]));
            feeds
        }

        fn call(&self, action: &str, given: &[(&str, Value)]) -> InvokeOutcome {
            let id = self.app.action(action);
            let args = self.app.args(&self.rt, action, given);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|error| panic!("{action}: {error:?}"))
        }

        fn subscribe(&self, url: &str) -> ResourceRef {
            returned_ref(self.call(
                "add_subscription",
                &[
                    ("lib", Value::resource(&self.lib)),
                    ("url", Value::text(url)),
                ],
            ))
        }

        /// Reads every subscription and takes what came back, as the window does each second.
        /// A subscription just added starts its own read, so a press only starts one when none
        /// runs; either way what the device answers is taken.
        fn refresh(&self) {
            if self.refresh_field("state") != json!("running") {
                self.call("refresh", &[("lib", Value::resource(&self.lib))]);
            }
            for _ in 0..3 {
                self.call("poll_refresh", &[("lib", Value::resource(&self.lib))]);
            }
        }

        fn window(&self, view: &str, args: Vec<Value>) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(&me(), &self.app.view(view), args)
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            window
        }

        fn feeds_window(&self) -> ViewInstance {
            self.window("feeds", vec![Value::resource(&self.lib)])
        }

        fn page(&self, article: &ResourceRef) -> ViewInstance {
            self.window("article_page", vec![Value::resource(article)])
        }

        /// The subscription the window's sidebar lists for `url`.
        fn feed(&self, url: &str) -> ResourceRef {
            let window = self.feeds_window();
            of_kind(window.tree(), "ui.NavItem")
                .into_iter()
                .filter_map(|item| {
                    item.props.get("item").and_then(|item| match item {
                        Value::Ref(r) => Some(ResourceRef::clone(&r.target)),
                        _ => None,
                    })
                })
                .find(|feed| self.field(feed, "url") == json!(url))
                .unwrap_or_else(|| panic!("no subscription to {url} in the sidebar"))
        }

        fn feed_field(&self, url: &str, field: &str) -> Json {
            self.field(&self.feed(url), field)
        }

        /// The article the list shows as `title`.
        fn article(&self, title: &str) -> ResourceRef {
            let window = self.feeds_window();
            of_kind(window.tree(), "ui.ListItem")
                .into_iter()
                .map(row_item)
                .find(|article| self.field(article, "title") == json!(title))
                .unwrap_or_else(|| panic!("no article called {title} in the list"))
        }

        /// The articles the list shows, by title.
        fn article_titles(&self) -> Vec<String> {
            let window = self.feeds_window();
            of_kind(window.tree(), "ui.ListItem")
                .into_iter()
                .map(row_item)
                .map(|article| match self.field(&article, "title") {
                    Json::String(title) => title,
                    other => panic!("an article's title is {other:?}"),
                })
                .collect()
        }

        /// A resource's field, as the store projects it.
        fn field(&self, at: &ResourceRef, field: &str) -> Json {
            self.rt.project(&Value::resource(at))["fields"][field].clone()
        }

        /// What the library's refresh record says of `field`.
        fn refresh_field(&self, field: &str) -> Json {
            let record = self.rt.read(&self.lib, "refresh").unwrap();
            let projected = self.rt.project(&record);
            projected["fields"][field].clone()
        }

        fn reader(&self) -> ResourceRef {
            ResourceRef::clone(&self.reader)
        }
    }

    /// The text of a node's property, whatever option carries it.
    fn text(node: &tilolang_runtime::UiNode, name: &str) -> String {
        match node.props.get(name) {
            Some(Value::Text(text)) => text.clone(),
            Some(Value::Opt(Some(held))) => match &**held {
                Value::Text(text) => text.clone(),
                other => panic!("{name} of {} is {other:?}", node.kind),
            },
            other => panic!("{name} of {} is {other:?}", node.kind),
        }
    }

    /// Every text the window shows, under `node`.
    fn shown(view: &ViewInstance) -> Vec<String> {
        fn walk(node: &tilolang_runtime::UiNode, out: &mut Vec<String>) {
            for value in node.props.values() {
                match value {
                    Value::Text(text) => out.push(text.clone()),
                    Value::Opt(Some(held)) => {
                        if let Value::Text(text) = &**held {
                            out.push(text.clone());
                        }
                    }
                    _ => {}
                }
            }
            for child in &node.children {
                walk(child, out);
            }
        }
        let mut out = Vec::new();
        walk(view.tree(), &mut out);
        out
    }

    fn number(node: &tilolang_runtime::UiNode, name: &str) -> f64 {
        match node.props.get(name) {
            Some(Value::Float(n)) => *n,
            Some(Value::Opt(Some(held))) => match &**held {
                Value::Float(n) => *n,
                other => panic!("{name} of {} is {other:?}", node.kind),
            },
            other => panic!("{name} of {} is {other:?}", node.kind),
        }
    }

    /// The item a row or a place shows.
    fn row_item(node: &tilolang_runtime::UiNode) -> ResourceRef {
        match node.props.get("item") {
            Some(Value::Ref(r)) => ResourceRef::clone(&r.target),
            other => panic!("a row's item is {other:?}"),
        }
    }

    #[test]
    fn a_subscription_is_named_by_its_site_until_a_refresh_brings_its_title() {
        let feeds = Feeds::new();
        let feed = feeds.subscribe(GUARDIAN);
        assert_eq!(feeds.field(&feed, "name"), json!("theguardian.com"));
        assert_eq!(feeds.field(&feed, "url"), json!(GUARDIAN));
        assert_eq!(feeds.field(&feed, "format"), json!({"variant": "rss"}));
        let capsule = feeds.subscribe(CAPSULE);
        assert_eq!(
            feeds.field(&capsule, "format"),
            json!({"variant": "gemini"})
        );

        // Subscribing twice shows the subscription the library already has.
        assert_eq!(
            feeds.subscribe(GUARDIAN).id_string(),
            feeds.feed(GUARDIAN).id_string(),
            "one subscription per address"
        );
    }

    #[test]
    fn a_refresh_asks_the_device_once_and_lands_each_answer_it_takes() {
        let feeds = Feeds::new();
        feeds.subscribe(GUARDIAN);
        feeds.refresh();
        assert_eq!(feeds.device.ops(), ["refresh", "take"]);
        let asked: Json = serde_json::from_str(&feeds.device.inputs()[0]).unwrap();
        assert_eq!(asked["feeds"][0]["url"], json!(GUARDIAN));
        assert_eq!(asked["feeds"][0]["pin"], json!(""), "nothing is pinned yet");

        assert_eq!(
            feeds.feed_field(GUARDIAN, "name"),
            json!("The Guardian: World"),
            "what the feed says of itself names it"
        );
        let mut titles = feeds.article_titles();
        titles.sort();
        assert_eq!(titles, ["A quiet morning", "A second piece"]);
        assert_eq!(feeds.refresh_field("fresh"), json!(2));
        assert_eq!(feeds.refresh_field("state"), json!("done"));

        // The window says what the refresh brought.
        let window = feeds.feeds_window();
        let banner = of_kind(window.tree(), "ui.Banner")
            .into_iter()
            .next()
            .expect("a banner over the list");
        assert_eq!(text(banner, "title"), "2 new articles");

        // A second refresh brings nothing twice: the articles it lists are already kept, and
        // the window says the subscriptions have nothing newer.
        feeds.refresh();
        assert_eq!(feeds.article_titles().len(), 2);
        let window = feeds.feeds_window();
        assert!(
            of_kind(window.tree(), "ui.Banner").is_empty(),
            "nothing new, no banner: {:?}",
            shown(&window)
        );
        assert!(
            shown(&window)
                .iter()
                .any(|text| text == "All feeds are up to date"),
            "{:?}",
            shown(&window)
        );
    }

    #[test]
    fn a_site_that_refuses_says_why_and_a_capsule_pins_its_certificate() {
        let feeds = Feeds::new();
        feeds.subscribe(BROKEN);
        feeds.subscribe(CAPSULE);
        feeds.refresh();
        assert_eq!(
            feeds.feed_field(BROKEN, "trouble"),
            json!("The site refused the request (HTTP 403)")
        );
        assert_eq!(feeds.refresh_field("failed"), json!(1));
        assert_eq!(
            feeds.feed_field(CAPSULE, "pin"),
            json!("abc123"),
            "the certificate is pinned"
        );
        assert_eq!(feeds.feed_field(CAPSULE, "trouble"), json!(""));

        // The window says which subscriptions failed and why, in a banner over the list.
        let window = feeds.feeds_window();
        let banner = of_kind(window.tree(), "ui.Banner")
            .into_iter()
            .next()
            .expect("a banner over the list");
        assert!(
            text(banner, "message")
                .contains("broken.test: The site refused the request (HTTP 403)"),
            "{}",
            text(banner, "message")
        );
    }

    #[test]
    fn opening_an_article_reads_it_and_its_marks_and_tags_are_the_persons() {
        let feeds = Feeds::new();
        feeds.subscribe(GUARDIAN);
        feeds.refresh();
        let article = feeds.article("A quiet morning");
        assert_eq!(feeds.field(&article, "read"), json!(false));

        // Opening it (what every path does) reads it.
        let opened = feeds
            .rt
            .opened_action(&article.ty)
            .expect("the type reads on open");
        feeds
            .rt
            .invoke_action(&me(), (opened.as_str(), vec![Value::resource(&article)]))
            .unwrap();
        assert_eq!(feeds.field(&article, "read"), json!(true));

        feeds.call(
            "set_read",
            &[
                ("a", Value::resource(&article)),
                ("read", Value::Bool(false)),
            ],
        );
        assert_eq!(feeds.field(&article, "read"), json!(false));

        feeds.call("star", &[("a", Value::resource(&article))]);
        assert_eq!(feeds.field(&article, "starred"), json!(true));

        feeds.call(
            "set_tags",
            &[
                ("a", Value::resource(&article)),
                ("tags", Value::text("  #Design, design , Later ")),
            ],
        );
        let mut tags: Vec<String> = match feeds.rt.read(&article, "tags").unwrap() {
            Value::List(items) => items
                .iter()
                .filter_map(|item| item.as_text().map(str::to_owned))
                .collect(),
            other => panic!("the tags read {other:?}"),
        };
        tags.sort();
        assert_eq!(
            tags,
            ["design", "later"],
            "trimmed, lower case, without `#`"
        );
    }

    #[test]
    fn marking_a_place_read_reads_what_the_reader_is_looking_at() {
        let feeds = Feeds::new();
        feeds.subscribe(GUARDIAN);
        feeds.subscribe(LWN);
        feeds.refresh();
        // The reader looks at one subscription's place.
        let guardian = feeds.feed(GUARDIAN);
        feeds.call(
            "show_place",
            &[
                ("reader", Value::resource(&feeds.reader())),
                (
                    "place",
                    Value::text(format!("feed:{}", guardian.id_string())),
                ),
            ],
        );
        feeds.call("mark_read", &[("lib", Value::resource(&feeds.lib))]);
        assert_eq!(
            feeds.field(&feeds.article("A quiet morning"), "read"),
            json!(true)
        );
        assert_eq!(
            feeds.field(&feeds.article("A second piece"), "read"),
            json!(true)
        );
    }

    #[test]
    fn unsubscribing_takes_its_articles_with_it() {
        let feeds = Feeds::new();
        feeds.subscribe(GUARDIAN);
        feeds.refresh();
        let guardian = feeds.feed(GUARDIAN);
        feeds.call(
            "unsubscribe",
            &[
                ("lib", Value::resource(&feeds.lib)),
                ("feed", Value::resource(&guardian)),
            ],
        );
        assert!(feeds.article_titles().is_empty());
        let project = feeds
            .rt
            .project(&feeds.rt.read(&feeds.lib, "feeds").unwrap());
        assert_eq!(project["items"].as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn an_opml_file_subscribes_what_it_lists_and_the_library_writes_one_back() {
        let feeds = Feeds::new();
        let file = feeds.device.file("feeds.opml", "<opml version=\"2.0\"/>");
        feeds.call(
            "import_opml",
            &[("lib", Value::resource(&feeds.lib)), ("file", file)],
        );
        assert_eq!(feeds.feed_field(LWN, "name"), json!("LWN"));
        assert_eq!(
            feeds.feed_field("https://bare.test/feed", "name"),
            json!("bare.test"),
            "an address that names no title is named by its site"
        );
        let folders = feeds
            .rt
            .project(&feeds.rt.read(&feeds.lib, "folders").unwrap());
        assert!(
            folders.to_string().contains("Reading"),
            "the folder the file names is made"
        );

        feeds.call("export_opml", &[("lib", Value::resource(&feeds.lib))]);
        let export = feeds
            .device
            .inputs()
            .into_iter()
            .filter(|input| input.contains("\"feeds\""))
            .next_back()
            .expect("the export names the subscriptions");
        assert!(
            export.contains("LWN") && export.contains("lwn.net"),
            "{export}"
        );
    }

    #[test]
    fn the_window_shows_its_places_rows_and_the_article_in_the_reading_chosen() {
        let feeds = Feeds::new();
        feeds.subscribe(GUARDIAN);
        feeds.subscribe(CAPSULE);
        feeds.refresh();
        let window = feeds.feeds_window();
        let places: Vec<String> = of_kind(window.tree(), "ui.NavItem")
            .into_iter()
            .map(|item| text(item, "label"))
            .collect();
        assert!(places.contains(&"All articles".to_owned()), "{places:?}");
        assert!(places.contains(&"Unread".to_owned()), "{places:?}");
        assert!(
            places.contains(&"The Guardian: World".to_owned()),
            "{places:?}"
        );

        // A subscription just added is read and shown, so the window stands on it.
        let rows: Vec<ResourceRef> = of_kind(window.tree(), "ui.ListItem")
            .into_iter()
            .map(row_item)
            .collect();
        assert_eq!(
            rows.len(),
            1,
            "the capsule's own article, in the place it opened"
        );

        feeds.call(
            "show_place",
            &[
                ("reader", Value::resource(&feeds.reader())),
                ("place", Value::text("all")),
            ],
        );
        let all = feeds.feeds_window();
        let rows: Vec<ResourceRef> = of_kind(all.tree(), "ui.ListItem")
            .into_iter()
            .map(row_item)
            .collect();
        assert_eq!(rows.len(), 3, "two articles and the capsule's");

        // The article's page is set in the reader's size, face and paper.
        let article = feeds.article("A quiet morning");
        let page = feeds.page(&article);
        let reading = of_kind(page.tree(), "ui.Reading")
            .into_iter()
            .next()
            .expect("the article's body");
        assert_eq!(text(reading, "face"), "serif");
        assert!((number(reading, "scale") - 1.0).abs() < f64::EPSILON);
        assert!(
            matches!(reading.props.get("paper"), None | Some(Value::Opt(None))),
            "{:?}",
            reading.props.get("paper")
        );

        let reader = feeds.reader();
        feeds.call(
            "set_face",
            &[
                ("reader", Value::resource(&reader)),
                ("face", Value::text("mono")),
            ],
        );
        feeds.call(
            "set_paper",
            &[
                ("reader", Value::resource(&reader)),
                ("paper", Value::text("sepia")),
            ],
        );
        feeds.call(
            "set_scale",
            &[
                ("reader", Value::resource(&reader)),
                ("scale", Value::Float(1.4)),
            ],
        );
        let page = feeds.page(&article);
        let reading = of_kind(page.tree(), "ui.Reading")
            .into_iter()
            .next()
            .expect("the article's body");
        assert_eq!(text(reading, "face"), "mono");
        assert!((number(reading, "scale") - 1.4).abs() < 0.001);
        assert_eq!(text(reading, "paper"), "#f4ecdc", "the sepia page");

        // A search hit (or an agent's open) shows the article's feed in the window's places.
        feeds.call("show_article", &[("a", Value::resource(&article))]);
        let guardian = feeds.feed(GUARDIAN);
        assert_eq!(
            feeds.field(&feeds.reader(), "place"),
            json!(format!("feed:{}", guardian.id_string()))
        );

        // A tag's place shows the articles that carry it.
        feeds.call(
            "set_tags",
            &[
                ("a", Value::resource(&article)),
                ("tags", Value::text("morning")),
            ],
        );
        feeds.call(
            "show_tag",
            &[
                ("reader", Value::resource(&feeds.reader())),
                ("tag", Value::text("morning")),
            ],
        );
        let window = feeds.feeds_window();
        let rows: Vec<ResourceRef> = of_kind(window.tree(), "ui.ListItem")
            .into_iter()
            .map(row_item)
            .collect();
        assert_eq!(rows.len(), 1, "only the tagged article");
        assert_eq!(feeds.field(&feeds.reader(), "place"), json!("tag:morning"));
    }

    #[test]
    fn an_empty_library_says_so_once_and_points_at_the_first_subscription() {
        let feeds = Feeds::new();
        let window = feeds.feeds_window();
        let empty = of_kind(window.tree(), "ui.EmptyState")
            .into_iter()
            .next()
            .expect("the window's one empty state");
        assert_eq!(text(empty, "title"), "No feeds yet");
        assert_eq!(text(empty, "action"), "Add feed");
    }
}
