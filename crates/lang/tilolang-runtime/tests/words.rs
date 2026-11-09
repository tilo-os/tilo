//! A view's words in the reader's language (i18n.md §2): a literal a component shows is a
//! message of the package's catalogue, drawn through the runtime's words with its
//! interpolations as arguments; a literal that is data, or that a view compares, is not.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{AmStore, Runtime, SimpleAuthority, Value, Words};

    const SOURCE: &str = r#"app so.tilo.inbox "Inbox"

type Inbox {
    unread: Int = 3
    kind: Text = "work"
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

    /// A record with no title is called what its type says, in the reader's language, at every
    /// surface that names it: its row, the title every window shows, and its title field's
    /// placeholder.
    #[test]
    fn a_types_untitled_words_read_in_the_readers_language() {
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

    #[test]
    fn a_views_words_draw_in_the_readers_language() {
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
