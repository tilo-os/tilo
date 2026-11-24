//! Written views (`tilolang::written`): a `view` declaration someone writes, with every
//! installed app in scope by its name and no `use`.

use crate::common::{compile_with, todo_meta, validate};

/// A package with a named root, as another app publishes one.
const NOTES: &str = "app so.tilo.notes \"Notes\"\ntype Note {\n    title: Text\n    pinned: Bool = false\n}\ntype Notebook {\n    notes: Collection<Note>\n}\nroot notebook: Notebook\ndefault view notebook for Notebook\nview notebook(nb: Notebook) {\n    for n in nb.notes {\n        text(n.title)\n    }\n}\n";

/// A view that names the notebook alone, and one that names one notebook by its id.
const ALONE: &str =
    "view titles() {\n    for n in notes.notebook.notes {\n        text(n.title)\n    }\n}";
const BY_ID: &str = "view titles() {\n    for n in @notes.Notebook:uuid:0f2b1c00-0000-4000-8000-000000000001.notes {\n        text(n.title)\n    }\n}";

fn written(src: &str) -> Result<tilolang::CompiledView, Vec<tilolang::Diagnostic>> {
    tilolang::compile_view_definition(src, "ada", &[todo_meta()])
}

fn codes(src: &str) -> Vec<String> {
    written(src)
        .err()
        .unwrap_or_default()
        .into_iter()
        .map(|d| d.code)
        .collect()
}

