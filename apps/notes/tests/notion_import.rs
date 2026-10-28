//! A Notion import lands in the notebook as one step: every page arrives,
//! and a single undo takes the whole import back.

use notes::notion::{ImportedPage, import_action};

fn pages() -> Vec<ImportedPage> {
    (0..3)
        .map(|at| ImportedPage {
            title: format!("Page {at}"),
            markdown: format!("body {at}\n"),
            images: Vec::new(),
        })
        .collect()
}

fn titles(app: &ui::framework::testing::TestApp<notes::NotesShell>) -> Vec<String> {
    crate::framework_support::shown(app)
        .children_of(None)
        .iter()
        .map(|page| page.title.clone())
        .collect()
}

#[test]
fn one_undo_takes_back_every_page_the_import_added() {
    let mut app = crate::framework_support::notes();
    app.invoke_confirmed(import_action(&pages())).unwrap();
    assert_eq!(titles(&app), ["Page 0", "Page 1", "Page 2"]);

    assert!(app.undo(), "the import is one undo step");
    assert_eq!(
        titles(&app),
        Vec::<String>::new(),
        "one undo takes back every page, not only the last"
    );

    assert!(app.redo());
    assert_eq!(
        titles(&app),
        ["Page 0", "Page 1", "Page 2"],
        "redo brings the whole import back"
    );
}
