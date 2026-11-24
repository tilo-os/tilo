//! A view's words in the reader's language (i18n.md §2): a literal a component shows is a
//! message of the package's catalogue, drawn through the runtime's words with its
//! interpolations as arguments; a literal that is data, or that a view compares, is not.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::path::Path;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang::source::compile_package;
    use tilolang_runtime::{AmStore, Runtime, SimpleAuthority, Value, Words};

    /// A reader who resolves a message from the catalogue of one language, and reads the source
    /// text where the package has none: what a window's host does with the catalogues a package's
    /// module carries.
    struct Speaking {
        language: &'static str,
        held: RefCell<Vec<(String, String)>>,
    }

    impl Speaking {
        fn new(language: &'static str) -> Rc<Self> {
            Rc::new(Self {
                language,
                held: RefCell::new(Vec::new()),
            })
        }
    }

    impl Words for Speaking {
        fn add(&self, catalogues: &[(String, String)]) {
            *self.held.borrow_mut() = catalogues.to_vec();
        }

        fn add_used(&self, _: &[(String, String)]) {}

        fn word(&self, key: &str, _args: &[(String, String)], source: &str) -> String {
            self.held
                .borrow()
                .iter()
                .find(|(locale, _)| locale == self.language)
                .and_then(|(_, catalogue)| {
                    let entry = format!("{key} = ");
                    catalogue
                        .lines()
                        .find_map(|line| line.strip_prefix(&entry).map(str::to_owned))
                })
                .unwrap_or_else(|| source.to_owned())
        }
    }

    const SOURCE: &str = r#"app so.tilo.inbox "Inbox"

type Inbox {
    unread: Int = 3
    kind: Text
    pinned: Bool = false

    search {
        filter "Pinned" = pinned
    }
}

root Inbox

default view inbox for Inbox

view inbox(i: Inbox) {
    ui.Text("{i.unread} new messages")
    ui.Text(if i.unread > 2 and i.kind != "home" { "At work" } else { "At home" })
    ui.Link("https://example.com/help", label: .some("Help"))
    ui.Text(meta(i).title)
    ui.Text(filters(Inbox).find(f -> f.query == "is:pinned")?.label ?? "Pinned")
}
"#;

    /// German for the keys it knows, keyed exactly as the catalogue keys them.
    struct German;
    impl Words for German {
        fn add(&self, _: &[(String, String)]) {}
        fn add_used(&self, _: &[(String, String)]) {}
        fn word(&self, key: &str, args: &[(String, String)], source: &str) -> String {
            let arg = |name: &str| {
                args.iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default()
            };
            match key.rsplit_once('.').map(|(_, attr)| attr) {
                Some("w-new-messages") => format!("{} neue Nachrichten", arg("unread")),
                Some("w-at-work") => "Bei der Arbeit".to_owned(),
                Some("w-help") => "Hilfe".to_owned(),
                // What a type calls one of its records while its title is empty.
                Some("untitled") => "Ohne Titel".to_owned(),
                // The app's title, keyed by the app's id, and a filter's label, keyed by its
                // type and the filter's key: neither is a declaration with an attribute.
                _ if key == "d-so-tilo-inbox" => "Eingang".to_owned(),
                _ if key.ends_with("-filter-pinned") => "Angeheftet".to_owned(),
                _ => source.to_owned(),
            }
        }
    }

    const TITLED: &str = r#"app so.tilo.notebook "Notebook"

type Note (untitled: "Untitled") {
    title: Text
}

type Notebook {
    notes: Collection<Note>
}

root Notebook

default view notebook for Notebook

default view note_editor for Note

primary action new_note(nb: Notebook) -> Note (icon: .plus) {
    nb.notes.insert(Note(title: ""))
}

view notebook(nb: Notebook) {
    for n in nb.notes {
        ui.ListRow(title: meta(n).title)
    }
}

view note_editor(n: Note) {
    ui.TextInput(bind: n.title, title: true)
}

