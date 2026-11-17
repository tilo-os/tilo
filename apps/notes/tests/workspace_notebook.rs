//! One notebook per workspace: every Notes window in a workspace shows the same
//! pages.

use crate::framework_support::{add_note, body, deliver, notes, open, shown, surface};
use notes::NotesShell;
use ui::framework::testing::TestApp;

fn titles(app: &TestApp<NotesShell>) -> Vec<String> {
    shown(app)
        .pages()
        .iter()
        .map(|page| page.title.clone())
        .collect()
}

#[test]
fn a_new_window_shows_the_pages_an_earlier_window_wrote() {
    let mut app = notes();
    let inbox = add_note(&mut app, "Inbox", "Call the dentist");
    // Closing Notes and opening it again is a window the launcher never
    // had: before, it opened an empty notebook of its own.
    let mut app = app.another_window().unwrap();
    assert_eq!(titles(&app), ["Inbox"]);
    assert_eq!(body(&app, inbox), ["Call the dentist"]);
    let later = add_note(&mut app, "Later", "Read the paper");
    let app = app.another_window().unwrap();
    assert_eq!(titles(&app), ["Inbox", "Later"]);
    assert_eq!(body(&app, later), ["Read the paper"]);
}

/// Two Notes windows open at once in one workspace show one notebook, and what
/// one window writes appears in the other as it is written: in the note it
/// has open and in its page tree, without closing or reopening anything.
#[test]
fn a_second_open_window_updates_live_with_what_the_first_writes() {
    let mut first = notes();
    let inbox = add_note(&mut first, "Inbox", "Call the dentist");
    open(&mut first, inbox);
    // The part the note is kept in is on disk when the second window opens,
    // as a window opened later finds what an earlier one saved.
    first.flush().unwrap();
    let mut second = first.open_window().unwrap();
    assert_eq!(
        second.record(),
        first.record(),
        "both windows show the workspace's notebook"
    );
    open(&mut second, inbox);

    // Typing in the first window, as a person does.
    let editor = first.harness().targets()["block_editor"];
    first.harness().click_at(editor.x0 + 20.0, editor.y0 + 10.0);
    first.harness().key("End");
    first.harness().type_text(" today");
    assert!(deliver(&mut first, &mut second));
    let shown_text = surface(&mut second, |notes| notes.editor_text_for_test());
    assert!(
        shown_text.contains("Call the dentist today"),
        "the second window's open note shows the first window's typing: {shown_text:?}"
    );

    // A note the second window adds appears in the first window's tree.
    let later = add_note(&mut second, "Later", "Read the paper");
    assert!(deliver(&mut second, &mut first));
    let tree = surface(&mut first, |notes| {
        notes
            .workspace_pages_for_test()
            .into_iter()
            .map(|page| page.title)
            .collect::<Vec<_>>()
    });
    assert_eq!(tree, ["Inbox", "Later"]);
    assert_eq!(body(&first, later), ["Read the paper"]);
    assert_eq!(body(&second, inbox), ["Call the dentist today"]);
}
