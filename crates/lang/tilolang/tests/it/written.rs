//! Written views (`tilolang::written`): a `view` declaration someone writes, with every
//! installed app in scope by its name and no `use`.

use crate::common::{todo_meta, validate};

fn written(src: &str) -> Result<tilolang::CompiledView, Vec<tilolang::Diagnostic>> {
    tilolang::compile_view_definition(src, "person:ada", &[todo_meta()])
}

fn codes(src: &str) -> Vec<String> {
    written(src)
        .err()
        .unwrap_or_default()
        .into_iter()
        .map(|d| d.code)
        .collect()
}

#[test]
fn a_view_over_an_installed_app_needs_no_use() {
    let v = written(
        "/// What is left.\nview left(list: todo.TodoList) {\n    for t in list.items {\n        text(t.title)\n    }\n}",
    )
    .unwrap_or_else(|d| panic!("{d:#?}"));
    validate(&v.wasm);
    assert_eq!(v.name, "left");
    assert_eq!(v.shows(), Some("so.tilo.todo.TodoList"));
    assert!(v.meta.app.starts_with(tilolang::WRITTEN_VIEW_APP_PREFIX));
}

#[test]
fn its_writer_is_part_of_what_it_is() {
    let src = "view hello() {\n    text(\"Hello\")\n}";
    let ada = written(src).unwrap();
    let bo = tilolang::compile_view_definition(src, "person:bo", &[todo_meta()]).unwrap();
    assert_ne!(ada.meta.app, bo.meta.app);
    assert_eq!(
        ada.meta.app,
        written(src).unwrap().meta.app,
        "and nothing else"
    );
}

#[test]
fn a_bare_body_is_a_view_that_takes_nothing() {
    let v = written("column {\n    text(\"Good morning\")\n}").unwrap();
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

#[test]
fn it_is_one_view_and_its_functions_only() {
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
