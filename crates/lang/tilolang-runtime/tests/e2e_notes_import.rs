//! What a person arrives with: a folder of Markdown pages, and a Notion export. A zip of pages
//! comes in as the notebook's tree — a page under the page whose folder it stood in — with the
//! links between its pages kept as links between notes, the pictures it shows kept with the
//! note, and a database exported beside a page a table at it.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use bounded_zip::{ZipLimits, write_zip};
    use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
    use tilolang_runtime::rich::RichValue;
    use tilolang_runtime::{
        AmStore, ResourceRef, Runtime, SimpleAuthority, UiNode, Value, ViewInstance,
    };

    use crate::common::{App, me, returned_ref};

    const SOURCE: &str = include_str!("../../../../apps/tl/notes/app.tl");
    const DISCUSSION: &str = include_str!("../../../../apps/tl/discussion/app.tl");

    /// The device the test runs on: the content store a file's bytes are kept in.
    #[derive(Clone, Default)]
    struct Device {
        content: Rc<RefCell<HashMap<String, Vec<u8>>>>,
    }

    impl Device {
        /// `bytes` kept as a file called `name`: what a person choosing it would answer.
        fn keeping(&self, name: &str, media: &str, bytes: &[u8]) -> FileValue {
            self.keep(name, media, bytes).expect("the bytes are kept")
        }

        /// The pages `entries` as one archive, kept under `name`.
        fn archive(&self, name: &str, entries: &[(&str, &[u8])]) -> FileValue {
            let entries: Vec<(String, Vec<u8>)> = entries
                .iter()
                .map(|(name, bytes)| ((*name).to_owned(), bytes.to_vec()))
                .collect();
            let bytes = write_zip(&entries, &ZipLimits::default()).expect("the archive writes");
            self.keeping(name, "application/zip", &bytes)
        }
    }

    impl BlobHost for Device {
        fn read(&self, hash: &str) -> Option<Vec<u8>> {
            self.content.borrow().get(hash).cloned()
        }

        fn choose(&self, _request: &FileRequest) -> Result<Option<FileValue>, String> {
            Ok(None)
        }

        fn save(&self, _request: &SaveRequest, _content: &[u8]) -> Result<(), String> {
            Err("this test writes no files out".to_owned())
        }

        fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
            // The content's own address: the same bytes are one file, as a content store keeps
            // them.
            let mut digest = 0xcbf2_9ce4_8422_2325_u64;
            for byte in bytes {
                digest = (digest ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
            }
            let hash = format!("k{digest:x}");
            self.content
                .borrow_mut()
                .insert(hash.clone(), bytes.to_vec());
            Ok(FileValue::new(
                name,
                media,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash,
            ))
        }
    }

    /// A pixel of a PNG: what a page that shows a picture shows beside it.
    fn png() -> Vec<u8> {
        // A 2x2 PNG of one colour, written out here so the test needs no image library.
        const PIXELS: &[u8] = &[
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H',
            b'D', b'R', 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x72, 0xb6, 0x0d, 0x24, 0x00, 0x00, 0x00, 0x16, b'I', b'D', b'A', b'T', 0x78,
            0x9c, 0x63, 0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x00, 0x00, 0x00, 0x08, 0x00,
            0x01, 0x2c, 0x0a, 0x0b, 0x4e, 0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0xae,
            0x42, 0x60, 0x82,
        ];
        PIXELS.to_vec()
    }

    struct Notes {
        rt: Runtime,
        app: App,
        nb: ResourceRef,
    }

    impl Notes {
        fn new(device: Device) -> Self {
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            rt.set_clock(|| "2026-11-02T09:00:00Z".to_owned());
            rt.set_blobs(device);
            let discussion = App::compile(DISCUSSION, &[]);
            discussion.load(&rt);
            let app = App::compile(SOURCE, &[discussion.meta().clone()]);
            app.load(&rt);
            let nb = rt.create("Notebook").unwrap();
            Self { rt, app, nb }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> tilolang_runtime::InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        /// Import the archive `file` as notes.
        fn import(&self, file: &FileValue) {
            self.call(
                "import_archive",
                vec![
                    Value::resource(&self.nb),
                    Value::file(file.clone()),
                ],
            );
        }

        /// The notebook's window, every row of its tree drawn.
        fn window(&self) -> ViewInstance {
            let mut view = self
                .rt
                .open_view(&me(), &self.app.view("notebook"), vec![Value::resource(&self.nb)])
                .unwrap();
            view.set_lazy_rows(false).unwrap();
            view
        }

        /// Every note the notebook holds, by the tree the window shows them in.
        fn notes(&self) -> Vec<ResourceRef> {
            let view = self.window();
            let mut notes: Vec<ResourceRef> = Vec::new();
            collect_items(view.tree(), &mut notes);
            notes
        }

        fn note_titled(&self, title: &str) -> ResourceRef {
            self.notes()
                .into_iter()
                .find(|note| self.title_of(note) == title)
                .unwrap_or_else(|| panic!("no note titled {title}"))
        }

        fn title_of(&self, note: &ResourceRef) -> String {
            match self.rt.read(note, "title").unwrap() {
                Value::Text(title) => title,
                other => panic!("title reads {other:?}"),
            }
        }

        fn text_of(&self, note: &ResourceRef, field: &str) -> String {
            match self.rt.read(note, field).unwrap() {
                Value::Text(text) => text,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn body_of(&self, note: &ResourceRef) -> RichValue {
            match self.rt.read(note, "body").unwrap() {
                Value::Rich(body) => body,
                other => panic!("body reads {other:?}"),
            }
        }

        fn parent_of(&self, note: &ResourceRef) -> Option<ResourceRef> {
            match self.rt.read(note, "parent").unwrap() {
                Value::Opt(Some(held)) => match *held {
                    Value::Ref(r) => Some(ResourceRef::clone(&r.target)),
                    other => panic!("a parent is a note, not {other:?}"),
                },
                Value::Opt(None) => None,
                other => panic!("parent reads {other:?}"),
            }
        }

        /// The pictures the note keeps, each by the address of its bytes and the name it came
        /// in as: a map reads as its entries, each a pair.
        fn images_of(&self, note: &ResourceRef) -> Vec<(Value, Value)> {
            match self.rt.read(note, "images").unwrap() {
                Value::List(entries) => entries
                    .into_iter()
                    .filter_map(|entry| match entry {
                        Value::Tuple(fields) if fields.len() == 2 => {
                            let mut fields = fields.into_iter();
                            Some((fields.next()?, fields.next()?))
                        }
                        _ => None,
                    })
                    .collect(),
                other => panic!("images reads {other:?}"),
            }
        }

        /// The notes the body of `note` refers to, by the ids of the notes they name.
        fn linked_ids(&self, note: &ResourceRef) -> Vec<String> {
            self.body_of(note)
                .references()
                .iter()
                .filter(|reference| reference.as_resource().is_some())
                .filter_map(tilolang_runtime::rich::resource_id)
                .map(str::to_owned)
                .collect()
        }
    }

    /// Every item the tree names, in its order: a node's item is the resource its `item` prop is.
    fn collect_items(node: &UiNode, out: &mut Vec<ResourceRef>) {
        if node.kind == "ui.TreeViewNode"
            && let Some(Value::Ref(r)) = node.props.get("item")
        {
            out.push(ResourceRef::clone(&r.target));
        }
        for child in &node.children {
            collect_items(child, out);
        }
    }

    /// Every node of `kind` in the tree, in the order they stand.
    fn of_kind<'a>(node: &'a UiNode, kind: &str) -> Vec<&'a UiNode> {
        fn walk<'a>(node: &'a UiNode, kind: &str, out: &mut Vec<&'a UiNode>) {
            if node.kind == kind {
                out.push(node);
            }
            for child in &node.children {
                walk(child, kind, out);
            }
        }
        let mut out: Vec<&UiNode> = Vec::new();
        walk(node, kind, &mut out);
        out
    }

    /// A picture the editor put into a note's body is named by the note, so an export writes it
    /// beside the note under the name it came in as.
    #[test]
    fn a_picture_the_editor_puts_in_is_named_by_the_note() {
        let device = Device::default();
        let n = Notes::new(device);
        let note = returned_ref(n.call(
            "new_note",
            vec![
                Value::resource(&n.nb),
                Value::text("Plan"),
                Value::text("Ship the offline editor."),
            ],
        ));
        let view = n.window();
        let editor = of_kind(view.tree(), "ui.BlockEditor")
            .into_iter()
            .next()
            .expect("the open note's body is the editor")
            .key
            .clone();
        // What the editor says of a picture it wrote in at the caret: its name, then where its
        // bytes were kept.
        view.fire_with(
            &editor,
            "image",
            Some(&Value::List(vec![
                Value::text("Pasted image.png"),
                Value::text("tilo-blob://pixels"),
            ])),
        )
        .expect("the view takes the picture");
        assert_eq!(
            n.images_of(&note),
            vec![(
                Value::text("tilo-blob://pixels"),
                Value::text("Pasted image.png")
            )],
            "the note keeps the picture's name beside the address of its bytes"
        );
    }

    /// A notebook's own export shape: `Plan.md`, a page under its folder, and that page's own
    /// with a link back up to the one it stands in.
    #[test]
    fn a_folder_of_markdown_comes_in_as_a_tree_with_its_links_kept() {
        let device = Device::default();
        let archive = device.archive(
            "Notes.zip",
            &[
                (
                    "Plan.md",
                    "# Plan\n\nShip the offline editor. See [the standups](Plan/Standups.md).\n".as_bytes(),
                ),
                (
                    "Plan/Standups.md",
                    "# Standups\n\nTen minutes, every morning.\n\n[Monday](Standups/Monday.md)\n".as_bytes(),
                ),
                (
                    "Plan/Standups/Monday.md",
                    "# Monday\n\nAda is on the sync bug. Back to [the plan](../../Plan.md).\n".as_bytes(),
                ),
            ],
        );
        let n = Notes::new(device);
        n.import(&archive);

        let plan = n.note_titled("Plan");
        let standups = n.note_titled("Standups");
        let monday = n.note_titled("Monday");
        assert_eq!(n.notes().len(), 3, "one note per page");
        assert_eq!(
            n.parent_of(&standups),
            Some(ResourceRef::clone(&plan)),
            "a page stands under the page whose folder it was written in"
        );
        assert_eq!(n.parent_of(&monday), Some(ResourceRef::clone(&standups)));
        assert_eq!(n.parent_of(&plan), None);

        // A link to a page of the archive is the reference to the note that page made, wherever
        // the reader of the file is: the note opens, it is not a web address.
        let plan_links = n.linked_ids(&plan);
        assert_eq!(
            plan_links,
            vec![standups.id_string()],
            "the link to the standups names their note"
        );
        assert_eq!(n.linked_ids(&standups), vec![monday.id_string()]);
        assert_eq!(
            n.linked_ids(&monday),
            vec![plan.id_string()],
            "a link up out of a folder names the page it left"
        );
        // The heading that named the page is its title, not its body's first line again.
        let body = n.body_of(&plan).written_text();
        assert!(!body.contains("# Plan"), "the title's heading is not said twice: {body}");
    }

    /// A Notion export: the pages it wrote, the database it wrote beside one as a CSV, and a
    /// page it never titled.
    #[test]
    fn a_notion_export_comes_in_with_its_databases_as_tables() {
        let device = Device::default();
        let archive = device.archive(
            "Export.zip",
            &[
                (
                    "Plan.md",
                    "# Plan\n\nShip the offline editor.\n".as_bytes(),
                ),
                (
                    "Plan Tasks.csv",
                    "Task,Owner\nShip the editor,Ada\nGrow the garden,\n".as_bytes(),
                ),
                (
                    "Untitled 1f2e3d4c.md",
                    "# Notes from the call\n\nThree people, two decisions.\n".as_bytes(),
                ),
                ("__MACOSX/Plan.md", "not a page".as_bytes()),
            ],
        );
        let n = Notes::new(device);
        n.import(&archive);

        let titles: Vec<String> = n.notes().iter().map(|note| n.title_of(note)).collect();
        assert_eq!(
            titles,
            ["Plan", "Notes from the call"],
            "a page the export never titled takes the heading it says, and the archive's own \
             bookkeeping is no page"
        );

        let plan = n.note_titled("Plan");
        let body = n.body_of(&plan).written_text();
        assert!(
            body.contains("| Task | Owner |") && body.contains("| Ship the editor | Ada |"),
            "the database beside the page is a table at it: {body}"
        );
        assert!(
            body.contains("| Grow the garden |  |"),
            "a row's empty field is an empty cell: {body}"
        );
    }

    /// A picture a page shows is kept with the note, and named: an export writes it beside the
    /// note under the name it came in as.
    #[test]
    fn a_page_picture_is_kept_with_its_note_and_named() {
        let device = Device::default();
        let pixels = png();
        let archive = device.archive(
            "Notes.zip",
            &[
                (
                    "Plan.md",
                    "# Plan\n\n![a fox](Plan/images/fox.png)\n".as_bytes(),
                ),
                ("Plan/images/fox.png", pixels.as_slice()),
            ],
        );
        let n = Notes::new(device);
        n.import(&archive);

        let plan = n.note_titled("Plan");
        let images = n.images_of(&plan);
        assert_eq!(images.len(), 1, "the note keeps the picture it shows");
        let (Value::Text(address), Value::Text(name)) = (&images[0].0, &images[0].1) else {
            panic!("a picture is kept by its address and its name: {images:?}");
        };
        assert!(address.starts_with("tilo-blob://"), "{address}");
        assert_eq!(name, "fox.png");
        assert!(
            n.body_of(&plan).written_text().contains(address),
            "the body shows the picture from where it is kept"
        );
    }

    /// Importing the same archive twice keeps one note of each page: a note remembers the file
    /// it came from.
    #[test]
    fn the_same_archive_imported_twice_makes_no_second_note() {
        let device = Device::default();
        let archive = device.archive("Notes.zip", &[("Plan.md", "# Plan\n\nGo.\n".as_bytes())]);
        let n = Notes::new(device);
        n.import(&archive);
        n.import(&archive);
        assert_eq!(n.notes().len(), 1, "the page already made its note");
    }

    /// An import from another service says where each page came from in its own file beside the
    /// pages: a page keeps the title that service gave it, who wrote it, its address there, the
    /// page it stood under and its dates. Importing the page again by its address there — however
    /// its words changed — opens the note it made rather than making a second one.
    #[test]
    fn a_migrated_page_keeps_where_it_came_from_and_its_note() {
        let device = Device::default();
        let pixels = png();
        let record = |path: &str, title: &str, page: &str, above: &str| {
            format!(
                "{path}\t{title}\tAda\tOutline\t{page}\t{above}\t2025-08-01T10:00:00Z\t\
                 2025-09-01T10:00:00Z\n"
            )
        };
        let said = format!(
            "tilo-import 1\n{}{}",
            record(
                "Q3 plan.md",
                "Q3 plan",
                "https://outline.example/doc/top",
                ""
            ),
            record(
                "Q3 plan/Ideas.md",
                "Ideas to ship",
                "https://outline.example/doc/ideas",
                "https://outline.example/doc/top"
            ),
        );
        let archive = device.archive(
            "Outline pages.zip",
            &[
                ("tilo-import.tsv", said.as_bytes()),
                (
                    "Q3 plan.md",
                    "# Q3 plan\n\nShip the offline editor.\n".as_bytes(),
                ),
                (
                    "Q3 plan/Ideas.md",
                    "Three ideas, and no heading of its own.\n\n\
                     ![a fox](Ideas/images/fox.png)\n"
                        .as_bytes(),
                ),
                ("Q3 plan/Ideas/images/fox.png", pixels.as_slice()),
            ],
        );
        // The page's words changed where it came from: it is the same page there, so the second
        // archive names the same page address, and its file's bytes are not the first one's.
        let changed = device.archive(
            "Outline pages.zip",
            &[
                ("tilo-import.tsv", said.as_bytes()),
                (
                    "Q3 plan/Ideas.md",
                    "Rewritten at the source since.\n".as_bytes(),
                ),
            ],
        );
        let n = Notes::new(device);
        n.import(&archive);

        assert_eq!(n.notes().len(), 2, "a note a page");
        let plan = n.note_titled("Q3 plan");
        let ideas = n.note_titled("Ideas to ship");
        assert_eq!(
            n.parent_of(&ideas),
            Some(ResourceRef::clone(&plan)),
            "the page stands under the page it stood under there, by that page's address"
        );
        assert_eq!(n.text_of(&ideas, "by"), "Ada");
        assert_eq!(n.text_of(&ideas, "service"), "Outline");
        assert_eq!(
            n.text_of(&ideas, "page"),
            "https://outline.example/doc/ideas"
        );
        assert_eq!(n.text_of(&ideas, "created"), "2025-08-01T10:00:00Z");
        assert_eq!(n.text_of(&ideas, "updated"), "2025-09-01T10:00:00Z");
        assert_eq!(
            n.title_of(&ideas),
            "Ideas to ship",
            "the service's title, not its file's name"
        );
        let body = n.body_of(&ideas).written_text();
        assert!(
            body.contains("Three ideas, and no heading of its own."),
            "{body}"
        );
        let images = n.images_of(&ideas);
        assert_eq!(images.len(), 1, "the picture the page shows: {images:?}");
        assert_eq!(images[0].1, Value::text("fox.png"));

        // The page's words changed where it came from: it is the same page there, so it is the
        // same note here, and the note is the person's now.
        n.import(&changed);
        assert_eq!(n.notes().len(), 2, "the page already made its note");
        assert_eq!(
            n.notes().first().map(|note| n.title_of(note)),
            Some("Q3 plan".to_owned())
        );
        assert!(
            n.body_of(&n.note_titled("Ideas to ship"))
                .written_text()
                .contains("Three ideas"),
            "the note is the person's now, as they left it"
        );
    }
}
