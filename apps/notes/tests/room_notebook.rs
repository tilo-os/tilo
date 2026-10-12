//! One notebook per room: every Notes window in a room shows the same
//! pages, and the notebooks Notes kept per window before are brought into
//! it exactly once.

use crate::framework_support::{add_note, body, notes, shown};
use notes::actions::{
    create_page_action, new_tag_action, set_tag_assignment_action, write_body_action,
};
use notes::{MainPanel, NotesShell, PageId};
use ui::framework::testing::{TestApp, window_record};
use ui::record::RecordId;

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

#[test]
fn the_room_notebook_adopts_each_window_notebook_exactly_once() {
    ui::i18n::I18nManager::init("en-US");
    let inbox = PageId::new_v4();
    let child = PageId::new_v4();
    let ideas = PageId::new_v4();
    let tag = PageId::new_v4();
    let blocks = |markdown: &str| MainPanel::seed_blocks(markdown, |_| None);
    let first = window_record::<NotesShell>(
        0x41,
        &[
            create_page_action(inbox, None, "Inbox"),
            write_body_action(inbox, &blocks("Call the dentist")),
            create_page_action(child, Some(inbox), "Errands"),
            write_body_action(child, &blocks("- milk\n- bread")),
        ],
    )
    .unwrap();
    let second = window_record::<NotesShell>(
        0x42,
        &[
            create_page_action(ideas, None, "Ideas"),
            write_body_action(ideas, &blocks("A **bold** idea")),
            new_tag_action(tag, "later"),
            set_tag_assignment_action(ideas, tag, true),
        ],
    )
    .unwrap();
    // Most windows were opened and closed without a note.
    let empty = window_record::<NotesShell>(0x43, &[]).unwrap();
    let originals = [&first, &second, &empty].map(|doc| doc.clone().get_heads());
    let app = TestApp::<NotesShell>::with_window_records(&mut [second, empty, first]).unwrap();
    assert!(app.record().is_package_primary());

    // Every page, in record order, with its tree, body and tag.
    let state = shown(&app);
    let roots = state
        .children_of(None)
        .iter()
        .map(|page| page.id)
        .collect::<Vec<_>>();
    assert_eq!(roots, [inbox, ideas]);
    assert_eq!(state.pages().len(), 3);
    assert_eq!(state.get(child).unwrap().parent, Some(inbox));
    assert_eq!(body(&app, inbox), ["Call the dentist"]);
    assert_eq!(body(&app, child), ["milk", "bread"]);
    assert_eq!(body(&app, ideas), ["A bold idea"]);
    assert_eq!(
        state
            .tags_for_page(ideas)
            .iter()
            .map(|tag| tag.name.clone())
            .collect::<Vec<_>>(),
        ["later"]
    );

    // Opening Notes again, in this window or another, adopts nothing more.
    let app = app.reopen().unwrap();
    assert_eq!(shown(&app).pages().len(), 3);
    let app = app.another_window().unwrap();
    assert_eq!(shown(&app).pages().len(), 3);

    // The per-window records are still there, untouched.
    let windows = app.window_records();
    assert_eq!(
        windows.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
        [0x41, 0x42, 0x43].map(RecordId::window_default)
    );
    for ((_, doc), heads) in windows.iter().zip(originals) {
        assert_eq!(doc.clone().get_heads(), heads);
    }
}