view editor_with_own_words(n: Note) {
    ui.TextInput(bind: n.title, placeholder: "Say it in a line", title: true)
}
"#;

    tilo_test_env::scenario! {
        fn words_read_in_the_readers_language;

        phase a_real_packages_window_reads_the_english_or_german_its_catalogue_says;
        // The catalogue a package's build writes from its sources is what its window reads: one
        // package, one module, a German reader's window and an English one.
        {
            let dir =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/counter");
            let built =
                compile_package(&dir, &[], &[], &mut tilolang::SeededEntropy(0)).unwrap();
            assert_eq!(built.last().map(|b| &b.dir), Some(&dir), "the root compiles last");
            let compiled = &built.last().unwrap().compiled;
            let counter = compiled
                .meta
                .views
                .iter()
                .find(|view| view.name == "counter" && view.row_of.is_none())
                .expect("the counter's window view")
                .id
                .clone();
            for (language, expected) in [("en-US", "Increment"), ("de-DE", "Erhöhen")] {
                let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
                rt.set_words(Speaking::new(language));
                rt.load_compiled(compiled).unwrap();
                let c = rt.create("Counter").unwrap();
                let mut view = rt
                    .open_view(&me(), &counter, vec![Value::resource(&c)])
                    .unwrap();
                view.poll_patches().unwrap();
                let labels: Vec<String> = of_kind(view.tree(), "ui.Button")
                    .into_iter()
                    .map(|button| text_prop(button, "label"))
                    .collect();
                assert!(
                    labels.iter().any(|label| label == expected),
                    "{language} reads {expected:?} among {labels:?}"
                );
            }
        }

        phase a_types_untitled_words_read_in_the_readers_language;
        // A record with no title is called what its type says, in the reader's language, at every
        // surface that names it: its row, the title every window shows, and its title field's
        // placeholder.
        {
            let app = App::compile(TITLED, &[]);
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            app.load(&rt);
            rt.set_words(German);
            let nb = rt.create("Notebook").unwrap();
            let note = returned_ref(
                rt.invoke_action(
                    &me(),
                    (app.action("new_note").as_str(), vec![Value::resource(&nb)]),
                )
                .unwrap(),
            );
            assert_eq!(rt.title(&note).unwrap(), "Ohne Titel");
            let mut editor = rt
                .open_view(
                    &me(),
                    &app.view("note_editor"),
                    vec![Value::resource(&note)],
                )
                .unwrap();
            editor.poll_patches().unwrap();
            let field = of_kind(editor.tree(), "ui.TextInput")[0];
            assert_eq!(text_prop(field, "placeholder"), "Ohne Titel");
            // A view that gives its own placeholder keeps it.
            let mut own = rt
                .open_view(
                    &me(),
                    &app.view("editor_with_own_words"),
                    vec![Value::resource(&note)],
                )
                .unwrap();
            own.poll_patches().unwrap();
            assert_eq!(
                text_prop(of_kind(own.tree(), "ui.TextInput")[0], "placeholder"),
                "Say it in a line"
            );
            let mut list = rt
                .open_view(&me(), &app.view("notebook"), vec![Value::resource(&nb)])
                .unwrap();
            list.poll_patches().unwrap();
            let row = of_kind(list.tree(), "ui.ListRow")[0];
            assert_eq!(text_prop(row, "title"), "Ohne Titel");
        }

        phase a_type_that_says_which_field_names_it_is_called_by_that_field;
        {
            const DRAFTS: &str = r#"app so.tilo.drafts "Drafts"

type Draft (title: subject, untitled: "New message") {
    from: Text
    subject: Text
    body: Text
}

type Drafts {
    drafts: Collection<Draft>
}

root Drafts

default view drafts for Drafts

default view draft_editor for Draft

primary action new_draft(ds: Drafts, subject: Text = "") -> Draft (icon: .plus) {
    ds.drafts.insert(Draft(from: "me", subject: subject))
}

view drafts(ds: Drafts) {
    for d in ds.drafts {
        ui.ListRow(title: meta(d).title)
    }
}

view draft_editor(d: Draft) {
    ui.TextInput(bind: d.subject, title: true)
}
"#;
            let app = App::compile(DRAFTS, &[]);
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            app.load(&rt);
            let ds = rt.create("Drafts").unwrap();
            let draft = |subject: &str| {
                returned_ref(
                    rt.invoke_action(
                        &me(),
                        (
                            app.action("new_draft").as_str(),
                            vec![Value::resource(&ds), Value::Text(subject.to_owned())],
                        ),
                    )
                    .unwrap(),
                )
            };
            let hi = draft("Hi there");
            assert_eq!(
                rt.title(&hi).unwrap(),
                "Hi there",
                "the field the type names, not the first one it declares"
            );
            let empty = draft("");
            assert_eq!(rt.title(&empty).unwrap(), "New message");
            let mut list = rt
                .open_view(&me(), &app.view("drafts"), vec![Value::resource(&ds)])
                .unwrap();
            list.poll_patches().unwrap();
            let titles: Vec<String> = of_kind(list.tree(), "ui.ListRow")
                .into_iter()
                .map(|row| text_prop(row, "title"))
                .collect();
            assert_eq!(titles, ["Hi there", "New message"]);
            let mut editor = rt
                .open_view(
                    &me(),
                    &app.view("draft_editor"),
                    vec![Value::resource(&empty)],
                )
                .unwrap();
            editor.poll_patches().unwrap();
            assert_eq!(
                text_prop(of_kind(editor.tree(), "ui.TextInput")[0], "placeholder"),
                "New message"
            );
        }

        phase a_views_words_draw_in_the_readers_language;
        {
            let app = App::compile(SOURCE, &[]);
            let catalogue = tilolang::messages::source_catalogue(app.meta());
            assert!(
                catalogue.contains(".w-new-messages = { $unread } new messages"),
                "{catalogue}"
            );
            assert!(catalogue.contains(".w-at-work = At work"), "{catalogue}");
            assert!(
                !catalogue.contains("w-home ="),
                "a compared literal is no message: {catalogue}"
            );
            assert!(
                !catalogue.contains("example"),
                "a data prop's literal is no message: {catalogue}"
            );
            assert!(catalogue.contains("d-so-tilo-inbox = Inbox"), "{catalogue}");
            let inbox_id = app.meta().types[0].id.clone();
            let filter = tilolang::messages::filter_key(&inbox_id, "pinned");
            assert!(
                catalogue.contains(&format!("{filter} = Pinned")),
                "a filter's label is a message: {catalogue}"
            );

            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            app.load(&rt);
            rt.set_words(German);
            let inbox = rt.create("Inbox").unwrap();
            let mut view = rt
                .open_view(&me(), &app.view("inbox"), vec![Value::resource(&inbox)])
                .unwrap();
            view.poll_patches().unwrap();
            let texts: Vec<String> = of_kind(view.tree(), "ui.Text")
                .into_iter()
                .map(|t| text_prop(t, "value"))
                .collect();
            assert_eq!(
                texts,
                [
                    "3 neue Nachrichten",
                    "Bei der Arbeit",
                    "Eingang",
                    "Angeheftet"
                ],
                "a window's title and a filter's label are words too"
            );
            let link = of_kind(view.tree(), "ui.Link");
            assert_eq!(text_prop(link[0], "url"), "https://example.com/help");
            assert_eq!(
                text_prop(link[0], "label"),
                "Hilfe",
                "an option of words is words"
            );
        }
    }
}
