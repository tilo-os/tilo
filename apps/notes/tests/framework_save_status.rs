//! The framework-hosted app reports the durability the host measures: a note
//! written to the record's storage reads "Saved", never the session-only
//! warning, and is still there after the app is closed and reopened.

use notes::actions::new_page_action;
use notes::doc::NotesDoc;
use notes::{NotesShell, PageId};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;

fn save_labels(app: &mut TestApp<NotesShell>) -> Vec<String> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .filter(|label| ["Saved", "Saving…", "Save failed", "Not saved"].contains(&label.as_str()))
        .collect()
}

#[test]
fn a_hosted_note_reports_saved_and_survives_a_reopen() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    app.invoke(
        Invoke::apply(&new_page_action(
            PageId::new_v4(),
            None,
            "a".into(),
            "Groceries",
        ))
        .unwrap(),
    )
    .unwrap();
    app.flush().unwrap();
    app.settle();
    assert_eq!(
        save_labels(&mut app),
        ["Saved"],
        "the status follows the host"
    );

    let mut app = app.reopen().unwrap();
    let titles = app.read_document(|doc| {
        NotesDoc::list_pages(doc)
            .into_iter()
            .map(|page| page.title)
            .collect::<Vec<_>>()
    });
    assert_eq!(titles, ["Groceries"], "the note is durable");
    assert_eq!(save_labels(&mut app), ["Saved"]);
}
