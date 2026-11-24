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
        AmStore, InvokeOutcome, ResourceRef, Runtime, UiNode, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/ebook/app.tl");

    /// The book the tests open, as base64 text the repository may hold: three chapters, its own
    /// title, author and language.
    const TIDEGLASS_BASE64: &str = include_str!("fixtures/tideglass.epub.b64");

    /// The session app, as far as the books app needs it: opening a resource's window. A tilolang
    /// package cannot name an action `open` (a built-in's name is taken), so the stand-in
    /// declares `open_window` and is published under the name the session's native surface uses.
    /// The workspace package, as far as this app needs it: its `use` of the people a workspace
    /// has. A stand-in carries the name the import names.
    const WORKSPACE_SOURCE: &str = r#"app tilo.workspace "Workspace"

/// One person of the workspace.
type Member {
    name: Text
}

/// The workspace's members, as its apps read them.
type Members {
    people: Collection<Member>
}

root members: Members

default view members_page for Members

row view member_row for Member

view members_page(m: Members) {
    ui.Text(m.people.len().to_text())
}

view member_row(p: Member) {
    ui.ListRow(title: p.name)
}
"#;

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
    /// What a path on the device's disk holds: whether it is a folder, and its bytes.
    type DiskEntry = (bool, Vec<u8>);

    /// answered.
    #[derive(Clone, Default)]
    struct Device {
        files: Rc<RefCell<std::collections::HashMap<String, Vec<u8>>>>,
        requests: Rc<RefCell<Vec<WebRequest>>>,
        /// The device's disk: each path it holds, and whether it is a folder.
        disk: Rc<RefCell<std::collections::BTreeMap<String, DiskEntry>>>,
        /// The folders the app asked to watch, by path, with the action each runs.
        watches: Rc<RefCell<Vec<(String, String)>>>,
        /// What the person last copied.
        clip: Rc<RefCell<String>>,
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

        fn copied(&self) -> String {
            self.clip.borrow().clone()
        }
    }

    impl tilolang_runtime::clipboard::ClipboardHost for Device {
        fn copy(&self, _mime: &str, bytes: &[u8]) -> bool {
            *self.clip.borrow_mut() = String::from_utf8_lossy(bytes).into_owned();
            true
        }

        fn paste(&self, _types: &[&str]) -> Option<(String, Vec<u8>)> {
            None
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
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let device = Device::default();
            rt.set_blobs(device.clone());
            rt.set_web(device.clone());
            rt.set_clipboard(device.clone());
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
            let workspace = App::compile(WORKSPACE_SOURCE, &[]);
            workspace.load(&rt);
            let app = App::compile(SOURCE, &[workspace.meta().clone(), session.meta().clone()]);
            authority.trust(&app.load(&rt));
            authority.grant_outside(&app.action("copy_passage"));
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
            let mut window = self
                .rt
                .open_view(&me(), &self.app.view(view), args)
                .unwrap();
            // The shelf is a virtual grid: its cards are built when a host shows them, so a
            // test that reads the tree reads every one.
            window.set_lazy_rows(false).unwrap();
            window
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

        /// The face of the family `family`, read from the library's own collection.
        fn font_ref(&self, family: &str) -> ResourceRef {
            let fonts = self.rt.read(&self.lib, "fonts").unwrap();
            let project = self.rt.project(&fonts);
            let id = project["items"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|item| item["fields"]["family"] == serde_json::json!(family))
                .and_then(|item| item["id"].as_str())
                .unwrap_or_else(|| panic!("no face of {family} in the reader's settings"))
                .to_owned();
            let ty = self
                .app
                .meta()
                .types
                .iter()
                .find(|declared| declared.name == "Font")
                .expect("the books app declares its faces")
                .id
                .clone();
            ResourceRef::parse_id(&id, ty).expect("a face's id is a resource id")
        }

        /// Whether the library holds a face of `family`.
        fn has_font(&self, family: &str) -> bool {
            let fonts = self.rt.read(&self.lib, "fonts").unwrap();
            let project = self.rt.project(&fonts);
            project["items"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|item| item["fields"]["family"] == serde_json::json!(family))
        }

        /// The face called `title` in the reader's settings, as its row stands for it.
        fn face(&self, title: &str) -> ResourceRef {
            let window = self.window("library_settings", vec![Value::resource(&self.lib)]);
            of_kind(window.tree(), "ui.ListRow")
                .into_iter()
                .find(|row| text_prop(row, "title") == title)
                .and_then(|row| match row.props.get("item") {
                    Some(Value::Ref(r)) => Some(ResourceRef::clone(&r.target)),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no face {title} in the reader's settings"))
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
            let lib = Value::resource(&books.lib);
            // The reader starts with the faces the toolkit ships, and the plain three.
            let settings = books.window("library_settings", vec![lib.clone()]);
            let titles: Vec<String> = of_kind(settings.tree(), "ui.ListRow")
                .into_iter()
                .map(|row| text_prop(row, "title"))
                .collect();
            for face in [
                "Serif",
                "Sans",
                "Monospace",
                "Libertinus",
                "Literata",
                "EB Garamond",
                "Source Serif",
            ] {
                assert!(
                    titles.iter().any(|title| title == face),
                    "the reader ships with {face}: {titles:?}"
                );
            }
            // A family this device sets text in is added, then renamed.
            books.call(
                "add_font",
                books.args("add_font", &[("lib", lib.clone()), ("family", Value::text("Our Sans"))]),
            );
            let added = books.face("Our Sans");
            books.call(
                "rename_font",
                books.args(
                    "rename_font",
                    &[
                        ("lib", lib.clone()),
                        ("f", Value::resource(&added)),
                        ("name", Value::text("Our own sans")),
                    ],
                ),
            );
            let settings = books.window("library_settings", vec![lib.clone()]);
            assert!(
                of_kind(settings.tree(), "ui.ListRow")
                    .iter()
                    .any(|row| text_prop(row, "title") == "Our own sans"),
                "the settings call the face the name a person gave it"
            );
            // The face is chosen, reads larger by its own size, and hides from the reader's
            // quick picker while the settings still offer it.
            books.call(
                "set_face",
                books.args("set_face", &[("lib", lib.clone()), ("face", Value::text("Our Sans"))]),
            );
            books.call(
                "set_font_size",
                books.args(
                    "set_font_size",
                    &[
                        ("lib", lib.clone()),
                        ("f", Value::resource(&added)),
                        ("size", Value::Float(1.4)),
                    ],
                ),
            );
            let book = books.open();
            let page = books.book_window(&book);
            let reading = of_kind(page.tree(), "ui.Reading")[0];
            assert_eq!(prop(reading, "face"), "Our Sans");
            assert_eq!(
                prop(reading, "scale"),
                "1.4",
                "the page is set at the text size times the face's own"
            );
            books.call(
                "set_font_shown",
                books.args(
                    "set_font_shown",
                    &[
                        ("lib", lib.clone()),
                        ("f", Value::resource(&added)),
                        ("shown", Value::Bool(false)),
                    ],
                ),
            );
            let page = books.book_window(&book);
            let menu = of_kind(page.tree(), "ui.MenuButton")
                .into_iter()
                .find(|menu| text_prop(menu, "label") == "Face")
                .expect("the reader's bar offers its faces");
            assert!(
                of_kind(menu, "ui.MenuItem")
                    .iter()
                    .all(|item| text_prop(item, "label") != "Our own sans"),
                "a hidden face leaves the reader's quick picker"
            );
            let settings = books.window("library_settings", vec![lib.clone()]);
            assert!(
                of_kind(settings.tree(), "ui.ListRow")
                    .iter()
                    .any(|row| text_prop(row, "title") == "Our own sans"),
                "the settings still offer a hidden face, to show again"
            );
            // A face the reader ships with stays; the person's own goes, with its row open.
            let serif = books.font_ref("serif");
            let refused = books.rt.invoke_action(
                &me(),
                (
                    books.app.action("remove_font").as_str(),
                    books.args(
                        "remove_font",
                        &[("lib", lib.clone()), ("f", Value::resource(&serif))],
                    ),
                ),
            );
            assert!(refused.is_err(), "a face the reader ships with is kept");
            let own = books.font_ref("Our Sans");
            books.call(
                "remove_font",
                books.args("remove_font", &[("lib", lib), ("f", Value::resource(&own))]),
            );
            assert!(!books.has_font("Our Sans"), "the person's own face goes");
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
            let made = match books.call(
                "download_found",
                books.args(
                    "download_found",
                    &[("lib", Value::resource(&books.lib)), ("f", Value::resource(&found))],
                ),
            ) {
                InvokeOutcome::Done(Value::Opt(Some(held))) => {
                    returned_ref(InvokeOutcome::Done(*held))
                }
                other => panic!("the download made no book: {other:?}"),
            };
            let declared = books
                .app
                .meta()
                .actions
                .iter()
                .find(|action| action.name == "download_found")
                .expect("the app downloads a book it found");
            assert_eq!(
                declared.role.as_deref(),
                None,
                "the library's primary action is adding a book; a download shows what it made"
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

        phase a_finished_book_leaves_a_shelf_that_hides_finished_books;
        {
            let books = Books::new();
            let book = books.open();
            books.call(
                "set_finished",
                books.args(
                    "set_finished",
                    &[
                        ("b", Value::resource(&book)),
                        ("who", Value::Actor(me())),
                        ("finished", Value::Bool(true)),
                    ],
                ),
            );
            assert_eq!(
                books.progress(&book, "finished"),
                serde_json::json!(true),
                "the person finished the book"
            );
            let hiding = |hidden: bool| {
                books.call(
                    "set_hide_finished",
                    books.args(
                        "set_hide_finished",
                        &[
                            ("lib", Value::resource(&books.lib)),
                            ("hidden", Value::Bool(hidden)),
                        ],
                    ),
                )
            };
            hiding(true);
            let window = books.window("library", vec![Value::resource(&books.lib)]);
            assert!(
                of_kind(window.tree(), "ui.MediaCard").is_empty(),
                "a finished book is left off the shelf"
            );
            hiding(false);
            let window = books.window("library", vec![Value::resource(&books.lib)]);
            assert_eq!(
                of_kind(window.tree(), "ui.MediaCard").len(),
                1,
                "a finished book shows while the shelf keeps them"
            );
        }

        phase a_books_details_are_edited_through_its_page;
        {
            let books = Books::new();
            let book = books.open();
            assert_eq!(books.said(&book, "title"), "The Tide Glass");
            let mut window = books.book_window(&book);
            // The page's own control opens the form, filled with what the book says now.
            let details = of_kind(window.tree(), "ui.Button")
                .into_iter()
                .find(|button| text_prop(button, "label") == "Book details")
                .expect("the book's page offers its details")
                .key
                .clone();
            window.fire(&details, "press").unwrap();
            window.poll_patches().unwrap();
            let dialog = of_kind(window.tree(), "ui.ModalDialog")[0];
            assert_eq!(text_prop(dialog, "title"), "Book details");
            assert!(
                matches!(
                    dialog.props.get("open"),
                    Some(Value::Bool(true) | Value::Int(1))
                ),
                "the form stands open: {:?}",
                dialog.props
            );
            let title_field = of_kind(window.tree(), "ui.TextInput")[0].key.clone();
            window
                .edit(&title_field, &Value::text("The Tide Glass, corrected"))
                .unwrap();
            // A book without a title is not saved: the form's Save waits for one.
            window.edit(&title_field, &Value::text("   ")).unwrap();
            window.poll_patches().unwrap();
            let dialog = of_kind(window.tree(), "ui.ModalDialog")[0];
            assert!(
                matches!(
                    dialog.props.get("primary_enabled"),
                    Some(Value::Bool(false) | Value::Int(0))
                ),
                "a book without a title cannot be saved: {:?}",
                dialog.props
            );
            window
                .edit(&title_field, &Value::text("The Tide Glass, corrected"))
                .unwrap();
            window.poll_patches().unwrap();
            let dialog = of_kind(window.tree(), "ui.ModalDialog")[0].key.clone();
            window.fire(&dialog, "act_primary").unwrap();
            window.poll_patches().unwrap();
            assert_eq!(
                books.said(&book, "title"),
                "The Tide Glass, corrected",
                "the page's Save writes the title the person typed"
            );
            let header = of_kind(window.tree(), "ui.NavShell")[0];
            assert_eq!(
                text_prop(header, "title"),
                "The Tide Glass, corrected",
                "the page is called what the book is called now"
            );
            // The rest of the book's facts are typed on it too.
            books.call(
                "set_author",
                books.args("set_author", &[("b", Value::resource(&book)), ("author", Value::text("Ada Ferrer"))]),
            );
            books.call(
                "set_subject",
                books.args("set_subject", &[("b", Value::resource(&book)), ("subject", Value::text("Fable"))]),
            );
            books.call(
                "set_language",
                books.args("set_language", &[("b", Value::resource(&book)), ("language", Value::text("de"))]),
            );
            assert_eq!(books.said(&book, "author"), "Ada Ferrer");
            assert_eq!(books.said(&book, "subject"), "Fable");
            assert_eq!(books.said(&book, "language"), "de");
            // A cover is a picture the person chooses, and taken off again.
            let picture = Value::file(books.device.keeping(
                "cover.png",
                "image/png",
                &[0x89, b'P', b'N', b'G'],
            ));
            books.call(
                "set_cover",
                books.args("set_cover", &[("b", Value::resource(&book)), ("picture", picture)]),
            );
            assert_ne!(books.said(&book, "cover"), "", "the picture stands on the book");
            books.call(
                "clear_cover",
                books.args("clear_cover", &[("b", Value::resource(&book))]),
            );
            assert_eq!(books.said(&book, "cover"), "");
        }

        phase a_note_is_edited_and_a_passage_is_copied;
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
            let mark = books.mark_of(&book);
            books.call(
                "copy_passage",
                books.args("copy_passage", &[("text", Value::text("Worth keeping"))]),
            );
            assert_eq!(books.device.copied(), "Worth keeping");
            books.call(
                "edit_mark_text",
                books.args(
                    "edit_mark_text",
                    &[
                        ("b", Value::resource(&book)),
                        ("m", Value::resource(&mark)),
                        (
                            "text",
                            Value::Rich(tilolang_runtime::rich::RichValue::from_text(
                                "Edited note",
                            )),
                        ),
                    ],
                ),
            );
            assert_eq!(
                books.mark_field(&book, "text"),
                serde_json::json!("Edited note"),
                "the note says what its author wrote"
            );
        }

        phase the_windows_search_finds_the_passages_of_the_open_chapter;
        {
            let books = Books::new();
            let book = books.open();
            let chapter = books.chapter(&book, 0);
            let Value::Rich(body) = books.rt.read(&chapter, "body").unwrap() else {
                panic!("a chapter is rich text");
            };
            // A word the chapter says, and where the passage holding it stands.
            let lines = body.written_text();
            let lines: Vec<&str> = lines.split('\n').collect();
            let words = "the";
            let first = lines
                .iter()
                .position(|line| line.to_lowercase().contains(words))
                .expect("the chapter says the words");

            let mut window = books.book_window(&book);
            assert!(
                of_kind(window.tree(), "ui.Button")
                    .iter()
                    .all(|button| prop(button, "label") != "Next passage"),
                "no find line stands until the window searches:\n{}",
                dump(window.tree())
            );
            let shell = of_kind(window.tree(), "ui.NavShell")[0].key.clone();
            window
                .fire_with(&shell, "search", Some(&Value::text(words)))
                .unwrap();
            window.poll_patches().unwrap();
            let tree = window.tree().clone();
            let reading = of_kind(&tree, "ui.Reading")[0];
            assert_eq!(
                number_prop(reading, "reveal"),
                Some(i64::try_from(first).unwrap()),
                "the first passage holding the words is shown"
            );
            let said: Vec<String> = of_kind(&tree, "ui.Text")
                .iter()
                .filter_map(|text| text.props.get("value").and_then(Value::as_text))
                .map(str::to_owned)
                .collect();
            let held = lines
                .iter()
                .filter(|line| line.to_lowercase().contains(words))
                .count();
            let count = if held == 1 {
                "1 passage".to_owned()
            } else {
                format!("{held} passages")
            };
            assert!(
                said.contains(&count),
                "the line counts the passages the chapter holds: {said:?}"
            );

            // The next passage stands under it.
            let next = of_kind(&tree, "ui.Button")
                .iter()
                .find(|button| prop(button, "label") == "Next passage")
                .expect("the find line's way on")
                .key
                .clone();
            window.press(&next).unwrap();
            window.poll_patches().unwrap();
            let reading = of_kind(window.tree(), "ui.Reading")[0];
            assert_eq!(
                number_prop(reading, "reveal"),
                Some(i64::try_from(first + 1).unwrap()),
                "the next passage is shown"
            );
        }
    }
}
