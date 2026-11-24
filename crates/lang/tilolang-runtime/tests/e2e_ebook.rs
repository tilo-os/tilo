//! The books app (`apps/tl/ebook/app.tl`) end to end: an EPUB opened from a file into its
//! chapters, the passages a reader marks, the palettes and the page's setting, a search of
//! Project Gutenberg and a book downloaded from it, and the library's and a book's windows.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use tilolang_runtime::run::WebRequest;
    use tilolang_runtime::web::{WebReply, WebTransport};
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, UiNode, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/ebook/app.tl");

    /// The book the tests open, as base64 text the repository may hold: three chapters, its own
    /// title, author and language.
    const TIDEGLASS_BASE64: &str = include_str!("fixtures/tideglass.epub.b64");

    /// The session app, as far as the books app needs it: opening a resource's window. A tilolang
    /// package cannot name an action `open` (a built-in's name is taken), so the stand-in
    /// declares `open_window` and is published under the name the session's native surface uses.
    const SESSION_SOURCE: &str = r#"app so.tilo.session "Session"
action open_window(resource: host.Resource) {
}
"#;

    /// Another book a watched folder holds, as base64 text.
    const SALTROAD_BASE64: &str = include_str!("fixtures/saltroad.epub.b64");

    /// [`SALTROAD_BASE64`] as the EPUB bytes a device holds.
    fn saltroad() -> Vec<u8> {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(SALTROAD_BASE64.trim())
            .expect("the fixture is base64")
    }

    /// [`TIDEGLASS_BASE64`] as the EPUB bytes a device holds.
    fn tideglass() -> Vec<u8> {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(TIDEGLASS_BASE64.trim())
            .expect("the fixture is base64")
    }

    /// The answer Project Gutenberg's search gives for one book, as gutendex answers it.
    const SEARCH: &str = r#"{"count":1,"results":[{"title":"The Tide Glass","authors":[{"name":"Ada Ferrer"}],"languages":["en"],"formats":{"application/epub+zip":"https://www.gutenberg.org/ebooks/1.epub"}}]}"#;

    /// What a device's content store and network said: the files it keeps, and the requests it
    /// answered.
    #[derive(Clone, Default)]
    struct Device {
        files: Rc<RefCell<std::collections::HashMap<String, Vec<u8>>>>,
        requests: Rc<RefCell<Vec<WebRequest>>>,
        /// The device's disk: each path it holds, and whether it is a folder.
        disk: Rc<RefCell<std::collections::BTreeMap<String, (bool, Vec<u8>)>>>,
        /// The folders the app asked to watch, by path, with the action each runs.
        watches: Rc<RefCell<Vec<(String, String)>>>,
    }

    impl Device {
        /// The file `name` of `bytes`, kept in this device's store.
        fn keeping(
            &self,
            name: &str,
            media: &str,
            bytes: &[u8],
        ) -> tilolang_runtime::blob::FileValue {
            let hash = digest_of(bytes);
            self.files.borrow_mut().insert(hash.clone(), bytes.to_vec());
            tilolang_runtime::blob::FileValue::new(
                name,
                media,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash,
            )
        }

        /// Put the file `name` in the folder `folder`, keeping the folder itself.
        fn put(&self, folder: &str, name: &str, bytes: &[u8]) {
            let mut disk = self.disk.borrow_mut();
            disk.insert(folder.to_owned(), (true, Vec::new()));
            disk.insert(format!("{folder}/{name}"), (false, bytes.to_vec()));
        }

        fn watches(&self) -> Vec<(String, String)> {
            self.watches.borrow().clone()
        }

        fn requests(&self) -> Vec<String> {
            self.requests
                .borrow()
                .iter()
                .map(|request| request.url.clone())
                .collect()
        }
    }

    /// The address a content store names `bytes` by.
    fn digest_of(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        let mut digest = 0xcbf2_9ce4_8422_2325_u64;
        for byte in bytes {
            digest = (digest ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        let mut hex = String::with_capacity(17);
        let _ = write!(hex, "h{digest:016x}");
        hex
    }

    impl tilolang_runtime::blob::BlobHost for Device {
        fn read(&self, hash: &str) -> Option<Vec<u8>> {
            self.files.borrow().get(hash).cloned()
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
            Ok(self.keeping(name, media, bytes))
        }
    }

    impl tilolang_runtime::files::FilesHost for Device {
        fn list(&self, path: &str) -> Result<Vec<tilolang_runtime::files::Entry>, String> {
            let disk = self.disk.borrow();
            let Some((folder, _)) = disk.get(path) else {
                return Err(format!("`{path}` is not a folder on this device"));
            };
            if !folder {
                return Err(format!("`{path}` is not a folder"));
            }
            let prefix = format!("{path}/");
            Ok(disk
                .iter()
                .filter_map(|(at, (is_folder, bytes))| {
                    let name = at.strip_prefix(&prefix)?;
                    if name.contains('/') {
                        return None;
                    }
                    Some(tilolang_runtime::files::Entry {
                        name: name.to_owned(),
                        path: at.clone(),
                        folder: *is_folder,
                        size: i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                        modified: 0,
                        hidden: name.starts_with('.'),
                    })
                })
                .collect())
        }

        fn stat(&self, path: &str) -> Result<Option<tilolang_runtime::files::Entry>, String> {
            let entry = self
                .list(path.rsplit_once('/').map_or("", |(folder, _)| folder))
                .unwrap_or_default()
                .into_iter()
                .find(|entry| entry.path == path);
            Ok(entry)
        }

        fn read(&self, path: &str) -> Result<Vec<u8>, String> {
            let disk = self.disk.borrow();
            match disk.get(path) {
                Some((false, bytes)) => Ok(bytes.clone()),
                Some((true, _)) => Err(format!("`{path}` is a folder")),
                None => Err(format!("`{path}` is not on this device")),
            }
        }

        fn watch(&self, path: &str, run: &str) -> Result<String, String> {
            self.watches
                .borrow_mut()
                .push((path.to_owned(), run.to_owned()));
            Ok(format!("watch:{path}"))
        }
    }

    impl tilolang_runtime::fonts::FontHost for Device {
        fn families(&self) -> Vec<String> {
            vec!["Our Serif".to_owned(), "Our Sans".to_owned()]
        }
    }

    impl WebTransport for Device {
        fn fetch(&self, request: &WebRequest) -> WebReply {
            self.requests.borrow_mut().push(request.clone());
            if request.url.contains("gutendex.com") {
                return WebReply {
                    status: 200,
                    status_text: "OK".to_owned(),
                    headers: Vec::new(),
                    body: SEARCH.as_bytes().to_vec(),
                };
            }
            WebReply {
                status: 200,
                status_text: "OK".to_owned(),
                headers: Vec::new(),
                body: tideglass(),
            }
        }
    }

    struct Books {
        rt: Runtime,
        app: App,
        lib: ResourceRef,
        device: Device,
    }

    impl Books {
        fn new() -> Self {
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            let device = Device::default();
            rt.set_blobs(device.clone());
            rt.set_web(device.clone());
            rt.set_fonts(device.clone());
            rt.set_device_files(device.clone());
            rt.set_clock(|| "2026-11-23T09:00:00Z".to_owned());
            let mut session = App::compile(SESSION_SOURCE, &[]);
            for action in &mut session.compiled.meta.actions {
                if action.name == "open_window" {
                    "open".clone_into(&mut action.name);
                }
            }
            session.load(&rt);
            let app = App::compile(SOURCE, &[session.meta().clone()]);
            app.load(&rt);
            let lib = rt.create_root("Library").unwrap();
            let books = Self {
                rt,
                app,
                lib,
                device,
            };
            // What the app's library starts with: the palettes a reader knows.
            books.call("fill", vec![Value::resource(&books.lib)]);
            books
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|error| panic!("{action}: {error:?}"))
        }

        fn args(&self, action: &str, given: &[(&str, Value)]) -> Vec<Value> {
            // The reader is the person the window is shown to: a test is that person, so an
            // action taking `who` is given them unless the test says otherwise.
            let mut given = given.to_vec();
            let takes_who = self
                .app
                .meta()
                .actions
                .iter()
                .find(|a| a.name == action)
                .is_some_and(|a| a.params.iter().any(|p| p.name == "who"));
            if takes_who && !given.iter().any(|(name, _)| *name == "who") {
                given.push(("who", Value::Actor(me())));
            }
            self.app.args(&self.rt, action, &given)
        }

        /// The EPUB kept in this device's store as the file `name`.
        fn epub(&self, name: &str) -> Value {
            Value::file(
                self.device
                    .keeping(name, "application/epub+zip", &tideglass()),
            )
        }

        fn open(&self) -> ResourceRef {
            returned_ref(self.call(
                "open_book",
                self.args(
                    "open_book",
                    &[
                        ("lib", Value::resource(&self.lib)),
                        ("file", self.epub("The Tide Glass.epub")),
                    ],
                ),
            ))
        }

        fn said(&self, at: &ResourceRef, field: &str) -> String {
            match self.rt.read(at, field).unwrap() {
                Value::Text(text) => text,
                other => panic!("{field} reads {other:?}"),
            }
        }

        /// What the person reading `book` is at, as their own progress record says: the field
        /// `chapter`, `block`, `page` or `place`.
        fn progress(&self, book: &ResourceRef, field: &str) -> serde_json::Value {
            let reg = self.rt.read(book, "progress").unwrap();
            let project = self.rt.project(&reg);
            project["items"][0]["fields"][field].clone()
        }

        fn window(&self, view: &str, args: Vec<Value>) -> ViewInstance {
            self.rt
                .open_view(&me(), &self.app.view(view), args)
                .unwrap()
        }

        fn book_window(&self, book: &ResourceRef) -> ViewInstance {
            self.window("book_page", vec![Value::resource(book)])
        }

        /// The books on the shelf, as their cards stand for them.
        fn opened_books(&self) -> Vec<ResourceRef> {
            let window = self.window("library", vec![Value::resource(&self.lib)]);
            of_kind(window.tree(), "ui.MediaCard")
                .iter()
                .filter_map(|card| match card.props.get("item") {
                    Some(Value::Ref(r)) => Some(ResourceRef::clone(&r.target)),
                    _ => None,
                })
                .collect()
        }

        /// The one book an online search found, as its row stands for it.
        fn found_online(&self) -> ResourceRef {
            let found = self.rt.read(&self.lib, "found").unwrap();
            let window = self.window("found_books", vec![Value::resource(&self.lib), found]);
            of_kind(window.tree(), "ui.ListRow")
                .iter()
                .find_map(|row| match row.props.get("item") {
                    Some(Value::Ref(r)) => Some(ResourceRef::clone(&r.target)),
                    _ => None,
                })
                .expect("a found book's row stands for it")
        }

        /// What the one mark of `book` says of `field`, as the store projects it.
        fn mark_field(&self, book: &ResourceRef, field: &str) -> serde_json::Value {
            let marks = self.rt.read(book, "marks").unwrap();
            self.rt.project(&marks)["items"][0]["fields"][field].clone()
        }

        /// The one mark of `book`'s open chapter, as its row stands for it.
        fn mark_of(&self, book: &ResourceRef) -> ResourceRef {
            let window = self.book_window(book);
            of_kind(window.tree(), "ui.ListRow")
                .iter()
                .filter(|row| matches!(row.props.get("item"), Some(Value::Ref(_))))
                .find_map(|row| match row.props.get("item") {
                    Some(Value::Ref(r)) => Some(ResourceRef::clone(&r.target)),
                    _ => None,
                })
                .expect("a mark's row stands for it")
        }

        /// A chapter of `book`, by the place its contents give it.
        fn chapter(&self, book: &ResourceRef, at: i64) -> ResourceRef {
            let window = self.book_window(book);
            of_kind(window.tree(), "ui.NavItem")
                .get(usize::try_from(at).unwrap_or(0))
                .and_then(|item| match item.props.get("item") {
                    Some(Value::Ref(r)) => Some(ResourceRef::clone(&r.target)),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no chapter {at} in the contents"))
        }
    }

    /// A count an option prop holds, however the prop carries it.
    fn number_prop(node: &UiNode, name: &str) -> Option<i64> {
        match node.props.get(name) {
            Some(Value::Int(n)) => Some(*n),
            Some(Value::Opt(Some(held))) => match &**held {
                Value::Int(n) => Some(*n),
                _ => None,
            },
            _ => None,
        }
    }

    /// A yes-or-no prop, however the tree carries it: a `Bool`, an `Int` flag, or either
    /// wrapped in an option.
    fn flag_prop(node: &UiNode, name: &str) -> Option<bool> {
        let value = match node.props.get(name) {
            Some(Value::Opt(Some(held))) => &**held,
            Some(value) => value,
            None => return None,
        };
        match value {
            Value::Bool(flag) => Some(*flag),
            Value::Int(n) => Some(*n != 0),
            _ => None,
        }
    }

    fn prop(node: &UiNode, name: &str) -> String {
        match node.props.get(name) {
            Some(Value::Text(text)) => text.clone(),
            Some(Value::Float(n)) => n.to_string(),
            other => panic!("{name} of {} is {other:?}", node.kind),
        }
    }

    tilo_test_env::scenario! {
        fn a_book_opens_from_its_epub_and_is_read_a_chapter_at_a_time;

        phase opening_an_epub_gives_the_book_its_facts_and_chapters;
        {
            let books = Books::new();
            let book = books.open();
            assert_eq!(books.said(&book, "title"), "The Tide Glass");
            assert_eq!(books.said(&book, "author"), "Ada Ferrer");
            assert_eq!(books.said(&book, "subject"), "Fable");
            assert_eq!(books.said(&book, "language"), "en");
            assert_eq!(
                books.said(&book, "source"),
                format!("tilo-blob://{}", digest_of(&tideglass())),
                "the book is the file's content"
            );
            let window = books.book_window(&book);
            let contents: Vec<String> = of_kind(window.tree(), "ui.NavItem")
                .iter()
                .map(|item| text_prop(item, "label"))
                .collect();
            assert_eq!(
                contents,
                ["The Glass", "The Tide", "The Keepers"],
                "the contents are the package's spine, by its own labels"
            );
            let reading = of_kind(window.tree(), "ui.Reading");
            assert_eq!(reading.len(), 1, "the book's window holds one reading page");
            let opens = books
                .app
                .meta()
                .actions
                .iter()
                .find(|action| action.name == "open_book")
                .map(|action| action.opens.clone())
                .unwrap_or_default();
            assert!(
                opens.iter().any(|kind| kind == "application/epub+zip")
                    && opens.iter().any(|kind| kind == "epub"),
                "the app declares the EPUB files it opens: {opens:?}"
            );
        }

        phase the_same_file_opened_twice_is_one_book;
        {
            let books = Books::new();
            let first = books.open();
            let again = books.open();
            assert_eq!(
                (first.doc.clone(), &first.item),
                (again.doc.clone(), &again.item),
                "the file's content names one book"
            );
            assert_eq!(
                books.rt.project(&books.rt.read(&books.lib, "books").unwrap())["count"],
                serde_json::json!(1)
            );
        }

        phase a_watched_folder_gives_its_epubs_as_books;
        {
            let books = Books::new();
            books.device.put("books", "The Salt Road.epub", &saltroad());
            books.device.put("books", "notes.txt", b"salt on the road, and carts");
            books.call(
                "watch_folder",
                books.args(
                    "watch_folder",
                    &[("lib", Value::resource(&books.lib)), ("path", Value::text("books"))],
                ),
            );
            let shelf = books.rt.project(&books.rt.read(&books.lib, "books").unwrap());
            assert_eq!(
                shelf["count"],
                serde_json::json!(1),
                "the folder's EPUB becomes a book, and its text file is no book"
            );
            let folders = books.rt.project(&books.rt.read(&books.lib, "folders").unwrap());
            assert_eq!(folders["count"], serde_json::json!(1));
            let watches = books.device.watches();
            assert!(!watches.is_empty(), "the platform keeps the watch");
            assert!(
                watches
                    .iter()
                    .all(|(path, run)| path == "books" && run == "foldered"),
                "the watch names the folder and the library's scan: {watches:?}"
            );
            // A book arrives in the folder: the platform runs the watched action.
            books.device.put("books", "The Tide Glass.epub", &tideglass());
            books.call(
                "foldered",
                books.args("foldered", &[("lib", Value::resource(&books.lib))]),
            );
            let shelf = books.rt.project(&books.rt.read(&books.lib, "books").unwrap());
            assert_eq!(
                shelf["count"],
                serde_json::json!(2),
                "what arrived is a book too, and the one already read is not read again"
            );
        }

        phase a_chapter_is_opened_from_the_contents;
        {
            let books = Books::new();
            let book = books.open();
            let second = {
                let contents = books.book_window(&book);
                let ids: Vec<String> = of_kind(contents.tree(), "ui.NavItem")
                    .iter()
                    .map(|node| node.key.clone())
                    .collect();
                ids
            };
            assert_eq!(second.len(), 3);
            books.call(
                "show_passage",
                books.args(
                    "show_passage",
                    &[
                        ("b", Value::resource(&book)),
                        ("who", Value::Actor(me())),
                        ("chapter", Value::Int(1)),
                        ("block", Value::Int(2)),
                    ],
                ),
            );
            assert_eq!(books.progress(&book, "chapter"), serde_json::json!(1));
            assert_eq!(
                books.progress(&book, "block"),
                serde_json::json!(2),
                "the link keeps the block it asks for"
            );
        }

        phase a_book_is_dropped_on_the_shelf_and_a_photo_is_not;
        {
            let books = Books::new();
            let book = Value::file(books.device.keeping("The Tide Glass.epub", "application/epub+zip", &tideglass()));
            let picture = Value::file(books.device.keeping("cover.png", "image/png", b"png"));
            books.call(
                "drop_books",
                books.args(
                    "drop_books",
                    &[("lib", Value::resource(&books.lib)), ("files", Value::List(vec![book, picture]))],
                ),
            );
            assert_eq!(
                books.rt.project(&books.rt.read(&books.lib, "books").unwrap())["count"],
                serde_json::json!(1),
                "the EPUB is added and the picture is not"
            );
        }

        phase a_passage_is_highlighted_with_its_words;
        {
            let books = Books::new();
            let book = books.open();
            books.call(
                "highlight",
                books.args(
                    "highlight",
                    &[
                        ("b", Value::resource(&book)),
                        ("chapter", Value::Int(0)),
                        (
                            "ranges",
                            Value::List(vec![Value::Int(1), Value::Int(0), Value::Int(10)]),
                        ),
                        ("color", Value::text("#f5d76e")),
                    ],
                ),
            );
            let marks = books.rt.project(&books.rt.read(&book, "marks").unwrap());
            assert_eq!(marks["count"], serde_json::json!(1));
            let window = books.book_window(&book);
            let reading = of_kind(window.tree(), "ui.Reading")[0];
            let highlights = match reading.props.get("highlights") {
                Some(Value::List(items)) => items
                    .iter()
                    .map(|item| match item {
                        Value::Text(text) => text.clone(),
                        other => panic!("a highlight is text, not {other:?}"),
                    })
                    .collect::<Vec<_>>(),
                other => panic!("highlights read {other:?}"),
            };
            assert_eq!(
                highlights,
                ["1:0:10:#f5d76e"],
                "the page bands the passage the mark anchors"
            );
        }

        phase a_passage_across_blocks_is_one_mark;
        {
            let books = Books::new();
            let book = books.open();
            books.call(
                "highlight",
                books.args(
                    "highlight",
                    &[
                        ("b", Value::resource(&book)),
                        ("chapter", Value::Int(0)),
                        (
                            "ranges",
                            Value::List(vec![
                                Value::Int(1),
                                Value::Int(0),
                                Value::Int(10),
                                Value::Int(2),
                                Value::Int(0),
                                Value::Int(5),
                            ]),
                        ),
                        ("color", Value::text("#f5d76e")),
                    ],
                ),
            );
            assert_eq!(
                books.rt.project(&books.rt.read(&book, "marks").unwrap())["count"],
                serde_json::json!(1),
                "a passage crossing blocks is one mark"
            );
            let excerpt = books.mark_field(&book, "excerpt");
            assert!(
                excerpt
                    .as_str()
                    .is_some_and(|words| words.contains(" … ")),
                "its words name both blocks: {excerpt}"
            );
            let window = books.book_window(&book);
            let reading = of_kind(window.tree(), "ui.Reading")[0];
            match reading.props.get("highlights") {
                Some(Value::List(items)) => assert_eq!(items.len(), 2, "a band for each block"),
                other => panic!("highlights read {other:?}"),
            }
        }

        phase a_note_and_a_comment_stand_on_their_passages;
        {
            let books = Books::new();
            let book = books.open();
            books.call(
                "highlight",
                books.args(
                    "highlight",
                    &[
                        ("b", Value::resource(&book)),
                        ("chapter", Value::Int(0)),
                        (
                            "ranges",
                            Value::List(vec![Value::Int(1), Value::Int(0), Value::Int(6)]),
                        ),
                        ("color", Value::text("#f5d76e")),
                    ],
                ),
            );
            books.call(
                "add_note",
                books.args(
                    "add_note",
                    &[
                        ("b", Value::resource(&book)),
                        ("chapter", Value::Int(0)),
                        (
                            "ranges",
                            Value::List(vec![Value::Int(1), Value::Int(0), Value::Int(6)]),
                        ),
                        ("color", Value::text("#f5d76e")),
                        ("text", Value::Rich(tilolang_runtime::rich::RichValue::from_text("A note of mine"))),
                    ],
                ),
            );
            assert_eq!(
                books.rt.project(&books.rt.read(&book, "marks").unwrap())["count"],
                serde_json::json!(2)
            );
        }

        phase a_pages_bookmark_goes_on_and_off;
        {
            let books = Books::new();
            let book = books.open();
            let toggle = || {
                books.call(
                    "toggle_bookmark",
                    books.args(
                        "toggle_bookmark",
                        &[
                            ("b", Value::resource(&book)),
                            ("chapter", Value::Int(0)),
                            ("block", Value::Int(0)),
                        ],
                    ),
                )
            };
            toggle();
            assert_eq!(
                books.rt.project(&books.rt.read(&book, "marks").unwrap())["count"],
                serde_json::json!(1)
            );
            toggle();
            assert_eq!(
                books.rt.project(&books.rt.read(&book, "marks").unwrap())["count"],
                serde_json::json!(0),
                "pressing it again takes the bookmark off"
            );
        }

        phase a_palette_is_chosen_and_one_of_the_readers_own_made_and_deleted;
        {
            let books = Books::new();
            assert_eq!(books.said(&books.lib, "scheme"), "Sepia");
            books.call(
                "set_scheme",
                books.args("set_scheme", &[("lib", Value::resource(&books.lib)), ("name", Value::text("Dark"))]),
            );
            assert_eq!(books.said(&books.lib, "scheme"), "Dark");
            books.call(
                "add_scheme",
                books.args(
                    "add_scheme",
                    &[
                        ("lib", Value::resource(&books.lib)),
                        ("name", Value::text("Mine")),
                        ("page", Value::text("#102030")),
                        ("ink", Value::text("#ffffff")),
                    ],
                ),
            );
            // The settings' button makes the palette and chooses it; the action that makes
            // one only adds it to the reader's own.
            books.call(
                "set_scheme",
                books.args("set_scheme", &[("lib", Value::resource(&books.lib)), ("name", Value::text("Mine"))]),
            );
            assert_eq!(books.said(&books.lib, "scheme"), "Mine");
            let window = books.window("library_settings", vec![Value::resource(&books.lib)]);
            assert!(
                of_kind(window.tree(), "ui.ListRow")
                    .iter()
                    .any(|row| row.props.get("title") == Some(&Value::text("Mine"))),
                "the reader's own palette stands in the settings"
            );
            let window = books.window("library", vec![Value::resource(&books.lib)]);
            let settings = {
                let segment = of_kind(window.tree(), "ui.Segment")
                    .into_iter()
                    .find(|node| text_prop(node, "label") == "Settings")
                    .expect("the library offers its settings");
                segment.key.clone()
            };
            let _ = settings;
        }

        phase the_faces_the_device_has_are_offered;
        {
            let books = Books::new();
            let mut window = books.window("library_settings", vec![Value::resource(&books.lib)]);
            let picker = of_kind(window.tree(), "ui.Dropdown")
                .into_iter()
                .find(|node| text_prop(node, "caption") == "Face")
                .expect("the settings offer a face to pick");
            let options = match picker.props.get("options") {
                Some(Value::List(items)) => items
                    .iter()
                    .map(|item| match item {
                        Value::Text(family) => family.clone(),
                        other => panic!("a family is text, not {other:?}"),
                    })
                    .collect::<Vec<_>>(),
                other => panic!("a dropdown's options are a list, not {other:?}"),
            };
            assert_eq!(
                options,
                ["Our Serif", "Our Sans"],
                "the picker offers the families this device sets text in"
            );
            let key = picker.key.clone();
            window
                .fire_with(&key, "change", Some(&Value::text("Our Sans")))
                .unwrap();
            window.poll_patches().unwrap();
            assert_eq!(
                books.rt.read(&books.lib, "face").unwrap(),
                Value::text("Our Sans"),
                "picking a face records it through the reading settings' action"
            );
            let book = books.open();
            let page = books.book_window(&book);
            let reading = of_kind(page.tree(), "ui.Reading")[0];
            assert_eq!(prop(reading, "face"), "Our Sans");
        }

        phase the_page_is_set_the_way_its_reader_likes_it;
        {
            let books = Books::new();
            let lib = Value::resource(&books.lib);
            books.call("set_text_size", books.args("set_text_size", &[("lib", lib.clone()), ("size", Value::Float(3.0))]));
            assert_eq!(
                books.rt.read(&books.lib, "text_size").unwrap(),
                Value::Float(2.0),
                "a size past what a page reads at is held at it"
            );
            books.call("set_face", books.args("set_face", &[("lib", lib.clone()), ("face", Value::text("literata"))]));
            books.call(
                "set_justified",
                books.args("set_justified", &[("lib", lib), ("on", Value::Bool(true))]),
            );
            let book = books.open();
            let window = books.book_window(&book);
            let reading = of_kind(window.tree(), "ui.Reading")[0];
            assert_eq!(prop(reading, "face"), "literata");
            assert_eq!(
                prop(reading, "mode"),
                "paged",
                "a window opens showing a page at a time"
            );
            assert_eq!(flag_prop(reading, "justified"), Some(true));
        }

        phase gutenberg_search_fills_the_books_found_online;
        {
            let books = Books::new();
            let action = books
                .app
                .meta()
                .actions
                .iter()
                .find(|action| action.name == "find_books")
                .expect("the app searches for books");
            assert_eq!(
                action.search.as_deref(),
                Some("Found"),
                "the app declares the search the system asks, and the kind its answers are"
            );
            books.call(
                "find_books",
                books.args(
                    "find_books",
                    &[("lib", Value::resource(&books.lib)), ("query", Value::text("tide glass"))],
                ),
            );
            let asked = books.device.requests();
            assert_eq!(
                books.rt.project(&books.rt.read(&books.lib, "found").unwrap())["count"],
                serde_json::json!(1),
                "the search's answer stands on the shelf's found list: {asked:?}"
            );
            assert!(
                asked.iter().any(|url| url.contains("gutendex.com") && url.contains("tide+glass")),
                "the search is the one the query names: {asked:?}"
            );
            let window = books.window("library", vec![Value::resource(&books.lib)]);
            let _ = window;
        }

        phase a_book_found_online_downloads_and_opens;
        {
            let books = Books::new();
            books.call(
                "find_books",
                books.args(
                    "find_books",
                    &[("lib", Value::resource(&books.lib)), ("query", Value::text("tide"))],
                ),
            );
            let found = books.found_online();
            let made = returned_ref(books.call(
                "download_found",
                books.args(
                    "download_found",
                    &[("lib", Value::resource(&books.lib)), ("f", Value::resource(&found))],
                ),
            ));
            let declared = books
                .app
                .meta()
                .actions
                .iter()
                .find(|action| action.name == "download_found")
                .expect("the app downloads a book it found");
            assert_eq!(
                declared.role.as_deref(),
                Some("primary"),
                "a window opens what a primary action returns"
            );
            assert!(
                books
                    .device
                    .requests()
                    .iter()
                    .any(|url| url == "https://www.gutenberg.org/ebooks/1.epub"),
                "the download is the answer's own address: {:?}",
                books.device.requests()
            );
            assert_eq!(
                books.rt.project(&books.rt.read(&books.lib, "found").unwrap())["count"],
                serde_json::json!(0),
                "the answer is taken away once its book stands on the shelf"
            );
            let shelf = books.rt.project(&books.rt.read(&books.lib, "books").unwrap());
            assert_eq!(shelf["count"], serde_json::json!(1), "the book is on the shelf");
            let book = books.opened_books()[0].clone();
            assert_eq!(
                book, made,
                "the press lands on the book it downloaded, which the window opens"
            );
            assert_eq!(books.said(&book, "title"), "The Tide Glass");
            assert_eq!(books.said(&book, "language"), "en");
        }

        phase a_chapters_page_shows_its_words;
        {
            let books = Books::new();
            let book = books.open();
            let chapter = books.chapter(&book, 1);
            assert_eq!(books.said(&chapter, "title"), "The Tide");
            let window = books.window("chapter_page", vec![Value::resource(&chapter)]);
            let shell = window.tree();
            assert_eq!(text_prop(shell, "title"), "The Tide", "the page is the chapter's");
            assert_eq!(
                of_kind(shell, "ui.Reading").len(),
                1,
                "the chapter's words are read on its page"
            );
        }

        phase a_marks_page_and_row_show_what_was_marked;
        {
            let books = Books::new();
            let book = books.open();
            books.call(
                "add_note",
                books.args(
                    "add_note",
                    &[
                        ("b", Value::resource(&book)),
                        ("chapter", Value::Int(0)),
                        (
                            "ranges",
                            Value::List(vec![Value::Int(1), Value::Int(0), Value::Int(8)]),
                        ),
                        ("color", Value::text("#f5d76e")),
                        ("text", Value::Rich(tilolang_runtime::rich::RichValue::from_text("Worth keeping"))),
                    ],
                ),
            );
            let mark = books.mark_of(&book);
            let row = books.window("mark_row", vec![Value::resource(&mark)]);
            assert_eq!(text_prop(row.tree(), "title"), "The tide");
            let page = books.window("mark_page", vec![Value::resource(&mark)]);
            let words: Vec<String> = of_kind(page.tree(), "ui.Text")
                .iter()
                .map(|node| text_prop(node, "value"))
                .collect();
            assert!(
                words.iter().any(|word| word == "Worth keeping"),
                "the mark's page says what it says: {words:?}"
            );
        }

        phase the_book_opens_where_the_person_left_it;
        {
            let books = Books::new();
            let book = books.open();
            let chapter = books.chapter(&book, 2);
            books.call(
                "read_chapter",
                books.args(
                    "read_chapter",
                    &[
                        ("b", Value::resource(&book)),
                        ("who", Value::Actor(me())),
                        ("c", Value::resource(&chapter)),
                    ],
                ),
            );
            books.call(
                "show_passage",
                books.args(
                    "show_passage",
                    &[
                        ("b", Value::resource(&book)),
                        ("who", Value::Actor(me())),
                        ("chapter", Value::Int(2)),
                        ("block", Value::Int(3)),
                    ],
                ),
            );
            assert_eq!(books.progress(&book, "chapter"), serde_json::json!(2));
            assert_eq!(books.progress(&book, "block"), serde_json::json!(3));
            let window = books.book_window(&book);
            let selected = of_kind(window.tree(), "ui.NavList")[0];
            assert_eq!(
                text_prop(selected, "selected"),
                "2",
                "the contents show the chapter the person left"
            );
            let reading = of_kind(window.tree(), "ui.Reading")[0];
            assert_eq!(
                number_prop(reading, "reveal"),
                Some(3),
                "the window opens on the passage the person left"
            );
        }

        phase the_librarys_window_lists_its_shelf;
        {
            let books = Books::new();
            books.open();
            let window = books.window("library", vec![Value::resource(&books.lib)]);
            let titles: Vec<String> = of_kind(window.tree(), "ui.MediaCard")
                .iter()
                .map(|card| text_prop(card, "title"))
                .collect();
            assert_eq!(titles, ["The Tide Glass"], "the shelf shows its book");
        }
    }
}