tilo_test_env::scenario! {
    /// Written views.
    fn a_written_view_is_one_view_with_every_installed_app_in_scope;

    phase a_view_over_an_installed_app_needs_no_use;
    {
        let v = written(
            "/// What is left.\nview left(list: todo.TodoList) {\n    for t in list.items {\n        text(t.title)\n    }\n}",
        )
        .unwrap_or_else(|d| panic!("{d:#?}"));
        validate(&v.wasm);
        assert_eq!(v.name, "left");
        assert_eq!(v.shows(), Some("so.tilo.todo.TodoList"));
        assert!(v.meta.app.starts_with(tilolang::WRITTEN_VIEW_APP_PREFIX));
    }

    phase its_writer_is_part_of_what_it_is;
    {
        let src = "view hello() {\n    text(\"Hello\")\n}";
        let ada = written(src).unwrap();
        let bo = tilolang::compile_view_definition(src, "bo", &[todo_meta()]).unwrap();
        assert_ne!(ada.meta.app, bo.meta.app);
        assert_eq!(
            ada.meta.app,
            written(src).unwrap().meta.app,
            "and nothing else"
        );
    }

    phase the_standard_library_a_runtime_loaded_is_std_once;
    // A running workspace loads the standard library as a package beside the apps: a written view
    // reads it as `std`, as every package does, and the library is not in scope twice.
    {
        let v = tilolang::compile_view_definition(
            "view history(of: todo.TodoList) {\n    std.history(of)\n}",
            "ada",
            &[tilolang::stdlib::meta().clone(), todo_meta()],
        )
        .unwrap_or_else(|d| panic!("{d:#?}"));
        validate(&v.wasm);
        assert_eq!(v.shows(), Some("so.tilo.todo.TodoList"));
    }

    phase a_bare_body_is_a_view_that_takes_nothing;
    {
        let v = written("stack {\n    text(\"Good morning\")\n}").unwrap();
        assert!(v.params.is_empty());
        assert_eq!(v.shows(), None);
        // Its diagnostics are where it was written, not where it was wrapped.
        let wrong = written("text(nope)").unwrap_err();
        assert!(
            wrong
                .iter()
                .all(|d| d.line == 1 && d.col == 6 && d.file.is_none()),
            "{wrong:#?}"
        );
    }

    phase it_is_one_view_and_its_functions_only;
    {
        assert_eq!(
            codes("type Note {\n    title: Text\n}\nview n(x: Note) {\n    text(x.title)\n}"),
            ["E116"]
        );
        assert_eq!(
            codes("use so.tilo.todo as todo\nview n() {\n    text(\"x\")\n}"),
            ["E116"]
        );
        assert_eq!(
            codes("view a() {\n    text(\"a\")\n}\nview b() {\n    text(\"b\")\n}"),
            ["E117"]
        );
        assert!(
            codes("fn twice(n: Int) -> Int {\n    n * 2\n}\nview a() {\n    text(\"{twice(2)}\")\n}")
                .is_empty()
        );
        // An app the workspace does not have is not in scope.
        let missing = written("view c(x: board.Card) {\n    text(x.title)\n}").unwrap_err();
        assert!(missing.iter().all(|d| d.line == 1), "{missing:#?}");
    }

    phase a_kind_kept_for_each_document_is_named_by_its_id_and_never_alone;
    {
        let notes = compile_with(NOTES, &[]).expect("notes").meta;
        let view = |source: &str, notes: &tilolang::PackageMeta| {
            tilolang::compile_view_definition(source, "ada", std::slice::from_ref(notes))
        };
        // The workspace's notebook: named alone it is that one.
        view(ALONE, &notes).unwrap_or_else(|d| panic!("{d:#?}"));

        // A package that says it keeps a notebook for each document (`document: notebook`) keeps
        // none for the workspace: named alone it is none of them, and the fault ends with the
        // form that works.
        let notes = compile_with(
            &NOTES.replacen("\"Notes\"", "\"Notes\" (document: notebook)", 1),
            &[],
        )
        .expect("notes kept for each document")
        .meta;
        assert_eq!(notes.each_document, ["notebook"]);
        let alone = view(ALONE, &notes).unwrap_err();
        let said: Vec<&str> = alone.iter().map(|d| d.code.as_str()).collect();
        assert_eq!(said, ["E142"], "{alone:#?}");
        let fault = &alone[0];
        assert!(fault.line == 2 && fault.file.is_none(), "{fault:#?}");
        assert!(
            fault
                .message
                .starts_with("`notes.notebook` alone names no notebook"),
            "{fault:#?}"
        );
        assert!(
            fault.message.ends_with(
                "write `@notes.Notebook:<its id>` where this says `notes.notebook`"
            ),
            "{fault:#?}"
        );
        // One of them, by its id, is read as before.
        view(BY_ID, &notes).unwrap_or_else(|d| panic!("{d:#?}"));
    }

    phase windows_on_two_items_share_the_one_library_another_app_lists;
    {
        // Each window shows one note of the workspace's one notebook (`document:
        // notebook.notes`): the notebook stays the workspace's, which another app lists as a
        // whole, and two windows hold two of its notes, each by its id.
        let notes = compile_with(
            &NOTES.replacen("\"Notes\"", "\"Notes\" (document: notebook.notes)", 1),
            &[],
        )
        .expect("notes whose windows each show a note")
        .meta;
        assert!(notes.windows_hold_documents());
        assert!(notes.each_document.is_empty(), "the notebook is no window's own");
        let items = &notes.document_items;
        assert_eq!(items.len(), 1, "{items:#?}");
        assert_eq!((items[0].root.as_str(), items[0].field.as_str()), ("notebook", "notes"));
        let note = notes.type_named("Note").expect("Note");
        assert_eq!(items[0].ty, note.id, "a window opens a note");
        let view = |source: &str| {
            tilolang::compile_view_definition(source, "ada", std::slice::from_ref(&notes))
        };
        view(ALONE).unwrap_or_else(|d| panic!("the library is listed whole: {d:#?}"));
        let two = "view two() {\n    text(@notes.Note:uuid:0f2b1c00-0000-4000-8000-000000000001.title)\n    text(@notes.Note:uuid:0f2b1c00-0000-4000-8000-000000000002.title)\n}";
        view(two).unwrap_or_else(|d| panic!("two windows' notes, each by its id: {d:#?}"));
    }
}
